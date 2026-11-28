use std::path::Path;
use std::time::Duration;

use anyhow::{Result, bail};
use glassdeck_package::Package;
use glassdeck_convert::EncodeSettings;
use glassdeck_wallpaper_engine::{self as we, Item, Project};

use crate::background_source::{self, WebEncoding};
use crate::pipeline::Source;

/// What streaming a wallpaper plays: video and web on a loop from their .deck (built on first use).
pub fn source(item: &str, rebuild: bool, mbps: u32, fps: u32, duration: Option<Duration>) -> Result<Source> {
    let item = we::find(item)?;
    let project = item.project()?;
    println!("{} ({}, {})", project.title, item.id, project.kind);
    let path = we::package_path(&glassdeck_convert::cache_dir()?, &item)?;
    let background = if glassdeck_package::is_package(&path) && !rebuild { Package::open(&path)? } else { convert(&item, &project, &path, mbps)? };
    background_source::open(background, true, WebEncoding { fps, bitrate: mbps * 1_000_000, duration })
}

fn convert(item: &Item, project: &Project, out: &Path, mbps: u32) -> Result<Package> {
    if let Some(why) = we::why_unsupported(&project.kind) {
        bail!("{}: {why}", item.id);
    }
    we::convert(item, project, out, EncodeSettings { bitrate: mbps * 1_000_000 })
}
