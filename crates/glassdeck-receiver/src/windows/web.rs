use std::path::Path;
use std::sync::Arc;

use anyhow::{Result, bail};

use crate::mailbox::Mailbox;

/// The window has no shared buffers to register; it exists so both platforms build the same director.
#[derive(Clone)]
pub struct Registrar;

/// Web backgrounds render on the keyboard; on the PC, `glassdeck play` renders them with CEF instead.
pub enum WebBackground {}

impl WebBackground {
    pub fn start(
        _background: &Path,
        _registrar: Registrar,
        _mailbox: Arc<Mailbox>,
        _stopped: impl FnOnce(anyhow::Error) + Send + 'static,
    ) -> Result<Self> {
        bail!("web backgrounds play only on the keyboard; stream them from the host with `glassdeck play`")
    }

    pub fn stop(self) {
        match self {}
    }
}
