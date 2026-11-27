use std::fs;
use std::path::{Path, PathBuf};

use glassdeck_layout::{Layout, Variant};
use glassdeck_package::{DECK_SOURCE, DeckSource, HOST_SCRIPT, read_script, KEYBOARD_SCRIPT, Kind, MANIFEST, OPTIONS_SCRIPT, Package, PackageId, PackageType, QualifiedId, Version, check_name};
use serde::{Deserialize, Serialize};
use serde_json::json;
use specta::Type;

pub const GRID: u32 = 8;
pub(crate) const FIRST_VERSION: &str = "1.0.0";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct Dims {
    pub w: u32,
    pub h: u32,
}

/// How a widget may be sized when placed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Sizing {
    Fixed { size: Dims },
    Range { size: Dims, min: Dims, max: Dims },
    /// Any size in the widget area; stored as the grid's smallest step up to the whole area
    Any { size: Dims },
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct WidgetSummary {
    pub id: String,
    pub title: String,
    pub version: String,
    pub sizing: Sizing,
    pub has_options: bool,
    pub runs_on_host: bool,
    /// Its still, once drawn
    pub preview: Option<crate::stills::Still>,
}

#[derive(Clone, Debug, Deserialize, Type)]
pub struct NewWidget {
    pub title: String,
    pub id: String,
    pub sizing: Sizing,
}

#[derive(Clone, Debug, PartialEq, Serialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LibraryError {
    InvalidTitle { reason: String },
    InvalidId { reason: String },
    InvalidSize { reason: String },
    AlreadyExists { id: String },
    NotFound { id: String },
    /// A background's video file that can't be used
    BadVideo { reason: String },
    /// A `.deck` to import that isn't a widget package this build reads
    NotAWidget { reason: String },
    InvalidVersion { reason: String },
    /// A preview time that isn't a number of seconds from 0 on
    InvalidTime { reason: String },
    /// The type checker couldn't run, such as its native library missing
    CheckerUnavailable { reason: String },
    /// A widget folder in the library that can't be read as one
    Unreadable { folder: String, reason: String },
    Io { reason: String },
}

pub(crate) fn io(context: String) -> impl FnOnce(std::io::Error) -> LibraryError {
    move |e| LibraryError::Io { reason: format!("{context}: {e}") }
}

/// Where the app keeps the packages it edits, one source folder per package id.
pub fn root() -> Result<PathBuf, LibraryError> {
    let roaming = std::env::var_os("APPDATA").ok_or(LibraryError::Io { reason: "APPDATA isn't set".into() })?;
    Ok(PathBuf::from(roaming).join("Glassdeck").join("library"))
}

/// The widget area's size in whole grid steps.
pub fn widget_area() -> Dims {
    let area = Layout::new(Variant::Ansi).widget_area;
    let snap = |v: f32| (v as u32) / GRID * GRID;
    Dims { w: snap(area.width), h: snap(area.height) }
}

const ANY_MIN: Dims = Dims { w: GRID, h: GRID };

fn sizing_of(details: &serde_json::Value) -> Result<Sizing, String> {
    let dims = |key: &str| -> Result<Option<Dims>, String> {
        details.get(key).map(|v| serde_json::from_value(v.clone()).map_err(|e| format!("details.{key}: {e}"))).transpose()
    };
    let size = dims("size")?.ok_or("details has no size")?;
    Ok(match (dims("min")?, dims("max")?) {
        (None, None) => Sizing::Fixed { size },
        (Some(min), Some(max)) if min == ANY_MIN && max == widget_area() => Sizing::Any { size },
        (Some(min), Some(max)) => Sizing::Range { size, min, max },
        _ => return Err("details gives only one of min and max".into()),
    })
}

fn details_of(sizing: Sizing) -> serde_json::Value {
    match sizing {
        Sizing::Fixed { size } => json!({ "size": size }),
        Sizing::Range { size, min, max } => json!({ "size": size, "min": min, "max": max }),
        Sizing::Any { size } => json!({ "size": size, "min": ANY_MIN, "max": widget_area() }),
    }
}

