//! Image backgrounds: a picture is cropped to the panel's shape and scaled to its size once, when it's
//! made or replaced, and only the result is kept. A folder holds `image.json` (the file it came
//! from) and `image.jpg`, which the keyboard shows as it is.

use std::fs;
use std::io::Cursor;
use std::path::Path;

use glassdeck_package::{Content, Kind, Manifest, Origin, PackageId, PackageWriter, Version};
use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use serde::{Deserialize, Serialize};
use specta::Type;

use crate::backgrounds::{BackgroundKind, BackgroundSummary, background_summary, typed_background};
use crate::library::{LibraryError, io, read_deck_json};

pub const IMAGE_RECORD: &str = "image.json";
pub const IMAGE_FILE: &str = "image.jpg";
/// The keyboard's panel, which the picture fills.
pub const PANEL: (u32, u32) = (1920, 1080);
const JPEG_QUALITY: u8 = 90;

/// Where an image background's picture came from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Type)]
pub struct ImageRecord {
    /// The file's name, as it was picked
    pub from: String,
}

/// An image background as its page shows it.
#[derive(Clone, Debug, Serialize, Type)]
pub struct ImageBackground {
    pub background: BackgroundSummary,
    pub record: ImageRecord,
    /// The picture as the keyboard shows it
    pub path: String,
    pub width: u32,
    pub height: u32,
}

/// A picked picture, ready for a folder: its record and the panel-sized JPEG.
pub struct PreparedImage {
    pub record: ImageRecord,
    pub jpeg: Vec<u8>,
}

/// Reads any picture the app can decode; its file name and the decoded picture.
fn open(source: &Path) -> Result<(String, image::DynamicImage), LibraryError> {
    let bad = |reason: String| LibraryError::BadImage { reason };
    let from = source.file_name().map(|n| n.to_string_lossy().into_owned()).ok_or_else(|| bad("Pick a picture file".into()))?;
    let picture = image::ImageReader::open(source)
        .map_err(|e| bad(format!("{from} can't be opened: {e}")))?
        .with_guessed_format()
        .map_err(|e| bad(format!("{from} can't be read: {e}")))?
        .decode()
        .map_err(|e| bad(format!("{from} isn't a picture the app can read: {e}")))?;
    Ok((from, picture))
}

/// The picture's middle at the panel's shape, scaled to `size`, as a JPEG.
fn filled_jpeg(picture: &image::DynamicImage, size: (u32, u32), filter: FilterType) -> Result<Vec<u8>, LibraryError> {
    let filled = picture.resize_to_fill(size.0, size.1, filter).to_rgb8();
    let mut jpeg = Vec::new();
    JpegEncoder::new_with_quality(Cursor::new(&mut jpeg), JPEG_QUALITY)
        .encode_image(&filled)
        .map_err(|e| LibraryError::BadImage { reason: format!("encoding the picture: {e}") })?;
    Ok(jpeg)
}

/// Reads any picture the app can decode, crops its middle to the panel's shape and scales it to the panel.
pub fn prepare(source: &Path) -> Result<PreparedImage, LibraryError> {
    let (from, picture) = open(source)?;
    Ok(PreparedImage { record: ImageRecord { from }, jpeg: filled_jpeg(&picture, PANEL, FilterType::Lanczos3)? })
}

/// How a picked picture will look on the panel, as a base64 JPEG half the panel's size. Scaled with a
/// quicker filter than the real one: it's shown at once, and small.
pub fn preview(source: &Path) -> Result<String, LibraryError> {
    use base64::Engine;
    let (_, picture) = open(source)?;
    Ok(base64::engine::general_purpose::STANDARD.encode(filled_jpeg(&picture, (PANEL.0 / 2, PANEL.1 / 2), FilterType::Triangle)?))
}

/// The picture formats the app reads, for the file dialog.
pub const EXTENSIONS: [&str; 7] = ["jpg", "jpeg", "png", "webp", "gif", "bmp", "jfif"];

