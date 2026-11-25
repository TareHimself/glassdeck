//! The only writer to the keyboard: one thread owns the socket, reconnects when it drops, and sends
//! control messages first, then the stream, then widget data in the gaps.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::net::SocketAddr;
use std::sync::{Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use glassdeck_capture::link::Link;
use glassdeck_proto::{Message, Mode, StreamInfo};

/// Realistic USB 2.0 bulk throughput, what the TCP stand-in is throttled to by default.
pub const USB2_PRACTICAL_BYTES_PER_SEC: u64 = 35_000_000;
/// Stream messages waiting for the Link before the source is made to wait.
const QUEUED_STREAM_MESSAGES: usize = 4;
const FIRST_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkState {
    Disconnected { retry_at: Instant, backoff: Duration },
    Connecting { backoff: Duration },
    Connected,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkEvent {
    RetryDue,
    Connected,
    ConnectFailed,
    Lost,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkEffect {
    /// Re-send the background selection and every widget's latest value, and tell the source runner
    Resend,
    /// Drop queued messages and refuse the stream until the Link is back
    DropQueued,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BadMove(LinkState, LinkEvent);

impl fmt::Display for BadMove {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "the Link can't go from {:?} on {:?}", self.0, self.1)
    }
}

impl std::error::Error for BadMove {}

/// Every move between Link states goes through here; the test walks the full table.
pub fn step(state: LinkState, event: LinkEvent, now: Instant) -> Result<(LinkState, Vec<LinkEffect>), BadMove> {
    use LinkState::{Connected, Connecting, Disconnected};
    Ok(match (state, event) {
        (Disconnected { backoff, .. }, LinkEvent::RetryDue) => (Connecting { backoff }, vec![]),
        (Connecting { .. }, LinkEvent::Connected) => (Connected, vec![LinkEffect::Resend]),
        (Connecting { backoff }, LinkEvent::ConnectFailed) => {
            (Disconnected { retry_at: now + backoff, backoff: (backoff * 2).min(MAX_BACKOFF) }, vec![])
        }
        (Connected, LinkEvent::Lost) => {
            (Disconnected { retry_at: now + FIRST_BACKOFF, backoff: FIRST_BACKOFF * 2 }, vec![LinkEffect::DropQueued])
        }
        _ => return Err(BadMove(state, event)),
    })
}

/// The stream was refused because the keyboard isn't connected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LinkDown;

impl fmt::Display for LinkDown {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "the keyboard isn't connected")
    }
}

impl std::error::Error for LinkDown {}

/// What waits for the Link. Everyone else hands messages to it; only the Link thread sends.
pub struct Uplink {
    queues: Mutex<Queues>,
    changed: Condvar,
    on_connected: Box<dyn Fn() + Send + Sync>,
}

#[derive(Default)]
struct Queues {
    connected: bool,
    control: VecDeque<Message>,
    stream: VecDeque<Message>,
    /// Each widget's latest value, kept after sending so a reconnect brings the keyboard up to date.
    latest: BTreeMap<u16, Vec<u8>>,
    unsent: BTreeSet<u16>,
    selected: Option<String>,
}

enum Outgoing {
    Control(Message),
    Stream(Message),
    ModuleData { instance: u16, payload: Vec<u8> },
}

impl Uplink {
    pub fn new(on_connected: impl Fn() + Send + Sync + 'static) -> Self {
        Self { queues: Mutex::new(Queues::default()), changed: Condvar::new(), on_connected: Box::new(on_connected) }
    }

    pub fn is_connected(&self) -> bool {
        self.lock().connected
    }

    pub fn select_background(&self, name: String) {
        let mut queues = self.lock();
        if queues.connected {
            queues.control.push_back(Message::SelectBackground { name: name.clone() });
        }
        queues.selected = Some(name);
        self.changed.notify_all();
    }

    /// Latest value wins: a widget's new data replaces anything of its not yet sent.
    pub fn put_module_data(&self, instance: u16, payload: Vec<u8>) {
        let mut queues = self.lock();
        queues.latest.insert(instance, payload);
        queues.unsent.insert(instance);
        self.changed.notify_all();
    }

