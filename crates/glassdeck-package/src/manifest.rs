//! `manifest.json`: what every package says about itself, with its type's own fields under `details`.

use std::fmt;

use anyhow::{Context, Result, bail, ensure};
use glassdeck_proto::Codec;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::canvas::Canvas;
use crate::{FORMAT_VERSION, KEYBOARD_SCRIPT, PANEL, SHADER, check_name};

/// serde can't refuse unknown fields beside a flattened enum, so a misspelt optional field is ignored.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub format: u32,
    pub id: PackageId,
    pub title: String,
    pub version: Version,
    pub origin: Origin,
    /// Entry name, such as `preview.jpg`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
    /// The hosts `host.luau` may reach with `net`
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub network: Vec<crate::NetworkHost>,
    #[serde(flatten)]
    pub kind: Kind,
}

impl Manifest {
    /// What Glassdeck keys the package by: its type and its id.
    pub fn qualified_id(&self) -> QualifiedId {
        QualifiedId::new(self.kind.package_type(), &self.id)
    }
}

/// What the package is, which decides the layer it draws on.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "details", rename_all = "snake_case")]
pub enum Kind {
    /// Fills the panel behind everything
    Background(Content),
    /// Sits in the widget area
    Widget(WidgetSize),
}

impl Kind {
    pub fn package_type(&self) -> PackageType {
        match self {
            Kind::Background(_) => PackageType::Background,
            Kind::Widget(_) => PackageType::Widget,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageType {
    Background,
    Widget,
}

impl PackageType {
    pub fn as_str(self) -> &'static str {
        match self {
            PackageType::Background => "background",
            PackageType::Widget => "widget",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        [PackageType::Background, PackageType::Widget].into_iter().find(|t| t.as_str() == text)
    }
}

impl fmt::Display for PackageType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "content", rename_all = "snake_case")]
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
    /// Drawn on the keyboard's GPU by glassdeck-gpu, from `shader.essl` and an optional `keyboard.luau`
    Canvas(Canvas),
    /// One still picture, a JPEG at the panel's size, shown by glassdeck-gpu
    Image { file: String },
}

/// `{tld}.{entity}.{name}` in lowercase, such as `local.user.clock`: what a package calls itself.
/// Unique within its type; Glassdeck keys packages by `QualifiedId`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct PackageId(String);

impl PackageId {
    pub fn parse(text: &str) -> Result<Self> {
        let part_ok = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
        let parts: Vec<&str> = text.split('.').collect();
        ensure!(parts.len() == 3 && parts.iter().all(|p| part_ok(p)), "id {text:?} is not {{tld}}.{{entity}}.{{name}} in lowercase, like local.user.clock");
        Ok(Self(text.to_owned()))
    }

