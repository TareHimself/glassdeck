//! Deploying: checking a file before the keyboard installs it, and keeping glassdeckd's own modules
//! and profile the same as the keyboard's, so the host halves match what the keyboard runs.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, bail, ensure};
use glassdeck_luau::profile::{Profile, WidgetArea};
use glassdeck_package::{Background, Content, EXTENSION, MODULE_EXTENSION, ModulePackage, CanvasFiles};
use glassdeck_proto::FileTarget;

use crate::module_host;

/// The Link carries a file name's length in one byte.
const MAX_NAME_BYTES: usize = 255;

/// Where the keyboard puts `path`, once it's known to open as what its extension says.
pub fn target_for(path: &Path, modules: &Path) -> Result<FileTarget> {
    let name = path.file_name().and_then(|n| n.to_str()).with_context(|| format!("{} has no usable file name", path.display()))?;
    ensure!(name.len() <= MAX_NAME_BYTES, "{name} is longer than {MAX_NAME_BYTES} bytes");
    match path.extension().and_then(|e| e.to_str()) {
        Some(EXTENSION) => {
            Background::open(path)?;
            Ok(FileTarget::Background(name.to_owned()))
        }
        Some(MODULE_EXTENSION) => {
            ModulePackage::open(path)?;
            Ok(FileTarget::Module(name.to_owned()))
        }
        Some("json") => {
            let installed = module_host::installed(modules)?;
            Profile::read(path, WidgetArea::of_panel().size, |id| installed.get(id).map(|p| p.manifest().clone()))?;
            Ok(FileTarget::Profile)
        }
        _ => bail!("{} isn't a .{EXTENSION}, .{MODULE_EXTENSION} or profile .json", path.display()),
    }
}

/// What goes to the keyboard for a file: modules go as their keyboard copy, so host files never
/// leave the host. A copy made for the upload is deleted with this.
pub struct KeyboardFile {
    path: PathBuf,
    made_for_upload: bool,
}

impl KeyboardFile {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for KeyboardFile {
    fn drop(&mut self) {
        if self.made_for_upload
            && let Err(e) = fs::remove_file(&self.path)
        {
            eprintln!("error: removing {}: {e}", self.path.display());
        }
    }
}

pub fn keyboard_file(target: &FileTarget, path: &Path, staging: &Path) -> Result<KeyboardFile> {
    let FileTarget::Module(name) = target else {
        return Ok(KeyboardFile { path: path.to_owned(), made_for_upload: false });
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    fs::create_dir_all(staging).with_context(|| format!("creating {}", staging.display()))?;
    let copy = staging.join(format!("{}-{name}", NEXT.fetch_add(1, Ordering::Relaxed)));
    ModulePackage::open(path)?.write_keyboard_copy(&copy).context("making the keyboard's copy")?;
    Ok(KeyboardFile { path: copy, made_for_upload: true })
}

/// Copies a deployed module, profile or canvas (one with options or a host half) into glassdeckd's own
/// folder; the module host runs host sides from there.
pub fn keep_locally(target: &FileTarget, from: &Path, modules: &Path, profile: &Path, canvases: &Path) -> Result<()> {
    let place = match target {
        FileTarget::Background(name) => match runs_on_host(from)? {
            true => canvases.join(name),
            false => return forget_local_copy(from, &canvases.join(name)),
        },
        FileTarget::Module(name) => modules.join(name),
        FileTarget::Profile => profile.to_owned(),
    };
    if same_file(from, &place) {
        return Ok(());
    }
    if let Some(folder) = place.parent() {
        fs::create_dir_all(folder).with_context(|| format!("creating {}", folder.display()))?;
    }
    let staging = staging(&place);
    fs::copy(from, &staging).with_context(|| format!("copying {} to {}", from.display(), staging.display()))?;
    fs::rename(&staging, &place).with_context(|| format!("moving {} into place", place.display()))
}

/// A redeployed background that no longer runs on the host drops the copy an earlier version left,
/// so its old host side stops with it.
fn forget_local_copy(from: &Path, copy: &Path) -> Result<()> {
    if same_file(from, copy) {
        return Ok(());
    }
    match fs::remove_file(copy) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e).with_context(|| format!("removing {}", copy.display())),
        _ => Ok(()),
    }
}

/// A canvas with options or a host half, which glassdeckd runs part of.
fn runs_on_host(path: &Path) -> Result<bool> {
    let background = Background::open(path)?;
    if !matches!(background.manifest().content, Content::Canvas { .. }) {
        return Ok(false);
    }
    let plan = CanvasFiles::read(&background)?.plan;
    Ok(plan.options.is_some() || plan.host.is_some())
}

fn same_file(a: &Path, b: &Path) -> bool {
    matches!((fs::canonicalize(a), fs::canonicalize(b)), (Ok(a), Ok(b)) if a == b)
}

fn staging(place: &Path) -> PathBuf {
    let mut name = place.file_name().expect("install places are files").to_os_string();
    name.push(".tmp");
    place.with_file_name(name)
}
