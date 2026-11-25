mod overlay;
mod playout;
mod stats;

#[cfg(target_os = "linux")]
#[path = "linux/mod.rs"]
mod platform;
#[cfg(windows)]
#[path = "windows/mod.rs"]
mod platform;

use std::io::Write;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use clap::Parser;
use fontdue::{Font, FontSettings};
use glassdeck_proto::{Action, Message, Mode, SessionState, StreamInfo, step, unix_micros};

use platform::{Decoder, Display, Picture};
use playout::PlayoutClock;
use stats::{FrameTimes, LatencyStats};

const STATS_INTERVAL: Duration = Duration::from_secs(1);
const IDLE_POLL: Duration = Duration::from_millis(1);

#[derive(Parser)]
#[command(about = "Stand-in for the keyboard: decode streams and show them with key labels")]
struct Cli {
    #[arg(long, default_value = platform::DEFAULT_LISTEN)]
    listen: SocketAddr,
    #[arg(long, default_value = platform::DEFAULT_FONT)]
    font: PathBuf,
    /// Show the stream without key labels
    #[arg(long)]
    no_labels: bool,
}

enum Timing {
    Live { capture_us: u64, received_us: u64, decoded_us: u64 },
    Paced { late_us: u64 },
}

struct ShownFrame {
    picture: Picture,
    timing: Timing,
}

/// Holds only the newest frame, so a slow display skips frames instead of falling behind.
struct Mailbox {
    slot: Mutex<(Option<ShownFrame>, u32)>,
}

impl Mailbox {
    fn new() -> Self {
        Self { slot: Mutex::new((None, 0)) }
    }

    fn put(&self, frame: ShownFrame) {
        let mut slot = self.slot.lock().expect("mailbox poisoned");
        if slot.0.replace(frame).is_some() {
            slot.1 += 1;
        }
    }

    fn take(&self) -> (Option<ShownFrame>, u32) {
        let mut slot = self.slot.lock().expect("mailbox poisoned");
        (slot.0.take(), std::mem::take(&mut slot.1))
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let font = if cli.no_labels { None } else { Some(load_font(&cli.font)?) };
    let display = Display::open(font)?;
    let listener = TcpListener::bind(cli.listen).with_context(|| format!("listening on {}", cli.listen))?;
    println!("Waiting for streams on {}", cli.listen);

    let mailbox = Arc::new(Mailbox::new());
    let producer = Arc::clone(&mailbox);
    thread::Builder::new().name("receive".into()).spawn(move || accept_loop(listener, &producer))?;
    display_loop(&mailbox, display)
}

fn load_font(path: &PathBuf) -> Result<Font> {
    let bytes = std::fs::read(path).with_context(|| format!("reading font {}", path.display()))?;
    Font::from_bytes(bytes, FontSettings::default()).map_err(|e| anyhow!("parsing font: {e}"))
}

/// The receiver is a server: one sender at a time, and a broken connection is reported, not fatal.
fn accept_loop(listener: TcpListener, mailbox: &Mailbox) {
    if let Err(e) = platform::init_decoder_thread() {
        eprintln!("error: starting the decoder: {e:#}");
        std::process::exit(1);
    }
    for connection in listener.incoming() {
        let result = connection
            .context("accepting connection")
            .and_then(|socket| serve(socket, mailbox));
        match result {
            Ok(()) => println!("Sender disconnected"),
            Err(e) => eprintln!("error: {e:#}"),
        }
    }
}

struct Playback {
    info: StreamInfo,
    decoder: Decoder,
    clock: Option<PlayoutClock>,
}

fn serve(mut socket: TcpStream, mailbox: &Mailbox) -> Result<()> {
    socket.set_nodelay(true)?;
    println!("Sender connected from {}", socket.peer_addr()?);
    let mut state = SessionState::Idle;
    let mut playback: Option<Playback> = None;

    while let Some(message) = Message::read_from(&mut socket)? {
        let received_us = unix_micros();
        let (next, action) = step(state, &message)?;
        state = next;
        match action {
            Action::Begin(info) => {
                let decoder = Decoder::new(&info)?;
                println!(
                    "Stream {}: {:?} {}x{} {:?} via {}",
                    info.stream_id, info.codec, info.width, info.height, info.mode, decoder.name
                );
                playback = Some(Playback { info, decoder, clock: None });
            }
            Action::Decode(frame) => {
                let playback = playback.as_mut().expect("session is streaming");
                for picture in playback.decoder.decode(&frame.payload, frame.time_us)? {
                    let timing = match playback.info.mode {
                        Mode::Live { clock_offset_us } => Timing::Live {
                            capture_us: picture.time_us.saturating_add_signed(clock_offset_us),
                            received_us,
                            decoded_us: unix_micros(),
                        },
                        Mode::Paced => {
                            let clock = *playback.clock.get_or_insert(PlayoutClock::start(picture.time_us, unix_micros()));
                            Timing::Paced { late_us: wait_until(clock.due_us(picture.time_us)) }
                        }
                    };
                    mailbox.put(ShownFrame { picture, timing });
                }
            }
            Action::End => {
                println!("Stream ended");
                playback = None;
            }
            Action::AnswerProbe { probe_sent_us } => {
                let reply = Message::ClockReply { probe_sent_us, received_us };
                socket.write_all(&reply.encode()).context("answering a clock probe")?;
            }
        }
    }
    Ok(())
}

/// Sleeps until `due_us`; returns how late we already were if it had passed.
fn wait_until(due_us: u64) -> u64 {
    let now = unix_micros();
    match due_us.checked_sub(now) {
        Some(ahead) => {
            thread::sleep(Duration::from_micros(ahead));
            0
        }
        None => now - due_us,
    }
}

fn display_loop(mailbox: &Mailbox, mut display: Display) -> Result<()> {
    let mut stats = LatencyStats::default();
    let mut stats_since = Instant::now();

    while display.is_open() {
        let (frame, replaced) = mailbox.take();
        stats = stats.replaced(replaced);

        match frame {
            Some(ShownFrame { picture, timing }) => {
                display.show(picture)?;
                stats = match timing {
                    Timing::Live { capture_us, received_us, decoded_us } => stats.record(FrameTimes {
                        capture_us,
                        received_us,
                        decoded_us,
                        displayed_us: unix_micros(),
                    }),
                    Timing::Paced { late_us } => stats.record_paced(late_us),
                };
            }
            None => {
                display.idle();
                thread::sleep(IDLE_POLL);
            }
        }

        if stats_since.elapsed() >= STATS_INTERVAL {
            if let Some(summary) = stats.summary(stats_since.elapsed().as_secs_f64()) {
                println!("recv: {summary}");
                display.set_status(&summary);
            }
            stats = LatencyStats::default();
            stats_since = Instant::now();
        }
    }
    Ok(())
}
