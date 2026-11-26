mod compositor;
mod decode;
mod canvas;
mod web;
mod yuv;

pub use compositor::Compositor;
pub use decode::{Decoder, Picture, init_decoder_thread};
pub use canvas::{FallbackCanvas, CanvasBackground};
pub use web::{Registrar, WebBackground};

pub const DEFAULT_LISTEN: &str = "127.0.0.1:7878";
pub const DEFAULT_BACKGROUNDS: &str = r"C:\ProgramData\Glassdeck\backgrounds";
pub const DEFAULT_MODULES: &str = r"C:\ProgramData\Glassdeck\modules";
pub const DEFAULT_PROFILE: &str = r"C:\ProgramData\Glassdeck\profile.json";

pub fn decoded(picture: Picture) -> Picture {
    picture
}
