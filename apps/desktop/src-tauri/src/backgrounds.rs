//! Backgrounds in the library: a canvas is a source folder packed with Slang; a video is a source file
//! that converting turns into a `.deck`.

use std::fs;
use std::path::{Path, PathBuf};

use glassdeck_package::{DECK_SOURCE, DeckSource, HOST_SCRIPT, KEYBOARD_SCRIPT, OPTIONS_SCRIPT, PackageId, PackageType, QualifiedId};
use glassdeck_slang::pack::CANVAS_SOURCE;
use serde::{Deserialize, Serialize};
use serde_json::json;
use specta::Type;

use crate::library::{FIRST_VERSION, LibraryError, SourceEdit, SourceFile, io, read_sources, typed_folder, write_sources};

/// A video background's source file, as the library keeps it.
pub const VIDEO_SOURCE: &str = "video.json";
/// A video background once converted, ready to send as it is.
pub const CONVERTED: &str = "background.deck";

/// A new canvas starts as the built-in gradient, which is known to compile.
const CANVAS_TEMPLATE: &str = include_str!("../../../../canvases/gradient/canvas.slang");
const CANVAS_SCRIPT_TEMPLATE: &str = include_str!("../../../../canvases/gradient/keyboard.luau");

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundKind {
    Canvas,
    Video,
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct BackgroundSummary {
    pub id: String,
    pub title: String,
    pub version: String,
    pub kind: BackgroundKind,
}

#[derive(Clone, Debug, Deserialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NewContent {
    Canvas,
    /// `source`: the video file to convert
    Video { source: String },
}

#[derive(Clone, Debug, Deserialize, Type)]
pub struct NewBackground {
    pub title: String,
    pub id: String,
    pub content: NewContent,
}

#[derive(Serialize, Deserialize)]
struct VideoSource {
    source: PathBuf,
}

/// A checked request to create a background: everything past here trusts it.
struct ValidNewBackground {
    title: String,
    id: PackageId,
    qualified: QualifiedId,
    content: ValidContent,
}

enum ValidContent {
    Canvas,
    Video { source: PathBuf },
}

fn parse_new(new: NewBackground) -> Result<ValidNewBackground, LibraryError> {
    let title = new.title.trim().to_owned();
    if title.is_empty() {
        return Err(LibraryError::InvalidTitle { reason: "Give the background a title".into() });
    }
    let id = PackageId::parse(new.id.trim()).map_err(|e| LibraryError::InvalidId { reason: e.to_string() })?;
    let content = match new.content {
        NewContent::Canvas => ValidContent::Canvas,
        NewContent::Video { source } => {
            let source = PathBuf::from(source.trim());
            if !source.is_file() {
                return Err(LibraryError::BadVideo { reason: format!("{} isn't a file", source.display()) });
            }
            ValidContent::Video { source }
        }
    };
    let qualified = QualifiedId::new(PackageType::Background, &id);
    Ok(ValidNewBackground { title, id, qualified, content })
}

/// The files a new background starts with, by name.
fn starting_files(new: &ValidNewBackground) -> Vec<(&'static str, String)> {
    let deck = json!({ "id": new.id.as_str(), "title": new.title, "version": FIRST_VERSION, "type": "background" });
    let mut files = vec![(DECK_SOURCE, format!("{}\n", serde_json::to_string_pretty(&deck).expect("a JSON value serializes")))];
    match &new.content {
        ValidContent::Canvas => files.extend([
            (CANVAS_SOURCE, CANVAS_TEMPLATE.to_owned()),
            (KEYBOARD_SCRIPT, CANVAS_SCRIPT_TEMPLATE.to_owned()),
            (OPTIONS_SCRIPT, String::new()),
            (HOST_SCRIPT, String::new()),
        ]),
        ValidContent::Video { source } => {
            let video = VideoSource { source: source.clone() };
            files.push((VIDEO_SOURCE, format!("{}\n", serde_json::to_string_pretty(&video).expect("a path serializes"))));
        }
    }
    files
}

/// What the folder holds decides the kind: `canvas.slang` or `video.json`.
fn kind_of(folder: &Path) -> Option<BackgroundKind> {
    if folder.join(CANVAS_SOURCE).is_file() {
        Some(BackgroundKind::Canvas)
    } else if folder.join(VIDEO_SOURCE).is_file() {
        Some(BackgroundKind::Video)
    } else {
        None
    }
}

fn summarize(folder: &Path) -> Result<Option<BackgroundSummary>, LibraryError> {
    if !folder.join(DECK_SOURCE).is_file() {
        return Ok(None);
    }
    let unreadable = |reason: String| LibraryError::Unreadable { folder: folder.display().to_string(), reason };
    let (deck, _) = DeckSource::read(folder).map_err(|e| unreadable(format!("{e:#}")))?;
    if deck.kind != PackageType::Background {
        return Ok(None);
    }
    let kind = kind_of(folder).ok_or_else(|| unreadable(format!("a background folder holds {CANVAS_SOURCE} or {VIDEO_SOURCE}")))?;
    Ok(Some(BackgroundSummary {
        id: QualifiedId::new(PackageType::Background, &deck.id).to_string(),
        title: deck.title,
        version: deck.version.to_string(),
        kind,
    }))
}

