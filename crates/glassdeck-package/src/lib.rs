//! A `.deckbg`: one ZIP file of stored entries, each aligned to 4 KB and read in place, never extracted.
//! The PC streams from it, and the keyboard keeps a copy to play on its own. Backgrounds are replaced whole.

mod manifest;
mod module;
mod resource;
mod canvas;
mod zip;

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};

pub use manifest::{Content, FrameRate, Manifest, Origin, ProbedCodec, ProbedVideo, SpecMismatch, VideoFormat, check};
pub use module::{
    ASSETS_FOLDER, HOST_SCRIPT, KEYBOARD_SCRIPT, KeyboardModule, MODULE_EXTENSION, MODULE_FORMAT_VERSION, MODULE_MANIFEST, ModuleManifest,
    ModulePackage, ModuleWriter, OPTIONS_SCRIPT, Resize, Size, is_module, pack_folder,
};
pub use resource::{Resource, ResourceKind, ResourceName, literal_resources};
pub use canvas::{
    InputField, InputKind, CANVAS_API, CANVAS_FRAGMENT, CANVAS_HOST, CANVAS_OPTIONS, CANVAS_PLAN, CANVAS_SCRIPT, CanvasFiles, CanvasPlan,
    CanvasScripts, canvas_sources, source_digest,
};
pub use zip::{ALIGNMENT, Entry, check_name};

pub const EXTENSION: &str = "deckbg";
pub const MANIFEST: &str = "manifest.json";
pub const FORMAT_VERSION: u32 = 2;
/// The Flux panel; every background video is exactly this size, so nothing scales on the keyboard.
pub const PANEL: (u16, u16) = (1920, 1080);
const MAX_MANIFEST_LEN: u64 = 16 << 20;

/// An opened background whose manifest parsed and whose named entries are all present.
pub struct Background {
    path: PathBuf,
    file: File,
    manifest: Manifest,
    entries: HashMap<String, Entry>,
}

impl Background {
    pub fn open(path: &Path) -> Result<Self> {
        let mut file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
        let entries = zip::read_directory(&mut file).with_context(|| format!("reading {}", path.display()))?;
        ensure!(!entries.contains_key(MODULE_MANIFEST), "{} is a module, not a background", path.display());
        let raw = entries.get(MANIFEST).copied().with_context(|| format!("{} has no {MANIFEST}", path.display()))?;
        ensure!(raw.len <= MAX_MANIFEST_LEN, "{MANIFEST} is too large");
        let mut text = vec![0; raw.len as usize];
        read_exact_at(&file, raw.offset, &mut text).with_context(|| format!("reading {}", path.display()))?;
        let manifest: Manifest = serde_json::from_slice(&text).with_context(|| format!("parsing {} in {}", MANIFEST, path.display()))?;
        manifest.validate(|name| entries.contains_key(name)).with_context(|| format!("checking {}", path.display()))?;
        Ok(Self { path: path.to_path_buf(), file, manifest, entries })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    pub fn entry(&self, name: &str) -> Option<Entry> {
        self.entries.get(name).copied()
    }

    pub fn entry_names(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(String::as_str)
    }

    /// Reads from `at` bytes into `entry`, up to `buf.len()` or the entry's end; 0 means the end.
    pub fn read_range(&self, entry: Entry, at: u64, buf: &mut [u8]) -> Result<usize> {
        if at >= entry.len {
            return Ok(0);
        }
        let want = buf.len().min((entry.len - at) as usize);
        read_at(&self.file, entry.offset + at, &mut buf[..want]).with_context(|| format!("reading {}", self.path.display()))
    }

    pub fn read_entry(&self, entry: Entry) -> Result<Vec<u8>> {
        let mut data = vec![0; usize::try_from(entry.len)?];
        read_exact_at(&self.file, entry.offset, &mut data).with_context(|| format!("reading {}", self.path.display()))?;
        Ok(data)
    }
}

/// Builds `<path>.tmp` with the manifest first, then renames it over `path`.
pub struct BackgroundWriter {
    path: PathBuf,
    staging: PathBuf,
    manifest: Manifest,
    zip: zip::ZipWriter,
}

impl BackgroundWriter {
    pub fn create(path: &Path, manifest: Manifest) -> Result<Self> {
        ensure!(manifest.format == FORMAT_VERSION, "manifests are written as format {FORMAT_VERSION}");
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        let staging = staging_path(path)?;
        let file = File::create(&staging).with_context(|| format!("creating {}", staging.display()))?;
        let mut zip = zip::ZipWriter::new(file);
        zip.add(MANIFEST, &mut serde_json::to_vec_pretty(&manifest)?.as_slice())?;
        Ok(Self { path: path.to_path_buf(), staging, manifest, zip })
    }

