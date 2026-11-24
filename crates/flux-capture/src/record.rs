use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use windows_capture::capture::{Context, GraphicsCaptureApiHandler};
use windows_capture::encoder::{
    AudioSettingsBuilder, ContainerSettingsBuilder, VideoEncoder, VideoSettingsBuilder,
    VideoSettingsSubType,
};
use windows_capture::frame::Frame;
use windows_capture::graphics_capture_api::InternalCaptureControl;
use windows_capture::settings::{
    ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
    MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
};

use crate::monitors::FoundMonitor;

pub struct RecordOptions {
    pub out: PathBuf,
    pub duration: Duration,
    pub bitrate: u32,
    pub fps: u32,
}

struct Flags {
    options: RecordOptions,
    width: u32,
    height: u32,
}

struct Recorder {
    encoder: Option<VideoEncoder>,
    duration: Duration,
    started: Instant,
    frames: u64,
}

type BoxError = Box<dyn std::error::Error + Send + Sync>;

impl GraphicsCaptureApiHandler for Recorder {
    type Flags = Flags;
    type Error = BoxError;

    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        let Flags { options, width, height } = ctx.flags;
        let video = VideoSettingsBuilder::new(width, height)
            .sub_type(VideoSettingsSubType::H264)
            .bitrate(options.bitrate)
            .frame_rate(options.fps);
        let encoder = VideoEncoder::new(
            video,
            AudioSettingsBuilder::default().disabled(true),
            ContainerSettingsBuilder::default(),
            &options.out,
        )?;
        Ok(Self {
            encoder: Some(encoder),
            duration: options.duration,
            started: Instant::now(),
            frames: 0,
        })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut Frame,
        control: InternalCaptureControl,
    ) -> Result<(), Self::Error> {
        let Some(encoder) = self.encoder.as_mut() else {
            return Ok(());
        };
        encoder.send_frame(frame)?;
        self.frames += 1;

        if self.started.elapsed() >= self.duration {
            self.encoder.take().expect("encoder present").finish()?;
            let secs = self.started.elapsed().as_secs_f64();
            println!(
                "Wrote {} frames in {secs:.1}s ({:.1} fps)",
                self.frames,
                self.frames as f64 / secs
            );
            control.stop();
        }
        Ok(())
    }

    fn on_closed(&mut self) -> Result<(), Self::Error> {
        match self.encoder.take() {
            Some(encoder) => {
                encoder.finish()?;
                Err("monitor closed before the recording finished".into())
            }
            None => Ok(()),
        }
    }
}

pub fn run(found: FoundMonitor, options: RecordOptions) -> Result<()> {
    let flags = Flags { options, width: found.info.width, height: found.info.height };
    let settings = Settings::new(
        found.monitor,
        CursorCaptureSettings::WithoutCursor,
        DrawBorderSettings::WithoutBorder,
        SecondaryWindowSettings::Default,
        MinimumUpdateIntervalSettings::Default,
        DirtyRegionSettings::Default,
        ColorFormat::Bgra8,
        flags,
    );
    Recorder::start(settings).map_err(|e| anyhow!("capture failed: {e}"))
}
