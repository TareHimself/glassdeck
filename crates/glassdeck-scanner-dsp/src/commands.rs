//! Answers the USB commands (Keyglass `fw_usb_cmd.c` `dispatch_command`): each request either
//! changes the runtime and saves it, or reads it back. Storage is behind [`Storage`], so this runs
//! the same on the board and in tests.

use glassdeck_scanner_proto::keymap::{Keymap, ProfileSettings};
use glassdeck_scanner_proto::usb::{
    Command, CommandCode, REFINEMENT_HEADER_LEN, REFINEMENT_RECORD_LEN, RefinementRecord,
    RefinementState, Reply, RuntimeState, u32_argument,
};

use crate::refine::MODEL_LEN;
use crate::scanner::Runtime;

/// Where commands save what they change. Each returns whether the write succeeded; the
/// runtime only changes after a successful save.
pub trait Storage {
    fn save_keymap(&mut self, blob: &[u8]) -> bool;
    /// `upload` still has its 16-byte file header.
    fn save_profile_settings(&mut self, upload: &[u8]) -> bool;
    fn save_language(&mut self, code: u32) -> bool;
    fn save_platform(&mut self, code: u32) -> bool;
    /// Reads the stored refinement record into `record`; only [`StoredRefinement::Installed`]
    /// leaves a record that checks out.
    fn read_refinement(&mut self, record: &mut [u8; REFINEMENT_RECORD_LEN]) -> StoredRefinement;
    /// Replaces the stored refinement record in one step.
    fn install_refinement(&mut self, record: &RefinementRecord<'_>) -> bool;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoredRefinement {
    Absent,
    Installed,
    Invalid,
    Unavailable,
}

/// What the firmware does once the answer has gone out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AfterAnswer {
    Nothing,
    /// Reboot into the RP2040's USB boot ROM.
    EnterBootloader,
    /// Reset the keyboard's computer, keeping it powered.
    ResetComputer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Answer {
    pub reply: Reply,
    /// Payload bytes written to the start of `out`.
    pub len: usize,
    pub then: AfterAnswer,
}

impl Answer {
    const NAK: Self = Self {
        reply: Reply::Nak,
        len: 0,
        then: AfterAnswer::Nothing,
    };

    fn ack(len: usize) -> Self {
        Self {
            reply: Reply::Ack,
            len,
            then: AfterAnswer::Nothing,
        }
    }

