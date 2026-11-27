//! The canvas editor's preview: the editor's unsaved `canvas.slang` compiled by Slang as packing would,
//! drawn by glassdeck-gpu (the keyboard's own renderer) in a hidden OpenGL context, with `host.luau`
//! run by glassdeck-host. One thread owns the context. Frames go out as JPEGs, the inputs the script
//! wrote a few times a second, and prints and errors as log lines.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, ensure};
use glassdeck_gpu::offscreen::Offscreen;
use glassdeck_gpu::program::{CanvasProgram, render_size};
use glassdeck_gpu::script::{CanvasScript, InputValue};
use glassdeck_host::{HostSide, Services, SharedNowPlaying, Sources, Wake};
use glassdeck_luau::Print;
use glassdeck_proto::compositor::MAX_SCRIPT_PAYLOAD_LEN;
use glassdeck_luau::options::read_values;
use glassdeck_package::{CanvasFiles, DeckSource, HOST_SCRIPT, InputField, KEYBOARD_SCRIPT, NetworkHost, OPTIONS_SCRIPT, Origin, TargetFormat, is_blank_script};
use glassdeck_slang::Slang;
use glassdeck_slang::pack::{CANVAS_SOURCE, CanvasSources, compile_canvas};
use image::RgbImage;
use image::codecs::jpeg::JpegEncoder;
use serde::Serialize;
use specta::Type;
use tauri::AppHandle;
use tauri_specta::Event as _;

use crate::library::SourceFile;
use crate::preview::{LogLevel, LogSource, PreviewRun};

/// The fastest the preview draws, whatever the canvas asks for.
const MAX_FPS: u32 = 30;
const JPEG_QUALITY: u8 = 82;
/// A still's width, the same as a video background's preview image
const STILL_WIDTH: u32 = 480;
const INPUTS_EVERY: Duration = Duration::from_millis(200);
const IDLE: Duration = Duration::from_secs(1);

/// An input `keyboard.luau` wrote, for the shader.
#[derive(Clone, Debug, Serialize, Type)]
pub struct InputUpdate {
    pub name: String,
    pub value: InputPayload,
}

/// An input's new value, the way the shader takes it.
#[derive(Clone, Debug, Serialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InputPayload {
    /// One to four floats: a float or a vector
    Float { values: Vec<f32> },
    Uint { value: u32 },
    Array { values: Vec<f32> },
}

impl InputUpdate {
    fn of(name: &str, value: &InputValue) -> Self {
        let value = match value {
            InputValue::Float(values) => InputPayload::Float { values: values.clone() },
            InputValue::Uint(value) => InputPayload::Uint { value: *value },
            InputValue::Array(values) => InputPayload::Array { values: values.clone() },
        };
        Self { name: name.to_owned(), value }
    }
}

/// A pass the preview draws before the canvas: its shader, and the target it draws into.
#[derive(Clone, Debug, Serialize, Type)]
pub struct PreviewPass {
    pub fragment: String,
    pub writes: String,
}

/// A target as the preview makes it, at its size in pixels.
#[derive(Clone, Debug, Serialize, Type)]
pub struct PreviewTarget {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub format: PreviewFormat,
    pub feedback: bool,
}

#[derive(Clone, Copy, Debug, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum PreviewFormat {
    Rgba8,
    Rgba16f,
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct InputReading {
    pub name: String,
    pub values: Vec<f32>,
}

/// Something the canvas preview did, in the run it belongs to.
#[derive(Clone, Debug, Serialize, Type, tauri_specta::Event)]
pub struct CanvasUpdate {
    pub run: PreviewRun,
    pub change: CanvasChange,
}

#[derive(Clone, Debug, Serialize, Type)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CanvasChange {
    /// A run's shaders, compiled to GLSL ES 3.00 (what the keyboard runs, and WebGL2 too): the passes
    /// drawn first into their targets, then the canvas at its size
    Program { fragment: String, width: u32, height: u32, passes: Vec<PreviewPass>, targets: Vec<PreviewTarget> },
    /// One frame: the clock, and the inputs `keyboard.luau` wrote since the last one
    Frame { time: f32, delta: f32, index: u32, inputs: Vec<InputUpdate> },
    /// Nothing to show: the canvas didn't compile, or stopped on an error
    Cleared,
    Inputs { inputs: Vec<InputReading> },
    Log { source: LogSource, level: LogLevel, text: String },
    /// The edit didn't start (it doesn't compile, or its script's top level failed); the last working run
    /// goes on, its program sent again for the new run
    Held { error: crate::script_errors::ScriptError },
    /// The run stopped on an error, also logged once
    Failed { error: crate::script_errors::ScriptError },
}

