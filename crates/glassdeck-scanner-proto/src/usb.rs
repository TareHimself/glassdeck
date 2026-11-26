//! The key scanner's USB command interface (interface 1, bulk OUT `0x01` / IN `0x84`), byte for
//! byte as the stock firmware answers it. A request is a header packet, the payload in whole
//! packets, then a zero-length packet; a response has the same shape. Integers are little-endian.

/// The key scanner's packet size (USB full speed).
pub const FIRMWARE_PACKET: usize = 64;
/// The longest request payload the key scanner takes; longer ones are read, dropped and refused.
pub const FIRMWARE_MAX_REQUEST: usize = 16384;
const HEADER_FIELDS: usize = 9;

pub const VID: u16 = 0x3761;
pub const PID: u16 = 0x4015;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Command {
    Version = 1,
    SendKeymap = 100,
    EnterBootloader = 101,
    /// Resets the keyboard's computer, keeping it powered.
    EnterOsBootloader = 102,
    GetKeymap = 103,
    SetLanguage = 104,
    SetPlatform = 105,
    SetActiveProfile = 106,
    /// Profile settings, behind a 16-byte file header.
    FileFromHost = 107,
    FileToHost = 108,
    GetRuntimeState = 109,
    PressRefinementStatus = 112,
    PutPressRefinement = 113,
    GetPressRefinement = 114,
}

impl Command {
    pub fn from_code(code: u8) -> Option<Self> {
        use Command::*;
        Some(match code {
            1 => Version,
            100 => SendKeymap,
            101 => EnterBootloader,
            102 => EnterOsBootloader,
            103 => GetKeymap,
            104 => SetLanguage,
            105 => SetPlatform,
            106 => SetActiveProfile,
            107 => FileFromHost,
            108 => FileToHost,
            109 => GetRuntimeState,
            112 => PressRefinementStatus,
            113 => PutPressRefinement,
            114 => GetPressRefinement,
            _ => return None,
        })
    }

    /// Whether a request announcing `bytes` of payload is worth receiving. The key scanner
    /// checks this once the header says a payload follows.
    fn takes_payload(code: u8, bytes: u32, max: usize) -> bool {
        match Self::from_code(code) {
            Some(Self::PutPressRefinement) => bytes as usize == REFINEMENT_RECORD_LEN,
            Some(Self::PressRefinementStatus | Self::GetPressRefinement) => false,
            _ => bytes as usize <= max,
        }
    }
}

/// A request's command byte: one this key scanner knows, or one it answers with a NAK.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandCode {
    Known(Command),
    Unknown(u8),
}

impl CommandCode {
    pub fn from_code(code: u8) -> Self {
        Command::from_code(code).map_or(Self::Unknown(code), Self::Known)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Reply {
    Nak = 0,
    Ack = 1,
}

/// The first packet of a request or response.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    /// A command number in a request, a [`Reply`] in a response.
    pub code: u8,
    pub packets: u32,
    pub bytes: u32,
}

impl Header {
    pub fn for_payload<const P: usize>(code: u8, len: usize) -> Self {
        Self {
            code,
            packets: len.div_ceil(P) as u32,
            bytes: len as u32,
        }
    }

    pub fn encode<const P: usize>(&self) -> [u8; P] {
        let mut out = [0; P];
        out[0] = self.code;
        out[1..5].copy_from_slice(&self.packets.to_le_bytes());
        out[5..9].copy_from_slice(&self.bytes.to_le_bytes());
        out
    }

    /// Reads a header from the start of `packet`; `None` when it's too short to hold one.
    pub fn parse(packet: &[u8]) -> Option<Self> {
        let fields: &[u8; HEADER_FIELDS] = packet.get(..HEADER_FIELDS)?.try_into().ok()?;
        Some(Self {
            code: fields[0],
            packets: u32::from_le_bytes(fields[1..5].try_into().unwrap()),
            bytes: u32::from_le_bytes(fields[5..9].try_into().unwrap()),
        })
    }

    fn packet_count_matches<const P: usize>(&self) -> bool {
        self.packets as u64 == (self.bytes as u64).div_ceil(P as u64)
    }
}

/// The packets of a response: the header, then the payload zero-padded to whole packets. The
/// caller ends the transfer with a zero-length packet.
pub fn response_packets<const P: usize>(
    reply: Reply,
    payload: &[u8],
) -> impl Iterator<Item = [u8; P]> + '_ {
    let header = Header::for_payload::<P>(reply as u8, payload.len()).encode::<P>();
    core::iter::once(header).chain(payload.chunks(P).map(|chunk| {
        let mut packet = [0; P];
        packet[..chunk.len()].copy_from_slice(chunk);
        packet
    }))
}

