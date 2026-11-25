use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Result, bail};
use clap::Subcommand;
use glassdeck_package::Background;
use glassdeck_wallpaper_engine::{self as we, EncodeSettings, Item, Kind, Project};

use crate::background_source::{self, WebEncoding};
use crate::pipeline::{self, EncoderSettings, Source};
use crate::scene::SceneSource;
use crate::{USB2_PRACTICAL_MBYTES_PER_SEC, link_options};

#[derive(Subcommand)]
pub enum WallpaperAction {
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
    /// Stream a wallpaper: video and web on a loop from its .deckbg (built on first use), scenes live from Wallpaper Engine
    Play {
        /// Workshop ID, or a folder holding a project.json
        item: String,
        #[arg(long, default_value = "127.0.0.1:7878")]
        to: SocketAddr,
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
        /// Simulated link speed in MB/s (default is realistic USB 2.0 bulk)
        #[arg(long, default_value_t = USB2_PRACTICAL_MBYTES_PER_SEC, conflicts_with = "unlimited_link")]
        link_mbytes: u64,
        /// Send as fast as localhost allows
        #[arg(long)]
        unlimited_link: bool,
    },
}

pub fn run(action: WallpaperAction) -> Result<()> {
    match action {
        WallpaperAction::List => list(),
        WallpaperAction::Background { item, out, mbps } => {
            let item = we::find(&item)?;
            let project = item.project()?;
            let out = match out {
                Some(out) => out,
                None => we::background_path(&we::default_backgrounds_dir()?, &item),
            };
            let background = convert(&item, &project, &out, mbps)?;
            println!("Wrote {}", background.path().display());
            Ok(())
        }
        WallpaperAction::Play { item, to, rebuild, mbps, fps, seconds, link_mbytes, unlimited_link } => {
            let item = we::find(&item)?;
            let project = item.project()?;
            println!("{} ({}, {})", project.title, item.id, project.kind);
            let link = link_options(to, link_mbytes, unlimited_link);
            if project.kind == Kind::Scene {
                let source = SceneSource::new(&item, scene_settings(mbps, fps), seconds.map(Duration::from_secs))?;
                return pipeline::run(Source::Frames(Box::new(source)), link);
            }
            let path = we::background_path(&we::default_backgrounds_dir()?, &item);
            let background = if glassdeck_package::is_background(&path) && !rebuild { Background::open(&path)? } else { convert(&item, &project, &path, mbps)? };
            let web = WebEncoding { fps, bitrate: mbps * 1_000_000, duration: seconds.map(Duration::from_secs) };
            pipeline::run(background_source::open(background, true, web)?, link)
        }
    }
}

/// Scenes stream at the panel's size, so Wallpaper Engine renders exactly what the keyboard shows.
fn scene_settings(mbps: u32, fps: u32) -> EncoderSettings {
    let (width, height) = glassdeck_package::PANEL;
    EncoderSettings {
        width: width.into(),
        height: height.into(),
        codec: glassdeck_proto::Codec::Hevc,
        fps,
        bitrate: mbps * 1_000_000,
        keyframe_every: fps * 2,
        vbv_frames: None,
    }
}

fn convert(item: &Item, project: &Project, out: &Path, mbps: u32) -> Result<Background> {
    if let Some(why) = why_unsupported(&project.kind) {
        bail!("{}: {why}", item.id);
    }
    we::convert(item, project, out, EncodeSettings { bitrate: mbps * 1_000_000 })
}

fn why_unsupported(kind: &Kind) -> Option<String> {
    Some(match kind {
        Kind::Video { .. } | Kind::Web { .. } => return None,
        Kind::Scene => "scenes have no background yet; `wallpaper play` streams them live from Wallpaper Engine".into(),
        Kind::Application => "application wallpapers run a program and are not supported".into(),
        Kind::Preset { base_id } => format!("a preset for workshop item {base_id}; presets are not supported yet"),
        Kind::Unknown { declared: Some(t) } => format!("unknown wallpaper type {t:?}"),
        Kind::Unknown { declared: None } => "the project.json names no wallpaper type".into(),
    })
}

fn list() -> Result<()> {
    let items = we::subscribed()?;
    for item in &items {
        match item.project() {
            Ok(project) => {
                let note = match (&project.kind, why_unsupported(&project.kind)) {
                    (Kind::Video { file } | Kind::Web { file }, _) => file.clone(),
                    (_, why) => format!("unsupported: {}", why.unwrap_or_default()),
                };
                println!("{:>10}  {:<11}  {}  [{note}]", item.id, project.kind.name(), project.title);
            }
            Err(e) => println!("{:>10}  {:<11}  {e:#}", item.id, "unreadable"),
        }
    }
    println!("{} items", items.len());
    Ok(())
}
