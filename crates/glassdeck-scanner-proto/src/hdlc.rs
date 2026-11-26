//! Frames on the serial link: `0x7E`, a big-endian u16 length, the payload, a big-endian CRC.
//! Every byte after the flag that is `0x7E` or `0x7D` goes out as `0x7D, byte ^ 0x20`.
//! There is no closing flag: a frame ends when its length and CRC have arrived.

pub const FLAG: u8 = 0x7E;
pub const ESCAPE: u8 = 0x7D;
const ESCAPE_XOR: u8 = 0x20;
const HEADER_LEN: usize = 2;
const CRC_LEN: usize = 4;

/// The longest payload the firmware accepts on the serial link.
pub const SERIAL_MAX_PAYLOAD: usize = 512;

/// The most bytes a frame of `payload_len` can take: the flag, then everything escaped.
pub const fn max_encoded_len(payload_len: usize) -> usize {
    1 + 2 * (HEADER_LEN + payload_len + CRC_LEN)
}

const REFLECTED: [u8; 256] = {
    let mut table = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        table[i] = (i as u8).reverse_bits();
        i += 1;
    }
    table
};

const CRC_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut crc = (i as u32) << 24;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 0x8000_0000 != 0 { (crc << 1) ^ 0x04C1_1DB7 } else { crc << 1 };
            bit += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
};

/// CRC-32 (polynomial `0x04C11DB7`, init and final XOR `0xFFFFFFFF`) with each input byte
/// reflected but the result not: neither common CRC-32 variant, but what the firmware checks.
pub fn crc32(data: &[u8]) -> u32 {
    let crc = data.iter().fold(0xFFFF_FFFFu32, |crc, &byte| {
        CRC_TABLE[((crc >> 24) as u8 ^ REFLECTED[byte as usize]) as usize] ^ (crc << 8)
    });
    !crc
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EncodeError {
    TooLong { len: usize, max: usize },
    OutputTooSmall,
}

/// Writes `payload` as one frame into `out` and returns the frame's length.
pub fn encode<const MAX: usize>(payload: &[u8], out: &mut [u8]) -> Result<usize, EncodeError> {
    if payload.len() > MAX {
        return Err(EncodeError::TooLong { len: payload.len(), max: MAX });
    }
    let len = (payload.len() as u16).to_be_bytes();
    let crc = crc32(payload).to_be_bytes();
    let body = len.iter().chain(payload).chain(&crc);
    let mut writer = Writer { out, pos: 0 };
    writer.put(FLAG)?;
    for &byte in body {
        if byte == FLAG || byte == ESCAPE {
            writer.put(ESCAPE)?;
            writer.put(byte ^ ESCAPE_XOR)?;
        } else {
            writer.put(byte)?;
        }
    }
    Ok(writer.pos)
}

struct Writer<'a> {
    out: &'a mut [u8],
    pos: usize,
}

impl Writer<'_> {
    fn put(&mut self, byte: u8) -> Result<(), EncodeError> {
        let slot = self.out.get_mut(self.pos).ok_or(EncodeError::OutputTooSmall)?;
        *slot = byte;
        self.pos += 1;
        Ok(())
    }
}

/// Where the decoder is within a frame. Escapes are undone before a byte reaches a phase.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    WaitFlag,
    LengthHigh,
    LengthLow { high: u8 },
    Payload { len: u16, have: u16 },
    Crc { len: u16, crc: u32, have: u8 },
}

/// What one byte did to the frame in progress.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Continue(Phase),
    Store { at: u16, byte: u8, next: Phase },
    Complete { len: u16, crc: u32 },
    TooLong { len: u16 },
}

