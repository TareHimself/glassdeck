//! The keyboard itself: what it runs (Flux's own software or Glassdeck), and backing it up with
//! glassdeck-setup before Glassdeck goes on. Setup's steps reach the frontend as [`SetupUpdate`]s.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use glassdeck_api::KeyboardLink;
use glassdeck_setup::bridge::{self, Bridge, Package};
use glassdeck_setup::setup::{self, Ended, Event, Packages, Step};
use glassdeck_setup::archive::{self, Contents};
use glassdeck_setup::{Note, scanner};
use serde::Serialize;
use specta::Type;
use tauri::AppHandle;
use tauri_specta::Event as _;

/// What the keyboard runs, as far as the host can tell.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum Software {
    /// No keyboard is plugged in, or neither chip answers.
    Absent,
    /// Flux's own firmware and display software: Glassdeck isn't on it yet.
    Flux,
    Glassdeck,
}

const GLASSDECK_FIRMWARE: &str = "glassdeck-scanner ";

/// The receiver only exists on a keyboard with Glassdeck, so a Link that answers at all (even
/// with another version) settles it; otherwise the key scanner's firmware does.
pub fn software(scanner_firmware: Option<&str>, link: KeyboardLink) -> Software {
    match (link, scanner_firmware) {
        (KeyboardLink::Connected | KeyboardLink::Incompatible { .. }, _) => Software::Glassdeck,
        (KeyboardLink::Unreachable, Some(firmware)) if firmware.starts_with(GLASSDECK_FIRMWARE) => Software::Glassdeck,
        (KeyboardLink::Unreachable, Some(_)) => Software::Flux,
        (KeyboardLink::Unreachable, None) => Software::Absent,
    }
}

/// What the stock bridge on the keyboard's computer reports.
#[derive(Clone, Debug, Serialize, Type)]
pub struct Computer {
    pub bridge: String,
    pub display: String,
    pub os: String,
    pub free_bytes: u64,
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct Keyboard {
    pub software: Software,
    /// The key scanner's USB serial, which names the keyboard's backup folder.
    pub serial: Option<String>,
    pub firmware: Option<String>,
    pub computer: Option<Computer>,
}

/// Asks both chips over USB. A chip that doesn't answer is left out rather than failing the rest.
pub fn look(link: KeyboardLink) -> Keyboard {
    let identity = scanner::identify().ok();
    let computer = Bridge::open().and_then(|mut b| bridge::versions(&mut b)).ok().map(|v| Computer {
        bridge: v.bridge,
        display: v.display,
        os: v.os,
        free_bytes: v.free_bytes,
    });
    let firmware = identity.as_ref().map(|i| i.firmware.clone());
    Keyboard {
        software: software(firmware.as_deref(), link),
        serial: identity.and_then(|i| i.usb_serial),
        firmware,
        computer,
    }
}

/// Setup's steps as the frontend names them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum SetupStep {
    ScannerFound,
    ScannerDriver,
    ScannerBackup,
    BridgeFound,
    Probe,
    LinuxBackup,
}

impl From<Step> for SetupStep {
    fn from(step: Step) -> Self {
        match step {
            Step::ScannerFound => Self::ScannerFound,
            Step::ScannerDriver => Self::ScannerDriver,
            Step::ScannerBackup => Self::ScannerBackup,
            Step::BridgeFound => Self::BridgeFound,
            Step::Probe => Self::Probe,
            Step::LinuxBackup => Self::LinuxBackup,
        }
    }
}

#[derive(Clone, Debug, Serialize, Type, tauri_specta::Event)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SetupUpdate {
    Started { step: SetupStep },
    /// Something that happened, or with `progress`, how far a transfer has got.
    Note { step: SetupStep, text: String, progress: bool },
    /// What the user has to do before the backup can go on; the run has stopped.
    NeedsYou { step: SetupStep, todo: Vec<String> },
    Finished { step: SetupStep },
    Failed { step: SetupStep, reason: String },
    /// Every step is done and the backup is saved, checked, as this zip.
    BackedUp { file: String },
    /// The run is over, however it ended.
    Ended,
}

