use std::fmt;
use std::net::SocketAddr;
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow};
use clap::ValueEnum;
use glassdeck_proto::{Codec, Frame, Message, Mode, StreamInfo, unix_micros};
use windows_capture::capture::{Context, GraphicsCaptureApiHandler};
use windows_capture::frame::Frame as CapturedFrame;
use windows_capture::graphics_capture_api::InternalCaptureControl;
use windows_capture::settings::{
    ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
    MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
};

use crate::gpu::GpuEncoder;
use crate::transport::{Transport, new_stream_id};
use crate::monitors::FoundMonitor;
use crate::software::SoftwareEncoder;

const STATS_INTERVAL: Duration = Duration::from_secs(1);
const QUEUED_FRAMES: usize = 4;

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum EncoderKind {
    /// Hardware H.264 or HEVC through Media Foundation, frames stay on the GPU
    Gpu,
    /// openh264 on the CPU, H.264 only
    Software,
}

/// Codec choice on the command line.
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum VideoCodec {
    /// Fewer bits for the same picture, and the RK3566 decodes it faster
    Hevc,
    H264,
}

impl From<VideoCodec> for Codec {
    fn from(codec: VideoCodec) -> Self {
        match codec {
            VideoCodec::Hevc => Codec::Hevc,
            VideoCodec::H264 => Codec::H264,
        }
    }
}

pub struct StreamOptions {
    pub to: SocketAddr,
    pub duration: Option<Duration>,
    pub encoder: EncoderKind,
    pub codec: Codec,
    pub bitrate: u32,
    pub fps: u32,
    pub keyframe_every: u32,
    pub vbv_frames: Option<u32>,
    pub link_bytes_per_sec: Option<u64>,
}

#[derive(Clone, Copy)]
pub struct EncoderSettings {
    pub width: u32,
    pub height: u32,
    pub codec: Codec,
    pub fps: u32,
    pub bitrate: u32,
    pub keyframe_every: u32,
    /// Rate-control buffer in frames of average size; small values keep keyframes small.
    pub vbv_frames: Option<u32>,
}

pub struct EncodedFrame {
    pub seq: u32,
    pub capture_us: u64,
    pub keyframe: bool,
    pub data: Vec<u8>,
    pub encode_time: Duration,
}

struct Flags {
    tx: SyncSender<EncodedFrame>,
    kind: EncoderKind,
    settings: EncoderSettings,
    duration: Option<Duration>,
}

enum Backend {
    Gpu(GpuEncoder),
    Software(SoftwareEncoder),
}

struct Streamer {
    backend: Backend,
    size: (u32, u32),
    duration: Option<Duration>,
    started: Instant,
}

type BoxError = Box<dyn std::error::Error + Send + Sync>;

impl GraphicsCaptureApiHandler for Streamer {
    type Flags = Flags;
    type Error = BoxError;

    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        let Flags { tx, kind, settings, duration } = ctx.flags;
        let backend = match kind {
            EncoderKind::Gpu => {
                let encoder = GpuEncoder::start(&ctx.device, &ctx.device_context, settings, tx)?;
                println!("Encoder: {}", encoder.name);
                Backend::Gpu(encoder)
            }
            EncoderKind::Software => {
                println!("Encoder: openh264 (software)");
                Backend::Software(SoftwareEncoder::new(settings, tx)?)
            }
        };
        Ok(Self {
            backend,
            size: (settings.width, settings.height),
            duration,
            started: Instant::now(),
        })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut CapturedFrame,
        control: InternalCaptureControl,
    ) -> Result<(), Self::Error> {
        if (frame.width(), frame.height()) != self.size {
            return Err("monitor resolution changed during the stream".into());
        }
        let capture_us = unix_micros();
        match &mut self.backend {
            Backend::Gpu(encoder) => encoder.submit(frame.as_raw_texture(), capture_us)?,
            Backend::Software(encoder) => encoder.submit(frame, capture_us)?,
        }
        if self.duration.is_some_and(|d| self.started.elapsed() >= d) {
            control.stop();
        }
        Ok(())
    }
}

