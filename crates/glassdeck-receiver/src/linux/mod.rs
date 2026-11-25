mod compositor;
mod mpp;
mod scanout;
mod web;

use anyhow::Result;

pub use compositor::{Compositor, Registrar};
pub use mpp::Decoder;
pub use web::WebDeck;

pub const DEFAULT_LISTEN: &str = "0.0.0.0:7878";
pub const DEFAULT_DECKS: &str = "/var/lib/glassdeck/decks";
pub const DEFAULT_MODULES: &str = "/var/lib/glassdeck/modules";
pub const DEFAULT_PROFILE: &str = "/var/lib/glassdeck/profile.json";
pub const DEFAULT_FONT: &str = "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf";

/// What the compositor shows: our own decoder's output, or a buffer from a renderer on the keyboard.
pub enum Picture {
    Decoded(mpp::Picture),
    Shared(web::SharedPicture),
}

pub fn decoded(picture: mpp::Picture) -> Picture {
    Picture::Decoded(picture)
}

pub fn init_decoder_thread() -> Result<()> {
    Ok(())
}
