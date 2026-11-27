//! `glassdeck scanner`: the key scanner commands, printed from glassdeck-setup's notes.

use std::path::PathBuf;

use anyhow::Result;
use clap::Subcommand;
use glassdeck_setup::Note;
use glassdeck_setup::scanner::{self, Backup, CommandSet};

use crate::printer::Printer;

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
    /// Leave the boot ROM and start the firmware in flash
    Reboot,
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
    let mut printer = Printer::default();
    let notes = &mut |note: Note| printer.note(note);
    match command {
        ScannerCommand::Version => {
            let identity = scanner::identify()?;
            println!("firmware: {}", identity.firmware);
            match (identity.commands, identity.state) {
                (CommandSet::Flux, _) => println!("Flux's own firmware"),
                (CommandSet::Keyglass, Some(s)) => println!(
                    "profile {}, actuation mode {}, platform {}",
                    s.active_profile, s.actuation_mode, s.platform_code
                ),
                (CommandSet::Keyglass, None) => println!("runtime state: not answered"),
            }
        }
        ScannerCommand::Backup { dir } => {
            let described = scanner::backup(&dir, notes)?;
            println!("backup complete ({described})");
        }
        ScannerCommand::CheckBackup { dir } => {
            let backup = Backup::load(&dir)?;
            println!(
                "{} is a complete backup: {}",
                dir.display(),
                backup.describe()
            );
        }
        ScannerCommand::Bootloader => {
            let drive = scanner::enter_boot_rom(notes)?;
            println!(
                "The boot ROM's drive is {}; copy a .uf2 onto it to flash",
                drive.display()
            );
        }
        ScannerCommand::Reboot => {
            scanner::reboot()?;
            println!("the key scanner is starting its firmware");
        }
        ScannerCommand::Flash { uf2, backup } => scanner::flash(&uf2, &backup, notes)?,
        ScannerCommand::Restore { backup, settings } => scanner::restore(&backup, settings, notes)?,
    }
    Ok(())
}