    /// An id under `prefix` (`{tld}.{entity}`) made from any name: lowercase, with runs of anything else as one `-`.
    pub fn from_name(prefix: &str, name: &str) -> Result<Self> {
        let mut slug = String::new();
        for c in name.chars() {
            match c.to_ascii_lowercase() {
                c @ ('a'..='z' | '0'..='9') => slug.push(c),
                _ if !slug.is_empty() && !slug.ends_with('-') => slug.push('-'),
                _ => {}
            }
        }
        Self::parse(&format!("{prefix}.{}", slug.trim_end_matches('-')))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for PackageId {
    type Error = anyhow::Error;
    fn try_from(text: String) -> Result<Self> {
        Self::parse(&text)
    }
}

impl From<PackageId> for String {
    fn from(id: PackageId) -> Self {
        id.0
    }
}

impl fmt::Display for PackageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// `{type}.{tld}.{entity}.{name}`, such as `widget.local.user.clock`: what Glassdeck keys a package by,
/// in packages folders, the library, themes and on the Link.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct QualifiedId(String);

impl QualifiedId {
    pub fn new(package_type: PackageType, id: &PackageId) -> Self {
        Self(format!("{package_type}.{id}"))
    }

    pub fn parse(text: &str) -> Result<Self> {
        let (package_type, id) = text.split_once('.').unwrap_or((text, ""));
        let package_type = PackageType::parse(package_type).with_context(|| format!("{text:?} does not start with widget. or background."))?;
        Ok(Self::new(package_type, &PackageId::parse(id)?))
    }

    pub fn package_type(&self) -> PackageType {
        PackageType::parse(self.parts().0).expect("checked when made")
    }

    pub fn id(&self) -> PackageId {
        PackageId(self.parts().1.to_owned())
    }

    fn parts(&self) -> (&str, &str) {
        self.0.split_once('.').expect("checked when made")
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Where the package lives in a packages folder.
    pub fn file_name(&self) -> String {
        format!("{}.{}", self.0, crate::EXTENSION)
    }
}

impl TryFrom<String> for QualifiedId {
    type Error = anyhow::Error;
    fn try_from(text: String) -> Result<Self> {
        Self::parse(&text)
    }
}

impl From<QualifiedId> for String {
    fn from(id: QualifiedId) -> Self {
        id.0
    }
}

impl fmt::Display for QualifiedId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// `major.minor.patch`
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Version(String);

impl Version {
    pub fn parse(text: &str) -> Result<Self> {
        let parts: Vec<&str> = text.split('.').collect();
        let ok = parts.len() == 3 && parts.iter().all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
        ensure!(ok, "version {text:?} is not major.minor.patch, like 1.0.0");
        Ok(Self(text.to_owned()))
    }
}

impl TryFrom<String> for Version {
    type Error = anyhow::Error;
    fn try_from(text: String) -> Result<Self> {
        Self::parse(&text)
    }
}

impl From<Version> for String {
    fn from(version: Version) -> Self {
        version.0
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Origin {
    WallpaperEngine { workshop_id: String },
    /// Made by `glassdeck convert` from the file or folder of this name
    Converted { from: String },
    /// Made by `glassdeck pack` from the folder of this name; `sources` is `source_digest` of what it read
    Packed { from: String, sources: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Size {
    pub w: u32,
    pub h: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resize {
    Fixed,
    Between { min: Size, max: Size },
}

impl Resize {
    pub fn allows(self, default: Size, size: Size) -> bool {
        match self {
            Resize::Fixed => size == default,
            Resize::Between { min, max } => (min.w..=max.w).contains(&size.w) && (min.h..=max.h).contains(&size.h),
        }
    }
}

/// A widget's default size and how far it may be resized.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawWidgetSize", into = "RawWidgetSize")]
pub struct WidgetSize {
    pub size: Size,
    pub resize: Resize,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawWidgetSize {
    size: Size,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    min: Option<Size>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    max: Option<Size>,
}

impl TryFrom<RawWidgetSize> for WidgetSize {
    type Error = anyhow::Error;
    fn try_from(raw: RawWidgetSize) -> Result<Self> {
        ensure!(raw.size.w > 0 && raw.size.h > 0, "size must not be zero");
        let resize = match (raw.min, raw.max) {
            (None, None) => Resize::Fixed,
            (Some(min), Some(max)) => Resize::Between { min, max },
            _ => bail!("give both min and max, or neither for a fixed size"),
        };
        ensure!(resize.allows(raw.size, raw.size), "size {}x{} is outside min and max", raw.size.w, raw.size.h);
        Ok(Self { size: raw.size, resize })
    }
}

impl From<WidgetSize> for RawWidgetSize {
    fn from(widget: WidgetSize) -> Self {
        let (min, max) = match widget.resize {
            Resize::Fixed => (None, None),
            Resize::Between { min, max } => (Some(min), Some(max)),
        };
        Self { size: widget.size, min, max }
    }
}

impl Manifest {
    /// Checks the format, the type's own rules, and that every entry the package needs is there.
    pub fn validate(&self, has_entry: impl Fn(&str) -> bool) -> Result<()> {
        ensure!(self.format == FORMAT_VERSION, "package format {} is not the {FORMAT_VERSION} this build reads", self.format);
        let needed: Vec<&str> = match &self.kind {
            Kind::Background(Content::Video { file, format }) => {
                if (format.width, format.height) != PANEL {
                    bail!("{}", SpecMismatch::Size { width: format.width, height: format.height });
                }
                vec![file]
            }
            Kind::Background(Content::Web { entry, .. }) => vec![entry],
            Kind::Background(Content::Image { file }) => vec![file],
            Kind::Background(Content::Canvas(canvas)) => {
                canvas.validate()?;
                vec![SHADER]
            }
            Kind::Widget(_) => vec![KEYBOARD_SCRIPT],
        };
        for name in needed.into_iter().chain(self.preview.as_deref()) {
            check_name(name)?;
            ensure!(has_entry(name), "{} needs {name:?}, which it does not hold", self.id);
        }
        Ok(())
    }

    pub fn widget(&self) -> Option<&WidgetSize> {
        match &self.kind {
            Kind::Widget(size) => Some(size),
            Kind::Background(_) => None,
        }
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
    fn the_type_is_written_once_and_details_follow_it() {
        let text = r#"{"format":4,"id":"com.wallpaperengine.1","title":"t","version":"1.0.0",
            "origin":{"kind":"wallpaper_engine","workshop_id":"1"},
            "type":"background","details":{"content":"video","file":"video.mp4","codec":"hevc","width":1920,"height":1080,
            "frame_rate":{"numerator":24,"denominator":1}}}"#;
        let manifest: Manifest = serde_json::from_str(text).unwrap();
        let Kind::Background(Content::Video { file, format }) = &manifest.kind else { panic!("not video") };
        assert_eq!((file.as_str(), format.codec, format.frame_rate.numerator), ("video.mp4", Codec::Hevc, 24));
        assert_eq!(serde_json::from_str::<Manifest>(&serde_json::to_string(&manifest).unwrap()).unwrap(), manifest);

        let widget_with_video = text.replace(r#""type":"background""#, r#""type":"widget""#);
        assert!(serde_json::from_str::<Manifest>(&widget_with_video).is_err());
    }

    #[test]
    fn widget_sizes_are_checked_when_read() {
        let widget = |details: &str| {
            serde_json::from_str::<Manifest>(&format!(
                r#"{{"format":4,"id":"local.user.clock","title":"Clock","version":"1.0.0",
                "origin":{{"kind":"packed","from":"clock","sources":"x"}},"type":"widget","details":{details}}}"#
            ))
        };
        let read = widget(r#"{"size":{"w":240,"h":120},"min":{"w":160,"h":80},"max":{"w":480,"h":240}}"#).unwrap();
        assert_eq!(read.widget().unwrap().resize, Resize::Between { min: Size { w: 160, h: 80 }, max: Size { w: 480, h: 240 } });
        assert!(widget(r#"{"size":{"w":8,"h":8},"min":{"w":8,"h":8}}"#).is_err());
        assert!(widget(r#"{"size":{"w":800,"h":8},"min":{"w":8,"h":8},"max":{"w":16,"h":16}}"#).is_err());
        assert!(widget(r#"{"size":{"w":0,"h":8}}"#).is_err());
    }

    #[test]
    fn ids_and_versions_are_checked_when_read() {
        assert!(PackageId::parse("local.user.clock").is_ok());
        assert!(PackageId::parse("com.wallpaperengine.1184945377").is_ok());
        for bad in ["clock", "me.clock", "a.b.c.d", "Local.Me.Clock", "a..b", "a.b/c.d", "a b.c.d"] {
            assert!(PackageId::parse(bad).is_err(), "{bad}");
        }
        assert_eq!(PackageId::from_name("local.converted", "Late Night (2) v2!").unwrap().as_str(), "local.converted.late-night-2-v2");
        assert!(PackageId::from_name("local.converted", "!!!").is_err());
        assert!(Version::parse("1.0.0").is_ok());
        assert!(Version::parse("1.0").is_err());
    }

    #[test]
    fn a_qualified_id_is_the_type_then_the_id() {
        let id = QualifiedId::parse("widget.local.user.clock").unwrap();
        assert_eq!(id.package_type(), PackageType::Widget);
        assert_eq!(id.id().as_str(), "local.user.clock");
        assert_eq!(id.file_name(), "widget.local.user.clock.deck");
        let gradient = PackageId::parse("local.user.gradient").unwrap();
        assert_eq!(QualifiedId::new(PackageType::Background, &gradient).as_str(), "background.local.user.gradient");
        for bad in ["local.user.clock", "canvas.local.user.clock", "widget.me.clock", "widget"] {
            assert!(QualifiedId::parse(bad).is_err(), "{bad}");
        }
    }
}
