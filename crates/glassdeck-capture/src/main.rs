mod clock;
mod deck_source;
mod deck_stream;
mod desktop;
mod gpu;
mod link;
mod monitors;
mod pipeline;
mod play;
mod record;
mod software;
mod wallpaper;
mod web;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use clap::{Parser, Subcommand};

use desktop::{DesktopSource, EncoderKind};
use monitors::MonitorQuery;
use pipeline::{EncoderSettings, LinkOptions, Source, VideoCodec};
use deck_source::WebEncoding;
use play::{FileSource, Media};
use record::RecordOptions;
use wallpaper::WallpaperAction;
use web::WebSource;

const USB2_PRACTICAL_MBYTES_PER_SEC: u64 = 35;

#[derive(Parser)]
#[command(about = "Glassdeck sources: capture a monitor, render a web page or play a file, and send it to a receiver")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List monitors with their index and EDID name
    List,
    /// Record a monitor to an MP4 file
    Record {
        /// Monitor EDID name (case-insensitive) or index from `list`
        #[arg(long)]
        monitor: MonitorQuery,
        #[arg(long, default_value = "capture.mp4")]
        out: PathBuf,
        #[arg(long, default_value_t = 10)]
        seconds: u64,
        #[arg(long, default_value_t = 15)]
        mbps: u32,
        #[arg(long, default_value_t = 60)]
        fps: u32,
    },
    /// Send an H.264 or HEVC video file without re-encoding, or play a .deck (video or web)
    Play {
        /// A video file, or a .deck
        file: PathBuf,
        #[arg(long, default_value = "127.0.0.1:7878")]
        to: SocketAddr,
        /// Start over at the end, for wallpapers
        #[arg(long = "loop")]
        looping: bool,
        /// Web decks: frames per second to render
        #[arg(long, default_value_t = 60)]
        fps: u32,
        /// Web decks: encoder bitrate
        #[arg(long, default_value_t = 12)]
        mbps: u32,
        /// Web decks: stop after this many seconds
        #[arg(long)]
        seconds: Option<u64>,
        /// Simulated link speed in MB/s (default is realistic USB 2.0 bulk)
        #[arg(long, default_value_t = USB2_PRACTICAL_MBYTES_PER_SEC, conflicts_with = "unlimited_link")]
        link_mbytes: u64,
        /// Send as fast as localhost allows
        #[arg(long)]
        unlimited_link: bool,
    },
    /// Render a web page offscreen with CEF and stream it as a paced wallpaper
    Web {
        url: String,
        #[arg(long, default_value = "127.0.0.1:7878")]
        to: SocketAddr,
        #[arg(long, default_value_t = 1920)]
        width: u32,
        #[arg(long, default_value_t = 1080)]
        height: u32,
        /// Stop after this many seconds instead of running until closed
        #[arg(long)]
        seconds: Option<u64>,
        #[arg(long, value_enum, default_value_t = VideoCodec::Hevc)]
        codec: VideoCodec,
        #[arg(long, default_value_t = 15)]
        mbps: u32,
        #[arg(long, default_value_t = 60)]
        fps: u32,
        #[arg(long, default_value_t = 120)]
        keyframe_every: u32,
        /// Simulated link speed in MB/s (default is realistic USB 2.0 bulk)
        #[arg(long, default_value_t = USB2_PRACTICAL_MBYTES_PER_SEC, conflicts_with = "unlimited_link")]
        link_mbytes: u64,
        /// Send as fast as localhost allows
        #[arg(long)]
        unlimited_link: bool,
    },
    /// Wallpaper Engine items already in this PC's Steam workshop folder
    Wallpaper {
        #[command(subcommand)]
        action: WallpaperAction,
    },
    /// Stream a monitor live to a receiver
    Stream {
        /// Monitor EDID name (case-insensitive) or index from `list`
        #[arg(long)]
        monitor: MonitorQuery,
        #[arg(long, default_value = "127.0.0.1:7878")]
        to: SocketAddr,
        /// Stop after this many seconds instead of running until closed
        #[arg(long)]
        seconds: Option<u64>,
        #[arg(long, value_enum, default_value_t = EncoderKind::Gpu)]
        encoder: EncoderKind,
        #[arg(long, value_enum, default_value_t = VideoCodec::Hevc)]
        codec: VideoCodec,
        #[arg(long, default_value_t = 15)]
        mbps: u32,
        #[arg(long, default_value_t = 60)]
        fps: u32,
        #[arg(long, default_value_t = 120)]
        keyframe_every: u32,
        /// Cap the encoder's rate-control buffer to N average frames, shrinking keyframes (GPU only)
        #[arg(long)]
        vbv_frames: Option<u32>,
        /// Simulated link speed in MB/s (default is realistic USB 2.0 bulk)
        #[arg(long, default_value_t = USB2_PRACTICAL_MBYTES_PER_SEC, conflicts_with = "unlimited_link")]
        link_mbytes: u64,
        /// Send as fast as localhost allows
        #[arg(long)]
        unlimited_link: bool,
    },
}

