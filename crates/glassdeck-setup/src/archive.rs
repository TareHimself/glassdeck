//! One keyboard's backup as a single zip: setup's folder layout (`scanner/`, `linux/`,
//! `probe.txt`, `versions.txt`) plus `backup.json` saying what it holds. Only a backup that checks
//! out is archived, and an archive is unpacked and checked again before it's trusted.

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::{bridge, scanner};

pub const FORMAT: u32 = 1;
pub const CONTENTS: &str = "backup.json";
const SCANNER: &str = "scanner";
const LINUX: &str = "linux";
const PROBE: &str = "probe.txt";
const VERSIONS: &str = "versions.txt";
const PARTS: [&str; 4] = [SCANNER, LINUX, PROBE, VERSIONS];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Contents {
    pub format: u32,
    /// The key scanner's USB serial, which names the keyboard.
    pub serial: String,
    /// Seconds since 1970 when it was archived.
    pub created: u64,
    pub scanner_firmware: String,
    pub calibration_version: u32,
    /// What the keyboard's computer reported when it was backed up.
    pub linux: Option<LinuxVersions>,
    /// The parts it holds, of `scanner`, `linux` and `probe`.
    pub holds: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinuxVersions {
    pub bridge: String,
    pub display: String,
    pub os: String,
}

/// Checks a setup folder (`<root>/<serial>`) and says what it holds, stamped `created`. Both
/// chips' backups must be there and check out.
pub fn verify(folder: &Path, created: u64) -> Result<Contents> {
    let scanner = scanner::Backup::load(&folder.join(SCANNER)).context("the key scanner's backup")?;
    let serial = scanner.usb_serial().context("the key scanner's backup doesn't say which keyboard it's from")?.to_owned();
    bridge::check_saved(&folder.join(LINUX)).context("the Linux side's backup")?;
    let linux = fs::read_to_string(folder.join(VERSIONS)).ok().and_then(|text| parse_versions(&text));
    let mut holds = vec![SCANNER.to_owned(), LINUX.to_owned()];
    if folder.join(PROBE).is_file() {
        holds.push("probe".to_owned());
    }
    Ok(Contents {
        format: FORMAT,
        serial,
        created,
        scanner_firmware: scanner.firmware().to_owned(),
        calibration_version: scanner.calibration_version(),
        linux,
        holds,
    })
}

/// `versions.txt` as setup writes it: `bridge: 1.4.8` lines.
pub fn parse_versions(text: &str) -> Option<LinuxVersions> {
    let field = |name: &str| text.lines().find_map(|l| l.strip_prefix(name)?.strip_prefix(": ").map(|v| v.trim().to_owned()));
    Some(LinuxVersions { bridge: field("bridge")?, display: field("display")?, os: field("os")? })
}

/// Already-compressed pieces go in as they are; the flash dump and text are deflated.
fn method(name: &str) -> CompressionMethod {
    if name.ends_with(".gz") || name.contains(".gz.") { CompressionMethod::Stored } else { CompressionMethod::Deflated }
}

/// Writes the checked folder as one zip at `into`, through a `.partial` file so a zip that's
/// there is always whole.
pub fn write(folder: &Path, contents: &Contents, into: &Path) -> Result<()> {
    let partial = into.with_extension("zip.partial");
    let file = File::create(&partial).with_context(|| format!("creating {}", partial.display()))?;
    let mut zip = ZipWriter::new(file);
    let options = |name: &str| SimpleFileOptions::default().compression_method(method(name)).large_file(false);
    zip.start_file(CONTENTS, options(CONTENTS))?;
    serde_json::to_writer_pretty(&mut zip, contents).context("writing backup.json")?;
    for part in PARTS {
        let path = folder.join(part);
        if path.is_file() {
            add_file(&mut zip, &path, part, &options)?;
        } else if path.is_dir() {
            add_dir(&mut zip, &path, part, &options)?;
        }
    }
    zip.finish().context("finishing the zip")?;
    fs::rename(&partial, into).with_context(|| format!("saving {}", into.display()))
}

fn add_dir(zip: &mut ZipWriter<File>, dir: &Path, name: &str, options: &impl Fn(&str) -> SimpleFileOptions) -> Result<()> {
    let mut entries: Vec<_> = fs::read_dir(dir)?.collect::<io::Result<_>>()?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let child = format!("{name}/{}", entry.file_name().to_string_lossy());
        if entry.file_type()?.is_dir() {
            add_dir(zip, &entry.path(), &child, options)?;
        } else {
            add_file(zip, &entry.path(), &child, options)?;
        }
    }
    Ok(())
}

