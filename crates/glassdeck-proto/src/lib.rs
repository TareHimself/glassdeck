//! Glassdeck wire protocol: an 8-byte header per message; a later `StreamStart` switches source.

pub mod compositor;
mod session;

use std::fmt;
use std::io::{self, Read};
use std::time::{SystemTime, UNIX_EPOCH};

pub use session::{Action, SessionError, SessionState, step};

pub const VERSION: u8 = 13;
pub const HEADER_LEN: usize = 8;
pub const MAX_BODY_LEN: u32 = 8 * 1024 * 1024;
/// The built-in canvas's package id: the default background, always on the keyboard and never a file.
pub const BUILT_IN_BACKGROUND: &str = "background.core.glassdeck.gradient";
/// Files at least this big pause the stream while they go to the keyboard, which shows their
/// progress; smaller ones finish too quickly to be worth changing the screen for.
pub const LARGE_FILE_BYTES: u64 = 10 * 1024 * 1024;

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
const KIND_PUT_FILE: u8 = 8;
const KIND_PUT_DONE: u8 = 9;
const KIND_PUT_REFUSED: u8 = 10;
const KIND_MODULE_RESOURCE: u8 = 11;
const KIND_BACKGROUND_DATA: u8 = 12;
const KIND_SET_WIDGETS: u8 = 13;
const KIND_INVENTORY: u8 = 14;
const KIND_REMOVE_PACKAGES: u8 = 15;
const STREAM_START_LEN: usize = 20;
const FRAME_FIXED_LEN: usize = 20;
const STREAM_END_LEN: usize = 4;
const CLOCK_PROBE_LEN: usize = 8;
const CLOCK_REPLY_LEN: usize = 16;
const MODULE_DATA_FIXED_LEN: usize = 4;
const MODULE_RESOURCE_FIXED_LEN: usize = 4;
const TARGET_FIXED_LEN: usize = 4;
const PUT_FILE_FIXED_LEN: usize = TARGET_FIXED_LEN + 16;
const TARGET_PACKAGE: u8 = 1;
const SET_WIDGETS_FIXED_LEN: usize = 4;
/// x, y, w, h as u16, then the id's length
const WIDGET_SLOT_FIXED_LEN: usize = 9;
/// A count, then padding, before a list's items
const LIST_FIXED_LEN: usize = 4;
/// The digest, then the name's length
const HELD_FIXED_LEN: usize = 9;
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

/// Where a file sent to the keyboard goes. The keyboard checks the names before writing anything.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileTarget {
    /// A `.deck` for the packages folder, by its file name `<id>.deck`
    Package(String),
}

impl fmt::Display for FileTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Package(name) => write!(f, "package {name}"),
        }
    }
}

/// A package the keyboard holds: its file name, and the digest of its bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeldPackage {
    pub name: String,
    pub digest: u64,
}

/// One widget in a `SetWidgets` layout: its package id, and where it sits in the widget area.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WidgetSlot {
    pub package: String,
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
}

/// One piece of a file. The file is complete once a piece ends at `total`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PutFile {
    pub target: FileTarget,
    pub offset: u64,
    pub total: u64,
    pub data: Vec<u8>,
}

impl PutFile {
    pub fn end(&self) -> u64 {
        self.offset + self.data.len() as u64
    }