pub enum Command {
    /// `folder`: the canvas's library folder, whose `deck.json` says which hosts `net` may reach
    Show { run: PreviewRun, folder: Option<PathBuf>, files: Vec<SourceFile>, values: serde_json::Map<String, serde_json::Value> },
    Options(serde_json::Map<String, serde_json::Value>),
    Play(bool),
    Restart,
    Stop,
    /// A `net` answer arrived for host.luau
    Answered,
}

pub struct CanvasPreview(Mutex<Sender<Command>>);

impl CanvasPreview {
    pub fn start(app: AppHandle, native: Option<PathBuf>) -> Self {
        let (tx, rx) = mpsc::channel();
        let answered = tx.clone();
        let wake: Wake = Arc::new(move || drop(answered.send(Command::Answered)));
        std::thread::Builder::new()
            .name("canvas preview".into())
            .spawn(move || run(app, native, rx, wake))
            .expect("starting the canvas preview thread");
        Self(Mutex::new(tx))
    }

    pub fn send(&self, command: Command) {
        let sent = self.0.lock().expect("the canvas preview sender isn't poisoned").send(command);
        if sent.is_err() {
            eprintln!("the canvas preview thread has stopped");
        }
    }
}

type Out = Rc<dyn Fn(CanvasChange)>;

struct Session {
    out: Out,
    native: Option<PathBuf>,
    slang: Option<Slang>,
    services: Services,
    wake: Wake,
}

/// The canvas's clock: it runs while playing and holds still while paused.
#[derive(Clone, Copy)]
struct Clock {
    base: f32,
    since: Option<Instant>,
}

impl Clock {
    fn started(now: Instant) -> Self {
        Self { base: 0.0, since: Some(now) }
    }

    fn at(self, now: Instant) -> f32 {
        self.base + self.since.map_or(0.0, |since| now.saturating_duration_since(since).as_secs_f32())
    }

    fn played(self, playing: bool, now: Instant) -> Self {
        match (playing, self.since) {
            (true, None) => Self { base: self.base, since: Some(now) },
            (false, Some(_)) => Self { base: self.at(now), since: None },
            _ => self,
        }
    }
}

/// A canvas running live: its script and host half here, its shader in the webview.
struct Running {
    files: CanvasFiles,
    script: Option<CanvasScript>,
    /// Every input's latest value, as the script last wrote it
    values: BTreeMap<String, InputValue>,
    host: Option<HostSide>,
    clock: Clock,
    last: Option<f32>,
    index: u32,
    /// The first frame has gone out (so every input has been written)
    started: bool,
    frame: Duration,
    next_frame: Instant,
    inputs_sent: Option<Instant>,
    /// Send every input on the next frame, not only the ones just written: a held run's new viewer has none
    resend: bool,
}