fn main() -> Result<()> {
    if let Some(code) = web::helper_process_exit_code() {
        std::process::exit(code);
    }
    pipeline::stop_on_ctrl_c()?;
    match Cli::parse().command {
        Command::List => {
            for info in monitors::enumerate()? {
                println!("{info}");
            }
            Ok(())
        }
        Command::Record { monitor, out, seconds, mbps, fps } => {
            let found = monitors::find(&monitor)?;
            println!("Recording {} for {seconds}s to {}", found.info, out.display());
            record::run(
                found,
                RecordOptions {
                    out,
                    duration: Duration::from_secs(seconds),
                    bitrate: mbps * 1_000_000,
                    fps,
                },
            )
        }
        Command::Web { url, to, width, height, seconds, codec, mbps, fps, keyframe_every, link_mbytes, unlimited_link } => {
            let settings = EncoderSettings {
                width,
                height,
                codec: codec.into(),
                fps,
                bitrate: mbps * 1_000_000,
                keyframe_every,
                vbv_frames: None,
            };
            let source = WebSource::new(&url, settings, seconds.map(Duration::from_secs))?;
            pipeline::run(Source::Frames(Box::new(source)), link_options(to, link_mbytes, unlimited_link))
        }
        Command::Play { file, to, looping, fps, mbps, seconds, link_mbytes, unlimited_link } => {
            let source = if glassdeck_deck::is_deck(&file) {
                let web = WebEncoding { fps, bitrate: mbps * 1_000_000, duration: seconds.map(Duration::from_secs) };
                deck_source::open(glassdeck_deck::Deck::open(&file)?, looping, web)?
            } else {
                Source::Encoded(Box::new(FileSource::open(Media::File(file), looping)?))
            };
            pipeline::run(source, link_options(to, link_mbytes, unlimited_link))
        }
        Command::Wallpaper { action } => wallpaper::run(action),
        Command::Stream {
            monitor,
            to,
            seconds,
            encoder,
            codec,
            mbps,
            fps,
            keyframe_every,
            vbv_frames,
            link_mbytes,
            unlimited_link,
        } => {
            let found = monitors::find(&monitor)?;
            println!("Streaming {} to {to}", found.info);
            let settings = EncoderSettings {
                width: found.info.width,
                height: found.info.height,
                codec: codec.into(),
                fps,
                bitrate: mbps * 1_000_000,
                keyframe_every,
                vbv_frames,
            };
            let source = DesktopSource::new(found, encoder, settings, seconds.map(Duration::from_secs))?;
            pipeline::run(Source::Frames(Box::new(source)), link_options(to, link_mbytes, unlimited_link))
        }
    }
}

fn link_options(to: SocketAddr, link_mbytes: u64, unlimited_link: bool) -> LinkOptions {
    LinkOptions { to, link_bytes_per_sec: (!unlimited_link).then_some(link_mbytes * 1_000_000) }
}
