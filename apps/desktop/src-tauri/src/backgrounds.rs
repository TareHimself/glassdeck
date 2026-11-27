//! Backgrounds in the library: a canvas is a source folder packed with Slang; a video is converted once
//! and kept as the result (see `videos`).

use std::fs;
use std::path::{Path, PathBuf};

use glassdeck_package::{DECK_SOURCE, DeckSource, HOST_SCRIPT, KEYBOARD_SCRIPT, OPTIONS_SCRIPT, PackageId, PackageType, QualifiedId};
use glassdeck_slang::pack::CANVAS_SOURCE;
use serde::{Deserialize, Serialize};
use serde_json::json;
use specta::Type;

use crate::library::{FIRST_VERSION, LibraryError, SourceEdit, SourceFile, io, read_sources, typed_folder, write_sources};
use crate::videos::{self, Conversion, VideoCut, VideoRecord};

/// What marks a folder as a video background: where its video came from and how it was cut.
pub const VIDEO_RECORD: &str = "video.json";

/// A new canvas starts as the built-in gradient, which is known to compile.
const CANVAS_TEMPLATE: &str = include_str!("../../../../canvases/gradient/canvas.slang");
const CANVAS_SCRIPT_TEMPLATE: &str = include_str!("../../../../canvases/gradient/keyboard.luau");

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundKind {
    Canvas,
    Video,
    Image,
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct BackgroundSummary {
    pub id: String,
    pub title: String,
    pub version: String,
    pub kind: BackgroundKind,
    /// Its still, once drawn
    pub preview: Option<crate::stills::Still>,
}

#[derive(Clone, Debug, Deserialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NewContent {
    Canvas,
    /// `source`: the video file to crop, trim and convert
    Video { source: String, cut: VideoCut },
    /// `source`: the picture to crop and scale to the panel
    Image { source: String },
}

#[derive(Clone, Debug, Deserialize, Type)]
pub struct NewBackground {
    pub title: String,
    pub id: String,
    pub content: NewContent,
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
    Video { record: VideoRecord, conversion: Conversion },
    Image(crate::images::PreparedImage),
}

fn parse_new(root: &Path, new: NewBackground) -> Result<ValidNewBackground, LibraryError> {
    let title = new.title.trim().to_owned();
    if title.is_empty() {
        return Err(LibraryError::InvalidTitle { reason: "Give the background a title".into() });
    }
    let id = PackageId::parse(new.id.trim()).map_err(|e| LibraryError::InvalidId { reason: e.to_string() })?;
    let qualified = QualifiedId::new(PackageType::Background, &id);
    let content = match new.content {
        NewContent::Canvas => ValidContent::Canvas,
        NewContent::Video { source, cut } => {
            let (record, conversion) = videos::plan(&qualified, &root.join(qualified.as_str()), &PathBuf::from(source.trim()), cut)?;
            ValidContent::Video { record, conversion }
        }
        NewContent::Image { source } => ValidContent::Image(crate::images::prepare(&PathBuf::from(source.trim()))?),
    };
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
        ValidContent::Video { record, .. } => files.push((VIDEO_RECORD, videos::record_text(record))),
        ValidContent::Image(_) => {}
    }
    files
}