fn run(app: AppHandle, native: Option<PathBuf>, rx: Receiver<Command>, wake: Wake) {
    let current = Rc::new(Cell::new(PreviewRun(0)));
    let stamp = Rc::clone(&current);
    let out: Out = Rc::new(move |change| {
        if let Err(e) = (CanvasUpdate { run: stamp.get(), change }).emit(&app) {
            eprintln!("emitting a canvas preview update: {e}");
        }
    });
    let now_playing = glassdeck_host::start_media().unwrap_or_else(|e| {
        out(CanvasChange::Log { source: LogSource::Host, level: LogLevel::Error, text: format!("media isn't available: {e:#}") });
        Arc::new(Mutex::new(None))
    });
    let mut cx = Session { out, native, slang: None, services: Services::new(now_playing), wake };
    let mut running: Option<Running> = None;
    let mut playing = true;
    loop {
        let wait = running.as_ref().map_or(IDLE, |r| wait_for(r, playing, Instant::now()));
        match rx.recv_timeout(wait) {
            Ok(Command::Show { run, folder, files, values }) => {
                current.set(run);
                let network = folder.map_or_else(|| Ok(Vec::new()), |folder| DeckSource::read(&folder).map(|(deck, _)| deck.network)).unwrap_or_else(|e| {
                    cx.log(LogSource::Host, LogLevel::Error, format!("{e:#}; net reaches no hosts"));
                    Vec::new()
                });
                match show(&mut cx, &files, &values, network) {
                    Ok(mut started) => {
                        if let Some(r) = running.take() {
                            started.clock = r.clock;
                        }
                        started.clock = started.clock.played(playing, Instant::now());
                        running = Some(started);
                    }
                    Err(e) => match &mut running {
                        Some(r) => {
                            (cx.out)(CanvasChange::Held { error: crate::script_errors::located(&format!("{e:#}")) });
                            (cx.out)(program_change(&r.files));
                            (r.started, r.resend, r.next_frame) = (false, true, Instant::now());
                        }
                        None => {
                            cx.log(LogSource::Pack, LogLevel::Error, format!("{e:#}"));
                            (cx.out)(CanvasChange::Cleared);
                        }
                    },
                }
            }
            Ok(Command::Options(values)) => {
                if let Some(r) = &running {
                    apply_options(&cx, r, &values);
                }
            }
            Ok(Command::Play(play)) => {
                playing = play;
                if let Some(r) = &mut running {
                    r.clock = r.clock.played(play, Instant::now());
                }
            }
            Ok(Command::Restart) => {
                if let Some(r) = &mut running
                    && let Err(e) = restart(&cx, r, playing)
                {
                    cx.log(LogSource::Keyboard, LogLevel::Error, format!("{e:#}"));
                    running = None;
                }
            }
            Ok(Command::Stop) => {
                running = None;
                playing = true;
            }
            Ok(Command::Answered) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        if let Some(r) = &mut running
            && let Err(e) = step(&cx, r, playing)
        {
            cx.log(LogSource::Keyboard, LogLevel::Error, format!("{e:#}"));
            (cx.out)(CanvasChange::Failed { error: crate::script_errors::located(&format!("{e:#}")) });
            (cx.out)(CanvasChange::Cleared);
            running = None;
        }
    }
}

impl Session {
    fn log(&self, source: LogSource, level: LogLevel, text: String) {
        (self.out)(CanvasChange::Log { source, level, text });
    }

    fn printer(&self, source: LogSource) -> Print {
        let out = Rc::clone(&self.out);
        Rc::new(move |text| out(CanvasChange::Log { source, level: LogLevel::Print, text }))
    }

    /// Slang, loaded on first use and kept.
    fn slang(&mut self) -> Result<&Slang> {
        if self.slang.is_none() {
            let native = self.native.as_ref().context("the native libraries' folder isn't known")?;
            self.slang = Some(Slang::load(native).context("loading Slang")?);
        }
        Ok(self.slang.as_ref().expect("loaded above"))
    }
}

fn wait_for(r: &Running, playing: bool, now: Instant) -> Duration {
    let host = r.host.as_ref().and_then(HostSide::next_timer).map(|at| at.saturating_duration_since(now));
    let frame = playing.then(|| r.next_frame.saturating_duration_since(now));
    [host, frame, Some(IDLE)].into_iter().flatten().min().unwrap_or(IDLE)
}

/// Runs the host half, hands what it sent to `keyboard.luau`, and draws a frame when one is due.
fn step(cx: &Session, r: &mut Running, playing: bool) -> Result<()> {
    let now = Instant::now();
    if let Some(host) = &r.host {
        if let Err(e) = host.run_timers(now).and_then(|()| host.run_answers()) {
            cx.log(LogSource::Host, LogLevel::Error, format!("{e:#}; host.luau stopped"));
            r.host = None;
        } else if let Some(payload) = host.take_options() {
            if let Some(script) = &r.script {
                script.deliver_options(&payload)?;
            }
            r.next_frame = now;
        }
    }
    if let (Some(host), Some(script)) = (&r.host, &r.script) {
        for sent in host.take_messages() {
            script.deliver_message(sent.message, &sent.payload)?;
            r.next_frame = now;
        }
        for sent in script.take_messages() {
            if let Err(e) = host.deliver_message(sent.message, &sent.payload) {
                cx.log(LogSource::Host, LogLevel::Error, format!("{e:#}"));
            }
        }
    }
    if now < r.next_frame {
        return Ok(());
    }
    r.next_frame = now + r.frame;
    let time = r.clock.at(now);
    let delta = r.last.map_or(0.0, |last| time - last);
    let changes = match &r.script {
        Some(script) => {
            script.frame(time, delta, r.index)?;
            if !r.started {
                let unwritten = script.unwritten();
                ensure!(unwritten.is_empty(), "keyboard.luau never wrote these inputs: {}", unwritten.join(", "));
            }
            script.take_changes()
        }
        None => BTreeMap::new(),
    };
    if playing || !r.started || !changes.is_empty() {
        let sent = if std::mem::take(&mut r.resend) { r.values.iter().chain(&changes).collect::<BTreeMap<_, _>>() } else { changes.iter().collect() };
        let inputs = sent.into_iter().map(|(name, value)| InputUpdate::of(name, value)).collect();
        (cx.out)(CanvasChange::Frame { time, delta, index: r.index, inputs });
        r.last = Some(time);
        r.index = r.index.wrapping_add(1);
        r.started = true;
        r.values.extend(changes);
    }
    if r.inputs_sent.is_none_or(|at| now.saturating_duration_since(at) >= INPUTS_EVERY) {
        r.inputs_sent = Some(now);
        (cx.out)(CanvasChange::Inputs { inputs: readings(&r.files.canvas.inputs, &r.values) });
    }
    if !playing {
        r.next_frame = now + IDLE;
    }
    Ok(())
}

/// The inputs written so far, in the order `[Inputs]` declares them.
fn readings(fields: &[InputField], written: &BTreeMap<String, InputValue>) -> Vec<InputReading> {
    fields
        .iter()
        .filter_map(|field| {
            let values = match written.get(&field.name)? {
                InputValue::Float(v) | InputValue::Array(v) => v.clone(),
                InputValue::Uint(n) => vec![*n as f32],
            };
            Some(InputReading { name: field.name.clone(), values })
        })
        .collect()
}

fn apply_options(cx: &Session, r: &Running, values: &serde_json::Map<String, serde_json::Value>) {
    let Some(host) = &r.host else { return };
    let chosen = read_values("preview", host.first(), values.iter());
    if let Err(e) = host.apply_options(&chosen, None) {
        cx.log(LogSource::Host, LogLevel::Error, format!("{e:#}"));
    }
}

/// The editor's text for one script, or `None` when it's missing or blank.
fn script<'a>(files: &'a [SourceFile], name: &str) -> Option<&'a str> {
    files.iter().find(|f| f.name == name).map(|f| f.text.as_str()).filter(|text| !is_blank_script(text))
}

