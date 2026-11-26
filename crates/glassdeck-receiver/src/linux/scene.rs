//! glassdeck-scene: the built-in scene, drawn whenever nothing else owns the screen, and scene backgrounds.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};

use super::compositor::Registrar;
use super::renderer::RendererProcess;
use crate::mailbox::Mailbox;

const RENDERER: &str = "glassdeck-scene";

pub struct FallbackScene(RendererProcess);

impl FallbackScene {
    pub fn start(registrar: Registrar, mailbox: Arc<Mailbox>, stopped: impl FnOnce(anyhow::Error) + Send + 'static) -> Result<Self> {
        let program = std::env::current_exe().context("finding the receiver")?.with_file_name(RENDERER);
        RendererProcess::start(RENDERER, &program.to_string_lossy(), &[], registrar, mailbox, stopped).map(Self)
    }

    pub fn stop(self) {
        self.0.stop();
    }
}

/// A scene background (.deckbg with scene content), drawn by glassdeck-scene from its plan, shader and script.
pub struct SceneBackground(RendererProcess);

impl SceneBackground {
    pub fn start(
        background: &Path,
        registrar: Registrar,
        mailbox: Arc<Mailbox>,
        stopped: impl FnOnce(anyhow::Error) + Send + 'static,
    ) -> Result<Self> {
        println!("Local background {}: scene", background.display());
        let program = std::env::current_exe().context("finding the receiver")?.with_file_name(RENDERER);
        let args = [format!("--scene={}", background.display())];
        RendererProcess::start(RENDERER, &program.to_string_lossy(), &args, registrar, mailbox, stopped).map(Self)
    }

    pub fn stop(self) {
        self.0.stop();
    }
}