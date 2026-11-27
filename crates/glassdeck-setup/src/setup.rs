//! The first-day routine as one resumable run: find both chips, back them up, verify. What's
//! done is read from the keyboard's folder, so running it again after an interruption carries on
//! where it stopped. A step writes into `<name>.partial` and renames it only once it checks out.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::bridge::{self, Bridge, OnKeyboard, Package};
use crate::scanner::{self, Driver};
use crate::{Note, Notes, said};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    ScannerFound,
    ScannerDriver,
    ScannerBackup,
    BridgeFound,
    Probe,
    LinuxBackup,
}

/// What shows a step was done in an earlier run.
#[derive(Clone, Copy, Debug)]
enum Proof {
    /// Nothing; it's done again every run.
    ThisRunOnly,
    /// Done whenever this other step is.
    Implied(Step),
    /// This file or folder in the keyboard's folder.
    Saved(&'static str),
}

const SCANNER_BACKUP: &str = "scanner";
const VERSIONS: &str = "versions.txt";
const PROBE: &str = "probe.txt";
const LINUX_BACKUP: &str = "linux";
const PARTIAL: &str = ".partial";

/// Every step, in the order setup runs them.
const STEPS: [(Step, Proof); 6] = [
    (Step::ScannerFound, Proof::ThisRunOnly),
    (Step::ScannerDriver, Proof::Implied(Step::ScannerBackup)),
    (Step::ScannerBackup, Proof::Saved(SCANNER_BACKUP)),
    (Step::BridgeFound, Proof::Saved(VERSIONS)),
    (Step::Probe, Proof::Saved(PROBE)),
    (Step::LinuxBackup, Proof::Saved(LINUX_BACKUP)),
];

const ZADIG: [&str; 4] = [
    "In Zadig, turn on Options > List All Devices",
    "Pick \"RP2 Boot (Interface 1)\", whose driver shows (NONE). Not Interface 0: that's the boot ROM's drive",
    "Set the target to WinUSB and click Install Driver",
    "Run setup again",
];

fn proof(step: Step) -> Proof {
    STEPS
        .iter()
        .find(|(s, _)| *s == step)
        .map(|(_, p)| *p)
        .expect("every step is in STEPS")
}

fn is_done(step: Step, saved: &[&str], finished: &[Step]) -> bool {
    finished.contains(&step)
        || match proof(step) {
            Proof::ThisRunOnly => false,
            Proof::Implied(other) => is_done(other, saved, finished),
            Proof::Saved(name) => saved.contains(&name),
        }
}

/// The first step not yet done, given which proofs are `saved` in the keyboard's folder and
/// which steps this run has `finished`.
pub fn next(saved: &[&str], finished: &[Step]) -> Option<Step> {
    STEPS
        .iter()
        .map(|(step, _)| *step)
        .find(|&step| !is_done(step, saved, finished))
}

/// The proofs present in `folder`. A `.partial` leftover is never one.
fn saved_in(folder: &Path) -> Vec<&'static str> {
    STEPS
        .iter()
        .filter_map(|(_, proof)| match proof {
            Proof::Saved(name) => folder.join(name).exists().then_some(*name),
            _ => None,
        })
        .collect()
}

/// Where a step writes before its result checks out. A leftover from an interrupted run is
/// thrown away first.
fn start_partial(folder: &Path, name: &str) -> Result<PathBuf> {
    let partial = folder.join(format!("{name}{PARTIAL}"));
    if partial.is_dir() {
        fs::remove_dir_all(&partial)
    } else if partial.exists() {
        fs::remove_file(&partial)
    } else {
        Ok(())
    }
    .with_context(|| format!("clearing the leftover {}", partial.display()))?;
    Ok(partial)
}

fn finish_partial(partial: &Path, folder: &Path, name: &str) -> Result<()> {
    fs::rename(partial, folder.join(name))
        .with_context(|| format!("renaming {} to {name}", partial.display()))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Started(Step),
    Progress(Step, Note),
    /// What the user has to do before setup can go on; the run stops here.
    NeedsYou(Step, Vec<String>),
    Finished(Step),
    Failed(Step, String),
}

