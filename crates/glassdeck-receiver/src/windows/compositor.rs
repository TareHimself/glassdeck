use anyhow::Result;
use fontdue::Font;
use glassdeck_proto::unix_micros;
use minifb::{Key, Window, WindowOptions};

use super::decode::Picture;
use crate::overlay::Overlay;
use crate::presentation::Outcome;

const UNTHROTTLED: usize = 0;

/// A desktop window standing in for the keyboard panel; key labels are blended on the CPU.
pub struct Compositor {
    font: Option<Font>,
    window: Option<Window>,
    overlay: Option<Overlay>,
    outcomes: Vec<Outcome>,
}

impl Compositor {
    pub fn open(font: Option<Font>) -> Result<Self> {
        Ok(Self { font, window: None, overlay: None, outcomes: Vec::new() })
    }

    /// Shows the picture at once: minifb has no page-flip events to wait for.
    pub fn submit(&mut self, mut picture: Picture, token: u64) -> Result<()> {
        if let Some(font) = &self.font {
            if !self.overlay.as_ref().is_some_and(|o| o.fits(picture.width, picture.height)) {
                self.overlay = Some(Overlay::build(font, picture.width, picture.height));
            }
            self.overlay.as_ref().expect("overlay built").composite_onto(&mut picture.pixels);
        }
        let window = match self.window.as_mut() {
            Some(window) => window,
            None => self.window.insert(open_window(picture.width, picture.height)?),
        };
        window.update_with_buffer(&picture.pixels, picture.width, picture.height)?;
        self.outcomes.push(Outcome::Presented { token, at_us: unix_micros() });
        Ok(())
    }

    pub fn poll(&mut self) -> Result<Vec<Outcome>> {
        Ok(std::mem::take(&mut self.outcomes))
    }

    pub fn idle(&mut self) {
        if let Some(window) = self.window.as_mut() {
            window.update();
        }
    }

    pub fn is_open(&self) -> bool {
        self.window.as_ref().is_none_or(|w| w.is_open() && !w.is_key_down(Key::Escape))
    }

    pub fn set_status(&mut self, status: &str) {
        if let Some(window) = self.window.as_mut() {
            window.set_title(&format!("glassdeck-receiver | {status}"));
        }
    }
}

fn open_window(width: usize, height: usize) -> Result<Window> {
    let mut window = Window::new("glassdeck-receiver", width, height, WindowOptions::default())?;
    window.set_target_fps(UNTHROTTLED);
    Ok(window)
}
