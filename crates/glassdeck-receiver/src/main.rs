mod director;
mod keymap;
mod local;
mod mailbox;
mod mp4;
mod overlay;
mod playout;
mod presentation;
mod put;
mod screen;
mod stats;
mod upstream;
mod widgets;

#[cfg(target_os = "linux")]
#[path = "linux/mod.rs"]
mod platform;
#[cfg(windows)]
#[path = "windows/mod.rs"]
mod platform;

use std::collections::HashMap;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::mpsc::{self, Sender, SyncSender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Parser;
use glassdeck_luau::placement::{Placement, PlacedWidget};
use glassdeck_package::QualifiedId;
use glassdeck_proto::{Action, Message, Mode, PACED_PLAYOUT_DELAY_US, PutFile, ScriptTarget, SessionState, StreamInfo, WidgetSlot, step, unix_micros};

use director::{Director, SharedDirector, lock};
use glassdeck_layout::Variant;
use mailbox::{Mailbox, ShownFrame, Timing};
use overlay::Labels;
use platform::{Compositor, Decoder};
use playout::{PlayoutClock, wait_until};
use presentation::Outcome;
use put::{Folders, QUEUED_PIECES};
use screen::Event;
use stats::{FrameTimes, LatencyStats};
use upstream::Upstream;
use widgets::{Input, Tiles};

const STATS_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Parser)]
#[command(about = "Stand-in for the keyboard: decode streams and show them with key labels")]
struct Cli {
    #[arg(long, default_value = platform::DEFAULT_LISTEN)]
    listen: SocketAddr,
    /// A .ttf or .otf to draw labels and widgets with, instead of the built-in Noto Sans JP Bold
    #[arg(long)]
    font: Option<PathBuf>,
    /// Show the stream without key labels
    #[arg(long)]
    no_labels: bool,
    /// The keyboard's physical layout, which places the key labels
    #[arg(long, value_enum, default_value_t = KeyboardLayout::Ansi)]
    layout: KeyboardLayout,
    /// Installed packages (.deck files): backgrounds and widgets
    #[arg(long, default_value = platform::DEFAULT_PACKAGES)]
    packages: PathBuf,
    /// Where the keyboard keeps its own state: the chosen background and the widget layout
    #[arg(long, default_value = platform::DEFAULT_STATE)]
    state: PathBuf,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum KeyboardLayout {
    Ansi,
    Iso,
}

impl From<KeyboardLayout> for Variant {
    fn from(layout: KeyboardLayout) -> Self {
        match layout {
            KeyboardLayout::Ansi => Variant::Ansi,
            KeyboardLayout::Iso => Variant::Iso,
        }
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let font = load_font(cli.font.as_deref())?;
    let variant = Variant::from(cli.layout);
    let labels = (!cli.no_labels).then(|| Labels { font: font.clone(), variant });
    let compositor = Compositor::open(labels)?;
    let tiles = Arc::new(Tiles::new(compositor.doorbell()));
    let folders = Folders { packages: cli.packages.clone() };
    let config = widgets::Config { packages: cli.packages.clone(), state: cli.state.clone(), font };
    let upstream = Arc::new(Upstream::default());
    let widgets = widgets::start(config, Arc::clone(&tiles), Arc::clone(&upstream))?;
    let listener = TcpListener::bind(cli.listen).with_context(|| format!("listening on {}", cli.listen))?;
    println!("Waiting for streams on {}", cli.listen);

    let mailbox = Arc::new(Mailbox::new(compositor.doorbell()));
    let director = Director::start(cli.packages, cli.state, compositor.registrar(), Arc::clone(&mailbox), Arc::clone(&upstream))?;
    let producer = Arc::clone(&mailbox);
    thread::Builder::new().name("receive".into()).spawn(move || accept_loop(listener, &producer, &director, &widgets, &folders, &upstream))?;
    compose_loop(&mailbox, &tiles, compositor)
}

pub(crate) use glassdeck_widget::load_font;

/// The receiver is a server: one sender at a time, and a broken connection is reported, not fatal.
fn accept_loop(listener: TcpListener, mailbox: &Mailbox, director: &SharedDirector, widgets: &Sender<Input>, folders: &Folders, upstream: &Upstream) {
    if let Err(e) = platform::init_decoder_thread() {
        eprintln!("error: starting the decoder: {e:#}");
        std::process::exit(1);
    }
    let mut digests = put::Digests::default();
    for connection in listener.incoming() {
        let mut state = SessionState::Idle;
        let result = connection.context("accepting connection").and_then(|socket| {
            upstream.connected(socket.try_clone()?);
            let (pieces, arriving) = mpsc::sync_channel(QUEUED_PIECES);
            thread::scope(|scope| {
                thread::Builder::new()
                    .name("files".into())
                    .spawn_scoped(scope, || put::write_files(folders, arriving, upstream, widgets, director))?;
                let inventory = Message::Inventory { packages: digests.inventory(folders)? };
                upstream.send(&inventory).context("telling the host what's installed")?;
                let served = serve(socket, mailbox, director, widgets, &pieces, upstream, folders, &mut state);
                drop(pieces);
                served
            })
        });
        upstream.disconnected();
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

fn serve(
    mut socket: TcpStream,
    mailbox: &Mailbox,
    director: &SharedDirector,
    widgets: &Sender<Input>,
    pieces: &SyncSender<PutFile>,
    upstream: &Upstream,
    folders: &Folders,
    state: &mut SessionState,
) -> Result<()> {
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
                println!("Stream {}: {:?} {}x{} {:?} via {}", info.stream_id, info.codec, info.width, info.height, info.mode, decoder.name);
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
                            let clock =
                                *playback.clock.get_or_insert(PlayoutClock::start(picture.time_us, unix_micros(), PACED_PLAYOUT_DELAY_US));
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
            Action::SelectBackground(name) => report(lock(director).select(name)),
            Action::RemovePackages(names) => report(put::remove_packages(folders, names)),
            Action::SetWidgets(slots) => match placed_widgets(slots) {
                Ok(layout) => widgets.send(Input::SetWidgets(layout)).context("the widget thread has stopped")?,
                Err(e) => report(Err(e.context("the host's widget layout"))),
            },
            Action::BackgroundData(payload) => report(lock(director).background_data(payload.to_vec())),
            Action::ModuleData { instance, payload } => {
                widgets.send(Input::Data { instance, payload: payload.to_vec() }).context("the widget thread has stopped")?
            }
            Action::ScriptMessage { to: ScriptTarget::Background, kind, message, payload } => {
                report(lock(director).script_message(kind, message, payload.to_vec()));
            }
            Action::ScriptMessage { to: ScriptTarget::Widget(instance), message, payload, .. } => widgets
                .send(Input::Message { instance, message, payload: payload.to_vec() })
                .context("the widget thread has stopped")?,
            Action::ModuleResource { instance, name, data } => widgets
                .send(Input::Resource { instance, name: name.to_owned(), data: data.to_vec() })
                .context("the widget thread has stopped")?,
            Action::PutFile(put) => pieces.send(put.clone()).context("the file writer has stopped")?,
            Action::AnswerProbe { probe_sent_us } => {
                let reply = Message::ClockReply { probe_sent_us, received_us };
                upstream.send(&reply).context("answering a clock probe")?;
            }
        }
    }
    Ok(())
}

/// A layout from the Link, with each id checked; `widgets` checks sizes against what's installed.
fn placed_widgets(slots: &[WidgetSlot]) -> Result<Vec<PlacedWidget>> {
    slots
        .iter()
        .map(|slot| {
            let placement = Placement { x: slot.x.into(), y: slot.y.into(), w: slot.w.into(), h: slot.h.into() };
            Ok(PlacedWidget { package: QualifiedId::parse(&slot.package)?, placement })
        })
        .collect()
}

/// A failure that is logged, not fatal: the connection carries on.
pub(crate) fn report(result: Result<()>) {
    if let Err(e) = result {
        eprintln!("error: {e:#}");
    }
}

fn compose_loop(mailbox: &Mailbox, tiles: &Tiles, mut compositor: Compositor) -> Result<()> {
    let mut stats = LatencyStats::default();
    let mut stats_since = Instant::now();
    let mut in_flight: HashMap<u64, Timing> = HashMap::new();
    let mut next_token = 0;

    while compositor.is_open() {
        let (frame, replaced) = mailbox.take();
        stats = stats.replaced(replaced);
        let new_tiles = tiles.take();
        let idle = frame.is_none() && new_tiles.is_empty();
        if !new_tiles.is_empty() {
            compositor.show_tiles(new_tiles)?;
        }
        if let Some(ShownFrame { picture, timing }) = frame {
            in_flight.insert(next_token, timing);
            compositor.submit(picture, next_token)?;
            next_token += 1;
        }
        for outcome in compositor.poll()? {
            stats = match outcome {
                Outcome::Presented { token, at_us } => match in_flight.remove(&token).context("presented an unknown picture")? {
                    Timing::Live { capture_us, received_us, decoded_us } => {
                        stats.record(FrameTimes { capture_us, received_us, decoded_us, displayed_us: at_us })
                    }
                    Timing::Paced { late_us } => stats.record_paced(late_us),
                },
                Outcome::Dropped { token } => {
                    in_flight.remove(&token);
                    stats.replaced(1)
                }
            };
        }
        if idle {
            compositor.wait(STATS_INTERVAL.saturating_sub(stats_since.elapsed()))?;
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