/// The editor's files compiled for the GPU, with the scripts the host half runs.
struct Compiled<'a> {
    files: CanvasFiles,
    options: Option<&'a str>,
    host: Option<&'a str>,
}

fn compile<'a>(slang: &Slang, files: &'a [SourceFile]) -> Result<Compiled<'a>> {
    let shader = files.iter().find(|f| f.name == CANVAS_SOURCE).map(|f| f.text.as_str()).with_context(|| format!("there's no {CANVAS_SOURCE}"))?;
    let sources = CanvasSources {
        shader,
        path: CANVAS_SOURCE,
        keyboard: script(files, KEYBOARD_SCRIPT),
        options: script(files, OPTIONS_SCRIPT),
        host: script(files, HOST_SCRIPT),
        messages: script(files, glassdeck_package::BRIDGE_SCRIPT),
    };
    let compiled = compile_canvas(slang, &sources)?;
    let canvas_files = CanvasFiles {
        origin: Origin::Packed { from: "preview".into(), sources: String::new() },
        canvas: compiled.canvas,
        fragment: compiled.fragment,
        passes: compiled.passes,
        script: sources.keyboard.map(str::to_owned),
        messages: sources.messages.map(str::to_owned),
    };
    Ok(Compiled { files: canvas_files, options: sources.options, host: sources.host })
}

