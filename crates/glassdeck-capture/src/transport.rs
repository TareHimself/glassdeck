use std::io::Write;
use std::net::{SocketAddr, TcpStream};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use glassdeck_proto::{Message, unix_micros};

use crate::link::SimulatedLink;

pub struct Transport {
    socket: TcpStream,
    link: Option<SimulatedLink>,
}

impl Transport {
    pub fn connect(to: SocketAddr, link_bytes_per_sec: Option<u64>) -> Result<Self> {
        let socket = TcpStream::connect(to).with_context(|| format!("connecting to receiver at {to}"))?;
        socket.set_nodelay(true)?;
        let link = link_bytes_per_sec.map(|rate| SimulatedLink::new(rate, Instant::now()));
        Ok(Self { socket, link })
    }

    /// Sends one message, first waiting as long as the simulated link would take. Returns that wait.
    pub fn send(&mut self, message: &Message) -> Result<Duration> {
        let bytes = message.encode();
        let wait = match self.link {
            Some(link) => {
                let (next, done) = link.transfer(bytes.len(), Instant::now());
                self.link = Some(next);
                let wait = done.saturating_duration_since(Instant::now());
                thread::sleep(wait);
                wait
            }
            None => Duration::ZERO,
        };
        self.socket.write_all(&bytes).context("sending to receiver")?;
        Ok(wait)
    }
}

pub fn new_stream_id() -> u32 {
    unix_micros() as u32
}
