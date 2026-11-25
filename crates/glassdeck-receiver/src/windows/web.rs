use std::path::Path;
use std::sync::Arc;

use anyhow::{Result, bail};

use crate::mailbox::Mailbox;

/// The window has no shared buffers to register; it exists so both platforms build the same director.
#[derive(Clone)]
pub struct Registrar;

/// Web decks render on the keyboard; on the PC, `glassdeck-capture play` renders them with CEF instead.
pub enum WebDeck {}

impl WebDeck {
    pub fn start(
        _deck: &Path,
        _registrar: Registrar,
        _mailbox: Arc<Mailbox>,
        _stopped: impl FnOnce(anyhow::Error) + Send + 'static,
    ) -> Result<Self> {
        bail!("web decks play only on the keyboard; stream them from the PC with `glassdeck-capture play`")
    }

    pub fn stop(self) {
        match self {}
    }
}