impl From<Event> for SetupUpdate {
    fn from(event: Event) -> Self {
        match event {
            Event::Started(step) => Self::Started { step: step.into() },
            Event::Progress(step, Note::Said(text)) => Self::Note { step: step.into(), text, progress: false },
            Event::Progress(step, Note::Progress(text)) => Self::Note { step: step.into(), text, progress: true },
            Event::NeedsYou(step, todo) => Self::NeedsYou { step: step.into(), todo },
            Event::Finished(step) => Self::Finished { step: step.into() },
            Event::Failed(step, reason) => Self::Failed { step: step.into(), reason },
        }
    }
}

#[derive(Clone, Debug, Serialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum KeyboardError {
    /// A backup or restore is already running.
    Busy,
    /// Not possible yet, and why.
    Unavailable { reason: String },
    Failed { reason: String },
}

/// Whether a backup is running; only one may talk to the keyboard at a time.
#[derive(Clone, Default)]
pub struct Running(Arc<AtomicBool>);

impl Running {
    pub fn is_running(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// Where backups go unless the user picks another folder: the app's own data.
pub fn default_backups() -> Result<PathBuf, KeyboardError> {
    let data = std::env::var_os("APPDATA").ok_or(KeyboardError::Failed { reason: "APPDATA isn't set".into() })?;
    Ok(PathBuf::from(data).join("Glassdeck").join("Keyboard backups"))
}

/// Setup works in here, so an interrupted backup carries on; only finished, checked backups
/// leave it, as zips.
const STAGING: &str = ".staging";

fn failed(what: &str) -> impl FnOnce(anyhow::Error) -> KeyboardError + '_ {
    move |e| KeyboardError::Failed { reason: format!("{what}: {e:#}") }
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Checks a finished setup folder and saves it into `store` as one zip; the folder goes once the
/// zip is written.
fn save_backup(folder: &Path, store: &Path, created: u64) -> anyhow::Result<PathBuf> {
    let contents = archive::verify(folder, created)?;
    let zip = store.join(archive::file_name(&contents.serial, contents.created));
    archive::write(folder, &contents, &zip)?;
    Ok(zip)
}

/// The keyboard packages setup installs: `GLASSDECK_PACKAGES`, else `packages` next to this
/// program, or in a dev build the workspace's `target/packages`.
fn packages_dir() -> Result<PathBuf, KeyboardError> {
    if let Some(dir) = std::env::var_os("GLASSDECK_PACKAGES") {
        return Ok(PathBuf::from(dir));
    }
    let exe = std::env::current_exe().map_err(|e| KeyboardError::Failed { reason: format!("finding this program: {e}") })?;
    let beside = exe.parent().ok_or(KeyboardError::Failed { reason: "this program has no folder".into() })?;
    let dev = beside.join("..").join("packages");
    Ok(if dev.is_dir() { dev } else { beside.join("packages") })
}

fn read_packages() -> Result<Packages, KeyboardError> {
    let dir = packages_dir()?;
    let read = |name: &str| {
        Package::read(&dir.join(name)).map_err(|e| KeyboardError::Unavailable { reason: format!("{name} isn't in {}: {e:#}", dir.display()) })
    };
    Ok(Packages { probe: read("glassdeck-probe.ipk")?, backup: read("glassdeck-backup.ipk")? })
}

/// Starts the backup on its own thread; its steps arrive as [`SetupUpdate`]s, ending with
/// [`SetupUpdate::Ended`].
pub fn start_backup(app: AppHandle, running: Running, folder: PathBuf) -> Result<(), KeyboardError> {
    let packages = read_packages()?;
    if running.0.swap(true, Ordering::SeqCst) {
        return Err(KeyboardError::Busy);
    }
    let emit = move |app: &AppHandle, update: SetupUpdate| {
        if let Err(e) = update.emit(app) {
            eprintln!("emitting a setup update: {e}");
        }
    };
    let worker = running.clone();
    std::thread::Builder::new()
        .name("keyboard backup".into())
        .spawn(move || {
            let staging = folder.join(STAGING);
            let ended = fs::create_dir_all(&staging)
                .map_err(anyhow::Error::from)
                .and_then(|()| setup::run(&staging, &packages, &mut |event| emit(&app, event.into())));
            if let Ok(Ended::Done(done)) = ended {
                match save_backup(&done, &folder, now()) {
                    Ok(zip) => {
                        if let Err(e) = fs::remove_dir_all(&done) {
                            eprintln!("removing the staged backup {}: {e}", done.display());
                        }
                        emit(&app, SetupUpdate::BackedUp { file: zip.display().to_string() });
                    }
                    Err(e) => emit(&app, SetupUpdate::Failed { step: SetupStep::LinuxBackup, reason: format!("saving the backup: {e:#}") }),
                }
            }
            worker.0.store(false, Ordering::SeqCst);
            emit(&app, SetupUpdate::Ended);
        })
        .map_err(|e| {
            running.0.store(false, Ordering::SeqCst);
            KeyboardError::Failed { reason: format!("starting the backup: {e}") }
        })?;
    Ok(())
}

/// One saved backup: a zip in the backups folder.
#[derive(Clone, Debug, Serialize, Type)]
pub struct BackupSummary {
    pub serial: String,
    pub file: String,
    /// Seconds since 1970 when it was made.
    pub created: u64,
    pub firmware: String,
    /// The display software on the keyboard's computer when it was backed up.
    pub display: Option<String>,
    pub bytes: u64,
}

fn summary(zip: &Path, contents: Contents) -> BackupSummary {
    BackupSummary {
        serial: contents.serial,
        file: zip.display().to_string(),
        created: contents.created,
        firmware: contents.scanner_firmware,
        display: contents.linux.map(|l| l.display),
        bytes: fs::metadata(zip).map_or(0, |m| m.len()),
    }
}

/// Every backup zip in `root`, newest first. A zip that can't be read is reported, not listed.
pub fn backups(root: &Path) -> Vec<BackupSummary> {
    let Ok(entries) = fs::read_dir(root) else { return Vec::new() };
    let mut found: Vec<BackupSummary> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "zip"))
        .filter_map(|zip| match archive::read_contents(&zip) {
            Ok(contents) => Some(summary(&zip, contents)),
            Err(e) => {
                eprintln!("error: {} isn't a readable backup: {e:#}", zip.display());
                None
            }
        })
        .collect();
    found.sort_by_key(|b| std::cmp::Reverse(b.created));
    found
}

