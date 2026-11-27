use std::sync::mpsc::SyncSender;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow};
use clap::ValueEnum;
use glassdeck_proto::unix_micros;
use windows_capture::capture::{Context, GraphicsCaptureApiHandler};
use windows_capture::frame::Frame as CapturedFrame;
use windows_capture::graphics_capture_api::InternalCaptureControl;
use windows_capture::settings::{
    ColorFormat, GraphicsCaptureItemType, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
    MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
};

use crate::gpu::GpuEncoder;
use crate::monitors::FoundMonitor;
use crate::pipeline::{EncodedFrame, EncoderSettings, FrameSource, Timing, VideoFormat, stop_requested};

const POLL: Duration = Duration::from_millis(50);
use crate::software::SoftwareEncoder;

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EncoderKind {
    /// Hardware H.264 or HEVC through Media Foundation, frames stay on the GPU
    Gpu,
    /// openh264 on the CPU, H.264 only
    Software,
}

/// A monitor captured with Windows.Graphics.Capture: the live mirroring source.
pub struct DesktopSource {
    monitor: FoundMonitor,
    format: VideoFormat,
    encoder: EncoderKind,
    settings: EncoderSettings,
    duration: Option<Duration>,
}

impl DesktopSource {
    pub fn new(monitor: FoundMonitor, encoder: EncoderKind, settings: EncoderSettings, duration: Option<Duration>) -> Result<Self> {
        let format = VideoFormat {
            codec: settings.codec,
            width: u16::try_from(settings.width).context("monitor too wide")?,
            height: u16::try_from(settings.height).context("monitor too tall")?,
        };
        Ok(Self { monitor, format, encoder, settings, duration })
    }
}

impl FrameSource for DesktopSource {
    fn format(&self) -> VideoFormat {
        self.format
    }

    fn timing(&self) -> Timing {
        Timing::Live
    }

    fn run(self: Box<Self>, out: SyncSender<EncodedFrame>) -> Result<()> {
        let captured = (self.monitor.info.width, self.monitor.info.height);
        capture_live(self.monitor.monitor, captured, self.encoder, self.settings, self.duration, out)
    }
}

/// Captures a monitor or a window with Windows.Graphics.Capture and encodes every frame until stopped.
/// Frames are `captured` in size; the GPU encoder scales them to the settings' size.
pub fn capture_live<T: TryInto<GraphicsCaptureItemType> + Send + 'static>(
    target: T,
    captured: (u32, u32),
    encoder: EncoderKind,
    settings: EncoderSettings,
    duration: Option<Duration>,
    out: SyncSender<EncodedFrame>,
) -> Result<()> {
    let interval = capture_interval(settings.fps);
    let flags = Flags { tx: out, kind: encoder, settings, captured };
    let settings = Settings::new(
        target,
        CursorCaptureSettings::WithoutCursor,
        DrawBorderSettings::WithoutBorder,
        SecondaryWindowSettings::Default,
        MinimumUpdateIntervalSettings::Custom(interval),
        DirtyRegionSettings::Default,
        ColorFormat::Bgra8,
        flags,
    );
    // Stopped from here rather than from the frame callback: a still picture delivers no frames.
    let control = Capture::start_free_threaded(settings).map_err(|e| anyhow!("starting capture: {e}"))?;
    let begun = Instant::now();
    while !control.is_finished() {
        if stop_requested() || duration.is_some_and(|d| begun.elapsed() >= d) {
            return control.stop().map_err(|e| anyhow!("capture failed: {e}"));
        }
        thread::sleep(POLL);
    }
    control.wait().map_err(|e| anyhow!("capture failed: {e}"))
}

/// A little under one frame at `fps`, so a fast monitor's refresh ticks that land just short of a
/// frame's length still count, and capture keeps up with the stream instead of the monitor.
fn capture_interval(fps: u32) -> Duration {
    Duration::from_secs(1) * 9 / (fps.max(1) * 10)
}

struct Flags {
    tx: SyncSender<EncodedFrame>,
    kind: EncoderKind,
    settings: EncoderSettings,
    captured: (u32, u32),
}

enum Backend {
    Gpu(GpuEncoder),
    Software(Box<SoftwareEncoder>),
}

