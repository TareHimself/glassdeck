//! Talks to the key scanner (the keyboard's RP2040) over its USB command interface: read what
//! it runs, back up its settings, and flash a new firmware through the RP2040's boot ROM.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use glassdeck_scanner_proto::keymap::ProfileSettings;
use glassdeck_scanner_proto::usb::{
    Command, FIRMWARE_PACKET, Header, PID, REFINEMENT_RECORD_LEN, RefinementState, Reply,
    RuntimeState, VID,
};
use glassdeck_scanner_proto::usb_descriptors::{
    EP_COMMANDS_IN, EP_COMMANDS_OUT, INTERFACE_COMMANDS,
};
use nusb::MaybeFuture;
use nusb::transfer::{Buffer, Bulk, In, Out};

const TIMEOUT: Duration = Duration::from_secs(2);
const BOOT_ROM_VID: u16 = 0x2E8A;
const BOOT_ROM_PID: u16 = 0x0003;
/// How long the boot ROM's drive may take to appear after the reboot.
const DRIVE_WAIT: Duration = Duration::from_secs(20);
/// The RP2040 boot ROM's drive holds this file.
const DRIVE_MARKER: &str = "INFO_UF2.TXT";

#[derive(Subcommand)]
pub enum ScannerCommand {
    /// The firmware the key scanner reports, and its active profile, mode and platform
    Version,
    /// Save the keymap, profile settings and press-refinement model to a new folder
    Backup {
        /// Created; must not exist yet
        dir: PathBuf,
    },
    /// Reboot the key scanner into its USB boot ROM (the RPI-RP2 drive)
    Bootloader,
    /// Reboot into the boot ROM and copy a .uf2 firmware onto its drive
    Flash {
        /// For example firmware/target/thumbv6m-none-eabi/release/glassdeck-scanner.uf2
        uf2: PathBuf,
    },
}

pub fn run(command: ScannerCommand) -> Result<()> {
    match command {
        ScannerCommand::Version => version(&mut UsbPipe::open()?),
        ScannerCommand::Backup { dir } => backup(&mut UsbPipe::open()?, &dir),
        ScannerCommand::Bootloader => {
            enter_bootloader(&mut UsbPipe::open()?)?;
            let drive = wait_for_drive()?;
            println!(
                "The boot ROM's drive is {}; copy a .uf2 onto it to flash",
                drive.display()
            );
            Ok(())
        }
        ScannerCommand::Flash { uf2 } => flash(&uf2),
    }
}

/// One request/response exchange on the command interface.
trait Pipe {
    fn send(&mut self, bytes: &[u8]) -> Result<()>;
    /// One IN transfer of up to a packet; may be empty (a zero-length packet).
    fn receive(&mut self) -> Result<Vec<u8>>;
}

struct UsbPipe {
    out: nusb::Endpoint<Bulk, Out>,
    input: nusb::Endpoint<Bulk, In>,
}

impl UsbPipe {
    fn open() -> Result<Self> {
        let mut found: Vec<_> = nusb::list_devices()
            .wait()
            .context("listing USB devices")?
            .filter(|d| d.vendor_id() == VID && d.product_id() == PID)
            .collect();
        let info = match found.len() {
            0 => bail!(
                "no key scanner on USB (looked for {VID:04x}:{PID:04x}); is the keyboard plugged in?"
            ),
            1 => found.remove(0),
            n => bail!("{n} key scanners are plugged in; unplug all but one"),
        };
        let device = info.open().wait().context("opening the key scanner")?;
        let interface = device.claim_interface(INTERFACE_COMMANDS).wait().context(
            "claiming the command interface; on Windows it must be bound to WinUSB (the firmware asks for that itself)",
        )?;
        Ok(Self {
            out: interface.endpoint::<Bulk, Out>(EP_COMMANDS_OUT)?,
            input: interface.endpoint::<Bulk, In>(EP_COMMANDS_IN)?,
        })
    }
}

impl Pipe for UsbPipe {
    fn send(&mut self, bytes: &[u8]) -> Result<()> {
        let mut buf = Buffer::new(bytes.len());
        buf.extend_from_slice(bytes);
        self.out
            .transfer_blocking(buf, TIMEOUT)
            .into_result()
            .context("sending a request")?;
        Ok(())
    }

