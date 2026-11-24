use std::fmt;
use std::io::Write;
use std::net::{SocketAddr, TcpStream};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow};
use clap::ValueEnum;
use glassdeck_proto::{FrameHeader, HEADER_LEN, unix_micros};
use windows_capture::capture::{Context, GraphicsCaptureApiHandler};
use windows_capture::frame::Frame;
use windows_capture::graphics_capture_api::InternalCaptureControl;
use windows_capture::settings::{
    ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
    MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
};

use crate::gpu::GpuEncoder;
use crate::link::SimulatedLink;
use crate::monitors::FoundMonitor;
use crate::software::SoftwareEncoder;

const STATS_INTERVAL: Duration = Duration::from_secs(1);
const QUEUED_FRAMES: usize = 4;

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum EncoderKind {
    /// Hardware H.264 through Media Foundation, frames stay on the GPU
    Gpu,
    /// openh264 on the CPU
    Software,
}

pub struct StreamOptions {
    pub to: SocketAddr,
    pub duration: Option<Duration>,
    pub encoder: EncoderKind,
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
        frame: &mut Frame,
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
    let socket = TcpStream::connect(options.to)
        .with_context(|| format!("connecting to receiver at {}", options.to))?;
    socket.set_nodelay(true)?;

    let (tx, rx) = mpsc::sync_channel(QUEUED_FRAMES);
    let link = options.link_bytes_per_sec;
    let network = thread::spawn(move || send_loop(socket, rx, link));

    let flags = Flags {
        tx,
        kind: options.encoder,
        settings: EncoderSettings {
            width: found.info.width,
            height: found.info.height,
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

fn send_loop(mut socket: TcpStream, rx: Receiver<EncodedFrame>, link_rate: Option<u64>) -> Result<()> {
    let mut link = link_rate.map(|rate| SimulatedLink::new(rate, Instant::now()));
    let mut stats = SendStats::new(Instant::now());

    for frame in rx {
        let link_wait = match link {
            Some(current) => {
                let (next, done) = current.transfer(HEADER_LEN + frame.data.len(), Instant::now());
                link = Some(next);
                let wait = done.saturating_duration_since(Instant::now());
                thread::sleep(wait);
                wait
            }
            None => Duration::ZERO,
        };

        let header = FrameHeader {
            seq: frame.seq,
            capture_us: frame.capture_us,
            keyframe: frame.keyframe,
            payload_len: frame.data.len() as u32,
        };
        socket.write_all(&header.encode())?;
        socket.write_all(&frame.data)?;

        stats = stats.record(&frame, link_wait);
        let now = Instant::now();
        if now - stats.since >= STATS_INTERVAL {
            println!("{stats}");
            stats = SendStats::new(now);
        }
    }
    Ok(())
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

    fn record(self, frame: &EncodedFrame, link_wait: Duration) -> Self {
        Self {
            frames: self.frames + 1,
            keyframes: self.keyframes + frame.keyframe as u32,
            bytes: self.bytes + frame.data.len() as u64,
            largest_frame: self.largest_frame.max(frame.data.len()),
            encode_total: self.encode_total + frame.encode_time,
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
