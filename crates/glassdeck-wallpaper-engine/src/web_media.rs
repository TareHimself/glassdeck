//! Videos inside web wallpapers must decode in hardware in stock CEF on the PC and in WebKit on the RK3566.
//! VP8 and VP9 do both; anything else is re-encoded to VP9 at import, keeping its file name. Needs ffmpeg.

use std::ffi::OsString;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;

pub const VIDEO_EXTENSIONS: &[&str] = &["mp4", "m4v", "mov", "webm", "mkv", "ogv"];
/// Largest picture the keyboard's decoder gets from a page; bigger videos are scaled down.
const MAX_SIZE: (u32, u32) = (1920, 1080);
const VP9_CRF: &str = "32";
const OPUS_BITRATE: &str = "128k";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Probe {
    pub video: Option<VideoStream>,
    pub audio: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VideoStream {
    pub codec: String,
    pub pix_fmt: String,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoAction {
    Copy,
    EncodeVp9,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioAction {
    Copy,
    EncodeOpus,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Plan {
    Keep,
    Convert { video: Option<VideoAction>, audio: Option<AudioAction> },
}

pub fn is_video_file(name: &str) -> bool {
    name.rsplit_once('.').is_some_and(|(_, ext)| VIDEO_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()))
}

/// Keeps what both browsers decode in hardware; AAC and other non-free audio becomes Opus alongside.
pub fn plan(probe: &Probe) -> Plan {
    let video = probe.video.as_ref().map(|v| {
        let decodable = matches!(v.codec.as_str(), "vp8" | "vp9") && v.pix_fmt == "yuv420p";
        let fits = v.width <= MAX_SIZE.0 && v.height <= MAX_SIZE.1;
        if decodable && fits { VideoAction::Copy } else { VideoAction::EncodeVp9 }
    });
    let audio = probe.audio.as_deref().map(|codec| match codec {
        "opus" | "vorbis" | "flac" | "mp3" => AudioAction::Copy,
        _ => AudioAction::EncodeOpus,
    });
    if video != Some(VideoAction::EncodeVp9) && audio != Some(AudioAction::EncodeOpus) {
        Plan::Keep
    } else {
        Plan::Convert { video, audio }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pass {
    Analyse,
    Encode,
}

/// The container follows the file's extension, so the page's references keep working.
pub fn ffmpeg_args(video: Option<VideoAction>, audio: Option<AudioAction>, src: &Path, dst: &Path, pass: Pass, log: &Path) -> Result<Vec<OsString>> {
    let ext = dst.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
    let muxer = match ext.as_str() {
        "webm" => "webm",
        "mkv" => "matroska",
        "ogv" => "ogg",
        "mp4" | "m4v" | "mov" => "mp4",
        other => bail!("no container for .{other}"),
    };
    let mut args: Vec<OsString> = ["-hide_banner", "-loglevel", "error", "-stats", "-y", "-i"].map(Into::into).to_vec();
    args.push(src.into());
    let push = |args: &mut Vec<OsString>, items: &[&str]| args.extend(items.iter().map(OsString::from));
    match video {
        Some(VideoAction::Copy) => push(&mut args, &["-c:v", "copy"]),
        Some(VideoAction::EncodeVp9) => {
            push(&mut args, &["-vf", "scale='min(1920,iw)':'min(1080,ih)':force_original_aspect_ratio=decrease:force_divisible_by=2"]);
            push(&mut args, &["-c:v", "libvpx-vp9", "-profile:v", "0", "-pix_fmt", "yuv420p", "-b:v", "0", "-crf", VP9_CRF]);
            push(&mut args, &["-deadline", "good", "-cpu-used", "2", "-row-mt", "1"]);
            push(&mut args, &["-pass", if pass == Pass::Analyse { "1" } else { "2" }]);
            args.push("-passlogfile".into());
            args.push(log.into());
        }
        None => push(&mut args, &["-vn"]),
    }
    match (pass, audio) {
        (Pass::Analyse, _) | (_, None) => push(&mut args, &["-an"]),
        (Pass::Encode, Some(AudioAction::Copy)) => push(&mut args, &["-c:a", "copy"]),
        (Pass::Encode, Some(AudioAction::EncodeOpus)) => push(&mut args, &["-c:a", "libopus", "-b:a", OPUS_BITRATE]),
    }
    push(&mut args, &["-f", if pass == Pass::Analyse { "null" } else { muxer }]);
    args.push(if pass == Pass::Analyse { "NUL".into() } else { dst.into() });
    Ok(args)
}

pub fn probe(path: &Path) -> Result<Probe> {
    #[derive(Deserialize)]
    struct Output {
        streams: Vec<Stream>,
    }
    #[derive(Deserialize)]
    struct Stream {
        codec_type: String,
        codec_name: Option<String>,
        pix_fmt: Option<String>,
        width: Option<u32>,
        height: Option<u32>,
    }
    let output = Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", "stream=codec_type,codec_name,pix_fmt,width,height", "-of", "json"])
        .arg(path)
        .output()
        .context("running ffprobe (videos inside web wallpapers need ffmpeg on PATH)")?;
    ensure!(output.status.success(), "ffprobe failed on {}: {}", path.display(), String::from_utf8_lossy(&output.stderr).trim());
    let parsed: Output = serde_json::from_slice(&output.stdout).context("parsing ffprobe's output")?;
    let mut probe = Probe { video: None, audio: None };
    for s in parsed.streams {
        match s.codec_type.as_str() {
            "video" if probe.video.is_none() => {
                probe.video = Some(VideoStream {
                    codec: s.codec_name.unwrap_or_default(),
                    pix_fmt: s.pix_fmt.unwrap_or_default(),
                    width: s.width.unwrap_or_default(),
                    height: s.height.unwrap_or_default(),
                })
            }
            "audio" if probe.audio.is_none() => probe.audio = Some(s.codec_name.unwrap_or_default()),
            _ => {}
        }
    }
    Ok(probe)
}

/// Two-pass VP9 (and Opus) into `dst`, which keeps the source's extension.
pub fn convert(src: &Path, dst: &Path, video: Option<VideoAction>, audio: Option<AudioAction>) -> Result<()> {
    let log = dst.with_extension("vp9pass");
    let passes: &[Pass] = if video == Some(VideoAction::EncodeVp9) { &[Pass::Analyse, Pass::Encode] } else { &[Pass::Encode] };
    for &pass in passes {
        let status = Command::new("ffmpeg")
            .args(ffmpeg_args(video, audio, src, dst, pass, &log)?)
            .status()
            .context("running ffmpeg (videos inside web wallpapers need ffmpeg with libvpx on PATH)")?;
        ensure!(status.success(), "ffmpeg failed converting {} ({pass:?} pass)", src.display());
    }
    for leftover in [log.with_extension("vp9pass-0.log"), log.with_extension("vp9pass-0.log.mbtree")] {
        if leftover.exists() {
            std::fs::remove_file(&leftover).with_context(|| format!("removing {}", leftover.display()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn video(codec: &str, width: u32, height: u32) -> Option<VideoStream> {
        Some(VideoStream { codec: codec.into(), pix_fmt: "yuv420p".into(), width, height })
    }

    #[test]
    fn vp8_and_vp9_up_to_1080p_with_free_audio_stay() {
        assert_eq!(plan(&Probe { video: video("vp8", 1920, 1080), audio: Some("vorbis".into()) }), Plan::Keep);
        assert_eq!(plan(&Probe { video: video("vp9", 1280, 720), audio: None }), Plan::Keep);
    }

    #[test]
    fn h264_hevc_and_av1_become_vp9() {
        for codec in ["h264", "hevc", "av1"] {
            assert_eq!(
                plan(&Probe { video: video(codec, 1920, 1080), audio: None }),
                Plan::Convert { video: Some(VideoAction::EncodeVp9), audio: None },
                "{codec}"
            );
        }
    }

    #[test]
    fn oversized_or_10_bit_vp9_is_re_encoded() {
        let big = Probe { video: video("vp9", 3840, 2160), audio: None };
        assert_eq!(plan(&big), Plan::Convert { video: Some(VideoAction::EncodeVp9), audio: None });
        let deep = Probe { video: Some(VideoStream { pix_fmt: "yuv420p10le".into(), ..video("vp9", 1920, 1080).unwrap() }), audio: None };
        assert_eq!(plan(&deep), Plan::Convert { video: Some(VideoAction::EncodeVp9), audio: None });
    }

    #[test]
    fn aac_alone_is_re_encoded_without_touching_the_video() {
        let probe = Probe { video: video("vp9", 1920, 1080), audio: Some("aac".into()) };
        assert_eq!(plan(&probe), Plan::Convert { video: Some(VideoAction::Copy), audio: Some(AudioAction::EncodeOpus) });
    }

    #[test]
    fn the_container_follows_the_extension_and_passes_differ() {
        let encode = Some(VideoAction::EncodeVp9);
        let args = |dst: &str, pass| {
            ffmpeg_args(encode, Some(AudioAction::EncodeOpus), Path::new("in.mp4"), Path::new(dst), pass, Path::new("log"))
                .unwrap()
                .into_iter()
                .map(|a| a.into_string().unwrap())
                .collect::<Vec<_>>()
                .join(" ")
        };
        assert!(args("clip.mp4", Pass::Encode).contains("-f mp4 clip.mp4"));
        assert!(args("clip.mp4", Pass::Encode).contains("-c:a libopus"));
        assert!(args("clip.webm", Pass::Encode).contains("-f webm"));
        let first = args("clip.mp4", Pass::Analyse);
        assert!(first.contains("-pass 1") && first.contains("-an") && first.ends_with("-f null NUL"), "{first}");
    }
}