fn check_sizing(sizing: Sizing) -> Result<(), String> {
    let area = widget_area();
    let on_grid = |d: Dims| d.w > 0 && d.h > 0 && d.w.is_multiple_of(GRID) && d.h.is_multiple_of(GRID);
    let fits = |d: Dims| d.w <= area.w && d.h <= area.h;
    let within = |d: Dims, lo: Dims, hi: Dims| (lo.w..=hi.w).contains(&d.w) && (lo.h..=hi.h).contains(&d.h);
    let (size, bounds) = match sizing {
        Sizing::Fixed { size } | Sizing::Any { size } => (size, None),
        Sizing::Range { size, min, max } => (size, Some((min, max))),
    };
    let all: Vec<Dims> = [Some(size), bounds.map(|b| b.0), bounds.map(|b| b.1)].into_iter().flatten().collect();
    if let Some(d) = all.iter().find(|d| !on_grid(**d)) {
        return Err(format!("{}×{} isn't on the {GRID} px grid", d.w, d.h));
    }
    if let Some(d) = all.iter().find(|d| !fits(**d)) {
        return Err(format!("{}×{} is bigger than the widget area, {}×{}", d.w, d.h, area.w, area.h));
    }
    if let Some((min, max)) = bounds
        && !within(size, min, max)
    {
        return Err(format!("The default size {}×{} is outside min and max", size.w, size.h));
    }
    Ok(())
}

/// A checked request to create a widget: everything past here trusts it.
struct ValidNewWidget {
    title: String,
    id: PackageId,
    qualified: QualifiedId,
    sizing: Sizing,
}

fn parse_new(new: NewWidget) -> Result<ValidNewWidget, LibraryError> {
    let title = new.title.trim().to_owned();
    if title.is_empty() {
        return Err(LibraryError::InvalidTitle { reason: "Give the widget a title".into() });
    }
    let id = PackageId::parse(new.id.trim()).map_err(|e| LibraryError::InvalidId { reason: e.to_string() })?;
    check_sizing(new.sizing).map_err(|reason| LibraryError::InvalidSize { reason })?;
    let qualified = QualifiedId::new(PackageType::Widget, &id);
    Ok(ValidNewWidget { title, id, qualified, sizing: new.sizing })
}

pub(crate) fn luau_string(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

/// The files a new widget starts with, by name: a `keyboard.luau` that draws its title, and an empty
/// `options.luau` and `host.luau`, which mean no options and no host half until something is written there.
fn starting_files(new: &ValidNewWidget) -> Vec<(&'static str, String)> {
    let deck = json!({
        "id": new.id.as_str(),
        "title": new.title,
        "version": FIRST_VERSION,
        "type": "widget",
        "details": details_of(new.sizing),
    });
    let mut files = vec![
        (DECK_SOURCE, format!("{}\n", serde_json::to_string_pretty(&deck).expect("a JSON value serializes"))),
        (KEYBOARD_SCRIPT, keyboard_script(new)),
    ];
    files.push((OPTIONS_SCRIPT, String::new()));
    files.push((HOST_SCRIPT, String::new()));
    files
}

fn keyboard_script(new: &ValidNewWidget) -> String {
    let title = luau_string(&new.title);
    format!(
        "local text = {title}\n\nwidget.on_draw(function(ui: Ui)\n    ui:text(text, {{ x = 0, y = ui.height * 0.35, w = ui.width, size = ui.height * 0.3, align = \"center\" }})\nend)\n"
    )
}

fn summarize(folder: &Path) -> Result<Option<WidgetSummary>, LibraryError> {
    let unreadable = |reason: String| LibraryError::Unreadable { folder: folder.display().to_string(), reason };
    if !folder.join(DECK_SOURCE).is_file() {
        return Ok(None);
    }
    let (deck, _) = DeckSource::read(folder).map_err(|e| unreadable(format!("{e:#}")))?;
    if deck.kind != PackageType::Widget {
        return Ok(None);
    }
    let details = deck.details.as_ref().ok_or_else(|| unreadable("a widget's deck.json needs details".into()))?;
    let sizing = sizing_of(details).map_err(unreadable)?;
    Ok(Some(WidgetSummary {
        id: QualifiedId::new(deck.kind, &deck.id).to_string(),
        title: deck.title.clone(),
        version: deck.version.to_string(),
        sizing,
        has_options: has_script(folder, OPTIONS_SCRIPT)?,
        runs_on_host: has_script(folder, HOST_SCRIPT)?,
        preview: crate::stills::still_of(folder),
    }))
}

/// A missing or blank script is the same as none.
pub(crate) fn has_script(folder: &Path, name: &str) -> Result<bool, LibraryError> {
    let unreadable = |e: anyhow::Error| LibraryError::Unreadable { folder: folder.display().to_string(), reason: format!("{e:#}") };
    Ok(read_script(folder, name).map_err(unreadable)?.is_some())
}

pub fn list_widgets(root: &Path) -> Result<Vec<WidgetSummary>, LibraryError> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(io(format!("reading {}", root.display()))(e)),
    };
    let mut widgets = Vec::new();
    for entry in entries {
        let entry = entry.map_err(io(format!("reading {}", root.display())))?;
        if entry.path().is_dir()
            && let Some(widget) = summarize(&entry.path())?
        {
            widgets.push(widget);
        }
    }
    widgets.sort_by_key(|w| w.title.to_lowercase());
    Ok(widgets)
}

