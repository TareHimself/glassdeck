//! Glassdeck wire protocol: an 8-byte header per message; a later `StreamStart` switches source.

pub mod compositor;
mod session;

use std::fmt;
use std::io::{self, Read};
use std::time::{SystemTime, UNIX_EPOCH};

pub use session::{Action, SessionError, SessionState, step};

pub const VERSION: u8 = 6;
pub const HEADER_LEN: usize = 8;
pub const MAX_BODY_LEN: u32 = 8 * 1024 * 1024;

/// Paced streams are wallpapers, where start-up delay costs nothing, so they buffer deeply enough to
/// ride out Wi-Fi stalls. The sender runs this far ahead of presentation time...
pub const PACED_SEND_AHEAD_US: u64 = 400_000;
/// ...and the receiver starts this far behind the first frame. It must exceed the send-ahead.
pub const PACED_PLAYOUT_DELAY_US: u64 = 500_000;

const KIND_STREAM_START: u8 = 1;
const KIND_FRAME: u8 = 2;
const KIND_STREAM_END: u8 = 3;
const KIND_CLOCK_PROBE: u8 = 4;
const KIND_CLOCK_REPLY: u8 = 5;
const KIND_SELECT_BACKGROUND: u8 = 6;
const KIND_MODULE_DATA: u8 = 7;
const STREAM_START_LEN: usize = 20;
const FRAME_FIXED_LEN: usize = 20;
const STREAM_END_LEN: usize = 4;
const CLOCK_PROBE_LEN: usize = 8;
const CLOCK_REPLY_LEN: usize = 16;
const MODULE_DATA_FIXED_LEN: usize = 4;
const FLAG_KEYFRAME: u8 = 0b0000_0001;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    H264,
    Hevc,
}