    /// For a new widget list, whose instance numbers mean different widgets.
    pub fn forget_module_data(&self) {
        let mut queues = self.lock();
        queues.latest.clear();
        queues.unsent.clear();
    }

    /// Queues one message of the running stream, waiting while the queue is full.
    pub fn push_stream(&self, message: Message) -> Result<(), LinkDown> {
        let mut queues = self.lock();
        loop {
            if !queues.connected {
                return Err(LinkDown);
            }
            if queues.stream.len() < QUEUED_STREAM_MESSAGES {
                queues.stream.push_back(message);
                self.changed.notify_all();
                return Ok(());
            }
            queues = self.changed.wait(queues).expect("uplink poisoned");
        }
    }

    fn lock(&self) -> MutexGuard<'_, Queues> {
        self.queues.lock().expect("uplink poisoned")
    }

    /// Control first, then the stream, then widget data, so widgets only use the gaps.
    fn next_outgoing(&self) -> Outgoing {
        let mut queues = self.lock();
        loop {
            if let Some(message) = queues.control.pop_front() {
                return Outgoing::Control(message);
            }
            if let Some(message) = queues.stream.pop_front() {
                self.changed.notify_all();
                return Outgoing::Stream(message);
            }
            if let Some(instance) = queues.unsent.pop_first() {
                let payload = queues.latest[&instance].clone();
                return Outgoing::ModuleData { instance, payload };
            }
            queues = self.changed.wait(queues).expect("uplink poisoned");
        }
    }

    fn apply(&self, effect: LinkEffect) {
        let mut queues = self.lock();
        match effect {
            LinkEffect::Resend => {
                queues.connected = true;
                queues.unsent = queues.latest.keys().copied().collect();
                if let Some(name) = queues.selected.clone() {
                    queues.control.push_back(Message::SelectBackground { name });
                }
            }
            LinkEffect::DropQueued => {
                queues.connected = false;
                queues.control.clear();
                queues.stream.clear();
            }
        }
        self.changed.notify_all();
        drop(queues);
        if effect == LinkEffect::Resend {
            (self.on_connected)();
        }
    }
}

/// The Link thread: runs for the life of glassdeckd.
pub fn run(uplink: &Uplink, to: SocketAddr, link_bytes_per_sec: Option<u64>) {
    let mut state = LinkState::Disconnected { retry_at: Instant::now(), backoff: FIRST_BACKOFF };
    let mut link: Option<Link> = None;
    let mut failing_since_connected = false;
    loop {
        let event = match state {
            LinkState::Disconnected { retry_at, .. } => {
                thread::sleep(retry_at.saturating_duration_since(Instant::now()));
                LinkEvent::RetryDue
            }
            LinkState::Connecting { .. } => match Link::connect(to, link_bytes_per_sec) {
                Ok(connected) => {
                    println!("Link: connected to {to}");
                    link = Some(connected);
                    failing_since_connected = false;
                    LinkEvent::Connected
                }
                Err(e) => {
                    if !failing_since_connected {
                        eprintln!("Link: can't reach the keyboard ({e:#}); retrying until it answers");
                        failing_since_connected = true;
                    }
                    LinkEvent::ConnectFailed
                }
            },
            LinkState::Connected => {
                let connected = link.as_mut().expect("a connected Link has a socket");
                match send(connected, uplink.next_outgoing()) {
                    Ok(()) => continue,
                    Err(e) => {
                        eprintln!("Link: lost the keyboard ({e:#}); reconnecting");
                        link = None;
                        LinkEvent::Lost
                    }
                }
            }
        };
        let (next, effects) = step(state, event, Instant::now()).expect("the Link thread only makes listed moves");
        state = next;
        for effect in effects {
            uplink.apply(effect);
        }
    }
}

