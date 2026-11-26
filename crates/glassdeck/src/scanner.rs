//! Talks to the key scanner (the keyboard's RP2040) over USB: read what it runs, back it up
//! (settings over its command interface, then the whole flash through the boot ROM), and flash
//! a new firmware, which it only does with a verified backup of this keyboard.

use std::fmt::Write as _;
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

use crate::flash_image::{
    self, CALIBRATION_FACTORS, CALIBRATION_INFO, FactoryCalibration, PARTITION_LEN,
    PARTITION_OFFSET,
};
use crate::picoboot::{self, BootRom, FLASH_SIZE};

const TIMEOUT: Duration = Duration::from_secs(2);
/// How long the boot ROM may take to appear after the reboot, and its drive to mount.
const BOOT_ROM_WAIT: Duration = Duration::from_secs(20);
/// The RP2040 boot ROM's drive holds this file.
const DRIVE_MARKER: &str = "INFO_UF2.TXT";
const FLASH_DUMP: &str = "flash.bin";
/// Written last, so a folder without it is an unfinished backup.
const MANIFEST: &str = "manifest.txt";
const SETTINGS: &str = "settings";

#[derive(Subcommand)]
pub enum ScannerCommand {
    /// The firmware the key scanner reports, and its active profile, mode and platform
    Version,
    /// Back up this keyboard to a new folder: its settings, all 16 MiB of flash (stock firmware
    /// included) and the factory calibration, checked. Ends by rebooting into the firmware.
    Backup {
        /// Created; must not exist yet
        dir: PathBuf,
    },
    /// Check that a backup folder is complete and its calibration loads
    CheckBackup { dir: PathBuf },
    /// Reboot the key scanner into its USB boot ROM (the RPI-RP2 drive)
    Bootloader,
    /// Flash a .uf2 firmware; refuses unless --backup is a complete backup of this keyboard
    Flash {
        /// For example firmware/target/thumbv6m-none-eabi/release/glassdeck-scanner.uf2
        uf2: PathBuf,
        /// A folder made by `glassdeck scanner backup` from this keyboard
        #[arg(long)]
        backup: PathBuf,
    },
}

