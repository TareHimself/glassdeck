//! Deploying: checking a file before the keyboard installs it, and keeping glassdeckd's own modules
//! and profile the same as the keyboard's, so the host halves match what the keyboard runs.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use glassdeck_luau::profile::{Profile, WidgetArea};
use glassdeck_package::{Background, EXTENSION, MODULE_EXTENSION, ModulePackage};
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

/// Copies a deployed module or profile into glassdeckd's own folder; the module host reloads from there.
pub fn keep_locally(target: &FileTarget, from: &Path, modules: &Path, profile: &Path) -> Result<()> {
    let place = match target {
        FileTarget::Background(_) => return Ok(()),
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

fn same_file(a: &Path, b: &Path) -> bool {
    matches!((fs::canonicalize(a), fs::canonicalize(b)), (Ok(a), Ok(b)) if a == b)
}

fn staging(place: &Path) -> PathBuf {
    let mut name = place.file_name().expect("install places are files").to_os_string();
    name.push(".tmp");
    place.with_file_name(name)
}
