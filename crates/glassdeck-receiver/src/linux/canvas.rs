//! glassdeck-gpu: the built-in canvas, drawn whenever nothing else owns the screen, and canvas backgrounds.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};

use super::compositor::Registrar;
use super::renderer::RendererProcess;
use crate::PanelRotation;
use crate::mailbox::Mailbox;
use crate::upstream::Upstream;

const RENDERER: &str = "glassdeck-gpu";

/// glassdeck-gpu turns what it draws itself: the video plane can't turn on this display controller.
fn rotation_arg(registrar: &Registrar) -> String {
    let degrees = match registrar.rotation() {
        PanelRotation::Upright => 0,
        PanelRotation::Turned180 => 180,
    };
    format!("--panel-rotation={degrees}")
}

pub struct FallbackCanvas(RendererProcess);

impl FallbackCanvas {
    pub fn start(registrar: Registrar, mailbox: Arc<Mailbox>, stopped: impl FnOnce(anyhow::Error) + Send + 'static) -> Result<Self> {
        let program = std::env::current_exe().context("finding the receiver")?.with_file_name(RENDERER);
        let args = [rotation_arg(&registrar)];
        RendererProcess::start(RENDERER, &program.to_string_lossy(), &args, registrar, mailbox, None, stopped).map(Self)
    }

    pub fn stop(self) {
        self.0.stop();
    }
}

/// A canvas background (a background `.deck` with canvas content), drawn by glassdeck-gpu from its shader and script.
pub struct CanvasBackground(RendererProcess);

impl CanvasBackground {
    pub fn start(
        background: &Path,
        registrar: Registrar,
        mailbox: Arc<Mailbox>,
        upstream: Arc<Upstream>,
        stopped: impl FnOnce(anyhow::Error) + Send + 'static,
    ) -> Result<Self> {
        println!("Local background {}: canvas", background.display());
        let program = std::env::current_exe().context("finding the receiver")?.with_file_name(RENDERER);
        let args = [format!("--canvas={}", background.display()), rotation_arg(&registrar)];
        RendererProcess::start(RENDERER, &program.to_string_lossy(), &args, registrar, mailbox, Some(upstream), stopped).map(Self)
    }

    /// What the canvas's host half sent, for its script.
    pub fn deliver(&self, payload: &[u8]) -> Result<()> {
        self.0.deliver(payload)
    }

    /// A message from the canvas's host half, for its script.
    pub fn deliver_message(&self, message: u16, payload: &[u8]) -> Result<()> {
        self.0.deliver_message(message, payload)
    }

    pub fn stop(self) {
        self.0.stop();
    }
}

/// An image background, drawn once by glassdeck-gpu and left on screen.
pub struct ImageBackground(RendererProcess);

impl ImageBackground {
    pub fn start(
        background: &Path,
        registrar: Registrar,
        mailbox: Arc<Mailbox>,
        stopped: impl FnOnce(anyhow::Error) + Send + 'static,
    ) -> Result<Self> {
        println!("Local background {}: image", background.display());
        let program = std::env::current_exe().context("finding the receiver")?.with_file_name(RENDERER);
        let args = [format!("--image={}", background.display()), rotation_arg(&registrar)];
        RendererProcess::start(RENDERER, &program.to_string_lossy(), &args, registrar, mailbox, None, stopped).map(Self)
    }

    pub fn stop(self) {
        self.0.stop();
    }
}
