use anyhow::Result;
use fontdue::Font;
use minifb::{Key, Window, WindowOptions};

use super::decode::Picture;
use crate::overlay::Overlay;

const UNTHROTTLED: usize = 0;

/// A desktop window standing in for the keyboard panel; key labels are blended on the CPU.
pub struct Display {
    font: Option<Font>,
    window: Option<Window>,
    overlay: Option<Overlay>,
}

impl Display {
    pub fn open(font: Option<Font>) -> Result<Self> {
        Ok(Self { font, window: None, overlay: None })
    }

    pub fn show(&mut self, mut picture: Picture) -> Result<()> {
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
        Ok(())
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
