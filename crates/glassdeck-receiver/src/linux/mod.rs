mod compositor;
mod doorbell;
mod mpp;
mod renderer;
mod rga;
mod scanout;
mod canvas;
mod usb_link;
mod web;

use std::sync::OnceLock;

use anyhow::{Context, Result};

pub use compositor::{Compositor, Registrar};
pub use doorbell::Doorbell;
pub use mpp::Decoder;
pub use canvas::{CanvasBackground, FallbackCanvas, ImageBackground};
pub use web::WebBackground;
pub use usb_link::start_usb_link;

pub const DEFAULT_LISTEN: &str = "0.0.0.0:7878";
pub const DEFAULT_PACKAGES: &str = "/var/lib/glassdeck/packages";
pub const DEFAULT_STATE: &str = "/var/lib/glassdeck/state";

/// What the compositor shows: our own decoder's output, or a buffer from a renderer on the keyboard.
pub enum Picture {
    Decoded(mpp::Picture),
    Shared(renderer::SharedPicture),
}

static TURNER: OnceLock<rga::Turner> = OnceLock::new();

/// For a turned panel: the video plane can't turn, so every decoded frame is turned as it arrives.
pub fn turn_decoded_video() -> Result<()> {
    let turner = rga::Turner::open().context("setting up the RGA to turn video")?;
    TURNER.set(turner).map_err(|_| anyhow::anyhow!("decoded video is already being turned"))
}

pub fn decoded(picture: mpp::Picture) -> Result<Picture> {
    let picture = match TURNER.get() {
        Some(turner) => picture.turned(turner)?,
        None => picture,
    };
    Ok(Picture::Decoded(picture))
}

pub fn init_decoder_thread() -> Result<()> {
    Ok(())
}
