//! The receiver's compositor socket, for renderers on the keyboard itself (glassdeck-local-web).
//!
//! One message per SOCK_SEQPACKET datagram, little-endian: `[version u8][kind u8][0 u8; 2][body]`.
//! `Register` carries the buffer's dma-buf fd as SCM_RIGHTS; no other message carries an fd.

use std::fmt;

pub const VERSION: u8 = 1;
pub const HEADER_LEN: usize = 4;
/// Room for the longest message, a `Refused` with its reason.
pub const MAX_MESSAGE_LEN: usize = 512;

const KIND_REGISTER: u8 = 1;
const KIND_PRESENT: u8 = 2;
const KIND_REGISTERED: u8 = 3;
const KIND_REFUSED: u8 = 4;
const KIND_RELEASED: u8 = 5;
const REGISTER_LEN: usize = 28;
const ID_LEN: usize = 4;
const MAX_REASON_LEN: usize = MAX_MESSAGE_LEN - HEADER_LEN;

/// A single-plane buffer layout; its fd travels beside the message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BufferLayout {
    pub width: u32,
    pub height: u32,
    /// DRM fourcc, e.g. XR24
    pub fourcc: u32,
    pub stride: u32,
    pub offset: u32,
    pub modifier: u64,
}

pub type BufferId = u32;

/// Renderer to receiver.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToCompositor {
    /// Answered with `Registered` or `Refused`, in order.
    Register(BufferLayout),
    /// Show this buffer from the next flip. It comes back as `Released` once replaced on screen,
    /// or at once if the renderer doesn't own the screen right now.
    Present(BufferId),
}

/// Receiver to renderer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FromCompositor {
    Registered(BufferId),
    /// The buffer can't be scanned out; the renderer should give up rather than show nothing.
    Refused { reason: String },
    /// The renderer may draw into this buffer again.
    Released(BufferId),
}

#[derive(Debug, PartialEq, Eq)]
pub enum CompositorProtoError {
    UnsupportedVersion(u8),
    UnknownKind(u8),
    BadLength { kind: u8, len: usize },
    TooShort(usize),
    ReasonNotUtf8,
}

impl fmt::Display for CompositorProtoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedVersion(v) => write!(f, "unsupported compositor protocol version {v} (expected {VERSION})"),
            Self::UnknownKind(k) => write!(f, "unknown compositor message kind {k}"),
            Self::BadLength { kind, len } => write!(f, "compositor message kind {kind} has invalid length {len}"),
            Self::TooShort(len) => write!(f, "compositor message of {len} bytes is shorter than its header"),
            Self::ReasonNotUtf8 => write!(f, "refusal reason is not UTF-8"),
        }
    }
}

impl std::error::Error for CompositorProtoError {}

impl ToCompositor {
    pub fn encode(&self) -> Vec<u8> {
        match *self {
            Self::Register(layout) => {
                let mut body = Vec::with_capacity(REGISTER_LEN);
                for field in [layout.width, layout.height, layout.fourcc, layout.stride, layout.offset] {
                    body.extend_from_slice(&field.to_le_bytes());
                }
                body.extend_from_slice(&layout.modifier.to_le_bytes());
                with_header(KIND_REGISTER, &body)
            }
            Self::Present(id) => with_header(KIND_PRESENT, &id.to_le_bytes()),
        }
    }

    pub fn decode(message: &[u8]) -> Result<Self, CompositorProtoError> {
        let (kind, body) = split_header(message)?;
        let bad_length = || CompositorProtoError::BadLength { kind, len: body.len() };
        match kind {
            KIND_REGISTER if body.len() == REGISTER_LEN => Ok(Self::Register(BufferLayout {
                width: u32_at(body, 0),
                height: u32_at(body, 4),
                fourcc: u32_at(body, 8),
                stride: u32_at(body, 12),
                offset: u32_at(body, 16),
                modifier: u64::from_le_bytes(body[20..28].try_into().expect("8 bytes")),
            })),
            KIND_PRESENT if body.len() == ID_LEN => Ok(Self::Present(u32_at(body, 0))),
            KIND_REGISTER | KIND_PRESENT => Err(bad_length()),
            other => Err(CompositorProtoError::UnknownKind(other)),
        }
    }
}

