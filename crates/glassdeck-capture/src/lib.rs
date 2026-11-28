//! Glassdeck sources: mirror a monitor, render a web page, play a file or a background,
//! encoded for the Link. glassdeckd runs them; the CLI uses the local tools.

pub mod audio_mute;
mod background_source;
mod background_stream;
pub mod clock;
mod desktop;
mod gpu;
pub mod link;
pub mod monitors;
pub mod pipeline;
mod play;
pub mod record;
mod software;
pub mod sources;
pub mod wallpaper;
mod web;

pub use desktop::{EncoderKind, capture_live, capture_still};
pub use web::helper_process_exit_code;