/// The packages setup installs on the keyboard, read and checked before anything touches it.
pub struct Packages {
    pub probe: Package,
    pub backup: Package,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Ended {
    /// Every step is done; everything is in this folder.
    Done(PathBuf),
    /// Stopped at a [`Event::NeedsYou`]; run setup again once that's done.
    WaitingOnYou,
}

enum Outcome {
    Finished,
    NeedsYou(Vec<String>),
}

/// Runs every step not yet done for the plugged-in keyboard, in `root\<its USB serial>`.
pub fn run(root: &Path, packages: &Packages, on_event: &mut dyn FnMut(Event)) -> Result<Ended> {
    let mut finished = Vec::new();
    let mut folder = None;
    loop {
        let saved = folder.as_deref().map(saved_in).unwrap_or_default();
        let Some(step) = next(&saved, &finished) else {
            return Ok(Ended::Done(
                folder.expect("the first step names the folder"),
            ));
        };
        on_event(Event::Started(step));
        let outcome = run_step(step, root, &mut folder, packages, &mut |note| {
            on_event(Event::Progress(step, note))
        });
        match outcome {
            Ok(Outcome::Finished) => {
                finished.push(step);
                on_event(Event::Finished(step));
            }
            Ok(Outcome::NeedsYou(todo)) => {
                on_event(Event::NeedsYou(step, todo));
                return Ok(Ended::WaitingOnYou);
            }
            Err(e) => {
                on_event(Event::Failed(step, format!("{e:#}")));
                return Err(e);
            }
        }
    }
}

fn run_step(
    step: Step,
    root: &Path,
    folder: &mut Option<PathBuf>,
    packages: &Packages,
    notes: Notes,
) -> Result<Outcome> {
    if step == Step::ScannerFound {
        return find_scanner(root, folder, notes);
    }
    let folder = folder.as_deref().expect("the first step names the folder");
    match step {
        Step::ScannerFound => unreachable!("handled above"),
        Step::ScannerDriver => Ok(match scanner::check_driver(notes)? {
            Driver::Ready => Outcome::Finished,
            Driver::Missing => Outcome::NeedsYou(ZADIG.map(String::from).to_vec()),
        }),
        Step::ScannerBackup => {
            let partial = start_partial(folder, SCANNER_BACKUP)?;
            let described = scanner::backup(&partial, notes)?;
            finish_partial(&partial, folder, SCANNER_BACKUP)?;
            said(notes, format!("key scanner backed up: {described}"));
            Ok(Outcome::Finished)
        }
        Step::BridgeFound => {
            let versions = bridge::versions(&mut Bridge::open_when_back()?)?;
            let partial = start_partial(folder, VERSIONS)?;
            fs::write(&partial, versions.describe())
                .with_context(|| format!("writing {}", partial.display()))?;
            finish_partial(&partial, folder, VERSIONS)?;
            said(notes, versions.describe().trim_end().replace('\n', ", "));
            Ok(Outcome::Finished)
        }
        Step::Probe => {
            let partial = start_partial(folder, PROBE)?;
            bridge::probe(&mut Bridge::open()?, &packages.probe, &partial, notes)?;
            finish_partial(&partial, folder, PROBE)?;
            Ok(Outcome::Finished)
        }
        Step::LinuxBackup => {
            let partial = start_partial(folder, LINUX_BACKUP)?;
            let results = bridge::backup(
                &mut Bridge::open()?,
                &packages.backup,
                &partial,
                OnKeyboard::ReuseFinished,
                notes,
            )?;
            finish_partial(&partial, folder, LINUX_BACKUP)?;
            for line in results.lines() {
                said(notes, line);
            }
            Ok(Outcome::Finished)
        }
    }
}

/// Finds the key scanner (bringing it out of its boot ROM if a last run left it there) and
/// names the keyboard's folder after its USB serial.
fn find_scanner(root: &Path, folder: &mut Option<PathBuf>, notes: Notes) -> Result<Outcome> {
    if scanner::in_boot_rom() {
        said(
            notes,
            "the key scanner is in its boot ROM; starting its firmware",
        );
        if scanner::leave_boot_rom().is_err() {
            let mut todo = vec![
                "Unplug the keyboard and plug it back in, or give the boot ROM its driver:"
                    .to_owned(),
            ];
            todo.extend(ZADIG.map(String::from));
            return Ok(Outcome::NeedsYou(todo));
        }
    }
    let identity = scanner::identify()?;
    let serial = identity.usb_serial.context(
        "the key scanner reports no USB serial, so setup can't name this keyboard's folder",
    )?;
    let named = root.join(&serial);
    fs::create_dir_all(&named).with_context(|| format!("creating {}", named.display()))?;
    said(
        notes,
        format!(
            "key scanner: firmware {}, USB serial {serial}; saving to {}",
            identity.firmware,
            named.display()
        ),
    );
    *folder = Some(named);
    Ok(Outcome::Finished)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_walks_the_table_in_order() {
        let mut finished = Vec::new();
        for (step, _) in STEPS {
            assert_eq!(next(&[], &finished), Some(step));
            finished.push(step);
        }
        assert_eq!(next(&[], &finished), None);
    }

    #[test]
    fn the_scanner_is_found_every_run_even_when_all_is_saved() {
        let all = [SCANNER_BACKUP, VERSIONS, PROBE, LINUX_BACKUP];
        assert_eq!(next(&all, &[]), Some(Step::ScannerFound));
        assert_eq!(next(&all, &[Step::ScannerFound]), None);
    }

    #[test]
    fn a_saved_scanner_backup_means_the_driver_step_is_done() {
        assert_eq!(
            next(&[SCANNER_BACKUP], &[Step::ScannerFound]),
            Some(Step::BridgeFound)
        );
        assert_eq!(next(&[], &[Step::ScannerFound]), Some(Step::ScannerDriver));
    }

    fn folder(name: &str, entries: &[&str]) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("glassdeck-setup-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        for entry in entries {
            if entry.ends_with(".txt") {
                fs::write(dir.join(entry), "x").unwrap();
            } else {
                fs::create_dir(dir.join(entry)).unwrap();
            }
        }
        dir
    }

