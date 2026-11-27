//! The active theme: what the app last pushed, checked against the installed packages, and kept in
//! `theme.json` so glassdeckd restores it at login without the app.

use std::collections::HashMap;
use std::fs;
use std::io::ErrorKind;
use std::path::Path;
use std::time::SystemTime;

use anyhow::{Context, Result};
use glassdeck_api::{THEME_FORMAT_VERSION, Theme};
use glassdeck_luau::placement::{Placement, PlacedWidget, WidgetArea, check_widgets};
use glassdeck_package::{Package, PackageType, QualifiedId, is_package};
use glassdeck_proto::{BUILT_IN_BACKGROUND, WidgetSlot};
use serde_json::{Map, Value as Json};

use crate::error::bad_request;
use crate::streams::StreamName;

/// Chosen values by option path, as a theme entry stores them.
pub type Chosen = Map<String, Json>;

/// An installed package, and when its file was written, so a rewrite counts as a new version.
pub struct Installed {
    pub package: Package,
    pub modified: SystemTime,
}

/// Every package glassdeckd holds a copy of, by id. One that doesn't open is reported and left out.
pub fn installed(folder: &Path) -> Result<HashMap<QualifiedId, Installed>> {
    let mut packages = HashMap::new();
    let entries = match fs::read_dir(folder) {
        Ok(entries) => entries,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(packages),
        Err(e) => return Err(e).with_context(|| format!("reading {}", folder.display())),
    };
    for entry in entries {
        let path = entry?.path();
        if !is_package(&path) {
            continue;
        }
        let opened = fs::metadata(&path)
            .and_then(|m| m.modified())
            .map_err(anyhow::Error::from)
            .and_then(|modified| Package::open(&path).map(|package| Installed { package, modified }));
        match opened {
            Ok(installed) => {
                packages.insert(installed.package.id().clone(), installed);
            }
            Err(e) => eprintln!("error: {e:#}"),
        }
    }
    Ok(packages)
}

/// A theme entry that's checked: which package, and what the user chose for it.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub package: QualifiedId,
    pub options: Chosen,
}

/// A theme that checks out against the installed packages.
#[derive(Clone, Debug, PartialEq)]
pub struct ActiveTheme {
    pub background: Entry,
    pub stream: Option<StreamName>,
    pub widgets: Vec<(Placement, Entry)>,
}

impl ActiveTheme {
    /// Parses `theme` at the edge: ids, the stream name, a background that's a background, and a
    /// widget layout the keyboard will accept.
    pub fn check(theme: &Theme, packages: &HashMap<QualifiedId, Installed>) -> Result<Self> {
        if theme.format != THEME_FORMAT_VERSION {
            return Err(bad_request(format!("theme format {} is not the {THEME_FORMAT_VERSION} this build reads", theme.format)));
        }
        let id = |text: &str| QualifiedId::parse(text).map_err(|e| bad_request(format!("{e:#}")));
        let background = id(&theme.background.package)?;
        if background.package_type() != PackageType::Background {
            return Err(bad_request(format!("{background} is a widget, not a background")));
        }
        if background.as_str() != BUILT_IN_BACKGROUND && !packages.contains_key(&background) {
            return Err(bad_request(format!("{background} isn't held; push its .deck with the theme")));
        }
        let stream = theme.stream.as_deref().map(StreamName::parse).transpose().map_err(|e| bad_request(format!("{e:#}")))?;
        let mut widgets = Vec::with_capacity(theme.widgets.len());
        for widget in &theme.widgets {
            let placement = Placement { x: widget.x, y: widget.y, w: widget.w, h: widget.h };
            widgets.push((placement, Entry { package: id(&widget.package)?, options: widget.options.clone() }));
        }
        let layout: Vec<PlacedWidget> = widgets.iter().map(|(placement, e)| PlacedWidget { package: e.package.clone(), placement: *placement }).collect();
        check_widgets(&layout, WidgetArea::of_panel().size, |id| packages.get(id).and_then(|i| i.package.manifest().widget().copied()))
            .map_err(|e| bad_request(format!("{e:#}")))?;
        Ok(Self { background: Entry { package: background, options: theme.background.options.clone() }, stream, widgets })
    }