pub fn create_widget(root: &Path, new: NewWidget) -> Result<WidgetSummary, LibraryError> {
    let new = parse_new(new)?;
    let folder = root.join(new.qualified.as_str());
    if folder.exists() {
        return Err(LibraryError::AlreadyExists { id: new.qualified.to_string() });
    }
    let staging = root.join(format!(".new-{}", new.qualified.as_str()));
    fs::create_dir_all(&staging).map_err(io(format!("creating {}", staging.display())))?;
    for (name, text) in starting_files(&new) {
        let path = staging.join(name);
        fs::write(&path, text).map_err(io(format!("writing {}", path.display())))?;
    }
    fs::rename(&staging, &folder).map_err(io(format!("moving {} into place", folder.display())))?;
    summarize(&folder)?.ok_or_else(|| LibraryError::Unreadable { folder: folder.display().to_string(), reason: "the new widget didn't read back".into() })
}

/// The folder of an existing package of any type, by qualified id.
pub fn package_folder(root: &Path, id: &str) -> Result<(QualifiedId, PathBuf), LibraryError> {
    let id = QualifiedId::parse(id).map_err(|e| LibraryError::InvalidId { reason: format!("{e:#}") })?;
    let folder = root.join(id.as_str());
    if !folder.join(DECK_SOURCE).is_file() {
        return Err(LibraryError::NotFound { id: id.to_string() });
    }
    Ok((id, folder))
}

/// The folder of an existing package of one type, by qualified id.
pub fn typed_folder(root: &Path, id: &str, package_type: PackageType) -> Result<PathBuf, LibraryError> {
    match package_folder(root, id)? {
        (id, folder) if id.package_type() == package_type => Ok(folder),
        (id, _) => Err(LibraryError::NotFound { id: id.to_string() }),
    }
}

pub fn widget_folder(root: &Path, id: &str) -> Result<PathBuf, LibraryError> {
    typed_folder(root, id, PackageType::Widget)
}

pub(crate) fn read_deck_json(folder: &Path) -> Result<serde_json::Map<String, serde_json::Value>, LibraryError> {
    let path = folder.join(DECK_SOURCE);
    let text = fs::read_to_string(&path).map_err(io(format!("reading {}", path.display())))?;
    serde_json::from_str(&text).map_err(|e| LibraryError::Unreadable { folder: folder.display().to_string(), reason: e.to_string() })
}

pub(crate) fn write_deck_json(folder: &Path, deck: &serde_json::Map<String, serde_json::Value>) -> Result<(), LibraryError> {
    let path = folder.join(DECK_SOURCE);
    let text = format!("{}\n", serde_json::to_string_pretty(deck).expect("a JSON map serializes"));
    fs::write(&path, text).map_err(io(format!("writing {}", path.display())))
}

fn copy_folder(from: &Path, to: &Path) -> Result<(), LibraryError> {
    fs::create_dir_all(to).map_err(io(format!("creating {}", to.display())))?;
    for entry in fs::read_dir(from).map_err(io(format!("reading {}", from.display())))? {
        let entry = entry.map_err(io(format!("reading {}", from.display())))?;
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_folder(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), &target).map_err(io(format!("copying {}", entry.path().display())))?;
        }
    }
    Ok(())
}

/// The first free id of the form `<id>-copy`, `<id>-copy-2`, ... among packages of `id`'s type.
fn free_copy_id(root: &Path, id: &QualifiedId) -> QualifiedId {
    let bare = id.id();
    (1..)
        .map(|n| if n == 1 { format!("{bare}-copy") } else { format!("{bare}-copy-{n}") })
        .map(|text| QualifiedId::new(id.package_type(), &PackageId::parse(&text).expect("a suffix on the last part keeps an id valid")))
        .find(|candidate| !root.join(candidate.as_str()).exists())
        .expect("an unbounded range finds a free id")
}