fn add_file(zip: &mut ZipWriter<File>, path: &Path, name: &str, options: &impl Fn(&str) -> SimpleFileOptions) -> Result<()> {
    zip.start_file(name, options(name))?;
    io::copy(&mut File::open(path).with_context(|| format!("reading {}", path.display()))?, zip).with_context(|| format!("adding {name}"))?;
    Ok(())
}

/// What a backup zip says it holds, without unpacking the rest.
pub fn read_contents(zip: &Path) -> Result<Contents> {
    let mut archive = ZipArchive::new(File::open(zip).with_context(|| format!("opening {}", zip.display()))?)?;
    let mut text = String::new();
    archive.by_name(CONTENTS).context("it has no backup.json")?.read_to_string(&mut text)?;
    let contents: Contents = serde_json::from_str(&text).context("reading backup.json")?;
    ensure!(contents.format == FORMAT, "backup format {} isn't the {FORMAT} this version reads", contents.format);
    Ok(contents)
}

/// Unpacks the zip into the new folder `into` and checks it as a setup folder; the zip's own
/// description has to agree with what's inside.
pub fn unpack(zip: &Path, into: &Path) -> Result<Contents> {
    let said = read_contents(zip)?;
    let mut archive = ZipArchive::new(File::open(zip)?)?;
    fs::create_dir(into).with_context(|| format!("creating {}", into.display()))?;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        let name = entry.enclosed_name().with_context(|| format!("entry {index} has an unsafe path"))?;
        let path = into.join(name);
        if entry.is_dir() {
            fs::create_dir_all(&path)?;
            continue;
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        io::copy(&mut entry, &mut File::create(&path)?).with_context(|| format!("unpacking {}", path.display()))?;
    }
    let found = verify(into, said.created)?;
    if found.serial != said.serial || found.scanner_firmware != said.scanner_firmware {
        bail!("backup.json says keyboard {} ({}), but the backup inside is {} ({})", said.serial, said.scanner_firmware, found.serial, found.scanner_firmware);
    }
    Ok(said)
}

/// `<serial> <YYYY-MM-DD HH.MM>.zip`, in UTC.
pub fn file_name(serial: &str, created: u64) -> String {
    let (days, seconds) = (created / 86_400, created % 86_400);
    let (year, month, day) = civil_from_days(days as i64);
    format!("{serial} {year:04}-{month:02}-{day:02} {:02}.{:02}.zip", seconds / 3600, seconds % 3600 / 60)
}

/// Days since 1970-01-01 to a calendar date (Howard Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}