    pub fn completes(&self) -> bool {
        self.end() >= self.total
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Message {
    StreamStart(StreamInfo),
    Frame(Frame),
    StreamEnd {
        stream_id: u32,
    },
    /// Sender to receiver, before a stream: "my clock reads `sent_us`".
    ClockProbe {
        sent_us: u64,
    },
    /// Receiver to sender: the probe's `sent_us`, and the receiver's clock when the probe arrived.
    ClockReply {
        probe_sent_us: u64,
        received_us: u64,
    },
    /// Host to keyboard: play this background package, by id, whenever no stream is open, and remember
    /// it across restarts.
    SelectBackground {
        name: String,
    },
    /// Host to keyboard: the widgets to run, in order, and remember them across restarts. An
    /// instance's number is its place in this list.
    SetWidgets {
        widgets: Vec<WidgetSlot>,
    },
    /// Keyboard to host, on every connect: the packages it holds, so the host uploads only what's missing.
    Inventory {
        packages: Vec<HeldPackage>,
    },
    /// Host to keyboard: delete these packages, which the active theme no longer uses.
    RemovePackages {
        names: Vec<String>,
    },
    /// Host to keyboard: what a widget's `host.luau` sent, for the instance numbered by its place in
    /// the widget list. `payload` is MessagePack.
    ModuleData {
        instance: u16,
        payload: Vec<u8>,
    },
    /// Host to keyboard: a piece of a file to install. The keyboard answers only the piece that
    /// completes the file, with `PutDone` or `PutRefused`.
    PutFile(PutFile),
    /// Keyboard to host: the file is in place.
    PutDone {
        target: FileTarget,
    },
    /// Keyboard to host: the file was not installed.
    PutRefused {
        target: FileTarget,
        reason: String,
    },
    /// Host to keyboard: a resource a widget's host half made at run time, such as album art, for
    /// its keyboard half to draw. Empty `data` removes it.
    ModuleResource {
        instance: u16,
        name: String,
        data: Vec<u8>,
    },
    /// Host to keyboard: what the selected canvas background's host half sent (or its options, with no
    /// host half), for its keyboard script. `payload` is MessagePack.
    BackgroundData {
        payload: Vec<u8>,
    },
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
    BadFileTarget,
    BadWidgetList,
    BadList,
    TextNotUtf8,
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
            Self::BadFileTarget => write!(f, "malformed file target"),
            Self::BadWidgetList => write!(f, "malformed widget list"),
            Self::BadList => write!(f, "malformed package list"),
            Self::TextNotUtf8 => write!(f, "text is not UTF-8"),
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
    /// The message with its header, written straight into one buffer so a frame's payload is copied once.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEADER_LEN + self.body_len_hint());
        out.extend_from_slice(&[VERSION, 0, 0, 0, 0, 0, 0, 0]);
        out[1] = self.write_body(&mut out);
        let len = u32::try_from(out.len() - HEADER_LEN).expect("a message body fits in 4 GiB");
        out[4..HEADER_LEN].copy_from_slice(&len.to_le_bytes());
        out
    }

    fn body_len_hint(&self) -> usize {
        match self {
            Self::Frame(frame) => FRAME_FIXED_LEN + frame.payload.len(),
            Self::PutFile(put) => put.data.len() + 64,
            Self::ModuleData { payload, .. } | Self::BackgroundData { payload } => payload.len() + 4,
            Self::ModuleResource { name, data, .. } => name.len() + data.len() + 4,
            Self::SetWidgets { widgets } => SET_WIDGETS_FIXED_LEN + widgets.iter().map(|w| WIDGET_SLOT_FIXED_LEN + w.package.len()).sum::<usize>(),
            Self::Inventory { packages } => LIST_FIXED_LEN + packages.iter().map(|p| HELD_FIXED_LEN + p.name.len()).sum::<usize>(),
            Self::RemovePackages { names } => LIST_FIXED_LEN + names.iter().map(|n| 1 + n.len()).sum::<usize>(),
            _ => 64,
        }
    }

    /// Appends the body to `out` and returns the message's kind.
    fn write_body(&self, out: &mut Vec<u8>) -> u8 {
        match self {
            Self::StreamStart(info) => {
                let (mode, clock_offset_us) = info.mode.to_wire();
                out.extend_from_slice(&info.stream_id.to_le_bytes());
                out.push(info.codec.to_wire());
                out.push(mode);
                out.extend_from_slice(&[0, 0]);
                out.extend_from_slice(&info.width.to_le_bytes());
                out.extend_from_slice(&info.height.to_le_bytes());
                out.extend_from_slice(&clock_offset_us.to_le_bytes());
                KIND_STREAM_START
            }
            Self::Frame(frame) => {
                out.extend_from_slice(&frame.stream_id.to_le_bytes());
                out.extend_from_slice(&frame.seq.to_le_bytes());
                out.extend_from_slice(&frame.time_us.to_le_bytes());
                out.push(if frame.keyframe { FLAG_KEYFRAME } else { 0 });
                out.extend_from_slice(&[0, 0, 0]);
                out.extend_from_slice(&frame.payload);
                KIND_FRAME
            }
            Self::StreamEnd { stream_id } => {
                out.extend_from_slice(&stream_id.to_le_bytes());
                KIND_STREAM_END
            }
            Self::ClockProbe { sent_us } => {
                out.extend_from_slice(&sent_us.to_le_bytes());
                KIND_CLOCK_PROBE
            }
            Self::ClockReply { probe_sent_us, received_us } => {
                out.extend_from_slice(&probe_sent_us.to_le_bytes());
                out.extend_from_slice(&received_us.to_le_bytes());
                KIND_CLOCK_REPLY
            }
            Self::SelectBackground { name } => {
                out.extend_from_slice(name.as_bytes());
                KIND_SELECT_BACKGROUND
            }
            Self::ModuleData { instance, payload } => {
                out.extend_from_slice(&instance.to_le_bytes());
                out.extend_from_slice(&[0, 0]);
                out.extend_from_slice(payload);
                KIND_MODULE_DATA
            }
            Self::BackgroundData { payload } => {
                out.extend_from_slice(payload);
                KIND_BACKGROUND_DATA
            }
            Self::SetWidgets { widgets } => {
                let count = u16::try_from(widgets.len()).expect("a layout holds at most u16::MAX widgets");
                out.extend_from_slice(&count.to_le_bytes());
                out.extend_from_slice(&[0, 0]);
                for slot in widgets {
                    for n in [slot.x, slot.y, slot.w, slot.h] {
                        out.extend_from_slice(&n.to_le_bytes());
                    }
                    out.push(u8::try_from(slot.package.len()).expect("package ids are at most 255 bytes"));
                    out.extend_from_slice(slot.package.as_bytes());
                }
                KIND_SET_WIDGETS
            }
            Self::Inventory { packages } => {
                write_count(out, packages.len());
                for held in packages {
                    out.extend_from_slice(&held.digest.to_le_bytes());
                    write_name(out, &held.name);
                }
                KIND_INVENTORY
            }
            Self::RemovePackages { names } => {
                write_count(out, names.len());
                for name in names {
                    write_name(out, name);
                }
                KIND_REMOVE_PACKAGES
            }
            Self::PutFile(put) => {
                let (head, name) = encode_target(&put.target);
                out.extend_from_slice(&head);
                out.extend_from_slice(&put.offset.to_le_bytes());
                out.extend_from_slice(&put.total.to_le_bytes());
                out.extend_from_slice(name);
                out.extend_from_slice(&put.data);
                KIND_PUT_FILE
            }
            Self::PutDone { target } => {
                let (head, name) = encode_target(target);
                out.extend_from_slice(&head);
                out.extend_from_slice(name);
                KIND_PUT_DONE
            }
            Self::PutRefused { target, reason } => {
                let (head, name) = encode_target(target);
                out.extend_from_slice(&head);
                out.extend_from_slice(name);
                out.extend_from_slice(reason.as_bytes());
                KIND_PUT_REFUSED
            }
            Self::ModuleResource { instance, name, data } => {
                let len = u8::try_from(name.len()).expect("live resource names are at most 255 bytes");
                out.extend_from_slice(&instance.to_le_bytes());
                out.extend_from_slice(&[len, 0]);
                out.extend_from_slice(name.as_bytes());
                out.extend_from_slice(data);
                KIND_MODULE_RESOURCE
            }
        }
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
            KIND_BACKGROUND_DATA => Ok(Self::BackgroundData { payload: body }),
            KIND_SET_WIDGETS if body.len() >= SET_WIDGETS_FIXED_LEN => decode_widgets(&body).map(|widgets| Self::SetWidgets { widgets }),
            KIND_INVENTORY if body.len() >= LIST_FIXED_LEN => decode_inventory(&body).map(|packages| Self::Inventory { packages }),
            KIND_REMOVE_PACKAGES if body.len() >= LIST_FIXED_LEN => decode_names(&body).map(|names| Self::RemovePackages { names }),
            KIND_PUT_FILE if body.len() >= PUT_FILE_FIXED_LEN => {
                let (target, name_end) = decode_target(&body, PUT_FILE_FIXED_LEN)?;
                let (offset, total) = (u64_at(&body, 4), u64_at(&body, 12));
                let mut body = body;
                Ok(Self::PutFile(PutFile { target, offset, total, data: body.split_off(name_end) }))
            }
            KIND_PUT_DONE if body.len() >= TARGET_FIXED_LEN => match decode_target(&body, TARGET_FIXED_LEN)? {
                (target, name_end) if name_end == body.len() => Ok(Self::PutDone { target }),
                _ => Err(bad_length()),
            },
            KIND_PUT_REFUSED if body.len() >= TARGET_FIXED_LEN => {
                let (target, name_end) = decode_target(&body, TARGET_FIXED_LEN)?;
                let reason = String::from_utf8(body[name_end..].to_vec()).map_err(|_| ProtoError::TextNotUtf8)?;
                Ok(Self::PutRefused { target, reason })
            }
            KIND_MODULE_RESOURCE if body.len() >= MODULE_RESOURCE_FIXED_LEN => {
                let name_end = MODULE_RESOURCE_FIXED_LEN + body[2] as usize;
                let name = body.get(MODULE_RESOURCE_FIXED_LEN..name_end).ok_or_else(bad_length)?;
                let name = String::from_utf8(name.to_vec()).map_err(|_| ProtoError::TextNotUtf8)?;
                let instance = u16::from_le_bytes([body[0], body[1]]);
                let mut body = body;
                Ok(Self::ModuleResource { instance, name, data: body.split_off(name_end) })
            }
            KIND_STREAM_START | KIND_FRAME | KIND_STREAM_END | KIND_CLOCK_PROBE | KIND_CLOCK_REPLY | KIND_MODULE_DATA | KIND_PUT_FILE
            | KIND_PUT_DONE | KIND_PUT_REFUSED | KIND_MODULE_RESOURCE | KIND_SET_WIDGETS | KIND_INVENTORY | KIND_REMOVE_PACKAGES => {
                Err(bad_length())
            }
            other => Err(ProtoError::UnknownKind(other)),
        }
    }
}

