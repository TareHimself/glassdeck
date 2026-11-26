//! The Wallpaper Engine converter: turns video and web wallpapers already in this PC's Steam workshop
//! folder into backgrounds. Separate from Glassdeck, which only ever sees the `.deck` files.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::{Parser, Subcommand};
use glassdeck_convert::EncodeSettings;
use glassdeck_wallpaper_engine::{self as we, Kind};

#[derive(Parser)]
#[command(about = "Turn Wallpaper Engine wallpapers into Glassdeck backgrounds (.deck)")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List subscribed items with their type, and why any can't become a background
    List,
    /// Turn a video or web wallpaper into a background .deck (a video is kept as is if it meets the background spec)
    Import {
        /// Workshop ID, or a folder holding a project.json
        item: String,
        /// Package file (default: wallpaper-engine.<id>.deck in the current folder)
        #[arg(long)]
        out: Option<PathBuf>,
        /// Bitrate when re-encoding
        #[arg(long, default_value_t = 12)]
        mbps: u32,
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
        Command::List => list(),
        Command::Import { item, out, mbps } => {
            let item = we::find(&item)?;
            let project = item.project()?;
            let out = match out {
                Some(out) => out,
                None => we::package_path(std::path::Path::new("."), &item)?,
            };
            we::convert(&item, &project, &out, EncodeSettings { bitrate: mbps * 1_000_000 })?;
            println!("Wrote {}; show it with `glassdeck bg {}`", out.display(), out.display());
            Ok(())
        }
    }
}

fn list() -> Result<()> {
    let items = we::subscribed()?;
    for item in &items {
        match item.project() {
            Ok(project) => {
                let note = match (&project.kind, we::why_unsupported(&project.kind)) {
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
