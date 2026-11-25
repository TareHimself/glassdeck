//! The built-in scene glassdeck-scene draws whenever nothing else owns the screen.

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
