//! A widget's keyboard half: `keyboard.luau` in its own Luau VM, drawn on the CPU into a pixel tile.
//! The keyboard's receiver runs it for every placed widget; the desktop app runs it for its preview,
//! so the preview is exactly what the keyboard draws.

mod draw;
mod installing;
mod instance;
mod layout;
mod live;
mod text;
mod ui;

use std::path::Path;
use std::rc::Rc;
use std::time::SystemTime;

use anyhow::{Context, Result, anyhow};
use fontdue::{Font, FontSettings};
use glassdeck_package::Package;

pub use installing::Installing;
pub use instance::{DrawnTile, Instance};
pub use live::LiveCache;

/// Noto Sans JP Bold (SIL Open Font License, `fonts/OFL.txt`): Latin, Cyrillic and Japanese.
const BUILT_IN_FONT: &[u8] = include_bytes!("../fonts/NotoSansJP-Bold.otf");

/// The font at `path`, or the built-in one.
pub fn load_font(path: Option<&Path>) -> Result<Font> {
    let bytes = match path {
        Some(path) => std::fs::read(path).with_context(|| format!("reading font {}", path.display()))?,
        None => BUILT_IN_FONT.to_vec(),
    };
    Font::from_bytes(bytes, FontSettings::default()).map_err(|e| anyhow!("parsing the font: {e}"))
}

/// A widget package, and when the file was written, so a rewrite counts as a new version.
#[derive(Clone)]
pub struct Installed {
    pub package: Rc<Package>,
    pub modified: SystemTime,
}

impl Installed {
    pub fn same_version(&self, other: &Installed) -> bool {
        self.package.path() == other.package.path() && self.modified == other.modified
    }
}
