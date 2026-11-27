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
    Command, FIRMWARE_PACKET, PID, REFINEMENT_RECORD_LEN, RefinementState, Reply, RuntimeState, VID,
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
use crate::picoboot::{self, BootRom, FLASH_SIZE, SECTOR};
use crate::usb_command::{self, Pipe};

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
    /// Put the backup's firmware (stock, for a backup taken before flashing) back on this
    /// keyboard. Rewrites only the 4 KiB sectors that differ and reads each one back.
    Restore {
        /// A folder made by `glassdeck scanner backup` from this keyboard
        #[arg(long)]
        backup: PathBuf,
        /// Also put back the settings partition (keymap, profiles, rest references). Its factory
        /// calibration bytes are the ones already checked to match.
        #[arg(long)]
        settings: bool,
    },
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
        ScannerCommand::Restore { backup, settings } => restore(&backup, settings),
    }
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
    const PACKET: usize = FIRMWARE_PACKET;

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

/// Every answer ends with a zero-length packet, and Flux's own firmware takes no new request
/// until that packet has been read.
fn request(pipe: &mut impl Pipe, command: Command, payload: &[u8]) -> Result<(Reply, Vec<u8>)> {
    let answer = usb_command::request(pipe, command as u8, payload)?;
    let end = pipe
        .receive()
        .context("reading the zero-length packet that ends the answer")?;
    if !end.is_empty() {
        bail!("the answer ran {} bytes past its packets", end.len());
    }
    Ok(answer)
}

/// The commands a key scanner's firmware knows. Flux's own firmware (versions like `1.4.9.D`)
/// has no runtime-state or press-refinement commands and doesn't answer them at all, so they're
/// never asked of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CommandSet {
    Flux,
    Keyglass,
}

impl CommandSet {
    fn of(version: &str) -> Self {
        if version.starts_with(|c: char| c.is_ascii_digit()) {
            Self::Flux
        } else {
            Self::Keyglass
        }
    }
}

