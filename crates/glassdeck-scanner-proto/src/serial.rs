//! Messages on the serial link between the key scanner and the keyboard's Linux side
//! (`/dev/ttyS3`). The first payload byte is the type; multi-byte fields are little-endian.

pub const KEY_STATE_LEN: usize = 139;
pub const SENSOR_READINGS_LEN: usize = 401;
pub const RESPONSE_LEN: usize = 4;
pub const SENSOR_COUNT: usize = 160;
const STATE: usize = 5;
const EVENT_RING_FORMAT: u8 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum MessageType {
    KeyState = 2,
    SensorReadings = 3,
}

impl MessageType {
    fn parse(byte: u8) -> Result<Self, ParseError> {
        match byte {
            2 => Ok(Self::KeyState),
            3 => Ok(Self::SensorReadings),
            other => Err(ParseError::UnknownType(other)),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParseError {
    Empty,
    UnknownType(u8),
    WrongLength { expected: usize, got: usize },
    BadValue { field: &'static str, value: u32 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum KeyframeLayout {
    Ansi = 1,
    Iso = 2,
    Jis = 3,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum KeyframePhase {
    Absent = 0,
    FieldDetected = 1,
    IdsSettling = 2,
    Attached = 3,
    /// Attached, but refusing new presses while it recovers.
    Recovering = 4,
}

/// The last 16 OpenFlux function-key presses (`0x5000_00xx` keycodes), in press order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FunctionEvents {
    /// Presses since boot; wraps at 65536. The newest is at `codes[(seq - 1) & 15]`.
    pub seq: u16,
    pub codes: [u8; 16],
    /// The active profile when each press in `codes` completed.
    pub profiles: [u8; 16],
}

/// Sent by the key scanner whenever a key or a lock LED changes, and at least once a second.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyState {
    pub seq: u32,
    /// Bit `k % 8` of byte `k / 8` is set while key `k` (in keymap order) is down.
    pub pressed: [u8; 32],
    pub uptime_ms: u32,
    /// `None` when no keyframe has been identified.
    pub keyframe_layout: Option<KeyframeLayout>,
    pub keyframe_attached: bool,
    pub switch_feel: u8,
    /// Bit `n` for each active layer, held layer keys included.
    pub active_layers: u32,
    pub actuation_mode: u32,
    /// `None` when the scanner doesn't fill the event ring.
    pub function_events: Option<FunctionEvents>,
    /// `0x01` Num Lock, `0x02` Caps Lock, `0x04` Scroll Lock.
    pub host_leds: u8,
    pub language_code: u32,
    pub platform_code: u32,
    pub active_profile: u32,
    pub keyframe_phase: KeyframePhase,
    pub module_types: u8,
    pub module_buttons: u16,
}

impl KeyState {
    pub fn encode(&self) -> [u8; KEY_STATE_LEN] {
        let mut out = [0; KEY_STATE_LEN];
        out[0] = MessageType::KeyState as u8;
        out[1..5].copy_from_slice(&self.seq.to_le_bytes());
        let st = &mut out[STATE..];
        st[0x00..0x20].copy_from_slice(&self.pressed);
        st[0x20..0x24].copy_from_slice(&self.uptime_ms.to_le_bytes());
        st[0x24] = self.keyframe_layout.map_or(0, |l| l as u8);
        st[0x25] = self.keyframe_attached as u8;
        st[0x26] = self.switch_feel;
        st[0x27..0x2B].copy_from_slice(&self.active_layers.to_le_bytes());
        st[0x2B..0x2F].copy_from_slice(&self.actuation_mode.to_le_bytes());
        if let Some(events) = &self.function_events {
            st[0x2F] = EVENT_RING_FORMAT;
            st[0x30..0x32].copy_from_slice(&events.seq.to_le_bytes());
            st[0x32..0x42].copy_from_slice(&events.codes);
            st[0x42..0x52].copy_from_slice(&events.profiles);
        }
        st[0x5C] = self.host_leds;
        st[0x5D..0x61].copy_from_slice(&self.language_code.to_le_bytes());
        st[0x61..0x65].copy_from_slice(&self.platform_code.to_le_bytes());
        st[0x65..0x69].copy_from_slice(&self.active_profile.to_le_bytes());
        st[0x6A] = self.keyframe_phase as u8;
        st[0x6B] = self.module_types;
        st[0x84..0x86].copy_from_slice(&self.module_buttons.to_le_bytes());
        out
    }

    pub fn parse(payload: &[u8]) -> Result<Self, ParseError> {
        let p = exact::<KEY_STATE_LEN>(payload, MessageType::KeyState)?;
        let st = &p[STATE..];
        let function_events = match st[0x2F] {
            0 => None,
            EVENT_RING_FORMAT => Some(FunctionEvents { seq: u16_at(st, 0x30), codes: array(st, 0x32), profiles: array(st, 0x42) }),
            other => return Err(bad("event ring format", other)),
        };
        Ok(Self {
            seq: u32_at(p, 1),
            pressed: array(st, 0x00),
            uptime_ms: u32_at(st, 0x20),
            keyframe_layout: match st[0x24] {
                0 => None,
                1 => Some(KeyframeLayout::Ansi),
                2 => Some(KeyframeLayout::Iso),
                3 => Some(KeyframeLayout::Jis),
                other => return Err(bad("keyframe layout", other)),
            },
            keyframe_attached: flag(st[0x25], "keyframe attached")?,
            switch_feel: st[0x26],
            active_layers: u32_at(st, 0x27),
            actuation_mode: u32_at(st, 0x2B),
            function_events,
            host_leds: st[0x5C],
            language_code: u32_at(st, 0x5D),
            platform_code: u32_at(st, 0x61),
            active_profile: u32_at(st, 0x65),
            keyframe_phase: match st[0x6A] {
                0 => KeyframePhase::Absent,
                1 => KeyframePhase::FieldDetected,
                2 => KeyframePhase::IdsSettling,
                3 => KeyframePhase::Attached,
                4 => KeyframePhase::Recovering,
                other => return Err(bad("keyframe phase", other)),
            },
            module_types: st[0x6B],
            module_buttons: u16_at(st, 0x84),
        })
    }

    pub fn is_pressed(&self, key: usize) -> bool {
        self.pressed.get(key / 8).is_some_and(|byte| byte & (1 << (key % 8)) != 0)
    }
}

/// Raw 12-bit ADC readings of every Hall-effect sensor, sent about every 16 ms in sensor mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SensorReadings {
    pub seq: u32,
    /// By sensor slot.
    pub raw: [u16; SENSOR_COUNT],
}

impl SensorReadings {
    pub fn encode(&self) -> [u8; SENSOR_READINGS_LEN] {
        let mut out = [0; SENSOR_READINGS_LEN];
        out[0] = MessageType::SensorReadings as u8;
        out[1..5].copy_from_slice(&self.seq.to_le_bytes());
        for (slot, reading) in out[5..5 + 2 * SENSOR_COUNT].as_chunks_mut::<2>().0.iter_mut().zip(self.raw) {
            slot.copy_from_slice(&reading.to_le_bytes());
        }
        out
    }

    pub fn parse(payload: &[u8]) -> Result<Self, ParseError> {
        let p = exact::<SENSOR_READINGS_LEN>(payload, MessageType::SensorReadings)?;
        Ok(Self { seq: u32_at(p, 1), raw: core::array::from_fn(|i| u16_at(p, 5 + 2 * i)) })
    }
}

/// A message from the key scanner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[expect(clippy::large_enum_variant, reason = "no_std without an allocator, and it lives on the stack only briefly")]
pub enum ScannerMessage {
    KeyState(KeyState),
    SensorReadings(SensorReadings),
}

impl ScannerMessage {
    pub fn parse(payload: &[u8]) -> Result<Self, ParseError> {
        match MessageType::parse(*payload.first().ok_or(ParseError::Empty)?)? {
            MessageType::KeyState => KeyState::parse(payload).map(Self::KeyState),
            MessageType::SensorReadings => SensorReadings::parse(payload).map(Self::SensorReadings),
        }
    }
}

/// The Linux side's answer to every scanner message. The scanner applies the two modes from
/// every response it gets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Response {
    pub answering: MessageType,
    /// The message had the wrong length.
    pub rejected: bool,
    /// Asks the scanner to send sensor readings.
    pub sensor_mode: bool,
    pub stress_test: bool,
}

impl Response {
    pub fn encode(&self) -> [u8; RESPONSE_LEN] {
        [self.answering as u8, self.rejected as u8, self.sensor_mode as u8, self.stress_test as u8]
    }

