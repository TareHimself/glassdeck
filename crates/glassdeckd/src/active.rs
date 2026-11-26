//! glassdeckd's copies of the active theme's packages, and nothing else. The app keeps the library;
//! these copies let glassdeckd run host sides and restore at login without it. Each package's
//! keyboard copy is kept beside it, with its digest, so the keyboard is sent only what it lacks.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result};
use glassdeck_package::{Package, PackageId, file_digest};
use serde::{Deserialize, Serialize};

use crate::error::bad_request;

const RECORD: &str = "active.json";
const KEYBOARD_COPIES: &str = "keyboard";

/// Where a copy came from, so pushing the same file again doesn't copy it again.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Source {
    path: PathBuf,
    len: u64,
    modified_ms: u128,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Held {
    source: Source,
    /// Of the keyboard copy, as the keyboard reports it
    keyboard_digest: u64,
}

pub struct ActivePackages {
    folder: PathBuf,
    held: BTreeMap<String, Held>,
}

impl ActivePackages {
    pub fn open(folder: PathBuf) -> Result<Self> {
        fs::create_dir_all(folder.join(KEYBOARD_COPIES)).with_context(|| format!("creating {}", folder.display()))?;
        let record = folder.join(RECORD);
        let held = match fs::read_to_string(&record) {
            Ok(text) => serde_json::from_str(&text).with_context(|| format!("parsing {}", record.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", record.display())),
        };
        Ok(Self { folder, held })
    }

    pub fn folder(&self) -> &Path {
        &self.folder
    }

    /// The keyboard copy to upload for `id`, and its digest.
    pub fn keyboard_copy(&self, id: &PackageId) -> Option<(PathBuf, u64)> {
        let held = self.held.get(id.as_str())?;
        Some((self.folder.join(KEYBOARD_COPIES).join(id.file_name()), held.keyboard_digest))
    }

    /// Copies `source` in as `id`, with its keyboard copy, unless the same file is already held.
    pub fn take_in(&mut self, id: &str, source: &Path) -> Result<()> {
        let package = Package::open(source).map_err(|e| bad_request(format!("{e:#}")))?;
        if package.id().as_str() != id {
            return Err(bad_request(format!("{} holds {}, not {id}", source.display(), package.id())));
        }
        let stamp = source_of(source)?;
        if self.held.get(id).is_some_and(|held| held.source == stamp) && self.folder.join(package.id().file_name()).is_file() {
            return Ok(());
        }
        let full = self.folder.join(package.id().file_name());
        copy_into_place(source, &full)?;
        let copy = self.folder.join(KEYBOARD_COPIES).join(package.id().file_name());
        Package::open(&full)?.write_keyboard_copy(&copy).context("making the keyboard's copy")?;
        let keyboard_digest = file_digest(&copy)?;
        self.held.insert(id.to_owned(), Held { source: stamp, keyboard_digest });
        println!("Active: took in {id} from {}", source.display());
        self.save()
    }

    /// Drops every copy the active theme doesn't use.
    pub fn keep_only(&mut self, used: &BTreeSet<String>) -> Result<()> {
        let unused: Vec<String> = self.held.keys().filter(|id| !used.contains(*id)).cloned().collect();
        for id in unused {
            let name = format!("{id}.{}", glassdeck_package::EXTENSION);
            for path in [self.folder.join(&name), self.folder.join(KEYBOARD_COPIES).join(&name)] {
                match fs::remove_file(&path) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e).with_context(|| format!("removing {}", path.display())),
                }
            }
            self.held.remove(&id);
            println!("Active: dropped {id}");
        }
        self.save()
    }

    fn save(&self) -> Result<()> {
        let record = self.folder.join(RECORD);
        let staging = record.with_extension("json.tmp");
        fs::write(&staging, serde_json::to_vec_pretty(&self.held)?).with_context(|| format!("writing {}", staging.display()))?;
        fs::rename(&staging, &record).with_context(|| format!("moving {} into place", record.display()))
    }
}

fn source_of(path: &Path) -> Result<Source> {
    let meta = fs::metadata(path).with_context(|| format!("reading {}", path.display()))?;
    let modified_ms = meta.modified()?.duration_since(UNIX_EPOCH).unwrap_or_default().as_millis();
    let path = fs::canonicalize(path).with_context(|| format!("finding {}", path.display()))?;
    Ok(Source { path, len: meta.len(), modified_ms })
}

fn copy_into_place(from: &Path, to: &Path) -> Result<()> {
    if fs::canonicalize(from).ok() == fs::canonicalize(to).ok() && to.exists() {
        return Ok(());
    }
    let staging = to.with_extension("deck.tmp");
    fs::copy(from, &staging).with_context(|| format!("copying {} to {}", from.display(), staging.display()))?;
    fs::rename(&staging, to).with_context(|| format!("moving {} into place", to.display()))
}
