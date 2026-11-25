mod decode;
mod compositor;
mod yuv;

pub use decode::{Decoder, Picture, init_decoder_thread};
pub use compositor::Compositor;

pub const DEFAULT_LISTEN: &str = "127.0.0.1:7878";
pub const DEFAULT_DECKS: &str = r"C:\ProgramData\Glassdeck\decks";
pub const DEFAULT_FONT: &str = r"C:\Windows\Fonts\segoeuib.ttf";
