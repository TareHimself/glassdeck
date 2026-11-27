//! `glassdeck keyboard`: the Linux side through its stock bridge, and the whole first-day setup,
//! printed from glassdeck-setup's notes and events.

use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::Subcommand;
use glassdeck_setup::Note;
use glassdeck_setup::bridge::{self, Bridge, OnKeyboard, PROBE_REPORT, Package};
use glassdeck_setup::setup::{self, Ended, Packages};

use crate::printer::Printer;

const PROBE_PACKAGE: &str = "target/packages/glassdeck-probe.ipk";
const BACKUP_PACKAGE: &str = "target/packages/glassdeck-backup.ipk";

#[derive(Subcommand)]
pub enum KeyboardCommand {
    /// The bridge, display and OS versions, and free space, from the stock bridge
    Versions,
    /// Install an .ipk: it goes to /home/root, where the keyboard installs it as root
    Install { ipk: PathBuf },
    /// The end of Glassdeck's logs on the keyboard: the bootstrap's install, the launcher, the receiver
    Logs,
    /// Install glassdeck-probe, wait for its report and save it
    Probe {
        #[arg(long, default_value = PROBE_PACKAGE)]
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
        #[arg(long, default_value = BACKUP_PACKAGE)]
        package: PathBuf,
    },
    /// The whole first-day routine: find both chips, back them up and verify, into
    /// <folder>\<USB serial>. Run it again after an interruption and it carries on.
    Setup {
        folder: PathBuf,
        #[arg(long, default_value = PROBE_PACKAGE)]
        probe: PathBuf,
        #[arg(long, default_value = BACKUP_PACKAGE)]
        backup: PathBuf,
    },
}

pub fn run(command: KeyboardCommand) -> Result<()> {
    let mut printer = Printer::default();
    let notes = &mut |note: Note| printer.note(note);
    // Packages are checked before anything touches the keyboard.
    match command {
        KeyboardCommand::Versions => {
            print!("{}", bridge::versions(&mut Bridge::open()?)?.describe())
        }
        KeyboardCommand::Logs => {
            for log in bridge::logs(&mut Bridge::open()?)? {
                match log.tail {
                    None => println!("===== {}: not there", log.path),
                    Some((lines, total)) => {
                        println!("===== {} (last {} of {total} lines)", log.path, lines.len());
                        for line in lines {
                            println!("{line}");
                        }
                    }
                }
            }
        }
        KeyboardCommand::Install { ipk } => {
            let package = Package::read(&ipk)?;
            bridge::install(&mut Bridge::open()?, &package, notes)?;
        }
        KeyboardCommand::Probe { package, out } => {
            let package = Package::read(&package)?;
            bridge::probe(&mut Bridge::open()?, &package, &out, notes)?;
        }
        KeyboardCommand::Backup { dir, package } => {
            let package = Package::read(&package)?;
            let results = bridge::backup(
                &mut Bridge::open()?,
                &package,
                &dir,
                OnKeyboard::Fresh,
                notes,
            )?;
            print!("backup complete\n{results}");
        }
        KeyboardCommand::Setup {
            folder,
            probe,
            backup,
        } => run_setup(&folder, &probe, &backup)?,
    }
    Ok(())
}

fn run_setup(folder: &Path, probe: &Path, backup: &Path) -> Result<()> {
    let packages = Packages {
        probe: Package::read(probe)?,
        backup: Package::read(backup)?,
    };
    let mut printer = Printer::default();
    match setup::run(folder, &packages, &mut |event| printer.event(event))? {
        Ended::Done(dir) => println!(
            "setup complete: everything is in {}. Copy it somewhere off this PC too; the factory calibration can't be remade.",
            dir.display()
        ),
        Ended::WaitingOnYou => println!("run the same command again once that's done"),
    }
    Ok(())
}