pub fn list_backgrounds(root: &Path) -> Result<Vec<BackgroundSummary>, LibraryError> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(io(format!("reading {}", root.display()))(e)),
    };
    let mut backgrounds = Vec::new();
    for entry in entries {
        let entry = entry.map_err(io(format!("reading {}", root.display())))?;
        if entry.path().is_dir()
            && let Some(background) = summarize(&entry.path())?
        {
            backgrounds.push(background);
        }
    }
    backgrounds.sort_by_key(|b| b.title.to_lowercase());
    Ok(backgrounds)
}

pub fn create_background(root: &Path, new: NewBackground) -> Result<BackgroundSummary, LibraryError> {
    let new = parse_new(new)?;
    let folder = root.join(new.qualified.as_str());
    if folder.exists() {
        return Err(LibraryError::AlreadyExists { id: new.qualified.to_string() });
    }
    let staging = root.join(format!(".new-{}", new.qualified));
    fs::create_dir_all(&staging).map_err(io(format!("creating {}", staging.display())))?;
    for (name, text) in starting_files(&new) {
        let path = staging.join(name);
        fs::write(&path, text).map_err(io(format!("writing {}", path.display())))?;
    }
    fs::rename(&staging, &folder).map_err(io(format!("moving {} into place", folder.display())))?;
    summarize(&folder)?.ok_or_else(|| LibraryError::Unreadable { folder: folder.display().to_string(), reason: "the new background didn't read back".into() })
}

/// The files the canvas editor shows, in tab order.
pub const CANVAS_FILES: [&str; 4] = [CANVAS_SOURCE, KEYBOARD_SCRIPT, OPTIONS_SCRIPT, HOST_SCRIPT];

/// A canvas as the editor opens it: its summary and every file, a missing one as empty.
#[derive(Clone, Debug, Serialize, Type)]
pub struct CanvasSource {
    pub background: BackgroundSummary,
    pub files: Vec<SourceFile>,
}

fn canvas_folder(root: &Path, id: &str) -> Result<PathBuf, LibraryError> {
    let folder = typed_folder(root, id, PackageType::Background)?;
    match kind_of(&folder) {
        Some(BackgroundKind::Canvas) => Ok(folder),
        _ => Err(LibraryError::NotFound { id: id.to_owned() }),
    }
}

pub fn read_canvas(root: &Path, id: &str) -> Result<CanvasSource, LibraryError> {
    let folder = canvas_folder(root, id)?;
    let background = summarize(&folder)?.ok_or(LibraryError::NotFound { id: id.to_owned() })?;
    Ok(CanvasSource { background, files: read_sources(&folder, &CANVAS_FILES)? })
}

pub fn save_canvas(root: &Path, id: &str, edit: SourceEdit) -> Result<BackgroundSummary, LibraryError> {
    let folder = canvas_folder(root, id)?;
    write_sources(&folder, edit, &CANVAS_FILES)?;
    summarize(&folder)?.ok_or(LibraryError::NotFound { id: id.to_owned() })
}

/// What checking a canvas as saved found: Slang's error, if it didn't compile, and the scripts' type problems.
#[derive(Clone, Debug, Serialize, Type)]
pub struct CanvasCheck {
    pub shader_error: Option<String>,
    pub problems: Vec<String>,
    pub checked: u32,
}

pub fn check_canvas(root: &Path, id: &str, native: &Path) -> Result<CanvasCheck, LibraryError> {
    let folder = canvas_folder(root, id)?;
    let unavailable = |e: anyhow::Error| LibraryError::CheckerUnavailable { reason: format!("{e:#}") };
    let slang = glassdeck_slang::Slang::load(native).map_err(unavailable)?;
    let luau = glassdeck_luau_check::LuauChecker::load(native).map_err(unavailable)?;
    let files = read_sources(&folder, &CANVAS_FILES)?;
    let text = |name: &str| files.iter().find(|f| f.name == name).map(|f| f.text.as_str()).filter(|t| !glassdeck_package::is_blank_script(t));
    let shader = text(CANVAS_SOURCE).unwrap_or_default();
    let sources = glassdeck_slang::pack::CanvasSources {
        shader,
        path: CANVAS_SOURCE,
        keyboard: text(KEYBOARD_SCRIPT),
        options: text(OPTIONS_SCRIPT),
        host: text(HOST_SCRIPT),
    };
    let compiled = match glassdeck_slang::pack::compile_canvas(&slang, &sources) {
        Ok(compiled) => compiled,
        Err(e) => return Ok(CanvasCheck { shader_error: Some(format!("{e:#}")), problems: Vec::new(), checked: 0 }),
    };
    let check = glassdeck_slang::pack::canvas_problems(&luau, &folder, &compiled, &sources).map_err(unavailable)?;
    Ok(CanvasCheck { shader_error: None, problems: check.problems, checked: check.checked.len() as u32 })
}

