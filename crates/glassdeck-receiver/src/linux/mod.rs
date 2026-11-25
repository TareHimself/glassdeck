mod compositor;
mod mpp;
mod scanout;

use anyhow::Result;

pub use compositor::Compositor;
pub use mpp::{Decoder, Picture};

pub const DEFAULT_LISTEN: &str = "0.0.0.0:7878";
pub const DEFAULT_DECKS: &str = "/var/lib/glassdeck/decks";
pub const DEFAULT_FONT: &str = "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf";

pub fn init_decoder_thread() -> Result<()> {
    Ok(())
}
