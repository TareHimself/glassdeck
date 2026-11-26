use std::fmt;

use anyhow::{Result, bail, ensure};
use glassdeck_proto::Codec;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::{FORMAT_VERSION, PANEL, check_name};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub format: u32,
    pub title: String,
    pub origin: Origin,
    pub content: Content,
    /// Entry name, such as `preview.jpg`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Origin {
    WallpaperEngine { workshop_id: String },
    /// Made by `glassdeck convert` from the file or folder of this name
    Converted { from: String },
    /// Made by glassdeck pack from the canvas folder of this name; sources is source_digest of what it read
    Packed { from: String, sources: String },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Content {
    Video {
        file: String,
        #[serde(flatten)]
        format: VideoFormat,
    },
    Web {
        /// The page to load, such as `web/index.html`
        entry: String,
        /// Wallpaper Engine's user properties with their defaults, as `applyUserProperties` receives them
        properties: Map<String, Value>,
    },
    /// Drawn on the keyboard's GPU by glassdeck-gpu, from a shader written in the glassdeck Slang API
    Canvas {
        /// The plan's entry, canvas/plan.json, which names the shader and the script
        plan: String,
    },
}

impl Manifest {
    /// Checks the format, the video spec, and that every entry the manifest names exists.
    pub fn validate(&self, has_entry: impl Fn(&str) -> bool) -> Result<()> {
        ensure!(self.format == FORMAT_VERSION, "background format {} is not the {FORMAT_VERSION} this build reads", self.format);
        let named = match &self.content {
            Content::Video { file, format } => {
                if (format.width, format.height) != PANEL {
                    bail!("{}", SpecMismatch::Size { width: format.width, height: format.height });
                }
                file
            }
            Content::Web { entry, .. } => entry,
            Content::Canvas { plan } => plan,
        };
        for name in std::iter::once(named).chain(&self.preview) {
            check_name(name)?;
            ensure!(has_entry(name), "the manifest names {name:?}, which the background does not hold");
        }
        Ok(())
    }
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

/// Why a video cannot go into a background as it is.
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

/// The background video spec: H.264 or HEVC at the panel's size.
pub fn check(video: ProbedVideo) -> Result<VideoFormat, SpecMismatch> {
    let ProbedCodec::Known(codec) = video.codec else {
        return Err(SpecMismatch::Codec);
    };
    if (video.width, video.height) != PANEL {
        return Err(SpecMismatch::Size { width: video.width, height: video.height });
    }
    Ok(VideoFormat { codec, width: video.width, height: video.height, frame_rate: video.frame_rate })
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
    fn video_content_reads_as_one_flat_variant() {
        let text = r#"{"format":2,"title":"t","origin":{"kind":"wallpaper_engine","workshop_id":"1"},
            "content":{"kind":"video","file":"video.mp4","codec":"hevc","width":1920,"height":1080,
            "frame_rate":{"numerator":24,"denominator":1}}}"#;
        let manifest: Manifest = serde_json::from_str(text).unwrap();
        let Content::Video { file, format } = &manifest.content else { panic!("not video") };
        assert_eq!((file.as_str(), format.codec, format.frame_rate.numerator), ("video.mp4", Codec::Hevc, 24));
        assert_eq!(serde_json::from_str::<Manifest>(&serde_json::to_string(&manifest).unwrap()).unwrap(), manifest);
    }

    #[test]
    fn web_content_without_properties_is_refused() {
        let text = r#"{"format":2,"title":"t","origin":{"kind":"wallpaper_engine","workshop_id":"1"},
            "content":{"kind":"web","entry":"web/index.html"}}"#;
        assert!(serde_json::from_str::<Manifest>(text).is_err());
    }
}
