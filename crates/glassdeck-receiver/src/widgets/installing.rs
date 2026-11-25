//! What the widget area shows while a large file is being installed: the file's name and a progress
//! bar, over the keyboard's own background.

use anyhow::{Result, bail};
use fontdue::Font;

use super::draw::{self, Align, Area, Color, Command, ResourceCache};

const PANEL: Color = Color { r: 0, g: 0, b: 0, a: 150 };
const TRACK: Color = Color { r: 255, g: 255, b: 255, a: 60 };
const TEXT_SIZE: f32 = 26.0;
const BAR_HEIGHT: f32 = 10.0;
/// Between the text and the bar, which sit either side of the area's middle.
const GAP: f32 = 16.0;

/// The upload being shown, and the whole percent last drawn, so the panel redraws once per percent.
pub struct Installing {
    pub what: String,
    pub fraction: f32,
    drawn_percent: Option<u32>,
}

impl Installing {
    pub fn new(what: String, fraction: f32) -> Self {
        Self { what, fraction, drawn_percent: None }
    }

    /// The panel's pixels, if the shown percent changed since the last draw.
    pub fn draw(&mut self, size: (u32, u32), font: &Font) -> Result<Option<Vec<u32>>> {
        let percent = (self.fraction.clamp(0.0, 1.0) * 100.0) as u32;
        if self.drawn_percent == Some(percent) {
            return Ok(None);
        }
        self.drawn_percent = Some(percent);
        let (w, h) = (size.0 as f32, size.1 as f32);
        let bar = Area { x: w * 0.25, y: h / 2.0 + GAP / 2.0, w: w * 0.5, h: BAR_HEIGHT };
        let commands = [
            Command::Rect { area: Area { x: 0.0, y: 0.0, w, h }, fill: Some(PANEL), stroke: None, radius: 16.0 },
            Command::Text {
                text: format!("Installing {}  {percent}%", self.what),
                x: 0.0,
                y: h / 2.0 - GAP / 2.0 - TEXT_SIZE,
                size: TEXT_SIZE,
                color: Color::WHITE,
                align: Align::Center,
                width: Some(w),
                font: None,
            },
            Command::Rect { area: bar, fill: Some(TRACK), stroke: None, radius: BAR_HEIGHT / 2.0 },
            Command::Bar { fraction: self.fraction, area: bar, color: Color::WHITE },
        ];
        let tile = draw::render(&commands, size, font, &mut ResourceCache::new(), &|name| bail!("the install panel has no asset {name}"))?;
        Ok(Some(draw::to_argb(&tile)))
    }
}
