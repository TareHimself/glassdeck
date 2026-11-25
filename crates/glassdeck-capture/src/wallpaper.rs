use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Result, bail};
use clap::Subcommand;
use glassdeck_deck::Deck;
use glassdeck_wallpaper_engine::{self as we, EncodeSettings, Item, Kind, Project};

use crate::pipeline::{self, EncoderSettings, Source};
use crate::play::FileSource;
use crate::web::WebSource;
use crate::{USB2_PRACTICAL_MBYTES_PER_SEC, link_options};

#[derive(Subcommand)]
pub enum WallpaperAction {
    /// List subscribed items with their type
    List,
    /// Put a video wallpaper into a .deck bundle: copied if it meets the deck spec, re-encoded otherwise
    Deck {
        /// Workshop ID, or a folder holding a project.json
        item: String,
        /// Bundle folder (default: %LOCALAPPDATA%\Glassdeck\decks\we-<id>.deck)
        #[arg(long)]
        out: Option<PathBuf>,
        /// Bitrate when re-encoding
        #[arg(long, default_value_t = 12)]
        mbps: u32,
    },
    /// Stream a wallpaper on a loop: a video through its .deck (built on first use), a web page through CEF
    Play {
        /// Workshop ID, or a folder holding a project.json
        item: String,
        #[arg(long, default_value = "127.0.0.1:7878")]
        to: SocketAddr,
        /// Build the video's .deck again even if one exists
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

const WEB_KEYFRAME_SECONDS: u32 = 2;

pub fn run(action: WallpaperAction) -> Result<()> {
    match action {
        WallpaperAction::List => list(),
        WallpaperAction::Deck { item, out, mbps } => {
            let item = we::find(&item)?;
            let project = item.project()?;
            let out = match out {
                Some(out) => out,
                None => we::deck_dir(&we::default_decks_dir()?, &item),
            };
            let deck = build_deck(&item, &project, &out, mbps)?;
            println!("Wrote {}", deck.dir.display());
            Ok(())
        }
        WallpaperAction::Play { item, to, rebuild, mbps, fps, seconds, link_mbytes, unlimited_link } => {
            let item = we::find(&item)?;
            let project = item.project()?;
            println!("{} ({}, {})", project.title, item.id, project.kind);
            let link = link_options(to, link_mbytes, unlimited_link);
            match &project.kind {
                Kind::Video { .. } => {
                    let out = we::deck_dir(&we::default_decks_dir()?, &item);
                    let deck = if glassdeck_deck::is_deck(&out) && !rebuild { Deck::open(&out)? } else { build_deck(&item, &project, &out, mbps)? };
                    println!("Streaming {}", deck.dir.display());
                    let source = FileSource::open(&deck.video(), true)?;
                    pipeline::run(Source::Encoded(Box::new(source)), link)
                }
                Kind::Web { file } => {
                    let page = we::web_page(&item, &project, file)?;
                    let settings = EncoderSettings {
                        width: glassdeck_deck::PANEL.0 as u32,
                        height: glassdeck_deck::PANEL.1 as u32,
                        codec: glassdeck_proto::Codec::Hevc,
                        fps,
                        bitrate: mbps * 1_000_000,
                        keyframe_every: fps * WEB_KEYFRAME_SECONDS,
                        vbv_frames: None,
                    };
                    let source = WebSource::new(&page.url, settings, seconds.map(Duration::from_secs))?.with_init_script(&page.init_script)?;
                    pipeline::run(Source::Frames(Box::new(source)), link)
                }
                other => bail!(why_unsupported(other).unwrap_or_else(|| format!("{other} wallpapers are not supported"))),
            }
        }
    }
}

fn build_deck(item: &Item, project: &Project, out: &std::path::Path, mbps: u32) -> Result<Deck> {
    let Kind::Video { file } = &project.kind else {
        bail!("{} is a {} wallpaper; only video wallpapers go into a .deck", item.id, project.kind);
    };
    we::convert_video(item, project, file, out, EncodeSettings { bitrate: mbps * 1_000_000 })
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
