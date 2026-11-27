//! What a module's `draw` asked for, and the rasteriser that turns it into a tile.

use std::collections::HashMap;
use std::f32::consts::PI;

use anyhow::{Context, Result, anyhow, bail};
use fontdue::{Font, FontSettings, Metrics};
use glassdeck_package::Resource;
use glassdeck_luau::ResourceRef;
use resvg::tiny_skia::{self, FillRule, LineCap, LineJoin, Mask, Paint, Path, PathBuilder, Pixmap, PixmapPaint, Stroke, Transform};
use resvg::usvg;

/// Text keeps the key labels' shadow, so it reads over any wallpaper or effect.
const SHADOW_OFFSET: f32 = 2.0;
const SHADOW_ALPHA: u8 = 170;
/// Bezier handle length for a quarter circle.
const KAPPA: f32 = 0.552_284_8;
const ARC_STEP_DEGREES: f32 = 3.0;
/// Glyphs kept per module before the cache starts over; text fitted to a width can ask for many sizes.
const MAX_CACHED_GLYPHS: usize = 2048;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const WHITE: Color = Color { r: 255, g: 255, b: 255, a: 255 };

    /// `#rrggbb` or `#rrggbbaa`.
    pub fn parse(text: &str) -> Result<Self> {
        let hex = text.strip_prefix('#').filter(|h| h.len() == 6 || h.len() == 8);
        let hex = hex.with_context(|| format!("color {text:?} is not #rrggbb or #rrggbbaa"))?;
        let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).with_context(|| format!("color {text:?} is not hex"));
        let a = if hex.len() == 8 { byte(6)? } else { 255 };
        Ok(Self { r: byte(0)?, g: byte(2)?, b: byte(4)?, a })
    }

    fn with_alpha_scaled(self, by: u8) -> Self {
        Self { a: (self.a as u32 * by as u32 / 255) as u8, ..self }
    }

    fn paint(self) -> Paint<'static> {
        let mut paint = Paint::default();
        paint.set_color_rgba8(self.r, self.g, self.b, self.a);
        paint.anti_alias = true;
        paint
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

