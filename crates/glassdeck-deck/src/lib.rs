//! A `.deck` bundle: one folder holding `manifest.json`, `video.mp4` and an optional preview image.
//! The PC streams the video from it, and the keyboard keeps a copy to play on its own.
//! Bundles are replaced whole, never patched.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use glassdeck_proto::Codec;
use serde::{Deserialize, Serialize};

pub const EXTENSION: &str = "deck";
pub const MANIFEST: &str = "manifest.json";
pub const VIDEO: &str = "video.mp4";
pub const FORMAT_VERSION: u32 = 1;
/// The Flux panel; every deck video is exactly this size, so nothing scales on the keyboard.
pub const PANEL: (u16, u16) = (1920, 1080);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub format: u32,
    pub title: String,
    pub origin: Origin,
    pub video: VideoFormat,
    /// File name inside the bundle, such as `preview.jpg`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Origin {
    WallpaperEngine { workshop_id: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoFormat {
    #[serde(with = "codec_name")]
    pub codec: Codec,
    pub width: u16,
    pub height: u16,
    pub frame_rate: FrameRate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameRate {
    pub numerator: u32,
    pub denominator: u32,
}

impl FrameRate {
    pub fn per_second(self) -> f64 {
        self.numerator as f64 / self.denominator.max(1) as f64
    }
}

/// Why a video cannot go into a deck as it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpecMismatch {
    Codec,
    Size { width: u16, height: u16 },
}

impl fmt::Display for SpecMismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Codec => write!(f, "the codec is not H.264 or HEVC"),
            Self::Size { width, height } => write!(f, "{width}x{height} is not the panel's {}x{}", PANEL.0, PANEL.1),
        }
    }
}

/// A probed video's codec, before we know whether the keyboard can decode it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbedCodec {
    Known(Codec),
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProbedVideo {
    pub codec: ProbedCodec,
    pub width: u16,
    pub height: u16,
    pub frame_rate: FrameRate,
}

/// The deck video spec: H.264 or HEVC at the panel's size.
pub fn check(video: ProbedVideo) -> Result<VideoFormat, SpecMismatch> {
    let ProbedCodec::Known(codec) = video.codec else {
        return Err(SpecMismatch::Codec);
    };
    if (video.width, video.height) != PANEL {
        return Err(SpecMismatch::Size { width: video.width, height: video.height });
    }
    Ok(VideoFormat { codec, width: video.width, height: video.height, frame_rate: video.frame_rate })
}

/// An opened bundle whose manifest parsed and whose files are present.
#[derive(Clone, Debug)]
pub struct Deck {
    pub dir: PathBuf,
    pub manifest: Manifest,
}

impl Deck {
    pub fn open(dir: &Path) -> Result<Self> {
        let path = dir.join(MANIFEST);
        let text = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let manifest: Manifest = serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        ensure!(
            manifest.format == FORMAT_VERSION,
            "{} is deck format {}, this build reads {FORMAT_VERSION}",
            dir.display(),
            manifest.format
        );
        if (manifest.video.width, manifest.video.height) != PANEL {
            bail!("{}: {}", dir.display(), SpecMismatch::Size { width: manifest.video.width, height: manifest.video.height });
        }
        let deck = Self { dir: dir.to_path_buf(), manifest };
        ensure!(deck.video().is_file(), "{} has no {VIDEO}", dir.display());
        if let Some(preview) = deck.preview() {
            ensure!(preview.is_file(), "{} names a preview it does not hold", dir.display());
        }
        Ok(deck)
    }

    pub fn video(&self) -> PathBuf {
        self.dir.join(VIDEO)
    }

    pub fn preview(&self) -> Option<PathBuf> {
        self.manifest.preview.as_ref().map(|name| self.dir.join(name))
    }

    /// Builds the bundle next to `dir` and swaps it in, so a reader never sees half a deck.
    pub fn write(dir: &Path, manifest: Manifest, video: &Path, preview: Option<&Path>) -> Result<Self> {
        ensure!(manifest.preview.is_some() == preview.is_some(), "manifest preview and preview file disagree");
        let staging = sibling(dir, "partial")?;
        if staging.exists() {
            fs::remove_dir_all(&staging).with_context(|| format!("clearing {}", staging.display()))?;
        }
        fs::create_dir_all(&staging).with_context(|| format!("creating {}", staging.display()))?;
        fs::copy(video, staging.join(VIDEO)).with_context(|| format!("copying {}", video.display()))?;
        if let (Some(from), Some(name)) = (preview, &manifest.preview) {
            fs::copy(from, staging.join(name)).with_context(|| format!("copying {}", from.display()))?;
        }
        fs::write(staging.join(MANIFEST), serde_json::to_string_pretty(&manifest)?)?;
        replace_dir(&staging, dir)?;
        Self::open(dir)
    }
}

