//! The keyboard itself: what it runs (Flux's own software or Glassdeck), and backing it up with
//! glassdeck-setup before Glassdeck goes on. Setup's steps reach the frontend as [`SetupUpdate`]s.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use glassdeck_api::KeyboardLink;
use glassdeck_setup::bridge::{self, Bridge, Package};
use glassdeck_setup::setup::{self, Ended, Event, Packages, Step};
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
    /// Every step is done.
    BackedUp { folder: String },
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

/// Where backups go unless the user picks another folder.
pub fn default_backups() -> Result<PathBuf, KeyboardError> {
    let home = std::env::var_os("USERPROFILE").ok_or(KeyboardError::Failed { reason: "USERPROFILE isn't set".into() })?;
    Ok(PathBuf::from(home).join("Documents").join("Glassdeck").join("Keyboard backups"))
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
            let ended = fs::create_dir_all(&folder)
                .map_err(anyhow::Error::from)
                .and_then(|()| setup::run(&folder, &packages, &mut |event| emit(&app, event.into())));
            if let Ok(Ended::Done(done)) = ended {
                emit(&app, SetupUpdate::BackedUp { folder: done.display().to_string() });
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

/// A finished backup of one keyboard, as saved in the backups folder.
#[derive(Clone, Debug, Serialize, Type)]
pub struct BackupSummary {
    pub serial: String,
    pub folder: String,
    /// Seconds since 1970 when it was finished.
    pub finished: u64,
    pub firmware: Option<String>,
    pub display: Option<String>,
}

/// Every keyboard folder under `root` whose backup finished: the key scanner's checked copy and
/// the Linux side's `VERIFIED`.
pub fn backups(root: &Path) -> Vec<BackupSummary> {
    let Ok(entries) = fs::read_dir(root) else { return Vec::new() };
    entries
        .filter_map(|e| e.ok())
        .filter_map(|entry| {
            let folder = entry.path();
            let verified = fs::metadata(folder.join("linux").join("VERIFIED")).ok()?;
            let manifest = fs::read_to_string(folder.join("scanner").join("manifest.txt")).ok()?;
            let versions = fs::read_to_string(folder.join("versions.txt")).unwrap_or_default();
            let finished = verified.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
            Some(BackupSummary {
                serial: entry.file_name().to_string_lossy().into_owned(),
                folder: folder.display().to_string(),
                finished,
                firmware: field(&manifest, "firmware"),
                display: field(&versions, "display"),
            })
        })
        .collect()
}

/// The value after `name` on a `name value` or `name: value` line.
fn field(text: &str, name: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let rest = line.strip_prefix(name)?;
        let value = rest.strip_prefix(':').unwrap_or(rest);
        (value.starts_with(' ')).then(|| value.trim().to_owned())
    })
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
    fn reads_fields_from_the_saved_files() {
        assert_eq!(field("firmware 1.4.9.D\nusb_serial E46\n", "firmware").as_deref(), Some("1.4.9.D"));
        assert_eq!(field("bridge: 1.4.8\ndisplay: 1.4.19\n", "display").as_deref(), Some("1.4.19"));
        assert_eq!(field("displayed: x\n", "display"), None);
    }

    #[test]
    fn lists_only_finished_backups() {
        let root = std::env::temp_dir().join(format!("glassdeck-backups-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        for (serial, linux_verified) in [("DONE1", true), ("HALF2", false)] {
            let folder = root.join(serial);
            fs::create_dir_all(folder.join("scanner")).unwrap();
            fs::create_dir_all(folder.join("linux")).unwrap();
            fs::write(folder.join("scanner").join("manifest.txt"), "firmware 1.4.9.D\n").unwrap();
            fs::write(folder.join("versions.txt"), "display: 1.4.19\n").unwrap();
            if linux_verified {
                fs::write(folder.join("linux").join("VERIFIED"), "ok\n").unwrap();
            }
        }
        let found = backups(&root);
        fs::remove_dir_all(&root).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].serial, "DONE1");
        assert_eq!(found[0].firmware.as_deref(), Some("1.4.9.D"));
        assert_eq!(found[0].display.as_deref(), Some("1.4.19"));
    }
}
