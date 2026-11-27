use std::fmt;
use std::io::Write;
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use glassdeck_proto::{Message, ProtoError, VERSION, unix_micros};

mod simulated;

use crate::clock::{ClockSample, best_sample};
use simulated::SimulatedLink;

const CLOCK_PROBES: usize = 8;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const REPLY_TIMEOUT: Duration = Duration::from_secs(2);

/// The receiver speaks another version of the protocol, so nothing it sends can be read.
#[derive(Debug)]
pub struct IncompatibleReceiver {
    pub version: u8,
}

impl fmt::Display for IncompatibleReceiver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "the receiver speaks protocol version {}, and this sender speaks {VERSION}", self.version)
    }
}

impl std::error::Error for IncompatibleReceiver {}

/// The sender's end of the link: the protocol over a socket, optionally throttled like USB 2.0. A
/// reader thread takes everything the receiver sends: clock replies come back to `sync_clock`, and
/// anything else goes to the `on_message` given to `connect`.
pub struct Link {
    socket: TcpStream,
    simulated: Option<SimulatedLink>,
    clock_replies: Receiver<Message>,
}

impl Link {
    pub fn connect(to: SocketAddr, link_bytes_per_sec: Option<u64>, on_message: impl Fn(Message) + Send + 'static) -> Result<Self> {
        let socket = TcpStream::connect_timeout(&to, CONNECT_TIMEOUT).with_context(|| format!("connecting to receiver at {to}"))?;
        socket.set_nodelay(true)?;
        let mut reading = socket.try_clone()?;
        on_message(first_message(&mut reading)?);
        let (replies, clock_replies) = mpsc::channel();
        thread::Builder::new().name("link-read".into()).spawn(move || read_loop(reading, &replies, &on_message))?;
        let simulated = link_bytes_per_sec.map(|rate| SimulatedLink::new(rate, Instant::now()));
        Ok(Self { socket, simulated, clock_replies })
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

    /// Probes the receiver's clock before a live stream; the result goes in `Mode::Live`.
    pub fn sync_clock(&mut self) -> Result<ClockSample> {
        let mut samples = Vec::with_capacity(CLOCK_PROBES);
        for _ in 0..CLOCK_PROBES {
            let sent_us = unix_micros();
            self.socket.write_all(&Message::ClockProbe { sent_us }.encode()).context("sending a clock probe")?;
            let reply = self.clock_replies.recv_timeout(REPLY_TIMEOUT);
            let returned_us = unix_micros();
            match reply {
                Ok(Message::ClockReply { probe_sent_us, received_us }) if probe_sent_us == sent_us => {
                    samples.push(ClockSample { sent_us, received_us, returned_us });
                }
                Ok(other) => bail!("expected the reply to probe {sent_us}, got {other:?}"),
                Err(RecvTimeoutError::Timeout) => bail!("no clock reply within {REPLY_TIMEOUT:?}"),
                Err(RecvTimeoutError::Disconnected) => bail!("receiver closed the connection during clock sync"),
            }
        }
        Ok(best_sample(&samples).expect("at least one probe"))
    }
}

/// The reader thread holds its own handle to the socket, so dropping the Link must close the
/// connection itself; otherwise the receiver would keep serving a connection nobody writes to.
impl Drop for Link {
    fn drop(&mut self) {
        let _ = self.socket.shutdown(Shutdown::Both);
    }
}

/// The receiver says what it holds as soon as it accepts, so its first message also shows which
/// version of the protocol it speaks.
fn first_message(socket: &mut TcpStream) -> Result<Message> {
    socket.set_read_timeout(Some(REPLY_TIMEOUT))?;
    let first = Message::read_from(socket);
    socket.set_read_timeout(None)?;
    match first {
        Ok(Some(message)) => Ok(message),
        Ok(None) => bail!("the receiver closed the connection before saying what it holds"),
        Err(ProtoError::UnsupportedVersion(version)) => Err(IncompatibleReceiver { version }.into()),
        Err(e) => Err(anyhow::Error::new(e).context("reading what the receiver holds")),
    }
}

/// Ends when the connection does. A message the receiver shouldn't send closes the socket, so the
/// sending side fails too and the owner reconnects.
fn read_loop(mut socket: TcpStream, clock_replies: &mpsc::Sender<Message>, on_message: &impl Fn(Message)) {
    loop {
        match Message::read_from(&mut socket) {
            Ok(Some(reply @ Message::ClockReply { .. })) => {
                if clock_replies.send(reply).is_err() {
                    return;
                }
            }
            Ok(Some(message)) => on_message(message),
            Ok(None) => return,
            Err(e) => {
                eprintln!("Link: reading from the receiver failed ({e}); closing the connection");
                let _ = socket.shutdown(Shutdown::Both);
                return;
            }
        }
    }
}

pub fn new_stream_id() -> u32 {
    unix_micros() as u32
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::net::TcpListener;

    use super::*;

    /// A receiver that accepts once and says what it holds, in the protocol `version`.
    fn receiver_speaking(version: u8) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let at = listener.local_addr().unwrap();
        thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut inventory = Message::Inventory { packages: Vec::new() }.encode();
            inventory[0] = version;
            socket.write_all(&inventory).unwrap();
            thread::sleep(Duration::from_millis(200));
        });
        at
    }

    #[test]
    fn an_older_receiver_is_named_as_incompatible() {
        let error = Link::connect(receiver_speaking(VERSION - 1), None, |_| {}).err().expect("refused");
        let incompatible = error.downcast_ref::<IncompatibleReceiver>().expect("an IncompatibleReceiver");
        assert_eq!(incompatible.version, VERSION - 1);
    }

    #[test]
    fn a_matching_receiver_hands_over_what_it_holds() {
        let (seen, saw) = mpsc::channel();
        let link = Link::connect(receiver_speaking(VERSION), None, move |m| seen.send(m).unwrap()).unwrap();
        assert!(matches!(saw.recv().unwrap(), Message::Inventory { .. }));
        drop(link);
    }
}
