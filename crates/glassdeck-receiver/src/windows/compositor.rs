use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use glassdeck_proto::unix_micros;
use glassdeck_layout::{PANEL_HEIGHT, PANEL_WIDTH};
use minifb::{Key, ScaleMode, Window, WindowOptions};

use super::Doorbell;
use super::decode::Picture;
use crate::overlay::{Labels, Overlay};
use crate::presentation::Outcome;
use crate::widgets::Tile;

const UNTHROTTLED: usize = 0;
/// How often the idle preview window handles its events.
const WINDOW_FRAME: Duration = Duration::from_millis(16);
const PANEL: (usize, usize) = (PANEL_WIDTH as usize, PANEL_HEIGHT as usize);

/// A desktop window standing in for the keyboard panel, open from the start so widgets show with
/// no stream. Labels and widget tiles are blended on the CPU over the latest picture, or black.
pub struct Compositor {
    labels: Option<Labels>,
    window: Window,
    overlay: Option<Overlay>,
    background: Option<Picture>,
    outcomes: Vec<Outcome>,
    widgets: Vec<Tile>,
    doorbell: Arc<Doorbell>,
}

impl Compositor {
    pub fn open(labels: Option<Labels>, rotation: crate::PanelRotation) -> Result<Self> {
        if rotation != crate::PanelRotation::Upright {
            anyhow::bail!("the desktop window only shows the panel upright");
        }
        let options = WindowOptions { resize: true, scale_mode: ScaleMode::AspectRatioStretch, ..WindowOptions::default() };
        let mut window = Window::new("glassdeck-receiver", PANEL.0, PANEL.1, options)?;
        window.set_target_fps(UNTHROTTLED);
        let mut compositor = Self {
            labels,
            window,
            overlay: None,
            background: None,
            outcomes: Vec::new(),
            widgets: Vec::new(),
            doorbell: Arc::new(Doorbell::new()?),
        };
        compositor.present()?;
        Ok(compositor)
    }

    /// Shows the picture at once: minifb has no page-flip events to wait for.
    pub fn submit(&mut self, picture: Picture, token: u64) -> Result<()> {
        self.background = Some(picture);
        self.present()?;
        self.outcomes.push(Outcome::Presented { token, at_us: unix_micros() });
        Ok(())
    }

    /// Keeps the latest tile per spot and blends them over every picture, like the keyboard's key layer.
    pub fn show_tiles(&mut self, tiles: Vec<Tile>) -> Result<()> {
        for tile in tiles {
            self.widgets.retain(|t| !overlaps(t, &tile));
            self.widgets.push(tile);
        }
        self.present()
    }

    fn present(&mut self) -> Result<()> {
        let (mut frame, width, height) = match &self.background {
            Some(picture) => (picture.pixels.clone(), picture.width, picture.height),
            None => (vec![0; PANEL.0 * PANEL.1], PANEL.0, PANEL.1),
        };
        if let Some(labels) = &self.labels {
            if !self.overlay.as_ref().is_some_and(|o| o.fits(width, height)) {
                self.overlay = Some(Overlay::build(labels, width, height));
            }
            self.overlay.as_ref().expect("overlay built").composite_onto(&mut frame);
        }
        for tile in &self.widgets {
            composite_tile(tile, &mut frame, width, height);
        }
        self.window.update_with_buffer(&frame, width, height)?;
        Ok(())
    }

    pub fn poll(&mut self) -> Result<Vec<Outcome>> {
        Ok(std::mem::take(&mut self.outcomes))
    }

    pub fn doorbell(&self) -> Arc<Doorbell> {
        Arc::clone(&self.doorbell)
    }

    /// Sleeps until the doorbell rings or `timeout` passes, at most a frame so the window stays responsive.
    pub fn wait(&mut self, timeout: Duration) -> Result<()> {
        self.doorbell.wait(timeout.min(WINDOW_FRAME));
        self.window.update();
        Ok(())
    }

    pub fn registrar(&self) -> super::Registrar {
        super::Registrar
    }

    pub fn is_open(&self) -> bool {
        self.window.is_open() && !self.window.is_key_down(Key::Escape)
    }

    pub fn set_status(&mut self, status: &str) {
        self.window.set_title(&format!("glassdeck-receiver | {status}"));
    }
}

fn overlaps(a: &Tile, b: &Tile) -> bool {
    a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h
}

/// Premultiplied source over an opaque frame; tiles past the frame's edge are cut off.
fn composite_tile(tile: &Tile, frame: &mut [u32], width: usize, height: usize) {
    for (row, pixels) in tile.pixels.chunks_exact(tile.w as usize).enumerate() {
        let y = tile.y as usize + row;
        if y >= height {
            break;
        }
        for (col, &src) in pixels.iter().enumerate() {
            let x = tile.x as usize + col;
            let alpha = src >> 24;
            if x >= width || alpha == 0 {
                continue;
            }
            let dst = &mut frame[y * width + x];
            let mix = |shift: u32| {
                let s = (src >> shift) & 0xFF;
                let d = (*dst >> shift) & 0xFF;
                (s + (d * (255 - alpha) + 127) / 255).min(255) << shift
            };
            *dst = mix(16) | mix(8) | mix(0);
        }
    }
}
