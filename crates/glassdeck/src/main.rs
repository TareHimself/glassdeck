//! The Glassdeck CLI. Anything that reaches the keyboard goes through glassdeckd, which owns the Link;
//! the rest (monitors, recording, converting, packing modules) runs here.

mod client;
mod flash_image;
mod keyboard;
mod picoboot;
mod scanner;
mod theme;
mod usb_command;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use glassdeck_capture::monitors::{self, MonitorQuery};
use glassdeck_capture::record::{self, RecordOptions};
use glassdeck_convert::{Details, EncodeSettings};
use glassdeck_api::{Status, Streams};
use glassdeck_package::{Origin, PackageId};

#[derive(Parser)]
#[command(about = "Glassdeck: choose what the keyboard shows, and put files on it")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// The Link, the stream and the active theme's background
    Status,
    /// Streams glassdeckd can show, such as monitor-q27g4zd
    Streams,
    /// Show or set the active theme's background: gradient (built in), a package id, a .deck, or stream:<name>
    #[command(alias = "bg")]
    Background {
        /// gradient, a package id, a .deck file (uploaded if it's a file here), or stream:<name>
        choice: Option<String>,
    },
    /// Show the active theme as JSON, or push a theme file, with its packages from a library folder
    Theme {
        /// A theme .json to make active
        file: Option<PathBuf>,
        /// Where its packages are, as <id>.deck (defaults to the current folder)
        #[arg(long)]
        library: Option<PathBuf>,
    },
    /// Show the background's (or a widget's) options, or set one: glassdeck options look.tint "#ff8000"
    Options {
        /// A widget's place in the theme, instead of the background
        #[arg(long)]
        widget: Option<usize>,
        /// An option's path, such as motion.speed
        path: Option<String>,
        /// Its new value, as you'd type it: #ff8000, 0.5, on, ember
        value: Option<String>,
        /// Put the option back to its default
        #[arg(long, conflicts_with = "value")]
        reset: bool,
    },
    /// Pack a widget folder (deck.json, keyboard.luau, optional options.luau, host.luau and assets/), or a
    /// canvas folder (deck.json, canvas.slang, optional scripts), into a .deck
    Pack {
        folder: PathBuf,
        /// Defaults to <folder>.deck
        #[arg(long)]
        out: Option<PathBuf>,
        /// Canvases: pack again whenever a file in the folder changes, refreshing its .glassdeck types
        #[arg(long)]
        watch: bool,
    },
    /// Turn a video file, or a web page (a folder with index.html, or an .html file), into a background .deck
    Convert {
        input: PathBuf,
        /// Defaults to converted.<name>.deck in the current folder
        #[arg(long)]
        out: Option<PathBuf>,
        /// Bitrate when a video has to be re-encoded
        #[arg(long, default_value_t = 12)]
        mbps: u32,
    },
    /// List monitors with their index and EDID name
    Monitors,
    /// Record a monitor to an MP4 file
    Record {
        /// Monitor index or EDID name from `monitors`
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
    /// The key scanner (the keyboard's RP2040), straight over USB
    #[command(subcommand)]
    Scanner(scanner::ScannerCommand),
    /// The keyboard's Linux side, through its stock bridge on USB: versions, install, probe, backup
    #[command(subcommand)]
    Keyboard(keyboard::KeyboardCommand),
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
        Command::Scanner(command) => scanner::run(command),
        Command::Keyboard(command) => keyboard::run(command),
        Command::Streams => streams(),
        Command::Background { choice: None } => theme::show_background(),
        Command::Background { choice: Some(choice) } => theme::set_background(&choice),
        Command::Options { widget, path: None, .. } => theme::show_options(widget),
        Command::Options { widget, path: Some(path), value, reset } => match (value, reset) {
            (Some(text), _) => theme::set_option(widget, &path, Some(&text)),
            (None, true) => theme::set_option(widget, &path, None),
            (None, false) => bail!("give {path} a value, or --reset it to its default"),
        },
        Command::Theme { file: None, .. } => theme::show_theme(),
        Command::Theme { file: Some(file), library } => theme::push_theme_file(&file, library.as_deref().unwrap_or(Path::new("."))),
        Command::Pack { folder, out, watch } if glassdeck_slang::pack::is_canvas_folder(&folder) => {
            let out = out.unwrap_or_else(|| folder.with_extension(glassdeck_package::EXTENSION));
            let native = native_libraries()?;
            let slang = glassdeck_slang::Slang::load(&native)?;
            let luau = glassdeck_luau_check::LuauChecker::load(&native)?;
            let pack = || -> Result<()> {
                let canvas = glassdeck_slang::pack::pack_canvas(&slang, &luau, &folder, &out)?;
                println!("Packed the canvas {} into {}", canvas.manifest().title, out.display());
                Ok(())
            };
            if !watch {
                return pack();
            }
            println!("Watching {}; Ctrl+C stops", folder.display());
            let mut seen = None;
            loop {
                let now = canvas_files(&folder)?;
                if seen.as_ref() != Some(&now) {
                    if let Err(e) = pack() {
                        eprintln!("error: {e:#}");
                    }
                    seen = Some(now);
                }
                std::thread::sleep(WATCH_EVERY);
            }
        }
        Command::Pack { watch: true, .. } => bail!("--watch is for canvas folders (with canvas.slang)"),
        Command::Pack { folder, out, .. } => {
            let out = out.unwrap_or_else(|| folder.with_extension(glassdeck_package::EXTENSION));
            if [glassdeck_package::OPTIONS_SCRIPT, glassdeck_package::HOST_SCRIPT].iter().any(|script| folder.join(script).is_file()) {
                let luau = glassdeck_luau_check::LuauChecker::load(&native_libraries()?)?;
                glassdeck_slang::pack::check_widget(&luau, &folder)?;
            }
            let package = glassdeck_package::pack_folder(&folder, &out)?;
            println!("Packed {} into {}", package.manifest().id, out.display());
            Ok(())
        }
        Command::Convert { input, out, mbps } => convert(&input, out, mbps),
        Command::Monitors => {
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

/// How often `pack --watch` looks for changes.
const WATCH_EVERY: Duration = Duration::from_millis(500);

/// Every file in a canvas folder with when it last changed, leaving out the types packing writes there.
fn canvas_files(folder: &Path) -> Result<std::collections::BTreeMap<PathBuf, std::time::SystemTime>> {
    let mut files = std::collections::BTreeMap::new();
    let mut pending = vec![folder.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).with_context(|| format!("reading {}", dir.display()))? {
            let entry = entry?;
            let path = entry.path();
            if entry.file_name() == glassdeck_slang::pack::TYPES_FOLDER {
                continue;
            }
            let meta = entry.metadata()?;
            if meta.is_dir() {
                pending.push(path);
            } else {
                files.insert(path, meta.modified()?);
            }
        }
    }
    Ok(files)
}

/// Where the native libraries are (Slang, Luau's checker; native/build.py collects them): GLASSDECK_NATIVE, else beside this program.
fn native_libraries() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("GLASSDECK_NATIVE") {
        return Ok(PathBuf::from(dir));
    }
    let exe = std::env::current_exe().context("finding this program")?;
    Ok(exe.parent().context("this program has no folder")?.to_path_buf())
}

