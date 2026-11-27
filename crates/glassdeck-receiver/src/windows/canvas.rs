//! Canvases in the preview: the same shader and script the keyboard's glassdeck-gpu runs, drawn on
//! the PC's GPU on a thread of its own and read back into the preview window.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use glassdeck_package::{CanvasFiles, Package};
use glassdeck_proto::unix_micros;
use glassdeck_gpu::offscreen::Offscreen;
use glassdeck_gpu::program::{CanvasProgram, render_size};

use super::Registrar;
use super::decode::Picture;
use glassdeck_proto::ScriptTarget;

use crate::mailbox::{Mailbox, ShownFrame, Timing};
use crate::upstream::Upstream;

/// The built-in gradient, shown whenever nothing else owns the screen.
pub struct FallbackCanvas(PreviewCanvas);

impl FallbackCanvas {
    pub fn start(_registrar: Registrar, mailbox: Arc<Mailbox>, stopped: impl FnOnce(anyhow::Error) + Send + 'static) -> Result<Self> {
        PreviewCanvas::start("built-in canvas", glassdeck_gpu::built_in()?, mailbox, None, stopped).map(Self)
    }

    pub fn stop(self) {
        self.0.stop();
    }
}

/// A canvas background (a background `.deck` with canvas content).
pub struct CanvasBackground(PreviewCanvas);

impl CanvasBackground {
    pub fn start(
        background: &Path,
        _registrar: Registrar,
        mailbox: Arc<Mailbox>,
        upstream: Arc<Upstream>,
        stopped: impl FnOnce(anyhow::Error) + Send + 'static,
    ) -> Result<Self> {
        println!("Local background {}: canvas, drawn on this PC", background.display());
        let files = CanvasFiles::read(&Package::open(background)?)?;
        PreviewCanvas::start("canvas background", files, mailbox, Some(upstream), stopped).map(Self)
    }

    /// What the canvas's host half sent, for its script.
    pub fn deliver(&self, payload: &[u8]) -> Result<()> {
        self.0.send(ToCanvas::Data(payload.to_vec()))
    }

    /// A message from the canvas's host half, for its script.
    pub fn deliver_message(&self, message: u16, payload: &[u8]) -> Result<()> {
        self.0.send(ToCanvas::Message { message, payload: payload.to_vec() })
    }

    pub fn stop(self) {
        self.0.stop();
    }
}

/// What the host half sent, on its way to the canvas thread.
enum ToCanvas {
    Data(Vec<u8>),
    Message { message: u16, payload: Vec<u8> },
}

struct PreviewCanvas {
    stop: Arc<AtomicBool>,
    inbox: Sender<ToCanvas>,
    thread: JoinHandle<()>,
}

impl PreviewCanvas {
    /// `stopped` runs if the canvas fails by itself, never after `stop`.
    fn start(
        label: &'static str,
        files: CanvasFiles,
        mailbox: Arc<Mailbox>,
        upstream: Option<Arc<Upstream>>,
        stopped: impl FnOnce(anyhow::Error) + Send + 'static,
    ) -> Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let asked = Arc::clone(&stop);
        let (inbox, arriving) = mpsc::channel();
        let thread = thread::Builder::new().name(label.into()).spawn(move || {
            let error = match run(label, files, &mailbox, &asked, &arriving, upstream.as_deref()) {
                Ok(()) => return,
                Err(e) => e,
            };
            if !asked.load(Ordering::Relaxed) {
                stopped(error);
            }
        })?;
        Ok(Self { stop, inbox, thread })
    }

    fn send(&self, input: ToCanvas) -> Result<()> {
        self.inbox.send(input).map_err(|_| anyhow!("the preview canvas has stopped"))
    }

    fn stop(self) {
        self.stop.store(true, Ordering::Relaxed);
        if self.thread.join().is_err() {
            eprintln!("error: the preview canvas thread panicked");
        }
    }
}

/// Draws at the canvas's frame rate until asked to stop; frames where nothing changed aren't sent.
fn run(label: &str, files: CanvasFiles, mailbox: &Mailbox, stop: &AtomicBool, inbox: &Receiver<ToCanvas>, upstream: Option<&Upstream>) -> Result<()> {
    let size = render_size(files.canvas.scale);
    let period = Duration::from_secs_f64(1.0 / f64::from(files.canvas.frame_rate));
    let offscreen = Offscreen::new(size)?;
    let mut canvas = CanvasProgram::new(&offscreen.gl, files, None)?;
    println!("Preview {label}: {} at {}x{}", offscreen.renderer_name(), size.0, size.1);
    let started = Instant::now();
    let mut due = started;
    while !stop.load(Ordering::Relaxed) {
        while let Ok(input) = inbox.try_recv() {
            match input {
                ToCanvas::Data(payload) => canvas.deliver_options(&payload)?,
                ToCanvas::Message { message, payload } => canvas.deliver_message(message, &payload)?,
            }
        }
        for sent in canvas.take_messages() {
            if let Some(upstream) = upstream {
                upstream.script_message(ScriptTarget::Background, sent.message, sent.payload);
            }
        }
        if canvas.draw(&offscreen.gl, offscreen.framebuffer(), size, started.elapsed().as_secs_f32())?.is_some() {
            let picture = Picture { pixels: offscreen.read_rgb(), width: size.0 as usize, height: size.1 as usize, time_us: unix_micros() };
            mailbox.put(ShownFrame { picture, timing: Timing::Paced { late_us: 0 } });
        }
        due += period;
        let now = Instant::now();
        match due.checked_duration_since(now) {
            Some(wait) => thread::sleep(wait),
            None => due = now,
        }
    }
    Ok(())
}

/// An image background in the preview: its picture, shown once.
pub struct ImageBackground;

impl ImageBackground {
    pub fn start(
        background: &Path,
        _registrar: Registrar,
        mailbox: Arc<Mailbox>,
        _stopped: impl FnOnce(anyhow::Error) + Send + 'static,
    ) -> Result<Self> {
        println!("Local background {}: image", background.display());
        let picture = glassdeck_gpu::image::Picture::read(&Package::open(background)?)?;
        let pixels = picture.rgb.as_chunks::<3>().0.iter().map(|p| u32::from_be_bytes([0, p[0], p[1], p[2]])).collect();
        let picture = Picture { pixels, width: picture.width as usize, height: picture.height as usize, time_us: unix_micros() };
        mailbox.put(ShownFrame { picture, timing: Timing::Paced { late_us: 0 } });
        Ok(Self)
    }

    pub fn stop(self) {}
}
