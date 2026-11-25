use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Result, bail};
use glassdeck_package::Background;
use glassdeck_wallpaper_engine::{self as we, EncodeSettings, Item, Kind, Project};

use crate::background_source::{self, WebEncoding};
use crate::pipeline::{EncoderSettings, Source};
use crate::scene::SceneSource;

/// Converts a video or web wallpaper into a .deckbg: a video is stored as is if it meets the background
/// spec, re-encoded otherwise. `out` defaults to the backgrounds folder.
pub fn convert_item(item: &str, out: Option<PathBuf>, mbps: u32) -> Result<Background> {
    let item = we::find(item)?;
    let project = item.project()?;
    let out = match out {
        Some(out) => out,
        None => we::background_path(&we::default_backgrounds_dir()?, &item),
    };
    convert(&item, &project, &out, mbps)
}

/// What streaming a wallpaper plays: video and web on a loop from their .deckbg (built on first use),
/// scenes live from Wallpaper Engine.
pub fn source(item: &str, rebuild: bool, mbps: u32, fps: u32, duration: Option<Duration>) -> Result<Source> {
    let item = we::find(item)?;
    let project = item.project()?;
    println!("{} ({}, {})", project.title, item.id, project.kind);
    if project.kind == Kind::Scene {
        return Ok(Source::Frames(Box::new(SceneSource::new(&item, scene_settings(mbps, fps), duration)?)));
    }
    let path = we::background_path(&we::default_backgrounds_dir()?, &item);
    let background = if glassdeck_package::is_background(&path) && !rebuild { Background::open(&path)? } else { convert(&item, &project, &path, mbps)? };
    background_source::open(background, true, WebEncoding { fps, bitrate: mbps * 1_000_000, duration })
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

/// Prints every subscribed item with its type, and why it isn't supported if it isn't.
pub fn list() -> Result<()> {
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