    pub fn parse(payload: &[u8]) -> Result<Self, ParseError> {
        let p: &[u8; RESPONSE_LEN] =
            payload.try_into().map_err(|_| ParseError::WrongLength { expected: RESPONSE_LEN, got: payload.len() })?;
        Ok(Self {
            answering: MessageType::parse(p[0])?,
            rejected: flag(p[1], "rejected")?,
            sensor_mode: flag(p[2], "sensor mode")?,
            stress_test: flag(p[3], "stress test")?,
        })
    }
}

fn exact<const N: usize>(payload: &[u8], kind: MessageType) -> Result<&[u8; N], ParseError> {
    let got = MessageType::parse(*payload.first().ok_or(ParseError::Empty)?)?;
    if got != kind {
        return Err(ParseError::UnknownType(got as u8));
    }
    payload.try_into().map_err(|_| ParseError::WrongLength { expected: N, got: payload.len() })
}

fn bad(field: &'static str, value: u8) -> ParseError {
    ParseError::BadValue { field, value: value as u32 }
}

fn flag(byte: u8, field: &'static str) -> Result<bool, ParseError> {
    match byte {
        0 => Ok(false),
        1 => Ok(true),
        other => Err(bad(field, other)),
    }
}

fn array<const N: usize>(bytes: &[u8], at: usize) -> [u8; N] {
    bytes[at..at + N].try_into().unwrap()
}

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(array(bytes, at))
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(array(bytes, at))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::hdlc::tests::{hex, unhex};
    use crate::hdlc::{SERIAL_MAX_PAYLOAD, encode, max_encoded_len};

