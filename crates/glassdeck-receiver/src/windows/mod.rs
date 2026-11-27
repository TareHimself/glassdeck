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

pub fn decoded(picture: Picture) -> Picture {
    picture
}
