//! The keyboard's Linux side, through its stock bridge (garfield) on USB. The bridge only moves
//! files inside /home, and the stock system installs any .ipk left in /home/root as root, so
//! this offers exactly the day-one steps built on that: install a package, run the probe, take
//! the backup. There's deliberately no general upload, download or delete.

use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write as _};
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use flate2::read::MultiGzDecoder;
use glassdeck_scanner_proto::usb::Reply;
use nusb::MaybeFuture;
use nusb::transfer::{Buffer, Bulk, In, Out};
use sha2::{Digest, Sha256};

use crate::usb_command::{self, Pipe};

const BRIDGE_VID: u16 = 0x3761;
const BRIDGE_PID: u16 = 0x0104;
const BRIDGE_INTERFACE: u8 = 0;
const EP_OUT: u8 = 0x01;
const EP_IN: u8 = 0x81;
const PACKET: usize = 512;
/// IN transfers this big move large files in few round trips.
const RECEIVE_CHUNK: usize = 64 * 1024;
const TIMEOUT: Duration = Duration::from_secs(30);

const BRIDGE_VERSION: u8 = 1;
const DISPLAY_VERSION: u8 = 2;
const OS_VERSION: u8 = 3;
const GET_FILE_LIST: u8 = 100;
const GET_FILE: u8 = 101;
const PUT_FILE: u8 = 102;
const DELETE_FILE: u8 = 103;
const GET_FREE_SPACE: u8 = 104;

const HOME: &str = "/home/root";
const PROBE_REPORT: &str = "glassdeck-probe.txt";
const BACKUP_DIR: &str = "/home/root/glassdeck-backup";
/// How long the stock installer may take to pick a package up.
const INSTALL_WAIT: Duration = Duration::from_secs(60);
const PROBE_WAIT: Duration = Duration::from_secs(120);
const BACKUP_WAIT: Duration = Duration::from_secs(3 * 60 * 60);
const POLL: Duration = Duration::from_secs(2);

#[derive(Subcommand)]
pub enum KeyboardCommand {
    /// The bridge, display and OS versions, and free space, from the stock bridge
    Versions,
    /// Install an .ipk: it goes to /home/root, where the keyboard installs it as root
    Install { ipk: PathBuf },
    /// Install glassdeck-probe, wait for its report and save it
    Probe {
        #[arg(long, default_value = "target/packages/glassdeck-probe.ipk")]
        package: PathBuf,
        /// Must not exist yet
        #[arg(long, default_value = PROBE_REPORT)]
        out: PathBuf,
    },
    /// Install glassdeck-backup, wait for it, then download every piece into a new folder and
    /// check each partition against its SHA-256
    Backup {
        /// Created; must not exist yet
        dir: PathBuf,
        #[arg(long, default_value = "target/packages/glassdeck-backup.ipk")]
        package: PathBuf,
    },
}

pub fn run(command: KeyboardCommand) -> Result<()> {
    // Packages are checked before anything touches the keyboard.
    match command {
        KeyboardCommand::Versions => versions(&mut Bridge::open()?),
        KeyboardCommand::Install { ipk } => {
            let package = Package::read(&ipk)?;
            install(&mut Bridge::open()?, &package)
        }
        KeyboardCommand::Probe { package, out } => {
            let package = Package::read(&package)?;
            probe(&mut Bridge::open()?, &package, &out)
        }
        KeyboardCommand::Backup { dir, package } => {
            let package = Package::read(&package)?;
            backup(&mut Bridge::open()?, &package, &dir)
        }
    }
}

struct Bridge {
    out: nusb::Endpoint<Bulk, Out>,
    input: nusb::Endpoint<Bulk, In>,
}

impl Bridge {
    fn open() -> Result<Self> {
        let info = nusb::list_devices()
            .wait()
            .context("listing USB devices")?
            .find(|d| d.vendor_id() == BRIDGE_VID && d.product_id() == BRIDGE_PID)
            .with_context(|| {
                format!("the keyboard's stock bridge isn't on USB (looked for {BRIDGE_VID:04x}:{BRIDGE_PID:04x}); is the keyboard plugged in and booted?")
            })?;
        let device = info.open().wait().context("opening the stock bridge")?;
        let interface = device.claim_interface(BRIDGE_INTERFACE).wait().context(
            "claiming the stock bridge's interface; on Windows it needs the WinUSB driver",
        )?;
        Ok(Self {
            out: interface.endpoint::<Bulk, Out>(EP_OUT)?,
            input: interface.endpoint::<Bulk, In>(EP_IN)?,
        })
    }
}

impl Pipe for Bridge {
    const PACKET: usize = PACKET;

