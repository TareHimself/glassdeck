use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::project::{self, Project};
use crate::steam;

pub const WALLPAPER_ENGINE_APP_ID: &str = "431960";

/// A wallpaper folder on this PC: a subscribed workshop item, or any folder holding a `project.json`.
#[derive(Clone, Debug)]
pub struct Item {
    pub id: String,
    pub dir: PathBuf,
}

impl Item {
    pub fn project(&self) -> Result<Project> {
        project::read(&self.dir)
    }
}

/// Wallpaper Engine's workshop folder in each Steam library that has one.
pub fn workshop_dirs() -> Result<Vec<PathBuf>> {
    let libraries = steam::library_folders(&steam::steam_root()?)?;
    Ok(libraries
        .into_iter()
        .map(|library| library.join("steamapps").join("workshop").join("content").join(WALLPAPER_ENGINE_APP_ID))
        .filter(|dir| dir.is_dir())
        .collect())
}

pub fn subscribed() -> Result<Vec<Item>> {
    let mut items = Vec::new();
    for workshop in workshop_dirs()? {
        for entry in std::fs::read_dir(&workshop).with_context(|| format!("listing {}", workshop.display()))? {
            let entry = entry?;
            if entry.file_type()?.is_dir() && entry.path().join("project.json").is_file() {
                items.push(Item { id: entry.file_name().to_string_lossy().into_owned(), dir: entry.path() });
            }
        }
    }
    items.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(items)
}

/// A workshop ID among the subscribed items, or a path to a wallpaper folder.
pub fn find(query: &str) -> Result<Item> {
    let path = Path::new(query);
    if path.join("project.json").is_file() {
        let dir = std::path::absolute(path)?;
        let id = dir.file_name().context("wallpaper folder has no name")?.to_string_lossy().into_owned();
        return Ok(Item { id, dir });
    }
    if query.is_empty() || !query.bytes().all(|b| b.is_ascii_digit()) {
        bail!("{query:?} is neither a workshop ID nor a folder with a project.json");
    }
    for workshop in workshop_dirs()? {
        let dir = workshop.join(query);
        if dir.join("project.json").is_file() {
            return Ok(Item { id: query.to_owned(), dir });
        }
    }
    bail!("workshop item {query} is not installed; subscribe to it in Wallpaper Engine first")
}
