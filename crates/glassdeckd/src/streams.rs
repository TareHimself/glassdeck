//! What `bg` chooses, and the streams it can choose from. The keyboard plays the built-in scene or a
//! `.deckbg` itself; a stream comes from the host while it's shown. For now the only streams are the
//! built-in ones, one per monitor.

use std::fmt;
use std::path::Path;

use anyhow::{Context, Result, bail, ensure};
use glassdeck_capture::EncoderKind;
use glassdeck_capture::monitors::MonitorInfo;
use glassdeck_capture::pipeline::VideoCodec;
use glassdeck_capture::sources::{Play, SourceRequest};
use serde::{Deserialize, Serialize};

pub const BG_FORMAT_VERSION: u32 = 1;
const BUILT_IN: &str = "gradient";
const STREAM_PREFIX: &str = "stream:";
const MONITOR_PREFIX: &str = "monitor-";
const MAX_NAME_BYTES: usize = 64;

/// A stream's name: `a-z 0-9 . -`, up to 64 bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamName(String);

impl StreamName {
    pub fn parse(name: &str) -> Result<Self> {
        let plain = !name.is_empty() && name.len() <= MAX_NAME_BYTES && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"-.".contains(&b));
        ensure!(plain, "{name:?} isn't a stream name: up to {MAX_NAME_BYTES} lowercase letters, digits, '-' and '.'");
        Ok(Self(name.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for StreamName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What the video layer shows when nothing else asks for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Choice {
    /// The keyboard's built-in scene
    BuiltIn,
    /// A `.deckbg` in the keyboard's background folder, by file name
    Background(String),
    Stream(StreamName),
}

impl Choice {
    pub fn parse(text: &str) -> Result<Self> {
        if text == BUILT_IN {
            return Ok(Self::BuiltIn);
        }
        if let Some(name) = text.strip_prefix(STREAM_PREFIX) {
            return StreamName::parse(name).map(Self::Stream);
        }
        let is_background = !text.contains(['/', '\\']) && text.strip_suffix(glassdeck_package::EXTENSION).is_some_and(|stem| stem.len() > 1 && stem.ends_with('.'));
        ensure!(is_background, "{text:?} isn't {BUILT_IN}, a background file name like late-night.deckbg, or {STREAM_PREFIX}<name>");
        Ok(Self::Background(text.to_owned()))
    }

    pub fn runs_on(&self) -> &'static str {
        match self {
            Self::BuiltIn | Self::Background(_) => "keyboard",
            Self::Stream(_) => "host",
        }
    }
}

impl fmt::Display for Choice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BuiltIn => f.write_str(BUILT_IN),
            Self::Background(name) => f.write_str(name),
            Self::Stream(name) => write!(f, "{STREAM_PREFIX}{name}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    /// Tell the keyboard what to play on its own, by its `SelectBackground` name
    SelectOnKeyboard(String),
    /// End the stream being shown, if any; the keyboard falls back to its own choice
    StopStream,
    ShowStream(StreamName),
}

/// What choosing `choice` does. The keyboard keeps its own choice while a stream is shown, and
/// returns to it when the stream ends.
pub fn effects(choice: &Choice) -> Vec<Effect> {
    match choice {
        Choice::BuiltIn => vec![Effect::StopStream, Effect::SelectOnKeyboard(glassdeck_proto::BUILT_IN_BACKGROUND.to_owned())],
        Choice::Background(name) => vec![Effect::StopStream, Effect::SelectOnKeyboard(name.clone())],
        Choice::Stream(name) => vec![Effect::ShowStream(name.clone())],
    }
}

/// A built-in stream: mirroring one monitor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MonitorStream {
    pub name: StreamName,
    pub monitor: MonitorInfo,
}

impl MonitorStream {
    pub fn request(&self) -> SourceRequest {
        let play = Play::Monitor {
            monitor: self.monitor.index.to_string(),
            encoder: EncoderKind::Gpu,
            codec: VideoCodec::Hevc,
            mbps: 15,
            fps: 60,
            keyframe_every: 120,
            vbv_frames: None,
        };
        SourceRequest { play, seconds: None }
    }
}

/// `monitor-<name>` from each monitor's EDID name, lowercase, other characters as '-'. Two monitors
/// with the same name get their index added.
pub fn monitor_streams(monitors: &[MonitorInfo]) -> Vec<MonitorStream> {
    let base = |m: &MonitorInfo| {
        let slug: String = m.name.to_ascii_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
        let slug = slug.split('-').filter(|part| !part.is_empty()).collect::<Vec<_>>().join("-");
        let slug = if slug.is_empty() { m.index.to_string() } else { slug };
        format!("{MONITOR_PREFIX}{slug}")
    };
    monitors
        .iter()
        .map(|m| {
            let shared = monitors.iter().filter(|other| base(other) == base(m)).count() > 1;
            let mut name = if shared { format!("{}-{}", base(m), m.index) } else { base(m) };
            name.truncate(MAX_NAME_BYTES);
            MonitorStream { name: StreamName(name), monitor: m.clone() }
        })
        .collect()
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Saved {
    format: u32,
    bg: String,
}

/// The remembered choice, if one was ever made.
pub fn load(path: &Path) -> Result<Option<Choice>> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let saved: Saved = serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    if saved.format != BG_FORMAT_VERSION {
        bail!("{} is format {}, not the {BG_FORMAT_VERSION} this build reads", path.display(), saved.format);
    }
    Choice::parse(&saved.bg).map(Some).with_context(|| format!("reading {}", path.display()))
}

pub fn save(path: &Path, choice: &Choice) -> Result<()> {
    let text = serde_json::to_string_pretty(&Saved { format: BG_FORMAT_VERSION, bg: choice.to_string() })?;
    let staging = path.with_extension("json.tmp");
    std::fs::write(&staging, text).with_context(|| format!("writing {}", staging.display()))?;
    std::fs::rename(&staging, path).with_context(|| format!("moving {} into place", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor(index: usize, name: &str) -> MonitorInfo {
        MonitorInfo { index, name: name.into(), width: 1920, height: 1080, refresh_hz: 60 }
    }

    #[test]
    fn choices_parse_and_print_back() {
        for text in ["gradient", "late-night.deckbg", "stream:monitor-q27g4zd"] {
            assert_eq!(Choice::parse(text).unwrap().to_string(), text);
        }
        for bad in ["", "late-night.mp4", "a/b.deckbg", ".deckbg", "stream:", "stream:Big", "monitor-q27g4zd"] {
            assert!(Choice::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn keyboard_choices_end_any_stream_and_streams_leave_the_keyboard_alone() {
        assert_eq!(effects(&Choice::BuiltIn), vec![Effect::StopStream, Effect::SelectOnKeyboard(glassdeck_proto::BUILT_IN_BACKGROUND.into())]);
        assert_eq!(effects(&Choice::Background("a.deckbg".into())), vec![Effect::StopStream, Effect::SelectOnKeyboard("a.deckbg".into())]);
        let name = StreamName::parse("monitor-benq").unwrap();
        assert_eq!(effects(&Choice::Stream(name.clone())), vec![Effect::ShowStream(name)]);
    }

    #[test]
    fn monitor_names_are_readable_and_unique() {
        let names: Vec<String> = monitor_streams(&[monitor(1, "Q27G4ZD"), monitor(2, "BenQ EX270QM"), monitor(28, "VDD by MTT"), monitor(3, "Dell"), monitor(4, "Dell"), monitor(5, "")])
            .into_iter()
            .map(|s| s.name.0)
            .collect();
        assert_eq!(names, ["monitor-q27g4zd", "monitor-benq-ex270qm", "monitor-vdd-by-mtt", "monitor-dell-3", "monitor-dell-4", "monitor-5"]);
    }

    #[test]
    fn the_choice_is_remembered() {
        let path = std::env::temp_dir().join(format!("glassdeckd-bg-{}.json", std::process::id()));
        assert_eq!(load(&path).unwrap(), None);
        let choice = Choice::Stream(StreamName::parse("monitor-benq").unwrap());
        save(&path, &choice).unwrap();
        assert_eq!(load(&path).unwrap(), Some(choice));
        std::fs::remove_file(&path).unwrap();
    }
}