/// The canvas at `at` seconds, as a JPEG: options at their defaults, and nothing from the PC (no media,
/// no timers), the way it first appears. A canvas with a history has it built up over the seconds
/// before. Makes its own GL context, so any thread may call it.
pub fn render_still(slang: &Slang, files: &[SourceFile], at: f32) -> Result<Vec<u8>> {
    let compiled = compile(slang, files)?;
    let offscreen = Offscreen::new(render_size(compiled.files.canvas.scale))?;
    let messages = compiled.files.messages.clone();
    let mut program = CanvasProgram::new(&offscreen.gl, compiled.files, None)?;
    let sources = Sources {
        label: "host.luau".into(),
        options: compiled.options.map(str::to_owned),
        host: compiled.host.map(str::to_owned),
        reader: None,
        message_limit: Some(MAX_SCRIPT_PAYLOAD_LEN),
        print: None,
        messages,
        network: Vec::new(),
        wake: None,
    };
    let nothing_playing: SharedNowPlaying = Arc::new(Mutex::new(None));
    if let Some(host) = HostSide::start(sources, &Services::new(nothing_playing))? {
        host.apply_options(&read_values("still", host.first(), serde_json::Map::new().iter()), None)?;
        if let Some(payload) = host.take_options() {
            program.deliver_options(&payload)?;
        }
    }
    let size = offscreen.size();
    for seconds in still_frames(&program.canvas, at) {
        program.draw(&offscreen.gl, offscreen.framebuffer(), size, seconds)?;
    }
    jpeg_bytes(&offscreen.read_rgb(), size, STILL_WIDTH)
}

/// How much history a still with feedback is drawn from: enough for trails to settle.
const STILL_HISTORY: f32 = 3.0;

/// The moments a still draws: just `at`, or with feedback each frame of the seconds leading up to it.
fn still_frames(canvas: &glassdeck_package::Canvas, at: f32) -> Vec<f32> {
    if !canvas.targets.iter().any(|t| t.feedback) {
        return vec![at];
    }
    let period = 1.0 / canvas.frame_rate as f32;
    let first = (at - STILL_HISTORY).max(0.0);
    let count = ((at - first) / period).floor() as usize;
    (0..count).map(|n| first + n as f32 * period).chain([at]).collect()
}

/// The canvas's shaders and targets, for the preview to draw.
fn program_change(files: &CanvasFiles) -> CanvasChange {
    let (width, height) = render_size(files.canvas.scale);
    let passes = files.canvas.passes.iter().zip(&files.passes).map(|(pass, fragment)| PreviewPass { fragment: fragment.clone(), writes: pass.writes.clone() }).collect();
    let targets = files
        .canvas
        .targets
        .iter()
        .map(|target| {
            let (width, height) = render_size(target.scale);
            let format = match target.format {
                TargetFormat::Rgba8 => PreviewFormat::Rgba8,
                TargetFormat::Rgba16f => PreviewFormat::Rgba16f,
            };
            PreviewTarget { name: target.name.clone(), width, height, format, feedback: target.feedback }
        })
        .collect();
    CanvasChange::Program { fragment: files.fragment.clone(), width, height, passes, targets }
}

/// Compiles the editor's files and starts the canvas and its host half.
fn show(cx: &mut Session, files: &[SourceFile], values: &serde_json::Map<String, serde_json::Value>, network: Vec<NetworkHost>) -> Result<Running> {
    let compiled = compile(cx.slang()?, files)?;
    let sources = (compiled.options, compiled.host);
    let files = compiled.files;
    let script = load_script(cx, &files)?;
    (cx.out)(program_change(&files));
    let host_sources = Sources {
        label: "host.luau".into(),
        options: sources.0.map(str::to_owned),
        host: sources.1.map(str::to_owned),
        reader: None,
        message_limit: Some(MAX_SCRIPT_PAYLOAD_LEN),
        print: Some(cx.printer(LogSource::Host)),
        messages: files.messages.clone(),
        network,
        wake: Some(Arc::clone(&cx.wake)),
    };
    let host = HostSide::start(host_sources, &cx.services).unwrap_or_else(|e| {
        cx.log(LogSource::Host, LogLevel::Error, format!("{e:#}"));
        None
    });
    let fps = files.canvas.frame_rate.clamp(1, MAX_FPS);
    let now = Instant::now();
    let running = Running {
        files,
        script,
        values: BTreeMap::new(),
        host,
        clock: Clock::started(now),
        last: None,
        index: 0,
        started: false,
        frame: Duration::from_secs(1) / fps,
        next_frame: now,
        inputs_sent: None,
        resend: false,
    };
    apply_options(cx, &running, values);
    Ok(running)
}

