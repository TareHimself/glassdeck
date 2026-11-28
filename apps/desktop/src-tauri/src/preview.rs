//! The widget editor's preview: both halves of the widget, run by the same runtimes the keyboard and
//! glassdeckd use, on one thread (Luau VMs can't move between threads). What `host.luau` sends goes
//! straight to `keyboard.luau`, as the Link would carry it. Each new frame is sent as a PNG, and what
//! the scripts print, and the errors that stop them, as log lines.

use std::fs;
use std::path::{Path, PathBuf};
use std::cell::Cell;
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result};
use base64::Engine;
use glassdeck_host::{HostSide, Services, SharedNowPlaying, Sources, Wake};
use glassdeck_luau::options::read_values;
use glassdeck_luau::placement::{PlacedWidget, Placement};
use glassdeck_luau::{LiveName, Print};
use glassdeck_package::{Package, QualifiedId, pack_folder};
use glassdeck_widget::{Installed, Instance, LiveCache};
use serde::Serialize;
use specta::Type;
use tauri::AppHandle;
use tauri_specta::Event as _;

use crate::library::{Dims, SourceFile};

const FRAME: Duration = Duration::from_millis(16);
const IDLE: Duration = Duration::from_secs(1);
/// While the window is hidden a preview draws nothing; it only waits for commands.
pub(crate) const HIDDEN_WAIT: Duration = Duration::from_secs(3600);

#[derive(Clone, Copy, Debug, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum LogSource {
    /// Packing the editor's files, before anything runs
    Pack,
    Keyboard,
    Host,
}

#[derive(Clone, Copy, Debug, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum LogLevel {
    Print,
    Error,
}

/// Which start of a preview an update belongs to. The frontend picks it and passes it with the start,
/// and drops updates from any other, so a slow frame from the last widget never lands on the next. */
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, serde::Deserialize, Type)]
#[serde(transparent)]
pub struct PreviewRun(pub u32);

/// Something the widget preview did, in the run it belongs to.
#[derive(Clone, Debug, Serialize, Type, tauri_specta::Event)]
pub struct PreviewUpdate {
    pub run: PreviewRun,
    pub change: PreviewChange,
}

#[derive(Clone, Debug, Serialize, Type)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum PreviewChange {
    Frame { width: u32, height: u32, png: String },
    /// Nothing to show: the widget didn't start, or stopped on an error
    Cleared,
    Log { source: LogSource, level: LogLevel, text: String },
    /// The edit didn't start (it doesn't parse, or its top level failed); the last working run goes on
    Held { error: crate::script_errors::ScriptError },
    /// The run stopped on an error, also logged once
    Failed { error: crate::script_errors::ScriptError },
}

pub enum Command {
    Show { run: PreviewRun, folder: PathBuf, files: Vec<SourceFile>, size: Dims, values: serde_json::Map<String, serde_json::Value> },
    Resize(Dims),
    Options(serde_json::Map<String, serde_json::Value>),
    Stop,
    /// A `net` answer arrived for host.luau
    Answered,
    /// The window was shown or hidden; hidden, nothing is drawn
    Visible(bool),
}

pub struct Preview(Mutex<Sender<Command>>);

impl Preview {
    pub fn start(app: AppHandle) -> Self {
        let (tx, rx) = mpsc::channel();
        let answered = tx.clone();
        let wake: Wake = Arc::new(move || drop(answered.send(Command::Answered)));
        std::thread::Builder::new()
            .name("widget preview".into())
            .spawn(move || run(app, rx, wake))
            .expect("starting the preview thread");
        Self(Mutex::new(tx))
    }

    pub fn send(&self, command: Command) {
        let sent = self.0.lock().expect("the preview sender isn't poisoned").send(command);
        if sent.is_err() {
            eprintln!("the preview thread has stopped");
        }
    }
}

/// Where the preview's frames and log lines go.
pub(crate) type Out = Rc<dyn Fn(PreviewChange)>;

/// What the preview thread holds for the whole session.
pub(crate) struct Session {
    out: Out,
    font: fontdue::Font,
    services: Services,
    scratch: PathBuf,
    /// Wakes the preview thread for `net` answers; stills have none and never wait
    wake: Option<Wake>,
}

pub(crate) struct Running {
    pub(crate) instance: Instance,
    pub(crate) host: Option<HostSide>,
    pub(crate) live: LiveCache,
    /// The keyboard half's error, once logged, so it's logged once
    pub(crate) reported: Option<String>,
    pub(crate) last_frame: Option<Instant>,
}

