use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::Value;

/// A Wallpaper Engine item's `project.json`, parsed into what Glassdeck can act on.
#[derive(Clone, Debug, PartialEq)]
pub struct Project {
    pub title: String,
    pub kind: Kind,
    /// File name in the item folder
    pub preview: Option<String>,
    /// User-editable settings with their defaults, by name
    pub properties: BTreeMap<String, Property>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    Video { file: String },
    Web { file: String },
    Scene,
    Application,
    /// Settings for another item, which holds the wallpaper itself
    Preset { base_id: String },
    Unknown { declared: Option<String> },
}

impl Kind {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Video { .. } => "video",
            Self::Web { .. } => "web",
            Self::Scene => "scene",
            Self::Application => "application",
            Self::Preset { .. } => "preset",
            Self::Unknown { .. } => "unknown",
        }
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Property {
    #[serde(rename = "type", default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub value: Value,
}

#[derive(Deserialize)]
struct RawProject {
    #[serde(default)]
    title: Option<String>,
    #[serde(rename = "type", default)]
    kind: Option<String>,
    #[serde(default)]
    file: Option<String>,
    #[serde(default)]
    preview: Option<String>,
    #[serde(default)]
    dependency: Option<String>,
    #[serde(default)]
    general: Option<RawGeneral>,
}

#[derive(Deserialize)]
struct RawGeneral {
    #[serde(default)]
    properties: Option<BTreeMap<String, Property>>,
}

pub fn read(item_dir: &Path) -> Result<Project> {
    let path = item_dir.join("project.json");
    let bytes = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
    parse(&bytes).with_context(|| format!("parsing {}", path.display()))
}

pub fn parse(bytes: &[u8]) -> Result<Project> {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let raw: RawProject = serde_json::from_slice(bytes)?;
    Ok(Project {
        title: raw.title.unwrap_or_default(),
        kind: kind(raw.kind, raw.file, raw.dependency)?,
        preview: raw.preview.filter(|p| !p.is_empty()),
        properties: raw.general.and_then(|g| g.properties).unwrap_or_default(),
    })
}

fn kind(declared: Option<String>, file: Option<String>, dependency: Option<String>) -> Result<Kind> {
    let file = file.filter(|f| !f.is_empty());
    let needs_file = |name: &str| file.clone().with_context(|| format!("a {name} wallpaper without a file"));
    Ok(match declared.as_deref().map(str::to_ascii_lowercase).as_deref() {
        Some("video") => Kind::Video { file: needs_file("video")? },
        Some("web") => Kind::Web { file: needs_file("web")? },
        Some("scene") => Kind::Scene,
        Some("application") => Kind::Application,
        _ => match dependency {
            Some(base_id) if !base_id.is_empty() => Kind::Preset { base_id },
            Some(_) => bail!("an empty dependency"),
            None => Kind::Unknown { declared },
        },
    })
}

/// What WE passes to `applyUserProperties`: each property as `{ value }`, keyed by name.
pub fn user_properties_json(properties: &BTreeMap<String, Property>) -> Value {
    Value::Object(
        properties
            .iter()
            .map(|(name, p)| (name.clone(), serde_json::json!({ "type": p.kind, "value": p.value })))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_with_bom_and_properties() {
        let json = "\u{FEFF}{\"file\":\"late.mp4\",\"general\":{\"properties\":{\"schemecolor\":{\"order\":0,\"type\":\"color\",\"value\":\"0.5 0.5 0.5\"}}},\"preview\":\"preview.jpg\",\"title\":\"LateNight1\",\"type\":\"video\"}";
        let project = parse(json.as_bytes()).unwrap();
        assert_eq!(project.kind, Kind::Video { file: "late.mp4".into() });
        assert_eq!(project.preview.as_deref(), Some("preview.jpg"));
        assert_eq!(project.properties["schemecolor"].value, Value::from("0.5 0.5 0.5"));
    }

    #[test]
    fn type_names_ignore_case() {
        assert_eq!(parse(br#"{"type":"Web","file":"index.html"}"#).unwrap().kind, Kind::Web { file: "index.html".into() });
        assert_eq!(parse(br#"{"type":"Scene","file":"scene.json"}"#).unwrap().kind, Kind::Scene);
    }

    #[test]
    fn presets_and_untyped_items() {
        assert_eq!(parse(br#"{"file":"","dependency":"884307090"}"#).unwrap().kind, Kind::Preset { base_id: "884307090".into() });
        assert_eq!(parse(br#"{"title":"Gargantua"}"#).unwrap().kind, Kind::Unknown { declared: None });
    }

    #[test]
    fn a_video_needs_its_file() {
        assert!(parse(br#"{"type":"video","file":""}"#).is_err());
    }

    #[test]
    fn user_properties_carry_their_values() {
        let project = parse(br#"{"type":"web","file":"i.html","general":{"properties":{"speed":{"type":"slider","value":3}}}}"#).unwrap();
        assert_eq!(
            user_properties_json(&project.properties),
            serde_json::json!({ "speed": { "type": "slider", "value": 3 } })
        );
    }
}