/// The setup folder a picked folder holds: the folder itself (`scanner/`, `linux/`), or the one
/// keyboard folder inside a setup root.
pub fn setup_folder(picked: &Path) -> Result<PathBuf> {
    let holds_both = |dir: &Path| dir.join(SCANNER).is_dir() && dir.join(LINUX).is_dir();
    if holds_both(picked) {
        return Ok(picked.to_owned());
    }
    let inside: Vec<PathBuf> = fs::read_dir(picked)
        .with_context(|| format!("reading {}", picked.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| holds_both(p))
        .collect();
    match inside.as_slice() {
        [one] => Ok(one.clone()),
        [] => bail!("{} isn't a keyboard backup: it has no scanner and linux folders", picked.display()),
        _ => bail!("{} holds backups of {} keyboards; pick one of them", picked.display(), inside.len()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_say_the_keyboard_and_when() {
        assert_eq!(file_name("E4645852431F3223", 1_795_827_600), "E4645852431F3223 2026-11-28 01.00.zip");
        assert_eq!(file_name("X", 0), "X 1970-01-01 00.00.zip");
        assert_eq!(file_name("X", 951_782_400), "X 2000-02-29 00.00.zip");
    }

    #[test]
    fn compressed_pieces_are_stored_and_the_rest_deflated() {
        assert_eq!(method("linux/mmcblk0p3.img.gz.000"), CompressionMethod::Stored);
        assert_eq!(method("linux/files.tar.gz"), CompressionMethod::Stored);
        assert_eq!(method("scanner/flash.bin"), CompressionMethod::Deflated);
        assert_eq!(method("probe.txt"), CompressionMethod::Deflated);
    }

    #[test]
    fn reads_the_versions_setup_writes() {
        let versions = parse_versions("bridge: 1.4.8\ndisplay: 1.4.19\nos: 0.0.9\nfree: 4892 MiB\n").unwrap();
        assert_eq!(versions, LinuxVersions { bridge: "1.4.8".into(), display: "1.4.19".into(), os: "0.0.9".into() });
        assert_eq!(parse_versions("bridge: 1.4.8\n"), None);
    }

    #[test]
    fn a_picked_folder_or_the_one_keyboard_in_it() {
        let root = std::env::temp_dir().join(format!("glassdeck-archive-pick-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let keyboard = root.join("E464");
        fs::create_dir_all(keyboard.join(SCANNER)).unwrap();
        fs::create_dir_all(keyboard.join(LINUX)).unwrap();
        assert_eq!(setup_folder(&keyboard).unwrap(), keyboard);
        assert_eq!(setup_folder(&root).unwrap(), keyboard);
        fs::create_dir_all(root.join("other").join(SCANNER)).unwrap();
        fs::create_dir_all(root.join("other").join(LINUX)).unwrap();
        assert!(setup_folder(&root).is_err());
        assert!(setup_folder(&keyboard.join(SCANNER)).is_err());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_zip_holds_the_parts_and_its_description() {
        let root = std::env::temp_dir().join(format!("glassdeck-archive-zip-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let folder = root.join("E464");
        fs::create_dir_all(folder.join(LINUX)).unwrap();
        fs::create_dir_all(folder.join(SCANNER)).unwrap();
        fs::write(folder.join(SCANNER).join("flash.bin"), vec![7u8; 4096]).unwrap();
        fs::write(folder.join(LINUX).join("mmcblk0p1.img.gz.000"), b"gz").unwrap();
        fs::write(folder.join(VERSIONS), "bridge: 1.4.8\n").unwrap();
        fs::write(folder.join("unrelated.txt"), "not a part").unwrap();
        let contents = Contents {
            format: FORMAT,
            serial: "E464".into(),
            created: 1,
            scanner_firmware: "1.4.9.D".into(),
            calibration_version: 2,
            linux: None,
            holds: vec!["scanner".into(), "linux".into()],
        };
        let zip = root.join("E464.zip");
        write(&folder, &contents, &zip).unwrap();
        assert!(!zip.with_extension("zip.partial").exists());
        assert_eq!(read_contents(&zip).unwrap(), contents);
        let archive = ZipArchive::new(File::open(&zip).unwrap()).unwrap();
        let mut names: Vec<String> = archive.file_names().map(|n| n.unwrap().into_owned()).collect();
        names.sort_unstable();
        assert_eq!(names, [CONTENTS, "linux/mmcblk0p1.img.gz.000", "scanner/flash.bin", VERSIONS]);
        // Not a real backup inside, so unpacking refuses it.
        assert!(unpack(&zip, &root.join("unpacked")).is_err());
        fs::remove_dir_all(&root).unwrap();
    }
}
