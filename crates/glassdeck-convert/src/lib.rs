//! Turns a video file or a web page folder into a `.deckbg`. Knows nothing about where they came from:
//! importers such as the Wallpaper Engine converter call this with their own title and origin.

pub mod video;
pub mod web_media;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use glassdeck_package::{Background, BackgroundWriter, Content, FORMAT_VERSION, Manifest, Origin, PANEL, VideoFormat};
use glassdeck_proto::Codec;
use serde_json::{Map, Value};

pub use video::EncodeSettings;

pub const WEB_ROOT: &str = "web";
const VIDEO_ENTRY: &str = "video.mp4";

/// What a background says about itself.
pub struct Details {
    pub title: String,
    pub origin: Origin,
    /// An image shown when choosing backgrounds
    pub preview: Option<PathBuf>,
}

/// Where converted backgrounds go unless told otherwise: `%LOCALAPPDATA%\Glassdeck\backgrounds`.
pub fn default_backgrounds_dir() -> Result<PathBuf> {
    let local = std::env::var_os("LOCALAPPDATA").context("LOCALAPPDATA is not set")?;
    Ok(PathBuf::from(local).join("Glassdeck").join("backgrounds"))
}

/// A video goes in as it is when it meets the background spec, and is re-encoded otherwise.
pub fn video(source: &Path, out: &Path, details: Details, settings: EncodeSettings) -> Result<Background> {
    video::start_media_foundation()?;
    let content = |format| Content::Video { file: VIDEO_ENTRY.into(), format };
    let name = source.file_name().map_or_else(|| source.display().to_string(), |n| n.to_string_lossy().into_owned());
    match glassdeck_package::check(video::probe(source)?) {
        Ok(format) => {
            println!("{name} already meets the background spec ({:?} {}x{}); storing it as is", format.codec, format.width, format.height);
            write_background(details, content(format), out, |w| w.add_file(VIDEO_ENTRY, source))
        }
        Err(why) => {
            println!("Re-encoding {name} to HEVC {}x{}: {why}", PANEL.0, PANEL.1);
            let encoded = out.with_extension("encoding.mp4");
            if let Some(parent) = encoded.parent() {
                std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
            }
            let frame_rate = video::transcode(source, &encoded, settings)?;
            let format = VideoFormat { codec: Codec::Hevc, width: PANEL.0, height: PANEL.1, frame_rate };
            let background = write_background(details, content(format), out, |w| w.add_file(VIDEO_ENTRY, &encoded));
            std::fs::remove_file(&encoded).with_context(|| format!("removing {}", encoded.display()))?;
            background
        }
    }
}

/// The whole folder goes under `web/`, so its relative URLs keep working from `background://`.
/// `entry` is the page to open, relative to the folder. Media goes through `web_media` first: VP9
/// where needed, and no sound.
pub fn web(folder: &Path, entry: &str, properties: Map<String, Value>, out: &Path, details: Details) -> Result<Background> {
    let files = folder_files(folder)?;
    let content = Content::Web { entry: format!("{WEB_ROOT}/{}", entry.replace('\\', "/")), properties };
    let converted_dir = out.with_extension("media");
    let sources = web_sources(folder, &files, &converted_dir)?;
    println!("Packing {} files from {}", files.len(), folder.display());
    let background = write_background(details, content, out, |w| {
        for (relative, source) in files.iter().zip(&sources) {
            w.add_file(&format!("{WEB_ROOT}/{relative}"), source)?;
        }
        Ok(())
    });
    if converted_dir.exists() {
        std::fs::remove_dir_all(&converted_dir).with_context(|| format!("removing {}", converted_dir.display()))?;
    }
    background
}

/// Where each file's bytes come from: the folder, or a converted copy under `converted_dir`.
fn web_sources(folder: &Path, files: &[String], converted_dir: &Path) -> Result<Vec<PathBuf>> {
    let mut sources = Vec::with_capacity(files.len());
    for relative in files {
        let original = folder.join(relative);
        if web_media::MediaKind::of(relative).is_none() {
            sources.push(original);
            continue;
        }
        let plan = web_media::plan(&web_media::probe(&original)?).with_context(|| format!("planning {relative}"))?;
        if plan == web_media::Plan::Keep {
            sources.push(original);
            continue;
        }
        println!("{relative}: {plan:?}");
        let converted = converted_dir.join(relative);
        if let Some(parent) = converted.parent() {
            std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        web_media::convert(&original, &converted, plan)?;
        sources.push(converted);
    }
    Ok(sources)
}

fn write_background(details: Details, content: Content, out: &Path, add_content: impl FnOnce(&mut BackgroundWriter) -> Result<()>) -> Result<Background> {
    let preview = details.preview.filter(|p| p.is_file());
    let preview_name = preview.as_deref().map(glassdeck_package::preview_name);
    let manifest = Manifest { format: FORMAT_VERSION, title: details.title, origin: details.origin, content, preview: preview_name.clone() };
    let mut writer = BackgroundWriter::create(out, manifest)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_files_are_relative_and_sorted() {
        let root = std::env::temp_dir().join(format!("glassdeck-convert-files-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("js")).unwrap();
        std::fs::write(root.join("index.html"), "").unwrap();
        std::fs::write(root.join("js").join("app.js"), "").unwrap();
        assert_eq!(folder_files(&root).unwrap(), ["index.html", "js/app.js"]);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
