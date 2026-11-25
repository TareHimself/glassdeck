//! A source worker: glassdeckd starts itself as `glassdeckd source` for each source, because CEF can
//! start only once per process. The worker reads its request on stdin and streams to glassdeckd over
//! a localhost socket; any byte (or the socket closing) from glassdeckd asks it to stop.

use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::thread;

use anyhow::{Context, Result};
use glassdeck_capture::pipeline::{self, StreamOut, request_stop};
use glassdeck_capture::sources::{self, SourceRequest};
use glassdeck_proto::Message;

pub fn run(uplink: SocketAddr, token: &str) -> Result<()> {
    let request: SourceRequest = serde_json::from_reader(std::io::stdin()).context("reading the source request")?;
    let mut socket = TcpStream::connect(uplink).with_context(|| format!("connecting to glassdeckd at {uplink}"))?;
    socket.set_nodelay(true)?;
    socket.write_all(handshake(token).as_bytes())?;
    let mut stop = socket.try_clone()?;
    thread::Builder::new().name("stop".into()).spawn(move || {
        let _ = stop.read(&mut [0u8; 1]);
        request_stop();
    })?;
    let ending = socket.try_clone()?;
    let result = sources::build(request).and_then(|source| pipeline::run(source, Box::new(SocketOut(socket))));
    // A clean close, so glassdeckd reads the end of the stream rather than a reset.
    let _ = ending.shutdown(Shutdown::Write);
    result
}

/// What a worker sends first, so only the worker glassdeckd started can feed the Link.
pub fn handshake(token: &str) -> String {
    format!("{token}\n")
}

struct SocketOut(TcpStream);

impl StreamOut for SocketOut {
    fn send(&mut self, message: &Message) -> Result<()> {
        self.0.write_all(&message.encode()).context("sending to glassdeckd")
    }
}
