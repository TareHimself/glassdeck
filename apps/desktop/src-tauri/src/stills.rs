//! Each package's still: one frame drawn the way it first appears, kept in the app's thumbnail cache
//! beside the library rather than in the package, so a `.deck` stays the author's source. A still is
//! named for what it was drawn from (the package's files as they are, and this app's version), so an
//! edit or an upgrade makes it missing, and the app draws it again in the background.

use std::collections::BTreeSet;
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use base64::Engine;
use glassdeck_package::PackageType;
use glassdeck_slang::Slang;
use serde::Serialize;
use specta::Type;
use tauri::AppHandle;
use tauri_specta::Event as _;

use crate::backgrounds::{self, BackgroundKind};
use crate::library::{self, Dims, Sizing};
use crate::preview::{self, Drawn, Session};

const THUMBNAILS: &str = "thumbnails";
/// Stills drawn by another version of the app are drawn again.
const DRAWN_BY: &str = env!("CARGO_PKG_VERSION");
/// Widgets keep transparency; canvases and videos are opaque.
const WIDGET_STILL: &str = "png";
const OPAQUE_STILL: &str = "jpg";
/// Files a package folder holds while it's being written, which don't change what it draws
const IN_PROGRESS: [&str; 2] = [".tmp", ".converting.mp4"];
/// A picture's still: as wide as the canvas and video ones
const STILL_WIDTH: u32 = 480;
const STILL_QUALITY: u8 = 85;
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

/// The cache folder, beside the library; the app allows the webview to read it.
pub fn thumbnails(root: &Path) -> PathBuf {
    root.parent().unwrap_or(root).join(THUMBNAILS)
}

/// A library folder's root and package id: packages live at `<root>/<id>`.
fn placed(folder: &Path) -> Option<(&Path, String)> {
    Some((folder.parent()?, folder.file_name()?.to_str()?.to_owned()))
}

/// What `id`'s still is drawn from, as a cache name: the app's version, and each source file's path,
/// size and time (no folder: the built-in gradient, which only changes with the app).
fn cache_name(id: &str, folder: Option<&Path>) -> Result<String> {
    let mut hasher = DefaultHasher::new();
    DRAWN_BY.hash(&mut hasher);
    if let Some(folder) = folder {
        for file in source_files(folder)? {
            file.hash(&mut hasher);
        }
    }
    Ok(format!("{id}.{:016x}", hasher.finish()))
}

/// Every file under `folder` with its size and modified time, in a stable order.
fn source_files(folder: &Path) -> Result<Vec<(String, u64, u128)>> {
    let mut files = Vec::new();
    let mut folders = vec![folder.to_path_buf()];
    while let Some(next) = folders.pop() {
        for entry in fs::read_dir(&next).with_context(|| format!("reading {}", next.display()))? {
            let entry = entry?;
            let meta = entry.metadata()?;
            if meta.is_dir() {
                folders.push(entry.path());
                continue;
            }
            let name = entry.path().strip_prefix(folder).unwrap_or(&entry.path()).to_string_lossy().replace('\\', "/");
            if IN_PROGRESS.iter().any(|suffix| name.ends_with(suffix)) {
                continue;
            }
            let modified = meta.modified()?.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis());
            files.push((name, meta.len(), modified));
        }
    }
    files.sort();
    Ok(files)
}

fn cached(root: &Path, name: &str) -> Option<Still> {
    let folder = thumbnails(root);
    [WIDGET_STILL, OPAQUE_STILL].iter().find_map(|ext| still_at(&folder.join(format!("{name}.{ext}"))))
}

/// A package folder's still, if one is drawn for it as it is now.
pub fn still_of(folder: &Path) -> Option<Still> {
    let (root, id) = placed(folder)?;
    cached(root, &cache_name(&id, Some(folder)).ok()?)
}

/// The built-in gradient's still, if it's been drawn.
pub fn built_in(root: &Path) -> Option<Still> {
    cached(root, &cache_name(glassdeck_proto::BUILT_IN_BACKGROUND, None).ok()?)
}

/// Keeps `bytes` as `id`'s still for `folder` as it is now, and drops the stills drawn before.
fn store(root: &Path, id: &str, folder: Option<&Path>, ext: &str, bytes: &[u8]) -> Result<Still> {
    let cache = thumbnails(root);
    fs::create_dir_all(&cache).with_context(|| format!("creating {}", cache.display()))?;
    let name = format!("{}.{ext}", cache_name(id, folder)?);
    let path = cache.join(&name);
    write(&path, bytes)?;
    let earlier = format!("{id}.");
    for entry in fs::read_dir(&cache)? {
        let entry = entry?;
        let other = entry.file_name().to_string_lossy().into_owned();
        if other.starts_with(&earlier) && other != name && !other.ends_with(".tmp") {
            fs::remove_file(entry.path()).with_context(|| format!("removing the old still {other}"))?;
        }
    }
    still_at(&path).context("the still wasn't written")
}