/// Where the receiver is within a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Idle,
    Receiving {
        code: u8,
        packets: u32,
        bytes: u32,
        got_packets: u32,
        got_bytes: u32,
    },
    /// Reading the packets of a request that will be refused, so the next header lines up.
    Discarding {
        packets: u32,
        got: u32,
    },
}

/// What one packet does. `Store` copies `len` bytes from the packet's start to `at` in the payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Next(Phase),
    Store {
        at: u32,
        len: u32,
        next: Phase,
    },
    StoreAndComplete {
        at: u32,
        len: u32,
        code: u8,
        bytes: u32,
    },
    Complete {
        code: u8,
    },
    Refuse,
}

fn step<const P: usize, const MAX: usize>(phase: Phase, packet: &[u8]) -> Step {
    match phase {
        Phase::Idle => match Header::parse(packet) {
            None => Step::Refuse,
            Some(h) if !h.packet_count_matches::<P>() => Step::Refuse,
            Some(h) if h.packets == 0 => Step::Complete { code: h.code },
            Some(h) if !Command::takes_payload(h.code, h.bytes, MAX) => {
                Step::Next(Phase::Discarding {
                    packets: h.packets,
                    got: 0,
                })
            }
            Some(h) => Step::Next(Phase::Receiving {
                code: h.code,
                packets: h.packets,
                bytes: h.bytes,
                got_packets: 0,
                got_bytes: 0,
            }),
        },
        Phase::Discarding { packets, got } if got + 1 >= packets => Step::Refuse,
        Phase::Discarding { packets, got } => Step::Next(Phase::Discarding {
            packets,
            got: got + 1,
        }),
        Phase::Receiving {
            code,
            packets,
            bytes,
            got_packets,
            got_bytes,
        } => {
            let len = (packet.len() as u32).min(bytes - got_bytes);
            let (got_packets, now_bytes) = (got_packets + 1, got_bytes + len);
            if got_packets < packets {
                let next = Phase::Receiving {
                    code,
                    packets,
                    bytes,
                    got_packets,
                    got_bytes: now_bytes,
                };
                Step::Store {
                    at: got_bytes,
                    len,
                    next,
                }
            } else if now_bytes == bytes {
                Step::StoreAndComplete {
                    at: got_bytes,
                    len,
                    code,
                    bytes,
                }
            } else {
                Step::Refuse
            }
        }
    }
}

/// What a packet completed.
#[derive(Debug, PartialEq, Eq)]
pub enum Received<'a> {
    Pending,
    Request {
        command: CommandCode,
        payload: &'a [u8],
    },
    /// Answer with a NAK.
    Refused,
}

/// Collects requests of up to `MAX` payload bytes from packets of `P` bytes. Zero-length packets
/// are ignored; a packet shorter than `P` still counts as one, leaving the payload short.
pub struct Receiver<const P: usize, const MAX: usize> {
    phase: Phase,
    payload: [u8; MAX],
}

impl<const P: usize, const MAX: usize> Default for Receiver<P, MAX> {
    fn default() -> Self {
        Self {
            phase: Phase::Idle,
            payload: [0; MAX],
        }
    }
}

impl<const P: usize, const MAX: usize> Receiver<P, MAX> {
    pub fn push(&mut self, packet: &[u8]) -> Received<'_> {
        if packet.is_empty() {
            return Received::Pending;
        }
        let step = step::<P, MAX>(self.phase, packet);
        self.phase = match step {
            Step::Next(next) | Step::Store { next, .. } => next,
            Step::StoreAndComplete { .. } | Step::Complete { .. } | Step::Refuse => Phase::Idle,
        };
        match step {
            Step::Next(_) => Received::Pending,
            Step::Store { at, len, .. } => {
                self.store(at, len, packet);
                Received::Pending
            }
            Step::StoreAndComplete {
                at,
                len,
                code,
                bytes,
            } => {
                self.store(at, len, packet);
                Received::Request {
                    command: CommandCode::from_code(code),
                    payload: &self.payload[..bytes as usize],
                }
            }
            Step::Complete { code } => Received::Request {
                command: CommandCode::from_code(code),
                payload: &[],
            },
            Step::Refuse => Received::Refused,
        }
    }

    /// Drops a partly received request without answering it, after a USB reset or a stall.
    pub fn reset(&mut self) {
        self.phase = Phase::Idle;
    }

    pub fn is_busy(&self) -> bool {
        self.phase != Phase::Idle
    }

    fn store(&mut self, at: u32, len: u32, packet: &[u8]) {
        let (at, len) = (at as usize, len as usize);
        self.payload[at..at + len].copy_from_slice(&packet[..len]);
    }
}

