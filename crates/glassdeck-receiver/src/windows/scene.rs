//! Scenes in the preview: the same shader and script the keyboard's glassdeck-scene runs, drawn on
//! the PC's GPU on a thread of its own and read back into the preview window.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::Result;
use glassdeck_package::{Background, SceneFiles};
use glassdeck_proto::unix_micros;
use glassdeck_scene::offscreen::Offscreen;
use glassdeck_scene::program::{SceneProgram, render_size};

use super::Registrar;
use super::decode::Picture;
use crate::mailbox::{Mailbox, ShownFrame, Timing};

/// The built-in gradient, shown whenever nothing else owns the screen.
pub struct FallbackScene(PreviewScene);

impl FallbackScene {
    pub fn start(_registrar: Registrar, mailbox: Arc<Mailbox>, stopped: impl FnOnce(anyhow::Error) + Send + 'static) -> Result<Self> {
        PreviewScene::start("built-in scene", SceneFiles::from_bytes(glassdeck_scene::BUILT_IN)?, mailbox, stopped).map(Self)
    }

    pub fn stop(self) {
        self.0.stop();
    }
}

/// A scene background (a `.deckbg` with scene content).
pub struct SceneBackground(PreviewScene);

impl SceneBackground {
    pub fn start(
        background: &Path,
        _registrar: Registrar,
        mailbox: Arc<Mailbox>,
        stopped: impl FnOnce(anyhow::Error) + Send + 'static,
    ) -> Result<Self> {
        println!("Local background {}: scene, drawn on this PC", background.display());
        PreviewScene::start("scene background", SceneFiles::read(&Background::open(background)?)?, mailbox, stopped).map(Self)
    }

    pub fn stop(self) {
        self.0.stop();
    }
}

struct PreviewScene {
    stop: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}

impl PreviewScene {
    /// `stopped` runs if the scene fails by itself, never after `stop`.
    fn start(
        label: &'static str,
        files: SceneFiles,
        mailbox: Arc<Mailbox>,
        stopped: impl FnOnce(anyhow::Error) + Send + 'static,
    ) -> Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let asked = Arc::clone(&stop);
        let thread = thread::Builder::new().name(label.into()).spawn(move || {
            let error = match run(label, files, &mailbox, &asked) {
                Ok(()) => return,
                Err(e) => e,
            };
            if !asked.load(Ordering::Relaxed) {
                stopped(error);
            }
        })?;
        Ok(Self { stop, thread })
    }

    fn stop(self) {
        self.stop.store(true, Ordering::Relaxed);
        if self.thread.join().is_err() {
            eprintln!("error: the preview scene thread panicked");
        }
    }
}

/// Draws at the scene's frame rate until asked to stop; frames where nothing changed aren't sent.
fn run(label: &str, files: SceneFiles, mailbox: &Mailbox, stop: &AtomicBool) -> Result<()> {
    let size = render_size(files.plan.scale);
    let period = Duration::from_secs_f64(1.0 / f64::from(files.plan.frame_rate));
    let offscreen = Offscreen::new(size)?;
    let mut scene = SceneProgram::new(&offscreen.gl, files)?;
    println!("Preview {label}: {} at {}x{}", offscreen.renderer_name(), size.0, size.1);
    let started = Instant::now();
    let mut due = started;
    while !stop.load(Ordering::Relaxed) {
        if scene.draw(&offscreen.gl, offscreen.framebuffer(), size, started.elapsed().as_secs_f32())?.is_some() {
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
