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

use anyhow::{Context, Result};
use base64::Engine;
use glassdeck_gpu::offscreen::Offscreen;
use glassdeck_gpu::program::{CanvasProgram, render_size};
use glassdeck_gpu::script::InputValue;
use glassdeck_host::{HostSide, SharedNowPlaying, SharedSystem, Sources};
use glassdeck_luau::Print;
use glassdeck_luau::options::read_values;
use glassdeck_package::{CanvasFiles, HOST_SCRIPT, InputField, KEYBOARD_SCRIPT, OPTIONS_SCRIPT, Origin, is_blank_script};
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

/// The preview is shown at most this wide, so frames stay small enough to send many a second.
const PREVIEW_WIDTH: u32 = 960;
/// The fastest the preview draws, whatever the canvas asks for.
const MAX_FPS: u32 = 30;
const JPEG_QUALITY: u8 = 82;
const INPUTS_EVERY: Duration = Duration::from_millis(200);
const IDLE: Duration = Duration::from_secs(1);

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
    Frame { jpeg: String, time: f32 },
    /// Nothing to show: the canvas didn't compile, or stopped on an error
    Cleared,
    Inputs { inputs: Vec<InputReading> },
    Log { source: LogSource, level: LogLevel, text: String },
}

pub enum Command {
    Show { run: PreviewRun, files: Vec<SourceFile>, values: serde_json::Map<String, serde_json::Value> },
    Options(serde_json::Map<String, serde_json::Value>),
    Play(bool),
    Restart,
    Stop,
}

pub struct CanvasPreview(Mutex<Sender<Command>>);