/// Whether a backup may join this keyboard's: any backup when no keyboard is plugged in,
/// otherwise only one from this keyboard.
pub fn belongs(backup_serial: &str, keyboard_serial: Option<&str>) -> Result<(), KeyboardError> {
    match keyboard_serial {
        Some(keyboard) if keyboard != backup_serial => Err(KeyboardError::Failed {
            reason: format!("This backup is of keyboard {backup_serial}, not the one plugged in ({keyboard})"),
        }),
        _ => Ok(()),
    }
}

/// Copies a backup made elsewhere into `store`: a zip, or a folder `glassdeck keyboard setup`
/// made. Either is checked in full first.
pub fn import(store: &Path, picked: &Path, keyboard_serial: Option<&str>) -> Result<BackupSummary, KeyboardError> {
    fs::create_dir_all(store).map_err(|e| failed("creating the backups folder")(e.into()))?;
    if picked.extension().is_some_and(|x| x == "zip") {
        let contents = check_unpacked(store, picked, "import")?;
        belongs(&contents.serial, keyboard_serial)?;
        let zip = store.join(archive::file_name(&contents.serial, contents.created));
        fs::copy(picked, &zip).map_err(|e| failed("copying the backup")(e.into()))?;
        return Ok(summary(&zip, contents));
    }
    let folder = archive::setup_folder(picked).map_err(failed("finding the backup"))?;
    let finished = fs::metadata(folder.join("linux").join("VERIFIED"))
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or_else(now, |d| d.as_secs());
    let contents = archive::verify(&folder, finished).map_err(failed("checking the backup"))?;
    belongs(&contents.serial, keyboard_serial)?;
    let zip = store.join(archive::file_name(&contents.serial, contents.created));
    archive::write(&folder, &contents, &zip).map_err(failed("saving the backup"))?;
    Ok(summary(&zip, contents))
}

