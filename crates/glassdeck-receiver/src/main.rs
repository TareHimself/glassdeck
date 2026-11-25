mod director;
mod local;
mod mailbox;
mod mp4;
mod overlay;
mod playout;
mod presentation;
mod screen;
mod stats;

#[cfg(target_os = "linux")]
#[path = "linux/mod.rs"]
mod platform;
#[cfg(windows)]
#[path = "windows/mod.rs"]
mod platform;

use std::collections::HashMap;
use std::io::Write;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use clap::Parser;
use fontdue::{Font, FontSettings};
use glassdeck_proto::{Action, Message, Mode, PACED_PLAYOUT_DELAY_US, SessionState, StreamInfo, step, unix_micros};

use platform::{Compositor, Decoder};
use director::{Director, SharedDirector, lock};
use mailbox::{Mailbox, ShownFrame, Timing};
use playout::{PlayoutClock, wait_until};
use screen::Event;
use presentation::Outcome;
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
    /// Where the keyboard keeps decks it plays with the PC off
    #[arg(long, default_value = platform::DEFAULT_DECKS)]
    decks: PathBuf,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let font = if cli.no_labels { None } else { Some(load_font(&cli.font)?) };
    let compositor = Compositor::open(font)?;
    let listener = TcpListener::bind(cli.listen).with_context(|| format!("listening on {}", cli.listen))?;
    println!("Waiting for streams on {}", cli.listen);

    let mailbox = Arc::new(Mailbox::new());
    let director = Director::start(cli.decks, compositor.registrar(), Arc::clone(&mailbox))?;
    let producer = Arc::clone(&mailbox);
    thread::Builder::new().name("receive".into()).spawn(move || accept_loop(listener, &producer, &director))?;
    compose_loop(&mailbox, compositor)
}

fn load_font(path: &PathBuf) -> Result<Font> {
    let bytes = std::fs::read(path).with_context(|| format!("reading font {}", path.display()))?;
    Font::from_bytes(bytes, FontSettings::default()).map_err(|e| anyhow!("parsing font: {e}"))
}

/// The receiver is a server: one sender at a time, and a broken connection is reported, not fatal.
fn accept_loop(listener: TcpListener, mailbox: &Mailbox, director: &SharedDirector) {
    if let Err(e) = platform::init_decoder_thread() {
        eprintln!("error: starting the decoder: {e:#}");
        std::process::exit(1);
    }
    for connection in listener.incoming() {
        let mut state = SessionState::Idle;
        let result = connection
            .context("accepting connection")
            .and_then(|socket| serve(socket, mailbox, director, &mut state));
        match result {
            Ok(()) => println!("Sender disconnected"),
            Err(e) => eprintln!("error: {e:#}"),
        }
        if matches!(state, SessionState::Streaming(_)) {
            report(lock(director).handle(Event::StreamEnded));
        }
    }
}

struct Playback {
    info: StreamInfo,
    decoder: Decoder,
    clock: Option<PlayoutClock>,
}

fn serve(mut socket: TcpStream, mailbox: &Mailbox, director: &SharedDirector, state: &mut SessionState) -> Result<()> {
    socket.set_nodelay(true)?;
    println!("Sender connected from {}", socket.peer_addr()?);
    let mut playback: Option<Playback> = None;

    while let Some(message) = Message::read_from(&mut socket)? {
        let received_us = unix_micros();
        let (next, action) = step(*state, &message)?;
        *state = next;
        match action {
            Action::Begin(info) => {
                lock(director).handle(Event::StreamStarted)?;
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
                            let clock = *playback
                                .clock
                                .get_or_insert(PlayoutClock::start(picture.time_us, unix_micros(), PACED_PLAYOUT_DELAY_US));
                            Timing::Paced { late_us: wait_until(clock.due_us(picture.time_us)) }
                        }
                    };
                    mailbox.put(ShownFrame { picture: platform::decoded(picture), timing });
                }
            }
            Action::End => {
                println!("Stream ended");
                playback = None;
                lock(director).handle(Event::StreamEnded)?;
            }
            Action::SelectDeck(name) => report(lock(director).select(name)),
            Action::AnswerProbe { probe_sent_us } => {
                let reply = Message::ClockReply { probe_sent_us, received_us };
                socket.write_all(&reply.encode()).context("answering a clock probe")?;
            }
        }
    }
    Ok(())
}

/// A failure that is logged, not fatal: the connection carries on.
fn report(result: Result<()>) {
    if let Err(e) = result {
        eprintln!("error: {e:#}");
    }
}

fn compose_loop(mailbox: &Mailbox, mut compositor: Compositor) -> Result<()> {
    let mut stats = LatencyStats::default();
    let mut stats_since = Instant::now();
    let mut in_flight: HashMap<u64, Timing> = HashMap::new();
    let mut next_token = 0;

    while compositor.is_open() {
        let (frame, replaced) = mailbox.take();
        stats = stats.replaced(replaced);
        let idle = frame.is_none();
        if let Some(ShownFrame { picture, timing }) = frame {
            in_flight.insert(next_token, timing);
            compositor.submit(picture, next_token)?;
            next_token += 1;
        }
        for outcome in compositor.poll()? {
            stats = match outcome {
                Outcome::Presented { token, at_us } => match in_flight.remove(&token).context("presented an unknown picture")? {
                    Timing::Live { capture_us, received_us, decoded_us } => stats.record(FrameTimes {
                        capture_us,
                        received_us,
                        decoded_us,
                        displayed_us: at_us,
                    }),
                    Timing::Paced { late_us } => stats.record_paced(late_us),
                },
                Outcome::Dropped { token } => {
                    in_flight.remove(&token);
                    stats.replaced(1)
                }
            };
        }
        if idle {
            compositor.idle();
            thread::sleep(IDLE_POLL);
        }

        if stats_since.elapsed() >= STATS_INTERVAL {
            if let Some(summary) = stats.summary(stats_since.elapsed().as_secs_f64()) {
                println!("recv: {summary}");
                compositor.set_status(&summary);
            }
            stats = LatencyStats::default();
            stats_since = Instant::now();
        }
    }
    Ok(())
}
