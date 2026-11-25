//! The Glassdeck CLI. Anything that reaches the keyboard goes through glassdeckd, which owns the Link;
//! the rest (monitors, recording, converting wallpapers, packing modules) runs here.

mod client;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use glassdeck_capture::EncoderKind;
use glassdeck_capture::monitors::{self, MonitorQuery};
use glassdeck_capture::pipeline::VideoCodec;
use glassdeck_capture::record::{self, RecordOptions};
use glassdeck_capture::sources::{Play, SourceRequest};
use glassdeck_capture::wallpaper;
use serde_json::{Value, json};

#[derive(Parser)]
#[command(about = "Glassdeck: put monitors, web pages, videos and wallpapers on the keyboard")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// What glassdeckd is doing: the Link and the current source
    Status,
    /// Mirror a monitor on the keyboard, live
    Stream {
        /// Monitor EDID name (case-insensitive) or index from `list`
        #[arg(long)]
        monitor: String,
        /// Stop after this many seconds instead of running until stopped
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
    },
    /// Render a web page offscreen and play it as a wallpaper
    Web {
        url: String,
        #[arg(long, default_value_t = 1920)]
        width: u32,
        #[arg(long, default_value_t = 1080)]
        height: u32,
        /// Stop after this many seconds instead of running until stopped
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
    },
    /// Play an H.264 or HEVC file without re-encoding, or a .deckbg (video or web)
    Play {
        file: PathBuf,
        /// Start over at the end, for wallpapers
        #[arg(long = "loop")]
        looping: bool,
        /// Web backgrounds: frames per second to render
        #[arg(long, default_value_t = 60)]
        fps: u32,
        /// Web backgrounds: encoder bitrate
        #[arg(long, default_value_t = 12)]
        mbps: u32,
        /// Web backgrounds: stop after this many seconds
        #[arg(long)]
        seconds: Option<u64>,
    },
    /// Stop the current source; the keyboard goes back to its own background
    Stop,
    /// Choose the background the keyboard plays from its own folder whenever nothing is streaming
    SelectBackground {
        /// File name in the keyboard's background folder, such as we-2970107491.deckbg
        name: String,
    },
    /// Wallpaper Engine items already in this PC's Steam workshop folder
    Wallpaper {
        #[command(subcommand)]
        action: WallpaperAction,
    },
    /// Widget modules
    Module {
        #[command(subcommand)]
        action: ModuleAction,
    },
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
}

#[derive(Subcommand)]
enum WallpaperAction {
    /// List subscribed items with their type
    List,
    /// Convert a video or web wallpaper into a .deckbg (a video is stored as is if it meets the background spec, re-encoded otherwise)
    Background {
        /// Workshop ID, or a folder holding a project.json
        item: String,
        /// Background file (default: %LOCALAPPDATA%\Glassdeck\backgrounds\we-<id>.deckbg)
        #[arg(long)]
        out: Option<PathBuf>,
        /// Bitrate when re-encoding
        #[arg(long, default_value_t = 12)]
        mbps: u32,
    },
    /// Play a wallpaper: video and web on a loop from its .deckbg (built on first use), scenes live from Wallpaper Engine
    Play {
        /// Workshop ID, or a folder holding a project.json
        item: String,
        /// Build the .deckbg again even if one exists
        #[arg(long)]
        rebuild: bool,
        /// Bitrate when encoding (re-encoded videos and web pages)
        #[arg(long, default_value_t = 12)]
        mbps: u32,
        /// Web pages and scenes: frames per second
        #[arg(long, default_value_t = 60)]
        fps: u32,
        /// Web pages and scenes: stop after this many seconds
        #[arg(long)]
        seconds: Option<u64>,
    },
}

#[derive(Subcommand)]
enum ModuleAction {
    /// Pack a module folder (module.json, keyboard.luau, optional pc.luau and assets/) into a .deckmod
    Pack {
        folder: PathBuf,
        /// Defaults to <folder>.deckmod
        #[arg(long)]
        out: Option<PathBuf>,
    },
}