pub fn run(found: FoundMonitor, options: StreamOptions) -> Result<()> {
    let mut transport = Transport::connect(options.to, options.link_bytes_per_sec)?;
    let clock = transport.sync_clock()?;
    println!(
        "Receiver clock is {:+.1} ms from ours (probe round trip {:.1} ms)",
        clock.offset_us() as f64 / 1000.0,
        clock.round_trip_us() as f64 / 1000.0
    );
    let info = StreamInfo {
        stream_id: new_stream_id(),
        codec: options.codec,
        mode: Mode::Live { clock_offset_us: clock.offset_us() },
        width: u16::try_from(found.info.width).context("monitor too wide")?,
        height: u16::try_from(found.info.height).context("monitor too tall")?,
    };

    let (tx, rx) = mpsc::sync_channel(QUEUED_FRAMES);
    let network = thread::spawn(move || send_loop(transport, info, rx));

    let flags = Flags {
        tx,
        kind: options.encoder,
        settings: EncoderSettings {
            width: found.info.width,
            height: found.info.height,
            codec: options.codec,
            fps: options.fps,
            bitrate: options.bitrate,
            keyframe_every: options.keyframe_every,
            vbv_frames: options.vbv_frames,
        },
        duration: options.duration,
    };
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

    let capture = Streamer::start(settings);
    let network = network.join().expect("network thread panicked");
    network.context("sending to receiver")?;
    capture.map_err(|e| anyhow!("capture failed: {e}"))
}

fn send_loop(mut transport: Transport, info: StreamInfo, rx: Receiver<EncodedFrame>) -> Result<()> {
    transport.send(&Message::StreamStart(info))?;
    let mut stats = SendStats::new(Instant::now());

    for frame in rx {
        let sent = SentFrame { bytes: frame.data.len(), keyframe: frame.keyframe, encode_time: frame.encode_time };
        let link_wait = transport.send(&Message::Frame(Frame {
            stream_id: info.stream_id,
            seq: frame.seq,
            time_us: frame.capture_us,
            keyframe: frame.keyframe,
            payload: frame.data,
        }))?;

        stats = stats.record(sent, link_wait);
        let now = Instant::now();
        if now - stats.since >= STATS_INTERVAL {
            println!("{stats}");
            stats = SendStats::new(now);
        }
    }
    transport.send(&Message::StreamEnd { stream_id: info.stream_id })?;
    Ok(())
}

#[derive(Clone, Copy)]
struct SentFrame {
    bytes: usize,
    keyframe: bool,
    encode_time: Duration,
}

#[derive(Clone, Copy)]
struct SendStats {
    since: Instant,
    frames: u32,
    keyframes: u32,
    bytes: u64,
    largest_frame: usize,
    encode_total: Duration,
    link_total: Duration,
}

impl SendStats {
    fn new(since: Instant) -> Self {
        Self {
            since,
            frames: 0,
            keyframes: 0,
            bytes: 0,
            largest_frame: 0,
            encode_total: Duration::ZERO,
            link_total: Duration::ZERO,
        }
    }

    fn record(self, sent: SentFrame, link_wait: Duration) -> Self {
        Self {
            frames: self.frames + 1,
            keyframes: self.keyframes + sent.keyframe as u32,
            bytes: self.bytes + sent.bytes as u64,
            largest_frame: self.largest_frame.max(sent.bytes),
            encode_total: self.encode_total + sent.encode_time,
            link_total: self.link_total + link_wait,
            ..self
        }
    }
}

impl fmt::Display for SendStats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let secs = self.since.elapsed().as_secs_f64();
        let per_frame = |total: Duration| total.as_secs_f64() * 1000.0 / self.frames.max(1) as f64;
        write!(
            f,
            "send: {:5.1} fps | {:5.1} Mbps | encode {:5.2} ms | link {:5.2} ms | largest {:4} KB | keyframes {}",
            self.frames as f64 / secs,
            self.bytes as f64 * 8.0 / secs / 1e6,
            per_frame(self.encode_total),
            per_frame(self.link_total),
            self.largest_frame / 1024,
            self.keyframes,
        )
    }
}