    pub fn add_bytes(&mut self, name: &str, data: &[u8]) -> Result<()> {
        self.zip.add(name, &mut &data[..])
    }

    pub fn add_file(&mut self, name: &str, source: &Path) -> Result<()> {
        let mut file = File::open(source).with_context(|| format!("opening {}", source.display()))?;
        self.zip.add(name, &mut file).with_context(|| format!("adding {}", source.display()))
    }

    /// Checks the manifest's entries were all added, then replaces any background at the path.
    pub fn finish(self) -> Result<Background> {
        let names: Vec<&str> = self.zip.names().collect();
        self.manifest.validate(|name| names.contains(&name))?;
        drop(self.zip.finish()?);
        if self.path.is_dir() {
            bail!("{} is a folder (an old-style background); delete it and convert again", self.path.display());
        }
        fs::rename(&self.staging, &self.path).with_context(|| format!("moving {} into place", self.path.display()))?;
        Background::open(&self.path)
    }
}

/// A preview entry name that keeps its image extension.
pub fn preview_name(source: &Path) -> String {
    match source.extension().and_then(|e| e.to_str()) {
        Some(ext) => format!("preview.{}", ext.to_ascii_lowercase()),
        None => "preview".to_owned(),
    }
}

pub fn is_background(path: &Path) -> bool {
    path.is_file() && path.extension().is_some_and(|e| e.eq_ignore_ascii_case(EXTENSION))
}

fn staging_path(path: &Path) -> Result<PathBuf> {
    let mut name = path.file_name().with_context(|| format!("{} has no file name", path.display()))?.to_os_string();
    name.push(".tmp");
    Ok(path.with_file_name(name))
}

fn read_exact_at(file: &File, offset: u64, buf: &mut [u8]) -> Result<()> {
    let mut filled = 0;
    while filled < buf.len() {
        let n = read_at(file, offset + filled as u64, &mut buf[filled..])?;
        ensure!(n > 0, "the file ended early");
        filled += n;
    }
    Ok(())
}

#[cfg(windows)]
fn read_at(file: &File, offset: u64, buf: &mut [u8]) -> std::io::Result<usize> {
    std::os::windows::fs::FileExt::seek_read(file, buf, offset)
}

#[cfg(unix)]
fn read_at(file: &File, offset: u64, buf: &mut [u8]) -> std::io::Result<usize> {
    std::os::unix::fs::FileExt::read_at(file, buf, offset)
}

/// Reads all of `entry` through `read_range`, in pieces, for callers holding only a reader.
pub struct EntryReader<'a> {
    background: &'a Background,
    entry: Entry,
    at: u64,
}

impl<'a> EntryReader<'a> {
    pub fn new(background: &'a Background, entry: Entry) -> Self {
        Self { background, entry, at: 0 }
    }
}

