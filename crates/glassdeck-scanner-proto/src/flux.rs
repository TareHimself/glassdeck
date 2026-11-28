//! The key state Flux's own firmware (Oates, 1.4.9.D) sends on the serial link: the same framing
//! and message type as [`crate::serial::KeyState`], but a 111-byte payload carrying the keyboard's
//! USB report, an HID usage bitmap and the modifier byte, in place of keymap-order key bits.
//! Read from a capture of the real keyboard (`vectors/flux-uart-capture.bin`).

use crate::serial::{MessageType, ParseError};

pub const FLUX_KEY_STATE_LEN: usize = 111;
const SEQUENCE: usize = 1;
const USAGES: usize = 52;
const USAGE_BYTES: usize = 28;
const MODIFIERS: usize = 80;

/// HID modifier bits.
pub const LEFT_SHIFT: u8 = 0x02;
pub const RIGHT_SHIFT: u8 = 0x20;

/// Sent whenever a key changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FluxKeyState {
    pub sequence: u32,
    /// Bit `u % 8` of byte `u / 8` is set while HID usage `u` (keyboard page) is down.
    pub usages: [u8; USAGE_BYTES],
    /// The HID modifier byte: Ctrl, Shift, Alt and GUI, left then right.
    pub modifiers: u8,
}

impl FluxKeyState {
    pub fn parse(payload: &[u8]) -> Result<Self, ParseError> {
        let Some(&kind) = payload.first() else { return Err(ParseError::Empty) };
        if kind != MessageType::KeyState as u8 {
            return Err(ParseError::UnknownType(kind));
        }
        if payload.len() != FLUX_KEY_STATE_LEN {
            return Err(ParseError::WrongLength { expected: FLUX_KEY_STATE_LEN, got: payload.len() });
        }
        let mut sequence = [0; 4];
        sequence.copy_from_slice(&payload[SEQUENCE..SEQUENCE + 4]);
        let mut usages = [0; USAGE_BYTES];
        usages.copy_from_slice(&payload[USAGES..USAGES + USAGE_BYTES]);
        Ok(Self { sequence: u32::from_le_bytes(sequence), usages, modifiers: payload[MODIFIERS] })
    }

    pub fn is_down(&self, usage: u8) -> bool {
        self.usages.get(usage as usize / 8).is_some_and(|byte| byte & (1 << (usage % 8)) != 0)
    }

    pub fn shift(&self) -> bool {
        self.modifiers & (LEFT_SHIFT | RIGHT_SHIFT) != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hdlc::{Decoder, SERIAL_MAX_PAYLOAD};

    const CAPTURE: &[u8] = include_bytes!("../vectors/flux-uart-capture.bin");
    const Q: u8 = 0x14;
    const W: u8 = 0x1A;
    const E: u8 = 0x08;
    const D: u8 = 0x07;
    const H: u8 = 0x0B;

    /// The capture's frames, each checked against its CRC by the decoder.
    fn states() -> Vec<FluxKeyState> {
        let mut decoder = Decoder::<SERIAL_MAX_PAYLOAD>::default();
        CAPTURE
            .iter()
            .filter_map(|&byte| decoder.push(byte).map(|frame| FluxKeyState::parse(frame.expect("a good CRC")).unwrap()))
            .collect()
    }

    fn down(state: &FluxKeyState) -> Vec<u8> {
        (0..=u8::MAX).filter(|&u| state.is_down(u)).collect()
    }

    #[test]
    fn every_captured_frame_passes_its_crc_and_counts_up() {
        let states = states();
        assert_eq!(states.len(), 14);
        assert!(states.windows(2).all(|w| w[1].sequence == w[0].sequence + 1));
    }

    #[test]
    fn the_capture_reads_as_it_was_typed() {
        let keys: Vec<(Vec<u8>, bool)> = states().iter().map(|s| (down(s), s.shift())).collect();
        let none = (vec![], false);
        assert_eq!(
            keys,
            [
                none.clone(),
                (vec![Q], false),
                none.clone(),
                (vec![W], false),
                none.clone(),
                (vec![E], false),
                none.clone(),
                (vec![], true),
                (vec![D], true),
                (vec![], true),
                none.clone(),
                (vec![H], false),
                none.clone(),
                none,
            ]
        );
    }

    #[test]
    fn other_lengths_and_types_are_refused() {
        let mut payload = [0u8; FLUX_KEY_STATE_LEN];
        payload[0] = MessageType::KeyState as u8;
        assert!(FluxKeyState::parse(&payload).is_ok());
        assert_eq!(FluxKeyState::parse(&payload[..110]), Err(ParseError::WrongLength { expected: 111, got: 110 }));
        payload[0] = 3;
        assert_eq!(FluxKeyState::parse(&payload), Err(ParseError::UnknownType(3)));
        assert_eq!(FluxKeyState::parse(&[]), Err(ParseError::Empty));
    }
}
