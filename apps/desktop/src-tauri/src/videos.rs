//! Video backgrounds: a video is cropped, trimmed and converted once, when it's made or replaced, and
//! only the result is kept. A folder holds `video.json` (where it came from and how it was cut),
//! `video.mp4` in the keyboard's spec once converted; its still is in the app's cache (see `stills`).

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use base64::Engine;
use glassdeck_convert::video::{self, Cut, EncodeSettings};
use glassdeck_package::{Origin, ProbedCodec, QualifiedId};
use image::RgbImage;
use image::codecs::jpeg::JpegEncoder;
use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::AppHandle;
use tauri_specta::Event as _;

use crate::backgrounds::{BackgroundSummary, VIDEO_RECORD, background_summary, typed_background};
use crate::library::{LibraryError, io, read_deck_json};

pub const VIDEO_FILE: &str = "video.mp4";
/// The re-encode bitrate, as `glassdeck convert` uses by default.
const BITRATE: u32 = 12_000_000;
const FRAME_WIDTH: u32 = 960;
const PREVIEW_WIDTH: u32 = 480;
const JPEG_QUALITY: u8 = 85;

/// Which part of the video to keep: the crop's place along the side that overflows the panel, from 0
/// to 1, and the seconds the loop starts and ends at.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, Type)]
pub struct VideoCut {
    pub crop: f32,
    pub start: f64,
    pub end: f64,
}

impl VideoCut {
    fn parse(self, length: Option<f64>) -> Result<Cut, LibraryError> {
        let bad = |reason: String| LibraryError::BadVideo { reason };
        if !(0.0..=1.0).contains(&self.crop) {
            return Err(bad(format!("crop {} isn't between 0 and 1", self.crop)));
        }
        if self.start < 0.0 || self.end <= self.start {
            return Err(bad(format!("the loop {}s to {}s is empty", self.start, self.end)));
        }
        if let Some(length) = length
            && self.end > length + 0.5
        {
            return Err(bad(format!("the loop ends at {}s, after the video's {length:.1}s", self.end)));
        }
        Ok(Cut { crop: self.crop, start: self.start, end: Some(self.end) })
    }
}

/// `video.json`: the file the video came from, by name, and how it was cut.
#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct VideoRecord {
    pub from: String,
    pub cut: VideoCut,
}

/// A video file's facts, as the New background dialog and the video page show them.
#[derive(Clone, Debug, Serialize, Type)]
pub struct VideoFacts {
    /// "H.264", "HEVC", or "Other"
    pub codec: String,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub length: Option<f64>,
    /// Already H.264 or HEVC at 1920x1080, so a whole-length cut is copied as it is
    pub fits_panel: bool,
}

pub fn facts(path: &Path) -> Result<VideoFacts, LibraryError> {
    let bad = |e: anyhow::Error| LibraryError::BadVideo { reason: format!("{e:#}") };
    video::start_media_foundation().map_err(bad)?;
    let probed = video::probe(path).map_err(bad)?;
    let length = video::duration(path).map_err(bad)?;
    let codec = match probed.codec {
        ProbedCodec::Known(glassdeck_proto::Codec::H264) => "H.264",
        ProbedCodec::Known(glassdeck_proto::Codec::Hevc) => "HEVC",
        ProbedCodec::Other => "Other",
    };
    let rate = probed.frame_rate;
    Ok(VideoFacts {
        codec: codec.into(),
        width: probed.width.into(),
        height: probed.height.into(),
        fps: rate.numerator as f64 / rate.denominator.max(1) as f64,
        length,
        fits_panel: glassdeck_package::check(probed).is_ok(),
    })
}

/// The frame at `seconds`, as a base64 JPEG at most `width` wide.
pub fn frame(path: &Path, seconds: f64, width: u32) -> Result<String, LibraryError> {
    let bad = |e: anyhow::Error| LibraryError::BadVideo { reason: format!("{e:#}") };
    video::start_media_foundation().map_err(bad)?;
    let (w, h, rgb) = video::frame_at(path, seconds).map_err(bad)?;
    Ok(base64::engine::general_purpose::STANDARD.encode(jpeg(w, h, rgb, width).map_err(bad)?))
}

pub fn source_frame(path: &Path, seconds: f64) -> Result<String, LibraryError> {
    frame(path, seconds, FRAME_WIDTH)
}

fn jpeg(width: u32, height: u32, rgb: Vec<u8>, at_most: u32) -> Result<Vec<u8>> {
    let mut image = RgbImage::from_raw(width, height, rgb).context("the frame isn't its size")?;
    if width > at_most {
        image = image::imageops::thumbnail(&image, at_most, height * at_most / width);
    }
    let mut bytes = Vec::new();
    JpegEncoder::new_with_quality(&mut bytes, JPEG_QUALITY).encode_image(&image).context("encoding a frame")?;
    Ok(bytes)
}