/// Packs a canvas with Slang, or copies a converted video, to `out`. `native`: where Slang and the
/// Luau checker are.
pub fn export_background(root: &Path, id: &str, out: &Path, native: &Path) -> Result<(), LibraryError> {
    let folder = typed_folder(root, id, PackageType::Background)?;
    let failed = |e: anyhow::Error| LibraryError::Io { reason: format!("{e:#}") };
    match kind_of(&folder) {
        Some(BackgroundKind::Canvas) => {
            let unavailable = |e: anyhow::Error| LibraryError::CheckerUnavailable { reason: format!("{e:#}") };
            let slang = glassdeck_slang::Slang::load(native).map_err(unavailable)?;
            let luau = glassdeck_luau_check::LuauChecker::load(native).map_err(unavailable)?;
            glassdeck_slang::pack::pack_canvas(&slang, &luau, &folder, out).map(drop).map_err(failed)
        }
        Some(BackgroundKind::Video) => {
            let converted = folder.join(CONVERTED);
            if !converted.is_file() {
                return Err(LibraryError::BadVideo { reason: "Convert the video before exporting it".into() });
            }
            fs::copy(&converted, out).map(drop).map_err(io(format!("copying {} to {}", converted.display(), out.display())))
        }
        None => Err(LibraryError::NotFound { id: id.to_owned() }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::{duplicate_package, list_widgets, rename_package};

    fn scratch(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("glassdeck-backgrounds-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        root
    }

    fn new(title: &str, id: &str, content: NewContent) -> NewBackground {
        NewBackground { title: title.into(), id: id.into(), content }
    }

    #[test]
    fn a_canvas_starts_as_the_gradient_with_every_script_and_lists_as_a_canvas() {
        let root = scratch("canvas");
        let made = create_background(&root, new("Aurora", "local.me.aurora", NewContent::Canvas)).unwrap();
        assert_eq!((made.id.as_str(), made.kind), ("background.local.me.aurora", BackgroundKind::Canvas));
        let folder = root.join("background.local.me.aurora");
        for name in [CANVAS_SOURCE, KEYBOARD_SCRIPT, OPTIONS_SCRIPT, HOST_SCRIPT] {
            assert!(folder.join(name).is_file(), "{name}");
        }
        assert!(matches!(create_background(&root, new("Aurora", "local.me.aurora", NewContent::Canvas)), Err(LibraryError::AlreadyExists { .. })));
        assert!(list_widgets(&root).unwrap().is_empty(), "a background isn't a widget");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_video_records_its_source_and_refuses_a_missing_file() {
        let root = scratch("video");
        fs::create_dir_all(&root).unwrap();
        let file = root.join("rain.mp4");
        fs::write(&file, b"not really a video").unwrap();
        let made = create_background(&root, new("Rain", "local.me.rain", NewContent::Video { source: file.display().to_string() })).unwrap();
        assert_eq!(made.kind, BackgroundKind::Video);
        let recorded: VideoSource = serde_json::from_str(&fs::read_to_string(root.join("background.local.me.rain").join(VIDEO_SOURCE)).unwrap()).unwrap();
        assert_eq!(recorded.source, file);
        let missing = new("Gone", "local.me.gone", NewContent::Video { source: root.join("gone.mp4").display().to_string() });
        assert!(matches!(create_background(&root, missing), Err(LibraryError::BadVideo { .. })));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_canvas_editor_reads_every_file_and_saves_title_and_version() {
        let root = scratch("editor");
        create_background(&root, new("Aurora", "local.me.aurora", NewContent::Canvas)).unwrap();
        let source = read_canvas(&root, "background.local.me.aurora").unwrap();
        let names: Vec<&str> = source.files.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, CANVAS_FILES);
        let edit = SourceEdit { title: "Dawn".into(), version: "1.1.0".into(), files: vec![SourceFile { name: HOST_SCRIPT.into(), text: "-- later\n".into() }] };
        let saved = save_canvas(&root, "background.local.me.aurora", edit).unwrap();
        assert_eq!((saved.title.as_str(), saved.version.as_str()), ("Dawn", "1.1.0"));
        let stray = SourceEdit { title: "Dawn".into(), version: "1.1.0".into(), files: vec![SourceFile { name: "deck.json".into(), text: String::new() }] };
        assert!(save_canvas(&root, "background.local.me.aurora", stray).is_err());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn card_actions_work_on_backgrounds_too() {
        let root = scratch("actions");
        create_background(&root, new("Aurora", "local.me.aurora", NewContent::Canvas)).unwrap();
        let copy = duplicate_package(&root, "background.local.me.aurora").unwrap();
        assert_eq!(copy.as_str(), "background.local.me.aurora-copy");
        rename_package(&root, copy.as_str(), "Dawn").unwrap();
        let titles: Vec<String> = list_backgrounds(&root).unwrap().into_iter().map(|b| b.title).collect();
        assert_eq!(titles, ["Aurora", "Dawn"]);
        assert!(matches!(parse_new(new(" ", "local.me.x", NewContent::Canvas)), Err(LibraryError::InvalidTitle { .. })));
        assert!(matches!(parse_new(new("X", "aurora", NewContent::Canvas)), Err(LibraryError::InvalidId { .. })));
        fs::remove_dir_all(&root).unwrap();
    }
}
