//! A `.deck`: one ZIP file of stored entries, each aligned to 4 KB and read in place, never extracted.
//! `manifest.json` says what the package is; scripts and resources are found by name. Packages are
//! replaced whole.

mod canvas;
mod folder;
mod manifest;
mod network;
mod resource;
mod zip;

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};

pub use canvas::{
    CANVAS_API, Canvas, CanvasFiles, CanvasScripts, InputField, InputKind, MAX_PASSES, MAX_TARGETS, Pass, RenderTarget, TargetFormat, canvas_sources,
    source_digest,
};
pub use folder::{DECK_SOURCE, DeckSource, is_blank_script, pack_folder, read_script};
pub use network::NetworkHost;
pub use manifest::{
    Content, FrameRate, Kind, Manifest, Origin, PackageId, PackageType, ProbedCodec, QualifiedId, ProbedVideo, Resize, Size, SpecMismatch, Version, VideoFormat,
    WidgetSize, check,
};
pub use resource::{Resource, ResourceKind, ResourceName, literal_resources};
pub use zip::{ALIGNMENT, Entry, check_name};

pub const EXTENSION: &str = "deck";
pub const MANIFEST: &str = "manifest.json";
pub const FORMAT_VERSION: u32 = 4;
pub const KEYBOARD_SCRIPT: &str = "keyboard.luau";
pub const HOST_SCRIPT: &str = "host.luau";
/// What the user can set, declared in Luau; only the host runs it, so it never reaches the keyboard.
pub const OPTIONS_SCRIPT: &str = "options.luau";
/// The messages between the package's two halves, as Luau types; read by both, so on both sides
pub const BRIDGE_SCRIPT: &str = "bridge.luau";
/// A canvas's compiled GLSL ES 3.00 fragment shader
pub const SHADER: &str = "shader.essl";

/// Pass `n`'s fragment shader in a canvas package.
pub fn pass_shader(n: usize) -> String {
    format!("pass{n}.essl")
}
pub const ASSETS_FOLDER: &str = "assets";
/// The Flux panel; every background video is exactly this size, so nothing scales on the keyboard.
pub const PANEL: (u16, u16) = (1920, 1080);
const MAX_MANIFEST_LEN: u64 = 16 << 20;
const MAX_TEXT_LEN: u64 = 16 << 20;

/// Where a package's bytes are.
enum Source {
    File { path: PathBuf, file: File },
    /// Built into a program, such as the built-in gradient
    BuiltIn(&'static [u8]),
}

impl Source {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Self::File { file, .. } => read_at(file, offset, buf),
            Self::BuiltIn(bytes) => {
                let start = usize::try_from(offset).unwrap_or(usize::MAX).min(bytes.len());
                let n = buf.len().min(bytes.len() - start);
                buf[..n].copy_from_slice(&bytes[start..start + n]);
                Ok(n)
            }
        }
    }

    fn describe(&self) -> String {
        match self {
            Self::File { path, .. } => path.display().to_string(),
            Self::BuiltIn(_) => "the built-in package".to_owned(),
        }
    }
}

/// An opened package whose manifest parsed and whose needed entries are all present.
pub struct Package {
    source: Source,
    manifest: Manifest,
    id: QualifiedId,
    entries: HashMap<String, Entry>,
}

impl Package {
    pub fn open(path: &Path) -> Result<Self> {
        let mut file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
        let entries = zip::read_directory(&mut file).with_context(|| format!("reading {}", path.display()))?;
        Self::from_parts(Source::File { path: path.to_path_buf(), file }, entries)
    }

    pub fn built_in(bytes: &'static [u8]) -> Result<Self> {
        let entries = zip::read_directory(&mut Cursor::new(bytes)).context("reading the built-in package")?;
        Self::from_parts(Source::BuiltIn(bytes), entries)
    }