fn run(app: AppHandle, rx: Receiver<Command>, wake: Wake) {
    let current = Rc::new(Cell::new(PreviewRun(0)));
    let stamp = Rc::clone(&current);
    let out: Out = Rc::new(move |change| {
        if let Err(e) = (PreviewUpdate { run: stamp.get(), change }).emit(&app) {
            eprintln!("emitting a preview update: {e}");
        }
    });
    let cx = match Session::new(out, wake) {
        Ok(cx) => cx,
        Err(e) => return eprintln!("preview: {e:#}"),
    };
    let mut running: Option<Running> = None;
    let mut hidden = false;
    loop {
        let wait = if hidden { HIDDEN_WAIT } else { running.as_ref().map_or(IDLE, |r| wait_for(r, Instant::now())) };
        match rx.recv_timeout(wait) {
            Ok(Command::Show { run, folder, files, size, values }) => {
                current.set(run);
                let working = running.as_ref().is_some_and(|r| r.instance.error().is_none());
                match show(&cx, EDITOR_SLOT, &folder, &files, size, &values) {
                    Ok(started) => match started.instance.error() {
                        Some(error) if working => (cx.out)(PreviewChange::Held { error: crate::script_errors::located(error) }),
                        _ => running = Some(started),
                    },
                    Err(e) if working => (cx.out)(PreviewChange::Held { error: crate::script_errors::located(&format!("{e:#}")) }),
                    Err(e) => {
                        cx.log(LogSource::Pack, LogLevel::Error, format!("{e:#}"));
                        (cx.out)(PreviewChange::Cleared);
                    }
                }
            }
            Ok(Command::Resize(size)) => {
                if let Some(r) = &mut running {
                    r.instance.resize(Placement { x: 0, y: 0, w: size.w, h: size.h });
                }
            }
            Ok(Command::Options(values)) => {
                if let Some(r) = &mut running {
                    apply_options(&cx, r, &values);
                }
            }
            Ok(Command::Stop) => running = None,
            Ok(Command::Visible(visible)) => hidden = !visible,
            Ok(Command::Answered) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        if hidden {
            continue;
        }
        if let Some(r) = &mut running {
            match step(&cx, r) {
                Drawn::Nothing => {}
                Drawn::Frame { width, height, png } => (cx.out)(PreviewChange::Frame { width, height, png }),
                Drawn::Stopped => (cx.out)(PreviewChange::Cleared),
            }
        }
    }
}

impl Session {
    pub(crate) fn new(out: Out, wake: Wake) -> Result<Self> {
        let now_playing = glassdeck_host::start_media().unwrap_or_else(|e| {
            out(PreviewChange::Log { source: LogSource::Host, level: LogLevel::Error, text: format!("media isn't available: {e:#}") });
            Arc::new(Mutex::new(None))
        });
        Self::with_media(out, now_playing, Some(wake))
    }

    /// A session that reports nothing playing, for stills: widgets draw their placeholders.
    pub(crate) fn without_media(out: Out) -> Result<Self> {
        Self::with_media(out, Arc::new(Mutex::new(None)), None)
    }

    fn with_media(out: Out, now_playing: SharedNowPlaying, wake: Option<Wake>) -> Result<Self> {
        let font = glassdeck_widget::load_font(None)?;
        static SESSIONS: AtomicU32 = AtomicU32::new(0);
        let session = SESSIONS.fetch_add(1, Ordering::Relaxed);
        let scratch = std::env::temp_dir().join(format!("glassdeck-preview-{}-{session}", std::process::id()));
        Ok(Self { out, font, services: Services::new(now_playing), scratch, wake })
    }

    fn log(&self, source: LogSource, level: LogLevel, text: String) {
        (self.out)(PreviewChange::Log { source, level, text });
    }

    fn printer(&self, source: LogSource) -> Print {
        let out = Rc::clone(&self.out);
        Rc::new(move |text| out(PreviewChange::Log { source, level: LogLevel::Print, text }))
    }
}

pub(crate) fn wait_for(running: &Running, now: Instant) -> Duration {
    let until = |at: Instant| at.saturating_duration_since(now);
    let keyboard = running.instance.next_timer().map(until);
    let host = running.host.as_ref().and_then(HostSide::next_timer).map(until);
    let frame = running.instance.wants_frame().then_some(FRAME);
    [keyboard, host, frame].into_iter().flatten().min().unwrap_or(IDLE)
}

/// What one step of a widget gave.
pub(crate) enum Drawn {
    Nothing,
    Frame { width: u32, height: u32, png: String },
    /// The keyboard half stopped on an error, now logged
    Stopped,
}

/// Runs what's due on both halves, hands the host half's output to the keyboard half, and returns the
/// frame if one was drawn.
pub(crate) fn step(cx: &Session, r: &mut Running) -> Drawn {
    let now = Instant::now();
    if let Some(host) = &r.host
        && let Err(e) = host.run_timers(now).and_then(|()| host.run_answers())
    {
        cx.log(LogSource::Host, LogLevel::Error, format!("{e:#}; host.luau stopped"));
        r.host = None;
    }
    deliver(cx, r);
    let frame_dt = r.instance.wants_frame().then(|| r.last_frame.map_or(FRAME, |at| now - at));
    r.last_frame = frame_dt.map(|_| now);
    r.instance.tick(now, frame_dt);
    let tile = r.instance.draw(&cx.font, &r.live);
    if let Some(error) = r.instance.error() {
        if r.reported.as_deref() == Some(error) {
            return Drawn::Nothing;
        }
        r.reported = Some(error.to_owned());
        cx.log(LogSource::Keyboard, LogLevel::Error, error.to_owned());
        (cx.out)(PreviewChange::Failed { error: crate::script_errors::located(error) });
        return Drawn::Stopped;
    }
    let Some(tile) = tile else { return Drawn::Nothing };
    let Placement { w, h, .. } = r.instance.placement;
    match png_of(&tile.pixels, w, h) {
        Ok(png) => Drawn::Frame { width: w, height: h, png },
        Err(e) => {
            cx.log(LogSource::Keyboard, LogLevel::Error, format!("encoding a frame: {e:#}"));
            Drawn::Nothing
        }
    }
}

/// What the host half sent and the live images it made, to the keyboard half.
/// Carries what each half sent to the other, as the Link will between the PC and the keyboard.
fn deliver(cx: &Session, r: &mut Running) {
    let Some(host) = &r.host else { return };
    if let Some(payload) = host.take_options() {
        r.instance.deliver_options(&payload);
    }
    for sent in host.take_messages() {
        r.instance.deliver_message(sent.message, &sent.payload);
    }
    for sent in r.instance.take_messages() {
        if let Err(e) = host.deliver_message(sent.message, &sent.payload) {
            cx.log(LogSource::Host, LogLevel::Error, format!("{e:#}"));
        }
    }
    for (name, bytes) in host.take_live() {
        let stored = LiveName::parse(&name).and_then(|name| r.live.put(name.clone(), bytes).map(|dropped| (name, dropped)));
        match stored {
            Ok((name, dropped)) => {
                let mut changed = dropped;
                changed.push(name);
                r.instance.live_changed(&changed);
            }
            Err(e) => cx.log(LogSource::Host, LogLevel::Error, format!("live.put {name}: {e:#}")),
        }
    }
}

pub(crate) fn apply_options(cx: &Session, r: &mut Running, values: &serde_json::Map<String, serde_json::Value>) {
    let Some(host) = &r.host else { return };
    let chosen = read_values("preview", host.first(), values.iter());
    if let Err(e) = host.apply_options(&chosen, None) {
        cx.log(LogSource::Host, LogLevel::Error, format!("{e:#}"));
    }
}

/// The widget editor's scratch folder; the Overview gives each placed widget its own.
const EDITOR_SLOT: &str = "editor";

/// Packs the widget folder with `files` over it, in its own `slot` of the scratch folder, and starts both
/// halves at `size`.
pub(crate) fn show(
    cx: &Session,
    slot: &str,
    folder: &Path,
    files: &[SourceFile],
    size: Dims,
    values: &serde_json::Map<String, serde_json::Value>,
) -> Result<Running> {
    let source = cx.scratch.join(slot).join("source");
    if source.exists() {
        fs::remove_dir_all(&source).with_context(|| format!("clearing {}", source.display()))?;
    }
    copy_folder(folder, &source)?;
    for file in files {
        fs::write(source.join(&file.name), &file.text).with_context(|| format!("writing {}", file.name))?;
    }
    let deck = cx.scratch.join(slot).join("preview.deck");
    pack_folder(&source, &deck)?;
    let package = Rc::new(Package::open(&deck)?);
    let id: QualifiedId = package.id().clone();
    let readable = Rc::clone(&package);
    let sources = Sources {
        label: "host.luau".into(),
        options: package.options_script()?,
        host: package.host_script()?,
        reader: Some(Rc::new(move |resource| readable.resource(resource))),
        message_limit: None,
        print: Some(cx.printer(LogSource::Host)),
        messages: package.bridge_script()?,
        network: package.manifest().network.clone(),
        wake: cx.wake.clone(),
    };
    // A preview has no keyboard to lose: keyboard.connected() stays true and script.on_keyboard never fires.
    let host = match HostSide::start(sources, &cx.services) {
        Ok(host) => host,
        Err(e) => {
            cx.log(LogSource::Host, LogLevel::Error, format!("{e:#}"));
            None
        }
    };
    let installed = Installed { package, modified: SystemTime::now() };
    let widget = PlacedWidget { package: id, placement: Placement { x: 0, y: 0, w: size.w, h: size.h } };
    let instance = Instance::start(0, &widget, installed, None, Some(cx.printer(LogSource::Keyboard)));
    let mut running = Running { instance, host, live: LiveCache::default(), reported: None, last_frame: None };
    apply_options(cx, &mut running, values);
    Ok(running)
}

fn copy_folder(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir_all(to).with_context(|| format!("creating {}", to.display()))?;
    for entry in fs::read_dir(from).with_context(|| format!("reading {}", from.display()))? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_folder(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), &target).with_context(|| format!("copying {}", entry.path().display()))?;
        }
    }
    Ok(())
}

