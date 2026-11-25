use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Result, bail};
use clap::Subcommand;
use glassdeck_deck::Deck;
use glassdeck_wallpaper_engine::{self as we, EncodeSettings, Item, Kind, Project};

use crate::deck_source::{self, WebEncoding};
use crate::pipeline;
use crate::{USB2_PRACTICAL_MBYTES_PER_SEC, link_options};

#[derive(Subcommand)]
pub enum WallpaperAction {
    /// List subscribed items with their type
    List,
    /// Convert a video or web wallpaper into a .deck (a video is stored as is if it meets the deck spec, re-encoded otherwise)
    Deck {
        /// Workshop ID, or a folder holding a project.json
        item: String,
        /// Deck file (default: %LOCALAPPDATA%\Glassdeck\decks\we-<id>.deck)
        #[arg(long)]
        out: Option<PathBuf>,
        /// Bitrate when re-encoding
        #[arg(long, default_value_t = 12)]
        mbps: u32,
    },
    /// Stream a wallpaper on a loop from its .deck, built on first use
    Play {
        /// Workshop ID, or a folder holding a project.json
        item: String,
        #[arg(long, default_value = "127.0.0.1:7878")]
        to: SocketAddr,
        /// Build the .deck again even if one exists
        #[arg(long)]
        rebuild: bool,
        /// Bitrate when encoding (re-encoded videos and web pages)
        #[arg(long, default_value_t = 12)]
        mbps: u32,
        /// Web pages: frames per second to render
        #[arg(long, default_value_t = 60)]
        fps: u32,
        /// Web pages: stop after this many seconds
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
        WallpaperAction::Deck { item, out, mbps } => {
            let item = we::find(&item)?;
            let project = item.project()?;
            let out = match out {
                Some(out) => out,
                None => we::deck_path(&we::default_decks_dir()?, &item),
            };
            let deck = convert(&item, &project, &out, mbps)?;
            println!("Wrote {}", deck.path().display());
            Ok(())
        }
        WallpaperAction::Play { item, to, rebuild, mbps, fps, seconds, link_mbytes, unlimited_link } => {
            let item = we::find(&item)?;
            let project = item.project()?;
            println!("{} ({}, {})", project.title, item.id, project.kind);
            let path = we::deck_path(&we::default_decks_dir()?, &item);
            let deck = if glassdeck_deck::is_deck(&path) && !rebuild { Deck::open(&path)? } else { convert(&item, &project, &path, mbps)? };
            let web = WebEncoding { fps, bitrate: mbps * 1_000_000, duration: seconds.map(Duration::from_secs) };
            pipeline::run(deck_source::open(deck, true, web)?, link_options(to, link_mbytes, unlimited_link))
        }
    }
}

fn convert(item: &Item, project: &Project, out: &Path, mbps: u32) -> Result<Deck> {
    if let Some(why) = why_unsupported(&project.kind) {
        bail!("{}: {why}", item.id);
    }
    we::convert(item, project, out, EncodeSettings { bitrate: mbps * 1_000_000 })
}

fn why_unsupported(kind: &Kind) -> Option<String> {
    Some(match kind {
        Kind::Video { .. } | Kind::Web { .. } => return None,
        Kind::Scene => "scene wallpapers are not supported yet".into(),
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
