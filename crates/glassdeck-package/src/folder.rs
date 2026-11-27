//! A package's source folder: `deck.json` with what a person writes, the scripts, and `assets/keyboard/`
//! and `assets/host/`. `glassdeck pack` fills in the rest of the manifest.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;

use crate::resource::{Resource, ResourceKind, ResourceName, literal_resources};
use crate::{
    ASSETS_FOLDER, FORMAT_VERSION, HOST_SCRIPT, KEYBOARD_SCRIPT, Kind, BRIDGE_SCRIPT, Manifest, OPTIONS_SCRIPT, Origin, Package, PackageId, PackageType, PackageWriter,
    Version, WidgetSize, source_digest,
};

pub const DECK_SOURCE: &str = "deck.json";

/// `deck.json`: the hand-written part of a package's manifest.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeckSource {
    /// Lets editors find the schema; not used otherwise
    #[serde(rename = "$schema", default)]
    _schema: Option<String>,
    pub id: PackageId,
    pub title: String,
    pub version: Version,
    #[serde(rename = "type")]
    pub kind: PackageType,
    /// A widget's sizes; packing works out a canvas's details from its shader
    #[serde(default)]
    pub details: Option<serde_json::Value>,
    /// Seconds into a canvas or video at which the app draws its still; 0 when left out
    #[serde(default)]
    pub preview_at: Option<f32>,
    /// The hosts `host.luau` may reach with `net`
    #[serde(default)]
    pub network: Vec<crate::NetworkHost>,
}


impl DeckSource {
    /// The folder's `deck.json`, and its text for the source digest.
    pub fn read(folder: &Path) -> Result<(Self, String)> {
        let path = folder.join(DECK_SOURCE);
        let text = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let source = serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        Ok((source, text))
    }

    /// The manifest. Packed folders carry no `preview`: the app draws stills itself and keeps them apart.
    pub fn manifest(&self, origin: Origin, kind: Kind) -> Manifest {
        Manifest {
            format: FORMAT_VERSION,
            id: self.id.clone(),
            title: self.title.clone(),
            version: self.version.clone(),
            origin,
            preview: None,
            network: self.network.clone(),
            kind,
        }
    }
}

/// A script in a source folder; one that is missing or blank (only whitespace and comments) is `None`,
/// so an empty `options.luau` or `host.luau` means the same as none.
pub fn read_script(folder: &Path, name: &str) -> Result<Option<String>> {
    let path = folder.join(name);
    if !path.is_file() {
        return Ok(None);
    }
    let source = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    Ok((!is_blank_script(&source)).then_some(source))
}

/// Only whitespace and Luau comments: `-- line` and `--[[ block ]]`, with any number of `=`.
pub fn is_blank_script(source: &str) -> bool {
    let mut rest = source.trim_start();
    while let Some(comment) = rest.strip_prefix("--") {
        let level = comment.strip_prefix('[').map(|open| open.bytes().take_while(|b| *b == b'=').count());
        rest = match level {
            Some(level) if comment[1 + level..].starts_with('[') => {
                let close = format!("]{}]", "=".repeat(level));
                match comment.find(&close) {
                    Some(end) => &comment[end + close.len()..],
                    None => return false,
                }
            }
            _ => comment.find('\n').map_or("", |end| &comment[end..]),
        }
        .trim_start();
    }
    rest.is_empty()
}

/// Packs a widget folder: `deck.json`, `keyboard.luau`, `options.luau` and `host.luau` if present, and
/// the assets. Every resource a script names as a literal must be in the folder, on a side that script can use.
pub fn pack_folder(folder: &Path, out: &Path) -> Result<Package> {
    let (deck, deck_text) = DeckSource::read(folder)?;
    ensure!(deck.kind == PackageType::Widget, "{} is a {:?} folder; only widget folders pack this way", folder.display(), deck.kind);
    let details = deck.details.clone().context("a widget's deck.json gives its sizes in details")?;
    let size: WidgetSize = serde_json::from_value(details).context("reading the widget's sizes")?;
    let resources = resource_files(&folder.join(ASSETS_FOLDER))?;
    let mut scripts = Vec::new();
    for script in [KEYBOARD_SCRIPT, OPTIONS_SCRIPT, HOST_SCRIPT, BRIDGE_SCRIPT] {
        if let Some(source) = read_script(folder, script)? {
            check_literal_resources(script, &source, &resources)?;
            scripts.push((script, folder.join(script), source));
        }
    }
    let sources = source_digest(std::iter::once(deck_text.as_str()).chain(scripts.iter().map(|(_, _, source)| source.as_str())));
    let from = folder.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut writer = PackageWriter::create(out, deck.manifest(Origin::Packed { from, sources }, Kind::Widget(size)))?;
    for (script, path, _) in &scripts {
        writer.add_file(script, path)?;
    }
    for (resource, path) in &resources {
        writer.add_file(&resource.entry(), path)?;
    }
    writer.finish()
}