/// Copies a package of any type under the next free `-copy` id, titled "<title> copy".
pub fn duplicate_package(root: &Path, id: &str) -> Result<QualifiedId, LibraryError> {
    let (original, from) = package_folder(root, id)?;
    let copy = free_copy_id(root, &original);
    let staging = root.join(format!(".new-{copy}"));
    copy_folder(&from, &staging)?;
    let mut deck = read_deck_json(&staging)?;
    let title = deck.get("title").and_then(|t| t.as_str()).unwrap_or(id).to_owned();
    deck.insert("id".into(), json!(copy.id().as_str()));
    deck.insert("title".into(), json!(format!("{title} copy")));
    write_deck_json(&staging, &deck)?;
    let folder = root.join(copy.as_str());
    fs::rename(&staging, &folder).map_err(io(format!("moving {} into place", folder.display())))?;
    Ok(copy)
}

pub fn rename_package(root: &Path, id: &str, title: &str) -> Result<(), LibraryError> {
    let title = title.trim();
    if title.is_empty() {
        return Err(LibraryError::InvalidTitle { reason: "Give it a title".into() });
    }
    let (_, folder) = package_folder(root, id)?;
    let mut deck = read_deck_json(&folder)?;
    deck.insert("title".into(), json!(title));
    write_deck_json(&folder, &deck)
}

/// Moves the package's folder to the Recycle Bin, so a mistaken delete can be undone there.
pub fn delete_package(root: &Path, id: &str) -> Result<(), LibraryError> {
    let (_, folder) = package_folder(root, id)?;
    trash::delete(&folder).map_err(|e| LibraryError::Io { reason: format!("moving {} to the Recycle Bin: {e}", folder.display()) })
}

pub fn export_widget(root: &Path, id: &str, out: &Path) -> Result<(), LibraryError> {
    let folder = widget_folder(root, id)?;
    glassdeck_package::pack_folder(&folder, out).map(|_| ()).map_err(|e| LibraryError::Io { reason: format!("{e:#}") })
}

/// Unpacks a widget `.deck` into a source folder: its entries as they are, and a `deck.json` from its manifest.
pub fn import_widget(root: &Path, deck: &Path) -> Result<WidgetSummary, LibraryError> {
    let not_a_widget = |reason: String| LibraryError::NotAWidget { reason };
    let package = Package::open(deck).map_err(|e| not_a_widget(format!("{e:#}")))?;
    let manifest = package.manifest();
    let Kind::Widget(size) = &manifest.kind else {
        return Err(not_a_widget(format!("{} is a background, not a widget", manifest.id)));
    };
    let folder = root.join(package.id().as_str());
    if folder.exists() {
        return Err(LibraryError::AlreadyExists { id: package.id().to_string() });
    }
    let staging = root.join(format!(".new-{}", package.id()));
    if staging.exists() {
        fs::remove_dir_all(&staging).map_err(io(format!("clearing {}", staging.display())))?;
    }
    for name in package.entry_names().filter(|name| *name != MANIFEST) {
        check_name(name).map_err(|e| not_a_widget(format!("{e:#}")))?;
        let entry = package.entry(name).expect("a listed entry exists");
        let bytes = package.read_entry(entry).map_err(|e| not_a_widget(format!("{e:#}")))?;
        let path = staging.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(io(format!("creating {}", parent.display())))?;
        }
        fs::write(&path, bytes).map_err(io(format!("writing {}", path.display())))?;
    }
    let source = json!({
        "id": manifest.id.as_str(),
        "title": manifest.title,
        "version": manifest.version.to_string(),
        "type": "widget",
        "details": size,
    });
    write_deck_json(&staging, source.as_object().expect("a JSON object"))?;
    fs::rename(&staging, &folder).map_err(io(format!("moving {} into place", folder.display())))?;
    summarize(&folder)?.ok_or(LibraryError::NotFound { id: package.id().to_string() })
}

/// The scripts the widget editor shows, in tab order.
pub const SCRIPTS: [&str; 3] = [KEYBOARD_SCRIPT, OPTIONS_SCRIPT, HOST_SCRIPT];

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct SourceFile {
    pub name: String,
    pub text: String,
}

