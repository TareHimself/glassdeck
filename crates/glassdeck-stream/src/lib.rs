//! Glassdeck's stream protocol v1: how a program attaches a named video stream to glassdeckd.
//!
//! A program connects to `127.0.0.1:<stream_port>` (both in glassdeckd's `api.json`) and sends
//! [`Hello`]. glassdeckd answers `Welcome` or `Refused`, then `Activate` while anyone watches the
//! stream (the keyboard or a preview) and `Deactivate` once nobody does. Every message has an 8-byte
//! header: version, kind, two zero bytes, the body length as a little-endian `u32`.

mod client;

use std::fmt;
use std::io::{self, Read};

pub use client::{Control, Controls, Sender, attach, attach_local};

pub const VERSION: u8 = 1;
pub const HEADER_LEN: usize = 8;
pub const MAX_BODY_LEN: u32 = 32 << 20;
pub const MAX_NAME_BYTES: usize = 64;

const HELLO: u8 = 0x01;
const STILL: u8 = 0x02;
const STREAM_START: u8 = 0x03;
const FRAME: u8 = 0x04;
const STREAM_END: u8 = 0x05;
const WELCOME: u8 = 0x81;
const REFUSED: u8 = 0x82;
const ACTIVATE: u8 = 0x83;
const DEACTIVATE: u8 = 0x84;
const KEYFRAME: u8 = 0x85;

const FLAG_KEYFRAME: u8 = 1;
const STREAM_START_LEN: usize = 5;
const FRAME_FIXED_LEN: usize = 13;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Shown on arrival; `time_us` is the host's Unix time at capture
    Live,
    /// Shown at its presentation time; `time_us` counts from the stream's start
    Paced,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    H264,
    Hevc,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hello {
    pub token: String,
    pub name: String,
    pub mode: Mode,
    /// Every `StreamStart` on this connection uses it
    pub codec: Codec,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamFormat {
    pub codec: Codec,
    pub width: u16,
    pub height: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub seq: u32,
    pub time_us: u64,
    pub keyframe: bool,
    /// Annex B
    pub payload: Vec<u8>,
}

/// What a program sends glassdeckd.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToHost {
    Hello(Hello),
    /// A JPEG of what the stream shows, for previews before any video; send it again when it changes
    Still { jpeg: Vec<u8> },
    StreamStart(StreamFormat),
    Frame(Frame),
    StreamEnd,
}

/// What glassdeckd sends a program.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToProgram {
    Welcome,
    Refused { reason: String },
    /// Someone watches: start encoding, keyframe first
    Activate,
    /// Nobody watches: stop encoding and stay attached
    Deactivate,
    /// A new viewer: send a keyframe as soon as you can
    Keyframe,
}