/// A file name for the preview that keeps its image extension.
pub fn preview_name(source: &Path) -> String {
    match source.extension().and_then(|e| e.to_str()) {
        Some(ext) => format!("preview.{}", ext.to_ascii_lowercase()),
        None => "preview".to_owned(),
    }
}

pub fn is_deck(path: &Path) -> bool {
    path.is_dir() && path.join(MANIFEST).is_file()
}

fn sibling(dir: &Path, suffix: &str) -> Result<PathBuf> {
    let name = dir.file_name().with_context(|| format!("{} has no folder name", dir.display()))?;
    let mut name = name.to_os_string();
    name.push(format!(".{suffix}"));
    Ok(dir.with_file_name(name))
}

fn replace_dir(staging: &Path, dir: &Path) -> Result<()> {
    if dir.exists() {
        let old = sibling(dir, "old")?;
        if old.exists() {
            fs::remove_dir_all(&old)?;
        }
        fs::rename(dir, &old).with_context(|| format!("moving the old {} aside", dir.display()))?;
        fs::rename(staging, dir).with_context(|| format!("moving in the new {}", dir.display()))?;
        fs::remove_dir_all(&old).with_context(|| format!("removing {}", old.display()))?;
    } else {
        fs::rename(staging, dir).with_context(|| format!("moving in {}", dir.display()))?;
    }
    Ok(())
}

mod codec_name {
    use glassdeck_proto::Codec;
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S: Serializer>(codec: &Codec, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(match codec {
            Codec::H264 => "h264",
            Codec::Hevc => "hevc",
        })
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Codec, D::Error> {
        match String::deserialize(d)?.as_str() {
            "h264" => Ok(Codec::H264),
            "hevc" => Ok(Codec::Hevc),
            other => Err(D::Error::custom(format!("unknown codec {other:?}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIXTY: FrameRate = FrameRate { numerator: 60, denominator: 1 };

    fn probed(codec: ProbedCodec, width: u16, height: u16) -> ProbedVideo {
        ProbedVideo { codec, width, height, frame_rate: SIXTY }
    }

    fn manifest() -> Manifest {
        Manifest {
            format: FORMAT_VERSION,
            title: "LateNight1".into(),
            origin: Origin::WallpaperEngine { workshop_id: "1184945377".into() },
            video: VideoFormat { codec: Codec::Hevc, width: 1920, height: 1080, frame_rate: SIXTY },
            preview: Some("preview.jpg".into()),
        }
    }

    #[test]
    fn panel_sized_h264_and_hevc_meet_the_spec() {
        for codec in [Codec::H264, Codec::Hevc] {
            assert_eq!(check(probed(ProbedCodec::Known(codec), 1920, 1080)).unwrap().codec, codec);
        }
    }

    #[test]
    fn other_sizes_and_codecs_need_re_encoding() {
        assert_eq!(check(probed(ProbedCodec::Known(Codec::Hevc), 3840, 2160)), Err(SpecMismatch::Size { width: 3840, height: 2160 }));
        assert_eq!(check(probed(ProbedCodec::Other, 1920, 1080)), Err(SpecMismatch::Codec));
    }

    #[test]
    fn manifest_round_trips_with_readable_names() {
        let text = serde_json::to_string(&manifest()).unwrap();
        assert!(text.contains(r#""codec":"hevc""#), "{text}");
        assert!(text.contains(r#""kind":"wallpaper_engine""#), "{text}");
        assert_eq!(serde_json::from_str::<Manifest>(&text).unwrap(), manifest());
    }

    #[test]
    fn writing_replaces_the_whole_bundle() {
        let root = std::env::temp_dir().join(format!("glassdeck-deck-test-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let video = root.join("in.mp4");
        let preview = root.join("in.JPG");
        fs::write(&video, b"video").unwrap();
        fs::write(&preview, b"image").unwrap();
        let dir = root.join("late.deck");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("stale.txt"), b"old").unwrap();

        let mut m = manifest();
        m.preview = Some(preview_name(&preview));
        let deck = Deck::write(&dir, m, &video, Some(&preview)).unwrap();

        assert_eq!(fs::read(deck.video()).unwrap(), b"video");
        assert_eq!(deck.preview().unwrap().file_name().unwrap(), "preview.jpg");
        assert!(!dir.join("stale.txt").exists());
        assert!(is_deck(&dir));
        fs::remove_dir_all(&root).unwrap();
    }
}