/// A widget as the editor opens it: its summary and every script, a missing one as empty.
#[derive(Clone, Debug, Serialize, Type)]
pub struct WidgetSource {
    pub widget: WidgetSummary,
    pub files: Vec<SourceFile>,
}

/// What an editor saves: its scripts, and the title and version for `deck.json`.
#[derive(Clone, Debug, Deserialize, Type)]
pub struct SourceEdit {
    pub title: String,
    pub version: String,
    pub files: Vec<SourceFile>,
}

/// Every one of `names` in `folder`, a missing one as empty.
pub(crate) fn read_sources(folder: &Path, names: &[&str]) -> Result<Vec<SourceFile>, LibraryError> {
    let mut files = Vec::new();
    for name in names {
        let path = folder.join(name);
        let text = if path.is_file() { fs::read_to_string(&path).map_err(io(format!("reading {}", path.display())))? } else { String::new() };
        files.push(SourceFile { name: (*name).to_owned(), text });
    }
    Ok(files)
}

/// Writes an editor's files, which must be among `names`, and the title and version into `deck.json`.
pub(crate) fn write_sources(folder: &Path, edit: SourceEdit, names: &[&str]) -> Result<(), LibraryError> {
    let title = edit.title.trim();
    if title.is_empty() {
        return Err(LibraryError::InvalidTitle { reason: "Give it a title".into() });
    }
    let version = Version::parse(edit.version.trim()).map_err(|e| LibraryError::InvalidVersion { reason: e.to_string() })?;
    if let Some(stray) = edit.files.iter().find(|f| !names.contains(&f.name.as_str())) {
        return Err(LibraryError::Io { reason: format!("{} isn't a file the editor writes", stray.name) });
    }
    for file in &edit.files {
        let path = folder.join(&file.name);
        fs::write(&path, &file.text).map_err(io(format!("writing {}", path.display())))?;
    }
    let mut deck = read_deck_json(folder)?;
    deck.insert("title".into(), json!(title));
    deck.insert("version".into(), json!(version.to_string()));
    write_deck_json(folder, &deck)
}

pub fn read_widget(root: &Path, id: &str) -> Result<WidgetSource, LibraryError> {
    let folder = widget_folder(root, id)?;
    let widget = summarize(&folder)?.ok_or(LibraryError::NotFound { id: id.to_owned() })?;
    Ok(WidgetSource { widget, files: read_sources(&folder, &SCRIPTS)? })
}

