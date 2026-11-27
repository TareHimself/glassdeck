//! What to play, as data: the CLI sends a `SourceRequest` to glassdeckd, whose source worker builds it.

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Result, bail};
use glassdeck_package::Package;
use serde::{Deserialize, Serialize};

use crate::background_source::{self, WebEncoding};
use crate::desktop::{DesktopSource, EncoderKind};
use crate::monitors::{self, MonitorQuery};
use crate::pipeline::{EncoderSettings, Source, VideoCodec};
use crate::play::{FileSource, Media};
use crate::wallpaper;
use crate::web::WebSource;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRequest {
    pub play: Play,
    /// Stop after this long; sources without an end (monitors, web pages, scenes) otherwise run until stopped.
    pub seconds: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Play {
    /// Mirror a monitor, live
    Monitor {
        monitor: String,
        encoder: EncoderKind,
        codec: VideoCodec,
        mbps: u32,
        fps: u32,
        keyframe_every: u32,
        vbv_frames: Option<u32>,
        /// Scaled down to this width (GPU encoder only), keeping the monitor's shape
        #[serde(default)]
        max_width: Option<u32>,
    },
    /// Render a page offscreen with CEF, paced
    Web { url: String, width: u32, height: u32, codec: VideoCodec, mbps: u32, fps: u32, keyframe_every: u32 },
    /// An H.264 or HEVC file, sent without re-encoding
    File { path: PathBuf, looping: bool },
    /// A background .deck: video sent as it is, web rendered and encoded
    Background { path: PathBuf, looping: bool, mbps: u32, fps: u32 },
    /// A Wallpaper Engine item from this PC's workshop folder
    Wallpaper { item: String, rebuild: bool, mbps: u32, fps: u32 },
}

impl fmt::Display for Play {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Play::Monitor { monitor, .. } => write!(f, "monitor {monitor}"),
            Play::Web { url, .. } => write!(f, "web page {url}"),
            Play::File { path, .. } => write!(f, "file {}", path.display()),
            Play::Background { path, .. } => write!(f, "background {}", path.display()),
            Play::Wallpaper { item, .. } => write!(f, "wallpaper {item}"),
        }
    }
}

pub fn build(request: SourceRequest) -> Result<Source> {
    let duration = request.seconds.map(Duration::from_secs);
    Ok(match request.play {
        Play::Monitor { monitor, encoder, codec, mbps, fps, keyframe_every, vbv_frames, max_width } => {
            let query: MonitorQuery = monitor.parse().expect("any text is a monitor query");
            let found = monitors::find(&query)?;
            println!("Streaming {}", found.info);
            if max_width.is_some() && encoder != EncoderKind::Gpu {
                bail!("scaling a monitor down needs the GPU encoder");
            }
            let (width, height) = scaled_to(found.info.width, found.info.height, max_width);
            let settings = EncoderSettings {
                width,
                height,
                codec: codec.into(),
                fps,
                bitrate: mbps * 1_000_000,
                keyframe_every,
                vbv_frames,
            };
            Source::Frames(Box::new(DesktopSource::new(found, encoder, settings, duration)?))
        }
        Play::Web { url, width, height, codec, mbps, fps, keyframe_every } => {
            let settings =
                EncoderSettings { width, height, codec: codec.into(), fps, bitrate: mbps * 1_000_000, keyframe_every, vbv_frames: None };
            Source::Frames(Box::new(WebSource::new(&url, settings, duration)?))
        }
        Play::File { path, looping } => {
            if duration.is_some() {
                bail!("a file plays to its end; --seconds only applies to sources without one");
            }
            Source::Encoded(Box::new(FileSource::open(Media::File(path), looping)?))
        }
        Play::Background { path, looping, mbps, fps } => {
            let web = WebEncoding { fps, bitrate: mbps * 1_000_000, duration };
            background_source::open(Package::open(&path)?, looping, web)?
        }
        Play::Wallpaper { item, rebuild, mbps, fps } => wallpaper::source(&item, rebuild, mbps, fps, duration)?,
    })
}

/// The size a `width` x `height` picture encodes at within `max_width`: same shape, even sides.
fn scaled_to(width: u32, height: u32, max_width: Option<u32>) -> (u32, u32) {
    match max_width {
        Some(max) if width > max => (max & !1, ((height as u64 * max as u64 / width as u64) as u32).max(2) & !1),
        _ => (width, height),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scaling_keeps_the_shape_with_even_sides() {
        assert_eq!(scaled_to(2560, 1440, Some(960)), (960, 540));
        assert_eq!(scaled_to(1920, 1200, Some(961)), (960, 600));
        assert_eq!(scaled_to(800, 600, Some(960)), (800, 600));
        assert_eq!(scaled_to(2560, 1440, None), (2560, 1440));
    }

    #[test]
    fn requests_read_as_tagged_json() {
        let json = r#"{ "play": { "kind": "file", "path": "C:/v.mp4", "looping": true }, "seconds": null }"#;
        let request: SourceRequest = serde_json::from_str(json).unwrap();
        assert_eq!(request.play, Play::File { path: "C:/v.mp4".into(), looping: true });
    }
}
