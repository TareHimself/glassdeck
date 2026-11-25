//! Wallpaper Engine import, "bring your own install": reads items already in the local Steam workshop folder.
//! Nothing is downloaded, and converted decks stay on this PC and its keyboard.

pub mod library;
pub mod project;
pub mod steam;
pub mod video;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use glassdeck_deck::{Content, Deck, DeckWriter, FORMAT_VERSION, Manifest, Origin, PANEL, VideoFormat};
use glassdeck_proto::Codec;
use serde_json::{Map, Value};

pub use library::{Item, find, subscribed};
pub use project::{Kind, Project};
pub use video::EncodeSettings;

pub const WEB_ROOT: &str = "web";
const VIDEO_ENTRY: &str = "video.mp4";

/// Where converted decks go unless told otherwise: `%LOCALAPPDATA%\Glassdeck\decks`.
pub fn default_decks_dir() -> Result<PathBuf> {
    let local = std::env::var_os("LOCALAPPDATA").context("LOCALAPPDATA is not set")?;
    Ok(PathBuf::from(local).join("Glassdeck").join("decks"))
}

pub fn deck_path(decks_dir: &Path, item: &Item) -> PathBuf {
    decks_dir.join(format!("we-{}.{}", item.id, glassdeck_deck::EXTENSION))
}

/// Converts a video or web item into a deck at `out`; other kinds are refused.
pub fn convert(item: &Item, project: &Project, out: &Path, settings: EncodeSettings) -> Result<Deck> {
    match &project.kind {
        Kind::Video { file } => convert_video(item, project, file, out, settings),
        Kind::Web { file } => convert_web(item, project, file, out),
        other => bail!("{} is a {other} wallpaper; only video and web wallpapers convert to a deck", item.id),
    }
}

/// A video goes in as it is when it meets the deck spec, and is re-encoded otherwise.
pub fn convert_video(item: &Item, project: &Project, file: &str, out: &Path, settings: EncodeSettings) -> Result<Deck> {
    video::start_media_foundation()?;
    let source = item.dir.join(file);
    let content = |format| Content::Video { file: VIDEO_ENTRY.into(), format };
    match glassdeck_deck::check(video::probe(&source)?) {
        Ok(format) => {
            println!("{file} already meets the deck spec ({:?} {}x{}); storing it as is", format.codec, format.width, format.height);
            write_deck(item, project, content(format), out, |w| w.add_file(VIDEO_ENTRY, &source))
        }
        Err(why) => {
            println!("Re-encoding {file} to HEVC {}x{}: {why}", PANEL.0, PANEL.1);
            let encoded = out.with_extension("encoding.mp4");
            if let Some(parent) = encoded.parent() {
                std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
            }
            let frame_rate = video::transcode(&source, &encoded, settings)?;
            let format = VideoFormat { codec: Codec::Hevc, width: PANEL.0, height: PANEL.1, frame_rate };
            let deck = write_deck(item, project, content(format), out, |w| w.add_file(VIDEO_ENTRY, &encoded));
            std::fs::remove_file(&encoded).with_context(|| format!("removing {}", encoded.display()))?;
            deck
        }
    }
}

/// The item's whole folder goes under `web/`, so its relative URLs keep working from `deck://`.
pub fn convert_web(item: &Item, project: &Project, file: &str, out: &Path) -> Result<Deck> {
    let files = folder_files(&item.dir)?;
    let content = Content::Web {
        entry: format!("{WEB_ROOT}/{}", file.replace('\\', "/")),
        properties: project::user_properties(&project.properties),
    };
    println!("Packing {} files from {}", files.len(), item.dir.display());
    write_deck(item, project, content, out, |w| {
        for relative in &files {
            w.add_file(&format!("{WEB_ROOT}/{relative}"), &item.dir.join(relative))?;
        }
        Ok(())
    })
}

fn write_deck(
    item: &Item,
    project: &Project,
    content: Content,
    out: &Path,
    add_content: impl FnOnce(&mut DeckWriter) -> Result<()>,
) -> Result<Deck> {
    let preview = project.preview.as_ref().map(|name| item.dir.join(name)).filter(|p| p.is_file());
    let preview_name = preview.as_deref().map(glassdeck_deck::preview_name);
    let manifest = Manifest {
        format: FORMAT_VERSION,
        title: project.title.clone(),
        origin: Origin::WallpaperEngine { workshop_id: item.id.clone() },
        content,
        preview: preview_name.clone(),
    };
    let mut writer = DeckWriter::create(out, manifest)?;
    if let (Some(path), Some(name)) = (&preview, &preview_name) {
        writer.add_file(name, path)?;
    }
    add_content(&mut writer)?;
    writer.finish()
}

/// Every file under `root`, as sorted `/`-separated relative paths.
fn folder_files(root: &Path) -> Result<Vec<String>> {
    let mut files = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).with_context(|| format!("listing {}", dir.display()))? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file() {
                let relative = entry.path().strip_prefix(root)?.to_str().context("a file name is not UTF-8")?.replace('\\', "/");
                files.push(relative);
            }
        }
    }
    files.sort();
    Ok(files)
}

/// Stands in for Wallpaper Engine's JS hooks: user properties get their defaults, audio and media stay silent.
pub fn init_script(properties: &Map<String, Value>) -> String {
    let properties = Value::Object(properties.clone());
    format!(
        r#"(() => {{
  const userProperties = {properties};
  for (const name of ["wallpaperRegisterAudioListener", "wallpaperRegisterMediaStatusListener",
      "wallpaperRegisterMediaPropertiesListener", "wallpaperRegisterMediaThumbnailListener",
      "wallpaperRegisterMediaPlaybackListener", "wallpaperRegisterMediaTimelineListener"]) {{
    window[name] = () => {{}};
  }}
  window.addEventListener("load", () => setTimeout(() => {{
    const listener = window.wallpaperPropertyListener;
    if (listener && typeof listener.applyUserProperties === "function") listener.applyUserProperties(userProperties);
  }}, 0));
}})();"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_init_script_embeds_property_defaults() {
        let project = project::parse(br#"{"type":"web","file":"i.html","general":{"properties":{"speed":{"type":"slider","value":3}}}}"#).unwrap();
        let script = init_script(&project::user_properties(&project.properties));
        assert!(script.contains(r#"{"speed":{"type":"slider","value":3}}"#), "{script}");
        assert!(script.contains("wallpaperRegisterAudioListener"));
    }

    #[test]
    fn folder_files_are_relative_and_sorted() {
        let root = std::env::temp_dir().join(format!("glassdeck-we-files-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("js")).unwrap();
        std::fs::write(root.join("index.html"), "").unwrap();
        std::fs::write(root.join("js").join("app.js"), "").unwrap();
        assert_eq!(folder_files(&root).unwrap(), ["index.html", "js/app.js"]);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
