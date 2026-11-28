//! Where every key and the widget area sit on the Flux keyboard's 1920x1080 panel.
//!
//! Geometry adapted from Keyglass (github.com/keyglass-dev/keyglass, keyboard/display/src/layout.rs),
//! Copyright (c) 2026 The OpenFlux Contributors, MIT OR Apache-2.0.

pub mod keymap;

pub const PANEL_WIDTH: f32 = 1920.0;
pub const PANEL_HEIGHT: f32 = 1080.0;

/// The panel's active width, so pixels per millimetre follow from it.
const PANEL_WIDTH_MM: f32 = 309.31;
const PX_PER_MM: f32 = PANEL_WIDTH / PANEL_WIDTH_MM;
const KEY_HEIGHT_MM: f32 = 17.0;
const KEY_GAP_MM: f32 = 2.0;
/// Gap under each row, top row first: the F-row sits apart from the rest.
const GAP_BELOW_ROW_MM: [f32; ROWS] = [6.75, 2.0, 2.0, 2.0, 2.0, 3.07];
const ROWS: usize = 6;

const WIDGET_AREA_MARGIN_X: f32 = 8.1;
const WIDGET_AREA_TOP: f32 = 5.0;
/// Space left between the widget area and the F-row, as Keyglass leaves it.
const WIDGET_AREA_GAP_ABOVE_KEYS: f32 = 80.0;

const U: f32 = 17.0;

/// Key widths in millimetres per row, left to right; rows are centred on the panel.
const ANSI: [&[f32]; ROWS] = [
    &[U; 16],
    &[U, U, U, U, U, U, U, U, U, U, U, U, U, 36.0, U],
    &[26.53, U, U, U, U, U, U, U, U, U, U, U, U, 26.53, U],
    &[31.29, U, U, U, U, U, U, U, U, U, U, U, 40.82, U],
    &[40.82, U, U, U, U, U, U, U, U, U, U, 31.29, U, U],
    &[21.77, U, U, U, 107.49, U, U, U, U, U, U],
];

const ISO: [&[f32]; ROWS] = [
    &[U; 16],
    &[U, U, U, U, U, U, U, U, U, U, U, U, U, 36.0, U],
    &[26.53, U, U, U, U, U, U, U, U, U, U, U, U, 26.53, U],
    &[31.29, U, U, U, U, U, U, U, U, U, U, U, U, 21.77, U],
    &[21.77, U, U, U, U, U, U, U, U, U, U, U, 31.29, U, U],
    &[21.77, U, U, U, 107.49, U, U, U, U, U, U],
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Variant {
    Ansi,
    Iso,
}

/// Panel pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    pub fn centre(&self) -> (f32, f32) {
        (self.x + self.width / 2.0, self.y + self.height / 2.0)
    }
}

pub struct Layout {
    pub variant: Variant,
    /// Indexed like the firmware's pressed-key bitmap: row by row, left to right.
    pub keys: Vec<Rect>,
    /// The band above the F-row where widgets go.
    pub widget_area: Rect,
}

impl Layout {
    pub fn new(variant: Variant) -> Self {
        let widths = match variant {
            Variant::Ansi => ANSI,
            Variant::Iso => ISO,
        };
        let tops = row_tops_mm();
        let keys = widths.iter().zip(tops).flat_map(|(row, top)| row_keys(row, top)).collect();
        let keys_top = tops[0] * PX_PER_MM;
        let widget_area = Rect {
            x: WIDGET_AREA_MARGIN_X,
            y: WIDGET_AREA_TOP,
            width: PANEL_WIDTH - 2.0 * WIDGET_AREA_MARGIN_X,
            height: keys_top - WIDGET_AREA_GAP_ABOVE_KEYS - WIDGET_AREA_TOP,
        };
        Self { variant, keys, widget_area }
    }

    /// The key under a panel point, for touches and effects.
    pub fn key_at(&self, x: f32, y: f32) -> Option<usize> {
        self.keys.iter().position(|k| x >= k.x && x < k.x + k.width && y >= k.y && y < k.y + k.height)
    }
}

/// Rows stack up from the panel's bottom edge.
fn row_tops_mm() -> [f32; ROWS] {
    let mut tops = [0.0; ROWS];
    let mut bottom = PANEL_HEIGHT / PX_PER_MM;
    for row in (0..ROWS).rev() {
        bottom -= GAP_BELOW_ROW_MM[row];
        tops[row] = bottom - KEY_HEIGHT_MM;
        bottom = tops[row];
    }
    tops
}

fn row_keys(widths_mm: &[f32], top_mm: f32) -> impl Iterator<Item = Rect> + '_ {
    let row_mm = widths_mm.iter().sum::<f32>() + (widths_mm.len() - 1) as f32 * KEY_GAP_MM;
    let mut left_mm = (PANEL_WIDTH_MM - row_mm) / 2.0;
    widths_mm.iter().map(move |&width_mm| {
        let key = Rect {
            x: left_mm * PX_PER_MM,
            y: top_mm * PX_PER_MM,
            width: width_mm * PX_PER_MM,
            height: KEY_HEIGHT_MM * PX_PER_MM,
        };
        left_mm += width_mm + KEY_GAP_MM;
        key
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.5
    }

    #[test]
    fn key_counts_match_the_keyboards() {
        assert_eq!(Layout::new(Variant::Ansi).keys.len(), 85);
        assert_eq!(Layout::new(Variant::Iso).keys.len(), 87);
    }

    #[test]
    fn bottom_row_sits_its_margin_above_the_panel_edge() {
        let layout = Layout::new(Variant::Ansi);
        let space = layout.keys.last().unwrap();
        assert!(close(space.y + space.height, PANEL_HEIGHT - 3.07 * PX_PER_MM));
    }

    #[test]
    fn every_row_is_centred() {
        for variant in [Variant::Ansi, Variant::Iso] {
            let layout = Layout::new(variant);
            let mut rows: Vec<Vec<Rect>> = Vec::new();
            for key in &layout.keys {
                match rows.last_mut() {
                    Some(row) if close(row[0].y, key.y) => row.push(*key),
                    _ => rows.push(vec![*key]),
                }
            }
            assert_eq!(rows.len(), ROWS);
            for row in rows {
                let (first, last) = (row[0], row[row.len() - 1]);
                assert!(close(first.x, PANEL_WIDTH - (last.x + last.width)), "{variant:?} row at {}", first.y);
            }
        }
    }

    #[test]
    fn widget_area_spans_the_top_and_stops_above_the_keys() {
        let layout = Layout::new(Variant::Ansi);
        let area = layout.widget_area;
        assert!(close(area.x, WIDGET_AREA_MARGIN_X) && close(area.x + area.width, PANEL_WIDTH - WIDGET_AREA_MARGIN_X));
        assert!(close(area.y + area.height + WIDGET_AREA_GAP_ABOVE_KEYS, layout.keys[0].y));
        assert!(area.height > 200.0);
    }

    #[test]
    fn finds_the_key_under_a_point() {
        let layout = Layout::new(Variant::Ansi);
        let (x, y) = layout.keys[20].centre();
        assert_eq!(layout.key_at(x, y), Some(20));
        assert_eq!(layout.key_at(1.0, 1.0), None);
    }
}