    fn receive(&mut self) -> Result<Vec<u8>> {
        let buf = self
            .input
            .transfer_blocking(Buffer::new(FIRMWARE_PACKET), TIMEOUT)
            .into_result()
            .context("waiting for the answer")?;
        Ok(buf.into_vec())
    }
}

/// A request's bytes: the header packet, then the payload zero-padded to whole packets.
fn request_bytes(command: Command, payload: &[u8]) -> Vec<u8> {
    let mut bytes = Header::for_payload::<FIRMWARE_PACKET>(command as u8, payload.len())
        .encode::<FIRMWARE_PACKET>()
        .to_vec();
    for chunk in payload.chunks(FIRMWARE_PACKET) {
        let start = bytes.len();
        bytes.extend_from_slice(chunk);
        bytes.resize(start + FIRMWARE_PACKET, 0);
    }
    bytes
}

/// Sends `command`, then reads the answer, skipping the zero-length packets the firmware puts
/// between and after packets.
fn request(pipe: &mut impl Pipe, command: Command, payload: &[u8]) -> Result<(Reply, Vec<u8>)> {
    pipe.send(&request_bytes(command, payload))?;
    pipe.send(&[])?;
    let header = loop {
        let packet = pipe.receive()?;
        if packet.is_empty() {
            continue;
        }
        break Header::parse(&packet)
            .with_context(|| format!("a {}-byte answer header", packet.len()))?;
    };
    let reply = match header.code {
        0 => Reply::Nak,
        1 => Reply::Ack,
        other => bail!("the answer's reply code is {other}, neither ACK nor NAK"),
    };
    let mut payload = Vec::with_capacity(header.packets as usize * FIRMWARE_PACKET);
    let mut got = 0;
    while got < header.packets {
        let packet = pipe.receive()?;
        if !packet.is_empty() {
            payload.extend_from_slice(&packet);
            got += 1;
        }
    }
    if payload.len() < header.bytes as usize {
        bail!(
            "the answer says {} bytes but carried {}",
            header.bytes,
            payload.len()
        );
    }
    payload.truncate(header.bytes as usize);
    Ok((reply, payload))
}

/// The answer's payload, or an error naming the command it refused.
fn acked(pipe: &mut impl Pipe, command: Command, payload: &[u8]) -> Result<Vec<u8>> {
    match request(pipe, command, payload)? {
        (Reply::Ack, data) => Ok(data),
        (Reply::Nak, _) => bail!("the key scanner refused {command:?}"),
    }
}

/// The answer's payload, or `None` if the firmware refused (it has none, or doesn't know the command).
fn optional(pipe: &mut impl Pipe, command: Command) -> Result<Option<Vec<u8>>> {
    Ok(match request(pipe, command, &[])? {
        (Reply::Ack, data) => Some(data),
        (Reply::Nak, _) => None,
    })
}

fn firmware_version(pipe: &mut impl Pipe) -> Result<String> {
    let raw = acked(pipe, Command::Version, &[])?;
    let text = raw.split(|&b| b == 0).next().unwrap_or_default();
    Ok(String::from_utf8_lossy(text).into_owned())
}

fn runtime_state(pipe: &mut impl Pipe) -> Result<Option<RuntimeState>> {
    optional(pipe, Command::GetRuntimeState)?
        .map(|data| RuntimeState::parse(&data).context("a runtime state that isn't 12 bytes"))
        .transpose()
}

fn version(pipe: &mut impl Pipe) -> Result<()> {
    println!("firmware: {}", firmware_version(pipe)?);
    match runtime_state(pipe)? {
        Some(s) => println!(
            "profile {}, actuation mode {}, platform {}",
            s.active_profile, s.actuation_mode, s.platform_code
        ),
        None => println!("runtime state: not answered"),
    }
    Ok(())
}

