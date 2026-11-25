//! Source -> Encoder -> Link: everything the sources share, from connecting to the receiver to StreamEnd.

use std::fmt;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow};
use clap::ValueEnum;
use glassdeck_proto::{Codec, Frame, Message, Mode, StreamInfo};

use crate::link::{Link, new_stream_id};

const STATS_INTERVAL: Duration = Duration::from_secs(1);
const QUEUED_FRAMES: usize = 4;
/// Encoded sources send this far ahead of presentation time, inside the receiver's playout delay.
const SEND_AHEAD: Duration = Duration::from_millis(100);

static STOP: AtomicBool = AtomicBool::new(false);

/// Ctrl+C asks the running source to end its stream cleanly, so the receiver gets StreamEnd.
/// A second Ctrl+C quits at once, in case a source is stuck.
pub fn stop_on_ctrl_c() -> Result<()> {
    ctrlc::set_handler(request_stop).context("installing the Ctrl+C handler")
}

pub fn request_stop() {
    if STOP.swap(true, Ordering::Relaxed) {
        std::process::exit(130);
    }
}

/// Sources poll this and stop as if their duration had run out.
pub fn stop_requested() -> bool {
    STOP.load(Ordering::Relaxed)
}

/// What produces the picture. Raw-frame sources go through an encoder; encoded sources never do.
pub enum Source {
    Frames(Box<dyn FrameSource>),
    Encoded(Box<dyn EncodedSource>),
}

/// Captures or renders raw frames and encodes them itself, on whatever GPU device it owns.
pub trait FrameSource {
    fn format(&self) -> VideoFormat;
    fn timing(&self) -> Timing;
    /// Runs until done or stopped, sending each encoded frame with its wall-clock capture time.
    fn run(self: Box<Self>, out: SyncSender<EncodedFrame>) -> Result<()>;
}

/// Already-encoded media, always paced: frames come with presentation times and the pipeline paces them.
pub trait EncodedSource {
    fn format(&self) -> VideoFormat;
    fn next_frame(&mut self) -> Result<Option<EncodedFrame>>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Timing {
    /// Mirroring: show on arrival, drop stale frames
    Live,
    /// Wallpapers: buffered and shown at their times
    Paced,
}

#[derive(Clone, Copy, Debug)]
pub struct VideoFormat {
    pub codec: Codec,
    pub width: u16,
    pub height: u16,
}

pub struct EncodedFrame {
    pub seq: u32,
    /// Wall-clock capture time from a frame source, presentation time from an encoded source
    pub time_us: u64,
    pub keyframe: bool,
    pub data: Vec<u8>,
    pub encode_time: Duration,
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

pub struct LinkOptions {
    pub to: SocketAddr,
    pub link_bytes_per_sec: Option<u64>,
}

/// How a frame's `time_us` becomes the time sent on the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StreamClock {
    /// Live: the capture time itself; the receiver adds the clock offset
    Wall,
    /// Paced frames: microseconds since the stream's first frame
    FromFirstFrame,
    /// Encoded media: presentation times as they are, since B-frames arrive out of order
    Presentation,
}

impl StreamClock {
    fn of(source: &Source) -> Self {
        match source {
            Source::Frames(frames) if frames.timing() == Timing::Live => Self::Wall,
            Source::Frames(_) => Self::FromFirstFrame,
            Source::Encoded(_) => Self::Presentation,
        }
    }

    fn wire_time_us(self, first_us: u64, time_us: u64) -> u64 {
        match self {
            Self::Wall | Self::Presentation => time_us,
            Self::FromFirstFrame => time_us.saturating_sub(first_us),
        }
    }
}

pub fn run(source: Source, options: LinkOptions) -> Result<()> {
    let format = match &source {
        Source::Frames(frames) => frames.format(),
        Source::Encoded(encoded) => encoded.format(),
    };
    let clock = StreamClock::of(&source);
    let mut link = Link::connect(options.to, options.link_bytes_per_sec)?;
    let mode = match clock {
        StreamClock::Wall => {
            let sample = link.sync_clock()?;
            println!(
                "Receiver clock is {:+.1} ms from ours (probe round trip {:.1} ms)",
                sample.offset_us() as f64 / 1000.0,
                sample.round_trip_us() as f64 / 1000.0
            );
            Mode::Live { clock_offset_us: sample.offset_us() }
        }
        StreamClock::FromFirstFrame | StreamClock::Presentation => Mode::Paced,
    };
    let info = StreamInfo { stream_id: new_stream_id(), codec: format.codec, mode, width: format.width, height: format.height };

    let (tx, rx) = mpsc::sync_channel(QUEUED_FRAMES);
    let network = thread::spawn(move || send_loop(link, info, clock, rx));
    let produced = match source {
        Source::Frames(frames) => frames.run(tx),
        Source::Encoded(encoded) => pace(encoded, tx),
    };
    let sent = network.join().map_err(|_| anyhow!("network thread panicked"))?;
    // A failed send ends the source too, so the send error is the one worth reporting.
    sent.context("sending to receiver")?;
    produced
}

fn pace(mut source: Box<dyn EncodedSource>, out: SyncSender<EncodedFrame>) -> Result<()> {
    let started = Instant::now();
    let mut first_us = None;
    while !stop_requested() {
        let Some(frame) = source.next_frame()? else { break };
        let first = *first_us.get_or_insert(frame.time_us);
        let due = started + Duration::from_micros(frame.time_us.saturating_sub(first));
        thread::sleep(due.saturating_duration_since(Instant::now() + SEND_AHEAD));
        if out.send(frame).is_err() {
            break;
        }
    }
    Ok(())
}

fn send_loop(mut link: Link, info: StreamInfo, clock: StreamClock, frames: Receiver<EncodedFrame>) -> Result<()> {
    link.send(&Message::StreamStart(info))?;
    let mut stats = SendStats::new(Instant::now());
    let mut first_us = None;

    for frame in frames {
        let first = *first_us.get_or_insert(frame.time_us);
        let sent = SentFrame { bytes: frame.data.len(), keyframe: frame.keyframe, encode_time: frame.encode_time };
        let link_wait = link.send(&Message::Frame(Frame {
            stream_id: info.stream_id,
            seq: frame.seq,
            time_us: clock.wire_time_us(first, frame.time_us),
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
    link.send(&Message::StreamEnd { stream_id: info.stream_id })?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_frames_keep_their_capture_time() {
        assert_eq!(StreamClock::Wall.wire_time_us(1_000, 5_000), 5_000);
    }

    #[test]
    fn paced_frames_count_from_the_first_frame() {
        assert_eq!(StreamClock::FromFirstFrame.wire_time_us(1_000, 1_000), 0);
        assert_eq!(StreamClock::FromFirstFrame.wire_time_us(1_000, 17_667), 16_667);
    }

    #[test]
    fn encoded_frames_keep_presentation_times_even_before_the_first() {
        assert_eq!(StreamClock::Presentation.wire_time_us(66_666, 33_333), 33_333);
    }
}
