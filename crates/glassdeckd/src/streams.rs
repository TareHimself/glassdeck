//! The streams a theme can show while the host is on. For now the only streams are the built-in
//! ones, one per monitor.

use std::fmt;

use anyhow::{Result, ensure};
use glassdeck_capture::EncoderKind;
use glassdeck_capture::monitors::MonitorInfo;
use glassdeck_capture::pipeline::VideoCodec;
use glassdeck_capture::sources::{Play, SourceRequest};

const MONITOR_PREFIX: &str = "monitor-";
const MAX_NAME_BYTES: usize = 64;
/// HEVC: fewer bits for the same picture, and the RK3566 decodes it faster
const STREAM_CODEC: VideoCodec = VideoCodec::Hevc;
const PREVIEW_WIDTH: u32 = 960;
const PREVIEW_FPS: u32 = 30;
const PREVIEW_MBPS: u32 = 3;

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
            codec: STREAM_CODEC,
            mbps: 15,
            fps: 60,
            keyframe_every: 120,
            vbv_frames: None,
            max_width: None,
        };
        SourceRequest { play, seconds: None }
    }

    pub fn codec(&self) -> glassdeck_proto::Codec {
        STREAM_CODEC.into()
    }

    /// A small H.264 encoding of the monitor, for a viewer that can't decode the stream's own codec.
    pub fn preview_request(&self) -> SourceRequest {
        let play = Play::Monitor {
            monitor: self.monitor.index.to_string(),
            encoder: EncoderKind::Gpu,
            codec: VideoCodec::H264,
            mbps: PREVIEW_MBPS,
            fps: PREVIEW_FPS,
            keyframe_every: PREVIEW_FPS * 2,
            vbv_frames: None,
            max_width: Some(PREVIEW_WIDTH),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor(index: usize, name: &str) -> MonitorInfo {
        MonitorInfo { index, name: name.into(), width: 1920, height: 1080, refresh_hz: 60 }
    }

    #[test]
    fn stream_names_are_plain() {
        assert!(StreamName::parse("monitor-q27g4zd").is_ok());
        for bad in ["", "Big", "a/b", "a b"] {
            assert!(StreamName::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn monitor_names_are_readable_and_unique() {
        let names: Vec<String> = monitor_streams(&[monitor(1, "Q27G4ZD"), monitor(2, "BenQ EX270QM"), monitor(28, "VDD by MTT"), monitor(3, "Dell"), monitor(4, "Dell"), monitor(5, "")])
            .into_iter()
            .map(|s| s.name.0)
            .collect();
        assert_eq!(names, ["monitor-q27g4zd", "monitor-benq-ex270qm", "monitor-vdd-by-mtt", "monitor-dell-3", "monitor-dell-4", "monitor-5"]);
    }
}