fn read_record(folder: &Path) -> Result<VideoRecord, LibraryError> {
    let path = folder.join(VIDEO_RECORD);
    let text = fs::read_to_string(&path).map_err(io(format!("reading {}", path.display())))?;
    serde_json::from_str(&text).map_err(|e| LibraryError::Unreadable { folder: folder.display().to_string(), reason: format!("{VIDEO_RECORD}: {e}") })
}

pub fn record_text(record: &VideoRecord) -> String {
    format!("{}\n", serde_json::to_string_pretty(record).expect("a record serializes"))
}

/// What converting needs, once the library has the folder.
pub struct Conversion {
    pub id: String,
    pub folder: PathBuf,
    pub source: PathBuf,
    pub cut: Cut,
}

/// Checks `source` and `cut` before anything is written, and writes the record for them.
pub fn plan(id: &QualifiedId, folder: &Path, source: &Path, cut: VideoCut) -> Result<(VideoRecord, Conversion), LibraryError> {
    if !source.is_file() {
        return Err(LibraryError::BadVideo { reason: format!("{} isn't a file", source.display()) });
    }
    let length = facts(source)?.length;
    let parsed = cut.parse(length)?;
    let from = source.file_name().map_or_else(|| source.display().to_string(), |n| n.to_string_lossy().into_owned());
    let conversion = Conversion { id: id.to_string(), folder: folder.to_path_buf(), source: source.to_path_buf(), cut: parsed };
    Ok((VideoRecord { from, cut }, conversion))
}

#[derive(Clone, Debug, Serialize, Type)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ConversionState {
    Converting { fraction: f32 },
    Failed { reason: String },
}

#[derive(Clone, Debug, Serialize, Type, tauri_specta::Event)]
pub struct VideoUpdate {
    pub id: String,
    /// `None` once the conversion is done
    pub state: Option<ConversionState>,
}

/// The conversions running or failed since the app started, by qualified id.
#[derive(Clone, Default)]
pub struct Conversions(Arc<Mutex<HashMap<String, ConversionState>>>);

impl Conversions {
    pub fn state(&self, id: &str) -> Option<ConversionState> {
        self.0.lock().expect("conversions aren't poisoned").get(id).cloned()
    }

    fn set(&self, app: &AppHandle, id: &str, state: Option<ConversionState>) {
        let mut all = self.0.lock().expect("conversions aren't poisoned");
        match &state {
            Some(state) => all.insert(id.to_owned(), state.clone()),
            None => all.remove(id),
        };
        drop(all);
        if let Err(e) = (VideoUpdate { id: id.to_owned(), state }).emit(app) {
            eprintln!("emitting a video update: {e}");
        }
    }

    /// Converts on its own thread into `video.mp4`, and draws its still.
    pub fn start(&self, app: AppHandle, conversion: Conversion) {
        let jobs = self.clone();
        jobs.set(&app, &conversion.id, Some(ConversionState::Converting { fraction: 0.0 }));
        std::thread::spawn(move || {
            let reported = Mutex::new(0.0f32);
            let progress = |fraction: f32| {
                let mut last = reported.lock().expect("progress isn't poisoned");
                if fraction - *last >= 0.01 {
                    *last = fraction;
                    jobs.set(&app, &conversion.id, Some(ConversionState::Converting { fraction }));
                }
            };
            let state = match convert(&conversion, &progress) {
                Ok(()) => None,
                Err(e) => Some(ConversionState::Failed { reason: format!("{e:#}") }),
            };
            jobs.set(&app, &conversion.id, state);
        });
    }
}

fn convert(conversion: &Conversion, progress: &dyn Fn(f32)) -> Result<()> {
    let Conversion { folder, source, cut, .. } = conversion;
    let partial = folder.join("video.converting.mp4");
    glassdeck_convert::spec_video(source, &partial, EncodeSettings { bitrate: BITRATE }, *cut, progress)?;
    let done = folder.join(VIDEO_FILE);
    fs::rename(&partial, &done).with_context(|| format!("moving {} into place", done.display()))?;
    crate::stills::draw_video(folder).map(drop)
}

/// The converted video's frame at `seconds`, as a JPEG still.
pub fn frame_jpeg(folder: &Path, seconds: f64) -> Result<Vec<u8>> {
    let (w, h, rgb) = video::frame_at(&folder.join(VIDEO_FILE), seconds)?;
    jpeg(w, h, rgb, PREVIEW_WIDTH)
}