impl Read for EntryReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.background.read_range(self.entry, self.at, buf).map_err(std::io::Error::other)?;
        self.at += n as u64;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use glassdeck_proto::Codec;
    use serde_json::json;

    use super::*;

    const SIXTY: FrameRate = FrameRate { numerator: 60, denominator: 1 };

    pub(crate) struct Scratch(pub PathBuf);

    impl Scratch {
        pub(crate) fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("glassdeck-package-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn video_manifest() -> Manifest {
        Manifest {
            format: FORMAT_VERSION,
            title: "LateNight1".into(),
            origin: Origin::WallpaperEngine { workshop_id: "1184945377".into() },
            content: Content::Video {
                file: "video.mp4".into(),
                format: VideoFormat { codec: Codec::Hevc, width: 1920, height: 1080, frame_rate: SIXTY },
            },
            preview: Some("preview.jpg".into()),
        }
    }

    pub(crate) fn web_manifest() -> Manifest {
        Manifest {
            format: FORMAT_VERSION,
            title: "Matrix".into(),
            origin: Origin::WallpaperEngine { workshop_id: "1129194457".into() },
            content: Content::Web {
                entry: "web/index.html".into(),
                properties: json!({ "speed": { "type": "slider", "value": 3 } }).as_object().unwrap().clone(),
            },
            preview: None,
        }
    }

    #[test]
    fn round_trip_keeps_manifest_and_bytes_at_aligned_offsets() {
        let scratch = Scratch::new("round-trip");
        let path = scratch.0.join("late.deckbg");
        let video: Vec<u8> = (0..10_000u32).map(|i| i as u8).collect();
        let mut w = BackgroundWriter::create(&path, video_manifest()).unwrap();
        w.add_bytes("preview.jpg", b"jpeg").unwrap();
        w.add_bytes("video.mp4", &video).unwrap();
        let background = w.finish().unwrap();

        assert_eq!(background.manifest(), &video_manifest());
        for name in [MANIFEST, "preview.jpg", "video.mp4"] {
            assert_eq!(background.entry(name).unwrap().offset % ALIGNMENT, 0, "{name}");
        }
        assert_eq!(background.read_entry(background.entry("video.mp4").unwrap()).unwrap(), video);
        assert!(!scratch.0.join("late.deckbg.tmp").exists());
    }

    #[test]
    fn range_reads_stop_at_the_entry_end() {
        let scratch = Scratch::new("range");
        let path = scratch.0.join("web.deckbg");
        let mut w = BackgroundWriter::create(&path, web_manifest()).unwrap();
        w.add_bytes("web/index.html", b"<html>hello</html>").unwrap();
        w.add_bytes("web/next.txt", b"NEXT").unwrap();
        let background = w.finish().unwrap();

        let page = background.entry("web/index.html").unwrap();
        let mut buf = [0u8; 64];
        let n = background.read_range(page, 6, &mut buf).unwrap();
        assert_eq!(&buf[..n], b"hello</html>");
        assert_eq!(background.read_range(page, page.len, &mut buf).unwrap(), 0);
        let mut small = [0u8; 4];
        assert_eq!(background.read_range(page, 1, &mut small).unwrap(), 4);
        assert_eq!(&small, b"html");
    }

    #[test]
    fn a_manifest_naming_a_missing_entry_is_refused_before_replacing() {
        let scratch = Scratch::new("missing");
        let path = scratch.0.join("late.deckbg");
        let w = BackgroundWriter::create(&path, video_manifest()).unwrap();
        assert!(w.finish().is_err());
        assert!(!path.exists());
    }

    #[test]
    fn a_deflated_entry_is_refused_by_name() {
        let scratch = Scratch::new("deflated");
        let path = scratch.0.join("late.deckbg");
        let mut w = BackgroundWriter::create(&path, web_manifest()).unwrap();
        w.add_bytes("web/index.html", b"<html></html>").unwrap();
        w.finish().unwrap();

        let mut bytes = fs::read(&path).unwrap();
        let central = bytes.windows(4).rposition(|w| w == 0x0201_4b50u32.to_le_bytes()).unwrap();
        bytes[central + 10] = 8;
        File::create(&path).unwrap().write_all(&bytes).unwrap();

        let error = format!("{:#}", Background::open(&path).err().unwrap());
        assert!(error.contains("web/index.html") && error.contains("compressed"), "{error}");
    }

    #[test]
    fn writing_replaces_an_existing_background() {
        let scratch = Scratch::new("replace");
        let path = scratch.0.join("web.deckbg");
        for body in [&b"first"[..], b"second"] {
            let mut w = BackgroundWriter::create(&path, web_manifest()).unwrap();
            w.add_bytes("web/index.html", body).unwrap();
            w.finish().unwrap();
        }
        let background = Background::open(&path).unwrap();
        assert_eq!(background.read_entry(background.entry("web/index.html").unwrap()).unwrap(), b"second");
    }
}
