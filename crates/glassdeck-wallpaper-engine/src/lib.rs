//! Wallpaper Engine import, "bring your own install": reads items already in the local Steam workshop folder.
//! Nothing is downloaded, and converted bundles stay on this PC and its keyboard.

pub mod library;
pub mod project;
pub mod steam;
pub mod video;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use glassdeck_deck::{Deck, FORMAT_VERSION, Manifest, Origin, PANEL, VideoFormat};
use glassdeck_proto::Codec;

pub use library::{Item, find, subscribed};
pub use project::{Kind, Project};
pub use video::EncodeSettings;

/// Where converted bundles go unless told otherwise: `%LOCALAPPDATA%\Glassdeck\decks`.
pub fn default_decks_dir() -> Result<PathBuf> {
    let local = std::env::var_os("LOCALAPPDATA").context("LOCALAPPDATA is not set")?;
    Ok(PathBuf::from(local).join("Glassdeck").join("decks"))
}

pub fn deck_dir(decks_dir: &Path, item: &Item) -> PathBuf {
    decks_dir.join(format!("we-{}.{}", item.id, glassdeck_deck::EXTENSION))
}

/// Puts a video wallpaper into a deck: copied when it already meets the deck spec, re-encoded otherwise.
pub fn convert_video(item: &Item, project: &Project, file: &str, out: &Path, settings: EncodeSettings) -> Result<Deck> {
    video::start_media_foundation()?;
    let source = item.dir.join(file);
    let probed = video::probe(&source)?;
    let preview = project.preview.as_ref().map(|name| item.dir.join(name)).filter(|p| p.is_file());
    let manifest = |video: VideoFormat| Manifest {
        format: FORMAT_VERSION,
        title: project.title.clone(),
        origin: Origin::WallpaperEngine { workshop_id: item.id.clone() },
        video,
        preview: preview.as_deref().map(glassdeck_deck::preview_name),
    };
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }

    match glassdeck_deck::check(probed) {
        Ok(format) => {
            println!("{} already meets the deck spec ({:?} {}x{}); copying it", file, format.codec, format.width, format.height);
            Deck::write(out, manifest(format), &source, preview.as_deref())
        }
        Err(why) => {
            println!("Re-encoding {file} to HEVC {}x{}: {why}", PANEL.0, PANEL.1);
            let encoded = out.with_extension("encoding.mp4");
            let frame_rate = video::transcode(&source, &encoded, settings)?;
            let format = VideoFormat { codec: Codec::Hevc, width: PANEL.0, height: PANEL.1, frame_rate };
            let deck = Deck::write(out, manifest(format), &encoded, preview.as_deref());
            std::fs::remove_file(&encoded).with_context(|| format!("removing {}", encoded.display()))?;
            deck
        }
    }
}

/// A web wallpaper as the `web` source opens it: the page's `file://` URL and a script to run before the page's own.
pub struct WebPage {
    pub url: String,
    pub init_script: String,
}

pub fn web_page(item: &Item, project: &Project, file: &str) -> Result<WebPage> {
    let page = item.dir.join(file);
    anyhow::ensure!(page.is_file(), "{} is missing", page.display());
    Ok(WebPage { url: file_url(&std::path::absolute(&page)?), init_script: init_script(project) })
}

/// Stands in for Wallpaper Engine's JS hooks: user properties get their defaults, audio and media stay silent.
pub fn init_script(project: &Project) -> String {
    let properties = project::user_properties_json(&project.properties);
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

fn file_url(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let text = text.strip_prefix("//?/").unwrap_or(&text);
    let mut url = String::from("file:///");
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' | b':' => url.push(byte as char),
            other => url.push_str(&format!("%{other:02X}")),
        }
    }
    url
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_urls_escape_spaces_and_unicode() {
        assert_eq!(
            file_url(Path::new(r"E:\SteamLibrary\a b\Vídeo.html")),
            "file:///E:/SteamLibrary/a%20b/V%C3%ADdeo.html"
        );
    }

    #[test]
    fn the_init_script_embeds_property_defaults() {
        let project = project::parse(br#"{"type":"web","file":"i.html","general":{"properties":{"speed":{"type":"slider","value":3}}}}"#).unwrap();
        let script = init_script(&project);
        assert!(script.contains(r#"{"speed":{"type":"slider","value":3}}"#), "{script}");
        assert!(script.contains("wallpaperRegisterAudioListener"));
    }
}