/// An image background's still: its picture, scaled down.
pub fn draw_image(folder: &Path) -> Result<Still> {
    let (root, id) = placed(folder).context("a package folder in the library")?;
    let picture = image::open(folder.join(crate::images::IMAGE_FILE)).context("reading the picture")?.to_rgb8();
    let (w, h) = picture.dimensions();
    let small = image::imageops::thumbnail(&picture, STILL_WIDTH, h * STILL_WIDTH / w.max(1));
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, STILL_QUALITY).encode_image(&small).context("encoding the still")?;
    store(root, &id, Some(folder), OPAQUE_STILL, &jpeg)
}

/// Draws a video background's still from its converted video, at its `preview_at`.
pub fn draw_video(folder: &Path) -> Result<Still> {
    let (root, id) = placed(folder).context("a package folder in the library")?;
    let at = library::read_deck_json(folder).map_err(|e| anyhow::anyhow!("{e:?}"))?.get("preview_at").and_then(|v| v.as_f64()).unwrap_or(0.0);
    store(root, &id, Some(folder), OPAQUE_STILL, &crate::videos::frame_jpeg(folder, at)?)
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
        let jpeg = crate::canvas_preview::render_still(loaded(slang, native)?, &backgrounds::canvas_template(), 0.0)?;
        return store(&root, id, None, OPAQUE_STILL, &jpeg);
    }
    let (qualified, folder) = library::package_folder(&root, id).map_err(|e| anyhow::anyhow!("{e:?}"))?;
    match qualified.package_type() {
        PackageType::Widget => {
            let size = match library::read_widget(&root, id).map_err(|e| anyhow::anyhow!("{e:?}"))?.widget.sizing {
                Sizing::Fixed { size } | Sizing::Range { size, .. } | Sizing::Any { size } => size,
            };
            store(&root, id, Some(&folder), WIDGET_STILL, &widget_still(&folder, size)?)
        }
        PackageType::Background => match backgrounds::kind_of(&folder) {
            Some(BackgroundKind::Canvas) => {
                let files = backgrounds::read_canvas(&root, id).map_err(|e| anyhow::anyhow!("{e:?}"))?.files;
                let at = glassdeck_package::DeckSource::read(&folder)?.0.preview_at.unwrap_or(0.0);
                store(&root, id, Some(&folder), OPAQUE_STILL, &crate::canvas_preview::render_still(loaded(slang, native)?, &files, at)?)
            }
            Some(BackgroundKind::Video) => draw_video(&folder),
            Some(BackgroundKind::Image) => draw_image(&folder),
            None => bail!("{id} is neither a canvas nor a video"),
        },
    }
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
    fn a_widget_still_is_cached_apart_from_its_deck_and_goes_stale_with_an_edit() {
        let base = std::env::temp_dir().join(format!("glassdeck-stills-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let root = base.join("library");
        create_widget(&root, NewWidget { title: "Sky".into(), id: "local.user.sky".into(), sizing: Sizing::Any { size: Dims { w: 320, h: 160 } } }).unwrap();
        let (id, folder) = ("widget.local.user.sky", root.join("widget.local.user.sky"));
        let png = widget_still(&folder, Dims { w: 320, h: 160 }).unwrap();
        assert_eq!(&png[1..4], b"PNG");
        let first = store(&root, id, Some(&folder), WIDGET_STILL, &png).unwrap();
        assert_eq!(still_of(&folder), Some(first.clone()));
        assert!(Path::new(&first.path).starts_with(base.join(THUMBNAILS)), "kept beside the library, not in the package");

        let deck = glassdeck_package::pack_folder(&folder, &base.join("sky.deck")).unwrap();
        assert_eq!(deck.manifest().preview, None, "the .deck carries no generated still");

        std::thread::sleep(Duration::from_millis(20));
        fs::write(folder.join("keyboard.luau"), "-- edited\n").unwrap();
        assert_eq!(still_of(&folder), None, "an edit makes the still missing");
        let second = store(&root, id, Some(&folder), WIDGET_STILL, &png).unwrap();
        assert_eq!(still_of(&folder), Some(second));
        assert!(!Path::new(&first.path).exists(), "the earlier still is dropped");
        fs::remove_dir_all(&base).unwrap();
    }
}
