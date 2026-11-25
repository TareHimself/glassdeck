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
    ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
    MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
};

use crate::gpu::GpuEncoder;
use crate::monitors::FoundMonitor;
use crate::pipeline::{EncodedFrame, EncoderSettings, FrameSource, Timing, VideoFormat, stop_requested};

const POLL: Duration = Duration::from_millis(50);
use crate::software::SoftwareEncoder;

#[derive(Clone, Copy, Debug, ValueEnum)]
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
        let flags = Flags { tx: out, kind: self.encoder, settings: self.settings };
        let settings = Settings::new(
            self.monitor.monitor,
            CursorCaptureSettings::WithoutCursor,
            DrawBorderSettings::WithoutBorder,
            SecondaryWindowSettings::Default,
            MinimumUpdateIntervalSettings::Default,
            DirtyRegionSettings::Default,
            ColorFormat::Bgra8,
            flags,
        );
        // Stopped from here rather than from the frame callback: a still screen delivers no frames.
        let control = Capture::start_free_threaded(settings).map_err(|e| anyhow!("starting capture: {e}"))?;
        let begun = Instant::now();
        while !control.is_finished() {
            if stop_requested() || self.duration.is_some_and(|d| begun.elapsed() >= d) {
                return control.stop().map_err(|e| anyhow!("capture failed: {e}"));
            }
            thread::sleep(POLL);
        }
        control.wait().map_err(|e| anyhow!("capture failed: {e}"))
    }
}

struct Flags {
    tx: SyncSender<EncodedFrame>,
    kind: EncoderKind,
    settings: EncoderSettings,
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
        let Flags { tx, kind, settings } = ctx.flags;
        let backend = match kind {
            EncoderKind::Gpu => {
                let encoder = GpuEncoder::start(&ctx.device, &ctx.device_context, settings, tx)?;
                println!("Encoder: {}", encoder.name);
                Backend::Gpu(encoder)
            }
            EncoderKind::Software => {
                println!("Encoder: openh264 (software)");
                Backend::Software(Box::new(SoftwareEncoder::new(settings, tx)?))
            }
        };
        Ok(Self {
            backend,
            size: (settings.width, settings.height),
        })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut CapturedFrame,
        _control: InternalCaptureControl,
    ) -> Result<(), Self::Error> {
        if (frame.width(), frame.height()) != self.size {
            return Err("monitor resolution changed during the stream".into());
        }
        let capture_us = unix_micros();
        match &mut self.backend {
            Backend::Gpu(encoder) => encoder.submit(frame.as_raw_texture(), capture_us)?,
            Backend::Software(encoder) => encoder.submit(frame, capture_us)?,
        }
        Ok(())
    }
}