impl FromCompositor {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Self::Registered(id) => with_header(KIND_REGISTERED, &id.to_le_bytes()),
            Self::Refused { reason } => with_header(KIND_REFUSED, truncate_utf8(reason, MAX_REASON_LEN).as_bytes()),
            Self::Released(id) => with_header(KIND_RELEASED, &id.to_le_bytes()),
        }
    }

    pub fn decode(message: &[u8]) -> Result<Self, CompositorProtoError> {
        let (kind, body) = split_header(message)?;
        let bad_length = || CompositorProtoError::BadLength { kind, len: body.len() };
        match kind {
            KIND_REGISTERED if body.len() == ID_LEN => Ok(Self::Registered(u32_at(body, 0))),
            KIND_RELEASED if body.len() == ID_LEN => Ok(Self::Released(u32_at(body, 0))),
            KIND_REFUSED => {
                let reason = std::str::from_utf8(body).map_err(|_| CompositorProtoError::ReasonNotUtf8)?;
                Ok(Self::Refused { reason: reason.to_owned() })
            }
            KIND_REGISTERED | KIND_RELEASED => Err(bad_length()),
            other => Err(CompositorProtoError::UnknownKind(other)),
        }
    }
}

fn with_header(kind: u8, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_LEN + body.len());
    out.extend_from_slice(&[VERSION, kind, 0, 0]);
    out.extend_from_slice(body);
    out
}

fn split_header(message: &[u8]) -> Result<(u8, &[u8]), CompositorProtoError> {
    if message.len() < HEADER_LEN {
        return Err(CompositorProtoError::TooShort(message.len()));
    }
    if message[0] != VERSION {
        return Err(CompositorProtoError::UnsupportedVersion(message[0]));
    }
    Ok((message[1], &message[HEADER_LEN..]))
}

fn truncate_utf8(text: &str, max_len: usize) -> &str {
    let mut end = text.len().min(max_len);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().expect("4 bytes"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const XR24: u32 = u32::from_le_bytes(*b"XR24");

    fn layout() -> BufferLayout {
        BufferLayout { width: 1920, height: 1080, fourcc: XR24, stride: 7680, offset: 0, modifier: 0 }
    }

    #[test]
    fn every_message_round_trips() {
        for message in [ToCompositor::Register(layout()), ToCompositor::Present(7)] {
            assert_eq!(ToCompositor::decode(&message.encode()), Ok(message));
        }
        for message in [
            FromCompositor::Registered(7),
            FromCompositor::Released(7),
            FromCompositor::Refused { reason: "plane rejected XR24 linear".into() },
        ] {
            assert_eq!(FromCompositor::decode(&message.encode()), Ok(message.clone()));
        }
    }

    /// The exact bytes glassdeck-local-web's C side must produce and accept.
    #[test]
    fn wire_layout_is_fixed() {
        assert_eq!(
            ToCompositor::Register(layout()).encode(),
            [
                [1, 1, 0, 0].as_slice(),
                &1920u32.to_le_bytes(),
                &1080u32.to_le_bytes(),
                b"XR24",
                &7680u32.to_le_bytes(),
                &0u32.to_le_bytes(),
                &0u64.to_le_bytes(),
            ]
            .concat()
        );
        assert_eq!(ToCompositor::Present(258).encode(), [1, 2, 0, 0, 2, 1, 0, 0]);
        assert_eq!(FromCompositor::Registered(258).encode(), [1, 3, 0, 0, 2, 1, 0, 0]);
        assert_eq!(FromCompositor::Refused { reason: "no".into() }.encode(), [1, 4, 0, 0, b'n', b'o']);
        assert_eq!(FromCompositor::Released(258).encode(), [1, 5, 0, 0, 2, 1, 0, 0]);
    }

    #[test]
    fn refuses_bad_messages() {
        assert_eq!(ToCompositor::decode(&[1, 2]), Err(CompositorProtoError::TooShort(2)));
        assert_eq!(ToCompositor::decode(&[2, 2, 0, 0, 1, 0, 0, 0]), Err(CompositorProtoError::UnsupportedVersion(2)));
        assert_eq!(ToCompositor::decode(&[1, 9, 0, 0]), Err(CompositorProtoError::UnknownKind(9)));
        assert_eq!(
            ToCompositor::decode(&[1, 2, 0, 0, 1]),
            Err(CompositorProtoError::BadLength { kind: KIND_PRESENT, len: 1 })
        );
        assert_eq!(ToCompositor::decode(&FromCompositor::Released(1).encode()), Err(CompositorProtoError::UnknownKind(5)));
        assert_eq!(FromCompositor::decode(&[1, 4, 0, 0, 0xff]), Err(CompositorProtoError::ReasonNotUtf8));
    }

    #[test]
    fn long_reasons_are_cut_on_a_character_boundary() {
        let reason = "é".repeat(MAX_MESSAGE_LEN);
        let encoded = FromCompositor::Refused { reason }.encode();
        assert!(encoded.len() <= MAX_MESSAGE_LEN);
        assert!(FromCompositor::decode(&encoded).is_ok());
    }
}