/// How the receiver times frames.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Show each frame on arrival and drop stale ones. `time_us` is the sender's wall-clock capture time;
    /// adding `clock_offset_us` (receiver clock minus sender clock) puts it on the receiver's clock.
    Live { clock_offset_us: i64 },
    /// Show each frame at its presentation time. `time_us` is microseconds from the start of the stream.
    Paced,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamInfo {
    pub stream_id: u32,
    pub codec: Codec,
    pub mode: Mode,
    pub width: u16,
    pub height: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub stream_id: u32,
    pub seq: u32,
    pub time_us: u64,
    pub keyframe: bool,
    pub payload: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Message {
    StreamStart(StreamInfo),
    Frame(Frame),
    StreamEnd { stream_id: u32 },
    /// Sender to receiver, before a stream: "my clock reads `sent_us`".
    ClockProbe { sent_us: u64 },
    /// Receiver to sender: the probe's `sent_us`, and the receiver's clock when the probe arrived.
    ClockReply { probe_sent_us: u64, received_us: u64 },
    /// Sender to receiver: play this background from the keyboard's background folder whenever no stream is open,
    /// and remember it across restarts.
    SelectBackground { name: String },
    /// PC to keyboard: what a widget's `pc.luau` sent, for the instance numbered by its place in
    /// the profile's widget list. `payload` is MessagePack.
    ModuleData { instance: u16, payload: Vec<u8> },
}

#[derive(Debug)]
pub enum ProtoError {
    UnsupportedVersion(u8),
    UnknownKind(u8),
    BodyTooLarge(u32),
    BadBodyLength { kind: u8, len: usize },
    UnknownCodec(u8),
    UnknownMode(u8),
    BackgroundNameNotUtf8,
    Io(io::Error),
}

impl fmt::Display for ProtoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedVersion(v) => write!(f, "unsupported protocol version {v} (expected {VERSION})"),
            Self::UnknownKind(k) => write!(f, "unknown message kind {k}"),
            Self::BodyTooLarge(len) => write!(f, "message body of {len} bytes exceeds {MAX_BODY_LEN}"),
            Self::BadBodyLength { kind, len } => write!(f, "message kind {kind} has invalid body length {len}"),
            Self::UnknownCodec(c) => write!(f, "unknown codec {c}"),
            Self::UnknownMode(m) => write!(f, "unknown mode {m}"),
            Self::BackgroundNameNotUtf8 => write!(f, "background name is not UTF-8"),
            Self::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ProtoError {}

impl From<io::Error> for ProtoError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl Codec {
    const fn to_wire(self) -> u8 {
        match self {
            Self::H264 => 1,
            Self::Hevc => 2,
        }
    }

    const fn from_wire(v: u8) -> Result<Self, ProtoError> {
        match v {
            1 => Ok(Self::H264),
            2 => Ok(Self::Hevc),
            other => Err(ProtoError::UnknownCodec(other)),
        }
    }
}

impl Mode {
    const fn to_wire(self) -> (u8, i64) {
        match self {
            Self::Live { clock_offset_us } => (1, clock_offset_us),
            Self::Paced => (2, 0),
        }
    }

    const fn from_wire(v: u8, clock_offset_us: i64) -> Result<Self, ProtoError> {
        match v {
            1 => Ok(Self::Live { clock_offset_us }),
            2 => Ok(Self::Paced),
            other => Err(ProtoError::UnknownMode(other)),
        }
    }
}

impl Message {
    pub fn encode(&self) -> Vec<u8> {
        let (kind, body) = match self {
            Self::StreamStart(info) => {
                let (mode, clock_offset_us) = info.mode.to_wire();
                let mut body = Vec::with_capacity(STREAM_START_LEN);
                body.extend_from_slice(&info.stream_id.to_le_bytes());
                body.push(info.codec.to_wire());
                body.push(mode);
                body.extend_from_slice(&[0, 0]);
                body.extend_from_slice(&info.width.to_le_bytes());
                body.extend_from_slice(&info.height.to_le_bytes());
                body.extend_from_slice(&clock_offset_us.to_le_bytes());
                (KIND_STREAM_START, body)
            }
            Self::Frame(frame) => {
                let mut body = Vec::with_capacity(FRAME_FIXED_LEN + frame.payload.len());
                body.extend_from_slice(&frame.stream_id.to_le_bytes());
                body.extend_from_slice(&frame.seq.to_le_bytes());
                body.extend_from_slice(&frame.time_us.to_le_bytes());
                body.push(if frame.keyframe { FLAG_KEYFRAME } else { 0 });
                body.extend_from_slice(&[0, 0, 0]);
                body.extend_from_slice(&frame.payload);
                (KIND_FRAME, body)
            }
            Self::StreamEnd { stream_id } => (KIND_STREAM_END, stream_id.to_le_bytes().to_vec()),
            Self::ClockProbe { sent_us } => (KIND_CLOCK_PROBE, sent_us.to_le_bytes().to_vec()),
            Self::ClockReply { probe_sent_us, received_us } => {
                (KIND_CLOCK_REPLY, [probe_sent_us.to_le_bytes(), received_us.to_le_bytes()].concat())
            }
            Self::SelectBackground { name } => (KIND_SELECT_BACKGROUND, name.as_bytes().to_vec()),
            Self::ModuleData { instance, payload } => {
                (KIND_MODULE_DATA, [&instance.to_le_bytes()[..], &[0, 0], payload].concat())
            }
        };
        let mut out = Vec::with_capacity(HEADER_LEN + body.len());
        out.push(VERSION);
        out.push(kind);
        out.extend_from_slice(&[0, 0]);
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&body);
        out
    }

    /// Reads one message. Returns `Ok(None)` on a clean end of stream between messages.
    pub fn read_from(reader: &mut impl Read) -> Result<Option<Self>, ProtoError> {
        let mut header = [0u8; HEADER_LEN];
        match reader.read_exact(&mut header) {
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
            other => other?,
        }
        let (kind, len) = parse_header(&header)?;
        let mut body = vec![0u8; len as usize];
        reader.read_exact(&mut body)?;
        Self::parse_body(kind, body).map(Some)
    }

    fn parse_body(kind: u8, body: Vec<u8>) -> Result<Self, ProtoError> {
        let bad_length = || ProtoError::BadBodyLength { kind, len: body.len() };
        match kind {
            KIND_STREAM_START if body.len() == STREAM_START_LEN => Ok(Self::StreamStart(StreamInfo {
                stream_id: u32_at(&body, 0),
                codec: Codec::from_wire(body[4])?,
                mode: Mode::from_wire(body[5], u64_at(&body, 12) as i64)?,
                width: u16::from_le_bytes([body[8], body[9]]),
                height: u16::from_le_bytes([body[10], body[11]]),
            })),
            KIND_FRAME if body.len() >= FRAME_FIXED_LEN => {
                let mut body = body;
                let payload = body.split_off(FRAME_FIXED_LEN);
                Ok(Self::Frame(Frame {
                    stream_id: u32_at(&body, 0),
                    seq: u32_at(&body, 4),
                    time_us: u64_at(&body, 8),
                    keyframe: body[16] & FLAG_KEYFRAME != 0,
                    payload,
                }))
            }
            KIND_STREAM_END if body.len() == STREAM_END_LEN => Ok(Self::StreamEnd { stream_id: u32_at(&body, 0) }),
            KIND_CLOCK_PROBE if body.len() == CLOCK_PROBE_LEN => Ok(Self::ClockProbe { sent_us: u64_at(&body, 0) }),
            KIND_CLOCK_REPLY if body.len() == CLOCK_REPLY_LEN => {
                Ok(Self::ClockReply { probe_sent_us: u64_at(&body, 0), received_us: u64_at(&body, 8) })
            }
            KIND_SELECT_BACKGROUND => {
                let name = String::from_utf8(body).map_err(|_| ProtoError::BackgroundNameNotUtf8)?;
                Ok(Self::SelectBackground { name })
            }
            KIND_MODULE_DATA if body.len() >= MODULE_DATA_FIXED_LEN => {
                let mut body = body;
                let payload = body.split_off(MODULE_DATA_FIXED_LEN);
                Ok(Self::ModuleData { instance: u16::from_le_bytes([body[0], body[1]]), payload })
            }
            KIND_STREAM_START | KIND_FRAME | KIND_STREAM_END | KIND_CLOCK_PROBE | KIND_CLOCK_REPLY | KIND_MODULE_DATA => {
                Err(bad_length())
            }
            other => Err(ProtoError::UnknownKind(other)),
        }
    }
}

