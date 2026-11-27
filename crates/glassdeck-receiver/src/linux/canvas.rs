//! glassdeck-gpu: the built-in canvas, drawn whenever nothing else owns the screen, and canvas backgrounds.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};

use super::compositor::Registrar;
use super::renderer::RendererProcess;
use crate::mailbox::Mailbox;

const RENDERER: &str = "glassdeck-gpu";

pub struct FallbackCanvas(RendererProcess);

impl FallbackCanvas {
    pub fn start(registrar: Registrar, mailbox: Arc<Mailbox>, stopped: impl FnOnce(anyhow::Error) + Send + 'static) -> Result<Self> {
        let program = std::env::current_exe().context("finding the receiver")?.with_file_name(RENDERER);
        RendererProcess::start(RENDERER, &program.to_string_lossy(), &[], registrar, mailbox, stopped).map(Self)
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
        stopped: impl FnOnce(anyhow::Error) + Send + 'static,
    ) -> Result<Self> {
        println!("Local background {}: canvas", background.display());
        let program = std::env::current_exe().context("finding the receiver")?.with_file_name(RENDERER);
        let args = [format!("--canvas={}", background.display())];
        RendererProcess::start(RENDERER, &program.to_string_lossy(), &args, registrar, mailbox, stopped).map(Self)
    }

    /// What the canvas's host half sent, for its script.
    pub fn deliver(&self, payload: &[u8]) -> Result<()> {
        self.0.deliver(payload)
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
        let args = [format!("--image={}", background.display())];
        RendererProcess::start(RENDERER, &program.to_string_lossy(), &args, registrar, mailbox, stopped).map(Self)
    }

    pub fn stop(self) {
        self.0.stop();
    }
}