/// The argument of `SET_LANGUAGE`, `SET_PLATFORM` and `SET_ACTIVE_PROFILE`. Like the stock
/// firmware, bytes after the first four are ignored.
pub fn u32_argument(payload: &[u8]) -> Option<u32> {
    Some(u32::from_le_bytes(payload.get(..4)?.try_into().ok()?))
}

fn word(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

/// The `GET_RUNTIME_STATE` answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RuntimeState {
    pub active_profile: u32,
    pub actuation_mode: u32,
    pub platform_code: u32,
}

impl RuntimeState {
    pub const LEN: usize = 12;

    pub fn encode(&self) -> [u8; Self::LEN] {
        words([self.active_profile, self.actuation_mode, self.platform_code])
    }

    pub fn parse(payload: &[u8]) -> Option<Self> {
        let p: &[u8; Self::LEN] = payload.try_into().ok()?;
        Some(Self {
            active_profile: word(p, 0),
            actuation_mode: word(p, 4),
            platform_code: word(p, 8),
        })
    }
}

fn words<const N: usize, const BYTES: usize>(values: [u32; N]) -> [u8; BYTES] {
    const { assert!(N * 4 == BYTES) };
    let mut out = [0; BYTES];
    for (slot, value) in out.as_chunks_mut::<4>().0.iter_mut().zip(values) {
        *slot = value.to_le_bytes();
    }
    out
}

pub const REFINEMENT_FORMAT: u32 = 1;
pub const REFINEMENT_HEADER_LEN: usize = 16;
pub const REFINEMENT_PAYLOAD_LEN: usize = 1456;
pub const REFINEMENT_RECORD_LEN: usize = REFINEMENT_HEADER_LEN + REFINEMENT_PAYLOAD_LEN;
const REFINEMENT_MAGIC: &[u8; 4] = b"OFRM";

/// A press-refinement record (`OFRM`) whose header and payload CRC check out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RefinementRecord<'a> {
    bytes: &'a [u8; REFINEMENT_RECORD_LEN],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefinementError {
    WrongLength(usize),
    BadMagic,
    UnsupportedFormat(u32),
    WrongPayloadLength(u32),
    BadCrc,
}

impl<'a> RefinementRecord<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, RefinementError> {
        let bytes: &[u8; REFINEMENT_RECORD_LEN] = bytes
            .try_into()
            .map_err(|_| RefinementError::WrongLength(bytes.len()))?;
        if &bytes[..4] != REFINEMENT_MAGIC {
            return Err(RefinementError::BadMagic);
        }
        if word(bytes, 4) != REFINEMENT_FORMAT {
            return Err(RefinementError::UnsupportedFormat(word(bytes, 4)));
        }
        if word(bytes, 8) as usize != REFINEMENT_PAYLOAD_LEN {
            return Err(RefinementError::WrongPayloadLength(word(bytes, 8)));
        }
        let record = Self { bytes };
        if zlib_crc32(record.payload()) != record.payload_crc() {
            return Err(RefinementError::BadCrc);
        }
        Ok(record)
    }

    pub fn bytes(&self) -> &'a [u8; REFINEMENT_RECORD_LEN] {
        self.bytes
    }

    pub fn payload(&self) -> &'a [u8] {
        &self.bytes[REFINEMENT_HEADER_LEN..]
    }

    pub fn payload_crc(&self) -> u32 {
        word(self.bytes, 12)
    }
}

/// What `PRESS_REFINEMENT_STATUS` reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefinementState {
    Absent,
    /// Stored; `active` when the scan uses it.
    Installed {
        active: bool,
        payload_crc: u32,
    },
    Invalid,
    StorageUnavailable,
}

impl RefinementState {
    pub const LEN: usize = 16;