/// What the folder holds decides the kind: `canvas.slang` or `video.json`.
pub(crate) fn kind_of(folder: &Path) -> Option<BackgroundKind> {
    if folder.join(CANVAS_SOURCE).is_file() {
        Some(BackgroundKind::Canvas)
    } else if folder.join(VIDEO_RECORD).is_file() {
        Some(BackgroundKind::Video)
    } else if folder.join(crate::images::IMAGE_RECORD).is_file() {
        Some(BackgroundKind::Image)
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
    let kind = kind_of(folder).ok_or_else(|| unreadable(format!("a background folder holds {CANVAS_SOURCE} or {VIDEO_RECORD}")))?;
    Ok(Some(BackgroundSummary {
        id: QualifiedId::new(PackageType::Background, &deck.id).to_string(),
        title: deck.title,
        version: deck.version.to_string(),
        kind,
        preview: crate::stills::still_of(folder),
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

/// Makes the background's folder; a video's conversion is returned for the caller to start.
pub fn create_background(root: &Path, new: NewBackground) -> Result<(BackgroundSummary, Option<Conversion>), LibraryError> {
    let new = parse_new(root, new)?;
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
    if let ValidContent::Image(image) = &new.content {
        crate::images::write(&staging, image)?;
    }
    fs::rename(&staging, &folder).map_err(io(format!("moving {} into place", folder.display())))?;
    let conversion = match new.content {
        ValidContent::Canvas | ValidContent::Image(_) => None,
        ValidContent::Video { conversion, .. } => Some(conversion),
    };
    Ok((background_summary(&folder)?, conversion))
}

/// The summary of a folder known to hold a background.
pub fn background_summary(folder: &Path) -> Result<BackgroundSummary, LibraryError> {
    summarize(folder)?.ok_or_else(|| LibraryError::Unreadable { folder: folder.display().to_string(), reason: "it isn't a background".into() })
}

/// The folder of a background of one kind, by qualified id.
pub fn typed_background(root: &Path, id: &str, kind: BackgroundKind) -> Result<PathBuf, LibraryError> {
    let folder = typed_folder(root, id, PackageType::Background)?;
    if kind_of(&folder) == Some(kind) { Ok(folder) } else { Err(LibraryError::NotFound { id: id.to_owned() }) }
}

/// The files the canvas editor shows, in tab order.
pub const CANVAS_FILES: [&str; 5] = [CANVAS_SOURCE, KEYBOARD_SCRIPT, OPTIONS_SCRIPT, HOST_SCRIPT, glassdeck_package::MESSAGES_SCRIPT];

/// A canvas as the editor opens it: its summary and every file, a missing one as empty.
#[derive(Clone, Debug, Serialize, Type)]
pub struct CanvasSource {
    pub background: BackgroundSummary,
    pub files: Vec<SourceFile>,
}

fn canvas_folder(root: &Path, id: &str) -> Result<PathBuf, LibraryError> {
    typed_background(root, id, BackgroundKind::Canvas)
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

/// Makes `at` seconds the moment the canvas's still is drawn from, in its `deck.json`.
pub fn set_preview_time(root: &Path, id: &str, at: f32) -> Result<BackgroundSummary, LibraryError> {
    if !at.is_finite() || at < 0.0 {
        return Err(LibraryError::InvalidTime { reason: format!("{at} isn't a time from 0 seconds on") });
    }
    let folder = canvas_folder(root, id)?;
    let mut deck = crate::library::read_deck_json(&folder)?;
    deck.insert("preview_at".into(), serde_json::json!(at));
    crate::library::write_deck_json(&folder, &deck)?;
    summarize(&folder)?.ok_or(LibraryError::NotFound { id: id.to_owned() })
}

/// The files a new canvas starts with, for previewing it before it exists.
pub fn canvas_template() -> Vec<SourceFile> {
    [(CANVAS_SOURCE, CANVAS_TEMPLATE), (KEYBOARD_SCRIPT, CANVAS_SCRIPT_TEMPLATE), (OPTIONS_SCRIPT, ""), (HOST_SCRIPT, "")]
        .into_iter()
        .map(|(name, text)| SourceFile { name: name.into(), text: text.into() })
        .collect()
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
        messages: text(glassdeck_package::MESSAGES_SCRIPT),
    };
    let compiled = match glassdeck_slang::pack::compile_canvas(&slang, &sources) {
        Ok(compiled) => compiled,
        Err(e) => return Ok(CanvasCheck { shader_error: Some(format!("{e:#}")), problems: Vec::new(), checked: 0 }),
    };
    let check = glassdeck_slang::pack::canvas_problems(&luau, &folder, &compiled, &sources).map_err(unavailable)?;
    Ok(CanvasCheck { shader_error: None, problems: check.problems, checked: check.checked.len() as u32 })
}

/// Packs a canvas with Slang, a converted video, or a picture, to `out`. `native`: where Slang and the
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
        Some(BackgroundKind::Video) => videos::export(&folder, out),
        Some(BackgroundKind::Image) => crate::images::export(&folder, out),
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
    fn a_canvas_remembers_the_moment_its_still_is_drawn_from() {
        let root = scratch("preview-time");
        create_background(&root, new("Aurora", "local.user.aurora", NewContent::Canvas)).unwrap();
        set_preview_time(&root, "background.local.user.aurora", 2.5).unwrap();
        let (deck, _) = DeckSource::read(&root.join("background.local.user.aurora")).unwrap();
        assert_eq!(deck.preview_at, Some(2.5));
        assert!(matches!(set_preview_time(&root, "background.local.user.aurora", -1.0), Err(LibraryError::InvalidTime { .. })));
        assert!(matches!(set_preview_time(&root, "background.local.user.aurora", f32::NAN), Err(LibraryError::InvalidTime { .. })));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_canvas_starts_as_the_gradient_with_every_script_and_lists_as_a_canvas() {
        let root = scratch("canvas");
        let (made, conversion) = create_background(&root, new("Aurora", "local.user.aurora", NewContent::Canvas)).unwrap();
        assert!(conversion.is_none());
        assert_eq!((made.id.as_str(), made.kind), ("background.local.user.aurora", BackgroundKind::Canvas));
        let folder = root.join("background.local.user.aurora");
        for name in [CANVAS_SOURCE, KEYBOARD_SCRIPT, OPTIONS_SCRIPT, HOST_SCRIPT] {
            assert!(folder.join(name).is_file(), "{name}");
        }
        assert!(matches!(create_background(&root, new("Aurora", "local.user.aurora", NewContent::Canvas)), Err(LibraryError::AlreadyExists { .. })));
        assert!(list_widgets(&root).unwrap().is_empty(), "a background isn't a widget");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_video_that_isnt_one_or_isnt_there_is_refused_before_anything_is_written() {
        let root = scratch("video");
        fs::create_dir_all(&root).unwrap();
        let cut = VideoCut { crop: 0.5, start: 0.0, end: 4.0 };
        let fake = root.join("rain.mp4");
        fs::write(&fake, b"not really a video").unwrap();
        for source in [fake, root.join("gone.mp4")] {
            let video = new("Rain", "local.user.rain", NewContent::Video { source: source.display().to_string(), cut });
            assert!(matches!(create_background(&root, video), Err(LibraryError::BadVideo { .. })), "{}", source.display());
        }
        assert!(!root.join("background.local.user.rain").exists());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_canvas_editor_reads_every_file_and_saves_title_and_version() {
        let root = scratch("editor");
        create_background(&root, new("Aurora", "local.user.aurora", NewContent::Canvas)).unwrap();
        let source = read_canvas(&root, "background.local.user.aurora").unwrap();
        let names: Vec<&str> = source.files.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, CANVAS_FILES);
        let edit = SourceEdit { title: "Dawn".into(), version: "1.1.0".into(), files: vec![SourceFile { name: HOST_SCRIPT.into(), text: "-- later\n".into() }] };
        let saved = save_canvas(&root, "background.local.user.aurora", edit).unwrap();
        assert_eq!((saved.title.as_str(), saved.version.as_str()), ("Dawn", "1.1.0"));
        let stray = SourceEdit { title: "Dawn".into(), version: "1.1.0".into(), files: vec![SourceFile { name: "deck.json".into(), text: String::new() }] };
        assert!(save_canvas(&root, "background.local.user.aurora", stray).is_err());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn card_actions_work_on_backgrounds_too() {
        let root = scratch("actions");
        create_background(&root, new("Aurora", "local.user.aurora", NewContent::Canvas)).unwrap();
        let copy = duplicate_package(&root, "background.local.user.aurora").unwrap();
        assert_eq!(copy.as_str(), "background.local.user.aurora-copy");
        rename_package(&root, copy.as_str(), "Dawn").unwrap();
        let titles: Vec<String> = list_backgrounds(&root).unwrap().into_iter().map(|b| b.title).collect();
        assert_eq!(titles, ["Aurora", "Dawn"]);
        assert!(matches!(parse_new(&root, new(" ", "local.user.x", NewContent::Canvas)), Err(LibraryError::InvalidTitle { .. })));
        assert!(matches!(parse_new(&root, new("X", "aurora", NewContent::Canvas)), Err(LibraryError::InvalidId { .. })));
        fs::remove_dir_all(&root).unwrap();
    }
}