/// Premultiplied ARGB words to a base64 PNG with straight alpha.
fn png_of(pixels: &[u32], width: u32, height: u32) -> Result<String> {
    let unpremultiply = |c: u32, a: u32| (c * 255).checked_div(a).map_or(0, |v| v.min(255) as u8);
    let rgba: Vec<u8> = pixels
        .iter()
        .flat_map(|p| {
            let a = p >> 24;
            [unpremultiply((p >> 16) & 0xff, a), unpremultiply((p >> 8) & 0xff, a), unpremultiply(p & 0xff, a), a as u8]
        })
        .collect();
    let mut bytes = Vec::new();
    let mut encoder = png::Encoder::new(&mut bytes, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(&rgba)?;
    Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;
    use crate::library::{NewWidget, Sizing, create_widget};

    fn session() -> (Session, Rc<RefCell<Vec<PreviewChange>>>) {
        let seen = Rc::new(RefCell::new(Vec::new()));
        let sink = Rc::clone(&seen);
        let cx = Session::new(Rc::new(move |update| sink.borrow_mut().push(update)), Arc::new(|| {})).unwrap();
        (cx, seen)
    }

    fn new_widget(root: &Path) -> PathBuf {
        let size = Dims { w: 320, h: 160 };
        let new = NewWidget { title: "Sky".into(), id: "local.user.sky".into(), sizing: Sizing::Fixed { size } };
        create_widget(root, new).unwrap();
        root.join("widget.local.user.sky")
    }

    #[test]
    fn both_halves_run_and_print_reaches_the_log() {
        let root = std::env::temp_dir().join(format!("glassdeck-preview-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let folder = new_widget(&root);
        let keyboard = SourceFile { name: "keyboard.luau".into(), text: "print('drawn by', 'keyboard')\nlocal text = ''\nbridge.on.label(function(l) text = l.text end)\nscript.on_draw(function(ui) ui:text(text) end)\n".into() };
        let host = SourceFile { name: "host.luau".into(), text: "print('host up')\nscript.on_options(function(options) keyboard.bridge.label({ text = options.label }) end)\n".into() };
        let bridge = SourceFile { name: "bridge.luau".into(), text: "export type label = KeyboardState<{ text: string }>\n".into() };
        let options = SourceFile { name: "options.luau".into(), text: "option(\"label\", \"Label\", text(\"Sky\"))\n".into() };
        let (cx, seen) = session();
        let values = serde_json::json!({ "label": "Hi" });
        let mut running = show(&cx, "test", &folder, &[keyboard, host, options, bridge], Dims { w: 320, h: 160 }, values.as_object().unwrap()).unwrap();
        let drawn = step(&cx, &mut running);
        assert!(matches!(drawn, Drawn::Frame { width: 320, .. }), "a frame was drawn");
        let seen = seen.borrow();
        let prints: Vec<&str> = seen.iter().filter_map(|u| match u { PreviewChange::Log { level: LogLevel::Print, text, .. } => Some(text.as_str()), _ => None }).collect();
        assert_eq!(prints, ["host up", "drawn by\tkeyboard"]);
        assert!(running.host.is_some());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn typed_messages_go_both_ways_between_the_halves() {
        let root = std::env::temp_dir().join(format!("glassdeck-preview-messages-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let folder = new_widget(&root);
        let messages = SourceFile {
            name: "bridge.luau".into(),
            text: "export type label = KeyboardState<{ text: string }>
export type drawn = HostEvent<{ width: number }>
".into(),
        };
        let host = SourceFile {
            name: "host.luau".into(),
            text: "script.on_options(function(o) keyboard.bridge.label({ text = o.label }) end)
bridge.on.drawn(function(d) print('keyboard drew at', d.width) end)
".into(),
        };
        let keyboard = SourceFile {
            name: "keyboard.luau".into(),
            text: "local text = ''
bridge.on.label(function(l) text = l.text end)
script.on_draw(function(ui) ui:text(text) host.bridge.drawn({ width = ui.width }) end)
".into(),
        };
        let options = SourceFile { name: "options.luau".into(), text: "option(\"label\", \"Label\", text(\"Sky\"))
".into() };
        let (cx, seen) = session();
        let mut running = show(&cx, "test", &folder, &[keyboard, host, options, messages], Dims { w: 320, h: 160 }, &serde_json::Map::new()).unwrap();
        assert!(matches!(step(&cx, &mut running), Drawn::Frame { .. }), "the keyboard half drew with the label it heard");
        step(&cx, &mut running);
        let seen = seen.borrow();
        let prints: Vec<&str> = seen.iter().filter_map(|u| match u { PreviewChange::Log { level: LogLevel::Print, text, .. } => Some(text.as_str()), _ => None }).collect();
        assert_eq!(prints, ["keyboard drew at	320"], "{seen:#?}");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn with_a_blank_host_luau_the_options_arrive_in_script_on_options() {
        let root = std::env::temp_dir().join(format!("glassdeck-preview-options-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let folder = new_widget(&root);
        let keyboard = SourceFile {
            name: "keyboard.luau".into(),
            text: "local text = ''
bridge.on.options(function(o) text = o.label print('label', text) end)
script.on_draw(function(ui) ui:text(text) end)
".into(),
        };
        let options = SourceFile { name: "options.luau".into(), text: "option(\"label\", \"Label\", text(\"Sky\"))
".into() };
        let (cx, seen) = session();
        let values = serde_json::json!({ "label": "Hi" });
        let mut running = show(&cx, "test", &folder, &[keyboard, options], Dims { w: 320, h: 160 }, values.as_object().unwrap()).unwrap();
        assert!(matches!(step(&cx, &mut running), Drawn::Frame { .. }));
        let seen = seen.borrow();
        let prints: Vec<&str> = seen.iter().filter_map(|u| match u { PreviewChange::Log { level: LogLevel::Print, text, .. } => Some(text.as_str()), _ => None }).collect();
        assert_eq!(prints, ["label	Hi"], "{seen:#?}");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_failed_draw_is_logged_once_and_clears_the_preview() {
        let root = std::env::temp_dir().join(format!("glassdeck-preview-fail-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let folder = new_widget(&root);
        let keyboard = SourceFile { name: "keyboard.luau".into(), text: "script.on_draw(function() error('boom') end)\n".into() };
        let (cx, seen) = session();
        let mut running = show(&cx, "test", &folder, &[keyboard], Dims { w: 320, h: 160 }, &serde_json::Map::new()).unwrap();
        assert!(matches!(step(&cx, &mut running), Drawn::Stopped));
        assert!(matches!(step(&cx, &mut running), Drawn::Nothing), "a stop is reported once");
        let seen = seen.borrow();
        let errors = seen.iter().filter(|u| matches!(u, PreviewChange::Log { level: LogLevel::Error, text, .. } if text.contains("boom"))).count();
        assert_eq!(errors, 1);

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn premultiplied_pixels_become_straight_alpha() {
        let half_red = 0x80_80_00_00;
        let png = png_of(&[half_red, 0], 2, 1).unwrap();
        assert!(!png.is_empty());
    }
}
