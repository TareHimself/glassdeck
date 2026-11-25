//! Wallpaper Engine import, "bring your own install": reads items already in the local Steam workshop folder.
//! Nothing is downloaded, and converted backgrounds stay on this PC and its keyboard.

pub mod library;
pub mod project;
pub mod steam;

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use glassdeck_convert::{Details, EncodeSettings};
use glassdeck_package::{Background, Origin};
use serde_json::{Map, Value};

pub use library::{Item, find, subscribed};
pub use project::{Kind, Project};

/// Why an item can't become a background, or `None` when it can.
pub fn why_unsupported(kind: &Kind) -> Option<String> {
    Some(match kind {
        Kind::Video { .. } | Kind::Web { .. } => return None,
        Kind::Scene => "scenes only render in Wallpaper Engine; they'll stream live once the Wallpaper Engine stream exists".into(),
        Kind::Application => "application wallpapers run a program and are not supported".into(),
        Kind::Preset { base_id } => format!("a preset for workshop item {base_id}; presets are not supported yet"),
        Kind::Unknown { declared: Some(t) } => format!("unknown wallpaper type {t:?}"),
        Kind::Unknown { declared: None } => "the project.json names no wallpaper type".into(),
    })
}

pub fn background_path(backgrounds_dir: &Path, item: &Item) -> PathBuf {
    backgrounds_dir.join(format!("we-{}.{}", item.id, glassdeck_package::EXTENSION))
}

/// Converts a video or web item into a background at `out`; other kinds are refused.
pub fn convert(item: &Item, project: &Project, out: &Path, settings: EncodeSettings) -> Result<Background> {
    let details = Details {
        title: project.title.clone(),
        origin: Origin::WallpaperEngine { workshop_id: item.id.clone() },
        preview: project.preview.as_ref().map(|name| item.dir.join(name)),
    };
    match &project.kind {
        Kind::Video { file } => glassdeck_convert::video(&item.dir.join(file), out, details, settings),
        Kind::Web { file } => glassdeck_convert::web(&item.dir, file, project::user_properties(&project.properties), out, details),
        other => bail!("{} is a {other} wallpaper; only video and web wallpapers convert to a background", item.id),
    }
}

/// Stands in for Wallpaper Engine's JS hooks: user properties get their defaults, audio and media stay silent.
pub fn init_script(properties: &Map<String, Value>) -> String {
    let properties = Value::Object(properties.clone());
    format!(
        r#"(() => {{
  const userProperties = {properties};
  for (const name of ["wallpaperRegisterAudioListener", "wallpaperRegisterMediaStatusListener",
      "wallpaperRegisterMediaPropertiesListener", "wallpaperRegisterMediaThumbnailListener",
      "wallpaperRegisterMediaPlaybackListener", "wallpaperRegisterMediaTimelineListener"]) {{
    window[name] = () => {{}};
  }}
  window.addEventListener("load", () => setTimeout(() => {{
    const listener = window.wallpaperPropertyListener;
    if (listener && typeof listener.applyUserProperties === "function") listener.applyUserProperties(userProperties);
  }}, 0));
}})();"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_init_script_embeds_property_defaults() {
        let project = project::parse(br#"{"type":"web","file":"i.html","general":{"properties":{"speed":{"type":"slider","value":3}}}}"#).unwrap();
        let script = init_script(&project::user_properties(&project.properties));
        assert!(script.contains(r#"{"speed":{"type":"slider","value":3}}"#), "{script}");
        assert!(script.contains("wallpaperRegisterAudioListener"));
    }
}
