//! Each package's still: one frame drawn the way it first appears, kept as `preview.png` (widgets) or
//! `preview.jpg` (canvases; videos write theirs when converting) in its folder and packed into its
//! `.deck`. Rendered in the background when a package is created or saved, or when the app finds one
//! missing.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use base64::Engine;
use glassdeck_package::{PackageType, folder_preview};
use glassdeck_slang::Slang;
use serde::Serialize;
use specta::Type;
use tauri::AppHandle;
use tauri_specta::Event as _;

use crate::backgrounds::{self, BackgroundKind};
use crate::library::{self, Dims, Sizing};
use crate::preview::{self, Drawn, Session};

const WIDGET_STILL: &str = "preview.png";
const CANVAS_STILL: &str = "preview.jpg";
/// How long a widget may take to draw its first frame for a still
const FIRST_DRAW_LIMIT: Duration = Duration::from_secs(3);

/// A still on disk, with a stamp that changes whenever it's redrawn, so the webview doesn't show a stale copy.
#[derive(Clone, Debug, PartialEq, Serialize, Type)]
pub struct Still {
    pub path: String,
    pub stamp: u64,
}

/// The still at `path`, if there is one.
pub fn still_at(path: &Path) -> Option<Still> {
    let modified = fs::metadata(path).and_then(|m| m.modified()).ok()?;
    let stamp = modified.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
    Some(Still { path: path.to_string_lossy().into_owned(), stamp })
}

/// A package folder's still, if it has one.
pub fn still_of(folder: &Path) -> Option<Still> {
    folder_preview(folder).and_then(|preview| still_at(&preview.path))
}

/// The built-in gradient has no folder; its still sits beside the library instead.
fn built_in_still(root: &Path) -> PathBuf {
    root.parent().unwrap_or(root).join("stills").join(format!("{}.jpg", glassdeck_proto::BUILT_IN_BACKGROUND))
}

/// The built-in gradient's still, if it's been drawn.
pub fn built_in(root: &Path) -> Option<Still> {
    still_at(&built_in_still(root))
}

#[derive(Clone, Debug, Serialize, Type, tauri_specta::Event)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum StillUpdate {
    Ready { id: String, still: Still },
    Failed { id: String, reason: String },
}

pub struct Stills(Mutex<Sender<String>>);

impl Stills {
    pub fn start(app: AppHandle, native: Option<PathBuf>) -> Self {
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new().name("stills".into()).spawn(move || run(app, native, rx)).expect("starting the stills thread");
        Self(Mutex::new(tx))
    }

    /// Draws `id`'s still again, soon; asking twice before it's drawn draws it once.
    pub fn request(&self, id: String) {
        if self.0.lock().expect("the stills sender isn't poisoned").send(id).is_err() {
            eprintln!("the stills thread has stopped");
        }
    }
}

fn run(app: AppHandle, native: Option<PathBuf>, rx: Receiver<String>) {
    let mut slang: Option<Slang> = None;
    while let Ok(first) = rx.recv() {
        let wanted: BTreeSet<String> = std::iter::once(first).chain(rx.try_iter()).collect();
        for id in wanted {
            let update = match render(&id, native.as_deref(), &mut slang) {
                Ok(still) => StillUpdate::Ready { id, still },
                Err(e) => StillUpdate::Failed { id, reason: format!("{e:#}") },
            };
            if let Err(e) = update.emit(&app) {
                eprintln!("emitting a still: {e}");
            }
        }
    }
}

fn render(id: &str, native: Option<&Path>, slang: &mut Option<Slang>) -> Result<Still> {
    let root = library::root().map_err(|e| anyhow::anyhow!("{e:?}"))?;
    if id == glassdeck_proto::BUILT_IN_BACKGROUND {
        let path = built_in_still(&root);
        fs::create_dir_all(path.parent().expect("a file in a folder"))?;
        write(&path, &crate::canvas_preview::render_still(loaded(slang, native)?, &backgrounds::canvas_template(), 0.0)?)?;
        return still_at(&path).context("the still wasn't written");
    }
    let (qualified, folder) = library::package_folder(&root, id).map_err(|e| anyhow::anyhow!("{e:?}"))?;
    match qualified.package_type() {
        PackageType::Widget => {
            let size = match library::read_widget(&root, id).map_err(|e| anyhow::anyhow!("{e:?}"))?.widget.sizing {
                Sizing::Fixed { size } | Sizing::Range { size, .. } | Sizing::Any { size } => size,
            };
            write(&folder.join(WIDGET_STILL), &widget_still(&folder, size)?)?;
        }
        PackageType::Background => match backgrounds::kind_of(&folder) {
            Some(BackgroundKind::Canvas) => {
                let files = backgrounds::read_canvas(&root, id).map_err(|e| anyhow::anyhow!("{e:?}"))?.files;
                let at = glassdeck_package::DeckSource::read(&folder)?.0.preview_at.unwrap_or(0.0);
                write(&folder.join(CANVAS_STILL), &crate::canvas_preview::render_still(loaded(slang, native)?, &files, at)?)?;
            }
            Some(BackgroundKind::Video) => {}
            None => bail!("{id} is neither a canvas nor a video"),
        },
    }
    still_of(&folder).context("no still was written")
}