/// A target is 4 fixed bytes (kind, name length, padding) at the body's start, and its name after
/// the message's other fixed fields.
fn encode_target(target: &FileTarget) -> ([u8; TARGET_FIXED_LEN], &[u8]) {
    let (kind, name) = match target {
        FileTarget::Package(name) => (TARGET_PACKAGE, name.as_bytes()),
    };
    let len = u8::try_from(name.len()).expect("file names are at most 255 bytes");
    ([kind, len, 0, 0], name)
}

/// Returns the target and where its name ends.
fn decode_target(body: &[u8], name_at: usize) -> Result<(FileTarget, usize), ProtoError> {
    let name_end = name_at + body[1] as usize;
    let name = body.get(name_at..name_end).ok_or(ProtoError::BadFileTarget)?;
    let text = || String::from_utf8(name.to_vec()).map_err(|_| ProtoError::TextNotUtf8);
    let target = match body[0] {
        TARGET_PACKAGE => FileTarget::Package(text()?),
        _ => return Err(ProtoError::BadFileTarget),
    };
    Ok((target, name_end))
}

fn write_count(out: &mut Vec<u8>, count: usize) {
    out.extend_from_slice(&u16::try_from(count).expect("lists hold at most u16::MAX items").to_le_bytes());
    out.extend_from_slice(&[0, 0]);
}

