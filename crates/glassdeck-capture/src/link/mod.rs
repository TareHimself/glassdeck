use std::io::Write;
use std::net::{SocketAddr, TcpStream};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use glassdeck_proto::{Message, unix_micros};

mod simulated;

use crate::clock::{ClockSample, best_sample};
use simulated::SimulatedLink;

const CLOCK_PROBES: usize = 8;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

/// The sender's end of the link: the protocol over a socket, optionally throttled like USB 2.0.
pub struct Link {
    socket: TcpStream,
    simulated: Option<SimulatedLink>,
}

impl Link {
    pub fn connect(to: SocketAddr, link_bytes_per_sec: Option<u64>) -> Result<Self> {
        let socket = TcpStream::connect_timeout(&to, CONNECT_TIMEOUT).with_context(|| format!("connecting to receiver at {to}"))?;
        socket.set_nodelay(true)?;
        let simulated = link_bytes_per_sec.map(|rate| SimulatedLink::new(rate, Instant::now()));
        Ok(Self { socket, simulated })
    }

    /// Sends one message, first waiting as long as the simulated link would take. Returns that wait.
    pub fn send(&mut self, message: &Message) -> Result<Duration> {
        let bytes = message.encode();
        let wait = match self.simulated {
            Some(simulated) => {
                let (next, done) = simulated.transfer(bytes.len(), Instant::now());
                self.simulated = Some(next);
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

impl Link {
    /// Probes the receiver's clock before a live stream; the result goes in `Mode::Live`.
    pub fn sync_clock(&mut self) -> Result<ClockSample> {
        let mut samples = Vec::with_capacity(CLOCK_PROBES);
        for _ in 0..CLOCK_PROBES {
            let sent_us = unix_micros();
            self.socket.write_all(&Message::ClockProbe { sent_us }.encode()).context("sending a clock probe")?;
            let reply = Message::read_from(&mut self.socket).context("reading a clock reply")?;
            let returned_us = unix_micros();
            match reply {
                Some(Message::ClockReply { probe_sent_us, received_us }) if probe_sent_us == sent_us => {
                    samples.push(ClockSample { sent_us, received_us, returned_us });
                }
                Some(other) => bail!("expected a clock reply, got {other:?}"),
                None => bail!("receiver closed the connection during clock sync"),
            }
        }
        Ok(best_sample(&samples).expect("at least one probe"))
    }
}

pub fn new_stream_id() -> u32 {
    unix_micros() as u32
}