/// A video background as its page shows it.
#[derive(Clone, Debug, Serialize, Type)]
pub struct VideoBackground {
    pub background: BackgroundSummary,
    pub record: VideoRecord,
    /// The converted video, once there is one
    pub video: Option<ConvertedVideo>,
    pub conversion: Option<ConversionState>,
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct ConvertedVideo {
    pub path: String,
    pub facts: VideoFacts,
    /// Base64 JPEG
    pub preview: Option<String>,
}

pub fn read_video(root: &Path, id: &str, conversions: &Conversions) -> Result<VideoBackground, LibraryError> {
    let folder = typed_background(root, id, crate::backgrounds::BackgroundKind::Video)?;
    let background = background_summary(&folder)?;
    let record = read_record(&folder)?;
    let conversion = conversions.state(&background.id);
    let file = folder.join(VIDEO_FILE);
    let video = if file.is_file() && conversion.is_none() {
        let preview = match background.preview.as_ref().map(|still| fs::read(&still.path)) {
            Some(Ok(bytes)) => Some(base64::engine::general_purpose::STANDARD.encode(bytes)),
            Some(Err(e)) => return Err(io("reading the video's still".into())(e)),
            None => None,
        };
        Some(ConvertedVideo { path: file.display().to_string(), facts: facts(&file)?, preview })
    } else {
        None
    };
    Ok(VideoBackground { background, record, video, conversion })
}

/// Makes the converted video's frame at `seconds` its still, keeping the time in `deck.json`; the new image.
pub fn set_preview(root: &Path, id: &str, seconds: f64) -> Result<String, LibraryError> {
    if !seconds.is_finite() || seconds < 0.0 {
        return Err(LibraryError::InvalidTime { reason: format!("{seconds} isn't a time from 0 seconds on") });
    }
    let folder = typed_background(root, id, crate::backgrounds::BackgroundKind::Video)?;
    let bad = |e: anyhow::Error| LibraryError::BadVideo { reason: format!("{e:#}") };
    video::start_media_foundation().map_err(bad)?;
    let mut deck = read_deck_json(&folder)?;
    deck.insert("preview_at".into(), serde_json::json!(seconds));
    crate::library::write_deck_json(&folder, &deck)?;
    let still = crate::stills::draw_video(&folder).map_err(bad)?;
    let bytes = fs::read(&still.path).map_err(io("reading the video's still".into()))?;
    Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
}

/// Points the background at a new file and cut; the conversion to start.
pub fn replace(root: &Path, id: &str, source: &Path, cut: VideoCut) -> Result<Conversion, LibraryError> {
    let folder = typed_background(root, id, crate::backgrounds::BackgroundKind::Video)?;
    let qualified = QualifiedId::parse(id).map_err(|e| LibraryError::InvalidId { reason: format!("{e:#}") })?;
    let (record, conversion) = plan(&qualified, &folder, source, cut)?;
    let path = folder.join(VIDEO_RECORD);
    fs::write(&path, record_text(&record)).map_err(io(format!("writing {}", path.display())))?;
    Ok(conversion)
}

/// Packs the converted video into a `.deck` at `out`.
pub fn export(folder: &Path, out: &Path) -> Result<(), LibraryError> {
    let file = folder.join(VIDEO_FILE);
    if !file.is_file() {
        return Err(LibraryError::BadVideo { reason: "Wait for the video to finish converting".into() });
    }
    let deck = read_deck_json(folder)?;
    let text = |key: &str| deck.get(key).and_then(|v| v.as_str()).unwrap_or_default().to_owned();
    let id = glassdeck_package::PackageId::parse(&text("id")).map_err(|e| LibraryError::InvalidId { reason: e.to_string() })?;
    let record = read_record(folder)?;
    let details = glassdeck_convert::Details { id, title: text("title"), origin: Origin::Converted { from: record.from }, preview: None };
    glassdeck_convert::video(&file, out, details, EncodeSettings { bitrate: BITRATE }).map(drop).map_err(|e| LibraryError::Io { reason: format!("{e:#}") })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cut_must_be_a_real_loop_inside_the_video() {
        let cut = |crop, start, end| VideoCut { crop, start, end };
        assert!(cut(0.5, 0.0, 10.0).parse(Some(10.0)).is_ok());
        assert!(cut(1.5, 0.0, 10.0).parse(Some(10.0)).is_err(), "crop past the edge");
        assert!(cut(0.5, 4.0, 4.0).parse(Some(10.0)).is_err(), "an empty loop");
        assert!(cut(0.5, 0.0, 30.0).parse(Some(10.0)).is_err(), "past the end");
        assert!(cut(0.5, 0.0, 30.0).parse(None).is_ok(), "no length to check against");
    }
}