struct Capture {
    backend: Backend,
    size: (u32, u32),
}

type BoxError = Box<dyn std::error::Error + Send + Sync>;

impl GraphicsCaptureApiHandler for Capture {
    type Flags = Flags;
    type Error = BoxError;

    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        let Flags { tx, kind, settings, captured } = ctx.flags;
        let backend = match kind {
            EncoderKind::Gpu => {
                let encoder = GpuEncoder::start(&ctx.device, &ctx.device_context, settings, captured, tx)?;
                println!("Encoder: {}", encoder.name);
                Backend::Gpu(encoder)
            }
            EncoderKind::Software => {
                println!("Encoder: openh264 (software)");
                Backend::Software(Box::new(SoftwareEncoder::new(settings, tx)?))
            }
        };
        Ok(Self { backend, size: captured })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut CapturedFrame,
        _control: InternalCaptureControl,
    ) -> Result<(), Self::Error> {
        if (frame.width(), frame.height()) != self.size {
            return Err(format!("captured {}x{}, expected {}x{}", frame.width(), frame.height(), self.size.0, self.size.1).into());
        }
        let capture_us = unix_micros();
        match &mut self.backend {
            Backend::Gpu(encoder) => encoder.submit(frame.as_raw_texture(), capture_us)?,
            Backend::Software(encoder) => encoder.submit(frame, capture_us)?,
        }
        Ok(())
    }
}

/// One frame of a monitor as it is now: BGRA rows without padding, and its size. For a preview of a
/// stream nobody is playing yet, so nothing is encoded.
pub fn capture_still(monitor: windows_capture::monitor::Monitor) -> Result<(Vec<u8>, (u32, u32))> {
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let settings = Settings::new(
        monitor,
        CursorCaptureSettings::WithoutCursor,
        DrawBorderSettings::WithoutBorder,
        SecondaryWindowSettings::Default,
        MinimumUpdateIntervalSettings::Default,
        DirtyRegionSettings::Default,
        ColorFormat::Bgra8,
        tx,
    );
    let control = StillCapture::start_free_threaded(settings).map_err(|e| anyhow!("starting capture: {e}"))?;
    let still = rx.recv_timeout(STILL_WAIT).map_err(|_| anyhow!("the monitor gave no frame within {STILL_WAIT:?}"));
    let stopped = control.stop().map_err(|e| anyhow!("stopping capture: {e}"));
    let still = still?;
    stopped?;
    Ok(still)
}

/// A monitor that shows nothing new still gives its first frame at once; this is for one that's asleep.
const STILL_WAIT: Duration = Duration::from_secs(2);

struct StillCapture {
    tx: SyncSender<(Vec<u8>, (u32, u32))>,
    scratch: Vec<u8>,
}

impl GraphicsCaptureApiHandler for StillCapture {
    type Flags = SyncSender<(Vec<u8>, (u32, u32))>;
    type Error = BoxError;

    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        Ok(Self { tx: ctx.flags, scratch: Vec::new() })
    }

    fn on_frame_arrived(&mut self, frame: &mut CapturedFrame, control: InternalCaptureControl) -> Result<(), Self::Error> {
        let size = (frame.width(), frame.height());
        let buffer = frame.buffer()?;
        let bgra = buffer.as_nopadding_buffer(&mut self.scratch).to_vec();
        // Only the first frame is wanted; a full channel means it's already on its way.
        let _ = self.tx.try_send((bgra, size));
        control.stop();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::monitors::{self, MonitorQuery};

    use super::*;

    #[test]
    fn a_still_is_one_whole_frame_of_the_monitor() {
        let Some(info) = monitors::enumerate().ok().and_then(|all| all.into_iter().next()) else {
            eprintln!("skipped: no monitor to capture");
            return;
        };
        let found = monitors::find(&MonitorQuery::Index(info.index)).unwrap();
        let (bgra, (width, height)) = capture_still(found.monitor).unwrap();
        assert_eq!((width, height), (info.width, info.height), "the monitor's own size");
        assert_eq!(bgra.len(), (width * height * 4) as usize, "every pixel, with no row padding");
    }
}