fn backup(pipe: &mut impl Pipe, dir: &Path) -> Result<()> {
    fs::create_dir(dir)
        .with_context(|| format!("creating {} (it must not exist yet)", dir.display()))?;
    let mut summary = String::new();
    let mut save = |name: &str, data: &[u8], what: &str| -> Result<()> {
        let path = dir.join(name);
        fs::File::create_new(&path)
            .and_then(|mut f| f.write_all(data))
            .with_context(|| format!("writing {}", path.display()))?;
        summary.push_str(&format!("{name}: {what}\n"));
        println!("saved {} ({} bytes)", path.display(), data.len());
        Ok(())
    };

    let firmware = firmware_version(pipe)?;
    save(
        "firmware.txt",
        format!("{firmware}\n").as_bytes(),
        "the VERSION answer",
    )?;
    if let Some(state) = runtime_state(pipe)? {
        let text = format!(
            "active_profile {}\nactuation_mode {}\nplatform_code {}\n",
            state.active_profile, state.actuation_mode, state.platform_code
        );
        save("runtime_state.txt", text.as_bytes(), "GET_RUNTIME_STATE")?;
    }
    match optional(pipe, Command::GetKeymap)? {
        Some(blob) => save("keymap.bin", &blob, "GET_KEYMAP; restore with SEND_KEYMAP")?,
        None => println!("no keymap stored"),
    }
    let profiles = acked(pipe, Command::FileToHost, &[])?;
    if let Err(e) = ProfileSettings::parse_export(&profiles) {
        bail!("the profile settings don't parse: {e:?}");
    }
    save(
        "profile_settings.bin",
        &profiles,
        "FILE_TO_HOST export form",
    )?;
    let refinement = optional(pipe, Command::PressRefinementStatus)?
        .map(|s| RefinementState::parse(&s).context("an unreadable refinement status"))
        .transpose()?;
    match refinement {
        Some(RefinementState::Installed { .. }) => {
            let record = acked(pipe, Command::GetPressRefinement, &[])?;
            if record.len() != REFINEMENT_RECORD_LEN {
                bail!(
                    "the refinement record is {} bytes, not {REFINEMENT_RECORD_LEN}",
                    record.len()
                );
            }
            save("press_refinement.bin", &record, "GET_PRESS_REFINEMENT")?;
        }
        Some(state) => println!("press refinement: {state:?}"),
        None => println!("press refinement: this firmware doesn't support it"),
    }
    summary.push_str(
        "\nThe factory calibration isn't reachable over USB; back it up with a full flash dump.\n",
    );
    fs::write(dir.join("README.txt"), summary).context("writing README.txt")?;
    Ok(())
}

fn enter_bootloader(pipe: &mut impl Pipe) -> Result<()> {
    println!("key scanner firmware: {}", firmware_version(pipe)?);
    // The firmware answers, then reboots 50 ms later; losing the answer to the reboot is fine.
    match request(pipe, Command::EnterBootloader, &[]) {
        Ok((Reply::Nak, _)) => bail!("the key scanner refused to enter its boot ROM"),
        Ok((Reply::Ack, _)) | Err(_) => Ok(()),
    }
}

/// A drive letter whose root holds the boot ROM's INFO_UF2.TXT.
fn find_drive() -> Option<PathBuf> {
    (b'D'..=b'Z')
        .map(|letter| PathBuf::from(format!("{}:\\", letter as char)))
        .find(|root| root.join(DRIVE_MARKER).is_file())
}

fn boot_rom_present() -> bool {
    nusb::list_devices().wait().is_ok_and(|mut d| {
        d.any(|d| d.vendor_id() == BOOT_ROM_VID && d.product_id() == BOOT_ROM_PID)
    })
}

fn wait_for_drive() -> Result<PathBuf> {
    let start = Instant::now();
    while start.elapsed() < DRIVE_WAIT {
        if let Some(drive) = find_drive() {
            return Ok(drive);
        }
        sleep(Duration::from_millis(250));
    }
    if boot_rom_present() {
        bail!("the RP2040 boot ROM is on USB but its drive didn't mount within {DRIVE_WAIT:?}");
    }
    bail!("the boot ROM didn't appear within {DRIVE_WAIT:?}");
}

