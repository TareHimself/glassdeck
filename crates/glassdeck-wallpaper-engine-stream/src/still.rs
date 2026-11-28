//! The still glassdeckd shows for the stream before any video: the wallpaper's own preview image.

use anyhow::{Context, Result};
use glassdeck_wallpaper_engine::Item;
use image::imageops::FilterType;

/// About the size the app's keyboard preview shows it at.
const WIDTH: u32 = 960;
const QUALITY: u8 = 80;

/// The item's preview (JPEG, PNG or the first frame of a GIF) as a JPEG at most `WIDTH` wide.
pub fn of(item: &Item) -> Result<Vec<u8>> {
    let project = item.project()?;
    let name = project.preview.context("its project.json names no preview")?;
    let path = item.dir.join(name);
    let picture = image::ImageReader::open(&path)
        .with_context(|| format!("opening {}", path.display()))?
        .with_guessed_format()?
        .decode()
        .with_context(|| format!("decoding {}", path.display()))?;
    let picture = if picture.width() > WIDTH { picture.resize(WIDTH, u32::MAX, FilterType::Triangle) } else { picture };
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, QUALITY).encode_image(&picture.to_rgb8()).context("encoding the still")?;
    Ok(jpeg)
}
