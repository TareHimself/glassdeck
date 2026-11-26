//! Packages the host installs on the keyboard. Pieces go into `<name>.tmp`, which is opened as a
//! package to check it, then renamed into place; the director and widget host pick it up as usual.

use std::fs::{self, File};
use std::io::Write;
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, Sender};

use anyhow::{Context, Result};
use glassdeck_package::{Package, check_file_name};
use glassdeck_proto::{FileTarget, LARGE_FILE_BYTES, Message, PutFile};

use crate::director::{SharedDirector, lock};
use crate::{report, send_reply};
use crate::widgets::Input;

/// Pieces waiting for the disk (8 MB) before the connection waits too.
pub const QUEUED_PIECES: usize = 32;

/// The file writer thread for one connection: disk writes and the final flush, which takes seconds
/// for a large file on an SD card, happen here so the receive thread keeps decoding frames.
pub fn write_files(folders: &Folders, pieces: Receiver<PutFile>, replies: &Mutex<TcpStream>, widgets: &Sender<Input>, director: &SharedDirector) {
    let mut uploads = Uploads::new(folders);
    let mut installing = false;
    for put in pieces {
        let arriving = put.total >= LARGE_FILE_BYTES && !put.completes();
        if arriving {
            if !installing {
                installing = true;
                report(lock(director).install(true));
            }
            show(widgets, Input::InstallProgress { what: put.target.to_string(), fraction: put.end() as f32 / put.total as f32 });
        }
        let answer = uploads.put(&put);
        if installing && !arriving {
            installing = false;
            end_install(widgets, director);
        }
        if let Some(answer) = answer
            && let Err(e) = send_reply(replies, &answer)
        {
            eprintln!("error: answering {}: {e:#}", put.target);
        }
    }
    uploads.abandon();
    if installing {
        end_install(widgets, director);
    }
}

/// After the file is in place, so a background coming back plays the new one.
fn end_install(widgets: &Sender<Input>, director: &SharedDirector) {
    show(widgets, Input::InstallEnded);
    report(lock(director).install(false));
}

/// A stopped widget thread is reported by the receive thread, which also sends it widget data.
fn show(widgets: &Sender<Input>, input: Input) {
    let _ = widgets.send(input);
}

