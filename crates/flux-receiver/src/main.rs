mod overlay;
mod stats;

use std::io::{ErrorKind, Read};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use clap::Parser;
use flux_proto::{FrameHeader, HEADER_LEN, unix_micros};
use fontdue::{Font, FontSettings};
use minifb::{Key, Window, WindowOptions};
use openh264::decoder::Decoder;
use openh264::formats::YUVSource;

use overlay::Overlay;
use stats::{FrameTimes, LatencyStats};

const STATS_INTERVAL: Duration = Duration::from_secs(1);
const IDLE_POLL: Duration = Duration::from_millis(1);
const UNTHROTTLED: usize = 0;

#[derive(Parser)]
#[command(about = "Stand-in for the keyboard: decode the stream and show it with key labels")]
struct Cli {
    #[arg(long, default_value = "127.0.0.1:7878")]
    listen: SocketAddr,
    #[arg(long, default_value = r"C:\Windows\Fonts\segoeuib.ttf")]
    font: PathBuf,
    /// Show the stream without key labels
    #[arg(long)]
    no_labels: bool,
}

struct DecodedFrame {
    pixels: Vec<u32>,
    width: usize,
    height: usize,
    capture_us: u64,
    received_us: u64,
    decoded_us: u64,
}

enum StreamState {
    Running,
    Ended(Result<()>),
}

/// Holds only the newest decoded frame, so a slow display skips frames instead of falling behind.
struct Mailbox {
    slot: Mutex<Slot>,
}

struct Slot {
    frame: Option<DecodedFrame>,
    replaced: u32,
    state: StreamState,
}

impl Mailbox {
    fn new() -> Self {
        Self { slot: Mutex::new(Slot { frame: None, replaced: 0, state: StreamState::Running }) }
    }

    fn put(&self, frame: DecodedFrame) {
        let mut slot = self.slot.lock().expect("mailbox poisoned");
        if slot.frame.replace(frame).is_some() {
            slot.replaced += 1;
        }
    }

    fn end(&self, result: Result<()>) {
        self.slot.lock().expect("mailbox poisoned").state = StreamState::Ended(result);
    }

    fn take(&self) -> (Option<DecodedFrame>, u32, Option<Result<()>>) {
        let mut slot = self.slot.lock().expect("mailbox poisoned");
        let ended = match std::mem::replace(&mut slot.state, StreamState::Running) {
            StreamState::Running => None,
            StreamState::Ended(result) => Some(result),
        };
        (slot.frame.take(), std::mem::take(&mut slot.replaced), ended)
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let font = if cli.no_labels { None } else { Some(load_font(&cli.font)?) };

    let listener = TcpListener::bind(cli.listen).with_context(|| format!("listening on {}", cli.listen))?;
    println!("Waiting for a stream on {}", cli.listen);
    let (socket, peer) = listener.accept()?;
    socket.set_nodelay(true)?;
    println!("Streaming from {peer}");

    let mailbox = Arc::new(Mailbox::new());
    let producer = Arc::clone(&mailbox);
    thread::spawn(move || {
        let result = receive_loop(socket, &producer);
        producer.end(result);
    });

    display_loop(&mailbox, font.as_ref())
}

fn load_font(path: &PathBuf) -> Result<Font> {
    let bytes = std::fs::read(path).with_context(|| format!("reading font {}", path.display()))?;
    Font::from_bytes(bytes, FontSettings::default()).map_err(|e| anyhow!("parsing font: {e}"))
}

fn receive_loop(mut socket: TcpStream, mailbox: &Mailbox) -> Result<()> {
    let mut decoder = Decoder::new()?;
    let mut header = [0u8; HEADER_LEN];
    let mut payload = Vec::new();
    let mut rgba = Vec::new();

    loop {
        match socket.read_exact(&mut header) {
            Err(e) if e.kind() == ErrorKind::UnexpectedEof => return Ok(()),
            other => other.context("reading frame header")?,
        }
        let frame = FrameHeader::decode(&header)?;
        payload.resize(frame.payload_len as usize, 0);
        socket.read_exact(&mut payload).context("reading frame payload")?;
        let received_us = unix_micros();

        let Some(yuv) = decoder.decode(&payload).context("decoding frame")? else {
            continue;
        };
        let (width, height) = yuv.dimensions();
        rgba.resize(width * height * 4, 0);
        yuv.write_rgba8(&mut rgba);
        let pixels = rgba
            .chunks_exact(4)
            .map(|p| u32::from_be_bytes([0, p[0], p[1], p[2]]))
            .collect();

        mailbox.put(DecodedFrame {
            pixels,
            width,
            height,
            capture_us: frame.capture_us,
            received_us,
            decoded_us: unix_micros(),
        });
    }
}

fn display_loop(mailbox: &Mailbox, font: Option<&Font>) -> Result<()> {
    let mut window: Option<Window> = None;
    let mut overlay: Option<Overlay> = None;
    let mut stats = LatencyStats::default();
    let mut stats_since = Instant::now();

    loop {
        let (frame, replaced, ended) = mailbox.take();
        if let Some(result) = ended {
            println!("Stream ended");
            return result;
        }
        stats = stats.replaced(replaced);

        match frame {
            Some(mut frame) => {
                if let Some(font) = font {
                    if !overlay.as_ref().is_some_and(|o| o.fits(frame.width, frame.height)) {
                        overlay = Some(Overlay::build(font, frame.width, frame.height));
                    }
                    overlay.as_ref().expect("overlay built").composite_onto(&mut frame.pixels);
                }
                let win = match window.as_mut() {
                    Some(win) => win,
                    None => window.insert(open_window(frame.width, frame.height)?),
                };
                win.update_with_buffer(&frame.pixels, frame.width, frame.height)?;
                stats = stats.record(FrameTimes {
                    capture_us: frame.capture_us,
                    received_us: frame.received_us,
                    decoded_us: frame.decoded_us,
                    displayed_us: unix_micros(),
                });
            }
            None => {
                if let Some(win) = window.as_mut() {
                    win.update();
                }
                thread::sleep(IDLE_POLL);
            }
        }

        if let Some(win) = window.as_mut() {
            if !win.is_open() || win.is_key_down(Key::Escape) {
                return Ok(());
            }
            if stats_since.elapsed() >= STATS_INTERVAL {
                let summary = stats.summary(stats_since.elapsed().as_secs_f64());
                println!("recv: {summary}");
                win.set_title(&format!("flux-receiver | {summary}"));
                stats = LatencyStats::default();
                stats_since = Instant::now();
            }
        }
    }
}

fn open_window(width: usize, height: usize) -> Result<Window> {
    let mut window = Window::new("flux-receiver", width, height, WindowOptions::default())?;
    window.set_target_fps(UNTHROTTLED);
    Ok(window)
}