    /// The ids of every package the theme uses, the built-in background aside.
    pub fn package_ids(&self) -> std::collections::BTreeSet<String> {
        let background = (self.background.package.as_str() != BUILT_IN_BACKGROUND).then(|| self.background.package.to_string());
        background.into_iter().chain(self.widgets.iter().map(|(_, e)| e.package.to_string())).collect()
    }

    /// The layout as the Link carries it.
    pub fn slots(&self) -> Vec<WidgetSlot> {
        self.widgets
            .iter()
            .map(|(p, entry)| WidgetSlot {
                package: entry.package.to_string(),
                x: to_u16(p.x),
                y: to_u16(p.y),
                w: to_u16(p.w),
                h: to_u16(p.h),
            })
            .collect()
    }
}

/// Placements are checked to fit the 1920x1080 panel, so they always fit a u16.
fn to_u16(n: u32) -> u16 {
    u16::try_from(n).expect("checked placements fit the panel")
}

/// The theme kept from last time, or the starting one on a fresh install.
pub fn load(path: &Path) -> Result<Theme> {
    match fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display())),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(Theme::starting(BUILT_IN_BACKGROUND)),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

pub fn save(path: &Path, theme: &Theme) -> Result<()> {
    if let Some(folder) = path.parent() {
        fs::create_dir_all(folder).with_context(|| format!("creating {}", folder.display()))?;
    }
    let staging = path.with_extension("json.tmp");
    fs::write(&staging, serde_json::to_vec_pretty(theme)?).with_context(|| format!("writing {}", staging.display()))?;
    fs::rename(&staging, path).with_context(|| format!("moving {} into place", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use glassdeck_api::{ThemeBackground, ThemeWidget};

    #[test]
    fn a_theme_must_name_installed_packages_of_the_right_type() {
        let packages = HashMap::new();
        let starting = Theme::starting(BUILT_IN_BACKGROUND);
        let active = ActiveTheme::check(&starting, &packages).unwrap();
        assert_eq!(active.background.package.as_str(), BUILT_IN_BACKGROUND);
        assert!(active.slots().is_empty());

        let missing = Theme { background: ThemeBackground { package: "background.local.converted.nope".into(), options: Map::new() }, ..starting.clone() };
        assert!(format!("{:#}", ActiveTheme::check(&missing, &packages).unwrap_err()).contains("isn't held"));
        let widget = ThemeWidget { package: "widget.local.user.clock".into(), x: 0, y: 0, w: 8, h: 8, options: Map::new() };
        let unknown_widget = Theme { widgets: vec![widget], ..starting.clone() };
        assert!(format!("{:#}", ActiveTheme::check(&unknown_widget, &packages).unwrap_err()).contains("no widget"));
        let widget_as_background = Theme { background: ThemeBackground { package: "widget.local.user.clock".into(), options: Map::new() }, ..starting.clone() };
        assert!(format!("{:#}", ActiveTheme::check(&widget_as_background, &packages).unwrap_err()).contains("not a background"));
        let bad_stream = Theme { stream: Some("Big Screen".into()), ..starting };
        assert!(ActiveTheme::check(&bad_stream, &packages).is_err());
    }

    #[test]
    fn a_fresh_install_starts_on_the_built_in_gradient() {
        let path = std::env::temp_dir().join(format!("glassdeckd-theme-{}.json", std::process::id()));
        assert_eq!(load(&path).unwrap(), Theme::starting(BUILT_IN_BACKGROUND));
        let theme = Theme { stream: Some("monitor-q27g4zd".into()), ..Theme::starting(BUILT_IN_BACKGROUND) };
        save(&path, &theme).unwrap();
        assert_eq!(load(&path).unwrap(), theme);
        let _ = fs::remove_file(&path);
    }
}
