//! Web backgrounds, rendered by glassdeck-local-web.

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;

use super::compositor::Registrar;
use super::renderer::RendererProcess;
use crate::mailbox::Mailbox;

const RENDERER: &str = "glassdeck-local-web";

pub struct WebBackground(RendererProcess);

impl WebBackground {
    pub fn start(
        background: &Path,
        registrar: Registrar,
        mailbox: Arc<Mailbox>,
        stopped: impl FnOnce(anyhow::Error) + Send + 'static,
    ) -> Result<Self> {
        println!("Local background {}: web", background.display());
        let args = [format!("--background={}", background.display())];
        RendererProcess::start(RENDERER, RENDERER, &args, registrar, mailbox, None, stopped).map(Self)
    }

    pub fn stop(self) {
        self.0.stop();
    }
}