    pub fn encode(&self) -> [u8; Self::LEN] {
        let installed = REFINEMENT_PAYLOAD_LEN as u32;
        let (state, len, crc) = match *self {
            Self::Absent => (0, 0, 0),
            Self::Installed {
                active: true,
                payload_crc,
            } => (1, installed, payload_crc),
            Self::Invalid => (2, 0, 0),
            Self::StorageUnavailable => (3, 0, 0),
            Self::Installed {
                active: false,
                payload_crc,
            } => (4, installed, payload_crc),
        };
        words([REFINEMENT_FORMAT, state, len, crc])
    }

    pub fn parse(payload: &[u8]) -> Option<Self> {
        let p: &[u8; Self::LEN] = payload.try_into().ok()?;
        if word(p, 0) != REFINEMENT_FORMAT {
            return None;
        }
        let installed = |active| {
            (word(p, 8) as usize == REFINEMENT_PAYLOAD_LEN).then_some(Self::Installed {
                active,
                payload_crc: word(p, 12),
            })
        };
        match word(p, 4) {
            0 => Some(Self::Absent),
            1 => installed(true),
            2 => Some(Self::Invalid),
            3 => Some(Self::StorageUnavailable),
            4 => installed(false),
            _ => None,
        }
    }
}

const ZLIB_CRC_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut crc = i as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
            bit += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
};