    fn send(&mut self, bytes: &[u8]) -> Result<()> {
        let mut buf = Buffer::new(bytes.len());
        buf.extend_from_slice(bytes);
        self.out
            .transfer_blocking(buf, TIMEOUT)
            .into_result()
            .context("sending to the bridge")?;
        Ok(())
    }

    fn receive(&mut self) -> Result<Vec<u8>> {
        let buf = self
            .input
            .transfer_blocking(Buffer::new(RECEIVE_CHUNK), TIMEOUT)
            .into_result()
            .context("waiting for the bridge")?;
        Ok(buf.into_vec())
    }
}

fn acked(pipe: &mut impl Pipe, code: u8, payload: &[u8], what: &str) -> Result<Vec<u8>> {
    match usb_command::request(pipe, code, payload)? {
        (Reply::Ack, data) => Ok(data),
        (Reply::Nak, _) => bail!("the bridge refused to {what}"),
    }
}

fn text(data: &[u8]) -> String {
    let end = data.iter().position(|&b| b == 0).unwrap_or(data.len());
    String::from_utf8_lossy(&data[..end]).trim_end().to_owned()
}

fn c_path(path: &str) -> Vec<u8> {
    let mut p = path.as_bytes().to_vec();
    p.push(0);
    p
}

/// `<size>:<name>\0` for every regular file.
fn parse_listing(data: &[u8]) -> BTreeMap<String, u64> {
    data.split(|&b| b == 0)
        .filter_map(|entry| {
            let entry = std::str::from_utf8(entry).ok()?;
            let (size, name) = entry.split_once(':')?;
            Some((name.to_owned(), size.parse().ok()?))
        })
        .collect()
}

fn list(pipe: &mut impl Pipe, dir: &str) -> Result<BTreeMap<String, u64>> {
    Ok(parse_listing(&acked(
        pipe,
        GET_FILE_LIST,
        &c_path(dir),
        &format!("list {dir}"),
    )?))
}

fn free_bytes(pipe: &mut impl Pipe) -> Result<u64> {
    let raw = text(&acked(pipe, GET_FREE_SPACE, &[], "report free space")?);
    raw.split(',')
        .find_map(|f| f.strip_prefix("free:")?.parse().ok())
        .with_context(|| format!("an unreadable free-space answer: {raw}"))
}

fn versions(pipe: &mut impl Pipe) -> Result<()> {
    for (code, name) in [
        (BRIDGE_VERSION, "bridge"),
        (DISPLAY_VERSION, "display"),
        (OS_VERSION, "os"),
    ] {
        println!(
            "{name}: {}",
            text(&acked(
                pipe,
                code,
                &[],
                &format!("report the {name} version")
            )?)
        );
    }
    println!("free: {} MiB", free_bytes(pipe)? / (1024 * 1024));
    Ok(())
}

/// An .ipk read from disk and checked to be one: an ar archive with the debian-binary member,
/// under a plain file name.
struct Package {
    name: String,
    bytes: Vec<u8>,
}

impl Package {
    fn read(path: &Path) -> Result<Self> {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .context("the package path has no file name")?
            .to_owned();
        Self::new(
            name,
            fs::read(path).with_context(|| format!("reading {}", path.display()))?,
        )
    }

    fn new(name: String, bytes: Vec<u8>) -> Result<Self> {
        let plain = name.strip_suffix(".ipk").is_some_and(|stem| {
            !stem.is_empty()
                && stem
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "._+-".contains(c))
                && !stem.starts_with('.')
        });
        if !plain {
            bail!("{name}: a package name is letters, digits and ._+- ending in .ipk");
        }
        if !bytes.starts_with(b"!<arch>\n")
            || bytes.get(8..8 + 13) != Some(b"debian-binary".as_slice())
        {
            bail!("{name} isn't an .ipk (an ar archive starting with debian-binary)");
        }
        Ok(Self { name, bytes })
    }
}

fn wait_for<T>(
    what: &str,
    limit: Duration,
    mut ready: impl FnMut() -> Result<Option<T>>,
) -> Result<T> {
    let start = Instant::now();
    loop {
        if let Some(t) = ready()? {
            return Ok(t);
        }
        if start.elapsed() >= limit {
            bail!("{what} didn't happen within {limit:?}");
        }
        sleep(POLL);
    }
}