impl Align {
    pub fn parse(text: &str) -> Result<Self> {
        Ok(match text {
            "left" => Self::Left,
            "center" => Self::Center,
            "right" => Self::Right,
            other => bail!("align {other:?} is not left, center or right"),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Area {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    /// `width` given: aligned within it and shrunk to fit. Otherwise aligned at `x`. `font`: a
    /// keyboard resource, or the built-in font when `None`.
    Text { text: String, x: f32, y: f32, size: f32, color: Color, align: Align, width: Option<f32>, font: Option<Resource> },
    Rect { area: Area, fill: Option<Color>, stroke: Option<(Color, f32)>, radius: f32 },
    Bar { fraction: f32, area: Area, color: Color },
    Graph { values: Vec<f32>, area: Area, color: Color, min: Option<f32>, max: Option<f32> },
    /// `radius` rounds the image's corners.
    Image { resource: ResourceRef, area: Area, radius: f32 },
    /// Degrees, clockwise from 12 o'clock.
    Arc { cx: f32, cy: f32, r: f32, from: f32, to: f32, width: f32, color: Color },
}

enum Image {
    Raster(Pixmap),
    Vector(Box<usvg::Tree>),
}

/// Reads a resource's bytes; `None` for a live resource the host half hasn't sent yet.
pub type ReadResource<'a> = &'a dyn Fn(&ResourceRef) -> Result<Option<Vec<u8>>>;

const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";

/// A module's images, fonts and rasterised glyphs, made on first use.
pub struct ResourceCache {
    decoded: HashMap<ResourceRef, Image>,
    fonts: HashMap<Resource, Font>,
    glyphs: GlyphCache,
}

impl ResourceCache {
    pub fn new() -> Self {
        Self { decoded: HashMap::new(), fonts: HashMap::new(), glyphs: GlyphCache::default() }
    }

    /// The font a text command draws with (its own, else `default`), and the glyph cache to draw from.
    fn text_font<'a>(&'a mut self, chosen: Option<&Resource>, default: &'a Font, read: ReadResource) -> Result<(&'a Font, &'a mut GlyphCache)> {
        let Some(resource) = chosen else { return Ok((default, &mut self.glyphs)) };
        if !self.fonts.contains_key(resource) {
            let packaged = ResourceRef::Packaged(resource.clone());
            let bytes = read(&packaged)?.with_context(|| format!("{resource} has no bytes"))?;
            let font = Font::from_bytes(bytes, FontSettings::default()).map_err(|e| anyhow!("parsing {resource} as a font: {e}"))?;
            self.fonts.insert(resource.clone(), font);
        }
        Ok((&self.fonts[resource], &mut self.glyphs))
    }

    /// For a live resource that changed, so the next draw decodes the new bytes.
    pub fn forget(&mut self, resource: &ResourceRef) {
        self.decoded.remove(resource);
    }

    fn get(&mut self, resource: &ResourceRef, read: ReadResource) -> Result<Option<&Image>> {
        if !self.decoded.contains_key(resource) {
            let Some(bytes) = read(resource)? else { return Ok(None) };
            let image = match resource {
                ResourceRef::Live(_) if bytes.starts_with(PNG_SIGNATURE) => raster(&bytes, resource)?,
                ResourceRef::Live(_) => bail!("{resource} isn't a PNG"),
                ResourceRef::Packaged(packaged) => {
                    match packaged.name.as_str().rsplit_once('.').map(|(_, ext)| ext.to_ascii_lowercase()).as_deref() {
                        Some("png") => raster(&bytes, resource)?,
                        Some("svg") => Image::Vector(Box::new(
                            usvg::Tree::from_data(&bytes, &usvg::Options::default()).with_context(|| format!("parsing {resource}"))?,
                        )),
                        _ => bail!("{resource} is not a .png or .svg"),
                    }
                }
            };
            self.decoded.insert(resource.clone(), image);
        }
        Ok(self.decoded.get(resource))
    }
}

/// Rasterised glyphs by font, character and pixel size, so unchanged text isn't rasterised again.
#[derive(Default)]
struct GlyphCache {
    glyphs: HashMap<(Option<Resource>, char, u32), (Metrics, Vec<u8>)>,
}

impl GlyphCache {
    fn glyph(&mut self, font: &Font, which: Option<&Resource>, c: char, px: f32) -> &(Metrics, Vec<u8>) {
        let key = (which.cloned(), c, px.to_bits());
        if !self.glyphs.contains_key(&key) && self.glyphs.len() >= MAX_CACHED_GLYPHS {
            self.glyphs.clear();
        }
        self.glyphs.entry(key).or_insert_with(|| font.rasterize(c, px))
    }
}

fn raster(bytes: &[u8], resource: &ResourceRef) -> Result<Image> {
    Ok(Image::Raster(Pixmap::decode_png(bytes).map_err(|e| anyhow!("decoding {resource}: {e}"))?))
}

/// Draws `commands` into a transparent tile of `size`; anything outside it is clipped.
pub fn render(
    commands: &[Command],
    size: (u32, u32),
    font: &Font,
    images: &mut ResourceCache,
    read_resource: ReadResource,
) -> Result<Pixmap> {
    let mut pixmap = Pixmap::new(size.0, size.1).context("a tile can't be empty")?;
    for command in commands {
        match command {
            Command::Text { text, x, y, size, color, align, width, font: chosen } => {
                let (font, glyphs) = images.text_font(chosen.as_ref(), font, read_resource)?;
                let face = Face { font, which: chosen.as_ref(), glyphs };
                draw_text(&mut pixmap, face, text, (*x, *y), *size, *color, *align, *width)
            }
            Command::Rect { area, fill, stroke, radius } => {
                if let Some(path) = rounded_rect(*area, *radius) {
                    if let Some(fill) = fill {
                        pixmap.fill_path(&path, &fill.paint(), FillRule::Winding, Transform::identity(), None);
                    }
                    if let Some((color, width)) = stroke {
                        let stroke = Stroke { width: *width, ..Stroke::default() };
                        pixmap.stroke_path(&path, &color.paint(), &stroke, Transform::identity(), None);
                    }
                }
            }
            Command::Bar { fraction, area, color } => {
                let radius = area.h / 2.0;
                if let Some(track) = rounded_rect(*area, radius) {
                    pixmap.fill_path(&track, &color.with_alpha_scaled(64).paint(), FillRule::Winding, Transform::identity(), None);
                }
                let filled = Area { w: area.w * fraction.clamp(0.0, 1.0), ..*area };
                if let Some(bar) = rounded_rect(filled, radius.min(filled.w / 2.0)) {
                    pixmap.fill_path(&bar, &color.paint(), FillRule::Winding, Transform::identity(), None);
                }
            }
            Command::Graph { values, area, color, min, max } => {
                if let Some(path) = graph_path(values, *area, *min, *max) {
                    let stroke = Stroke { width: 2.0, line_join: LineJoin::Round, ..Stroke::default() };
                    pixmap.stroke_path(&path, &color.paint(), &stroke, Transform::identity(), None);
                }
            }
            Command::Image { resource, area, radius } => {
                let corners = rounded_corners(&pixmap, *area, *radius);
                match images.get(resource, read_resource)? {
                    Some(Image::Raster(image)) => {
                        let scale = Transform::from_row(area.w / image.width() as f32, 0.0, 0.0, area.h / image.height() as f32, area.x, area.y);
                        let paint = PixmapPaint { quality: tiny_skia::FilterQuality::Bilinear, ..PixmapPaint::default() };
                        pixmap.draw_pixmap(0, 0, image.as_ref(), &paint, scale, corners.as_ref());
                    }
                    Some(Image::Vector(tree)) => {
                        let natural = tree.size();
                        let scale = Transform::from_row(area.w / natural.width(), 0.0, 0.0, area.h / natural.height(), area.x, area.y);
                        match &corners {
                            None => resvg::render(tree, scale, &mut pixmap.as_mut()),
                            Some(corners) => {
                                let mut layer = Pixmap::new(pixmap.width(), pixmap.height()).context("allocating an image layer")?;
                                resvg::render(tree, scale, &mut layer.as_mut());
                                pixmap.draw_pixmap(0, 0, layer.as_ref(), &PixmapPaint::default(), Transform::identity(), Some(corners));
                            }
                        }
                    }
                    None => {}
                }
            }
            Command::Arc { cx, cy, r, from, to, width, color } => {
                if let Some(path) = arc_path(*cx, *cy, *r, *from, *to) {
                    let stroke = Stroke { width: *width, line_cap: LineCap::Round, ..Stroke::default() };
                    pixmap.stroke_path(&path, &color.paint(), &stroke, Transform::identity(), None);
                }
            }
        }
    }
    Ok(pixmap)
}

/// The tile as premultiplied ARGB words, the key layer's format.
pub fn to_argb(pixmap: &Pixmap) -> Vec<u32> {
    let (pixels, _) = pixmap.data().as_chunks::<4>();
    pixels
        .iter()
        .map(|p| (p[3] as u32) << 24 | (p[0] as u32) << 16 | (p[1] as u32) << 8 | p[2] as u32)
        .collect()
}

/// A font to draw text with, and where its glyphs are cached.
struct Face<'a> {
    font: &'a Font,
    /// The module's own font, or `None` for the built-in one
    which: Option<&'a Resource>,
    glyphs: &'a mut GlyphCache,
}

#[allow(clippy::too_many_arguments)]
fn draw_text(pixmap: &mut Pixmap, face: Face, text: &str, at: (f32, f32), size: f32, color: Color, align: Align, width: Option<f32>) {
    let Face { font, which, glyphs } = face;
    let natural: f32 = text.chars().map(|c| font.metrics(c, size).advance_width).sum();
    let px = match width {
        Some(width) if natural > width && natural > 0.0 => size * width / natural,
        _ => size,
    };
    let text_width = natural * px / size;
    let left = match (align, width) {
        (Align::Left, _) => at.0,
        (Align::Center, Some(w)) => at.0 + (w - text_width) / 2.0,
        (Align::Right, Some(w)) => at.0 + w - text_width,
        (Align::Center, None) => at.0 - text_width / 2.0,
        (Align::Right, None) => at.0 - text_width,
    };
    let ascent = font.horizontal_line_metrics(px).map_or(px * 0.8, |m| m.ascent);
    let baseline = at.1 + ascent;
    let shadow = Color { r: 0, g: 0, b: 0, a: SHADOW_ALPHA }.with_alpha_scaled(color.a);
    for (offset, color) in [(SHADOW_OFFSET, shadow), (0.0, color)] {
        let mut pen = left + offset;
        for c in text.chars() {
            let (metrics, coverage) = glyphs.glyph(font, which, c, px);
            let gx = pen.round() as i32 + metrics.xmin;
            let gy = (baseline + offset).round() as i32 - metrics.ymin - metrics.height as i32;
            for row in 0..metrics.height {
                for col in 0..metrics.width {
                    blend(pixmap, gx + col as i32, gy + row as i32, color, coverage[row * metrics.width + col]);
                }
            }
            pen += metrics.advance_width;
        }
    }
}

/// Source-over of `color` at `coverage` onto one premultiplied RGBA pixel.
fn blend(pixmap: &mut Pixmap, x: i32, y: i32, color: Color, coverage: u8) {
    let (width, height) = (pixmap.width() as i32, pixmap.height() as i32);
    if coverage == 0 || x < 0 || y < 0 || x >= width || y >= height {
        return;
    }
    let a = color.a as u32 * coverage as u32 / 255;
    let keep = 255 - a;
    let at = (y * width + x) as usize * 4;
    let pixel = &mut pixmap.data_mut()[at..at + 4];
    for (channel, source) in pixel.iter_mut().zip([color.r, color.g, color.b]) {
        *channel = ((source as u32 * a + *channel as u32 * keep + 127) / 255) as u8;
    }
    pixel[3] = (a + (pixel[3] as u32 * keep + 127) / 255) as u8;
}

/// A mask that keeps `area` with rounded corners; `None` when nothing is rounded.
fn rounded_corners(pixmap: &Pixmap, area: Area, radius: f32) -> Option<Mask> {
    if radius <= 0.0 {
        return None;
    }
    let mut mask = Mask::new(pixmap.width(), pixmap.height())?;
    mask.fill_path(&rounded_rect(area, radius)?, FillRule::Winding, true, Transform::identity());
    Some(mask)
}

fn rounded_rect(area: Area, radius: f32) -> Option<Path> {
    let Area { x, y, w, h } = area;
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    let r = radius.clamp(0.0, w.min(h) / 2.0);
    let k = r * KAPPA;
    let mut path = PathBuilder::new();
    path.move_to(x + r, y);
    path.line_to(x + w - r, y);
    path.cubic_to(x + w - r + k, y, x + w, y + r - k, x + w, y + r);
    path.line_to(x + w, y + h - r);
    path.cubic_to(x + w, y + h - r + k, x + w - r + k, y + h, x + w - r, y + h);
    path.line_to(x + r, y + h);
    path.cubic_to(x + r - k, y + h, x, y + h - r + k, x, y + h - r);
    path.line_to(x, y + r);
    path.cubic_to(x, y + r - k, x + r - k, y, x + r, y);
    path.close();
    path.finish()
}

fn graph_path(values: &[f32], area: Area, min: Option<f32>, max: Option<f32>) -> Option<Path> {
    if values.len() < 2 {
        return None;
    }
    let low = min.unwrap_or_else(|| values.iter().copied().fold(f32::INFINITY, f32::min));
    let high = max.unwrap_or_else(|| values.iter().copied().fold(f32::NEG_INFINITY, f32::max));
    let span = if high > low { high - low } else { 1.0 };
    let step = area.w / (values.len() - 1) as f32;
    let mut path = PathBuilder::new();
    for (i, value) in values.iter().enumerate() {
        let x = area.x + i as f32 * step;
        let y = area.y + area.h - ((value - low) / span).clamp(0.0, 1.0) * area.h;
        if i == 0 { path.move_to(x, y) } else { path.line_to(x, y) }
    }
    path.finish()
}

fn arc_path(cx: f32, cy: f32, r: f32, from: f32, to: f32) -> Option<Path> {
    let steps = ((to - from).abs() / ARC_STEP_DEGREES).ceil().max(1.0) as usize;
    let point = |degrees: f32| {
        let radians = degrees * PI / 180.0;
        (cx + r * radians.sin(), cy - r * radians.cos())
    };
    let mut path = PathBuilder::new();
    let (x, y) = point(from);
    path.move_to(x, y);
    for i in 1..=steps {
        let (x, y) = point(from + (to - from) * i as f32 / steps as f32);
        path.line_to(x, y);
    }
    path.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_colors() {
        assert_eq!(Color::parse("#ff8000").unwrap(), Color { r: 255, g: 128, b: 0, a: 255 });
        assert_eq!(Color::parse("#00000080").unwrap(), Color { r: 0, g: 0, b: 0, a: 128 });
        for bad in ["ff8000", "#ff80", "#gg0000"] {
            assert!(Color::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn rect_fills_only_its_area() {
        let commands = [Command::Rect {
            area: Area { x: 2.0, y: 2.0, w: 4.0, h: 4.0 },
            fill: Some(Color { r: 255, g: 0, b: 0, a: 255 }),
            stroke: None,
            radius: 0.0,
        }];
        let no_assets = |resource: &ResourceRef| -> Result<Option<Vec<u8>>> { bail!("no {resource}") };
        let tile = render(&commands, (8, 8), &test_font(), &mut ResourceCache::new(), &no_assets).unwrap();
        let argb = to_argb(&tile);
        assert_eq!(argb[3 * 8 + 3], 0xFF_FF_00_00);
        assert_eq!(argb[0], 0);
    }

    #[test]
    fn a_missing_resource_fails_loudly() {
        let name = glassdeck_package::ResourceName::parse("missing.png").unwrap();
        let missing = ResourceRef::Packaged(glassdeck_package::Resource { kind: glassdeck_package::ResourceKind::Keyboard, name });
        let commands = [Command::Image { resource: missing, area: Area { x: 0.0, y: 0.0, w: 4.0, h: 4.0 }, radius: 0.0 }];
        let no_assets = |resource: &ResourceRef| -> Result<Option<Vec<u8>>> { bail!("no {resource}") };
        assert!(render(&commands, (8, 8), &test_font(), &mut ResourceCache::new(), &no_assets).is_err());
    }

    #[test]
    fn a_live_resource_not_sent_yet_draws_nothing() {
        let cover = ResourceRef::Live(glassdeck_luau::LiveName::parse("cover").unwrap());
        let commands = [Command::Image { resource: cover, area: Area { x: 0.0, y: 0.0, w: 4.0, h: 4.0 }, radius: 0.0 }];
        let not_yet = |_: &ResourceRef| -> Result<Option<Vec<u8>>> { Ok(None) };
        let tile = render(&commands, (8, 8), &test_font(), &mut ResourceCache::new(), &not_yet).unwrap();
        assert!(tile.pixels().iter().all(|p| p.alpha() == 0));
    }

    #[test]
    fn opaque_blend_takes_the_color() {
        let mut pixmap = Pixmap::new(1, 1).unwrap();
        blend(&mut pixmap, 0, 0, Color { r: 10, g: 20, b: 30, a: 255 }, 255);
        assert_eq!(pixmap.data(), &[10, 20, 30, 255]);
        blend(&mut pixmap, 5, 5, Color::WHITE, 255);
    }

    fn test_font() -> Font {
        crate::load_font(None).unwrap()
    }

    #[test]
    fn the_built_in_font_draws_latin_cyrillic_and_japanese() {
        let font = test_font();
        for c in ['A', 'ж', 'あ', '漢'] {
            let (metrics, coverage) = font.rasterize(c, 24.0);
            assert!(metrics.width > 0 && coverage.iter().any(|&a| a > 128), "{c} drew nothing");
        }
    }
}
