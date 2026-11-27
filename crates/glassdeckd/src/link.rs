//! The only writer to the keyboard: one thread owns the socket, reconnects when it drops, and sends
//! control messages first, then the stream, then widget data in the gaps.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::net::SocketAddr;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::sync::mpsc;

use anyhow::{Context, Result, anyhow};
use glassdeck_api::KeyboardLink;
use glassdeck_capture::link::{IncompatibleReceiver, Link};
use glassdeck_proto::{FileTarget, Message, Mode, PutFile, StreamInfo, VERSION, WidgetSlot};

/// Realistic USB 2.0 bulk throughput, what the TCP stand-in is throttled to by default.
pub const USB2_PRACTICAL_BYTES_PER_SEC: u64 = 35_000_000;
/// Stream messages waiting for the Link before the source is made to wait.
const QUEUED_STREAM_MESSAGES: usize = 4;
/// Small enough that a piece never holds up the next video frame for long.
const PIECE_BYTES: u64 = 256 * 1024;
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
    /// Told whenever whether glassdeckd can talk to the keyboard changes
    on_change: Box<dyn Fn(KeyboardLink) + Send + Sync>,
    /// Told when the keyboard has said what it holds, after each connect
    on_inventory: Box<dyn Fn() + Send + Sync>,
}

#[derive(Default)]
struct Queues {
    link: KeyboardLink,
    control: VecDeque<Message>,
    stream: VecDeque<Message>,
    /// Each widget's latest value, kept after sending so a reconnect brings the keyboard up to date.
    latest: BTreeMap<u16, Vec<u8>>,
    unsent: BTreeSet<u16>,
    /// Each widget's latest live resources by name, kept like `latest` for a reconnect. Empty means removed.
    resources: BTreeMap<(u16, String), Vec<u8>>,
    unsent_resources: BTreeSet<(u16, String)>,
    selected: Option<String>,
    /// The widget layout, kept like `selected` for a reconnect
    widgets: Option<Vec<WidgetSlot>>,
    /// What the keyboard holds, by file name, as it said on connecting and as uploads and removals since changed it
    inventory: Option<BTreeMap<String, u64>>,
    /// The selected canvas's latest data from its host half, kept like `latest` for a reconnect.
    background_data: Option<Vec<u8>>,
    background_data_unsent: bool,
    uploads: VecDeque<Upload>,
    /// Fully sent files, oldest first, which is the order the keyboard answers them in.
    awaiting: VecDeque<Waiter>,
}

impl Queues {
    fn connected(&self) -> bool {
        self.link == KeyboardLink::Connected
    }
}

enum Outgoing {
    Control(Message),
    Stream(Message),
    ModuleData { instance: u16, payload: Vec<u8> },
    ModuleResource { instance: u16, name: String, data: Vec<u8> },
    BackgroundData(Vec<u8>),
    Put(Upload),
}

/// Whoever asked for an upload, waiting for how it went.
struct Waiter {
    target: FileTarget,
    answer: mpsc::Sender<Result<()>>,
}

impl Waiter {
    /// They may have stopped waiting, which is fine.
    fn finish(self, result: Result<()>) {
        let _ = self.answer.send(result);
    }
}

/// A file on its way to the keyboard, sent a piece at a time in the gaps the stream and widgets leave.
pub struct Upload {
    file: File,
    total: u64,
    sent: u64,
    waiter: Waiter,
}

impl Upload {
    fn next_piece(&mut self) -> Result<PutFile> {
        let len = (self.total - self.sent).min(PIECE_BYTES);
        let mut data = vec![0; len as usize];
        self.file.read_exact(&mut data).with_context(|| format!("reading {}", self.waiter.target))?;
        let piece = PutFile { target: self.waiter.target.clone(), offset: self.sent, total: self.total, data };
        self.sent += len;
        Ok(piece)
    }
}