fn install(pipe: &mut impl Pipe, package: &Package) -> Result<()> {
    let free = free_bytes(pipe)?;
    if free < package.bytes.len() as u64 {
        bail!(
            "{} needs {} bytes; the keyboard has {free} free",
            package.name,
            package.bytes.len()
        );
    }
    let mut payload = c_path(&format!("{HOME}/{}", package.name));
    payload.extend_from_slice(&package.bytes);
    acked(
        pipe,
        PUT_FILE,
        &payload,
        &format!("upload {}", package.name),
    )?;
    println!(
        "uploaded {} to {HOME}; waiting for the keyboard to install it",
        package.name
    );
    // The stock installer takes the package out of /home/root once it has run it.
    match wait_for("the install", INSTALL_WAIT, || {
        Ok((!list(pipe, HOME)?.contains_key(&package.name)).then_some(()))
    }) {
        Ok(()) => println!("installed {}", package.name),
        Err(_) => println!(
            "{} is still in {HOME} after {INSTALL_WAIT:?}; the keyboard may not have installed it",
            package.name
        ),
    }
    Ok(())
}

fn probe(pipe: &mut impl Pipe, package: &Package, out: &Path) -> Result<()> {
    if out.exists() {
        bail!("{} already exists", out.display());
    }
    let report = format!("{HOME}/{PROBE_REPORT}");
    acked(
        pipe,
        DELETE_FILE,
        &c_path(&report),
        "clear the last probe report",
    )?;
    install(pipe, package)?;
    // The probe writes its report under another name and renames it, so a listed report is whole.
    wait_for("the probe report", PROBE_WAIT, || {
        Ok(list(pipe, HOME)?.contains_key(PROBE_REPORT).then_some(()))
    })?;
    let data = acked(pipe, GET_FILE, &c_path(&report), "send the probe report")?;
    fs::File::create_new(out)
        .and_then(|mut f| f.write_all(&data))
        .with_context(|| format!("writing {}", out.display()))?;
    println!(
        "saved the probe report to {} ({} bytes)",
        out.display(),
        data.len()
    );
    Ok(())
}

fn backup(pipe: &mut impl Pipe, package: &Package, dir: &Path) -> Result<()> {
    fs::create_dir(dir)
        .with_context(|| format!("creating {} (it must not exist yet)", dir.display()))?;
    acked(
        pipe,
        DELETE_FILE,
        &c_path(&format!("{BACKUP_DIR}/DONE")),
        "clear the last backup's DONE",
    )?;
    install(pipe, package)?;
    let mut last_log = String::new();
    wait_for("the backup", BACKUP_WAIT, || {
        let files = list(pipe, BACKUP_DIR).unwrap_or_default();
        if files.contains_key("DONE") {
            return Ok(Some(()));
        }
        if files.contains_key("log.txt") {
            let log = text(&acked(
                pipe,
                GET_FILE,
                &c_path(&format!("{BACKUP_DIR}/log.txt")),
                "send the backup log",
            )?);
            if let Some(line) = log.lines().last()
                && line != last_log
            {
                println!("keyboard: {line}");
                last_log = line.to_owned();
            }
        }
        Ok(None)
    })?;

    let files = list(pipe, BACKUP_DIR)?;
    for (name, size) in &files {
        let data = acked(
            pipe,
            GET_FILE,
            &c_path(&format!("{BACKUP_DIR}/{name}")),
            &format!("send {name}"),
        )?;
        if data.len() as u64 != *size {
            bail!("{name} came back as {} bytes, not {size}", data.len());
        }
        fs::File::create_new(dir.join(name))
            .and_then(|mut f| f.write_all(&data))
            .with_context(|| format!("writing {name}"))?;
        println!("downloaded {name} ({size} bytes)");
    }
    let checked = check_backup(dir)?;
    fs::write(dir.join("VERIFIED"), checked.join("\n") + "\n").context("writing VERIFIED")?;
    println!("backup complete; every partition matches its SHA-256");
    print!(
        "{}",
        fs::read_to_string(dir.join("RESULTS")).unwrap_or_default()
    );
    Ok(())
}

