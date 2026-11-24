//! Each frame on the wire is a fixed header followed by one H.264 access unit.

use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

pub const VERSION: u8 = 1;
pub const HEADER_LEN: usize = 20;
pub const MAX_PAYLOAD_LEN: u32 = 8 * 1024 * 1024;

const FLAG_KEYFRAME: u8 = 0b0000_0001;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameHeader {
    pub seq: u32,
    pub capture_us: u64,
    pub keyframe: bool,
    pub payload_len: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeaderError {
    UnsupportedVersion(u8),
    PayloadTooLarge(u32),
}

impl fmt::Display for HeaderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedVersion(v) => write!(f, "unsupported protocol version {v}"),
            Self::PayloadTooLarge(len) => write!(f, "payload of {len} bytes exceeds {MAX_PAYLOAD_LEN}"),
        }
    }
}

impl std::error::Error for HeaderError {}

impl FrameHeader {
    pub fn encode(&self) -> [u8; HEADER_LEN] {
        let mut out = [0u8; HEADER_LEN];
        out[0] = VERSION;
        out[1] = if self.keyframe { FLAG_KEYFRAME } else { 0 };
        out[4..8].copy_from_slice(&self.seq.to_le_bytes());
        out[8..16].copy_from_slice(&self.capture_us.to_le_bytes());
        out[16..20].copy_from_slice(&self.payload_len.to_le_bytes());
        out
    }

    pub fn decode(bytes: &[u8; HEADER_LEN]) -> Result<Self, HeaderError> {
        if bytes[0] != VERSION {
            return Err(HeaderError::UnsupportedVersion(bytes[0]));
        }
        let payload_len = u32::from_le_bytes(bytes[16..20].try_into().expect("4 bytes"));
        if payload_len > MAX_PAYLOAD_LEN {
            return Err(HeaderError::PayloadTooLarge(payload_len));
        }
        Ok(Self {
            seq: u32::from_le_bytes(bytes[4..8].try_into().expect("4 bytes")),
            capture_us: u64::from_le_bytes(bytes[8..16].try_into().expect("8 bytes")),
            keyframe: bytes[1] & FLAG_KEYFRAME != 0,
            payload_len,
        })
    }
}

/// Wall-clock microseconds. Sender and receiver compare these, so latency figures
/// are only meaningful while both run on the same machine.
pub fn unix_micros() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before 1970")
        .as_micros() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_round_trips() {
        let header = FrameHeader { seq: 7, capture_us: 1_234_567, keyframe: true, payload_len: 4096 };
        assert_eq!(FrameHeader::decode(&header.encode()), Ok(header));
    }

    #[test]
    fn refuses_other_versions() {
        let mut bytes = FrameHeader { seq: 0, capture_us: 0, keyframe: false, payload_len: 0 }.encode();
        bytes[0] = 9;
        assert_eq!(FrameHeader::decode(&bytes), Err(HeaderError::UnsupportedVersion(9)));
    }

    #[test]
    fn refuses_oversized_payloads() {
        let header = FrameHeader { seq: 0, capture_us: 0, keyframe: false, payload_len: MAX_PAYLOAD_LEN + 1 };
        assert_eq!(
            FrameHeader::decode(&header.encode()),
            Err(HeaderError::PayloadTooLarge(MAX_PAYLOAD_LEN + 1))
        );
    }
}
