mod clock;
mod desktop;
mod gpu;
mod link;
mod monitors;
mod play;
mod record;
mod software;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use clap::{Parser, Subcommand};

use monitors::MonitorQuery;
use play::PlayOptions;
use record::RecordOptions;
use desktop::{EncoderKind, StreamOptions, VideoCodec};

const USB2_PRACTICAL_MBYTES_PER_SEC: u64 = 35;

#[derive(Parser)]
#[command(about = "Capture a monitor and encode it to H.264")]
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
    /// Send an H.264 or HEVC video file to a receiver, paced by its timestamps, without re-encoding
    Play {
        file: PathBuf,
        #[arg(long, default_value = "127.0.0.1:7878")]
        to: SocketAddr,
        /// Start over at the end, for wallpapers
        #[arg(long = "loop")]
        looping: bool,
        /// Simulated link speed in MB/s (default is realistic USB 2.0 bulk)
        #[arg(long, default_value_t = USB2_PRACTICAL_MBYTES_PER_SEC, conflicts_with = "unlimited_link")]
        link_mbytes: u64,
        /// Send as fast as localhost allows
        #[arg(long)]
        unlimited_link: bool,
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
        Command::Play { file, to, looping, link_mbytes, unlimited_link } => play::run(
            &file,
            PlayOptions {
                to,
                looping,
                link_bytes_per_sec: (!unlimited_link).then_some(link_mbytes * 1_000_000),
            },
        ),
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
            let link_bytes_per_sec = (!unlimited_link).then_some(link_mbytes * 1_000_000);
            println!("Streaming {} to {to}", found.info);
            desktop::run(
                found,
                StreamOptions {
                    to,
                    duration: seconds.map(Duration::from_secs),
                    encoder,
                    codec: codec.into(),
                    bitrate: mbps * 1_000_000,
                    fps,
                    keyframe_every,
                    vbv_frames,
                    link_bytes_per_sec,
                },
            )
        }
    }
}
