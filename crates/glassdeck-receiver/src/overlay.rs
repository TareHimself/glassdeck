use fontdue::Font;

const COLUMNS: f32 = 15.0;
const KEY_INSET: f32 = 6.0;
const OUTLINE: usize = 2;
const OUTLINE_COLOR: u32 = 0xFF_FF_FF;
const OUTLINE_ALPHA: u8 = 70;
const LABEL_COLOR: u32 = 0xFF_FF_FF;
const LABEL_ALPHA: u8 = 235;
const SHADOW_COLOR: u32 = 0x00_00_00;
const SHADOW_ALPHA: u8 = 170;
const SHADOW_OFFSET: i32 = 2;

/// Placeholder layout, in key-width units summing to 15 per row, until we read the real one off the keyboard.
const ROWS: &[&[(&str, f32)]] = &[
    &[("Esc", 1.0), ("1", 1.0), ("2", 1.0), ("3", 1.0), ("4", 1.0), ("5", 1.0), ("6", 1.0), ("7", 1.0), ("8", 1.0), ("9", 1.0), ("0", 1.0), ("-", 1.0), ("=", 1.0), ("Bksp", 2.0)],
    &[("Tab", 1.5), ("Q", 1.0), ("W", 1.0), ("E", 1.0), ("R", 1.0), ("T", 1.0), ("Y", 1.0), ("U", 1.0), ("I", 1.0), ("O", 1.0), ("P", 1.0), ("[", 1.0), ("]", 1.0), ("\\", 1.5)],
    &[("Caps", 1.75), ("A", 1.0), ("S", 1.0), ("D", 1.0), ("F", 1.0), ("G", 1.0), ("H", 1.0), ("J", 1.0), ("K", 1.0), ("L", 1.0), (";", 1.0), ("'", 1.0), ("Enter", 2.25)],
    &[("Shift", 2.25), ("Z", 1.0), ("X", 1.0), ("C", 1.0), ("V", 1.0), ("B", 1.0), ("N", 1.0), ("M", 1.0), (",", 1.0), (".", 1.0), ("/", 1.0), ("Shift", 2.75)],
    &[("Ctrl", 1.25), ("Win", 1.25), ("Alt", 1.25), ("", 6.25), ("Alt", 1.25), ("Fn", 1.25), ("Menu", 1.25), ("Ctrl", 1.25)],
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct OverlayPixel {
    index: u32,
    color: u32,
    alpha: u8,
}

/// Pre-rendered key labels, stored sparsely so compositing touches only covered pixels.
pub struct Overlay {
    width: usize,
    height: usize,
    pixels: Vec<OverlayPixel>,
}

impl Overlay {
    pub fn build(font: &Font, width: usize, height: usize) -> Self {
        let mut canvas = Canvas { width, height, pixels: Vec::new() };
        let unit = width as f32 / COLUMNS;
        let row_height = height as f32 / ROWS.len() as f32;
        let label_px = row_height * 0.22;

        for (row, keys) in ROWS.iter().enumerate() {
            let top = row as f32 * row_height;
            let mut left = 0.0;
            for &(label, units) in *keys {
                let key = Rect {
                    x: (left + KEY_INSET) as i32,
                    y: (top + KEY_INSET) as i32,
                    w: (units * unit - 2.0 * KEY_INSET) as i32,
                    h: (row_height - 2.0 * KEY_INSET) as i32,
                };
                canvas.outline(key, OUTLINE_COLOR, OUTLINE_ALPHA);
                canvas.text(font, label, key, label_px, SHADOW_OFFSET, SHADOW_COLOR, SHADOW_ALPHA);
                canvas.text(font, label, key, label_px, 0, LABEL_COLOR, LABEL_ALPHA);
                left += units * unit;
            }
        }
        Self { width, height, pixels: canvas.pixels }
    }

    #[cfg(windows)]
    pub fn fits(&self, width: usize, height: usize) -> bool {
        (self.width, self.height) == (width, height)
    }

    #[cfg(windows)]
    pub fn composite_onto(&self, frame: &mut [u32]) {
        for p in &self.pixels {
            let dst = &mut frame[p.index as usize];
            *dst = blend(*dst, p.color, p.alpha);
        }
    }

    /// The labels as a premultiplied-alpha ARGB image, for a display plane that blends in hardware.
    #[cfg_attr(windows, allow(dead_code))]
    pub fn premultiplied_argb(&self) -> Vec<u32> {
        let mut image = vec![0u32; self.width * self.height];
        for p in &self.pixels {
            let dst = &mut image[p.index as usize];
            *dst = over_premultiplied(*dst, p.color, p.alpha);
        }
        image
    }
}

#[derive(Clone, Copy)]
struct Rect {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

struct Canvas {
    width: usize,
    height: usize,
    pixels: Vec<OverlayPixel>,
}

impl Canvas {
    fn plot(&mut self, x: i32, y: i32, color: u32, alpha: u8) {
        let inside = x >= 0 && y >= 0 && (x as usize) < self.width && (y as usize) < self.height;
        if inside && alpha > 0 {
            let index = (y as usize * self.width + x as usize) as u32;
            self.pixels.push(OverlayPixel { index, color, alpha });
        }
    }

    fn outline(&mut self, r: Rect, color: u32, alpha: u8) {
        for t in 0..OUTLINE as i32 {
            for x in r.x..r.x + r.w {
                self.plot(x, r.y + t, color, alpha);
                self.plot(x, r.y + r.h - 1 - t, color, alpha);
            }
            for y in r.y + OUTLINE as i32..r.y + r.h - OUTLINE as i32 {
                self.plot(r.x + t, y, color, alpha);
                self.plot(r.x + r.w - 1 - t, y, color, alpha);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn text(&mut self, font: &Font, text: &str, within: Rect, px: f32, offset: i32, color: u32, alpha: u8) {
        let glyphs: Vec<_> = text.chars().map(|c| font.rasterize(c, px)).collect();
        let text_width: f32 = glyphs.iter().map(|(m, _)| m.advance_width).sum();
        let mut pen_x = within.x as f32 + (within.w as f32 - text_width) / 2.0;
        let baseline = within.y as f32 + within.h as f32 / 2.0 + px * 0.35;

        for (metrics, coverage) in &glyphs {
            let gx = pen_x.round() as i32 + metrics.xmin + offset;
            let gy = baseline.round() as i32 - metrics.ymin - metrics.height as i32 + offset;
            for row in 0..metrics.height {
                for col in 0..metrics.width {
                    let c = coverage[row * metrics.width + col] as u32;
                    let a = (c * alpha as u32 / 255) as u8;
                    self.plot(gx + col as i32, gy + row as i32, color, a);
                }
            }
            pen_x += metrics.advance_width;
        }
    }
}

#[cfg_attr(not(windows), allow(dead_code))]
fn blend(dst: u32, src: u32, alpha: u8) -> u32 {
    let a = alpha as u32;
    let mix = |shift: u32| {
        let s = (src >> shift) & 0xFF;
        let d = (dst >> shift) & 0xFF;
        ((s * a + d * (255 - a) + 127) / 255) << shift
    };
    mix(16) | mix(8) | mix(0)
}

/// Porter-Duff "source over" for a straight-alpha source onto a premultiplied ARGB destination.
#[cfg_attr(windows, allow(dead_code))]
fn over_premultiplied(dst: u32, color: u32, alpha: u8) -> u32 {
    let a = alpha as u32;
    let keep = 255 - a;
    let scale = |v: u32| (v * keep + 127) / 255;
    let channel = |shift: u32| ((((color >> shift) & 0xFF) * a + 127) / 255 + scale((dst >> shift) & 0xFF)) << shift;
    (a + scale(dst >> 24)) << 24 | channel(16) | channel(8) | channel(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opaque_blend_takes_source() {
        assert_eq!(blend(0x12_34_56, 0xAB_CD_EF, 255), 0xAB_CD_EF);
    }

    #[test]
    fn transparent_blend_keeps_destination() {
        assert_eq!(blend(0x12_34_56, 0xAB_CD_EF, 0), 0x12_34_56);
    }

    #[test]
    fn half_blend_mixes_each_channel() {
        assert_eq!(blend(0x00_00_00, 0xFE_FE_FE, 128), 0x7F_7F_7F);
    }

    #[test]
    fn opaque_label_pixel_is_stored_as_is() {
        assert_eq!(over_premultiplied(0, 0xAB_CD_EF, 255), 0xFF_AB_CD_EF);
    }

    #[test]
    fn translucent_label_pixel_is_premultiplied() {
        assert_eq!(over_premultiplied(0, 0xFF_FF_FF, 128), 0x80_80_80_80);
    }

    #[test]
    fn label_over_shadow_stays_opaque_where_the_shadow_was() {
        let shadow = over_premultiplied(0, 0x00_00_00, 255);
        assert_eq!(over_premultiplied(shadow, 0xFF_FF_FF, 128), 0xFF_80_80_80);
    }

    #[test]
    fn every_row_spans_the_full_width() {
        for keys in ROWS {
            let units: f32 = keys.iter().map(|(_, u)| u).sum();
            assert!((units - COLUMNS).abs() < 1e-3, "row sums to {units}");
        }
    }

    #[test]
    fn plots_outside_the_canvas_are_dropped() {
        let mut canvas = Canvas { width: 4, height: 4, pixels: Vec::new() };
        canvas.plot(-1, 0, 0, 255);
        canvas.plot(4, 0, 0, 255);
        canvas.plot(3, 3, 0, 255);
        assert_eq!(canvas.pixels, vec![OverlayPixel { index: 15, color: 0, alpha: 255 }]);
    }
}