fn send(link: &mut Link, outgoing: Outgoing) -> Result<()> {
    let message = match outgoing {
        Outgoing::Control(message) => message,
        Outgoing::ModuleData { instance, payload } => Message::ModuleData { instance, payload },
        Outgoing::Stream(Message::StreamStart(info @ StreamInfo { mode: Mode::Live { .. }, .. })) => {
            let sample = link.sync_clock()?;
            println!(
                "Link: keyboard clock is {:+.1} ms from ours (probe round trip {:.1} ms)",
                sample.offset_us() as f64 / 1000.0,
                sample.round_trip_us() as f64 / 1000.0
            );
            Message::StreamStart(StreamInfo { mode: Mode::Live { clock_offset_us: sample.offset_us() }, ..info })
        }
        Outgoing::Stream(message) => message,
    };
    link.send(&message)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::LinkEffect::{DropQueued, Resend};
    use super::LinkEvent::{Connected as Up, ConnectFailed, Lost, RetryDue};
    use super::LinkState::{Connected, Connecting, Disconnected};
    use super::*;

    #[test]
    fn walks_the_whole_transition_table() {
        let now = Instant::now();
        let second = Duration::from_secs(1);
        let states = [Disconnected { retry_at: now, backoff: second }, Connecting { backoff: second * 8 }, Connected];
        let events = [RetryDue, Up, ConnectFailed, Lost];
        for state in states {
            for event in events {
                let expected = match (state, event) {
                    (Disconnected { .. }, RetryDue) => Ok((Connecting { backoff: second }, vec![])),
                    (Connecting { .. }, Up) => Ok((Connected, vec![Resend])),
                    (Connecting { .. }, ConnectFailed) => Ok((Disconnected { retry_at: now + second * 8, backoff: MAX_BACKOFF }, vec![])),
                    (Connected, Lost) => Ok((Disconnected { retry_at: now + FIRST_BACKOFF, backoff: FIRST_BACKOFF * 2 }, vec![DropQueued])),
                    _ => Err(BadMove(state, event)),
                };
                assert_eq!(step(state, event, now), expected, "from {state:?} on {event:?}");
            }
        }
    }

    #[test]
    fn widgets_wait_behind_the_stream_and_control_goes_first() {
        let uplink = Uplink::new(|| {});
        uplink.apply(Resend);
        uplink.put_module_data(0, vec![1]);
        uplink.push_stream(Message::StreamEnd { stream_id: 7 }).unwrap();
        uplink.select_background("a.deckbg".into());
        assert!(matches!(uplink.next_outgoing(), Outgoing::Control(Message::SelectBackground { .. })));
        assert!(matches!(uplink.next_outgoing(), Outgoing::Stream(Message::StreamEnd { stream_id: 7 })));
        assert!(matches!(uplink.next_outgoing(), Outgoing::ModuleData { instance: 0, .. }));
    }

    #[test]
    fn a_widgets_newest_value_replaces_its_unsent_one() {
        let uplink = Uplink::new(|| {});
        uplink.apply(Resend);
        uplink.put_module_data(3, vec![1]);
        uplink.put_module_data(3, vec![2]);
        assert!(matches!(uplink.next_outgoing(), Outgoing::ModuleData { instance: 3, payload } if payload == vec![2]));
    }

    #[test]
    fn reconnecting_resends_every_widget_and_the_selection() {
        let uplink = Uplink::new(|| {});
        uplink.select_background("a.deckbg".into());
        uplink.put_module_data(1, vec![9]);
        uplink.apply(Resend);
        assert!(matches!(uplink.next_outgoing(), Outgoing::Control(_)));
        assert!(matches!(uplink.next_outgoing(), Outgoing::ModuleData { instance: 1, .. }));
        uplink.apply(DropQueued);
        assert_eq!(uplink.push_stream(Message::StreamEnd { stream_id: 1 }), Err(LinkDown));
        uplink.apply(Resend);
        assert!(matches!(uplink.next_outgoing(), Outgoing::Control(_)));
        assert!(matches!(uplink.next_outgoing(), Outgoing::ModuleData { instance: 1, .. }));
    }
}