impl Uplink {
    pub fn new(on_change: impl Fn(KeyboardLink) + Send + Sync + 'static, on_inventory: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            queues: Mutex::new(Queues::default()),
            changed: Condvar::new(),
            on_change: Box::new(on_change),
            on_inventory: Box::new(on_inventory),
        }
    }

    /// What the keyboard holds, by file name and digest; `None` until it has said, after each connect.
    pub fn inventory(&self) -> Option<BTreeMap<String, u64>> {
        self.lock().inventory.clone()
    }

    /// The keyboard installed `name`, whose digest is `digest`.
    pub fn record_installed(&self, name: String, digest: u64) {
        if let Some(inventory) = self.lock().inventory.as_mut() {
            inventory.insert(name, digest);
        }
    }

    /// Asks the keyboard to delete packages, and stops counting them as held.
    pub fn remove_packages(&self, names: Vec<String>) {
        let mut queues = self.lock();
        if let Some(inventory) = queues.inventory.as_mut() {
            for name in &names {
                inventory.remove(name);
            }
        }
        if queues.connected() {
            queues.control.push_back(Message::RemovePackages { names });
        }
        self.changed.notify_all();
    }

    pub fn link(&self) -> KeyboardLink {
        self.lock().link
    }

    /// Why the last connection attempt failed; reports whether that changed anything.
    fn report_not_connected(&self, link: KeyboardLink) -> bool {
        let mut queues = self.lock();
        if queues.connected() || queues.link == link {
            return false;
        }
        queues.link = link;
        drop(queues);
        (self.on_change)(link);
        true
    }

    pub fn select_background(&self, name: String) {
        let mut queues = self.lock();
        if queues.connected() {
            queues.control.push_back(Message::SelectBackground { name: name.clone() });
        }
        if queues.selected.as_ref() != Some(&name) {
            queues.background_data = None;
            queues.background_data_unsent = false;
        }
        queues.selected = Some(name);
        self.changed.notify_all();
    }

    /// The widgets the keyboard runs, in order; sent now if connected, and again on every reconnect.
    pub fn set_widgets(&self, widgets: Vec<WidgetSlot>) {
        let mut queues = self.lock();
        if queues.connected() {
            queues.control.push_back(Message::SetWidgets { widgets: widgets.clone() });
        }
        queues.widgets = Some(widgets);
        self.changed.notify_all();
    }

    /// Latest value wins: a widget's new data replaces anything of its not yet sent.
    pub fn put_module_data(&self, instance: u16, payload: Vec<u8>) {
        let mut queues = self.lock();
        queues.latest.insert(instance, payload);
        queues.unsent.insert(instance);
        self.changed.notify_all();
    }

    /// Latest wins per widget and name: a resource replaces anything of the same name not yet sent.
    /// Empty `data` removes it on the keyboard.
    pub fn put_module_resource(&self, instance: u16, name: String, data: Vec<u8>) {
        let mut queues = self.lock();
        queues.resources.insert((instance, name.clone()), data);
        queues.unsent_resources.insert((instance, name));
        self.changed.notify_all();
    }

    /// Latest wins: what the selected canvas's host half sent, after the choice itself.
    pub fn put_background_data(&self, payload: Vec<u8>) {
        let mut queues = self.lock();
        queues.background_data = Some(payload);
        queues.background_data_unsent = true;
        self.changed.notify_all();
    }

    /// For a widget that stopped or restarted, so what its old host side sent isn't sent again.
    pub fn forget_module_instance(&self, instance: u16) {
        let mut queues = self.lock();
        queues.latest.remove(&instance);
        queues.unsent.remove(&instance);
        queues.resources.retain(|(owner, _), _| *owner != instance);
        queues.unsent_resources.retain(|(owner, _)| *owner != instance);
    }

    /// Queues one message of the running stream, waiting while the queue is full.
    pub fn push_stream(&self, message: Message) -> Result<(), LinkDown> {
        let mut queues = self.lock();
        loop {
            if !queues.connected() {
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

    /// Queues `path` for the keyboard to install as `target`; the receiver gets the keyboard's answer.
    pub fn put_file(&self, target: FileTarget, path: &Path) -> Result<mpsc::Receiver<Result<()>>> {
        let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
        let total = file.metadata()?.len();
        let (answer, answered) = mpsc::channel();
        let mut queues = self.lock();
        if !queues.connected() {
            return Err(LinkDown.into());
        }
        queues.uploads.push_back(Upload { file, total, sent: 0, waiter: Waiter { target, answer } });
        self.changed.notify_all();
        Ok(answered)
    }

    /// Puts a partly sent upload back at the front, or, after its last piece, waits for the answer.
    /// Called before the piece goes out, so the answer can't arrive first.
    fn sending_piece(&self, upload: Upload, last: bool) {
        let mut queues = self.lock();
        match (queues.connected(), last) {
            (false, _) => upload.waiter.finish(Err(LinkDown.into())),
            (true, false) => queues.uploads.push_front(upload),
            (true, true) => queues.awaiting.push_back(upload.waiter),
        }
    }

    /// The keyboard's answer to the oldest fully sent file, from the Link's reader thread.
    pub fn answered(&self, message: Message) {
        if let Message::Inventory { packages } = message {
            self.lock().inventory = Some(packages.into_iter().map(|p| (p.name, p.digest)).collect());
            (self.on_inventory)();
            return;
        }
        let waiter = self.lock().awaiting.pop_front();
        let (target, result) = match message {
            Message::PutDone { target } => (target, Ok(())),
            Message::PutRefused { target, reason } => {
                let refusal = anyhow!("the keyboard couldn't install {target}: {reason}");
                (target, Err(refusal))
            }
            other => {
                eprintln!("Link: the keyboard sent {other:?}, which nothing asked for");
                return;
            }
        };
        match waiter {
            Some(waiter) if waiter.target == target => waiter.finish(result),
            Some(waiter) => {
                eprintln!("Link: the keyboard answered for {target} while {} was waiting", waiter.target);
                waiter.finish(Err(anyhow!("the keyboard answered for {target} instead")));
            }
            None => eprintln!("Link: the keyboard answered for {target}, which nothing was waiting on"),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Queues> {
        self.queues.lock().expect("uplink poisoned")
    }

    /// Control first, then the stream, then widget data, then file pieces, so each only uses the
    /// gaps the ones before it leave.
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
            if std::mem::take(&mut queues.background_data_unsent)
                && let Some(payload) = queues.background_data.clone()
            {
                return Outgoing::BackgroundData(payload);
            }
            if let Some(instance) = queues.unsent.pop_first() {
                let payload = queues.latest[&instance].clone();
                return Outgoing::ModuleData { instance, payload };
            }
            if let Some(key) = queues.unsent_resources.pop_first() {
                let data = queues.resources[&key].clone();
                let (instance, name) = key;
                return Outgoing::ModuleResource { instance, name, data };
            }
            if let Some(upload) = queues.uploads.pop_front() {
                return Outgoing::Put(upload);
            }
            queues = self.changed.wait(queues).expect("uplink poisoned");
        }
    }

    fn apply(&self, effect: LinkEffect) {
        let mut queues = self.lock();
        let was = queues.link;
        match effect {
            LinkEffect::Resend => {
                queues.link = KeyboardLink::Connected;
                queues.unsent = queues.latest.keys().copied().collect();
                queues.unsent_resources = queues.resources.keys().cloned().collect();
                queues.background_data_unsent = queues.background_data.is_some();
                if let Some(name) = queues.selected.clone() {
                    queues.control.push_back(Message::SelectBackground { name });
                }
                if let Some(widgets) = queues.widgets.clone() {
                    queues.control.push_back(Message::SetWidgets { widgets });
                }
            }
            LinkEffect::DropQueued => {
                queues.link = KeyboardLink::Unreachable;
                queues.inventory = None;
                queues.control.clear();
                queues.stream.clear();
                let uploads: Vec<Waiter> = queues.uploads.drain(..).map(|upload| upload.waiter).collect();
                for waiter in uploads.into_iter().chain(queues.awaiting.drain(..)) {
                    waiter.finish(Err(LinkDown.into()));
                }
            }
        }
        self.changed.notify_all();
        let link = queues.link;
        drop(queues);
        if link != was {
            (self.on_change)(link);
        }
    }
}

/// The Link thread: runs for the life of glassdeckd.
pub fn run(uplink: &Arc<Uplink>, to: SocketAddr, link_bytes_per_sec: Option<u64>) {
    let mut state = LinkState::Disconnected { retry_at: Instant::now(), backoff: FIRST_BACKOFF };
    let mut link: Option<Link> = None;
    let mut failing_since_connected = false;
    loop {
        let event = match state {
            LinkState::Disconnected { retry_at, .. } => {
                thread::sleep(retry_at.saturating_duration_since(Instant::now()));
                LinkEvent::RetryDue
            }
            LinkState::Connecting { .. } => match Link::connect(to, link_bytes_per_sec, {
                let uplink = Arc::clone(uplink);
                move |answer| uplink.answered(answer)
            }) {
                Ok(connected) => {
                    println!("Link: connected to {to}");
                    link = Some(connected);
                    failing_since_connected = false;
                    LinkEvent::Connected
                }
                Err(e) => {
                    let why = match e.downcast_ref::<IncompatibleReceiver>() {
                        Some(receiver) => KeyboardLink::Incompatible { keyboard: receiver.version, host: VERSION },
                        None => KeyboardLink::Unreachable,
                    };
                    if uplink.report_not_connected(why) || !failing_since_connected {
                        eprintln!("Link: can't use the keyboard ({e:#}); retrying until it answers");
                        failing_since_connected = true;
                    }
                    LinkEvent::ConnectFailed
                }
            },
            LinkState::Connected => {
                let connected = link.as_mut().expect("a connected Link has a socket");
                match send(connected, uplink, uplink.next_outgoing()) {
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

fn send(link: &mut Link, uplink: &Uplink, outgoing: Outgoing) -> Result<()> {
    let message = match outgoing {
        Outgoing::Control(message) => message,
        Outgoing::ModuleData { instance, payload } => Message::ModuleData { instance, payload },
        Outgoing::BackgroundData(payload) => Message::BackgroundData { payload },
        Outgoing::ModuleResource { instance, name, data } => Message::ModuleResource { instance, name, data },
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
        Outgoing::Put(upload) => return put_piece(link, uplink, upload),
    };
    link.send(&message)?;
    Ok(())
}

/// Sends the upload's next piece. A failed send loses the Link, which fails the upload with it.
fn put_piece(link: &mut Link, uplink: &Uplink, mut upload: Upload) -> Result<()> {
    let piece = match upload.next_piece() {
        Ok(piece) => piece,
        Err(e) => {
            upload.waiter.finish(Err(e));
            return Ok(());
        }
    };
    uplink.sending_piece(upload, piece.completes());
    link.send(&Message::PutFile(piece))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::LinkEffect::{DropQueued, Resend};
    use super::LinkEvent::{ConnectFailed, Connected as Up, Lost, RetryDue};
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
        let uplink = Uplink::new(|_| {}, || {});
        uplink.apply(Resend);
        uplink.put_module_data(0, vec![1]);
        uplink.push_stream(Message::StreamEnd { stream_id: 7 }).unwrap();
        uplink.select_background("a.b".into());
        assert!(matches!(uplink.next_outgoing(), Outgoing::Control(Message::SelectBackground { .. })));
        assert!(matches!(uplink.next_outgoing(), Outgoing::Stream(Message::StreamEnd { stream_id: 7 })));
        assert!(matches!(uplink.next_outgoing(), Outgoing::ModuleData { instance: 0, .. }));
    }

    #[test]
    fn files_go_after_widgets_and_fail_when_the_link_drops() {
        let path = std::env::temp_dir().join(format!("glassdeckd-upload-{}", std::process::id()));
        std::fs::write(&path, [7u8; 10]).unwrap();
        let uplink = Uplink::new(|_| {}, || {});
        assert!(uplink.put_file(FileTarget::Package("a.b.deck".into()), &path).is_err());
        uplink.apply(Resend);
        let first = uplink.put_file(FileTarget::Package("a.b.deck".into()), &path).unwrap();
        let second = uplink.put_file(FileTarget::Package("a.b.deck".into()), &path).unwrap();
        uplink.put_module_data(0, vec![1]);
        assert!(matches!(uplink.next_outgoing(), Outgoing::ModuleData { .. }));
        let Outgoing::Put(mut upload) = uplink.next_outgoing() else { panic!("expected the first upload") };
        let piece = upload.next_piece().unwrap();
        assert_eq!((piece.offset, piece.total, piece.data.len(), piece.completes()), (0, 10, 10, true));
        uplink.sending_piece(upload, true);
        uplink.answered(Message::PutDone { target: FileTarget::Package("a.b.deck".into()) });
        assert!(first.recv().unwrap().is_ok());
        uplink.apply(DropQueued);
        assert!(second.recv().unwrap().is_err());
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn resources_go_after_widget_data_latest_wins_and_come_back_after_a_reconnect() {
        let uplink = Uplink::new(|_| {}, || {});
        uplink.apply(Resend);
        uplink.put_module_resource(2, "cover".into(), vec![1]);
        uplink.put_module_resource(2, "cover".into(), vec![2]);
        uplink.put_module_data(2, vec![9]);
        assert!(matches!(uplink.next_outgoing(), Outgoing::ModuleData { instance: 2, .. }));
        assert!(
            matches!(uplink.next_outgoing(), Outgoing::ModuleResource { instance: 2, ref name, ref data } if name == "cover" && *data == vec![2])
        );
        uplink.apply(DropQueued);
        uplink.apply(Resend);
        assert!(matches!(uplink.next_outgoing(), Outgoing::ModuleData { instance: 2, .. }));
        assert!(matches!(uplink.next_outgoing(), Outgoing::ModuleResource { instance: 2, .. }));
    }

    #[test]
    fn a_widgets_newest_value_replaces_its_unsent_one() {
        let uplink = Uplink::new(|_| {}, || {});
        uplink.apply(Resend);
        uplink.put_module_data(3, vec![1]);
        uplink.put_module_data(3, vec![2]);
        assert!(matches!(uplink.next_outgoing(), Outgoing::ModuleData { instance: 3, payload } if payload == vec![2]));
    }

    #[test]
    fn reconnecting_resends_every_widget_and_the_selection() {
        let uplink = Uplink::new(|_| {}, || {});
        uplink.select_background("a.b".into());
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