/// `keyboard.luau`, ready for its first frame; a canvas without one has none.
fn load_script(cx: &Session, files: &CanvasFiles) -> Result<Option<CanvasScript>> {
    match &files.script {
        Some(source) => Ok(Some(CanvasScript::load(source, files.messages.as_deref(), &files.canvas.inputs, Some(cx.printer(LogSource::Keyboard)))?)),
        None => Ok(None),
    }
}

/// Starts the canvas's script over from its first frame, at time 0.
fn restart(cx: &Session, r: &mut Running, playing: bool) -> Result<()> {
    r.script = load_script(cx, &r.files)?;
    r.last = None;
    r.index = 0;
    r.started = false;
    let now = Instant::now();
    r.clock = Clock::started(now).played(playing, now);
    r.next_frame = now;
    Ok(())
}

/// 0RGB pixels as JPEG bytes, scaled down to at most `max_width`.
fn jpeg_bytes(pixels: &[u32], (width, height): (u32, u32), max_width: u32) -> Result<Vec<u8>> {
    let rgb: Vec<u8> = pixels.iter().flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8]).collect();
    let mut image = RgbImage::from_raw(width, height, rgb).context("the frame isn't its size")?;
    if width > max_width {
        image = image::imageops::thumbnail(&image, max_width, height * max_width / width);
    }
    let mut bytes = Vec::new();
    JpegEncoder::new_with_quality(&mut bytes, JPEG_QUALITY).encode_image(&image).context("encoding a frame")?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_clock_holds_while_paused_and_carries_on_after() {
        let start = Instant::now();
        let second = |n: u64| start + Duration::from_secs(n);
        let clock = Clock::started(start);
        assert_eq!(clock.at(second(2)), 2.0);
        let paused = clock.played(false, second(2));
        assert_eq!(paused.at(second(10)), 2.0);
        let resumed = paused.played(true, second(10));
        assert_eq!(resumed.at(second(13)), 5.0);
        assert_eq!(resumed.played(true, second(20)).at(second(20)), 12.0, "playing again changes nothing");
    }

    #[test]
    fn the_new_canvas_template_compiles_and_draws_a_frame() {
        let Some(native) = std::env::var_os("GLASSDECK_NATIVE").map(PathBuf::from) else {
            eprintln!("skipped: set GLASSDECK_NATIVE to the native libraries' folder");
            return;
        };
        let seen = Rc::new(std::cell::RefCell::new(Vec::new()));
        let sink = Rc::clone(&seen);
        let mut cx = Session {
            out: Rc::new(move |update| sink.borrow_mut().push(update)),
            native: Some(native),
            slang: None,
            services: Services::new(Arc::new(Mutex::new(None))),
            wake: Arc::new(|| {}),
        };
        let mut running = show(&mut cx, &crate::backgrounds::canvas_template(), &serde_json::Map::new(), Vec::new()).unwrap();
        step(&cx, &mut running, true).unwrap();
        let seen = seen.borrow();
        let program = seen.iter().position(|u| matches!(u, CanvasChange::Program { fragment, .. } if fragment.starts_with("#version 300 es")));
        let frame = seen.iter().position(|u| matches!(u, CanvasChange::Frame { index: 0, .. }));
        assert!(program.is_some() && frame.is_some() && program < frame, "the shader, then the first frame: {seen:?}");
        let Some(CanvasChange::Frame { inputs, .. }) = frame.map(|i| &seen[i]) else { unreachable!("found above") };
        assert!(!inputs.is_empty(), "the first frame writes the inputs the gradient's script sets");
    }

    /// e2e/canvas.spec.ts draws this fixture in WebGL2; it must stay the shader the gradient packs.
    #[test]
    fn the_webgl_fixture_is_the_packed_gradient_shader() {
        let deck = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../crates/glassdeck-gpu/builtin/gradient.deck");
        let packed = glassdeck_package::Package::open(&deck).unwrap().bytes("shader.essl").unwrap();
        let fixture = include_bytes!("../../e2e/fixtures/gradient.essl");
        assert!(packed == fixture, "copy shader.essl out of builtin/gradient.deck into apps/desktop/e2e/fixtures/gradient.essl");
    }

    #[test]
    fn stills_are_scaled_down_to_their_width() {
        let pixels = vec![0x00ff_8000u32; 1920 * 1080];
        let decoded = image::load_from_memory(&jpeg_bytes(&pixels, (1920, 1080), STILL_WIDTH).unwrap()).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (STILL_WIDTH, 270));
    }
}