impl CanvasPreview {
    pub fn start(app: AppHandle, native: Option<PathBuf>) -> Self {
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("canvas preview".into())
            .spawn(move || run(app, native, rx))
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
    system: SharedSystem,
    now_playing: SharedNowPlaying,
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

struct Running {
    files: CanvasFiles,
    program: CanvasProgram,
    offscreen: Offscreen,
    host: Option<HostSide>,
    clock: Clock,
    frame: Duration,
    next_frame: Instant,
    inputs_sent: Option<Instant>,
}

fn run(app: AppHandle, native: Option<PathBuf>, rx: Receiver<Command>) {
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
    let mut cx = Session { out, native, slang: None, system: glassdeck_host::system(), now_playing };
    let mut running: Option<Running> = None;
    let mut playing = true;
    loop {
        let wait = running.as_ref().map_or(IDLE, |r| wait_for(r, playing, Instant::now()));
        match rx.recv_timeout(wait) {
            Ok(Command::Show { run, files, values }) => {
                current.set(run);
                let clock = running.take().map(|r| r.clock);
                running = match show(&mut cx, &files, &values) {
                    Ok(mut started) => {
                        if let Some(clock) = clock {
                            started.clock = clock;
                        }
                        started.clock = started.clock.played(playing, Instant::now());
                        Some(started)
                    }
                    Err(e) => {
                        cx.log(LogSource::Pack, LogLevel::Error, format!("{e:#}"));
                        (cx.out)(CanvasChange::Cleared);
                        None
                    }
                };
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
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        if let Some(r) = &mut running
            && let Err(e) = step(&cx, r, playing)
        {
            cx.log(LogSource::Keyboard, LogLevel::Error, format!("{e:#}"));
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
        if let Err(e) = host.run_timers(now) {
            cx.log(LogSource::Host, LogLevel::Error, format!("{e:#}; host.luau stopped"));
            r.host = None;
        } else if let Some(payload) = host.take_sent() {
            r.program.deliver(&payload)?;
            r.next_frame = now;
        }
    }
    if now < r.next_frame {
        return Ok(());
    }
    r.next_frame = now + r.frame;
    let time = r.clock.at(now);
    let size = r.offscreen.size();
    if r.program.draw(&r.offscreen.gl, r.offscreen.framebuffer(), size, time)?.is_some() {
        (cx.out)(CanvasChange::Frame { jpeg: jpeg_of(&r.offscreen.read_rgb(), size)?, time });
    }
    if r.inputs_sent.is_none_or(|at| now.saturating_duration_since(at) >= INPUTS_EVERY) {
        r.inputs_sent = Some(now);
        (cx.out)(CanvasChange::Inputs { inputs: readings(&r.program.canvas.inputs, r.program.inputs()) });
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

/// Compiles the editor's files and starts the canvas and its host half.
fn show(cx: &mut Session, files: &[SourceFile], values: &serde_json::Map<String, serde_json::Value>) -> Result<Running> {
    let shader = files.iter().find(|f| f.name == CANVAS_SOURCE).map(|f| f.text.as_str()).with_context(|| format!("there's no {CANVAS_SOURCE}"))?;
    let sources = CanvasSources {
        shader,
        path: CANVAS_SOURCE,
        keyboard: script(files, KEYBOARD_SCRIPT),
        options: script(files, OPTIONS_SCRIPT),
        host: script(files, HOST_SCRIPT),
    };
    let compiled = compile_canvas(cx.slang()?, &sources)?;
    let files = CanvasFiles {
        origin: Origin::Packed { from: "preview".into(), sources: String::new() },
        canvas: compiled.canvas,
        fragment: compiled.fragment,
        script: sources.keyboard.map(str::to_owned),
    };
    let offscreen = Offscreen::new(render_size(files.canvas.scale))?;
    let program = CanvasProgram::new(&offscreen.gl, files.clone(), Some(cx.printer(LogSource::Keyboard)))?;
    let host_sources = Sources {
        label: "host.luau".into(),
        options: sources.options.map(str::to_owned),
        host: sources.host.map(str::to_owned),
        reader: None,
        send_limit: None,
        print: Some(cx.printer(LogSource::Host)),
    };
    let host = HostSide::start(host_sources, &cx.system, &cx.now_playing).unwrap_or_else(|e| {
        cx.log(LogSource::Host, LogLevel::Error, format!("{e:#}"));
        None
    });
    let fps = files.canvas.frame_rate.clamp(1, MAX_FPS);
    let now = Instant::now();
    let running = Running {
        files,
        program,
        offscreen,
        host,
        clock: Clock::started(now),
        frame: Duration::from_secs(1) / fps,
        next_frame: now,
        inputs_sent: None,
    };
    apply_options(cx, &running, values);
    Ok(running)
}

/// Starts the canvas's script over from its first frame, at time 0.
fn restart(cx: &Session, r: &mut Running, playing: bool) -> Result<()> {
    r.program = CanvasProgram::new(&r.offscreen.gl, r.files.clone(), Some(cx.printer(LogSource::Keyboard)))?;
    let now = Instant::now();
    r.clock = Clock::started(now).played(playing, now);
    r.next_frame = now;
    Ok(())
}

/// 0RGB pixels to a base64 JPEG, scaled down to the preview's width.
fn jpeg_of(pixels: &[u32], (width, height): (u32, u32)) -> Result<String> {
    let rgb: Vec<u8> = pixels.iter().flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8]).collect();
    let mut image = RgbImage::from_raw(width, height, rgb).context("the frame isn't its size")?;
    if width > PREVIEW_WIDTH {
        image = image::imageops::thumbnail(&image, PREVIEW_WIDTH, height * PREVIEW_WIDTH / width);
    }
    let mut bytes = Vec::new();
    JpegEncoder::new_with_quality(&mut bytes, JPEG_QUALITY).encode_image(&image).context("encoding a frame")?;
    Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
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
            system: glassdeck_host::system(),
            now_playing: Arc::new(Mutex::new(None)),
        };
        let mut running = show(&mut cx, &crate::backgrounds::canvas_template(), &serde_json::Map::new()).unwrap();
        step(&cx, &mut running, true).unwrap();
        let seen = seen.borrow();
        assert!(seen.iter().any(|u| matches!(u, CanvasChange::Frame { .. })), "a frame: {seen:?}");
    }

    #[test]
    fn frames_are_scaled_down_to_the_preview_width() {
        let pixels = vec![0x00ff_8000u32; 1920 * 1080];
        let jpeg = base64::engine::general_purpose::STANDARD.decode(jpeg_of(&pixels, (1920, 1080)).unwrap()).unwrap();
        let decoded = image::load_from_memory(&jpeg).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (PREVIEW_WIDTH, 540));
    }
}