fn flash(uf2: &Path) -> Result<()> {
    let image = fs::read(uf2).with_context(|| format!("reading {}", uf2.display()))?;
    if image.len() % 512 != 0 || image.get(..8) != Some(b"UF2\nWQ]\x9e".as_slice()) {
        bail!("{} isn't a UF2 file", uf2.display());
    }
    // A boot drive that's already mounted could belong to another RP2040 board, so it's only
    // used when the key scanner itself isn't on USB.
    let drive = match (UsbPipe::open(), find_drive()) {
        (Ok(_), Some(other)) => {
            bail!(
                "{} is already an RP2040 boot drive; unplug that board first",
                other.display()
            )
        }
        (Ok(mut pipe), None) => {
            enter_bootloader(&mut pipe)?;
            wait_for_drive()?
        }
        (Err(_), Some(drive)) => {
            println!(
                "the key scanner isn't on USB; using the boot drive already at {}",
                drive.display()
            );
            drive
        }
        (Err(e), None) => return Err(e),
    };
    let target = drive.join(uf2.file_name().context("the .uf2 path has no file name")?);
    println!("copying {} to {}", uf2.display(), target.display());
    // The boot ROM reboots into the new image as soon as the last block lands, so the copy
    // can end in an error once the drive vanishes.
    match fs::write(&target, &image) {
        Ok(()) => println!("done; the key scanner is rebooting into the new firmware"),
        Err(e) if find_drive().is_none() => {
            println!("the drive went away at the end of the copy ({e}); that's the reboot")
        }
        Err(e) => return Err(e).with_context(|| format!("writing {}", target.display())),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use glassdeck_scanner_proto::usb::response_packets;
    use std::collections::VecDeque;

    #[derive(Default)]
    struct Fake {
        sent: Vec<Vec<u8>>,
        answers: VecDeque<Vec<u8>>,
    }

    impl Fake {
        fn answer(&mut self, reply: Reply, payload: &[u8], zlp_after_each: bool) {
            for packet in response_packets::<FIRMWARE_PACKET>(reply, payload) {
                self.answers.push_back(packet.to_vec());
                if zlp_after_each {
                    self.answers.push_back(vec![]);
                }
            }
        }
    }

    impl Pipe for Fake {
        fn send(&mut self, bytes: &[u8]) -> Result<()> {
            self.sent.push(bytes.to_vec());
            Ok(())
        }
        fn receive(&mut self) -> Result<Vec<u8>> {
            self.answers.pop_front().context("no answer queued")
        }
    }

    #[test]
    fn a_request_is_whole_packets_then_a_zero_length_packet() {
        let mut pipe = Fake::default();
        pipe.answer(Reply::Ack, &[], false);
        let payload: Vec<u8> = (0..100).collect();
        request(&mut pipe, Command::SendKeymap, &payload).unwrap();
        assert_eq!(pipe.sent.len(), 2);
        assert_eq!(pipe.sent[0].len(), 3 * FIRMWARE_PACKET);
        assert_eq!(
            Header::parse(&pipe.sent[0]),
            Some(Header {
                code: Command::SendKeymap as u8,
                packets: 2,
                bytes: 100
            })
        );
        assert_eq!(&pipe.sent[0][64..164], &payload[..]);
        assert!(pipe.sent[0][164..].iter().all(|&b| b == 0));
        assert!(pipe.sent[1].is_empty());
    }

    #[test]
    fn answers_survive_zero_length_packets_between_packets() {
        let mut pipe = Fake::default();
        let keymap: Vec<u8> = (0..300).map(|i| i as u8).collect();
        pipe.answers.push_back(vec![]);
        pipe.answer(Reply::Ack, &keymap, true);
        assert_eq!(
            request(&mut pipe, Command::GetKeymap, &[]).unwrap(),
            (Reply::Ack, keymap)
        );
        assert_eq!(
            pipe.answers.len(),
            1,
            "only the trailing zero-length packet is left"
        );
    }

    #[test]
    fn a_refusal_is_reported() {
        let mut pipe = Fake::default();
        pipe.answer(Reply::Nak, &[], false);
        assert!(optional(&mut pipe, Command::GetKeymap).unwrap().is_none());
        pipe.answer(Reply::Nak, &[], false);
        assert!(acked(&mut pipe, Command::FileToHost, &[]).is_err());
    }

    #[test]
    fn the_version_drops_its_nul() {
        let mut pipe = Fake::default();
        pipe.answer(Reply::Ack, b"glassdeck-scanner 0.0.1\0", false);
        assert_eq!(
            firmware_version(&mut pipe).unwrap(),
            "glassdeck-scanner 0.0.1"
        );
    }
}