/// CRC-32/ISO-HDLC, as `zlib.crc32` computes it. Not the serial link's CRC.
pub fn zlib_crc32(data: &[u8]) -> u32 {
    !data.iter().fold(!0u32, |crc, &byte| {
        ZLIB_CRC_TABLE[((crc ^ byte as u32) & 0xFF) as usize] ^ (crc >> 8)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Vectors, hex, unhex};

    const VECTORS: Vectors = Vectors(include_str!("../../../firmware/vectors/usb_commands.txt"));
    type FirmwareReceiver = Receiver<FIRMWARE_PACKET, FIRMWARE_MAX_REQUEST>;

    /// Answers as the C firmware does in `firmware/vectors/usb_commands.c`: runtime state
    /// (2, 1, 3), no keymap stored, refinement installed but never activated.
    #[derive(Default)]
    struct VectorScanner {
        refinement: Option<Vec<u8>>,
    }

    impl VectorScanner {
        fn answer(&mut self, command: CommandCode, payload: &[u8]) -> (Reply, Vec<u8>) {
            let ack = |data: &[u8]| (Reply::Ack, data.to_vec());
            let nak = (Reply::Nak, Vec::new());
            let CommandCode::Known(command) = command else {
                return nak;
            };
            match command {
                Command::Version => ack(b"openflux-firmware vectors\0"),
                Command::GetRuntimeState if payload.is_empty() => ack(&RuntimeState {
                    active_profile: 2,
                    actuation_mode: 1,
                    platform_code: 3,
                }
                .encode()),
                Command::SetPlatform if u32_argument(payload).is_some() => ack(&[]),
                Command::PressRefinementStatus => ack(&match &self.refinement {
                    None => RefinementState::Absent.encode(),
                    Some(record) => {
                        let payload_crc = RefinementRecord::parse(record).unwrap().payload_crc();
                        RefinementState::Installed {
                            active: false,
                            payload_crc,
                        }
                        .encode()
                    }
                }),
                Command::GetPressRefinement => self.refinement.as_deref().map_or(nak, ack),
                Command::PutPressRefinement => match RefinementRecord::parse(payload) {
                    Ok(record) => {
                        self.refinement = Some(record.bytes().to_vec());
                        ack(&[])
                    }
                    Err(_) => nak,
                },
                _ => nak,
            }
        }
    }

    fn respond(scanner: &mut VectorScanner, received: Received<'_>) -> Option<String> {
        let (reply, data) = match received {
            Received::Pending => return None,
            Received::Refused => (Reply::Nak, Vec::new()),
            Received::Request { command, payload } => scanner.answer(command, payload),
        };
        let packets: Vec<String> = response_packets::<FIRMWARE_PACKET>(reply, &data)
            .map(|p| hex(&p))
            .collect();
        Some(packets.join("|") + "|-")
    }

    #[test]
    fn every_request_gets_the_firmware_answer() {
        let mut receiver = FirmwareReceiver::default();
        let mut scanner = VectorScanner::default();
        let cases = VECTORS
            .0
            .lines()
            .filter_map(|line| line.split_once(".request "))
            .collect::<Vec<_>>();
        assert_eq!(cases.len(), 17);
        for (name, request) in cases {
            let answers: Vec<String> = request
                .split('|')
                .filter_map(|packet| respond(&mut scanner, receiver.push(&unhex(packet))))
                .collect();
            assert_eq!(
                answers,
                [VECTORS.get(&format!("{name}.response"))],
                "{name}"
            );
        }
    }

    #[test]
    fn an_oversized_request_is_read_to_the_end_then_refused() {
        let mut receiver = FirmwareReceiver::default();
        let header = unhex(VECTORS.get("too_large.header"));
        let packets = Header::parse(&header).unwrap().packets;
        assert_eq!(receiver.push(&header), Received::Pending);
        for _ in 1..packets {
            assert_eq!(receiver.push(&[0; FIRMWARE_PACKET]), Received::Pending);
            assert!(receiver.is_busy());
        }
        let last = respond(
            &mut VectorScanner::default(),
            receiver.push(&[0; FIRMWARE_PACKET]),
        )
        .unwrap();
        assert_eq!(format!("last:{last}"), VECTORS.get("too_large.response"));
        assert!(!receiver.is_busy());
    }

    #[test]
    fn a_reset_drops_a_partial_request() {
        let mut receiver = FirmwareReceiver::default();
        let header = Header::for_payload::<FIRMWARE_PACKET>(Command::SendKeymap as u8, 100);
        assert_eq!(
            receiver.push(&header.encode::<FIRMWARE_PACKET>()),
            Received::Pending
        );
        assert!(receiver.is_busy());
        receiver.reset();
        let version = Header::for_payload::<FIRMWARE_PACKET>(Command::Version as u8, 0);
        assert_eq!(
            receiver.push(&version.encode::<FIRMWARE_PACKET>()),
            Received::Request {
                command: CommandCode::Known(Command::Version),
                payload: &[]
            }
        );
    }

    #[test]
    fn a_multi_packet_payload_arrives_whole() {
        let mut receiver = FirmwareReceiver::default();
        let payload: Vec<u8> = (0..200).map(|i| i as u8).collect();
        let request = response_packets::<FIRMWARE_PACKET>(Reply::Ack, &payload).collect::<Vec<_>>();
        let mut header = request[0];
        header[0] = Command::SendKeymap as u8;
        assert_eq!(receiver.push(&header), Received::Pending);
        for packet in &request[1..request.len() - 1] {
            assert_eq!(receiver.push(packet), Received::Pending);
        }
        assert_eq!(receiver.push(&[]), Received::Pending);
        assert_eq!(
            receiver.push(request.last().unwrap()),
            Received::Request {
                command: CommandCode::Known(Command::SendKeymap),
                payload: &payload
            }
        );
    }

    #[test]
    fn payload_types_round_trip() {
        let state = RuntimeState {
            active_profile: 7,
            actuation_mode: 2,
            platform_code: 1,
        };
        assert_eq!(RuntimeState::parse(&state.encode()), Some(state));
        for refinement in [
            RefinementState::Absent,
            RefinementState::Installed {
                active: true,
                payload_crc: 0xDEAD_BEEF,
            },
            RefinementState::Installed {
                active: false,
                payload_crc: 1,
            },
            RefinementState::Invalid,
            RefinementState::StorageUnavailable,
        ] {
            assert_eq!(
                RefinementState::parse(&refinement.encode()),
                Some(refinement)
            );
        }
        assert_eq!(u32_argument(&[1, 0, 0, 0, 9]), Some(1));
        assert_eq!(u32_argument(&[1, 0]), None);
    }

    #[test]
    fn zlib_crc_matches_its_check_value() {
        assert_eq!(zlib_crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn bad_refinement_records_are_refused() {
        let request = VECTORS.get("put_refinement.request");
        let record: Vec<u8> = request
            .split('|')
            .skip(1)
            .flat_map(unhex)
            .take(REFINEMENT_RECORD_LEN)
            .collect();
        assert!(RefinementRecord::parse(&record).is_ok());
        let mut broken = record.clone();
        broken[0] = b'X';
        assert_eq!(
            RefinementRecord::parse(&broken),
            Err(RefinementError::BadMagic)
        );
        let mut broken = record.clone();
        broken[4] = 2;
        assert_eq!(
            RefinementRecord::parse(&broken),
            Err(RefinementError::UnsupportedFormat(2))
        );
        let mut broken = record.clone();
        broken[100] ^= 1;
        assert_eq!(
            RefinementRecord::parse(&broken),
            Err(RefinementError::BadCrc)
        );
        assert_eq!(
            RefinementRecord::parse(&record[1..]),
            Err(RefinementError::WrongLength(REFINEMENT_RECORD_LEN - 1))
        );
    }
}
