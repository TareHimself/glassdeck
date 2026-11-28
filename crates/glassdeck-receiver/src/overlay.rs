use fontdue::Font;
use glassdeck_layout::{Layout, PANEL_HEIGHT, PANEL_WIDTH, Variant};

use glassdeck_layout::keymap::Keymap;

/// Keycaps as the app's Overview draws them, in panel pixels: a translucent cap with a faint
/// border and a small label in its top-left corner.
const CAP_RADIUS: f32 = 13.0;
const CAP_FILL: u32 = 0x06_04_14;
const CAP_FILL_ALPHA: u8 = 77;
const CAP_BORDER: u32 = 0xFF_FF_FF;
const CAP_BORDER_ALPHA: u8 = 36;
const CAP_BORDER_WIDTH: f32 = 2.0;
const LABEL_PX: f32 = 17.0;
const LABEL_PAD: (f32, f32) = (11.0, 9.0);
const LABEL_COLOR: u32 = 0xFF_FF_FF;
const LABEL_ALPHA: u8 = 217;

/// What the key labels need: a font and which keyboard layout to place them on.
pub struct Labels {
    pub font: Font,
    pub variant: Variant,
    /// What each key does, so remapping a key relabels it
    pub keymap: Keymap,
}

/// The ISO Enter key spans two of the layout's rectangles; only the upper one is labelled.
const ISO_ENTER_LOWER_HALF: usize = 59;

/// Labels from the keymap's base layer, in the layout's key order, until profiles bring their own.
fn base_labels(keymap: &Keymap, variant: Variant) -> Vec<String> {
    (0..keymap.keys())
        .map(|key| match (variant, key) {
            (Variant::Iso, ISO_ENTER_LOWER_HALF) => String::new(),
            _ => keymap.label(0, key),
        })
        .collect()
}

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
    /// Draws the labels at the keys' real positions, scaled from the panel to `width` x `height`.
    pub fn build(labels: &Labels, width: usize, height: usize) -> Self {
        let Labels { font, variant, keymap } = labels;
        let variant = *variant;
        let mut canvas = Canvas { width, height, pixels: Vec::new() };
        let (scale_x, scale_y) = (width as f32 / PANEL_WIDTH, height as f32 / PANEL_HEIGHT);
        let layout = Layout::new(variant);
        for (key, label) in layout.keys.iter().zip(base_labels(keymap, variant)) {
            let cap = Rect {
                x: (key.x * scale_x).round() as i32,
                y: (key.y * scale_y).round() as i32,
                w: (key.width * scale_x).round() as i32,
                h: (key.height * scale_y).round() as i32,
            };
            canvas.cap(cap, CAP_RADIUS * scale_y, CAP_BORDER_WIDTH * scale_y);
            let (pad_x, pad_y) = (LABEL_PAD.0 * scale_x, LABEL_PAD.1 * scale_y);
            let label_px = fitted_size(font, &label, LABEL_PX * scale_y, cap.w as f32 - 2.0 * pad_x);
            canvas.text(font, &label, cap.x as f32 + pad_x, cap.y as f32 + pad_y, label_px, LABEL_COLOR, LABEL_ALPHA);
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

/// The preferred size, or smaller if the label would overflow its key.
fn fitted_size(font: &Font, label: &str, preferred_px: f32, max_width: f32) -> f32 {
    let width: f32 = label.chars().map(|c| font.metrics(c, preferred_px).advance_width).sum();
    if width > max_width { preferred_px * max_width / width } else { preferred_px }
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

    /// A rounded keycap: the fill, then the border ring over it, both anti-aliased at the corners.
    fn cap(&mut self, r: Rect, radius: f32, border: f32) {
        let (half_w, half_h) = (r.w as f32 / 2.0, r.h as f32 / 2.0);
        let radius = radius.min(half_w).min(half_h);
        for y in r.y..r.y + r.h {
            for x in r.x..r.x + r.w {
                let (px, py) = (x as f32 + 0.5 - r.x as f32 - half_w, y as f32 + 0.5 - r.y as f32 - half_h);
                let edge = rounded_box_distance(px, py, half_w, half_h, radius);
                let inside = (0.5 - edge).clamp(0.0, 1.0);
                let ring = inside - (0.5 - (edge + border)).clamp(0.0, 1.0);
                self.plot(x, y, CAP_FILL, (inside * CAP_FILL_ALPHA as f32) as u8);
                self.plot(x, y, CAP_BORDER, (ring * CAP_BORDER_ALPHA as f32) as u8);
            }
        }
    }

    /// `text` with its top-left at (`left`, `top`).
    fn text(&mut self, font: &Font, text: &str, left: f32, top: f32, px: f32, color: u32, alpha: u8) {
        let baseline = top + px * 0.8;
        let mut pen_x = left;
        for c in text.chars() {
            let (metrics, coverage) = font.rasterize(c, px);
            let gx = pen_x.round() as i32 + metrics.xmin;
            let gy = baseline.round() as i32 - metrics.ymin - metrics.height as i32;
            for row in 0..metrics.height {
                for col in 0..metrics.width {
                    let a = (coverage[row * metrics.width + col] as u32 * alpha as u32 / 255) as u8;
                    self.plot(gx + col as i32, gy + row as i32, color, a);
                }
            }
            pen_x += metrics.advance_width;
        }
    }
}

/// Signed distance from a point (relative to the box's centre) to a rounded box's edge; negative inside.
fn rounded_box_distance(px: f32, py: f32, half_w: f32, half_h: f32, radius: f32) -> f32 {
    let (qx, qy) = (px.abs() - (half_w - radius), py.abs() - (half_h - radius));
    let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
    outside + qx.max(qy).min(0.0) - radius
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
    fn every_key_has_a_label() {
        for variant in [Variant::Ansi, Variant::Iso] {
            assert_eq!(base_labels(&Keymap::default_for(variant), variant).len(), Layout::new(variant).keys.len(), "{variant:?}");
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