#[derive(Debug)]
pub enum Error {
    UnsupportedVersion(u8),
    UnknownKind(u8),
    BodyTooLarge(u32),
    BadBody { kind: u8 },
    Refused(String),
    /// A message that's valid but not at this point, such as a second `Welcome`
    OutOfTurn(&'static str),
    Io(io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedVersion(v) => write!(f, "stream protocol version {v}, expected {VERSION}"),
            Self::UnknownKind(k) => write!(f, "unknown message kind {k:#04x}"),
            Self::BodyTooLarge(len) => write!(f, "a message body of {len} bytes, over {MAX_BODY_LEN}"),
            Self::BadBody { kind } => write!(f, "message kind {kind:#04x} has a malformed body"),
            Self::Refused(reason) => write!(f, "glassdeckd refused the stream: {reason}"),
            Self::OutOfTurn(what) => write!(f, "{what} out of turn"),
            Self::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl ToHost {
    pub fn encode(&self) -> Vec<u8> {
        let mut body = Vec::new();
        let kind = match self {
            Self::Hello(hello) => {
                put_text(&mut body, &hello.token);
                put_text(&mut body, &hello.name);
                body.push(match hello.mode {
                    Mode::Live => 0,
                    Mode::Paced => 1,
                });
                body.push(codec_byte(hello.codec));
                HELLO
            }
            Self::Still { jpeg } => {
                body.extend_from_slice(jpeg);
                STILL
            }
            Self::StreamStart(format) => {
                body.push(codec_byte(format.codec));
                body.extend_from_slice(&format.width.to_le_bytes());
                body.extend_from_slice(&format.height.to_le_bytes());
                STREAM_START
            }
            Self::Frame(frame) => {
                body.extend_from_slice(&frame.seq.to_le_bytes());
                body.extend_from_slice(&frame.time_us.to_le_bytes());
                body.push(if frame.keyframe { FLAG_KEYFRAME } else { 0 });
                body.extend_from_slice(&frame.payload);
                FRAME
            }
            Self::StreamEnd => STREAM_END,
        };
        framed(kind, body)
    }

    /// One message, or `None` at a clean end between messages.
    pub fn read_from(reader: &mut impl Read) -> Result<Option<Self>, Error> {
        let Some((kind, body)) = read_message(reader)? else { return Ok(None) };
        let bad = || Error::BadBody { kind };
        Ok(Some(match kind {
            HELLO => {
                let mut at = Reader(&body);
                let token = at.text().ok_or_else(bad)?;
                let name = at.text().ok_or_else(bad)?;
                let (mode, codec) = match at.rest() {
                    [mode, codec] => (
                        match mode {
                            0 => Mode::Live,
                            1 => Mode::Paced,
                            _ => return Err(bad()),
                        },
                        codec_from(*codec).ok_or_else(bad)?,
                    ),
                    _ => return Err(bad()),
                };
                Self::Hello(Hello { token, name, mode, codec })
            }
            STILL => Self::Still { jpeg: body },
            STREAM_START if body.len() == STREAM_START_LEN => Self::StreamStart(StreamFormat {
                codec: codec_from(body[0]).ok_or_else(bad)?,
                width: u16::from_le_bytes([body[1], body[2]]),
                height: u16::from_le_bytes([body[3], body[4]]),
            }),
            FRAME if body.len() >= FRAME_FIXED_LEN => {
                let mut body = body;
                let payload = body.split_off(FRAME_FIXED_LEN);
                Self::Frame(Frame {
                    seq: u32::from_le_bytes(body[0..4].try_into().expect("4 bytes")),
                    time_us: u64::from_le_bytes(body[4..12].try_into().expect("8 bytes")),
                    keyframe: body[12] & FLAG_KEYFRAME != 0,
                    payload,
                })
            }
            STREAM_END if body.is_empty() => Self::StreamEnd,
            STREAM_START | FRAME | STREAM_END => return Err(bad()),
            other => return Err(Error::UnknownKind(other)),
        }))
    }
}

impl ToProgram {
    pub fn encode(&self) -> Vec<u8> {
        let mut body = Vec::new();
        let kind = match self {
            Self::Welcome => WELCOME,
            Self::Refused { reason } => {
                body.extend_from_slice(reason.as_bytes());
                REFUSED
            }
            Self::Activate => ACTIVATE,
            Self::Deactivate => DEACTIVATE,
            Self::Keyframe => KEYFRAME,
        };
        framed(kind, body)
    }

    pub fn read_from(reader: &mut impl Read) -> Result<Option<Self>, Error> {
        let Some((kind, body)) = read_message(reader)? else { return Ok(None) };
        let empty = |message: Self| if body.is_empty() { Ok(message) } else { Err(Error::BadBody { kind }) };
        Ok(Some(match kind {
            WELCOME => empty(Self::Welcome)?,
            REFUSED => Self::Refused { reason: String::from_utf8(body).map_err(|_| Error::BadBody { kind })? },
            ACTIVATE => empty(Self::Activate)?,
            DEACTIVATE => empty(Self::Deactivate)?,
            KEYFRAME => empty(Self::Keyframe)?,
            other => return Err(Error::UnknownKind(other)),
        }))
    }
}

fn framed(kind: u8, body: Vec<u8>) -> Vec<u8> {
    let len = u32::try_from(body.len()).expect("a message body fits in 4 GiB");
    let mut out = Vec::with_capacity(HEADER_LEN + body.len());
    out.extend_from_slice(&[VERSION, kind, 0, 0]);
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&body);
    out
}

fn read_message(reader: &mut impl Read) -> Result<Option<(u8, Vec<u8>)>, Error> {
    let mut header = [0u8; HEADER_LEN];
    match reader.read_exact(&mut header) {
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        other => other?,
    }
    if header[0] != VERSION {
        return Err(Error::UnsupportedVersion(header[0]));
    }
    let len = u32::from_le_bytes(header[4..8].try_into().expect("4 bytes"));
    if len > MAX_BODY_LEN {
        return Err(Error::BodyTooLarge(len));
    }
    let mut body = vec![0u8; len as usize];
    reader.read_exact(&mut body)?;
    Ok(Some((header[1], body)))
}

fn codec_byte(codec: Codec) -> u8 {
    match codec {
        Codec::H264 => 0,
        Codec::Hevc => 1,
    }
}

fn codec_from(byte: u8) -> Option<Codec> {
    match byte {
        0 => Some(Codec::H264),
        1 => Some(Codec::Hevc),
        _ => None,
    }
}

fn put_text(out: &mut Vec<u8>, text: &str) {
    let len = u16::try_from(text.len()).expect("text fits in 64 KiB");
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(text.as_bytes());
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn text(&mut self) -> Option<String> {
        let (len, rest) = self.0.split_first_chunk::<2>()?;
        let len = u16::from_le_bytes(*len) as usize;
        let text = rest.get(..len)?;
        self.0 = &rest[len..];
        String::from_utf8(text.to_vec()).ok()
    }

    fn rest(self) -> &'a [u8] {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host_round_trip(message: ToHost) {
        let bytes = message.encode();
        assert_eq!(ToHost::read_from(&mut bytes.as_slice()).unwrap(), Some(message));
    }

    fn program_round_trip(message: ToProgram) {
        let bytes = message.encode();
        assert_eq!(ToProgram::read_from(&mut bytes.as_slice()).unwrap(), Some(message));
    }

    #[test]
    fn program_messages_round_trip() {
        host_round_trip(ToHost::Hello(Hello { token: "abc".into(), name: "wallpaper-engine".into(), mode: Mode::Live, codec: Codec::H264 }));
        host_round_trip(ToHost::Hello(Hello { token: String::new(), name: "x".into(), mode: Mode::Paced, codec: Codec::Hevc }));
        host_round_trip(ToHost::Still { jpeg: vec![0xff, 0xd8, 1, 2] });
        host_round_trip(ToHost::StreamStart(StreamFormat { codec: Codec::Hevc, width: 1920, height: 1080 }));
        host_round_trip(ToHost::Frame(Frame { seq: 7, time_us: u64::MAX - 1, keyframe: true, payload: vec![0, 0, 0, 1, 0x67] }));
        host_round_trip(ToHost::Frame(Frame { seq: 8, time_us: 0, keyframe: false, payload: vec![] }));
        host_round_trip(ToHost::StreamEnd);
    }

    #[test]
    fn host_messages_round_trip() {
        for message in [ToProgram::Welcome, ToProgram::Refused { reason: "taken".into() }, ToProgram::Activate, ToProgram::Deactivate, ToProgram::Keyframe] {
            program_round_trip(message);
        }
    }

    #[test]
    fn the_header_is_version_kind_and_length() {
        let bytes = ToProgram::Refused { reason: "no".into() }.encode();
        assert_eq!(bytes, [VERSION, REFUSED, 0, 0, 2, 0, 0, 0, b'n', b'o']);
    }

    #[test]
    fn a_clean_end_is_none() {
        assert_eq!(ToHost::read_from(&mut [].as_slice()).unwrap(), None);
    }

    #[test]
    fn refuses_other_versions_kinds_and_bodies() {
        let mut bytes = ToHost::StreamEnd.encode();
        bytes[0] = 2;
        assert!(matches!(ToHost::read_from(&mut bytes.as_slice()), Err(Error::UnsupportedVersion(2))));
        assert!(matches!(ToHost::read_from(&mut ToProgram::Welcome.encode().as_slice()), Err(Error::UnknownKind(WELCOME))));
        let short_start = framed(STREAM_START, vec![0, 1]);
        assert!(matches!(ToHost::read_from(&mut short_start.as_slice()), Err(Error::BadBody { kind: STREAM_START })));
        let bad_hello = framed(HELLO, vec![5, 0, b'a']);
        assert!(matches!(ToHost::read_from(&mut bad_hello.as_slice()), Err(Error::BadBody { kind: HELLO })));
        let huge = [VERSION, FRAME, 0, 0, 0xff, 0xff, 0xff, 0xff];
        assert!(matches!(ToHost::read_from(&mut huge.as_slice()), Err(Error::BodyTooLarge(_))));
    }
}
