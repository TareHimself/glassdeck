//! The connection back to the host, shared by everything on the keyboard that answers it or sends to it.

use std::io::Write;
use std::net::TcpStream;
use std::sync::Mutex;

use anyhow::{Context, Result};
use glassdeck_proto::{Kind, Message, ScriptTarget};

/// Writers on different threads share one lock, so their messages never interleave on the socket.
#[derive(Default)]
pub struct Upstream {
    socket: Mutex<Option<TcpStream>>,
}

impl Upstream {
    pub fn connected(&self, socket: TcpStream) {
        *self.lock() = Some(socket);
    }

    pub fn disconnected(&self) {
        *self.lock() = None;
    }

    /// An answer the host is waiting on; fails with no host connected.
    pub fn send(&self, message: &Message) -> Result<()> {
        let mut socket = self.lock();
        let socket = socket.as_mut().context("no host is connected")?;
        socket.write_all(&message.encode()).context("writing to the host")
    }

    /// A keyboard script's `HostEvent`. With no host connected there's nobody to tell, so it's dropped,
    /// as glassdeckd drops events while the keyboard is away.
    pub fn script_message(&self, from: ScriptTarget, message: u16, payload: Vec<u8>) {
        let mut socket = self.lock();
        let Some(socket) = socket.as_mut() else { return };
        let message = Message::ScriptMessage { to: from, kind: Kind::HostEvent, message, payload };
        if let Err(e) = socket.write_all(&message.encode()) {
            eprintln!("error: sending a script message to the host: {e}");
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<TcpStream>> {
        self.socket.lock().expect("upstream poisoned")
    }
}