pub fn run(command: ScannerCommand) -> Result<()> {
    match command {
        ScannerCommand::Version => version(&mut UsbPipe::open()?),
        ScannerCommand::Backup { dir } => backup(&dir),
        ScannerCommand::CheckBackup { dir } => {
            let backup = Backup::load(&dir)?;
            println!(
                "{} is a complete backup: {}",
                dir.display(),
                backup.manifest.describe()
            );
            Ok(())
        }
        ScannerCommand::Bootloader => {
            enter_bootloader(&mut UsbPipe::open()?)?;
            let drive = wait_for_drive()?;
            println!(
                "The boot ROM's drive is {}; copy a .uf2 onto it to flash",
                drive.display()
            );
            Ok(())
        }
        ScannerCommand::Flash { uf2, backup } => flash(&uf2, &backup),
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
    /// The USB serial number, if the firmware reports one.
    serial: Option<String>,
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
        let serial = info.serial_number().map(str::to_owned);
        let device = info.open().wait().context("opening the key scanner")?;
        let interface = device.claim_interface(INTERFACE_COMMANDS).wait().context(
            "claiming the command interface; on Windows it must be bound to WinUSB (the firmware asks for that itself)",
        )?;
        Ok(Self {
            out: interface.endpoint::<Bulk, Out>(EP_COMMANDS_OUT)?,
            input: interface.endpoint::<Bulk, In>(EP_COMMANDS_IN)?,
            serial,
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

/// What a finished backup records about the keyboard it came from.
#[derive(Debug, PartialEq, Eq)]
struct Manifest {
    firmware: String,
    usb_serial: Option<String>,
    flash_fingerprint: u64,
    calibration_version: u32,
}

impl Manifest {
    fn encode(&self) -> String {
        format!(
            "firmware {}\nusb_serial {}\nflash_fnv1a64 {:016x}\ncalibration_version {}\n",
            self.firmware,
            self.usb_serial.as_deref().unwrap_or("none"),
            self.flash_fingerprint,
            self.calibration_version
        )
    }

    fn parse(text: &str) -> Result<Self> {
        let field = |key: &str| {
            text.lines()
                .find_map(|l| l.strip_prefix(key)?.strip_prefix(' '))
                .with_context(|| format!("the manifest has no {key}"))
        };
        Ok(Self {
            firmware: field("firmware")?.to_owned(),
            usb_serial: Some(field("usb_serial")?)
                .filter(|s| *s != "none")
                .map(str::to_owned),
            flash_fingerprint: u64::from_str_radix(field("flash_fnv1a64")?, 16)
                .context("the manifest's flash_fnv1a64")?,
            calibration_version: field("calibration_version")?
                .parse()
                .context("the manifest's calibration_version")?,
        })
    }

    fn describe(&self) -> String {
        format!(
            "firmware {}, USB serial {}, calibration version {}",
            self.firmware,
            self.usb_serial.as_deref().unwrap_or("none"),
            self.calibration_version
        )
    }
}

/// A backup folder that checked out: the manifest, a whole flash dump matching it, and a
/// factory calibration that loads and matches the copy inside the dump.
struct Backup {
    manifest: Manifest,
    calibration: FactoryCalibration,
}

impl Backup {
    fn load(dir: &Path) -> Result<Self> {
        let manifest_path = dir.join(MANIFEST);
        let manifest =
            Manifest::parse(&fs::read_to_string(&manifest_path).with_context(|| {
                format!(
                    "reading {} (a folder without it is an unfinished backup)",
                    manifest_path.display()
                )
            })?)?;
        let flash = fs::read(dir.join(FLASH_DUMP)).context("reading the flash dump")?;
        if flash.len() != FLASH_SIZE as usize {
            bail!("the flash dump is {} bytes, not {FLASH_SIZE}", flash.len());
        }
        if fingerprint(&flash) != manifest.flash_fingerprint {
            bail!("the flash dump doesn't match its manifest; it's damaged or from another backup");
        }
        let calibration = flash_image::factory_calibration(partition(&flash))
            .context("the flash dump's factory calibration")?;
        for (name, stored) in [
            (CALIBRATION_INFO, &calibration.info),
            (CALIBRATION_FACTORS, &calibration.factors),
        ] {
            let copy = fs::read(dir.join(name)).with_context(|| format!("reading {name}"))?;
            if &copy != stored {
                bail!("{name} differs from the calibration inside the flash dump");
            }
        }
        if calibration.calibration.version != manifest.calibration_version {
            bail!("the calibration version doesn't match the manifest");
        }
        Ok(Self {
            manifest,
            calibration,
        })
    }
}

fn partition(flash: &[u8]) -> &[u8] {
    &flash[PARTITION_OFFSET..PARTITION_OFFSET + PARTITION_LEN]
}

/// FNV-1a over the dump, so a damaged or swapped `flash.bin` is caught.
fn fingerprint(data: &[u8]) -> u64 {
    data.iter().fold(0xCBF2_9CE4_8422_2325u64, |h, &b| {
        (h ^ b as u64).wrapping_mul(0x0100_0000_01B3)
    })
}

fn write_new(path: &Path, data: &[u8]) -> Result<()> {
    fs::File::create_new(path)
        .and_then(|mut f| f.write_all(data))
        .with_context(|| format!("writing {}", path.display()))?;
    println!("saved {} ({} bytes)", path.display(), data.len());
    Ok(())
}

/// Saves what the firmware answers for into the new folder `dir`, returning a line per file.
fn save_settings(pipe: &mut impl Pipe, dir: &Path) -> Result<String> {
    fs::create_dir(dir).with_context(|| format!("creating {}", dir.display()))?;
    let mut summary = String::new();
    let mut save = |name: &str, data: &[u8], what: &str| -> Result<()> {
        write_new(&dir.join(name), data)?;
        writeln!(summary, "{SETTINGS}/{name}: {what}").expect("writing to a String");
        Ok(())
    };
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
    Ok(summary)
}

fn read_flash(rom: &mut BootRom, offset: u32, len: u32) -> Result<Vec<u8>> {
    let mib = |n: u32| n as f32 / (1024.0 * 1024.0);
    let mut shown = 0;
    let data = rom.read_flash(offset, len, |done| {
        if done - shown >= 256 * 1024 || done == len {
            shown = done;
            print!("\rreading flash: {:.2} / {:.2} MiB", mib(done), mib(len));
            let _ = std::io::stdout().flush();
        }
    })?;
    println!();
    Ok(data)
}

fn backup(dir: &Path) -> Result<()> {
    let mut pipe = UsbPipe::open()?;
    if picoboot::count() > 0 {
        bail!(
            "an RP2040 is already in its boot ROM; unplug it so the backup can't read the wrong board"
        );
    }
    let firmware = firmware_version(&mut pipe)?;
    let usb_serial = pipe.serial.clone();
    fs::create_dir(dir)
        .with_context(|| format!("creating {} (it must not exist yet)", dir.display()))?;
    let mut summary = save_settings(&mut pipe, &dir.join(SETTINGS))?;

    enter_bootloader(&mut pipe)?;
    drop(pipe);
    let mut rom = wait_for_boot_rom()?;
    let flash = read_flash(&mut rom, 0, FLASH_SIZE)?;
    let cal = flash_image::factory_calibration(partition(&flash))
        .context("the flash dump's factory calibration")?;
    write_new(&dir.join(FLASH_DUMP), &flash)?;
    write_new(&dir.join(CALIBRATION_INFO), &cal.info)?;
    write_new(&dir.join(CALIBRATION_FACTORS), &cal.factors)?;
    summary.push_str(&format!(
        "{FLASH_DUMP}: all 16 MiB of flash, the stock firmware first and the littlefs partition at {PARTITION_OFFSET:#x}\n\
         {CALIBRATION_INFO}, {CALIBRATION_FACTORS}: the factory calibration, copied out of that partition\n"
    ));
    fs::write(dir.join("README.txt"), summary).context("writing README.txt")?;
    let manifest = Manifest {
        firmware,
        usb_serial,
        flash_fingerprint: fingerprint(&flash),
        calibration_version: cal.calibration.version,
    };
    write_new(&dir.join(MANIFEST), manifest.encode().as_bytes())?;
    Backup::load(dir).context("checking the backup just written")?;
    rom.reboot().context("rebooting into the firmware")?;
    println!(
        "backup complete ({}); the key scanner is rebooting into its firmware",
        manifest.describe()
    );
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

fn wait_for<T>(what: &str, mut ready: impl FnMut() -> Option<T>) -> Result<T> {
    let start = Instant::now();
    while start.elapsed() < BOOT_ROM_WAIT {
        if let Some(t) = ready() {
            return Ok(t);
        }
        sleep(Duration::from_millis(250));
    }
    bail!("{what} didn't appear within {BOOT_ROM_WAIT:?}")
}

fn wait_for_boot_rom() -> Result<BootRom> {
    wait_for("the RP2040 boot ROM", || {
        (picoboot::count() > 0).then_some(())
    })?;
    // Windows can take a moment to make a new device openable.
    let mut last = None;
    for _ in 0..8 {
        match BootRom::open() {
            Ok(rom) => return Ok(rom),
            Err(e) => last = Some(e),
        }
        sleep(Duration::from_millis(500));
    }
    Err(last.expect("at least one attempt"))
}

/// A drive letter whose root holds the boot ROM's INFO_UF2.TXT.
fn find_drive() -> Option<PathBuf> {
    (b'D'..=b'Z')
        .map(|letter| PathBuf::from(format!("{}:\\", letter as char)))
        .find(|root| root.join(DRIVE_MARKER).is_file())
}

fn wait_for_drive() -> Result<PathBuf> {
    wait_for("the boot ROM's drive", find_drive)
}

fn flash(uf2: &Path, backup_dir: &Path) -> Result<()> {
    let backup = Backup::load(backup_dir).with_context(|| {
        format!(
            "{} isn't a usable backup, so nothing was flashed",
            backup_dir.display()
        )
    })?;
    println!("backup checked: {}", backup.manifest.describe());
    let image = fs::read(uf2).with_context(|| format!("reading {}", uf2.display()))?;
    if !image.len().is_multiple_of(512) || image.get(..8) != Some(b"UF2\nWQ]\x9e".as_slice()) {
        bail!("{} isn't a UF2 file", uf2.display());
    }
    if find_drive().is_some() || picoboot::count() > 0 {
        bail!(
            "an RP2040 is already in its boot ROM; unplug it (or, if it's this keyboard, replug it \
             so it starts its firmware) so this can't flash the wrong board"
        );
    }

    let mut pipe = UsbPipe::open()?;
    if let (Some(now), Some(then)) = (&pipe.serial, &backup.manifest.usb_serial)
        && now != then
    {
        bail!(
            "this keyboard's USB serial is {now}, the backup's is {then}; back up this keyboard first"
        );
    }
    enter_bootloader(&mut pipe)?;
    drop(pipe);

    let mut rom = wait_for_boot_rom()?;
    let live = read_flash(&mut rom, PARTITION_OFFSET as u32, PARTITION_LEN as u32)?;
    let live =
        flash_image::factory_calibration(&live).context("this keyboard's factory calibration")?;
    if live.info != backup.calibration.info || live.factors != backup.calibration.factors {
        rom.reboot().context("rebooting back into the firmware")?;
        bail!(
            "this keyboard's factory calibration isn't the one in the backup, so it's another keyboard; nothing was flashed"
        );
    }
    drop(rom);

    let drive = wait_for_drive()?;
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

    fn backup_folder(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "glassdeck-backup-test-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir(&dir).unwrap();
        let (info, factors) = flash_image::tests::calibration_files();
        let part = flash_image::build_partition(&[
            (CALIBRATION_INFO, &info),
            (CALIBRATION_FACTORS, &factors),
        ]);
        let mut flash = vec![0xFFu8; FLASH_SIZE as usize];
        flash[PARTITION_OFFSET..PARTITION_OFFSET + PARTITION_LEN].copy_from_slice(&part);
        fs::write(dir.join(FLASH_DUMP), &flash).unwrap();
        fs::write(dir.join(CALIBRATION_INFO), &info).unwrap();
        fs::write(dir.join(CALIBRATION_FACTORS), &factors).unwrap();
        let manifest = Manifest {
            firmware: "openflux-firmware 1.0.1".into(),
            usb_serial: Some("E6614103E7452D2F".into()),
            flash_fingerprint: fingerprint(&flash),
            calibration_version: 1,
        };
        fs::write(dir.join(MANIFEST), manifest.encode()).unwrap();
        dir
    }

    #[test]
    fn the_manifest_round_trips() {
        let m = Manifest {
            firmware: "openflux-firmware 1.0.1".into(),
            usb_serial: None,
            flash_fingerprint: 0x0123_4567_89AB_CDEF,
            calibration_version: 2,
        };
        assert_eq!(Manifest::parse(&m.encode()).unwrap(), m);
    }

    #[test]
    fn a_complete_backup_loads() {
        let dir = backup_folder("complete");
        let backup = Backup::load(&dir).unwrap();
        assert_eq!(
            backup.manifest.usb_serial.as_deref(),
            Some("E6614103E7452D2F")
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_incomplete_or_altered_backup_is_refused() {
        let dir = backup_folder("missing-manifest");
        fs::remove_file(dir.join(MANIFEST)).unwrap();
        assert!(Backup::load(&dir).is_err());
        fs::remove_dir_all(dir).unwrap();

        let dir = backup_folder("altered-dump");
        let mut flash = fs::read(dir.join(FLASH_DUMP)).unwrap();
        flash[0] = 0;
        fs::write(dir.join(FLASH_DUMP), &flash).unwrap();
        assert!(Backup::load(&dir).is_err());
        fs::remove_dir_all(dir).unwrap();

        let dir = backup_folder("altered-calibration");
        let mut factors = fs::read(dir.join(CALIBRATION_FACTORS)).unwrap();
        factors[0] ^= 1;
        fs::write(dir.join(CALIBRATION_FACTORS), &factors).unwrap();
        assert!(Backup::load(&dir).is_err());
        fs::remove_dir_all(dir).unwrap();

        let dir = backup_folder("missing-calibration-copy");
        fs::remove_file(dir.join(CALIBRATION_INFO)).unwrap();
        assert!(Backup::load(&dir).is_err());
        fs::remove_dir_all(dir).unwrap();
    }
}
