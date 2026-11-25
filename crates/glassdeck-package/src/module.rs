//! A `.deckmod`: a widget module in the same stored-zip container as a background, with `module.json` as
//! its manifest. Modules and backgrounds refuse each other's files.

use std::collections::HashMap;
use std::fs::{self, File};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;

use crate::zip::{self, Entry};
use crate::resource::{Resource, ResourceKind, ResourceName, literal_resources};
use crate::{MANIFEST, read_exact_at, staging_path};

pub const MODULE_EXTENSION: &str = "deckmod";
pub const MODULE_MANIFEST: &str = "module.json";
pub const KEYBOARD_SCRIPT: &str = "keyboard.luau";
pub const HOST_SCRIPT: &str = "host.luau";
pub const ASSETS_FOLDER: &str = "assets";
/// The module layout this build reads: module.json and the package folders (format 3 split assets/ into keyboard/ and host/).
pub const MODULE_FORMAT_VERSION: u32 = 3;
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

/// A checked `module.json`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModuleManifest {
    pub id: String,
    pub name: String,
    /// The module's own version, `major.minor.patch`.
    pub version: String,
    pub size: Size,
    pub resize: Resize,
    pub host: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawManifest {
    /// Lets editors find the schema; not used otherwise.
    #[serde(rename = "$schema")]
    _schema: Option<String>,
    format: u32,
    id: String,
    name: String,
    version: String,
    size: Size,
    min: Option<Size>,
    max: Option<Size>,
    #[serde(default)]
    host: bool,
}

impl ModuleManifest {
    pub fn parse(text: &str) -> Result<Self> {
        let raw: RawManifest = serde_json::from_str(text).context("parsing module.json")?;
        ensure!(raw.format == MODULE_FORMAT_VERSION, "module.json format {} is not the {MODULE_FORMAT_VERSION} this build reads", raw.format);
        ensure!(is_version(&raw.version), "version {:?} is not major.minor.patch, like 1.0.0", raw.version);
        ensure!(raw.id.contains('.') && !raw.id.contains(char::is_whitespace), "id {:?} is not reverse-DNS, like dev.glassdeck.clock", raw.id);
        ensure!(raw.size.w > 0 && raw.size.h > 0, "size must not be zero");
        let resize = match (raw.min, raw.max) {
            (None, None) => Resize::Fixed,
            (Some(min), Some(max)) => Resize::Between { min, max },
            _ => bail!("give both min and max, or neither for a fixed size"),
        };
        ensure!(resize.allows(raw.size, raw.size), "size {}x{} is outside min and max", raw.size.w, raw.size.h);
        Ok(Self { id: raw.id, name: raw.name, version: raw.version, size: raw.size, resize, host: raw.host })
    }
}

