//! Media inside web wallpapers. Videos must decode in hardware in stock CEF on the PC and in WebKit on the
//! RK3566: VP8 and VP9 do, anything else becomes VP9. Wallpapers are silent, so audio tracks are dropped and
//! audio files become silence of the same length, keeping every file name the page refers to. Silence is FLAC
//! wherever the container allows, the cheapest to decode on the keyboard; .mp3 and .webm keep codecs of their own.
//! Needs ffmpeg.

use std::ffi::OsString;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;

const VIDEO_EXTENSIONS: &[&str] = &["mp4", "m4v", "mov", "webm", "mkv", "ogv"];
const AUDIO_EXTENSIONS: &[&str] = &["mp3", "ogg", "oga", "opus", "weba", "m4a", "aac", "wav", "flac"];
/// Largest picture the keyboard's decoder gets from a page; bigger videos are scaled down.
const MAX_SIZE: (u32, u32) = (1920, 1080);
const VP9_CRF: &str = "32";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaKind {
    Video,
    Audio,
}

impl MediaKind {
    pub fn of(name: &str) -> Option<Self> {
        let ext = name.rsplit_once('.')?.1.to_ascii_lowercase();
        if VIDEO_EXTENSIONS.contains(&ext.as_str()) {
            Some(Self::Video)
        } else if AUDIO_EXTENSIONS.contains(&ext.as_str()) {
            Some(Self::Audio)
        } else {
            None
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Probe {
    pub video: Option<VideoStream>,
    pub has_audio: bool,
    pub seconds: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VideoStream {
    pub codec: String,
    pub pix_fmt: String,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Plan {
    Keep,
    /// A decodable video with an audio track: copy the video, drop the audio
    DropAudio,
    EncodeVp9,
    /// An audio file, or a "video" with no picture: silence of the same length
    Silence { seconds: f64 },
}

/// Every media file the page names stays, under its name: as it is, without audio, as VP9, or as silence.
pub fn plan(probe: &Probe) -> Result<Plan> {
    let Some(video) = &probe.video else {
        let seconds = probe.seconds.context("the audio has no duration")?;
        return Ok(Plan::Silence { seconds });
    };
    let decodable = matches!(video.codec.as_str(), "vp8" | "vp9") && video.pix_fmt == "yuv420p";
    let fits = video.width <= MAX_SIZE.0 && video.height <= MAX_SIZE.1;
    Ok(match (decodable && fits, probe.has_audio) {
        (false, _) => Plan::EncodeVp9,
        (true, true) => Plan::DropAudio,
        (true, false) => Plan::Keep,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pass {
    Analyse,
    Encode,
}

/// The arguments for one ffmpeg run; the container always follows `dst`'s extension.
pub fn ffmpeg_args(plan: Plan, src: &Path, dst: &Path, pass: Pass, log: &Path) -> Result<Vec<OsString>> {
    let ext = dst.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
    let mut args: Vec<OsString> = ["-hide_banner", "-loglevel", "error", "-stats", "-y"].map(Into::into).to_vec();
    let push = |args: &mut Vec<OsString>, items: &[&str]| args.extend(items.iter().map(OsString::from));
    match plan {
        Plan::Keep => bail!("nothing to do for a file that stays as it is"),
        Plan::DropAudio | Plan::EncodeVp9 => {
            args.push("-i".into());
            args.push(src.into());
            push(&mut args, &["-map", "0:v:0", "-an", "-sn", "-dn"]);
            if plan == Plan::DropAudio {
                push(&mut args, &["-c:v", "copy"]);
            } else {
                push(&mut args, &["-vf", "scale='min(1920,iw)':'min(1080,ih)':force_original_aspect_ratio=decrease:force_divisible_by=2"]);
                push(&mut args, &["-c:v", "libvpx-vp9", "-profile:v", "0", "-pix_fmt", "yuv420p", "-b:v", "0", "-crf", VP9_CRF]);
                push(&mut args, &["-deadline", "good", "-cpu-used", "2", "-row-mt", "1"]);
                push(&mut args, &["-pass", if pass == Pass::Analyse { "1" } else { "2" }]);
                args.push("-passlogfile".into());
                args.push(log.into());
            }
            if pass == Pass::Analyse {
                push(&mut args, &["-f", "null", "NUL"]);
                return Ok(args);
            }
            let muxer = match ext.as_str() {
                "webm" => "webm",
                "mkv" => "matroska",
                "ogv" => "ogg",
                "mp4" | "m4v" | "mov" => "mp4",
                other => bail!("no video container for .{other}"),
            };
            push(&mut args, &["-f", muxer]);
        }
        Plan::Silence { seconds } => {
            push(&mut args, &["-f", "lavfi", "-t", &format!("{seconds:.3}"), "-i", "anullsrc=r=48000:cl=mono"]);
            let encoding: &[&str] = match ext.as_str() {
                "flac" => &["-ar", "8000", "-c:a", "flac", "-f", "flac"],
                "ogg" | "oga" | "opus" | "ogv" => &["-ar", "8000", "-c:a", "flac", "-f", "ogg"],
                "m4a" | "aac" | "mp4" | "m4v" | "mov" => &["-ar", "8000", "-c:a", "flac", "-f", "mp4"],
                "mkv" => &["-ar", "8000", "-c:a", "flac", "-f", "matroska"],
                "weba" | "webm" => &["-ar", "8000", "-c:a", "libvorbis", "-q:a", "-1", "-f", "webm"],
                "mp3" => &["-ar", "24000", "-c:a", "libmp3lame", "-b:a", "8k", "-f", "mp3"],
                "wav" => &["-ar", "8000", "-c:a", "pcm_u8", "-f", "wav"],
                other => bail!("no silent stand-in for .{other}"),
            };
            push(&mut args, encoding);
        }
    }
    args.push(dst.into());
    Ok(args)
}

pub fn probe(path: &Path) -> Result<Probe> {
    #[derive(Deserialize)]
    struct Output {
        #[serde(default)]
        streams: Vec<Stream>,
        format: Option<Format>,
    }
    #[derive(Deserialize)]
    struct Stream {
        codec_type: String,
        codec_name: Option<String>,
        pix_fmt: Option<String>,
        width: Option<u32>,
        height: Option<u32>,
    }
    #[derive(Deserialize)]
    struct Format {
        duration: Option<String>,
    }
    let output = Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", "stream=codec_type,codec_name,pix_fmt,width,height:format=duration", "-of", "json"])
        .arg(path)
        .output()
        .context("running ffprobe (media inside web wallpapers needs ffmpeg on PATH)")?;
    ensure!(output.status.success(), "ffprobe failed on {}: {}", path.display(), String::from_utf8_lossy(&output.stderr).trim());
    let parsed: Output = serde_json::from_slice(&output.stdout).context("parsing ffprobe's output")?;
    let seconds = match parsed.format.and_then(|f| f.duration) {
        Some(text) => Some(text.parse::<f64>().with_context(|| format!("ffprobe gave a duration of {text:?}"))?),
        None => None,
    };
    let video = parsed.streams.iter().find(|s| s.codec_type == "video").map(|s| VideoStream {
        codec: s.codec_name.clone().unwrap_or_default(),
        pix_fmt: s.pix_fmt.clone().unwrap_or_default(),
        width: s.width.unwrap_or_default(),
        height: s.height.unwrap_or_default(),
    });
    let has_audio = parsed.streams.iter().any(|s| s.codec_type == "audio");
    Ok(Probe { video, has_audio, seconds })
}

/// Writes `dst` (same extension as the source) as the plan says; VP9 takes two passes.
pub fn convert(src: &Path, dst: &Path, plan: Plan) -> Result<()> {
    let log = dst.with_extension("vp9pass");
    let passes: &[Pass] = if plan == Plan::EncodeVp9 { &[Pass::Analyse, Pass::Encode] } else { &[Pass::Encode] };
    for &pass in passes {
        let status = Command::new("ffmpeg")
            .args(ffmpeg_args(plan, src, dst, pass, &log)?)
            .status()
            .context("running ffmpeg (media inside web wallpapers needs ffmpeg with libvpx and libopus on PATH)")?;
        ensure!(status.success(), "ffmpeg failed converting {} ({pass:?} pass)", src.display());
    }
    let pass_log = log.with_extension("vp9pass-0.log");
    if pass_log.exists() {
        std::fs::remove_file(&pass_log).with_context(|| format!("removing {}", pass_log.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_video(codec: &str, width: u32, height: u32, has_audio: bool) -> Probe {
        Probe {
            video: Some(VideoStream { codec: codec.into(), pix_fmt: "yuv420p".into(), width, height }),
            has_audio,
            seconds: Some(3.0),
        }
    }

    fn joined(plan: Plan, dst: &str, pass: Pass) -> String {
        ffmpeg_args(plan, Path::new("in"), Path::new(dst), pass, Path::new("log"))
            .unwrap()
            .into_iter()
            .map(|a| a.into_string().unwrap())
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn media_kind_comes_from_the_extension() {
        assert_eq!(MediaKind::of("video/test.WEBM"), Some(MediaKind::Video));
        assert_eq!(MediaKind::of("music/song.mp3"), Some(MediaKind::Audio));
        assert_eq!(MediaKind::of("index.html"), None);
    }

    #[test]
    fn silent_vp8_and_vp9_up_to_1080p_stay() {
        assert_eq!(plan(&with_video("vp8", 1920, 1080, false)).unwrap(), Plan::Keep);
        assert_eq!(plan(&with_video("vp9", 1280, 720, false)).unwrap(), Plan::Keep);
    }

    #[test]
    fn a_decodable_video_with_sound_only_loses_its_audio() {
        assert_eq!(plan(&with_video("vp8", 1920, 1080, true)).unwrap(), Plan::DropAudio);
        let args = joined(Plan::DropAudio, "clip.webm", Pass::Encode);
        assert!(args.contains("-c:v copy") && args.contains("-an") && args.ends_with("-f webm clip.webm"), "{args}");
    }

    #[test]
    fn other_codecs_sizes_and_depths_become_vp9() {
        for codec in ["h264", "hevc", "av1"] {
            assert_eq!(plan(&with_video(codec, 1920, 1080, true)).unwrap(), Plan::EncodeVp9, "{codec}");
        }
        assert_eq!(plan(&with_video("vp9", 3840, 2160, false)).unwrap(), Plan::EncodeVp9);
        let mut deep = with_video("vp9", 1920, 1080, false);
        deep.video.as_mut().unwrap().pix_fmt = "yuv420p10le".into();
        assert_eq!(plan(&deep).unwrap(), Plan::EncodeVp9);
    }

    #[test]
    fn vp9_runs_two_passes_without_audio() {
        let first = joined(Plan::EncodeVp9, "clip.mp4", Pass::Analyse);
        assert!(first.contains("-pass 1") && first.contains("-an") && first.ends_with("-f null NUL"), "{first}");
        let second = joined(Plan::EncodeVp9, "clip.mp4", Pass::Encode);
        assert!(second.contains("-pass 2") && second.ends_with("-f mp4 clip.mp4"), "{second}");
    }

    #[test]
    fn audio_becomes_silence_of_the_same_length_in_its_own_format() {
        let song = Probe { video: None, has_audio: true, seconds: Some(183.25) };
        let silence = plan(&song).unwrap();
        assert_eq!(silence, Plan::Silence { seconds: 183.25 });
        let mp3 = joined(silence, "song.mp3", Pass::Encode);
        assert!(mp3.contains("-t 183.250") && mp3.contains("anullsrc") && mp3.ends_with("-f mp3 song.mp3"), "{mp3}");
        let ogg = joined(silence, "loop.ogg", Pass::Encode);
        assert!(ogg.contains("-c:a flac") && ogg.ends_with("-f ogg loop.ogg"), "{ogg}");
        assert!(joined(silence, "song.m4a", Pass::Encode).contains("-c:a flac"));
        assert!(joined(silence, "song.weba", Pass::Encode).contains("libvorbis"));
        assert!(joined(silence, "hit.wav", Pass::Encode).contains("pcm_u8"));
    }

    #[test]
    fn audio_without_a_duration_fails() {
        assert!(plan(&Probe { video: None, has_audio: true, seconds: None }).is_err());
    }
}