fn step<const MAX: usize>(phase: Phase, byte: u8) -> Step {
    match phase {
        Phase::WaitFlag => Step::Continue(Phase::WaitFlag),
        Phase::LengthHigh => Step::Continue(Phase::LengthLow { high: byte }),
        Phase::LengthLow { high } => {
            let len = u16::from_be_bytes([high, byte]);
            match len {
                0 => Step::Continue(Phase::Crc { len, crc: 0, have: 0 }),
                _ if len as usize > MAX => Step::TooLong { len },
                _ => Step::Continue(Phase::Payload { len, have: 0 }),
            }
        }
        Phase::Payload { len, have } => {
            let next = if have + 1 == len { Phase::Crc { len, crc: 0, have: 0 } } else { Phase::Payload { len, have: have + 1 } };
            Step::Store { at: have, byte, next }
        }
        Phase::Crc { len, crc, have } => {
            let crc = (crc << 8) | byte as u32;
            if have as usize + 1 == CRC_LEN { Step::Complete { len, crc } } else { Step::Continue(Phase::Crc { len, crc, have: have + 1 }) }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameError {
    /// The frame's CRC didn't match; it was dropped.
    BadCrc,
    /// The frame declared a payload longer than the decoder accepts; it was dropped.
    TooLong { len: u16 },
}

/// Turns serial bytes back into payloads of at most `MAX` bytes. A flag always starts a new
/// frame, dropping one in progress, so the decoder resynchronises on its own.
pub struct Decoder<const MAX: usize> {
    phase: Phase,
    escaped: bool,
    payload: [u8; MAX],
}

impl<const MAX: usize> Default for Decoder<MAX> {
    fn default() -> Self {
        Self { phase: Phase::WaitFlag, escaped: false, payload: [0; MAX] }
    }
}

impl<const MAX: usize> Decoder<MAX> {
    /// Takes one byte off the wire; returns a payload, or why a frame was dropped, when one ends.
    pub fn push(&mut self, raw: u8) -> Option<Result<&[u8], FrameError>> {
        if raw == FLAG {
            self.phase = Phase::LengthHigh;
            self.escaped = false;
            return None;
        }
        if self.phase == Phase::WaitFlag {
            return None;
        }
        let byte = if self.escaped {
            self.escaped = false;
            raw ^ ESCAPE_XOR
        } else if raw == ESCAPE {
            self.escaped = true;
            return None;
        } else {
            raw
        };
        match step::<MAX>(self.phase, byte) {
            Step::Continue(next) => {
                self.phase = next;
                None
            }
            Step::Store { at, byte, next } => {
                self.payload[at as usize] = byte;
                self.phase = next;
                None
            }
            Step::Complete { len, crc } => {
                self.phase = Phase::WaitFlag;
                let payload = &self.payload[..len as usize];
                Some(if crc32(payload) == crc { Ok(payload) } else { Err(FrameError::BadCrc) })
            }
            Step::TooLong { len } => {
                self.phase = Phase::WaitFlag;
                Some(Err(FrameError::TooLong { len }))
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::serial::tests::vector;

    fn frame(payload: &[u8]) -> Vec<u8> {
        let mut out = vec![0; max_encoded_len(payload.len())];
        let n = encode::<SERIAL_MAX_PAYLOAD>(payload, &mut out).unwrap();
        out.truncate(n);
        out
    }

    fn pattern(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i * 37 + 11) as u8).collect()
    }

    #[test]
    fn crc_matches_the_firmware_check_value() {
        assert_eq!(format!("{:08x}", crc32(b"123456789")), vector("crc32.check"));
    }

    #[test]
    fn frames_match_the_firmware() {
        assert_eq!(hex(&frame(&[])), vector("frame.empty"));
        assert_eq!(hex(&frame(&[0x7E, 0x7D, 0x00, 0xFF, 0x5E, 0x5D])), vector("frame.escapes"));
        assert_eq!(hex(&frame(&pattern(SERIAL_MAX_PAYLOAD))), vector("frame.max"));
        assert_eq!(vector("frame.too_long"), "refused");
        let mut out = [0; max_encoded_len(SERIAL_MAX_PAYLOAD + 1)];
        assert_eq!(
            encode::<SERIAL_MAX_PAYLOAD>(&pattern(SERIAL_MAX_PAYLOAD + 1), &mut out),
            Err(EncodeError::TooLong { len: SERIAL_MAX_PAYLOAD + 1, max: SERIAL_MAX_PAYLOAD })
        );
    }

    #[test]
    fn a_short_buffer_is_refused() {
        assert_eq!(encode::<SERIAL_MAX_PAYLOAD>(&[1, 2, 3], &mut [0; 8]), Err(EncodeError::OutputTooSmall));
    }

    #[test]
    fn decoding_the_firmware_stream_gives_what_the_firmware_decoded() {
        let expected: Vec<String> = crate::serial::tests::vectors("decoded").collect();
        let mut decoder = Decoder::<SERIAL_MAX_PAYLOAD>::default();
        let mut decoded = Vec::new();
        let mut errors = Vec::new();
        for byte in unhex(&vector("stream")) {
            match decoder.push(byte) {
                Some(Ok(payload)) => decoded.push(hex(payload)),
                Some(Err(e)) => errors.push(e),
                None => {}
            }
        }
        assert_eq!(decoded, expected);
        assert_eq!(errors, [FrameError::BadCrc, FrameError::TooLong { len: 0x0201 }]);
        assert_eq!(vector("crc_errors"), "1");
    }

    #[test]
    fn every_payload_round_trips() {
        for len in [0, 1, 2, 63, 64, 139, 401, SERIAL_MAX_PAYLOAD] {
            let payload: Vec<u8> = (0..len).map(|i| [FLAG, ESCAPE, i as u8][i % 3]).collect();
            let mut decoder = Decoder::<SERIAL_MAX_PAYLOAD>::default();
            let results: Vec<_> = frame(&payload).into_iter().filter_map(|b| decoder.push(b).map(|r| r.map(<[u8]>::to_vec))).collect();
            assert_eq!(results, [Ok(payload)]);
        }
    }

    pub(crate) fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    pub(crate) fn unhex(text: &str) -> Vec<u8> {
        (0..text.len()).step_by(2).map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap()).collect()
    }
}