pub fn save_widget(root: &Path, id: &str, edit: SourceEdit) -> Result<WidgetSummary, LibraryError> {
    let folder = widget_folder(root, id)?;
    write_sources(&folder, edit, &SCRIPTS)?;
    summarize(&folder)?.ok_or(LibraryError::NotFound { id: id.to_owned() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new(sizing: Sizing) -> NewWidget {
        NewWidget { title: "Weather \"now\"".into(), id: "local.user.weather".into(), sizing }
    }

    const SIZE: Dims = Dims { w: 320, h: 160 };

    #[test]
    fn every_sizing_round_trips_through_deck_json() {
        for sizing in [
            Sizing::Fixed { size: SIZE },
            Sizing::Range { size: SIZE, min: Dims { w: 160, h: 80 }, max: Dims { w: 640, h: 240 } },
            Sizing::Any { size: SIZE },
        ] {
            assert_eq!(sizing_of(&details_of(sizing)), Ok(sizing));
        }
    }

    #[test]
    fn sizes_must_be_on_the_grid_inside_the_area_and_bounds() {
        assert!(check_sizing(Sizing::Fixed { size: Dims { w: 321, h: 160 } }).is_err());
        assert!(check_sizing(Sizing::Fixed { size: Dims { w: 320, h: 4000 } }).is_err());
        assert!(check_sizing(Sizing::Range { size: Dims { w: 800, h: 160 }, min: SIZE, max: Dims { w: 640, h: 240 } }).is_err());
        assert!(check_sizing(Sizing::Any { size: SIZE }).is_ok());
    }

    #[test]
    fn bad_ids_and_titles_are_refused_at_the_edge() {
        let mut bad = new(Sizing::Fixed { size: SIZE });
        bad.id = "Weather".into();
        assert!(matches!(parse_new(bad), Err(LibraryError::InvalidId { .. })));
        let mut untitled = new(Sizing::Fixed { size: SIZE });
        untitled.title = "  ".into();
        assert!(matches!(parse_new(untitled), Err(LibraryError::InvalidTitle { .. })));
    }

    #[test]
    fn a_new_widget_has_every_script_and_its_deck_json_reads_back() {
        let files = starting_files(&parse_new(new(Sizing::Any { size: SIZE })).unwrap());
        let names: Vec<&str> = files.iter().map(|(n, _)| *n).collect();
        assert_eq!(names, [DECK_SOURCE, KEYBOARD_SCRIPT, OPTIONS_SCRIPT, HOST_SCRIPT]);
        let deck: serde_json::Value = serde_json::from_str(&files[0].1).unwrap();
        assert_eq!(sizing_of(&deck["details"]), Ok(Sizing::Any { size: SIZE }));
        assert!(files[1].1.contains("\"Weather \\\"now\\\"\""));
    }

    #[test]
    fn create_then_list_finds_it_and_a_second_create_is_refused() {
        let root = std::env::temp_dir().join(format!("glassdeck-library-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let made = create_widget(&root, new(Sizing::Fixed { size: SIZE })).unwrap();
        assert_eq!(made.id, "widget.local.user.weather");
        assert!(!made.has_options && !made.runs_on_host, "empty scripts are no options and no host half");
        assert_eq!(list_widgets(&root).unwrap().len(), 1);
        glassdeck_package::pack_folder(&root.join("widget.local.user.weather"), &root.join("packed.deck")).expect("a new widget packs as it is");
        assert!(matches!(create_widget(&root, new(Sizing::Fixed { size: SIZE })), Err(LibraryError::AlreadyExists { .. })));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn duplicate_rename_export_and_import_round_trip() {
        let root = std::env::temp_dir().join(format!("glassdeck-library-actions-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        create_widget(&root, new(Sizing::Any { size: SIZE })).unwrap();
        assert_eq!(duplicate_package(&root, "widget.local.user.weather").unwrap().as_str(), "widget.local.user.weather-copy");
        assert_eq!(duplicate_package(&root, "widget.local.user.weather").unwrap().as_str(), "widget.local.user.weather-copy-2");
        rename_package(&root, "widget.local.user.weather-copy", "Sky").unwrap();
        assert!(list_widgets(&root).unwrap().iter().any(|w| w.title == "Sky" && w.id == "widget.local.user.weather-copy"));
        assert!(matches!(rename_package(&root, "widget.local.user.weather-copy", " "), Err(LibraryError::InvalidTitle { .. })));
        let deck = root.join("out.deck");
        export_widget(&root, "widget.local.user.weather", &deck).unwrap();
        assert!(matches!(import_widget(&root, &deck), Err(LibraryError::AlreadyExists { .. })));
        let imported = import_widget(&root.join("elsewhere"), &deck).unwrap();
        assert_eq!(imported.sizing, Sizing::Any { size: SIZE });
        assert!(matches!(widget_folder(&root, "widget.local.user.nope"), Err(LibraryError::NotFound { .. })));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_editor_reads_and_saves_scripts_title_and_version() {
        let root = std::env::temp_dir().join(format!("glassdeck-library-editor-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        create_widget(&root, new(Sizing::Fixed { size: SIZE })).unwrap();
        let source = read_widget(&root, "widget.local.user.weather").unwrap();
        let names: Vec<&str> = source.files.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, [KEYBOARD_SCRIPT, OPTIONS_SCRIPT, HOST_SCRIPT]);
        assert!(!source.widget.runs_on_host, "an empty host.luau is no host half");
        let host = SourceFile { name: HOST_SCRIPT.into(), text: "every(1, function() end)\n".into() };
        let saved = save_widget(&root, "widget.local.user.weather", SourceEdit { title: "Sky".into(), version: "1.1.0".into(), files: vec![host] }).unwrap();
        assert_eq!((saved.title.as_str(), saved.version.as_str(), saved.runs_on_host), ("Sky", "1.1.0", true));
        let bad_version = SourceEdit { title: "Sky".into(), version: "1.1".into(), files: vec![] };
        assert!(matches!(save_widget(&root, "widget.local.user.weather", bad_version), Err(LibraryError::InvalidVersion { .. })));
        let stray = SourceEdit { title: "Sky".into(), version: "1.1.0".into(), files: vec![SourceFile { name: "../x.luau".into(), text: String::new() }] };
        assert!(save_widget(&root, "widget.local.user.weather", stray).is_err());
        fs::remove_dir_all(&root).unwrap();
    }
}
