use std::io::{self, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::Deserialize;

use crate::{Codec, Error, Frame, Hello, Mode, StreamFormat, ToHost, ToProgram};

/// What glassdeckd asks of an attached program.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Control {
    Activate,
    Deactivate,
    Keyframe,
}

/// The program's end of glassdeckd's requests.
pub struct Controls(TcpStream);

impl Controls {
    /// Blocks for the next request; `None` once glassdeckd closes the connection.
    pub fn recv(&mut self) -> Result<Option<Control>, Error> {
        Ok(match ToProgram::read_from(&mut self.0)? {
            None => None,
            Some(ToProgram::Activate) => Some(Control::Activate),
            Some(ToProgram::Deactivate) => Some(Control::Deactivate),
            Some(ToProgram::Keyframe) => Some(Control::Keyframe),
            Some(ToProgram::Welcome | ToProgram::Refused { .. }) => return Err(Error::OutOfTurn("Welcome or Refused after attaching")),
        })
    }
}

/// Sends the stream; clones share one connection, so any thread may send.
#[derive(Clone)]
pub struct Sender(Arc<Mutex<TcpStream>>);

impl Sender {
    pub fn send(&self, message: &ToHost) -> io::Result<()> {
        self.0.lock().expect("sender poisoned").write_all(&message.encode())
    }

    pub fn still(&self, jpeg: Vec<u8>) -> io::Result<()> {
        self.send(&ToHost::Still { jpeg })
    }

    pub fn start(&self, format: StreamFormat) -> io::Result<()> {
        self.send(&ToHost::StreamStart(format))
    }

    pub fn frame(&self, frame: Frame) -> io::Result<()> {
        self.send(&ToHost::Frame(frame))
    }

    pub fn end(&self) -> io::Result<()> {
        self.send(&ToHost::StreamEnd)
    }
}

/// Attaches `name` to the glassdeckd at `address`.
pub fn attach(address: SocketAddr, token: &str, name: &str, mode: Mode, codec: Codec) -> Result<(Controls, Sender), Error> {
    let mut socket = TcpStream::connect(address)?;
    socket.set_nodelay(true)?;
    socket.write_all(&ToHost::Hello(Hello { token: token.to_owned(), name: name.to_owned(), mode, codec }).encode())?;
    match ToProgram::read_from(&mut socket)? {
        Some(ToProgram::Welcome) => {}
        Some(ToProgram::Refused { reason }) => return Err(Error::Refused(reason)),
        Some(_) => return Err(Error::OutOfTurn("a request before Welcome")),
        None => return Err(io::Error::from(io::ErrorKind::UnexpectedEof).into()),
    }
    let sender = Sender(Arc::new(Mutex::new(socket.try_clone()?)));
    Ok((Controls(socket), sender))
}

#[derive(Deserialize)]
struct ApiFile {
    stream_port: u16,
    token: String,
}

/// Attaches `name` to this user's glassdeckd, found through its `api.json`.
pub fn attach_local(name: &str, mode: Mode, codec: Codec) -> Result<(Controls, Sender), Error> {
    let path = api_file()?;
    let text = std::fs::read(&path).map_err(|e| io::Error::new(e.kind(), format!("reading {}: {e}; is glassdeckd running?", path.display())))?;
    let api: ApiFile = serde_json::from_slice(&text).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("reading {}: {e}", path.display())))?;
    attach(SocketAddr::from((Ipv4Addr::LOCALHOST, api.stream_port)), &api.token, name, mode, codec)
}

fn api_file() -> io::Result<PathBuf> {
    let local = std::env::var_os("LOCALAPPDATA").ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "LOCALAPPDATA is not set"))?;
    Ok(PathBuf::from(local).join("Glassdeck").join("api.json"))
}