/// Slang, loaded the first time a canvas still needs it and kept.
fn loaded<'a>(slang: &'a mut Option<Slang>, native: Option<&Path>) -> Result<&'a Slang> {
    if slang.is_none() {
        let native = native.context("the native libraries' folder isn't known")?;
        *slang = Some(Slang::load(native).context("loading Slang")?);
    }
    Ok(slang.as_ref().expect("loaded above"))
}

/// Written beside, then moved over the old one, so a reader never sees half a file.
fn write(path: &Path, bytes: &[u8]) -> Result<()> {
    let staging = path.with_extension("tmp");
    fs::write(&staging, bytes).with_context(|| format!("writing {}", staging.display()))?;
    fs::rename(&staging, path).with_context(|| format!("moving {} into place", path.display()))
}

/// The widget's first frame at `size`, as a PNG, with option defaults and nothing playing.
fn widget_still(folder: &Path, size: Dims) -> Result<Vec<u8>> {
    let cx = Session::without_media(std::rc::Rc::new(|_| {}))?;
    let mut running = preview::show(&cx, "still", folder, &[], size, &serde_json::Map::new())?;
    let deadline = Instant::now() + FIRST_DRAW_LIMIT;
    loop {
        match preview::step(&cx, &mut running) {
            Drawn::Frame { png, .. } => return base64::engine::general_purpose::STANDARD.decode(png).context("decoding the frame"),
            Drawn::Stopped => bail!("the widget stopped: {}", running.reported.clone().unwrap_or_default()),
            Drawn::Nothing if Instant::now() >= deadline => bail!("the widget drew nothing within {FIRST_DRAW_LIMIT:?}"),
            Drawn::Nothing => std::thread::sleep(preview::wait_for(&running, Instant::now()).min(Duration::from_millis(20))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::{NewWidget, create_widget};

    #[test]
    fn a_canvas_still_is_one_drawn_frame() {
        let native = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../target/native");
        let mut slang = None;
        let Ok(slang) = loaded(&mut slang, Some(&native)) else {
            eprintln!("skipped: Slang isn't built in {} (run native/build.py)", native.display());
            return;
        };
        let jpeg = crate::canvas_preview::render_still(slang, &backgrounds::canvas_template(), 0.0).unwrap();
        assert_eq!(&jpeg[..3], &[0xFF, 0xD8, 0xFF], "a JPEG");
        let image = image::load_from_memory(&jpeg).unwrap();
        assert_eq!(image.width(), 480, "the still's width");
        let pixels = image.to_rgb8();
        assert!(pixels.pixels().any(|p| p.0 != [0, 0, 0]), "the gradient drew something");
        let later = crate::canvas_preview::render_still(slang, &backgrounds::canvas_template(), 20.0).unwrap();
        assert_ne!(jpeg, later, "a still drawn at another time shows another moment");
    }

    #[test]
    fn a_new_widget_draws_a_still_that_packs_into_its_deck() {
        let root = std::env::temp_dir().join(format!("glassdeck-stills-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        create_widget(&root, NewWidget { title: "Sky".into(), id: "local.user.sky".into(), sizing: Sizing::Any { size: Dims { w: 320, h: 160 } } }).unwrap();
        let folder = root.join("widget.local.user.sky");
        let png = widget_still(&folder, Dims { w: 320, h: 160 }).unwrap();
        assert_eq!(&png[1..4], b"PNG");
        write(&folder.join(WIDGET_STILL), &png).unwrap();
        assert!(still_of(&folder).is_some());
        let deck = glassdeck_package::pack_folder(&folder, &root.join("sky.deck")).unwrap();
        assert_eq!(deck.manifest().preview.as_deref(), Some(WIDGET_STILL), "the still is the package's preview");
        fs::remove_dir_all(&root).unwrap();
    }
}
