//! A profile's widget list. An instance's number is its place in the list, on both PC and keyboard.

use std::path::Path;

use anyhow::{Context, Result, bail, ensure};
use glassdeck_layout::{Layout, Variant};
use glassdeck_package::{ModuleManifest, Size};
use serde::Deserialize;

/// Positions snap to this, so widgets line up.
pub const GRID: u32 = 8;
/// The profile.json layout this build reads.
pub const PROFILE_FORMAT_VERSION: u32 = 1;

/// The widget area in whole panel pixels: where widget (0, 0) sits, and how much room there is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WidgetArea {
    pub x: u32,
    pub y: u32,
    pub size: Size,
}

impl WidgetArea {
    /// The same on every layout: the F-row, which the area stops above, doesn't change.
    pub fn of_panel() -> Self {
        let area = Layout::new(Variant::Ansi).widget_area;
        Self {
            x: area.x.round() as u32,
            y: area.y.round() as u32,
            size: Size { w: area.width.floor() as u32, h: area.height.floor() as u32 },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawProfile {
    format: u32,
    widgets: Vec<RawWidget>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawWidget {
    module: String,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    /// What the user chose for this instance's options, by path; only the host reads it.
    #[serde(default)]
    options: serde_json::Map<String, serde_json::Value>,
}

/// Where an instance sits, relative to the widget area.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placement {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Placement {
    fn overlaps(&self, other: &Placement) -> bool {
        self.x < other.x + other.w && other.x < self.x + self.w && self.y < other.y + other.h && other.y < self.y + self.h
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Widget {
    pub module: String,
    pub placement: Placement,
    pub options: serde_json::Map<String, serde_json::Value>,
}

/// A widget list that fits the area, lines up on the grid, respects each module's sizes and never overlaps.
#[derive(Clone, Debug, PartialEq)]
pub struct Profile {
    pub widgets: Vec<Widget>,
}

impl Profile {
    pub fn read(path: &Path, area: Size, manifest: impl Fn(&str) -> Option<ModuleManifest>) -> Result<Self> {
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Self::parse(&text, area, manifest).with_context(|| format!("checking {}", path.display()))
    }

    pub fn parse(json: &str, area: Size, manifest: impl Fn(&str) -> Option<ModuleManifest>) -> Result<Self> {
        let raw: RawProfile = serde_json::from_str(json).context("parsing the profile")?;
        ensure!(raw.format == PROFILE_FORMAT_VERSION, "profile format {} is not the {PROFILE_FORMAT_VERSION} this build reads", raw.format);
        ensure!(raw.widgets.len() <= usize::from(u16::MAX), "a profile holds at most {} widgets", u16::MAX);
        let mut widgets: Vec<Widget> = Vec::with_capacity(raw.widgets.len());
        for (index, raw) in raw.widgets.into_iter().enumerate() {
            let RawWidget { module, x, y, w, h, options } = raw;
            let placement = Placement { x, y, w, h };
            let name = format!("widget {index} ({module})");
            let Some(manifest) = manifest(&module) else { bail!("{name}: no module with that id is installed") };
            ensure!(x % GRID == 0 && y % GRID == 0, "{name}: x and y must be multiples of {GRID}");
            ensure!(manifest.resize.allows(manifest.size, Size { w, h }), "{name}: {w}x{h} is not a size this module allows");
            ensure!(x + w <= area.w && y + h <= area.h, "{name}: reaches outside the {}x{} widget area", area.w, area.h);
            if let Some(other) = widgets.iter().position(|o| o.placement.overlaps(&placement)) {
                bail!("{name}: overlaps widget {other} ({})", widgets[other].module);
            }
            widgets.push(Widget { module, placement, options });
        }
        Ok(Self { widgets })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glassdeck_package::Resize;

    const AREA: Size = Size { w: 1903, h: 256 };

    fn clock(id: &str) -> Option<ModuleManifest> {
        (id == "dev.glassdeck.clock").then(|| ModuleManifest {
            id: id.into(),
            name: "Clock".into(),
            version: "1.0.0".into(),
            size: Size { w: 240, h: 120 },
            resize: Resize::Between { min: Size { w: 160, h: 80 }, max: Size { w: 480, h: 240 } },
            host: false,
        })
    }

    fn widget(x: u32, y: u32, w: u32, h: u32) -> String {
        format!(r#"{{ "module": "dev.glassdeck.clock", "x": {x}, "y": {y}, "w": {w}, "h": {h} }}"#)
    }

    fn profile(widgets: &[String]) -> Result<Profile> {
        Profile::parse(&format!(r#"{{ "format": 1, "widgets": [{}] }}"#, widgets.join(",")), AREA, clock)
    }

    #[test]
    fn every_layout_has_the_same_widget_area() {
        assert_eq!(Layout::new(Variant::Ansi).widget_area, Layout::new(Variant::Iso).widget_area);
        assert_eq!(WidgetArea::of_panel(), WidgetArea { x: 8, y: 5, size: Size { w: 1903, h: 251 } });
    }

    #[test]
    fn accepts_widgets_side_by_side() {
        let profile = profile(&[widget(8, 0, 240, 120), widget(248, 0, 240, 120)]).unwrap();
        assert_eq!(profile.widgets.len(), 2);
        assert_eq!(profile.widgets[1].placement, Placement { x: 248, y: 0, w: 240, h: 120 });
        assert!(profile.widgets[0].options.is_empty());
    }

    #[test]
    fn refuses_bad_placements_with_a_reason() {
        let cases = [
            (vec![widget(4, 0, 240, 120)], "multiples of 8"),
            (vec![widget(0, 0, 100, 120)], "not a size"),
            (vec![widget(1800, 0, 240, 120)], "outside"),
            (vec![widget(0, 0, 240, 120), widget(232, 0, 240, 120)], "overlaps widget 0"),
            (vec![r#"{ "module": "dev.glassdeck.nope", "x": 0, "y": 0, "w": 8, "h": 8 }"#.to_string()], "no module"),
        ];
        for (widgets, reason) in cases {
            let error = format!("{:#}", profile(&widgets).unwrap_err());
            assert!(error.contains(reason), "{error}");
        }
    }
}