fn write_name(out: &mut Vec<u8>, name: &str) {
    out.push(u8::try_from(name.len()).expect("package file names are at most 255 bytes"));
    out.extend_from_slice(name.as_bytes());
}

/// A name written by `write_name` at `at`, and where the next item starts.
fn read_name(body: &[u8], at: usize) -> Result<(String, usize), ProtoError> {
    let len = usize::from(*body.get(at).ok_or(ProtoError::BadList)?);
    let name = body.get(at + 1..at + 1 + len).ok_or(ProtoError::BadList)?;
    Ok((String::from_utf8(name.to_vec()).map_err(|_| ProtoError::TextNotUtf8)?, at + 1 + len))
}

fn decode_inventory(body: &[u8]) -> Result<Vec<HeldPackage>, ProtoError> {
    let count = u16::from_le_bytes([body[0], body[1]]);
    let mut at = LIST_FIXED_LEN;
    let mut packages = Vec::with_capacity(usize::from(count));
    for _ in 0..count {
        let digest = body.get(at..at + 8).ok_or(ProtoError::BadList)?;
        let digest = u64::from_le_bytes(digest.try_into().expect("eight bytes"));
        let (name, next) = read_name(body, at + 8)?;
        packages.push(HeldPackage { name, digest });
        at = next;
    }
    if at != body.len() {
        return Err(ProtoError::BadList);
    }
    Ok(packages)
}

