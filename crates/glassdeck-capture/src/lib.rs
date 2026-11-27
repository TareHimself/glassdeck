//! Glassdeck sources: mirror a monitor, render a web page, play a file or a background, or stream a
//! Wallpaper Engine scene, encoded for the Link. glassdeckd runs them; the CLI uses the local tools.

mod audio_mute;
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
mod scene;
mod software;
pub mod sources;
pub mod wallpaper;
mod web;

pub use desktop::{EncoderKind, capture_still};
pub use web::helper_process_exit_code;