/// Joins each partition's pieces, unpacks them and checks the result against SHA256SUMS.
/// Returns a line per partition checked.
fn check_backup(dir: &Path) -> Result<Vec<String>> {
    let sums = fs::read_to_string(dir.join("SHA256SUMS")).context("reading SHA256SUMS")?;
    let mut checked = Vec::new();
    for line in sums.lines().filter(|l| !l.trim().is_empty()) {
        let (want, image) = line
            .split_once("  ")
            .with_context(|| format!("an unreadable SHA256SUMS line: {line}"))?;
        let prefix = format!("{image}.gz.");
        let mut pieces: Vec<PathBuf> = fs::read_dir(dir)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with(&prefix))
            })
            .collect();
        pieces.sort();
        if pieces.is_empty() {
            bail!("{image} is listed in SHA256SUMS but has no pieces");
        }
        let joined = pieces
            .iter()
            .map(|p| fs::File::open(p).with_context(|| format!("opening {}", p.display())))
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .fold(Box::new(std::io::empty()) as Box<dyn Read>, |r, f| {
                Box::new(r.chain(f))
            });
        let mut decoder = MultiGzDecoder::new(joined);
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 1 << 20];
        let mut size = 0u64;
        loop {
            let n = decoder
                .read(&mut buf)
                .with_context(|| format!("unpacking {image}"))?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            size += n as u64;
        }
        let got: String = hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        if got != want {
            bail!("{image} doesn't match its SHA-256 after unpacking");
        }
        checked.push(format!("{image} {size} bytes sha256 {got}"));
    }
    Ok(checked)
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use glassdeck_scanner_proto::usb::response_packets;
    use std::collections::VecDeque;

    #[derive(Default)]
    struct FakeBridge {
        sent: Vec<Vec<u8>>,
        answers: VecDeque<Vec<u8>>,
    }

    impl FakeBridge {
        fn answer(&mut self, reply: Reply, payload: &[u8]) {
            let stream: Vec<u8> = response_packets::<PACKET>(reply, payload)
                .flatten()
                .collect();
            // The bridge's big IN transfers come back in pieces, then a zero-length packet.
            for chunk in stream.chunks(3 * PACKET) {
                self.answers.push_back(chunk.to_vec());
            }
            self.answers.push_back(vec![]);
        }
    }

    impl Pipe for FakeBridge {
        const PACKET: usize = PACKET;

        fn send(&mut self, bytes: &[u8]) -> Result<()> {
            self.sent.push(bytes.to_vec());
            Ok(())
        }
        fn receive(&mut self) -> Result<Vec<u8>> {
            self.answers.pop_front().context("no answer queued")
        }
    }

    fn ipk() -> Vec<u8> {
        let mut b = b"!<arch>\ndebian-binary   ".to_vec();
        b.resize(200, 0);
        b
    }

    #[test]
    fn only_real_packages_with_plain_names_are_installed() {
        assert!(Package::new("glassdeck-probe.ipk".into(), ipk()).is_ok());
        for name in [
            "../evil.ipk",
            "a/b.ipk",
            ".hidden.ipk",
            "x.sh",
            ".ipk",
            "probe.IPK",
        ] {
            assert!(Package::new(name.into(), ipk()).is_err(), "{name}");
        }
        assert!(Package::new("p.ipk".into(), b"#!/bin/sh\n".to_vec()).is_err());
    }

    #[test]
    fn install_uploads_to_home_root_and_waits_for_the_installer() {
        let package = Package::new("glassdeck-probe.ipk".into(), ipk()).unwrap();
        let mut bridge = FakeBridge::default();
        bridge.answer(Reply::Ack, b"free:900000000,total:1000000000\0");
        bridge.answer(Reply::Ack, &[]);
        bridge.answer(Reply::Ack, b"200:glassdeck-probe.ipk\0");
        bridge.answer(Reply::Ack, b"12:other.txt\0");
        install(&mut bridge, &package).unwrap();
        let put = &bridge.sent[2];
        assert_eq!(put[0], PUT_FILE);
        let path = b"/home/root/glassdeck-probe.ipk\0";
        assert_eq!(&put[PACKET..PACKET + path.len()], path);
        assert_eq!(
            &put[PACKET + path.len()..PACKET + path.len() + 8],
            b"!<arch>\n"
        );
    }

    #[test]
    fn a_listing_parses() {
        let l = parse_listing(b"12:a.txt\x004096:loop0.img.gz.000\x00");
        assert_eq!(l.get("a.txt"), Some(&12));
        assert_eq!(l.get("loop0.img.gz.000"), Some(&4096));
    }

    #[test]
    fn a_partition_in_pieces_checks_against_its_sum() {
        let dir = std::env::temp_dir().join(format!("glassdeck-kb-backup-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir(&dir).unwrap();
        let raw: Vec<u8> = (0..300_000u32).map(|i| (i * 7 % 251) as u8).collect();
        for (i, piece) in raw.chunks(128 * 1024).enumerate() {
            let mut gz = GzEncoder::new(Vec::new(), Compression::fast());
            gz.write_all(piece).unwrap();
            fs::write(
                dir.join(format!("mmcblk0p1.img.gz.{i:03}")),
                gz.finish().unwrap(),
            )
            .unwrap();
        }
        let sum: String = Sha256::digest(&raw)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        fs::write(dir.join("SHA256SUMS"), format!("{sum}  mmcblk0p1.img\n")).unwrap();
        assert_eq!(check_backup(&dir).unwrap().len(), 1);

        fs::write(
            dir.join("SHA256SUMS"),
            format!("{}  mmcblk0p1.img\n", "0".repeat(64)),
        )
        .unwrap();
        assert!(check_backup(&dir).is_err());
        fs::remove_dir_all(dir).unwrap();
    }
}