pub struct Folders {
    pub packages: PathBuf,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum Phase {
    #[default]
    Idle,
    Receiving { target: FileTarget, written: u64, total: u64 },
    /// Pieces of a file that won't be installed, read until its last piece so the refusal answers it.
    Refusing { reason: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Effect {
    /// Delete the unfinished file
    Discard,
    Create,
    Append,
    /// Move the finished file into place and answer `PutDone`
    Install,
    /// Answer `PutRefused`
    Refuse(String),
}

/// Every move between upload phases goes through here; the test walks the full table.
fn step(phase: Phase, put: &PutFile) -> (Phase, Vec<Effect>) {
    let mut effects = Vec::new();
    let starts = put.offset == 0;
    let phase = match phase {
        Phase::Receiving { .. } if starts => {
            effects.push(Effect::Discard);
            Phase::Idle
        }
        Phase::Refusing { .. } if starts => Phase::Idle,
        other => other,
    };
    let len = put.data.len() as u64;
    let phase = match phase {
        Phase::Idle if starts => match check_name(&put.target) {
            Ok(()) => {
                effects.extend([Effect::Create, Effect::Append]);
                Phase::Receiving { target: put.target.clone(), written: len, total: put.total }
            }
            Err(reason) => Phase::Refusing { reason },
        },
        Phase::Idle => Phase::Refusing { reason: format!("a piece at byte {} arrived with no start", put.offset) },
        Phase::Receiving { target, written, total } if target == put.target && written == put.offset && total == put.total => {
            effects.push(Effect::Append);
            Phase::Receiving { target, written: written + len, total }
        }
        Phase::Receiving { target, .. } => {
            effects.push(Effect::Discard);
            Phase::Refusing { reason: format!("pieces of {target} arrived out of order") }
        }
        refusing @ Phase::Refusing { .. } => refusing,
    };
    if !put.completes() {
        return (phase, effects);
    }
    match phase {
        Phase::Receiving { written, total, .. } if written == total => effects.push(Effect::Install),
        Phase::Receiving { written, total, .. } => {
            effects.extend([Effect::Discard, Effect::Refuse(format!("got {written} bytes of {total}"))]);
        }
        Phase::Refusing { reason } => effects.push(Effect::Refuse(reason)),
        Phase::Idle => unreachable!("a piece always leaves an upload receiving or refusing"),
    }
    (Phase::Idle, effects)
}

fn check_name(target: &FileTarget) -> Result<(), String> {
    let FileTarget::Package(name) = target;
    check_file_name(name).map(drop).map_err(|e| format!("{e:#}"))
}

/// One connection's uploads.
struct Uploads<'a> {
    folders: &'a Folders,
    phase: Phase,
    file: Option<(File, PathBuf)>,
}

impl<'a> Uploads<'a> {
    fn new(folders: &'a Folders) -> Self {
        Self { folders, phase: Phase::Idle, file: None }
    }

    /// Writes a piece, and returns the answer when the piece completes its file.
    fn put(&mut self, put: &PutFile) -> Option<Message> {
        let (next, effects) = step(std::mem::take(&mut self.phase), put);
        self.phase = next;
        for effect in effects {
            match self.apply(effect, put) {
                Ok(Some(answer)) => return Some(answer),
                Ok(None) => {}
                Err(e) => {
                    self.abandon();
                    let reason = format!("{e:#}");
                    if put.completes() {
                        return Some(refused(put, reason));
                    }
                    self.phase = Phase::Refusing { reason };
                    return None;
                }
            }
        }
        None
    }

    /// Deletes an unfinished file, as when the connection drops mid-upload.
    fn abandon(&mut self) {
        if let Some((file, path)) = self.file.take() {
            drop(file);
            remove_unfinished(&path);
        }
        self.phase = Phase::Idle;
    }

    fn apply(&mut self, effect: Effect, put: &PutFile) -> Result<Option<Message>> {
        match effect {
            Effect::Discard => {
                let phase = std::mem::take(&mut self.phase);
                self.abandon();
                self.phase = phase;
            }
            Effect::Create => {
                let path = staging(&self.place(&put.target));
                if let Some(folder) = path.parent() {
                    fs::create_dir_all(folder).with_context(|| format!("creating {}", folder.display()))?;
                }
                let file = File::create(&path).with_context(|| format!("creating {}", path.display()))?;
                self.file = Some((file, path));
            }
            Effect::Append => {
                let (file, path) = self.file.as_mut().context("no file is open")?;
                file.write_all(&put.data).with_context(|| format!("writing {}", path.display()))?;
            }
            Effect::Install => {
                let (file, path) = self.file.take().context("no file is open")?;
                let place = self.place(&put.target);
                let checked = file.sync_all().context("writing it out").and_then(|()| {
                    drop(file);
                    check_package(&path, &put.target)
                });
                if let Err(e) = checked {
                    remove_unfinished(&path);
                    return Err(e);
                }
                let installed = fs::rename(&path, &place);
                if let Err(e) = installed {
                    remove_unfinished(&path);
                    return Err(e).with_context(|| format!("installing {}", place.display()));
                }
                println!("Installed {}", put.target);
                return Ok(Some(Message::PutDone { target: put.target.clone() }));
            }
            Effect::Refuse(reason) => return Ok(Some(refused(put, reason))),
        }
        Ok(None)
    }

    fn place(&self, target: &FileTarget) -> PathBuf {
        let FileTarget::Package(name) = target;
        self.folders.packages.join(name)
    }
}

/// A complete upload opens as a package whose id is its file name, before it replaces anything.
fn check_package(path: &Path, target: &FileTarget) -> Result<()> {
    let FileTarget::Package(name) = target;
    let package = Package::open(path)?;
    let expected = check_file_name(name)?;
    anyhow::ensure!(*package.id() == expected, "{name} holds the package {}", package.id());
    Ok(())
}

fn refused(put: &PutFile, reason: String) -> Message {
    eprintln!("Refused {}: {reason}", put.target);
    Message::PutRefused { target: put.target.clone(), reason }
}

fn remove_unfinished(path: &Path) {
    if let Err(e) = fs::remove_file(path) {
        eprintln!("error: removing the unfinished {}: {e}", path.display());
    }
}

fn staging(place: &Path) -> PathBuf {
    let mut name = place.file_name().expect("install places are files").to_os_string();
    name.push(".tmp");
    place.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::Effect::{Append, Create, Discard, Install, Refuse};
    use super::*;

    fn piece(name: &str, offset: u64, len: usize, total: u64) -> PutFile {
        PutFile { target: FileTarget::Package(name.into()), offset, total, data: vec![0; len] }
    }

    fn receiving(name: &str, written: u64, total: u64) -> Phase {
        Phase::Receiving { target: FileTarget::Package(name.into()), written, total }
    }

    fn refusing(reason: &str) -> Phase {
        Phase::Refusing { reason: reason.into() }
    }

    const BAD_NAME: &str = "\"../a.b.deck\" is not <id>.deck: id \"../a.b\" is not lowercase reverse-DNS, like dev.glassdeck.clock";

    #[test]
    fn walks_the_whole_transition_table() {
        let out_of_order = "pieces of package a.b.deck arrived out of order";
        let table = [
            (Phase::Idle, piece("a.b.deck", 0, 4, 10), receiving("a.b.deck", 4, 10), vec![Create, Append]),
            (Phase::Idle, piece("a.b.deck", 0, 10, 10), Phase::Idle, vec![Create, Append, Install]),
            (Phase::Idle, piece("a.b.deck", 0, 0, 0), Phase::Idle, vec![Create, Append, Install]),
            (Phase::Idle, piece("../a.b.deck", 0, 4, 10), refusing(BAD_NAME), vec![]),
            (Phase::Idle, piece("a.b.deckbg", 0, 4, 10), refusing("\"a.b.deckbg\" does not end in .deck"), vec![]),
            (Phase::Idle, piece("a.b.deck", 4, 4, 10), refusing("a piece at byte 4 arrived with no start"), vec![]),
            (
                Phase::Idle,
                piece("a.b.deck", 4, 6, 10),
                Phase::Idle,
                vec![Refuse("a piece at byte 4 arrived with no start".into())],
            ),
            (receiving("a.b.deck", 4, 10), piece("a.b.deck", 4, 4, 10), receiving("a.b.deck", 8, 10), vec![Append]),
            (receiving("a.b.deck", 4, 10), piece("a.b.deck", 4, 6, 10), Phase::Idle, vec![Append, Install]),
            (
                receiving("a.b.deck", 4, 10),
                piece("a.b.deck", 4, 8, 10),
                Phase::Idle,
                vec![Append, Discard, Refuse("got 12 bytes of 10".into())],
            ),
            (receiving("a.b.deck", 4, 10), piece("a.b.deck", 6, 2, 10), refusing(out_of_order), vec![Discard]),
            (receiving("a.b.deck", 4, 10), piece("c.d.deck", 4, 2, 10), refusing(out_of_order), vec![Discard]),
            (receiving("a.b.deck", 4, 10), piece("c.d.deck", 0, 2, 10), receiving("c.d.deck", 2, 10), vec![Discard, Create, Append]),
            (refusing("full"), piece("a.b.deck", 4, 2, 10), refusing("full"), vec![]),
            (refusing("full"), piece("a.b.deck", 4, 6, 10), Phase::Idle, vec![Refuse("full".into())]),
            (refusing("full"), piece("a.b.deck", 0, 2, 10), receiving("a.b.deck", 2, 10), vec![Create, Append]),
        ];
        for (from, put, phase, effects) in table {
            assert_eq!(step(from.clone(), &put), (phase, effects), "from {from:?} on {put:?}");
        }
    }

    #[test]
    fn only_package_ids_with_the_deck_extension() {
        assert!(check_name(&FileTarget::Package("wallpaper-engine.1.deck".into())).is_ok());
        for bad in ["", ".deck", "a/b.c.deck", "a\\b.c.deck", "..", "c:a.b.deck", "a.b.deck.tmp", "a.b.mp4", "nodot.deck"] {
            assert!(check_name(&FileTarget::Package(bad.into())).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_complete_upload_must_open_as_its_package_and_an_abandoned_one_is_removed() {
        let root = std::env::temp_dir().join(format!("glassdeck-put-{}", std::process::id()));
        let folders = Folders { packages: root.join("packages") };
        let mut uploads = Uploads::new(&folders);
        let target = FileTarget::Package("a.b.deck".into());
        let answer = uploads.put(&PutFile { target: target.clone(), offset: 0, total: 4, data: b"junk".to_vec() });
        assert!(matches!(answer, Some(Message::PutRefused { .. })), "{answer:?}");
        assert!(!folders.packages.join("a.b.deck").exists());

        assert_eq!(uploads.put(&PutFile { target, offset: 0, total: 10, data: vec![1; 4] }), None);
        assert!(folders.packages.join("a.b.deck.tmp").exists());
        uploads.abandon();
        assert!(!folders.packages.join("a.b.deck.tmp").exists());
        fs::remove_dir_all(&root).unwrap();
    }
}