fn main() -> ExitCode {
    match run(Cli::parse().command) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(command: Command) -> Result<()> {
    match command {
        Command::Status => status(),
        Command::Stream { monitor, seconds, encoder, codec, mbps, fps, keyframe_every, vbv_frames } => {
            start(Play::Monitor { monitor, encoder, codec, mbps, fps, keyframe_every, vbv_frames }, seconds)
        }
        Command::Web { url, width, height, seconds, codec, mbps, fps, keyframe_every } => {
            start(Play::Web { url, width, height, codec, mbps, fps, keyframe_every }, seconds)
        }
        Command::Play { file, looping, fps, mbps, seconds } => {
            let path = std::path::absolute(&file).with_context(|| format!("finding {}", file.display()))?;
            let play = match glassdeck_package::is_background(&path) {
                true => Play::Background { path, looping, mbps, fps },
                false => Play::File { path, looping },
            };
            start(play, seconds)
        }
        Command::Stop => {
            client::call("DELETE", "/source", None)?;
            println!("Stopped; the keyboard plays its own background");
            Ok(())
        }
        Command::SelectBackground { name } => {
            client::call("PUT", "/background", Some(json!({ "name": name })))?;
            println!("The keyboard plays {name} whenever nothing is streaming");
            Ok(())
        }
        Command::Wallpaper { action } => match action {
            WallpaperAction::List => wallpaper::list(),
            WallpaperAction::Background { item, out, mbps } => {
                let background = wallpaper::convert_item(&item, out, mbps)?;
                println!("Wrote {}", background.path().display());
                Ok(())
            }
            WallpaperAction::Play { item, rebuild, mbps, fps, seconds } => start(Play::Wallpaper { item, rebuild, mbps, fps }, seconds),
        },
        Command::Module { action: ModuleAction::Pack { folder, out } } => {
            let out = out.unwrap_or_else(|| folder.with_extension(glassdeck_package::MODULE_EXTENSION));
            let package = glassdeck_package::pack_folder(&folder, &out)?;
            println!("Packed {} into {}", package.manifest().id, out.display());
            Ok(())
        }
        Command::List => {
            for info in monitors::enumerate()? {
                println!("{info}");
            }
            Ok(())
        }
        Command::Record { monitor, out, seconds, mbps, fps } => {
            let found = monitors::find(&monitor)?;
            println!("Recording {} for {seconds}s to {}", found.info, out.display());
            record::run(found, RecordOptions { out, duration: Duration::from_secs(seconds), bitrate: mbps * 1_000_000, fps })
        }
    }
}

fn start(play: Play, seconds: Option<u64>) -> Result<()> {
    let request = SourceRequest { play, seconds };
    let answer = client::call("PUT", "/source", Some(serde_json::to_value(&request)?))?;
    match (answer.get("playing"), answer.get("waiting"), answer.get("starting")) {
        (Some(what), _, _) => println!("Playing {}", text(what)),
        (_, Some(what), _) => println!("{} starts when the keyboard connects", text(what)),
        (_, _, Some(what)) => println!("{} is still starting; `glassdeck status` shows how it goes", text(what)),
        _ => println!("{answer}"),
    }
    Ok(())
}

fn status() -> Result<()> {
    let status = client::call("GET", "/status", None)?;
    let link = &status["link"];
    let connected = if link["connected"] == Value::Bool(true) { "connected" } else { "not connected" };
    println!("Keyboard {}: {connected}", text(&link["keyboard"]));
    let source = &status["source"];
    match source["playing"].as_str() {
        Some(playing) => println!("Source: {} ({})", playing, text(&source["state"])),
        None => println!("Source: none; the keyboard plays its own background"),
    }
    if let Some(ended) = source["last_ended"].as_array().and_then(|pair| pair.get(1)) {
        println!("Last source: {}", text(ended));
    }
    Ok(())
}

fn text(value: &Value) -> String {
    value.as_str().map_or_else(|| value.to_string(), str::to_owned)
}