    fn from_parts(source: Source, entries: HashMap<String, Entry>) -> Result<Self> {
        let raw = entries.get(MANIFEST).copied().with_context(|| format!("{} has no {MANIFEST}", source.describe()))?;
        ensure!(raw.len <= MAX_MANIFEST_LEN, "{MANIFEST} is too large");
        let mut text = vec![0; raw.len as usize];
        read_exact_at(&source, raw.offset, &mut text).with_context(|| format!("reading {}", source.describe()))?;
        let manifest: Manifest = serde_json::from_slice(&text).with_context(|| format!("parsing {MANIFEST} in {}", source.describe()))?;
        manifest.validate(|name| entries.contains_key(name)).with_context(|| format!("checking {}", source.describe()))?;
        let id = manifest.qualified_id();
        Ok(Self { source, manifest, id, entries })
    }

    /// `None` for a built-in package.
    pub fn path(&self) -> Option<&Path> {
        match &self.source {
            Source::File { path, .. } => Some(path),
            Source::BuiltIn(_) => None,
        }
    }

    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    pub fn id(&self) -> &QualifiedId {
        &self.id
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
        self.source.read_at(entry.offset + at, &mut buf[..want]).with_context(|| format!("reading {}", self.source.describe()))
    }

    pub fn read_entry(&self, entry: Entry) -> Result<Vec<u8>> {
        let mut data = vec![0; usize::try_from(entry.len)?];
        read_exact_at(&self.source, entry.offset, &mut data).with_context(|| format!("reading {}", self.source.describe()))?;
        Ok(data)
    }

    pub fn bytes(&self, name: &str) -> Result<Vec<u8>> {
        let entry = self.entry(name).with_context(|| format!("{} has no {name}", self.manifest.id))?;
        self.read_entry(entry)
    }

    /// A text entry such as a script; `None` when the package doesn't have it.
    pub fn text(&self, name: &str) -> Result<Option<String>> {
        let Some(entry) = self.entry(name) else { return Ok(None) };
        ensure!(entry.len <= MAX_TEXT_LEN, "{name} in {} is too large", self.manifest.id);
        let text = String::from_utf8(self.read_entry(entry)?).with_context(|| format!("{name} in {} isn't UTF-8", self.manifest.id))?;
        Ok(Some(text))
    }

    pub fn keyboard_script(&self) -> Result<Option<String>> {
        self.text(KEYBOARD_SCRIPT)
    }

    pub fn bridge_script(&self) -> Result<Option<String>> {
        self.text(BRIDGE_SCRIPT)
    }

    pub fn options_script(&self) -> Result<Option<String>> {
        self.text(OPTIONS_SCRIPT)
    }

    pub fn host_script(&self) -> Result<Option<String>> {
        self.text(HOST_SCRIPT)
    }

    pub fn resource(&self, resource: &Resource) -> Result<Vec<u8>> {
        let full = resource.entry();
        ensure!(self.entries.contains_key(&full), "{} has no {full}, which {resource} names", self.manifest.id);
        self.bytes(&full)
    }

    /// Whether the host runs part of this package: it has options or a host half.
    pub fn runs_on_host(&self) -> bool {
        self.entries.contains_key(OPTIONS_SCRIPT) || self.entries.contains_key(HOST_SCRIPT)
    }

    /// Writes the package as the keyboard gets it: everything but what only the host uses,
    /// which is `options.luau`, `host.luau` and `assets/host/`.
    pub fn write_keyboard_copy(&self, out: &Path) -> Result<Package> {
        let host_assets = format!("{ASSETS_FOLDER}/{}/", ResourceKind::Host.folder());
        let mut names: Vec<&String> = self
            .entries
            .keys()
            .filter(|name| ![MANIFEST, OPTIONS_SCRIPT, HOST_SCRIPT].contains(&name.as_str()) && !name.starts_with(&host_assets))
            .collect();
        names.sort();
        let mut writer = PackageWriter::create(out, self.manifest.clone())?;
        for name in names {
            writer.add_bytes(name, &self.bytes(name)?).with_context(|| format!("copying {name}"))?;
        }
        writer.finish()
    }
}

/// Builds `<path>.tmp` with the manifest first, then renames it over `path`.
pub struct PackageWriter {
    path: PathBuf,
    staging: PathBuf,
    manifest: Manifest,
    zip: zip::ZipWriter,
}