/// Every file under `assets/keyboard/` and `assets/host/`, each with a name a script can use.
fn resource_files(assets: &Path) -> Result<Vec<(Resource, PathBuf)>> {
    let mut found = Vec::new();
    if !assets.is_dir() {
        return Ok(found);
    }
    for entry in fs::read_dir(assets).with_context(|| format!("reading {}", assets.display()))? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let kind = ResourceKind::ALL.into_iter().find(|k| k.folder() == name && entry.path().is_dir());
        let kind = kind.with_context(|| format!("{ASSETS_FOLDER}/{name}: {ASSETS_FOLDER}/ holds only the keyboard/ and host/ folders"))?;
        collect_files(&entry.path(), "", kind, &mut found)?;
    }
    found.sort_by_key(|(resource, _)| resource.entry());
    Ok(found)
}

fn collect_files(folder: &Path, prefix: &str, kind: ResourceKind, found: &mut Vec<(Resource, PathBuf)>) -> Result<()> {
    for entry in fs::read_dir(folder).with_context(|| format!("reading {}", folder.display()))? {
        let entry = entry?;
        let name = format!("{prefix}{}", entry.file_name().to_string_lossy());
        if entry.file_type()?.is_dir() {
            collect_files(&entry.path(), &format!("{name}/"), kind, found)?;
        } else {
            let name = ResourceName::parse(&name).with_context(|| format!("{ASSETS_FOLDER}/{}/{name}", kind.folder()))?;
            found.push((Resource { kind, name }, entry.path()));
        }
    }
    Ok(())
}