/// Unpacks `zip` into a scratch folder under `store`, checks it in full and removes it again.
fn check_unpacked(store: &Path, zip: &Path, why: &str) -> Result<Contents, KeyboardError> {
    let scratch = store.join(STAGING).join(format!("{why}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&scratch);
    fs::create_dir_all(store.join(STAGING)).map_err(|e| failed("creating the scratch folder")(e.into()))?;
    let checked = archive::unpack(zip, &scratch);
    let _ = fs::remove_dir_all(&scratch);
    checked.map_err(failed("checking the backup"))
}

/// Unpacks and checks the backup a restore would use.
pub fn check_for_restore(zip: &Path) -> Result<Contents, KeyboardError> {
    let store = zip.parent().ok_or(KeyboardError::Failed { reason: "the backup has no folder".into() })?;
    check_unpacked(store, zip, "restore")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_link_that_answers_means_glassdeck() {
        assert_eq!(software(None, KeyboardLink::Connected), Software::Glassdeck);
        assert_eq!(software(Some("1.4.9.D"), KeyboardLink::Incompatible { keyboard: 13, host: 14 }), Software::Glassdeck);
    }

    #[test]
    fn without_a_link_the_key_scanner_firmware_decides() {
        assert_eq!(software(Some("1.4.9.D"), KeyboardLink::Unreachable), Software::Flux);
        assert_eq!(software(Some("glassdeck-scanner 0.0.1"), KeyboardLink::Unreachable), Software::Glassdeck);
        assert_eq!(software(None, KeyboardLink::Unreachable), Software::Absent);
    }

    #[test]
    fn a_backup_joins_only_its_own_keyboard() {
        assert!(belongs("E464", Some("E464")).is_ok());
        assert!(belongs("E464", None).is_ok());
        assert!(belongs("E464", Some("F123")).is_err());
    }

    #[test]
    fn lists_readable_backup_zips_newest_first() {
        let root = std::env::temp_dir().join(format!("glassdeck-backups-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let folder = root.join("src");
        fs::create_dir_all(folder.join("scanner")).unwrap();
        fs::write(folder.join("scanner").join("flash.bin"), [0u8; 16]).unwrap();
        for (serial, created) in [("OLD", 100), ("NEW", 200)] {
            let contents = Contents {
                format: archive::FORMAT,
                serial: serial.into(),
                created,
                scanner_firmware: "1.4.9.D".into(),
                calibration_version: 2,
                linux: archive::parse_versions("bridge: 1.4.8\ndisplay: 1.4.19\nos: 0.0.9\n"),
                holds: vec!["scanner".into()],
            };
            archive::write(&folder, &contents, &root.join(archive::file_name(serial, created))).unwrap();
        }
        fs::write(root.join("broken.zip"), "not a zip").unwrap();
        let found = backups(&root);
        fs::remove_dir_all(&root).unwrap();
        assert_eq!(found.iter().map(|b| b.serial.as_str()).collect::<Vec<_>>(), ["NEW", "OLD"]);
        assert_eq!(found[0].display.as_deref(), Some("1.4.19"));
        assert!(found[0].bytes > 0);
    }
}