/// Writes a prepared picture into a background's folder.
pub fn write(folder: &Path, image: &PreparedImage) -> Result<(), LibraryError> {
    let record = folder.join(IMAGE_RECORD);
    fs::write(&record, record_text(&image.record)).map_err(io(format!("writing {}", record.display())))?;
    let file = folder.join(IMAGE_FILE);
    fs::write(&file, &image.jpeg).map_err(io(format!("writing {}", file.display())))
}

pub fn record_text(record: &ImageRecord) -> String {
    format!("{}\n", serde_json::to_string_pretty(record).expect("a record serializes"))
}

fn read_record(folder: &Path) -> Result<ImageRecord, LibraryError> {
    let path = folder.join(IMAGE_RECORD);
    let text = fs::read_to_string(&path).map_err(io(format!("reading {}", path.display())))?;
    serde_json::from_str(&text).map_err(|e| LibraryError::Unreadable { folder: folder.display().to_string(), reason: format!("{IMAGE_RECORD}: {e}") })
}

pub fn read_image(root: &Path, id: &str) -> Result<ImageBackground, LibraryError> {
    let folder = typed_background(root, id, BackgroundKind::Image)?;
    let path = folder.join(IMAGE_FILE).display().to_string();
    Ok(ImageBackground { background: background_summary(&folder)?, record: read_record(&folder)?, path, width: PANEL.0, height: PANEL.1 })
}

/// Puts a new picture in place of the background's.
pub fn replace(root: &Path, id: &str, source: &Path) -> Result<ImageBackground, LibraryError> {
    let folder = typed_background(root, id, BackgroundKind::Image)?;
    write(&folder, &prepare(source)?)?;
    read_image(root, id)
}

/// Packs the picture into a `.deck` at `out`.
pub fn export(folder: &Path, out: &Path) -> Result<(), LibraryError> {
    let deck = read_deck_json(folder)?;
    let text = |key: &str| deck.get(key).and_then(|v| v.as_str()).unwrap_or_default().to_owned();
    let id = PackageId::parse(&text("id")).map_err(|e| LibraryError::InvalidId { reason: e.to_string() })?;
    let version = Version::parse(&text("version")).map_err(|e| LibraryError::InvalidVersion { reason: e.to_string() })?;
    let record = read_record(folder)?;
    let manifest = Manifest {
        format: glassdeck_package::FORMAT_VERSION,
        id,
        title: text("title"),
        version,
        origin: Origin::Converted { from: record.from },
        preview: None,
        kind: Kind::Background(Content::Image { file: IMAGE_FILE.into() }),
    };
    let failed = |e: anyhow::Error| LibraryError::Io { reason: format!("{e:#}") };
    let mut writer = PackageWriter::create(out, manifest).map_err(failed)?;
    writer.add_file(IMAGE_FILE, &folder.join(IMAGE_FILE)).map_err(failed)?;
    writer.finish().map(drop).map_err(failed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_picture_fills_the_panel_from_its_middle() {
        let dir = std::env::temp_dir().join(format!("glassdeck-images-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let source = dir.join("tall.png");
        let tall = image::RgbImage::from_fn(400, 900, |_, y| if (300..600).contains(&y) { image::Rgb([20, 200, 20]) } else { image::Rgb([200, 20, 20]) });
        tall.save(&source).unwrap();
        let prepared = prepare(&source).unwrap();
        assert_eq!(prepared.record.from, "tall.png");
        let out = image::load_from_memory(&prepared.jpeg).unwrap().to_rgb8();
        assert_eq!(out.dimensions(), PANEL);
        let middle = out.get_pixel(960, 540);
        assert!(middle[1] > 150 && middle[0] < 80, "the middle of a tall picture is kept: {middle:?}");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn something_that_isnt_a_picture_is_refused() {
        let dir = std::env::temp_dir().join(format!("glassdeck-images-bad-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let source = dir.join("notes.txt");
        fs::write(&source, "hello").unwrap();
        assert!(matches!(prepare(&source), Err(LibraryError::BadImage { .. })));
        fs::remove_dir_all(&dir).unwrap();
    }
}
