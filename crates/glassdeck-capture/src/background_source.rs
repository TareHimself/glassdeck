use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use glassdeck_package::{Content, Kind, Package, PANEL};
use glassdeck_proto::Codec;

use crate::pipeline::{EncoderSettings, Source};
use crate::play::{FileSource, Media};
use crate::web::{BACKGROUND_ORIGIN, WebSource};

const WEB_KEYFRAME_SECONDS: u32 = 2;

/// How a web background is rendered and encoded; video backgrounds are sent as they are.
#[derive(Clone, Copy, Debug)]
pub struct WebEncoding {
    pub fps: u32,
    pub bitrate: u32,
    pub duration: Option<Duration>,
}

/// A video background streams its MP4 entry in place; a web background renders through CEF from `background://`.
pub fn open(background: Package, looping: bool, web: WebEncoding) -> Result<Source> {
    let background = Arc::new(background);
    println!("{} ({})", background.manifest().title, background.id());
    match background.manifest().kind.clone() {
        Kind::Widget(_) => bail!("{} is a widget, not a background", background.id()),
        Kind::Background(Content::Video { file, .. }) => {
            let entry = background.entry(&file).with_context(|| format!("the background has no {file}"))?;
            Ok(Source::Encoded(Box::new(FileSource::open(Media::BackgroundEntry { background, entry }, looping)?)))
        }
        Kind::Background(Content::Canvas(_)) => bail!("{} is a canvas, which the keyboard draws itself; it can't be streamed", background.manifest().title),
        Kind::Background(Content::Web { entry, properties }) => {
            let settings = EncoderSettings {
                width: PANEL.0 as u32,
                height: PANEL.1 as u32,
                codec: Codec::Hevc,
                fps: web.fps,
                bitrate: web.bitrate,
                keyframe_every: web.fps * WEB_KEYFRAME_SECONDS,
                vbv_frames: None,
            };
            let url = format!("{BACKGROUND_ORIGIN}{}", url_path(&entry));
            let source = WebSource::new(&url, settings, web.duration)?
                .with_init_script(&glassdeck_wallpaper_engine::init_script(&properties))?
                .with_background(background);
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
