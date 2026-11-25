use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use glassdeck_deck::{Content, Deck, PANEL};
use glassdeck_proto::Codec;

use crate::pipeline::{EncoderSettings, Source};
use crate::play::{FileSource, Media};
use crate::web::{DECK_ORIGIN, WebSource};

const WEB_KEYFRAME_SECONDS: u32 = 2;

/// How a web deck is rendered and encoded; video decks are sent as they are.
#[derive(Clone, Copy, Debug)]
pub struct WebEncoding {
    pub fps: u32,
    pub bitrate: u32,
    pub duration: Option<Duration>,
}

/// A video deck streams its MP4 entry in place; a web deck renders through CEF from `deck://`.
pub fn open(deck: Deck, looping: bool, web: WebEncoding) -> Result<Source> {
    let deck = Arc::new(deck);
    println!("{} ({})", deck.manifest().title, deck.path().display());
    match deck.manifest().content.clone() {
        Content::Video { file, .. } => {
            let entry = deck.entry(&file).with_context(|| format!("the deck has no {file}"))?;
            Ok(Source::Encoded(Box::new(FileSource::open(Media::DeckEntry { deck, entry }, looping)?)))
        }
        Content::Web { entry, properties } => {
            let settings = EncoderSettings {
                width: PANEL.0 as u32,
                height: PANEL.1 as u32,
                codec: Codec::Hevc,
                fps: web.fps,
                bitrate: web.bitrate,
                keyframe_every: web.fps * WEB_KEYFRAME_SECONDS,
                vbv_frames: None,
            };
            let url = format!("{DECK_ORIGIN}{}", url_path(&entry));
            let source = WebSource::new(&url, settings, web.duration)?
                .with_init_script(&glassdeck_wallpaper_engine::init_script(&properties))?
                .with_deck(deck);
            Ok(Source::Frames(Box::new(source)))
        }
    }
}

fn url_path(entry: &str) -> String {
    let mut out = String::with_capacity(entry.len());
    for byte in entry.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => out.push(byte as char),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_paths_escape_spaces_and_unicode() {
        assert_eq!(url_path("web/my page/Vídeo.html"), "web/my%20page/V%C3%ADdeo.html");
    }
}
