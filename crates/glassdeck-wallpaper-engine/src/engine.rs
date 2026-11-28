//! Wallpaper Engine itself on this PC: where it's installed, and which wallpaper it shows.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::Value;

use crate::library::Item;
use crate::steam;

pub const EXE: &str = "wallpaper64.exe";

pub struct Install {
    pub dir: PathBuf,
}

/// The first Steam library holding Wallpaper Engine; `None` without Steam or without Wallpaper Engine.
pub fn install() -> Result<Option<Install>> {
    let Ok(root) = steam::steam_root() else { return Ok(None) };
    Ok(steam::library_folders(&root)?
        .into_iter()
        .map(|library| library.join("steamapps").join("common").join("wallpaper_engine"))
        .find(|dir| dir.join(EXE).is_file())
        .map(|dir| Install { dir }))
}

impl Install {
    pub fn exe(&self) -> PathBuf {
        self.dir.join(EXE)
    }

    /// The wallpaper on Wallpaper Engine's first monitor, when its folder is still there.
    pub fn shown(&self) -> Result<Option<Item>> {
        let path = self.dir.join("config.json");
        let config = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
        let Some(file) = shown_file(&config).with_context(|| format!("reading {}", path.display()))? else { return Ok(None) };
        Ok(item_of(&file))
    }
}

/// `selectedwallpapers` names a wallpaper's main file (`scene.pkg`, a video, `index.html`); its folder is the item.
fn item_of(file: &Path) -> Option<Item> {
    let dir = file.parent()?;
    let id = dir.file_name()?.to_string_lossy().into_owned();
    dir.join("project.json").is_file().then(|| Item { id, dir: dir.to_owned() })
}

/// The `file` Wallpaper Engine's config shows on its lowest-numbered monitor.
pub fn shown_file(config: &[u8]) -> Result<Option<PathBuf>> {
    let config: Value = serde_json::from_slice(config).context("not JSON")?;
    let Some(selected) = config
        .as_object()
        .into_iter()
        .flat_map(|users| users.values())
        .find_map(|user| user.pointer("/general/wallpaperconfig/selectedwallpapers")?.as_object())
    else {
        return Ok(None);
    };
    let first = selected
        .iter()
        .filter_map(|(monitor, choice)| Some((monitor_number(monitor)?, choice.get("file")?.as_str()?)))
        .filter(|(_, file)| !file.is_empty())
        .min_by_key(|(number, _)| *number);
    Ok(first.map(|(_, file)| PathBuf::from(file)))
}

fn monitor_number(key: &str) -> Option<u32> {
    key.strip_prefix("Monitor")?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lowest_numbered_monitor_wins() {
        let config = br#"{
            "?installdirectory": "E:/SteamLibrary/steamapps/common/wallpaper_engine",
            "someone": { "general": { "wallpaperconfig": { "selectedwallpapers": {
                "Monitor10": { "file": "E:/w/3/a.mp4" },
                "Monitor2": { "file": "E:/w/2/scene.pkg" },
                "Monitor1": { "file": "E:/w/1/scene.pkg" }
            } } } }
        }"#;
        assert_eq!(shown_file(config).unwrap(), Some(PathBuf::from("E:/w/1/scene.pkg")));
    }

    #[test]
    fn nothing_selected_is_none() {
        assert_eq!(shown_file(br#"{ "someone": { "general": {} } }"#).unwrap(), None);
        assert_eq!(shown_file(br#"{ "u": { "general": { "wallpaperconfig": { "selectedwallpapers": { "Monitor0": { "file": "" } } } } } }"#).unwrap(), None);
        assert_eq!(shown_file(br#"{ "u": { "general": { "wallpaperconfig": { "selectedwallpapers": { "Other": { "file": "x" } } } } } }"#).unwrap(), None);
    }

    #[test]
    fn a_broken_config_is_an_error() {
        assert!(shown_file(b"{ not json").is_err());
    }
}