fn parse_header(header: &[u8; HEADER_LEN]) -> Result<(u8, u32), ProtoError> {
    if header[0] != VERSION {
        return Err(ProtoError::UnsupportedVersion(header[0]));
    }
    let len = u32_at(header, 4);
    if len > MAX_BODY_LEN {
        return Err(ProtoError::BodyTooLarge(len));
    }
    Ok((header[1], len))
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().expect("4 bytes"))
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().expect("8 bytes"))
}

/// Wall-clock microseconds. Comparing two machines' readings needs the offset from clock sync.
pub fn unix_micros() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before 1970")
        .as_micros() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(message: Message) {
        let bytes = message.encode();
        let decoded = Message::read_from(&mut bytes.as_slice()).unwrap().unwrap();
        assert_eq!(decoded, message);
    }

    #[test]
    fn every_message_round_trips() {
        round_trip(Message::StreamStart(StreamInfo {
            stream_id: 3,
            codec: Codec::Hevc,
            mode: Mode::Paced,
            width: 1920,
            height: 1080,
        }));
        round_trip(Message::StreamStart(StreamInfo {
            stream_id: 4,
            codec: Codec::H264,
            mode: Mode::Live { clock_offset_us: -1_093_250 },
            width: 1920,
            height: 1080,
        }));
        round_trip(Message::Frame(Frame {
            stream_id: 3,
            seq: 9,
            time_us: 1_234_567,
            keyframe: true,
            payload: vec![0, 0, 0, 1, 0x65],
        }));
        round_trip(Message::StreamEnd { stream_id: 3 });
        round_trip(Message::ClockProbe { sent_us: 1_795_567_092_181_099 });
        round_trip(Message::ClockReply { probe_sent_us: 1, received_us: 1_795_567_093_267_780 });
        round_trip(Message::SelectBackground { name: "late-night.deckbg".into() });
        round_trip(Message::ModuleData { instance: 3, payload: vec![0x81, 0xa3, b'c', b'p', b'u', 0x2a] });
    }

    #[test]
    fn clean_eof_between_messages_is_not_an_error() {
        assert!(Message::read_from(&mut [].as_slice()).unwrap().is_none());
    }

    #[test]
    fn refuses_other_versions() {
        let mut bytes = Message::StreamEnd { stream_id: 1 }.encode();
        bytes[0] = 1;
        assert!(matches!(Message::read_from(&mut bytes.as_slice()), Err(ProtoError::UnsupportedVersion(1))));
    }

    #[test]
    fn refuses_unknown_kinds_codecs_and_bad_lengths() {
        let mut unknown = Message::StreamEnd { stream_id: 1 }.encode();
        unknown[1] = 77;
        assert!(matches!(Message::read_from(&mut unknown.as_slice()), Err(ProtoError::UnknownKind(77))));

        let mut start = Message::StreamStart(StreamInfo {
            stream_id: 1,
            codec: Codec::H264,
            mode: Mode::Live { clock_offset_us: 0 },
            width: 2,
            height: 2,
        })
        .encode();
        start[HEADER_LEN + 4] = 9;
        assert!(matches!(Message::read_from(&mut start.as_slice()), Err(ProtoError::UnknownCodec(9))));

        let mut short = Message::StreamEnd { stream_id: 1 }.encode();
        short[4] = 2;
        short.truncate(HEADER_LEN + 2);
        assert!(matches!(
            Message::read_from(&mut short.as_slice()),
            Err(ProtoError::BadBodyLength { kind: KIND_STREAM_END, len: 2 })
        ));
    }

    #[test]
    fn refuses_oversized_bodies() {
        let mut bytes = Message::StreamEnd { stream_id: 1 }.encode();
        bytes[4..8].copy_from_slice(&(MAX_BODY_LEN + 1).to_le_bytes());
        assert!(matches!(Message::read_from(&mut bytes.as_slice()), Err(ProtoError::BodyTooLarge(_))));
    }
}