fn convert(input: &Path, out: Option<PathBuf>, mbps: u32) -> Result<()> {
    let title = input.file_stem().context("the input has no name")?.to_string_lossy().into_owned();
    let id = PackageId::from_name("converted", &title)?;
    let out = match out {
        Some(out) => out,
        None => PathBuf::from(id.file_name()),
    };
    let details = Details { id, title, origin: Origin::Converted { from: file_name(input)? }, preview: None };
    let is_html = input.extension().is_some_and(|e| e.eq_ignore_ascii_case("html") || e.eq_ignore_ascii_case("htm"));
    if input.is_dir() {
        if !input.join("index.html").is_file() {
            bail!("{} has no index.html; give the page's .html file instead", input.display());
        }
        glassdeck_convert::web(input, "index.html", Default::default(), &out, details)?
    } else if is_html {
        let folder = input.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
        glassdeck_convert::web(folder, &file_name(input)?, Default::default(), &out, details)?
    } else {
        glassdeck_convert::video(input, &out, details, EncodeSettings { bitrate: mbps * 1_000_000 })?
    };
    println!("Wrote {}; show it with `glassdeck bg {}`", out.display(), out.display());
    Ok(())
}

fn file_name(path: &Path) -> Result<String> {
    Ok(path.file_name().with_context(|| format!("{} has no file name", path.display()))?.to_string_lossy().into_owned())
}

fn status() -> Result<()> {
    let status: Status = client::get("/status")?;
    let connected = if status.link.connected { "connected" } else { "not connected" };
    println!("Keyboard {}: {connected}", status.link.keyboard);
    theme::show_background()?;
    if let Some(playing) = &status.stream.playing {
        println!("Source: {playing} ({})", status.stream.state.describe());
    }
    if let Some(ended) = &status.stream.last_ended {
        println!("Last stream: {}", ended.said);
    }
    Ok(())
}

fn streams() -> Result<()> {
    let answer: Streams = client::get("/streams")?;
    for stream in answer.streams {
        let shown = if stream.shown { "shown" } else { "" };
        let mode = serde_json::to_value(stream.mode)?;
        println!("{:<28} {:<6} {:<9} {shown}", stream.name, mode.as_str().unwrap_or("?"), stream.from);
    }
    Ok(())
}