impl PackageWriter {
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
        ensure!(name != MANIFEST, "{MANIFEST} is written from the manifest");
        self.zip.add(name, &mut &data[..])
    }

    pub fn add_file(&mut self, name: &str, source: &Path) -> Result<()> {
        ensure!(name != MANIFEST, "{MANIFEST} is written from the manifest");
        let mut file = File::open(source).with_context(|| format!("opening {}", source.display()))?;
        self.zip.add(name, &mut file).with_context(|| format!("adding {}", source.display()))
    }

    /// Checks the package holds what its type needs, then replaces any package at the path.
    pub fn finish(self) -> Result<Package> {
        let names: Vec<&str> = self.zip.names().collect();
        self.manifest.validate(|name| names.contains(&name))?;
        drop(self.zip.finish()?);
        fs::rename(&self.staging, &self.path).with_context(|| format!("moving {} into place", self.path.display()))?;
        Package::open(&self.path)
    }
}

/// A preview entry name that keeps its image extension.
pub fn preview_name(source: &Path) -> String {
    match source.extension().and_then(|e| e.to_str()) {
        Some(ext) => format!("preview.{}", ext.to_ascii_lowercase()),
        None => "preview".to_owned(),
    }
}

/// A package's file name in a packages folder, `<qualified id>.deck`, so nothing can name anything outside it.
/// The one rule for every package the keyboard installs or selects.
pub fn check_file_name(name: &str) -> Result<QualifiedId> {
    let stem = name.strip_suffix(&format!(".{EXTENSION}")).with_context(|| format!("{name:?} does not end in .{EXTENSION}"))?;
    QualifiedId::parse(stem).with_context(|| format!("{name:?} is not <qualified id>.{EXTENSION}"))
}

/// A digest of a file's bytes (FNV-1a, 64-bit), so host and keyboard can tell whether they hold the same copy.
pub fn file_digest(path: &Path) -> Result<u64> {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut hash = FNV_OFFSET;
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf).with_context(|| format!("reading {}", path.display()))?;
        if n == 0 {
            return Ok(hash);
        }
        for &byte in &buf[..n] {
            hash = (hash ^ u64::from(byte)).wrapping_mul(FNV_PRIME);
        }
    }
}

pub fn is_package(path: &Path) -> bool {
    path.is_file() && path.extension().is_some_and(|e| e.eq_ignore_ascii_case(EXTENSION))
}

pub(crate) fn staging_path(path: &Path) -> Result<PathBuf> {
    let mut name = path.file_name().with_context(|| format!("{} has no file name", path.display()))?.to_os_string();
    name.push(".tmp");
    Ok(path.with_file_name(name))
}

fn read_exact_at(source: &Source, offset: u64, buf: &mut [u8]) -> Result<()> {
    let mut filled = 0;
    while filled < buf.len() {
        let n = source.read_at(offset + filled as u64, &mut buf[filled..])?;
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
    package: &'a Package,
    entry: Entry,
    at: u64,
}

impl<'a> EntryReader<'a> {
    pub fn new(package: &'a Package, entry: Entry) -> Self {
        Self { package, entry, at: 0 }
    }
}

impl Read for EntryReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.package.read_range(self.entry, self.at, buf).map_err(std::io::Error::other)?;
        self.at += n as u64;
        Ok(n)
    }
}

#[cfg(test)]
pub(crate) mod tests {
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

    pub(crate) fn manifest(id: &str, kind: Kind) -> Manifest {
        Manifest {
            format: FORMAT_VERSION,
            id: PackageId::parse(id).unwrap(),
            title: "Test".into(),
            version: Version::parse("1.0.0").unwrap(),
            origin: Origin::WallpaperEngine { workshop_id: "1".into() },
            preview: None,
            network: Vec::new(),
            kind,
        }
    }

    fn video_manifest() -> Manifest {
        let video = Content::Video {
            file: "video.mp4".into(),
            format: VideoFormat { codec: Codec::Hevc, width: 1920, height: 1080, frame_rate: SIXTY },
        };
        Manifest { preview: Some("preview.jpg".into()), ..manifest("com.wallpaperengine.1184945377", Kind::Background(video)) }
    }

    pub(crate) fn web_manifest() -> Manifest {
        let web = Content::Web {
            entry: "web/index.html".into(),
            properties: json!({ "speed": { "type": "slider", "value": 3 } }).as_object().unwrap().clone(),
        };
        manifest("com.wallpaperengine.1129194457", Kind::Background(web))
    }