    const VECTORS: &str = include_str!("../../../firmware/vectors/serial_link.txt");

    pub(crate) fn vectors(name: &str) -> impl Iterator<Item = String> + '_ {
        VECTORS.lines().filter_map(move |line| {
            let (key, value) = line.split_once(' ').unwrap_or((line, ""));
            (key == name).then(|| value.to_string())
        })
    }

    pub(crate) fn vector(name: &str) -> String {
        vectors(name).next().unwrap_or_else(|| panic!("no vector {name}"))
    }

    /// The values `firmware/vectors/serial_link.c` gives the firmware's runtime.
    fn key_state() -> KeyState {
        KeyState {
            seq: 0x1234_7D7E,
            pressed: core::array::from_fn(|i| (i * 29 + 3) as u8),
            uptime_ms: 0xDEAD_BEEF,
            keyframe_layout: Some(KeyframeLayout::Iso),
            keyframe_attached: true,
            switch_feel: 2,
            active_layers: 0x105,
            actuation_mode: 3,
            function_events: Some(FunctionEvents {
                seq: 0x7E21,
                codes: core::array::from_fn(|i| i as u8 + 2),
                profiles: core::array::from_fn(|i| (i % 3) as u8),
            }),
            host_leds: 0x05,
            language_code: 0x0409,
            platform_code: 3,
            active_profile: 1,
            keyframe_phase: KeyframePhase::Attached,
            module_types: 0,
            module_buttons: 0,
        }
    }

    #[test]
    fn key_state_matches_the_firmware() {
        let state = key_state();
        assert_eq!(hex(&state.encode()), vector("key_state"));
        assert_eq!(KeyState::parse(&unhex(&vector("key_state"))), Ok(state));
        let mut wire = [0; max_encoded_len(KEY_STATE_LEN)];
        let n = encode::<SERIAL_MAX_PAYLOAD>(&state.encode(), &mut wire).unwrap();
        assert_eq!(hex(&wire[..n]), vector("key_state.frame"));
    }

    #[test]
    fn sensor_readings_match_the_firmware() {
        let readings = SensorReadings { seq: 0x1234_7D7F, raw: core::array::from_fn(|i| ((i * 97 + 5) & 0x0FFF) as u16) };
        assert_eq!(hex(&readings.encode()), vector("sensor_readings"));
        assert_eq!(ScannerMessage::parse(&unhex(&vector("sensor_readings"))), Ok(ScannerMessage::SensorReadings(readings)));
    }

    #[test]
    fn module_fields_round_trip() {
        let state = KeyState { module_types: 0x21, module_buttons: 0xBEEF, function_events: None, keyframe_layout: None, ..key_state() };
        assert_eq!(KeyState::parse(&state.encode()), Ok(state));
        assert!(state.is_pressed(0) && !state.is_pressed(2) && !state.is_pressed(256));
    }

    #[test]
    fn responses_take_only_what_the_firmware_takes() {
        let r = Response { answering: MessageType::KeyState, rejected: false, sensor_mode: true, stress_test: false };
        assert_eq!(r.encode(), [2, 0, 1, 0]);
        assert_eq!(Response::parse(&[2, 0, 1, 0]), Ok(r));
        assert_eq!(Response::parse(&[3, 0, 2, 0]), Err(ParseError::BadValue { field: "sensor mode", value: 2 }));
        assert_eq!(Response::parse(&[1, 0, 0, 0]), Err(ParseError::UnknownType(1)));
        assert_eq!(Response::parse(&[2, 0, 0]), Err(ParseError::WrongLength { expected: 4, got: 3 }));
    }

    #[test]
    fn malformed_messages_are_refused() {
        assert_eq!(ScannerMessage::parse(&[]), Err(ParseError::Empty));
        assert_eq!(ScannerMessage::parse(&[9]), Err(ParseError::UnknownType(9)));
        assert_eq!(ScannerMessage::parse(&[2; 10]), Err(ParseError::WrongLength { expected: KEY_STATE_LEN, got: 10 }));
        let mut bytes = key_state().encode();
        bytes[STATE + 0x6A] = 7;
        assert_eq!(KeyState::parse(&bytes), Err(ParseError::BadValue { field: "keyframe phase", value: 7 }));
    }
}