fn is_version(text: &str) -> bool {
    let parts: Vec<&str> = text.split('.').collect();
    parts.len() == 3 && parts.iter().all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

/// What every opened module has: its container, parsed manifest and entries.
struct Contents {
    path: PathBuf,
    file: File,
    manifest: ModuleManifest,
    entries: HashMap<String, Entry>,
}

impl Contents {
    fn open(path: &Path) -> Result<Self> {
        let mut file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
        let entries = zip::read_directory(&mut file).with_context(|| format!("reading {}", path.display()))?;
        ensure!(!entries.contains_key(MANIFEST), "{} is a background, not a module", path.display());
        let manifest = entries.get(MODULE_MANIFEST).copied().with_context(|| format!("{} has no {MODULE_MANIFEST}", path.display()))?;
        let text = read_text(&file, manifest).with_context(|| format!("reading {}", path.display()))?;
        let manifest = ModuleManifest::parse(&text).with_context(|| format!("checking {}", path.display()))?;
        Ok(Self { path: path.to_path_buf(), file, manifest, entries })
    }

    fn bytes(&self, name: &str) -> Result<Vec<u8>> {
        let entry = self.entries.get(name).copied().with_context(|| format!("{} has no {name}", self.path.display()))?;
        let mut data = vec![0; usize::try_from(entry.len)?];
        read_exact_at(&self.file, entry.offset, &mut data).with_context(|| format!("reading {name} in {}", self.path.display()))?;
        Ok(data)
    }

    fn script(&self, name: &str) -> Result<String> {
        let entry = self.entries.get(name).copied().with_context(|| format!("{} has no {name}", self.path.display()))?;
        read_text(&self.file, entry).with_context(|| format!("reading {name} in {}", self.path.display()))
    }

    fn resource(&self, resource: &Resource) -> Result<Vec<u8>> {
        let full = resource.entry();
        ensure!(self.entries.contains_key(&full), "{} has no {full}, which {resource} names", self.path.display());
        self.bytes(&full)
    }
}

/// A whole module, as packed and as the host keeps it: its manifest parsed and its scripts present.
pub struct ModulePackage(Contents);

impl ModulePackage {
    pub fn open(path: &Path) -> Result<Self> {
        let contents = Contents::open(path)?;
        check_scripts(&contents.manifest, |name| contents.entries.contains_key(name)).with_context(|| format!("checking {}", path.display()))?;
        Ok(Self(contents))
    }

    pub fn path(&self) -> &Path {
        &self.0.path
    }

    pub fn manifest(&self) -> &ModuleManifest {
        &self.0.manifest
    }

    pub fn keyboard_script(&self) -> Result<String> {
        self.0.script(KEYBOARD_SCRIPT)
    }

    /// `None` when the module has no host half.
    pub fn host_script(&self) -> Result<Option<String>> {
        self.0.manifest.host.then(|| self.0.script(HOST_SCRIPT)).transpose()
    }

    pub fn resource(&self, resource: &Resource) -> Result<Vec<u8>> {
        self.0.resource(resource)
    }

    /// Writes what the keyboard needs, and nothing the host keeps to itself: `module.json`,
    /// `keyboard.luau` and `assets/keyboard/`.
    pub fn write_keyboard_copy(&self, out: &Path) -> Result<KeyboardModule> {
        let staging = staging_path(out)?;
        let file = File::create(&staging).with_context(|| format!("creating {}", staging.display()))?;
        let mut zip = zip::ZipWriter::new(file);
        let keyboard_assets = format!("{ASSETS_FOLDER}/{}/", ResourceKind::Keyboard.folder());
        let mut names: Vec<&String> = self.0.entries.keys().filter(|name| name.starts_with(&keyboard_assets)).collect();
        names.sort();
        for name in [MODULE_MANIFEST, KEYBOARD_SCRIPT].iter().copied().chain(names.into_iter().map(String::as_str)) {
            zip.add(name, &mut self.0.bytes(name)?.as_slice()).with_context(|| format!("copying {name}"))?;
        }
        drop(zip.finish()?);
        fs::rename(&staging, out).with_context(|| format!("moving {} into place", out.display()))?;
        KeyboardModule::open(out)
    }
}

/// A module as the keyboard uses it: `keyboard.luau` and keyboard resources. The host half and host
/// resources stay on the host, so this opens without them even when the manifest names a host half.
pub struct KeyboardModule(Contents);

impl KeyboardModule {
    pub fn open(path: &Path) -> Result<Self> {
        let contents = Contents::open(path)?;
        ensure!(contents.entries.contains_key(KEYBOARD_SCRIPT), "{} has no {KEYBOARD_SCRIPT}", path.display());
        Ok(Self(contents))
    }

    pub fn path(&self) -> &Path {
        &self.0.path
    }

    pub fn manifest(&self) -> &ModuleManifest {
        &self.0.manifest
    }

    pub fn keyboard_script(&self) -> Result<String> {
        self.0.script(KEYBOARD_SCRIPT)
    }

    pub fn resource(&self, resource: &Resource) -> Result<Vec<u8>> {
        ensure!(resource.kind == ResourceKind::Keyboard, "{resource} stays on the host");
        self.0.resource(resource)
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
    match (manifest.host, has(HOST_SCRIPT)) {
        (true, false) => bail!("module.json says \"host\": true but {HOST_SCRIPT} is missing"),
        (false, true) => bail!("{HOST_SCRIPT} is present but module.json doesn't say \"host\": true"),
        _ => Ok(()),
    }
}

pub fn is_module(path: &Path) -> bool {
    path.is_file() && path.extension().is_some_and(|e| e.eq_ignore_ascii_case(MODULE_EXTENSION))
}

/// Builds `<path>.tmp` with `module.json` first, then renames it over `path`.
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

/// Packs a module folder: `module.json`, the scripts, and `assets/keyboard/` and `assets/host/`.
/// Every resource a script names as a literal must be in the folder, and on a side that script can use.
pub fn pack_folder(folder: &Path, out: &Path) -> Result<ModulePackage> {
    let manifest_path = folder.join(MODULE_MANIFEST);
    let manifest = fs::read_to_string(&manifest_path).with_context(|| format!("reading {}", manifest_path.display()))?;
    let resources = resource_files(&folder.join(ASSETS_FOLDER))?;
    let mut writer = ModuleWriter::create(out, &manifest)?;
    for script in [KEYBOARD_SCRIPT, HOST_SCRIPT] {
        let path = folder.join(script);
        if path.exists() {
            let source = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
            check_literal_resources(script, &source, &resources)?;
            writer.add_file(script, &path)?;
        }
    }
    for (resource, path) in &resources {
        writer.add_file(&resource.entry(), path)?;
    }
    writer.finish()
}

/// Every file under `assets/keyboard/` and `assets/host/`, each with a name a script can use.
fn resource_files(assets: &Path) -> Result<Vec<(Resource, PathBuf)>> {
    let mut found = Vec::new();
    if !assets.is_dir() {
        return Ok(found);
    }
    for entry in fs::read_dir(assets).with_context(|| format!("reading {}", assets.display()))? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let kind = ResourceKind::ALL.into_iter().find(|k| k.folder() == name && entry.path().is_dir());
        let kind = kind.with_context(|| format!("{ASSETS_FOLDER}/{name}: {ASSETS_FOLDER}/ holds only the keyboard/ and host/ folders"))?;
        collect_files(&entry.path(), "", kind, &mut found)?;
    }
    found.sort_by_key(|(resource, _)| resource.entry());
    Ok(found)
}

fn collect_files(folder: &Path, prefix: &str, kind: ResourceKind, found: &mut Vec<(Resource, PathBuf)>) -> Result<()> {
    for entry in fs::read_dir(folder).with_context(|| format!("reading {}", folder.display()))? {
        let entry = entry?;
        let name = format!("{prefix}{}", entry.file_name().to_string_lossy());
        if entry.file_type()?.is_dir() {
            collect_files(&entry.path(), &format!("{name}/"), kind, found)?;
        } else {
            let name = ResourceName::parse(&name).with_context(|| format!("{ASSETS_FOLDER}/{}/{name}", kind.folder()))?;
            found.push((Resource { kind, name }, entry.path()));
        }
    }
    Ok(())
}

fn check_literal_resources(script: &str, source: &str, resources: &[(Resource, PathBuf)]) -> Result<()> {
    for (kind, name) in literal_resources(source) {
        let resource = Resource { kind, name: ResourceName::parse(&name).with_context(|| format!("{script} names {name:?}"))? };
        ensure!(
            !(script == KEYBOARD_SCRIPT && kind == ResourceKind::Host),
            "{script} names {resource}, but host resources never reach the keyboard"
        );
        ensure!(resources.iter().any(|(r, _)| *r == resource), "{script} names {resource}, but {} isn't in the folder", resource.entry());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{Scratch, web_manifest};

    const CLOCK: &str = r#"{
        "format": 3,
        "id": "dev.glassdeck.clock",
        "name": "Clock",
        "version": "1.0.0",
        "size": { "w": 240, "h": 120 },
        "min": { "w": 160, "h": 80 },
        "max": { "w": 480, "h": 240 }
    }"#;

    fn with_host(manifest: &str) -> String {
        manifest.replacen('{', "{ \"host\": true,", 1)
    }

    #[test]
    fn parses_a_resizable_manifest() {
        let manifest = ModuleManifest::parse(CLOCK).unwrap();
        assert_eq!(manifest.resize, Resize::Between { min: Size { w: 160, h: 80 }, max: Size { w: 480, h: 240 } });
        assert!(!manifest.host);
        assert!(manifest.resize.allows(manifest.size, Size { w: 200, h: 200 }));
        assert!(!manifest.resize.allows(manifest.size, Size { w: 100, h: 200 }));
    }

    #[test]
    fn a_fixed_module_allows_only_its_size() {
        let manifest = ModuleManifest::parse(r#"{ "format": 3, "id": "a.b", "name": "A", "version": "1.0.0", "size": { "w": 8, "h": 8 } }"#).unwrap();
        assert_eq!(manifest.resize, Resize::Fixed);
        assert!(manifest.resize.allows(manifest.size, Size { w: 8, h: 8 }));
        assert!(!manifest.resize.allows(manifest.size, Size { w: 16, h: 8 }));
    }

    #[test]
    fn refuses_bad_manifests() {
        for bad in [
            r#"{ "format": 3, "id": "clock", "name": "A", "version": "1.0.0", "size": { "w": 8, "h": 8 } }"#,
            r#"{ "format": 3, "id": "a.b", "name": "A", "version": "1.0.0", "size": { "w": 8, "h": 8 }, "min": { "w": 8, "h": 8 } }"#,
            r#"{ "format": 3, "id": "a.b", "name": "A", "version": "1.0.0", "size": { "w": 800, "h": 8 }, "min": { "w": 8, "h": 8 }, "max": { "w": 16, "h": 16 } }"#,
            r#"{ "format": 3, "id": "a.b", "name": "A", "version": "1.0.0", "size": { "w": 8, "h": 8 }, "colour": 1 }"#,
            r#"{ "format": 1, "id": "a.b", "name": "A", "version": "1.0.0", "size": { "w": 8, "h": 8 } }"#,
            r#"{ "format": 3, "id": "a.b", "name": "A", "version": "1.0", "size": { "w": 8, "h": 8 } }"#,
            r#"{ "id": "a.b", "name": "A", "version": "1.0.0", "size": { "w": 8, "h": 8 } }"#,
        ] {
            assert!(ModuleManifest::parse(bad).is_err(), "{bad}");
        }
    }

    fn resource(kind: ResourceKind, name: &str) -> Resource {
        Resource { kind, name: ResourceName::parse(name).unwrap() }
    }

    /// A clock folder with `keyboard.luau` and the given resource files, as (path under assets/, contents).
    fn clock_folder(scratch: &Scratch, script: &str, files: &[(&str, &str)]) -> PathBuf {
        let folder = scratch.0.join("clock");
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join(MODULE_MANIFEST), CLOCK).unwrap();
        fs::write(folder.join(KEYBOARD_SCRIPT), script).unwrap();
        for (path, contents) in files {
            let path = folder.join(ASSETS_FOLDER).join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }
        folder
    }

    #[test]
    fn packs_and_opens_a_folder() {
        let scratch = Scratch::new("module-pack");
        let script = r#"local face = keyboard.resource("dial/face.svg") return {}"#;
        let folder = clock_folder(&scratch, script, &[("keyboard/dial/face.svg", "<svg/>"), ("host/zones.json", "{}")]);
        let package = pack_folder(&folder, &scratch.0.join("clock.deckmod")).unwrap();
        assert_eq!(package.manifest().id, "dev.glassdeck.clock");
        assert_eq!(package.keyboard_script().unwrap(), script);
        assert_eq!(package.host_script().unwrap(), None);
        assert_eq!(package.resource(&resource(ResourceKind::Keyboard, "dial/face.svg")).unwrap(), b"<svg/>");
        assert_eq!(package.resource(&resource(ResourceKind::Host, "zones.json")).unwrap(), b"{}");
        assert!(package.resource(&resource(ResourceKind::Host, "dial/face.svg")).is_err());
    }

    #[test]
    fn packing_refuses_misplaced_or_missing_resources() {
        let refused = |name: &str, script: &str, files: &[(&str, &str)]| {
            let scratch = Scratch::new(name);
            let folder = clock_folder(&scratch, script, files);
            format!("{:#}", pack_folder(&folder, &scratch.0.join("clock.deckmod")).err().expect("refused"))
        };
        assert!(refused("module-flat-asset", "return {}", &[("face.svg", "<svg/>")]).contains("holds only the keyboard/ and host/"));
        assert!(refused("module-bad-name", "return {}", &[("keyboard/Face.svg", "<svg/>")]).contains("isn't a resource name"));
        assert!(refused("module-missing", r#"keyboard.resource("face.svg")"#, &[]).contains("isn't in the folder"));
        let host_on_keyboard = refused("module-host-on-kb", r#"host.resource("zones.json")"#, &[("host/zones.json", "{}")]);
        assert!(host_on_keyboard.contains("never reach the keyboard"));
    }

    #[test]
    fn the_keyboard_copy_leaves_host_files_behind() {
        let scratch = Scratch::new("module-keyboard-copy");
        let folder = clock_folder(&scratch, "return {}", &[("keyboard/face.svg", "<svg/>"), ("host/zones.json", "{}")]);
        fs::write(folder.join(MODULE_MANIFEST), with_host(CLOCK)).unwrap();
        fs::write(folder.join(HOST_SCRIPT), "every(1, function() end)").unwrap();
        let package = pack_folder(&folder, &scratch.0.join("clock.deckmod")).unwrap();
        let copy = package.write_keyboard_copy(&scratch.0.join("clock-keyboard.deckmod")).unwrap();

        assert!(copy.manifest().host);
        assert_eq!(copy.keyboard_script().unwrap(), "return {}");
        assert_eq!(copy.resource(&resource(ResourceKind::Keyboard, "face.svg")).unwrap(), b"<svg/>");
        assert!(format!("{:#}", copy.resource(&resource(ResourceKind::Host, "zones.json")).unwrap_err()).contains("stays on the host"));
        let entries = zip::read_directory(&mut File::open(copy.path()).unwrap()).unwrap();
        let mut names: Vec<&str> = entries.keys().map(String::as_str).collect();
        names.sort();
        assert_eq!(names, ["assets/keyboard/face.svg", "keyboard.luau", "module.json"]);
        assert!(ModulePackage::open(copy.path()).is_err(), "the copy isn't a whole package");
    }

    #[test]
    fn a_host_module_needs_its_host_script() {
        let scratch = Scratch::new("module-host");
        let folder = scratch.0.join("cpu");
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join(MODULE_MANIFEST), with_host(CLOCK)).unwrap();
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