    #[test]
    fn round_trip_keeps_manifest_and_bytes_at_aligned_offsets() {
        let scratch = Scratch::new("round-trip");
        let path = scratch.0.join("late.deck");
        let video: Vec<u8> = (0..10_000u32).map(|i| i as u8).collect();
        let mut w = PackageWriter::create(&path, video_manifest()).unwrap();
        w.add_bytes("preview.jpg", b"jpeg").unwrap();
        w.add_bytes("video.mp4", &video).unwrap();
        let package = w.finish().unwrap();

        assert_eq!(package.manifest(), &video_manifest());
        for name in [MANIFEST, "preview.jpg", "video.mp4"] {
            assert_eq!(package.entry(name).unwrap().offset % ALIGNMENT, 0, "{name}");
        }
        assert_eq!(package.bytes("video.mp4").unwrap(), video);
        assert!(!scratch.0.join("late.deck.tmp").exists());
        let built_in = Package::built_in(Box::leak(fs::read(&path).unwrap().into_boxed_slice())).unwrap();
        assert_eq!(built_in.bytes("video.mp4").unwrap(), video);
        assert_eq!(built_in.path(), None);
    }

    #[test]
    fn range_reads_stop_at_the_entry_end() {
        let scratch = Scratch::new("range");
        let path = scratch.0.join("web.deck");
        let mut w = PackageWriter::create(&path, web_manifest()).unwrap();
        w.add_bytes("web/index.html", b"<html>hello</html>").unwrap();
        w.add_bytes("web/next.txt", b"NEXT").unwrap();
        let package = w.finish().unwrap();

        let page = package.entry("web/index.html").unwrap();
        let mut buf = [0u8; 64];
        let n = package.read_range(page, 6, &mut buf).unwrap();
        assert_eq!(&buf[..n], b"hello</html>");
        assert_eq!(package.read_range(page, page.len, &mut buf).unwrap(), 0);
        let mut small = [0u8; 4];
        assert_eq!(package.read_range(page, 1, &mut small).unwrap(), 4);
        assert_eq!(&small, b"html");
    }

    #[test]
    fn a_package_missing_what_its_type_needs_is_refused_before_replacing() {
        let scratch = Scratch::new("missing");
        let path = scratch.0.join("late.deck");
        let w = PackageWriter::create(&path, video_manifest()).unwrap();
        assert!(w.finish().is_err());
        assert!(!path.exists());
    }

    #[test]
    fn a_deflated_entry_is_refused_by_name() {
        let scratch = Scratch::new("deflated");
        let path = scratch.0.join("web.deck");
        let mut w = PackageWriter::create(&path, web_manifest()).unwrap();
        w.add_bytes("web/index.html", b"<html></html>").unwrap();
        w.finish().unwrap();

        let mut bytes = fs::read(&path).unwrap();
        let central = bytes.windows(4).rposition(|w| w == 0x0201_4b50u32.to_le_bytes()).unwrap();
        bytes[central + 10] = 8;
        File::create(&path).unwrap().write_all(&bytes).unwrap();

        let error = format!("{:#}", Package::open(&path).err().unwrap());
        assert!(error.contains("web/index.html") && error.contains("compressed"), "{error}");
    }

    #[test]
    fn the_digest_follows_the_bytes() {
        let scratch = Scratch::new("digest");
        let (a, b) = (scratch.0.join("a"), scratch.0.join("b"));
        fs::write(&a, b"same").unwrap();
        fs::write(&b, b"same").unwrap();
        assert_eq!(file_digest(&a).unwrap(), file_digest(&b).unwrap());
        fs::write(&b, b"other").unwrap();
        assert_ne!(file_digest(&a).unwrap(), file_digest(&b).unwrap());
    }

    #[test]
    fn file_names_are_ids() {
        assert_eq!(check_file_name("widget.local.user.clock.deck").unwrap().as_str(), "widget.local.user.clock");
        for bad in ["clock.deck", "local.user.clock.deck", "widget.local.user.clock.deckbg", "../widget.a.b.deck", "widget.A.b.c.deck", "widget.x!.y.z.deck"] {
            assert!(check_file_name(bad).is_err(), "{bad}");
        }
    }
}
