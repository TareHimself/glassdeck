//! The Glassdeck CLI. Anything that reaches the keyboard goes through glassdeckd, which owns the Link;
//! the rest (monitors, recording, converting, packing modules) runs here.

mod client;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use glassdeck_capture::monitors::{self, MonitorQuery};
use glassdeck_capture::record::{self, RecordOptions};
use glassdeck_convert::{Details, EncodeSettings};
use glassdeck_package::Origin;
use serde_json::{Value, json};

#[derive(Parser)]
#[command(about = "Glassdeck: choose what the keyboard shows, and put files on it")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// The Link, the background and where it runs
    Status,
    /// Streams glassdeckd can show, such as monitor-q27g4zd
    Streams,
    /// Show or set the background: gradient (built in), a .deckbg, or stream:<name>
    Bg {
        /// gradient, a .deckbg file (uploaded if it's a file here) or name on the keyboard, or stream:<name>
        choice: Option<String>,
    },
    /// Put a .deckbg, .deckmod or profile .json on the keyboard
    Deploy { file: PathBuf },
    /// Show the selected background's (or a widget's) options, or set one: glassdeck options look.tint "#ff8000"
    Options {
        /// A widget's place in the profile, instead of the background
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
    /// Pack a module folder (module.json, keyboard.luau, optional host.luau and assets/) into a .deckmod,
    /// or a canvas folder (canvas.slang, optional keyboard.luau) into a .deckbg
    Pack {
        folder: PathBuf,
        /// Defaults to <folder>.deckmod, or <folder>.deckbg for a canvas
        #[arg(long)]
        out: Option<PathBuf>,
        /// Canvases: pack again whenever a file in the folder changes, refreshing its .glassdeck types
        #[arg(long)]
        watch: bool,
    },
    /// Turn a video file, or a web page (a folder with index.html, or an .html file), into a .deckbg
    Convert {
        input: PathBuf,
        /// Defaults to %LOCALAPPDATA%\Glassdeck\backgrounds\<name>.deckbg
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
        Command::Streams => streams(),
        Command::Bg { choice: None } => {
            println!("{}", describe_bg(&client::call("GET", "/bg", None)?));
            Ok(())
        }
        Command::Bg { choice: Some(choice) } => set_bg(&choice),
        Command::Options { widget, path: None, .. } => {
            let url = widget.map_or_else(|| "/options".to_owned(), |index| format!("/options?widget={index}"));
            print_options(&client::call("GET", &url, None)?);
            Ok(())
        }
        Command::Options { widget, path: Some(path), value, reset } => {
            if value.is_none() && !reset {
                bail!("give {path} a value, or --reset it to its default");
            }
            let answer = client::call("PUT", "/options", Some(json!({ "widget": widget, "path": path, "value": value })))?;
            println!("{} {path} = {}", answer["background"].as_str().unwrap_or("?"), answer["value"]);
            Ok(())
        }
        Command::Deploy { file } => {
            println!("The keyboard installed {}", deploy(&file)?);
            Ok(())
        }
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
            let out = out.unwrap_or_else(|| folder.with_extension(glassdeck_package::MODULE_EXTENSION));
            if [glassdeck_package::OPTIONS_SCRIPT, glassdeck_package::HOST_SCRIPT].iter().any(|script| folder.join(script).is_file()) {
                let luau = glassdeck_luau_check::LuauChecker::load(&native_libraries()?)?;
                glassdeck_slang::pack::check_module(&luau, &folder)?;
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

/// A `.deckbg` that's a file here is uploaded first and then chosen by its name; anything else is
/// passed to glassdeckd as it is.
fn set_bg(choice: &str) -> Result<()> {
    let local = Path::new(choice);
    let name = if glassdeck_package::is_background(local) {
        deploy(local)?;
        file_name(local)?
    } else {
        choice.to_owned()
    };
    let answer = client::call("PUT", "/bg", Some(json!({ "bg": name })))?;
    let state = match answer["state"].as_str() {
        Some("shown") | None => String::new(),
        Some(other) => format!(" ({other})"),
    };
    println!("{}{state}", describe_bg(&answer));
    Ok(())
}

/// The panel as the app would draw it: sections indented, hidden options marked.
fn print_options(view: &Value) {
    fn walk(items: &[Value], depth: usize) {
        let indent = "  ".repeat(depth);
        for item in items {
            if let Some(section) = item["section"].as_str() {
                println!("{indent}{} ({section})", item["label"].as_str().unwrap_or(section));
                walk(item["items"].as_array().map_or(&[][..], Vec::as_slice), depth + 1);
            } else if let Some(heading) = item["heading"].as_str() {
                println!("{indent}-- {heading} --");
            } else if let Some(path) = item["option"].as_str() {
                let kind = &item["kind"];
                let limits = match kind["type"].as_str() {
                    Some("range") => format!("range {} to {}", kind["min"], kind["max"]),
                    Some("choice") => format!(
                        "one of {}",
                        kind["values"]
                            .as_array()
                            .map(|v| v.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", "))
                            .unwrap_or_default()
                    ),
                    Some(other) => other.to_owned(),
                    None => String::new(),
                };
                let hidden = if item["visible"].as_bool() == Some(false) { "  [hidden]" } else { "" };
                println!("{indent}{}: {}  ({path}, {limits}){hidden}", item["label"].as_str().unwrap_or(path), item["value"]);
            }
        }
    }
    println!("Options for {}", view["background"].as_str().unwrap_or("?"));
    walk(view["items"].as_array().map_or(&[][..], Vec::as_slice), 1);
    for dropped in view["dropped"].as_array().into_iter().flatten().filter_map(Value::as_str) {
        println!("  (dropped a stored value: {dropped})");
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

fn deploy(file: &Path) -> Result<String> {
    let path = std::path::absolute(file).with_context(|| format!("finding {}", file.display()))?;
    let answer = client::call("PUT", "/deploy", Some(json!({ "path": path })))?;
    Ok(text(&answer["deployed"]))
}

fn convert(input: &Path, out: Option<PathBuf>, mbps: u32) -> Result<()> {
    let title = input.file_stem().context("the input has no name")?.to_string_lossy().into_owned();
    let out = match out {
        Some(out) => out,
        None => glassdeck_convert::default_backgrounds_dir()?.join(format!("{title}.{}", glassdeck_package::EXTENSION)),
    };
    let details = Details { title, origin: Origin::Converted { from: file_name(input)? }, preview: None };
    let is_html = input.extension().is_some_and(|e| e.eq_ignore_ascii_case("html") || e.eq_ignore_ascii_case("htm"));
    let background = if input.is_dir() {
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
    println!("Wrote {}; show it with `glassdeck bg {}`", background.path().display(), background.path().display());
    Ok(())
}

fn file_name(path: &Path) -> Result<String> {
    Ok(path.file_name().with_context(|| format!("{} has no file name", path.display()))?.to_string_lossy().into_owned())
}

fn status() -> Result<()> {
    let status = client::call("GET", "/status", None)?;
    let link = &status["link"];
    let connected = if link["connected"] == Value::Bool(true) { "connected" } else { "not connected" };
    println!("Keyboard {}: {connected}", text(&link["keyboard"]));
    println!("{}", describe_bg(&status["bg"]));
    let stream = &status["stream"];
    if let Some(playing) = stream["playing"].as_str() {
        println!("Stream: {playing} ({})", text(&stream["state"]));
    }
    if let Some(ended) = stream["last_ended"].as_array().and_then(|pair| pair.get(1)) {
        println!("Last stream: {}", text(ended));
    }
    Ok(())
}

fn streams() -> Result<()> {
    let answer = client::call("GET", "/streams", None)?;
    for stream in answer["streams"].as_array().context("glassdeckd sent no stream list")? {
        let shown = if stream["shown"] == Value::Bool(true) { "shown" } else { "" };
        println!("{:<28} {:<6} {:<9} {shown}", text(&stream["name"]), text(&stream["mode"]), text(&stream["from"]));
    }
    Ok(())
}

fn describe_bg(answer: &Value) -> String {
    match (answer["bg"].as_str(), answer["runs_on"].as_str()) {
        (None, _) => "Background: the keyboard's own choice (none set from here)".into(),
        (Some(bg), Some("host")) => format!("Background: {bg}, streamed from this PC"),
        (Some(bg), _) => format!("Background: {bg}, played by the keyboard itself"),
    }
}

fn text(value: &Value) -> String {
    value.as_str().map_or_else(|| value.to_string(), str::to_owned)
}
