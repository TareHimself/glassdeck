mod compositor;
mod mpp;
mod renderer;
mod scanout;
mod scene;
mod web;

use anyhow::Result;

pub use compositor::{Compositor, Registrar};
pub use mpp::Decoder;
pub use scene::{FallbackScene, SceneBackground};
pub use web::WebBackground;

pub const DEFAULT_LISTEN: &str = "0.0.0.0:7878";
pub const DEFAULT_BACKGROUNDS: &str = "/var/lib/glassdeck/backgrounds";
pub const DEFAULT_MODULES: &str = "/var/lib/glassdeck/modules";
pub const DEFAULT_PROFILE: &str = "/var/lib/glassdeck/profile.json";

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
