//! Where widgets sit in the widget area, and the rules a layout must keep. An instance's number is its
//! place in the list, on both host and keyboard.

use anyhow::{Result, bail, ensure};
use glassdeck_layout::{Layout, Variant};
use glassdeck_package::{QualifiedId, Size, WidgetSize};

/// Positions snap to this, so widgets line up.
pub const GRID: u32 = 8;

/// The widget area in whole panel pixels: where widget (0, 0) sits, and how much room there is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WidgetArea {
    pub x: u32,
    pub y: u32,
    pub size: Size,
}

impl WidgetArea {
    /// The same on every layout: the F-row, which the area stops above, doesn't change.
    pub fn of_panel() -> Self {
        let area = Layout::new(Variant::Ansi).widget_area;
        Self {
            x: area.x.round() as u32,
            y: area.y.round() as u32,
            size: Size { w: area.width.floor() as u32, h: area.height.floor() as u32 },
        }
    }
}

/// Where an instance sits, relative to the widget area.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placement {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Placement {
    fn overlaps(&self, other: &Placement) -> bool {
        self.x < other.x + other.w && other.x < self.x + self.w && self.y < other.y + other.h && other.y < self.y + self.h
    }
}

/// One widget in a layout: which package, and where.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlacedWidget {
    pub package: QualifiedId,
    pub placement: Placement,
}

/// A layout fits the area, lines up on the grid, keeps each widget's allowed sizes and never overlaps.
/// `size_of` gives an installed widget's sizes, and `None` for anything that isn't one.
pub fn check_widgets(widgets: &[PlacedWidget], area: Size, size_of: impl Fn(&QualifiedId) -> Option<WidgetSize>) -> Result<()> {
    ensure!(widgets.len() <= usize::from(u16::MAX), "a layout holds at most {} widgets", u16::MAX);
    for (index, widget) in widgets.iter().enumerate() {
        let Placement { x, y, w, h } = widget.placement;
        let name = format!("widget {index} ({})", widget.package);
        let Some(sizes) = size_of(&widget.package) else { bail!("{name}: no widget with that id is installed") };
        ensure!(x % GRID == 0 && y % GRID == 0, "{name}: x and y must be multiples of {GRID}");
        ensure!(sizes.resize.allows(sizes.size, Size { w, h }), "{name}: {w}x{h} is not a size this widget allows");
        ensure!(x + w <= area.w && y + h <= area.h, "{name}: reaches outside the {}x{} widget area", area.w, area.h);
        if let Some(other) = widgets[..index].iter().position(|o| o.placement.overlaps(&widget.placement)) {
            bail!("{name}: overlaps widget {other} ({})", widgets[other].package);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use glassdeck_package::Resize;

    const AREA: Size = Size { w: 1903, h: 256 };

    fn clock(id: &QualifiedId) -> Option<WidgetSize> {
        (id.as_str() == "widget.local.user.clock").then_some(WidgetSize {
            size: Size { w: 240, h: 120 },
            resize: Resize::Between { min: Size { w: 160, h: 80 }, max: Size { w: 480, h: 240 } },
        })
    }

    fn widget(x: u32, y: u32, w: u32, h: u32) -> PlacedWidget {
        PlacedWidget { package: QualifiedId::parse("widget.local.user.clock").unwrap(), placement: Placement { x, y, w, h } }
    }

    #[test]
    fn every_layout_has_the_same_widget_area() {
        assert_eq!(Layout::new(Variant::Ansi).widget_area, Layout::new(Variant::Iso).widget_area);
        assert_eq!(WidgetArea::of_panel(), WidgetArea { x: 8, y: 5, size: Size { w: 1903, h: 251 } });
    }

    #[test]
    fn accepts_widgets_side_by_side() {
        check_widgets(&[widget(8, 0, 240, 120), widget(248, 0, 240, 120)], AREA, clock).unwrap();
    }

    #[test]
    fn refuses_bad_placements_with_a_reason() {
        let nope = PlacedWidget { package: QualifiedId::parse("widget.local.user.nope").unwrap(), placement: Placement { x: 0, y: 0, w: 8, h: 8 } };
        let cases = [
            (vec![widget(4, 0, 240, 120)], "multiples of 8"),
            (vec![widget(0, 0, 100, 120)], "not a size"),
            (vec![widget(1800, 0, 240, 120)], "outside"),
            (vec![widget(0, 0, 240, 120), widget(232, 0, 240, 120)], "overlaps widget 0"),
            (vec![nope], "no widget"),
        ];
        for (widgets, reason) in cases {
            let error = format!("{:#}", check_widgets(&widgets, AREA, clock).unwrap_err());
            assert!(error.contains(reason), "{error}");
        }
    }
}