fn check_literal_resources(script: &str, source: &str, resources: &[(Resource, PathBuf)]) -> Result<()> {
    for (kind, name) in literal_resources(source) {
        let resource = Resource { kind, name: ResourceName::parse(&name).with_context(|| format!("{script} names {name:?}"))? };
        if script == KEYBOARD_SCRIPT && kind == ResourceKind::Host {
            bail!("{script} names {resource}, but host resources never reach the keyboard");
        }
        ensure!(resources.iter().any(|(r, _)| *r == resource), "{script} names {resource}, but {} isn't in the folder", resource.entry());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs::File;

    use super::*;
    use crate::MANIFEST;
    use crate::tests::{Scratch, web_manifest};
    use crate::zip;

    const CLOCK: &str = r#"{
        "id": "local.user.clock",
        "title": "Clock",
        "version": "1.0.0",
        "type": "widget",
        "details": { "size": { "w": 240, "h": 120 }, "min": { "w": 160, "h": 80 }, "max": { "w": 480, "h": 240 } }
    }"#;

    fn resource(kind: ResourceKind, name: &str) -> Resource {
        Resource { kind, name: ResourceName::parse(name).unwrap() }
    }

    /// A clock folder with `keyboard.luau` and the given resource files, as (path under assets/, contents).
    fn clock_folder(scratch: &Scratch, script: &str, files: &[(&str, &str)]) -> PathBuf {
        let folder = scratch.0.join("clock");
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join(DECK_SOURCE), CLOCK).unwrap();
        fs::write(folder.join(KEYBOARD_SCRIPT), script).unwrap();
        for (path, contents) in files {
            let path = folder.join(ASSETS_FOLDER).join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }
        folder
    }

    #[test]
    fn whitespace_and_comments_are_a_blank_script() {
        for blank in ["", "  \n\t", "-- nothing yet\n", "--[[ a\nblock ]]\n-- and a line", "--[==[ ]] still ]==]"] {
            assert!(is_blank_script(blank), "{blank:?}");
        }
        for code in ["x = 1", "-- note\nsend({})", "--[[ open", "--[==[ ]] ]==] y()"] {
            assert!(!is_blank_script(code), "{code:?}");
        }
    }

    #[test]
    fn packs_and_opens_a_widget_folder() {
        let scratch = Scratch::new("widget-pack");
        let script = r#"local face = keyboard.resource("dial/face.svg") return {}"#;
        let folder = clock_folder(&scratch, script, &[("keyboard/dial/face.svg", "<svg/>"), ("host/zones.json", "{}")]);
        let package = pack_folder(&folder, &scratch.0.join("widget.local.user.clock.deck")).unwrap();
        assert_eq!(package.id().as_str(), "widget.local.user.clock");
        assert!(package.manifest().widget().is_some());
        assert_eq!(package.keyboard_script().unwrap().as_deref(), Some(script));
        assert_eq!(package.host_script().unwrap(), None);
        assert!(!package.runs_on_host());
        assert_eq!(package.resource(&resource(ResourceKind::Keyboard, "dial/face.svg")).unwrap(), b"<svg/>");
        assert_eq!(package.resource(&resource(ResourceKind::Host, "zones.json")).unwrap(), b"{}");
        assert!(package.resource(&resource(ResourceKind::Host, "dial/face.svg")).is_err());
    }

    #[test]
    fn packing_refuses_misplaced_or_missing_resources() {
        let refused = |name: &str, script: &str, files: &[(&str, &str)]| {
            let scratch = Scratch::new(name);
            let folder = clock_folder(&scratch, script, files);
            format!("{:#}", pack_folder(&folder, &scratch.0.join("widget.local.user.clock.deck")).err().expect("refused"))
        };
        assert!(refused("widget-flat-asset", "return {}", &[("face.svg", "<svg/>")]).contains("holds only the keyboard/ and host/"));
        assert!(refused("widget-bad-name", "return {}", &[("keyboard/Face.svg", "<svg/>")]).contains("isn't a resource name"));
        assert!(refused("widget-missing", r#"keyboard.resource("face.svg")"#, &[]).contains("isn't in the folder"));
        let host_on_keyboard = refused("widget-host-on-kb", r#"host.resource("zones.json")"#, &[("host/zones.json", "{}")]);
        assert!(host_on_keyboard.contains("never reach the keyboard"));
    }

    #[test]
    fn a_widget_needs_its_keyboard_script() {
        let scratch = Scratch::new("widget-no-script");
        let folder = scratch.0.join("cpu");
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join(DECK_SOURCE), CLOCK).unwrap();
        assert!(pack_folder(&folder, &scratch.0.join("widget.local.user.clock.deck")).is_err());
    }

    #[test]
    fn the_keyboard_copy_leaves_host_files_behind() {
        let scratch = Scratch::new("widget-keyboard-copy");
        let folder = clock_folder(&scratch, "return {}", &[("keyboard/face.svg", "<svg/>"), ("host/zones.json", "{}")]);
        fs::write(folder.join(HOST_SCRIPT), "every(1, function() end)").unwrap();
        fs::write(folder.join(OPTIONS_SCRIPT), r#"option("seconds", toggle(true))"#).unwrap();
        let package = pack_folder(&folder, &scratch.0.join("widget.local.user.clock.deck")).unwrap();
        assert!(package.runs_on_host());
        let copy = package.write_keyboard_copy(&scratch.0.join("keyboard.deck")).unwrap();

        assert_eq!(copy.keyboard_script().unwrap().as_deref(), Some("return {}"));
        assert_eq!(copy.resource(&resource(ResourceKind::Keyboard, "face.svg")).unwrap(), b"<svg/>");
        assert!(!copy.runs_on_host());
        let entries = zip::read_directory(&mut File::open(copy.path().unwrap()).unwrap()).unwrap();
        let mut names: Vec<&str> = entries.keys().map(String::as_str).collect();
        names.sort();
        assert_eq!(names, ["assets/keyboard/face.svg", "keyboard.luau", MANIFEST]);
    }

    #[test]
    fn only_widget_folders_pack_as_widgets() {
        let scratch = Scratch::new("widget-vs-background");
        let folder = clock_folder(&scratch, "return {}", &[]);
        let background = CLOCK.replace(r#""type": "widget""#, r#""type": "background""#);
        fs::write(folder.join(DECK_SOURCE), background).unwrap();
        assert!(format!("{:#}", pack_folder(&folder, &scratch.0.join("x.y.deck")).err().unwrap()).contains("only widget folders"));
        let _ = web_manifest();
    }
}