    #[test]
    fn each_stage_of_a_folder_picks_the_right_step() {
        let found = [Step::ScannerFound];
        let stages: [(&[&str], Option<Step>); 6] = [
            (&[], Some(Step::ScannerDriver)),
            (&["scanner.partial"], Some(Step::ScannerDriver)),
            (&[SCANNER_BACKUP], Some(Step::BridgeFound)),
            (&[SCANNER_BACKUP, VERSIONS], Some(Step::Probe)),
            (
                &[SCANNER_BACKUP, VERSIONS, PROBE, "linux.partial"],
                Some(Step::LinuxBackup),
            ),
            (&[SCANNER_BACKUP, VERSIONS, PROBE, LINUX_BACKUP], None),
        ];
        for (i, (entries, want)) in stages.into_iter().enumerate() {
            let dir = folder(&format!("stage{i}"), entries);
            assert_eq!(next(&saved_in(&dir), &found), want, "{entries:?}");
            fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn a_leftover_partial_is_cleared_and_only_a_finished_one_counts() {
        let dir = folder("partial", &["linux.partial"]);
        fs::write(dir.join("linux.partial").join("piece"), "half").unwrap();
        let partial = start_partial(&dir, LINUX_BACKUP).unwrap();
        assert!(!partial.exists());
        fs::create_dir(&partial).unwrap();
        assert!(!saved_in(&dir).contains(&LINUX_BACKUP));
        finish_partial(&partial, &dir, LINUX_BACKUP).unwrap();
        assert!(saved_in(&dir).contains(&LINUX_BACKUP));
        fs::remove_dir_all(dir).unwrap();
    }
}