    fn ack_if(ok: bool) -> Self {
        if ok { Self::ack(0) } else { Self::NAK }
    }
}

/// Answers one complete request, writing any reply payload to `out`. `version` is the
/// `VERSION` answer, NUL included.
pub fn answer<const MAX: usize>(
    rt: &mut Runtime,
    storage: &mut impl Storage,
    version: &[u8],
    command: CommandCode,
    payload: &[u8],
    out: &mut [u8; MAX],
) -> Answer {
    let CommandCode::Known(command) = command else {
        return Answer::NAK;
    };
    match command {
        Command::Version => {
            out[..version.len()].copy_from_slice(version);
            Answer::ack(version.len())
        }
        Command::SendKeymap => Answer::ack_if(
            Keymap::parse(payload).is_ok()
                && storage.save_keymap(payload)
                && rt.load_keymap(payload).is_ok(),
        ),
        Command::GetKeymap if rt.keymap_len > 0 => {
            out[..rt.keymap_len].copy_from_slice(&rt.keymap_blob[..rt.keymap_len]);
            Answer::ack(rt.keymap_len)
        }
        Command::GetKeymap => Answer::NAK,
        Command::EnterBootloader => Answer {
            then: AfterAnswer::EnterBootloader,
            ..Answer::ack(0)
        },
        Command::EnterOsBootloader => Answer {
            then: AfterAnswer::ResetComputer,
            ..Answer::ack(0)
        },
        Command::SetLanguage => match u32_argument(payload) {
            Some(code) if storage.save_language(code) => {
                rt.language_code = code;
                Answer::ack(0)
            }
            _ => Answer::NAK,
        },
        Command::SetPlatform => match u32_argument(payload) {
            Some(code) if storage.save_platform(code) => {
                rt.platform_code = code;
                Answer::ack(0)
            }
            _ => Answer::NAK,
        },
        Command::SetActiveProfile => match u32_argument(payload) {
            Some(index) => {
                rt.set_active_profile(index);
                Answer::ack(0)
            }
            None => Answer::NAK,
        },
        Command::PressRefinementStatus | Command::GetPressRefinement if !payload.is_empty() => {
            Answer::NAK
        }
        Command::PressRefinementStatus => {
            let state = refinement_state(rt, storage, out);
            out[..RefinementState::LEN].copy_from_slice(&state.encode());
            Answer::ack(RefinementState::LEN)
        }
        Command::GetPressRefinement => {
            let record: &mut [u8; REFINEMENT_RECORD_LEN] =
                (&mut out[..REFINEMENT_RECORD_LEN]).try_into().unwrap();
            match storage.read_refinement(record) {
                StoredRefinement::Installed => Answer::ack(REFINEMENT_RECORD_LEN),
                _ => Answer::NAK,
            }
        }
        Command::PutPressRefinement => match RefinementRecord::parse(payload) {
            Ok(record) if storage.install_refinement(&record) => {
                let model: &[u8; MODEL_LEN] = record.payload().try_into().unwrap();
                rt.install_refinement(model);
                Answer::ack(0)
            }
            _ => Answer::NAK,
        },
        Command::FileFromHost => Answer::ack_if(
            ProfileSettings::parse_upload(payload).is_ok()
                && storage.save_profile_settings(payload)
                && rt.load_profile_settings(payload).is_ok(),
        ),
        Command::FileToHost if u32_argument(payload).is_some_and(|file| file != 0) => Answer::NAK,
        Command::FileToHost => {
            let export: &mut [u8; ProfileSettings::MAX_EXPORT_LEN] = (&mut out
                [..ProfileSettings::MAX_EXPORT_LEN])
                .try_into()
                .unwrap();
            Answer::ack(rt.profile_settings().encode_export(export))
        }
        Command::GetRuntimeState if payload.is_empty() => {
            let state = RuntimeState {
                active_profile: rt.active_profile,
                actuation_mode: rt.active_actuation_mode,
                platform_code: rt.platform_code,
            };
            out[..RuntimeState::LEN].copy_from_slice(&state.encode());
            Answer::ack(RuntimeState::LEN)
        }
        Command::GetRuntimeState => Answer::NAK,
    }
}

/// Reads the stored record into `scratch` to report on it.
fn refinement_state<const MAX: usize>(
    rt: &Runtime,
    storage: &mut impl Storage,
    scratch: &mut [u8; MAX],
) -> RefinementState {
    let record: &mut [u8; REFINEMENT_RECORD_LEN] =
        (&mut scratch[..REFINEMENT_RECORD_LEN]).try_into().unwrap();
    match storage.read_refinement(record) {
        StoredRefinement::Absent => RefinementState::Absent,
        StoredRefinement::Invalid => RefinementState::Invalid,
        StoredRefinement::Unavailable => RefinementState::StorageUnavailable,
        StoredRefinement::Installed => RefinementState::Installed {
            active: rt.refinement_active,
            payload_crc: u32::from_le_bytes(record[12..REFINEMENT_HEADER_LEN].try_into().unwrap()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glassdeck_scanner_proto::usb::{
        FIRMWARE_MAX_REQUEST, FIRMWARE_PACKET, Received, Receiver, response_packets,
    };

    const VECTORS: &str = include_str!("../../../firmware/vectors/usb_commands.txt");
    const KEYMAPS: &str = include_str!("../../../firmware/vectors/keymap.txt");

    fn vector<'a>(text: &'a str, name: &str) -> &'a str {
        text.lines()
            .find_map(|l| l.strip_prefix(name)?.strip_prefix(' '))
            .unwrap_or_else(|| panic!("no vector {name}"))
    }

    fn unhex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
            .collect()
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[derive(Default)]
    struct Flash {
        keymap: Option<Vec<u8>>,
        profiles: Option<Vec<u8>>,
        language: Option<u32>,
        platform: Option<u32>,
        refinement: Option<Vec<u8>>,
        broken: bool,
    }

    impl Storage for Flash {
        fn save_keymap(&mut self, blob: &[u8]) -> bool {
            self.keymap = Some(blob.to_vec());
            !self.broken
        }
        fn save_profile_settings(&mut self, upload: &[u8]) -> bool {
            self.profiles = Some(upload[16..].to_vec());
            !self.broken
        }
        fn save_language(&mut self, code: u32) -> bool {
            self.language = Some(code);
            !self.broken
        }
        fn save_platform(&mut self, code: u32) -> bool {
            self.platform = Some(code);
            !self.broken
        }
        fn read_refinement(
            &mut self,
            record: &mut [u8; REFINEMENT_RECORD_LEN],
        ) -> StoredRefinement {
            match &self.refinement {
                None => StoredRefinement::Absent,
                Some(r) => {
                    record.copy_from_slice(r);
                    StoredRefinement::Installed
                }
            }
        }
        fn install_refinement(&mut self, record: &RefinementRecord<'_>) -> bool {
            self.refinement = Some(record.bytes().to_vec());
            !self.broken
        }
    }

    struct Scanner {
        rt: Box<Runtime>,
        flash: Flash,
        receiver: Box<Receiver<FIRMWARE_PACKET, FIRMWARE_MAX_REQUEST>>,
        out: Box<[u8; FIRMWARE_MAX_REQUEST]>,
    }

    impl Scanner {
        fn new() -> Self {
            Self {
                rt: Box::default(),
                flash: Flash::default(),
                receiver: Box::default(),
                out: Box::new([0; FIRMWARE_MAX_REQUEST]),
            }
        }

        /// Feeds request packets and returns each response as the vectors print it.
        fn feed(&mut self, packets: &[Vec<u8>]) -> Vec<String> {
            let mut answers = vec![];
            for packet in packets {
                let answer = match self.receiver.push(packet) {
                    Received::Pending => continue,
                    Received::Refused => Answer::NAK,
                    Received::Request { command, payload } => answer(
                        &mut self.rt,
                        &mut self.flash,
                        b"openflux-firmware vectors\0",
                        command,
                        payload,
                        &mut self.out,
                    ),
                };
                answers.extend(answered(answer.reply, &self.out[..answer.len]));
            }
            answers
        }

        fn request(&mut self, command: Command, payload: &[u8]) -> Vec<String> {
            let packets: Vec<Vec<u8>> = response_packets::<FIRMWARE_PACKET>(Reply::Ack, payload)
                .enumerate()
                .map(|(i, mut p)| {
                    if i == 0 {
                        p[0] = command as u8;
                    }
                    p.to_vec()
                })
                .collect();
            self.feed(&packets)
        }
    }

    fn ack(payload: &[u8]) -> Vec<String> {
        answered(Reply::Ack, payload)
    }

    fn nak() -> Vec<String> {
        answered(Reply::Nak, &[])
    }

    fn answered(reply: Reply, payload: &[u8]) -> Vec<String> {
        let packets: Vec<String> = response_packets::<FIRMWARE_PACKET>(reply, payload)
            .map(|p| hex(&p))
            .collect();
        vec![packets.join("|") + "|-"]
    }

    #[test]
    fn requests_get_the_stock_answers() {
        let mut s = Scanner::new();
        s.rt.active_profile = 2;
        s.rt.active_actuation_mode = 1;
        s.rt.platform_code = 3;
        let cases: Vec<(&str, &str)> = VECTORS
            .lines()
            .filter_map(|line| line.split_once(".request "))
            .collect();
        assert_eq!(cases.len(), 17);
        for (name, request) in cases {
            let packets: Vec<Vec<u8>> = request.split('|').map(unhex).collect();
            let got = s.feed(&packets);
            if name == "refinement_status.installed" {
                // The vector ran without the firmware's hooks, so its runtime never took the
                // model; the firmware's runtime does, and reports it active.
                assert!(s.rt.refinement_active);
                let crc = u32::from_le_bytes(
                    s.flash.refinement.as_ref().unwrap()[12..16]
                        .try_into()
                        .unwrap(),
                );
                let active = RefinementState::Installed {
                    active: true,
                    payload_crc: crc,
                };
                assert_eq!(got, ack(&active.encode()), "{name}");
                continue;
            }
            assert_eq!(
                got,
                [vector(VECTORS, &format!("{name}.response"))],
                "{name}"
            );
        }
    }

    #[test]
    fn a_keymap_is_saved_loaded_and_read_back() {
        let mut s = Scanner::new();
        let blob = unhex(vector(KEYMAPS, "keymap.all"));
        assert_eq!(s.request(Command::GetKeymap, &[]), nak());
        assert_eq!(s.request(Command::SendKeymap, &blob), ack(&[]));
        assert_eq!(s.flash.keymap.as_deref(), Some(&blob[..]));
        assert_eq!(s.request(Command::GetKeymap, &[]), ack(&blob));

        let refused = unhex(vector(KEYMAPS, "keymap.short"));
        assert_eq!(s.request(Command::SendKeymap, &refused), nak());
        assert_eq!(s.flash.keymap.as_deref(), Some(&blob[..]));
    }

    #[test]
    fn nothing_changes_when_a_save_fails() {
        let mut s = Scanner::new();
        s.flash.broken = true;
        let blob = unhex(vector(KEYMAPS, "keymap.ansi"));
        assert_eq!(s.request(Command::SendKeymap, &blob), nak());
        assert_eq!(s.rt.keymap_len, 0);
        assert_eq!(s.request(Command::SetLanguage, &[9, 0, 0, 0]), nak());
        assert_eq!(s.rt.language_code, 0);
    }

    #[test]
    fn settings_commands_save_then_apply() {
        let mut s = Scanner::new();
        assert_eq!(s.request(Command::SetLanguage, &[9, 0, 0, 0]), ack(&[]));
        assert_eq!((s.flash.language, s.rt.language_code), (Some(9), 9));
        assert_eq!(
            s.request(Command::SetActiveProfile, &[70, 0, 0, 0]),
            ack(&[])
        );
        assert_eq!(s.rt.active_profile, 70);
        assert_eq!(s.request(Command::SetActiveProfile, &[1]), nak());
        assert_eq!(s.request(Command::FileToHost, &[1, 0, 0, 0]), nak());
        let mut export = [0; ProfileSettings::MAX_EXPORT_LEN];
        let len = s.rt.profile_settings().encode_export(&mut export);
        assert_eq!(s.request(Command::FileToHost, &[]), ack(&export[..len]));
    }

    #[test]
    fn the_bootloader_commands_answer_first() {
        let mut s = Scanner::new();
        let mut out = [0u8; 64];
        for (command, then) in [
            (Command::EnterBootloader, AfterAnswer::EnterBootloader),
            (Command::EnterOsBootloader, AfterAnswer::ResetComputer),
        ] {
            let a = answer(
                &mut s.rt,
                &mut s.flash,
                b"v\0",
                CommandCode::Known(command),
                &[],
                &mut out,
            );
            assert_eq!(
                a,
                Answer {
                    reply: Reply::Ack,
                    len: 0,
                    then
                }
            );
        }
    }
}