fn decode_names(body: &[u8]) -> Result<Vec<String>, ProtoError> {
    let count = u16::from_le_bytes([body[0], body[1]]);
    let mut at = LIST_FIXED_LEN;
    let mut names = Vec::with_capacity(usize::from(count));
    for _ in 0..count {
        let (name, next) = read_name(body, at)?;
        names.push(name);
        at = next;
    }
    if at != body.len() {
        return Err(ProtoError::BadList);
    }
    Ok(names)
}

fn decode_widgets(body: &[u8]) -> Result<Vec<WidgetSlot>, ProtoError> {
    let count = u16::from_le_bytes([body[0], body[1]]);
    let mut at = SET_WIDGETS_FIXED_LEN;
    let mut widgets = Vec::with_capacity(usize::from(count));
    for _ in 0..count {
        let fixed = body.get(at..at + WIDGET_SLOT_FIXED_LEN).ok_or(ProtoError::BadWidgetList)?;
        let n = |i: usize| u16::from_le_bytes([fixed[i * 2], fixed[i * 2 + 1]]);
        let id_end = at + WIDGET_SLOT_FIXED_LEN + usize::from(fixed[8]);
        let id = body.get(at + WIDGET_SLOT_FIXED_LEN..id_end).ok_or(ProtoError::BadWidgetList)?;
        let package = String::from_utf8(id.to_vec()).map_err(|_| ProtoError::TextNotUtf8)?;
        widgets.push(WidgetSlot { package, x: n(0), y: n(1), w: n(2), h: n(3) });
        at = id_end;
    }
    if at != body.len() {
        return Err(ProtoError::BadWidgetList);
    }
    Ok(widgets)
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
    SystemTime::now().duration_since(UNIX_EPOCH).expect("system clock before 1970").as_micros() as u64
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
        round_trip(Message::StreamStart(StreamInfo { stream_id: 3, codec: Codec::Hevc, mode: Mode::Paced, width: 1920, height: 1080 }));
        round_trip(Message::StreamStart(StreamInfo {
            stream_id: 4,
            codec: Codec::H264,
            mode: Mode::Live { clock_offset_us: -1_093_250 },
            width: 1920,
            height: 1080,
        }));
        round_trip(Message::Frame(Frame { stream_id: 3, seq: 9, time_us: 1_234_567, keyframe: true, payload: vec![0, 0, 0, 1, 0x65] }));
        round_trip(Message::StreamEnd { stream_id: 3 });
        round_trip(Message::ClockProbe { sent_us: 1_795_567_092_181_099 });
        round_trip(Message::ClockReply { probe_sent_us: 1, received_us: 1_795_567_093_267_780 });
        round_trip(Message::SelectBackground { name: "background.local.converted.late-night".into() });
        round_trip(Message::SetWidgets {
            widgets: vec![
                WidgetSlot { package: "widget.local.user.clock".into(), x: 792, y: 16, w: 320, h: 160 },
                WidgetSlot { package: "widget.local.user.pomodoro".into(), x: 1736, y: 16, w: 160, h: 160 },
            ],
        });
        round_trip(Message::SetWidgets { widgets: vec![] });
        round_trip(Message::Inventory {
            packages: vec![
                HeldPackage { name: "widget.local.user.clock.deck".into(), digest: 0x0123_4567_89ab_cdef },
                HeldPackage { name: "background.local.converted.late-night.deck".into(), digest: 7 },
            ],
        });
        round_trip(Message::Inventory { packages: vec![] });
        round_trip(Message::RemovePackages { names: vec!["a.b.deck".into(), "c.d.deck".into()] });
        round_trip(Message::ModuleData { instance: 3, payload: vec![0x81, 0xa3, b'c', b'p', b'u', 0x2a] });
        round_trip(Message::PutFile(PutFile {
            target: FileTarget::Package("background.local.converted.late-night.deck".into()),
            offset: 262_144,
            total: 300_000,
            data: vec![1, 2, 3],
        }));
        round_trip(Message::PutFile(PutFile { target: FileTarget::Package("a.b.deck".into()), offset: 0, total: 0, data: vec![] }));
        round_trip(Message::PutDone { target: FileTarget::Package("dev.glassdeck.cpu-meter.deck".into()) });
        round_trip(Message::PutRefused { target: FileTarget::Package("a.b.deck".into()), reason: "no space left".into() });
        round_trip(Message::ModuleResource { instance: 4, name: "cover".into(), data: vec![0x89, b'P', b'N', b'G'] });
        round_trip(Message::ModuleResource { instance: 4, name: "cover".into(), data: vec![] });
        round_trip(Message::BackgroundData { payload: vec![0x81, 0xa1, b'a', 1] });
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
        assert!(matches!(Message::read_from(&mut short.as_slice()), Err(ProtoError::BadBodyLength { kind: KIND_STREAM_END, len: 2 })));
    }

    #[test]
    fn refuses_malformed_file_targets() {
        let mut unknown_target = Message::PutDone { target: FileTarget::Package("a.b.deck".into()) }.encode();
        unknown_target[HEADER_LEN] = 9;
        assert!(matches!(Message::read_from(&mut unknown_target.as_slice()), Err(ProtoError::BadFileTarget)));
        let mut short_widgets = Message::SetWidgets { widgets: vec![WidgetSlot { package: "a.b".into(), x: 0, y: 0, w: 8, h: 8 }] }.encode();
        short_widgets.pop();
        short_widgets[4] -= 1;
        assert!(matches!(Message::read_from(&mut short_widgets.as_slice()), Err(ProtoError::BadWidgetList)));

        let mut long_name = Message::PutDone { target: FileTarget::Package("a.b.deck".into()) }.encode();
        long_name[HEADER_LEN + 1] = 200;
        assert!(matches!(Message::read_from(&mut long_name.as_slice()), Err(ProtoError::BadFileTarget)));
    }

    #[test]
    fn refuses_oversized_bodies() {
        let mut bytes = Message::StreamEnd { stream_id: 1 }.encode();
        bytes[4..8].copy_from_slice(&(MAX_BODY_LEN + 1).to_le_bytes());
        assert!(matches!(Message::read_from(&mut bytes.as_slice()), Err(ProtoError::BodyTooLarge(_))));
    }
}
