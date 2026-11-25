//! A `.deckmod`: a widget module in the same stored-zip container as a background, with `module.toml` as
//! its manifest. Modules and backgrounds refuse each other's files.

use std::collections::HashMap;
use std::fs::{self, File};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;

use crate::zip::{self, Entry};
use crate::{MANIFEST, read_exact_at, staging_path};

pub const MODULE_EXTENSION: &str = "deckmod";
pub const MODULE_MANIFEST: &str = "module.toml";
pub const KEYBOARD_SCRIPT: &str = "keyboard.luau";
pub const PC_SCRIPT: &str = "pc.luau";
pub const ASSETS_FOLDER: &str = "assets";
const MAX_SCRIPT_LEN: u64 = 16 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Size {
    pub w: u32,
    pub h: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resize {
    Fixed,
    Between { min: Size, max: Size },
}

impl Resize {
    pub fn allows(self, default: Size, size: Size) -> bool {
        match self {
            Resize::Fixed => size == default,
            Resize::Between { min, max } => (min.w..=max.w).contains(&size.w) && (min.h..=max.h).contains(&size.h),
        }
    }
}

/// A checked `module.toml`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModuleManifest {
    pub id: String,
    pub name: String,
    pub size: Size,
    pub resize: Resize,
    pub pc: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawManifest {
    id: String,
    name: String,
    size: Size,
    min: Option<Size>,
    max: Option<Size>,
    #[serde(default)]
    pc: bool,
}

impl ModuleManifest {
    pub fn parse(text: &str) -> Result<Self> {
        let raw: RawManifest = toml::from_str(text).context("parsing module.toml")?;
        ensure!(raw.id.contains('.') && !raw.id.contains(char::is_whitespace), "id {:?} is not reverse-DNS, like dev.glassdeck.clock", raw.id);
        ensure!(raw.size.w > 0 && raw.size.h > 0, "size must not be zero");
        let resize = match (raw.min, raw.max) {
            (None, None) => Resize::Fixed,
            (Some(min), Some(max)) => Resize::Between { min, max },
            _ => bail!("give both min and max, or neither for a fixed size"),
        };
        ensure!(resize.allows(raw.size, raw.size), "size {}x{} is outside min and max", raw.size.w, raw.size.h);
        Ok(Self { id: raw.id, name: raw.name, size: raw.size, resize, pc: raw.pc })
    }
}

/// An opened module whose manifest parsed and whose scripts are present.
pub struct ModulePackage {
    path: PathBuf,
    file: File,
    manifest: ModuleManifest,
    entries: HashMap<String, Entry>,
}

impl ModulePackage {
    pub fn open(path: &Path) -> Result<Self> {
        let mut file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
        let entries = zip::read_directory(&mut file).with_context(|| format!("reading {}", path.display()))?;
        ensure!(!entries.contains_key(MANIFEST), "{} is a background, not a module", path.display());
        let manifest = entries.get(MODULE_MANIFEST).copied().with_context(|| format!("{} has no {MODULE_MANIFEST}", path.display()))?;
        let text = read_text(&file, manifest).with_context(|| format!("reading {}", path.display()))?;
        let manifest = ModuleManifest::parse(&text).with_context(|| format!("checking {}", path.display()))?;
        check_scripts(&manifest, |name| entries.contains_key(name)).with_context(|| format!("checking {}", path.display()))?;
        Ok(Self { path: path.to_path_buf(), file, manifest, entries })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn manifest(&self) -> &ModuleManifest {
        &self.manifest
    }

    pub fn keyboard_script(&self) -> Result<String> {
        self.script(KEYBOARD_SCRIPT)
    }

    /// `None` when the module has no PC half.
    pub fn pc_script(&self) -> Result<Option<String>> {
        self.manifest.pc.then(|| self.script(PC_SCRIPT)).transpose()
    }

    /// A file from `assets/`, named without the folder.
    pub fn asset(&self, name: &str) -> Result<Vec<u8>> {
        let full = format!("{ASSETS_FOLDER}/{name}");
        let entry = self.entries.get(&full).copied().with_context(|| format!("{} has no {full}", self.path.display()))?;
        let mut data = vec![0; usize::try_from(entry.len)?];
        read_exact_at(&self.file, entry.offset, &mut data).with_context(|| format!("reading {full}"))?;
        Ok(data)
    }

    fn script(&self, name: &str) -> Result<String> {
        let entry = self.entries.get(name).copied().with_context(|| format!("{} has no {name}", self.path.display()))?;
        read_text(&self.file, entry).with_context(|| format!("reading {name} in {}", self.path.display()))
    }
}

fn read_text(file: &File, entry: Entry) -> Result<String> {
    ensure!(entry.len <= MAX_SCRIPT_LEN, "the file is too large");
    let mut data = vec![0; entry.len as usize];
    read_exact_at(file, entry.offset, &mut data)?;
    String::from_utf8(data).context("the file is not UTF-8")
}

fn check_scripts(manifest: &ModuleManifest, has: impl Fn(&str) -> bool) -> Result<()> {
    ensure!(has(KEYBOARD_SCRIPT), "{KEYBOARD_SCRIPT} is missing");
    match (manifest.pc, has(PC_SCRIPT)) {
        (true, false) => bail!("module.toml says pc = true but {PC_SCRIPT} is missing"),
        (false, true) => bail!("{PC_SCRIPT} is present but module.toml doesn't say pc = true"),
        _ => Ok(()),
    }
}

pub fn is_module(path: &Path) -> bool {
    path.is_file() && path.extension().is_some_and(|e| e.eq_ignore_ascii_case(MODULE_EXTENSION))
}

/// Builds `<path>.tmp` with `module.toml` first, then renames it over `path`.
pub struct ModuleWriter {
    path: PathBuf,
    staging: PathBuf,
    manifest: ModuleManifest,
    zip: zip::ZipWriter,
}

impl ModuleWriter {
    pub fn create(path: &Path, manifest_text: &str) -> Result<Self> {
        let manifest = ModuleManifest::parse(manifest_text)?;
        let staging = staging_path(path)?;
        let file = File::create(&staging).with_context(|| format!("creating {}", staging.display()))?;
        let mut zip = zip::ZipWriter::new(file);
        zip.add(MODULE_MANIFEST, &mut manifest_text.as_bytes())?;
        Ok(Self { path: path.to_path_buf(), staging, manifest, zip })
    }

    pub fn add_file(&mut self, name: &str, source: &Path) -> Result<()> {
        ensure!(name != MANIFEST, "a module can't contain {MANIFEST}, which marks a background");
        let mut file = File::open(source).with_context(|| format!("opening {}", source.display()))?;
        self.zip.add(name, &mut file).with_context(|| format!("adding {}", source.display()))
    }

    pub fn finish(self) -> Result<ModulePackage> {
        let names: Vec<&str> = self.zip.names().collect();
        check_scripts(&self.manifest, |name| names.contains(&name))?;
        drop(self.zip.finish()?);
        fs::rename(&self.staging, &self.path).with_context(|| format!("moving {} into place", self.path.display()))?;
        ModulePackage::open(&self.path)
    }
}

/// Packs a module folder: `module.toml`, the scripts and everything under `assets/`.
pub fn pack_folder(folder: &Path, out: &Path) -> Result<ModulePackage> {
    let manifest_path = folder.join(MODULE_MANIFEST);
    let manifest = fs::read_to_string(&manifest_path).with_context(|| format!("reading {}", manifest_path.display()))?;
    let mut writer = ModuleWriter::create(out, &manifest)?;
    for script in [KEYBOARD_SCRIPT, PC_SCRIPT] {
        let path = folder.join(script);
        if path.exists() {
            writer.add_file(script, &path)?;
        }
    }
    let assets = folder.join(ASSETS_FOLDER);
    if assets.is_dir() {
        for entry in fs::read_dir(&assets).with_context(|| format!("reading {}", assets.display()))? {
            let entry = entry?;
            let name = entry.file_name().into_string().map_err(|n| anyhow::anyhow!("asset name {n:?} is not UTF-8"))?;
            ensure!(entry.file_type()?.is_file(), "assets/{name} is a folder; assets/ holds files only");
            writer.add_file(&format!("{ASSETS_FOLDER}/{name}"), &entry.path())?;
        }
    }
    writer.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{Scratch, web_manifest};

    const CLOCK: &str = r#"
id = "dev.glassdeck.clock"
name = "Clock"
size = { w = 240, h = 120 }
min = { w = 160, h = 80 }
max = { w = 480, h = 240 }
"#;

    #[test]
    fn parses_a_resizable_manifest() {
        let manifest = ModuleManifest::parse(CLOCK).unwrap();
        assert_eq!(manifest.resize, Resize::Between { min: Size { w: 160, h: 80 }, max: Size { w: 480, h: 240 } });
        assert!(!manifest.pc);
        assert!(manifest.resize.allows(manifest.size, Size { w: 200, h: 200 }));
        assert!(!manifest.resize.allows(manifest.size, Size { w: 100, h: 200 }));
    }

    #[test]
    fn a_fixed_module_allows_only_its_size() {
        let manifest = ModuleManifest::parse("id = \"a.b\"\nname = \"A\"\nsize = { w = 8, h = 8 }").unwrap();
        assert_eq!(manifest.resize, Resize::Fixed);
        assert!(manifest.resize.allows(manifest.size, Size { w: 8, h: 8 }));
        assert!(!manifest.resize.allows(manifest.size, Size { w: 16, h: 8 }));
    }

    #[test]
    fn refuses_bad_manifests() {
        for bad in [
            "id = \"clock\"\nname = \"A\"\nsize = { w = 8, h = 8 }",
            "id = \"a.b\"\nname = \"A\"\nsize = { w = 8, h = 8 }\nmin = { w = 8, h = 8 }",
            "id = \"a.b\"\nname = \"A\"\nsize = { w = 800, h = 8 }\nmin = { w = 8, h = 8 }\nmax = { w = 16, h = 16 }",
            "id = \"a.b\"\nname = \"A\"\nsize = { w = 8, h = 8 }\ncolour = 1",
        ] {
            assert!(ModuleManifest::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn packs_and_opens_a_folder() {
        let scratch = Scratch::new("module-pack");
        let folder = scratch.0.join("clock");
        fs::create_dir_all(folder.join(ASSETS_FOLDER)).unwrap();
        fs::write(folder.join(MODULE_MANIFEST), CLOCK).unwrap();
        fs::write(folder.join(KEYBOARD_SCRIPT), "return {}").unwrap();
        fs::write(folder.join(ASSETS_FOLDER).join("face.svg"), "<svg/>").unwrap();
        let package = pack_folder(&folder, &scratch.0.join("clock.deckmod")).unwrap();
        assert_eq!(package.manifest().id, "dev.glassdeck.clock");
        assert_eq!(package.keyboard_script().unwrap(), "return {}");
        assert_eq!(package.pc_script().unwrap(), None);
        assert_eq!(package.asset("face.svg").unwrap(), b"<svg/>");
    }

    #[test]
    fn a_pc_module_needs_its_pc_script() {
        let scratch = Scratch::new("module-pc");
        let folder = scratch.0.join("cpu");
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join(MODULE_MANIFEST), format!("{CLOCK}pc = true\n")).unwrap();
        fs::write(folder.join(KEYBOARD_SCRIPT), "return {}").unwrap();
        assert!(pack_folder(&folder, &scratch.0.join("cpu.deckmod")).is_err());
    }

    #[test]
    fn backgrounds_and_modules_refuse_each_other() {
        let scratch = Scratch::new("module-vs-background");
        let background = scratch.0.join("web.deckbg");
        let mut w = crate::BackgroundWriter::create(&background, web_manifest()).unwrap();
        w.add_bytes("web/index.html", b"<html></html>").unwrap();
        w.finish().unwrap();
        assert!(format!("{:#}", ModulePackage::open(&background).err().unwrap()).contains("is a background"));

        let folder = scratch.0.join("clock");
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join(MODULE_MANIFEST), CLOCK).unwrap();
        fs::write(folder.join(KEYBOARD_SCRIPT), "return {}").unwrap();
        let module = pack_folder(&folder, &scratch.0.join("clock.deckmod")).unwrap();
        assert!(format!("{:#}", crate::Background::open(module.path()).err().unwrap()).contains("is a module"));
    }
}
