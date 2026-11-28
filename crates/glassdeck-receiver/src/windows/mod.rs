mod compositor;
mod decode;
mod doorbell;
mod canvas;
mod web;
mod yuv;

pub use compositor::Compositor;
pub use doorbell::Doorbell;
pub use decode::{Decoder, Picture, init_decoder_thread};
pub use canvas::{CanvasBackground, FallbackCanvas, ImageBackground};
pub use web::{Registrar, WebBackground};

pub const DEFAULT_LISTEN: &str = "127.0.0.1:7878";
pub const DEFAULT_PACKAGES: &str = r"C:\ProgramData\Glassdeck\packages";
pub const DEFAULT_STATE: &str = r"C:\ProgramData\Glassdeck\state";

/// The USB link needs FunctionFS, so it only runs on the keyboard.
pub fn start_usb_link(_function: &std::path::Path, _listener: std::net::SocketAddr) -> anyhow::Result<()> {
    anyhow::bail!("--usb-function only works on the keyboard")
}

pub fn decoded(picture: Picture) -> Picture {
    picture
}
