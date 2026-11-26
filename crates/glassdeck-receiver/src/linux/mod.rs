mod compositor;
mod doorbell;
mod mpp;
mod renderer;
mod scanout;
mod canvas;
mod web;

use anyhow::Result;

pub use compositor::{Compositor, Registrar};
pub use doorbell::Doorbell;
pub use mpp::Decoder;
pub use canvas::{FallbackCanvas, CanvasBackground};
pub use web::WebBackground;

pub const DEFAULT_LISTEN: &str = "0.0.0.0:7878";
pub const DEFAULT_PACKAGES: &str = "/var/lib/glassdeck/packages";
pub const DEFAULT_STATE: &str = "/var/lib/glassdeck/state";

/// What the compositor shows: our own decoder's output, or a buffer from a renderer on the keyboard.
pub enum Picture {
    Decoded(mpp::Picture),
    Shared(renderer::SharedPicture),
}

pub fn decoded(picture: mpp::Picture) -> Picture {
    Picture::Decoded(picture)
}

pub fn init_decoder_thread() -> Result<()> {
    Ok(())
}