/// The profile file's number. Flux's own firmware refuses `FILE_TO_HOST` without it.
const PROFILE_FILE: [u8; 4] = 0u32.to_le_bytes();

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
    let firmware = firmware_version(pipe)?;
    println!("firmware: {firmware}");
    if CommandSet::of(&firmware) == CommandSet::Flux {
        println!("Flux's own firmware");
        return Ok(());
    }
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
    flash: Vec<u8>,
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
            flash,
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
fn save_settings(pipe: &mut impl Pipe, commands: CommandSet, dir: &Path) -> Result<String> {
    fs::create_dir(dir).with_context(|| format!("creating {}", dir.display()))?;
    let mut summary = String::new();
    let mut save = |name: &str, data: &[u8], what: &str| -> Result<()> {
        write_new(&dir.join(name), data)?;
        writeln!(summary, "{SETTINGS}/{name}: {what}").expect("writing to a String");
        Ok(())
    };
    let state = match commands {
        CommandSet::Flux => None,
        CommandSet::Keyglass => runtime_state(pipe)?,
    };
    if let Some(state) = state {
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
    let profiles = acked(pipe, Command::FileToHost, &PROFILE_FILE)?;
    let parsed = match commands {
        CommandSet::Flux => ProfileSettings::parse_flux_export(&profiles),
        CommandSet::Keyglass => ProfileSettings::parse_export(&profiles),
    };
    if let Err(e) = parsed {
        bail!("the profile settings don't parse: {e:?}");
    }
    save(
        "profile_settings.bin",
        &profiles,
        "FILE_TO_HOST export form",
    )?;
    let status = match commands {
        CommandSet::Flux => None,
        CommandSet::Keyglass => optional(pipe, Command::PressRefinementStatus)?,
    };
    let refinement = status
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
    let mut summary = save_settings(&mut pipe, CommandSet::of(&firmware), &dir.join(SETTINGS))?;

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

fn load_backup(dir: &Path, refused: &str) -> Result<Backup> {
    let backup = Backup::load(dir)
        .with_context(|| format!("{} isn't a usable backup, so {refused}", dir.display()))?;
    println!("backup checked: {}", backup.manifest.describe());
    Ok(backup)
}

/// Gets the keyboard into its boot ROM. With `allow_boot_rom`, a lone RP2040 already in its
/// boot ROM (as after three crashes in a row) is taken to be this keyboard; the calibration
/// check that follows proves it or stops.
fn into_boot_rom(backup: &Backup, allow_boot_rom: bool) -> Result<BootRom> {
    match (UsbPipe::open(), picoboot::count()) {
        (Ok(mut pipe), 0) => {
            if let (Some(now), Some(then)) = (&pipe.serial, &backup.manifest.usb_serial)
                && now != then
            {
                bail!(
                    "this keyboard's USB serial is {now}, the backup's is {then}; back up this keyboard first"
                );
            }
            enter_bootloader(&mut pipe)?;
        }
        (Err(_), 1) if allow_boot_rom => println!("the key scanner is already in its boot ROM"),
        (Err(e), 0) => return Err(e),
        _ => bail!(
            "an RP2040 is already in its boot ROM; unplug it (or, if it's this keyboard, replug it \
             so it starts its firmware) so this can't write the wrong board"
        ),
    }
    wait_for_boot_rom()
}

/// Reads the keyboard's settings partition and checks its factory calibration is the backup's.
/// On a mismatch it reboots the keyboard and stops.
fn check_same_keyboard(mut rom: BootRom, backup: &Backup) -> Result<(BootRom, Vec<u8>)> {
    let live = read_flash(&mut rom, PARTITION_OFFSET as u32, PARTITION_LEN as u32)?;
    let same = match flash_image::factory_calibration(&live) {
        Ok(cal) => cal.info == backup.calibration.info && cal.factors == backup.calibration.factors,
        Err(e) => {
            rom.reboot().context("rebooting back into the firmware")?;
            return Err(e).context("this keyboard's factory calibration; nothing was written");
        }
    };
    if !same {
        rom.reboot().context("rebooting back into the firmware")?;
        bail!(
            "this keyboard's factory calibration isn't the one in the backup, so it's another \
             keyboard; nothing was written"
        );
    }
    Ok((rom, live))
}

fn flash(uf2: &Path, backup_dir: &Path) -> Result<()> {
    let backup = load_backup(backup_dir, "nothing was flashed")?;
    let image = fs::read(uf2).with_context(|| format!("reading {}", uf2.display()))?;
    if !image.len().is_multiple_of(512) || image.get(..8) != Some(b"UF2\nWQ]\x9e".as_slice()) {
        bail!("{} isn't a UF2 file", uf2.display());
    }
    if find_drive().is_some() {
        bail!("an RP2040 boot drive is already mounted; unplug that board so this can't flash it");
    }
    let rom = into_boot_rom(&backup, false)?;
    let (rom, _) = check_same_keyboard(rom, &backup)?;
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

/// Offsets of the 4 KiB sectors where `current` differs from `wanted`; both start at `base`.
fn changed_sectors(wanted: &[u8], current: &[u8], base: usize) -> Vec<usize> {
    assert_eq!(wanted.len(), current.len());
    wanted
        .chunks(SECTOR as usize)
        .zip(current.chunks(SECTOR as usize))
        .enumerate()
        .filter(|(_, (w, c))| w != c)
        .map(|(i, _)| base + i * SECTOR as usize)
        .collect()
}

fn restore(backup_dir: &Path, settings: bool) -> Result<()> {
    let backup = load_backup(backup_dir, "nothing was restored")?;
    let rom = into_boot_rom(&backup, true)?;
    let (mut rom, live_partition) = check_same_keyboard(rom, &backup)?;

    let firmware = read_flash(&mut rom, 0, PARTITION_OFFSET as u32)?;
    let mut sectors = changed_sectors(&backup.flash[..PARTITION_OFFSET], &firmware, 0);
    if settings {
        sectors.extend(changed_sectors(
            partition(&backup.flash),
            &live_partition,
            PARTITION_OFFSET,
        ));
    }
    if sectors.is_empty() {
        println!("flash already matches the backup");
    }
    for (i, &at) in sectors.iter().enumerate() {
        print!(
            "\rrewriting sector {} of {} ({at:#08x})",
            i + 1,
            sectors.len()
        );
        let _ = std::io::stdout().flush();
        rom.rewrite_sector(at as u32, &backup.flash[at..at + SECTOR as usize])?;
    }
    if !sectors.is_empty() {
        println!();
    }
    rom.reboot()
        .context("rebooting into the restored firmware")?;
    let what = if settings {
        "firmware and settings partition"
    } else {
        "firmware"
    };
    println!("restored the {what} from the backup; the key scanner is rebooting");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use glassdeck_scanner_proto::usb::{Header, response_packets};
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
            if !zlp_after_each {
                self.answers.push_back(vec![]);
            }
        }
    }

    impl Pipe for Fake {
        const PACKET: usize = FIRMWARE_PACKET;

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
        assert!(
            pipe.answers.is_empty(),
            "the trailing zero-length packet is read too"
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
    fn only_changed_sectors_are_rewritten() {
        let wanted = vec![0xFFu8; 4 * SECTOR as usize];
        let mut current = wanted.clone();
        current[SECTOR as usize + 7] = 0;
        current[4 * SECTOR as usize - 1] = 0;
        assert_eq!(changed_sectors(&wanted, &current, 0x1000), [0x2000, 0x4000]);
        assert!(changed_sectors(&wanted, &wanted, 0).is_empty());
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
