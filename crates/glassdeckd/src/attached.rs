//! Streams other programs attach over stream protocol v1 (`glassdeck-stream`). An attached stream
//! encodes only while someone watches it, the keyboard or a preview, and every watcher gets the same
//! bytes. Decisions are pure (`step`, `check_hello`, `VideoCheck`); `Attachments` does the I/O.

use std::collections::HashMap;
use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::Sender as ChangeSender;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use glassdeck_capture::link::new_stream_id;
use glassdeck_capture::pipeline::UNSYNCED_CLOCK_OFFSET_US;
use glassdeck_proto::{Codec, Frame, Message, Mode, StreamInfo};
use glassdeck_stream::{Hello, Mode as WireMode, StreamFormat, ToHost, ToProgram};
use tokio::sync::mpsc;

use crate::feeds::{ViewerFrame, Watching};
use crate::streams::StreamName;

/// The most the keyboard has been tested decoding; the panel itself is 1920x1080.
const MAX_SIZE: (u16, u16) = (2560, 1440);
/// Frames a watcher may fall behind by; past that it skips to the next keyframe.
const WATCHER_QUEUE: usize = 16;
const HELLO_WITHIN: Duration = Duration::from_secs(5);
const DETACHED: &str = "its program detached";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Watch {
    /// Attached, nobody watching: the program isn't encoding
    Attached,
    /// At least one watcher: the program encodes
    Active,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WatchEvent {
    Joined,
    Left { watchers_left: usize },
    VideoFailed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WatchEffect {
    Activate,
    Deactivate,
    Keyframe,
    CutWatchers,
}

/// Every move of an attached stream goes through here; the test walks the table.
pub fn step(state: Watch, event: WatchEvent) -> (Watch, Vec<WatchEffect>) {
    use Watch::{Active, Attached};
    use WatchEffect::{Activate, CutWatchers, Deactivate, Keyframe};
    match (state, event) {
        (Attached, WatchEvent::Joined) => (Active, vec![Activate]),
        (Active, WatchEvent::Joined) => (Active, vec![Keyframe]),
        (Active, WatchEvent::Left { watchers_left: 0 }) => (Attached, vec![Deactivate]),
        (Active, WatchEvent::Left { .. }) => (Active, vec![]),
        (Active, WatchEvent::VideoFailed) => (Attached, vec![Deactivate, CutWatchers]),
        (Attached, WatchEvent::Left { .. } | WatchEvent::VideoFailed) => (Attached, vec![]),
    }
}

/// The name a `Hello` may attach as, or why it's refused.
pub fn check_hello(hello: &Hello, token: &str, attached: &[&StreamName]) -> Result<StreamName, String> {
    if hello.token != token {
        return Err("wrong token; it's in api.json".to_owned());
    }
    if hello.mode == WireMode::Paced {
        return Err("paced streams aren't supported yet; attach as live".to_owned());
    }
    let name = StreamName::parse(&hello.name).map_err(|e| format!("{e:#}"))?;
    if name.is_reserved() {
        return Err(format!("{name}: names starting {} are for built-in streams", crate::streams::MONITOR_PREFIX));
    }
    if attached.contains(&&name) {
        return Err(format!("{name} is already attached"));
    }
    Ok(name)
}

#[derive(Debug, PartialEq, Eq)]
pub enum Checked {
    Pass,
    /// Frames still on their way after a `Deactivate`, which nobody watches
    DropUnwatched,
}

/// The edge checks on one connection's video.
#[derive(Debug, Default)]
pub struct VideoCheck {
    started: Option<StreamFormat>,
    needs_keyframe: bool,
    last: Option<(u32, u64)>,
}

impl VideoCheck {
    pub fn activated(&mut self) {
        self.needs_keyframe = true;
    }

    /// `Err` is why the stream's video is refused.
    pub fn check(&mut self, message: &ToHost, codec: glassdeck_stream::Codec, active: bool) -> Result<Checked, String> {
        match message {
            ToHost::Hello(_) => Err("a second Hello".to_owned()),
            ToHost::Still { .. } => Ok(Checked::Pass),
            ToHost::StreamStart(format) => {
                if format.codec != codec {
                    return Err(format!("StreamStart says {:?}, Hello said {codec:?}", format.codec));
                }
                let fits = (1..=MAX_SIZE.0).contains(&format.width) && (1..=MAX_SIZE.1).contains(&format.height);
                if !fits {
                    return Err(format!("{}x{} is outside 1x1 to {}x{}", format.width, format.height, MAX_SIZE.0, MAX_SIZE.1));
                }
                (self.started, self.needs_keyframe, self.last) = (Some(*format), true, None);
                Ok(Checked::Pass)
            }
            ToHost::Frame(frame) => {
                if !active {
                    return Ok(Checked::DropUnwatched);
                }
                if self.started.is_none() {
                    return Err("a frame before StreamStart".to_owned());
                }
                if self.needs_keyframe && !frame.keyframe {
                    return Err("the first frame after StreamStart or Activate isn't a keyframe".to_owned());
                }
                if let Some((seq, time_us)) = self.last {
                    if frame.seq <= seq {
                        return Err(format!("frame {} after frame {seq}", frame.seq));
                    }
                    if frame.time_us < time_us {
                        return Err(format!("frame {} went back in time", frame.seq));
                    }
                }
                (self.needs_keyframe, self.last) = (false, Some((frame.seq, frame.time_us)));
                Ok(Checked::Pass)
            }
            ToHost::StreamEnd => {
                self.started = None;
                Ok(Checked::Pass)
            }
        }
    }
}

fn proto_codec(codec: glassdeck_stream::Codec) -> Codec {
    match codec {
        glassdeck_stream::Codec::H264 => Codec::H264,
        glassdeck_stream::Codec::Hevc => Codec::Hevc,
    }
}

/// An attached stream has come or gone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    Attached(StreamName),
    Detached(StreamName),
}

type TapId = u64;

enum TapTo {
    /// The keyboard: whole messages, each picture after the stream's `StreamStart`
    Link(mpsc::Sender<Message>),
    Viewer(mpsc::Sender<ViewerFrame>),
}

struct Tap {
    id: TapId,
    to: TapTo,
    waiting_for_keyframe: bool,
    needs_start: bool,
}

struct Attachment {
    name: StreamName,
    codec: glassdeck_stream::Codec,
    control: TcpStream,
    watch: Watch,
    check: VideoCheck,
    still: Option<Arc<[u8]>>,
    /// The stream now, as the Link carries it
    started: Option<StreamInfo>,
    taps: Vec<Tap>,
}

impl Attachment {
    fn tell(&mut self, message: ToProgram) {
        // A program that's gone is noticed by its reader, which detaches it.
        let _ = self.control.write_all(&message.encode());
    }

    fn apply(&mut self, effects: Vec<WatchEffect>, reason: &str, cut: &mut HashMap<TapId, String>) {
        for effect in effects {
            match effect {
                WatchEffect::Activate => {
                    self.check.activated();
                    self.tell(ToProgram::Activate);
                }
                WatchEffect::Deactivate => self.tell(ToProgram::Deactivate),
                WatchEffect::Keyframe => self.tell(ToProgram::Keyframe),
                WatchEffect::CutWatchers => {
                    for tap in self.taps.drain(..) {
                        cut.insert(tap.id, reason.to_owned());
                    }
                }
            }
        }
    }

    fn move_on(&mut self, event: WatchEvent, reason: &str, cut: &mut HashMap<TapId, String>) {
        let (watch, effects) = step(self.watch, event);
        self.watch = watch;
        self.apply(effects, reason, cut);
    }

    fn fan_out(&mut self, message: ToHost) {
        match message {
            ToHost::Hello(_) => {}
            ToHost::Still { jpeg } => self.still = Some(jpeg.into()),
            ToHost::StreamStart(format) => {
                self.started = Some(StreamInfo {
                    stream_id: new_stream_id(),
                    codec: proto_codec(format.codec),
                    mode: Mode::Live { clock_offset_us: UNSYNCED_CLOCK_OFFSET_US },
                    width: format.width,
                    height: format.height,
                });
                for tap in &mut self.taps {
                    (tap.needs_start, tap.waiting_for_keyframe) = (true, true);
                }
            }
            ToHost::Frame(frame) => {
                let Some(info) = self.started else { return };
                let data: Arc<[u8]> = frame.payload.into();
                for tap in self.taps.iter_mut().filter(|t| frame.keyframe || !t.waiting_for_keyframe) {
                    let sent = match &tap.to {
                        TapTo::Link(to) => {
                            let started = !tap.needs_start || to.try_send(Message::StreamStart(info)).is_ok();
                            tap.needs_start &= !started;
                            started
                                && to
                                    .try_send(Message::Frame(Frame {
                                        stream_id: info.stream_id,
                                        seq: frame.seq,
                                        time_us: frame.time_us,
                                        keyframe: frame.keyframe,
                                        payload: data.to_vec(),
                                    }))
                                    .is_ok()
                        }
                        TapTo::Viewer(to) => to.try_send(ViewerFrame { time_us: frame.time_us, data: Arc::clone(&data) }).is_ok(),
                    };
                    tap.waiting_for_keyframe = !sent;
                }
            }
            ToHost::StreamEnd => {
                let Some(info) = self.started.take() else { return };
                for tap in &mut self.taps {
                    if let TapTo::Link(to) = &tap.to
                        && !tap.needs_start
                    {
                        let _ = to.try_send(Message::StreamEnd { stream_id: info.stream_id });
                    }
                    (tap.needs_start, tap.waiting_for_keyframe) = (true, true);
                }
            }
        }
    }
}

#[derive(Default)]
struct Registry {
    streams: Vec<Attachment>,
    next_tap: TapId,
    /// Why a watcher was cut off, for the one that asks
    cut: HashMap<TapId, String>,
}

impl Registry {
    fn find(&mut self, name: &StreamName) -> Option<&mut Attachment> {
        self.streams.iter_mut().find(|a| a.name == *name)
    }
}

pub struct Attachments {
    token: String,
    registry: Mutex<Registry>,
    changes: Mutex<ChangeSender<Change>>,
}

/// A watcher's hold on an attached stream; dropping it leaves.
pub struct Tapped {
    attachments: Arc<Attachments>,
    name: StreamName,
    id: TapId,
}

impl Watching for Tapped {
    fn ask_keyframe(&self) {
        let mut registry = self.attachments.lock();
        if let Some(attachment) = registry.find(&self.name).filter(|a| a.watch == Watch::Active) {
            attachment.tell(ToProgram::Keyframe);
        }
    }
}

impl Drop for Tapped {
    fn drop(&mut self) {
        self.attachments.leave(&self.name, self.id);
    }
}

impl Attachments {
    pub fn new(token: String, changes: ChangeSender<Change>) -> Arc<Self> {
        Arc::new(Self { token, registry: Mutex::default(), changes: Mutex::new(changes) })
    }

    fn lock(&self) -> MutexGuard<'_, Registry> {
        self.registry.lock().expect("attachments poisoned")
    }

    fn changed(&self, change: Change) {
        let _ = self.changes.lock().expect("changes poisoned").send(change);
    }

    /// Takes programs on `listener` forever, each on its own thread.
    pub fn serve(self: Arc<Self>, listener: TcpListener) {
        for socket in listener.incoming() {
            let socket = match socket {
                Ok(socket) => socket,
                Err(e) => {
                    eprintln!("error: accepting a stream program: {e}");
                    continue;
                }
            };
            let attachments = Arc::clone(&self);
            let spawned = thread::Builder::new().name("attached stream".into()).spawn(move || {
                if let Err(e) = attachments.attend(socket) {
                    eprintln!("error: an attached stream: {e:#}");
                }
            });
            if let Err(e) = spawned {
                eprintln!("error: starting a thread for a stream program: {e}");
            }
        }
    }

    fn attend(&self, mut socket: TcpStream) -> Result<()> {
        socket.set_nodelay(true)?;
        socket.set_read_timeout(Some(HELLO_WITHIN))?;
        let Some(ToHost::Hello(hello)) = ToHost::read_from(&mut socket).context("reading Hello")? else {
            bail!("a stream program sent something other than Hello first");
        };
        socket.set_read_timeout(None)?;
        let name = {
            let mut registry = self.lock();
            let attached: Vec<&StreamName> = registry.streams.iter().map(|a| &a.name).collect();
            match check_hello(&hello, &self.token, &attached) {
                Ok(name) => {
                    socket.write_all(&ToProgram::Welcome.encode())?;
                    registry.streams.push(Attachment {
                        name: name.clone(),
                        codec: hello.codec,
                        control: socket.try_clone()?,
                        watch: Watch::Attached,
                        check: VideoCheck::default(),
                        still: None,
                        started: None,
                        taps: Vec::new(),
                    });
                    name
                }
                Err(reason) => {
                    println!("Streams: refused {:?}: {reason}", hello.name);
                    socket.write_all(&ToProgram::Refused { reason }.encode())?;
                    return Ok(());
                }
            }
        };
        println!("Streams: {name} attached");
        self.changed(Change::Attached(name.clone()));
        let ended = self.relay(&name, &mut socket);
        self.detach(&name);
        println!("Streams: {name} detached");
        self.changed(Change::Detached(name));
        ended
    }

    fn relay(&self, name: &StreamName, socket: &mut TcpStream) -> Result<()> {
        loop {
            let Some(message) = ToHost::read_from(socket)? else { return Ok(()) };
            self.received(name, message);
        }
    }

    fn received(&self, name: &StreamName, message: ToHost) {
        let mut registry = self.lock();
        let Registry { streams, cut, .. } = &mut *registry;
        let Some(attachment) = streams.iter_mut().find(|a| a.name == *name) else { return };
        match attachment.check.check(&message, attachment.codec, attachment.watch == Watch::Active) {
            Ok(Checked::Pass) => attachment.fan_out(message),
            Ok(Checked::DropUnwatched) => {}
            Err(reason) => {
                eprintln!("error: stream {name}: {reason}; its watchers are cut off");
                attachment.started = None;
                attachment.move_on(WatchEvent::VideoFailed, &format!("its video was refused: {reason}"), cut);
            }
        }
    }

    fn detach(&self, name: &StreamName) {
        let mut registry = self.lock();
        let Some(at) = registry.streams.iter().position(|a| a.name == *name) else { return };
        let attachment = registry.streams.remove(at);
        for tap in attachment.taps {
            registry.cut.insert(tap.id, DETACHED.to_owned());
        }
    }

    fn join(self: &Arc<Self>, name: &StreamName, to: TapTo) -> Option<Tapped> {
        let mut registry = self.lock();
        let id = registry.next_tap;
        registry.next_tap += 1;
        let Registry { streams, cut, .. } = &mut *registry;
        let attachment = streams.iter_mut().find(|a| a.name == *name)?;
        attachment.taps.push(Tap { id, to, waiting_for_keyframe: true, needs_start: true });
        attachment.move_on(WatchEvent::Joined, "", cut);
        Some(Tapped { attachments: Arc::clone(self), name: name.clone(), id })
    }

    fn leave(&self, name: &StreamName, tap: TapId) {
        let mut registry = self.lock();
        registry.cut.remove(&tap);
        let Registry { streams, cut, .. } = &mut *registry;
        let Some(attachment) = streams.iter_mut().find(|a| a.name == *name) else { return };
        let before = attachment.taps.len();
        attachment.taps.retain(|t| t.id != tap);
        if attachment.taps.len() < before {
            let watchers_left = attachment.taps.len();
            attachment.move_on(WatchEvent::Left { watchers_left }, "", cut);
        }
    }

    /// The keyboard's watch on `name`, or `None` when it isn't attached.
    pub fn tap_link(self: &Arc<Self>, name: &StreamName) -> Option<(Tapped, mpsc::Receiver<Message>)> {
        let (to, frames) = mpsc::channel(WATCHER_QUEUE);
        Some((self.join(name, TapTo::Link(to))?, frames))
    }

    /// A preview's watch on `name`; its frames end at once when `name` isn't attached.
    pub fn tap_viewer(self: &Arc<Self>, name: &StreamName) -> (Option<Tapped>, mpsc::Receiver<ViewerFrame>) {
        let (to, frames) = mpsc::channel(WATCHER_QUEUE);
        (self.join(name, TapTo::Viewer(to)), frames)
    }

    /// Asks why the stream stopped feeding `tapped`, when glassdeckd cut it off.
    pub fn cut_reasons(self: &Arc<Self>, tapped: &Tapped) -> impl FnOnce() -> Option<String> + Send + 'static {
        let (attachments, id) = (Arc::clone(self), tapped.id);
        move || attachments.lock().cut.remove(&id)
    }

    pub fn list(&self) -> Vec<(StreamName, Codec)> {
        self.lock().streams.iter().map(|a| (a.name.clone(), proto_codec(a.codec))).collect()
    }

    pub fn still(&self, name: &StreamName) -> Option<Arc<[u8]>> {
        self.lock().find(name)?.still.clone()
    }
}

impl Watching for Option<Tapped> {
    fn ask_keyframe(&self) {
        if let Some(tapped) = self {
            tapped.ask_keyframe();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glassdeck_stream::Codec as WireCodec;
    use glassdeck_stream::Frame as WireFrame;

    #[test]
    fn walks_the_whole_watch_table() {
        use Watch::{Active, Attached};
        use WatchEffect::{Activate, CutWatchers, Deactivate, Keyframe};
        let table = [
            (Attached, WatchEvent::Joined, Active, vec![Activate]),
            (Active, WatchEvent::Joined, Active, vec![Keyframe]),
            (Active, WatchEvent::Left { watchers_left: 0 }, Attached, vec![Deactivate]),
            (Active, WatchEvent::Left { watchers_left: 2 }, Active, vec![]),
            (Active, WatchEvent::VideoFailed, Attached, vec![Deactivate, CutWatchers]),
            (Attached, WatchEvent::Left { watchers_left: 0 }, Attached, vec![]),
            (Attached, WatchEvent::VideoFailed, Attached, vec![]),
        ];
        for (from, event, to, effects) in table {
            assert_eq!(step(from, event), (to, effects), "from {from:?} on {event:?}");
        }
    }

    fn hello(name: &str) -> Hello {
        Hello { token: "t".into(), name: name.into(), mode: WireMode::Live, codec: WireCodec::H264 }
    }

    #[test]
    fn hello_needs_the_token_a_free_plain_name_and_live() {
        let taken = StreamName::parse("taken").unwrap();
        assert_eq!(check_hello(&hello("wallpaper-engine"), "t", &[&taken]).unwrap().as_str(), "wallpaper-engine");
        assert!(check_hello(&hello("wallpaper-engine"), "other", &[]).unwrap_err().contains("token"));
        assert!(check_hello(&hello("taken"), "t", &[&taken]).unwrap_err().contains("already attached"));
        assert!(check_hello(&hello("monitor-q27"), "t", &[]).unwrap_err().contains("built-in"));
        assert!(check_hello(&hello("Bad Name"), "t", &[]).is_err());
        let paced = Hello { mode: WireMode::Paced, ..hello("x") };
        assert!(check_hello(&paced, "t", &[]).unwrap_err().contains("paced"));
    }

    fn start(width: u16, height: u16) -> ToHost {
        ToHost::StreamStart(StreamFormat { codec: WireCodec::H264, width, height })
    }

    fn frame(seq: u32, time_us: u64, keyframe: bool) -> ToHost {
        ToHost::Frame(WireFrame { seq, time_us, keyframe, payload: vec![0, 0, 0, 1] })
    }

    #[test]
    fn video_passes_when_it_starts_on_a_keyframe_and_goes_forward() {
        let mut check = VideoCheck::default();
        check.activated();
        for message in [start(1920, 1080), frame(1, 10, true), frame(2, 20, false), frame(3, 20, false), ToHost::StreamEnd, start(1920, 1080), frame(1, 5, true)] {
            assert_eq!(check.check(&message, WireCodec::H264, true), Ok(Checked::Pass), "{message:?}");
        }
    }

    #[test]
    fn video_is_refused_when_it_breaks_a_rule() {
        let refused = |messages: &[ToHost]| {
            let mut check = VideoCheck::default();
            check.activated();
            messages.iter().map(|m| check.check(m, WireCodec::H264, true)).find(Result::is_err).is_some()
        };
        assert!(refused(&[frame(1, 1, true)]), "a frame before StreamStart");
        assert!(refused(&[start(1920, 1080), frame(1, 1, false)]), "no keyframe first");
        assert!(refused(&[start(1920, 1080), frame(2, 1, true), frame(2, 2, false)]), "seq repeats");
        assert!(refused(&[start(1920, 1080), frame(1, 9, true), frame(2, 8, false)]), "time goes back");
        assert!(refused(&[start(3840, 2160)]), "too big");
        assert!(refused(&[start(0, 1080)]), "empty");
        assert!(refused(&[ToHost::StreamStart(StreamFormat { codec: WireCodec::Hevc, width: 1920, height: 1080 })]), "not the Hello's codec");
        assert!(refused(&[ToHost::Hello(hello("x"))]), "a second Hello");
    }

    fn serving() -> (Arc<Attachments>, std::net::SocketAddr, std::sync::mpsc::Receiver<Change>) {
        let (changes, changed) = std::sync::mpsc::channel();
        let attachments = Attachments::new("t".into(), changes);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let serving = Arc::clone(&attachments);
        thread::spawn(move || serving.serve(listener));
        (attachments, address, changed)
    }

    fn picture(seq: u32, keyframe: bool) -> WireFrame {
        WireFrame { seq, time_us: u64::from(seq) * 1000, keyframe, payload: vec![0, 0, 0, 1, seq as u8] }
    }

    #[test]
    fn an_attached_program_encodes_only_while_watched_and_every_watcher_gets_its_frames() {
        use glassdeck_stream::Control;
        let (attachments, address, changed) = serving();
        let name = StreamName::parse("test-pattern").unwrap();
        let (mut controls, sender) = glassdeck_stream::attach(address, "t", name.as_str(), WireMode::Live, WireCodec::H264).unwrap();
        assert_eq!(changed.recv().unwrap(), Change::Attached(name.clone()));
        assert_eq!(attachments.list(), vec![(name.clone(), Codec::H264)]);

        let refused = glassdeck_stream::attach(address, "t", name.as_str(), WireMode::Live, WireCodec::H264);
        assert!(matches!(refused, Err(glassdeck_stream::Error::Refused(reason)) if reason.contains("already attached")));

        sender.still(vec![0xff, 0xd8]).unwrap();
        let (keyboard, mut link) = attachments.tap_link(&name).unwrap();
        assert_eq!(controls.recv().unwrap(), Some(Control::Activate));
        sender.start(StreamFormat { codec: WireCodec::H264, width: 1920, height: 1080 }).unwrap();
        sender.frame(picture(1, true)).unwrap();
        let Some(Message::StreamStart(info)) = link.blocking_recv() else { panic!("StreamStart first") };
        assert_eq!((info.codec, info.width, info.height), (Codec::H264, 1920, 1080));
        assert!(matches!(link.blocking_recv(), Some(Message::Frame(f)) if f.keyframe && f.stream_id == info.stream_id && f.seq == 1));
        assert_eq!(attachments.still(&name).as_deref(), Some(&[0xff, 0xd8][..]));

        let (preview, mut viewer) = attachments.tap_viewer(&name);
        assert_eq!(controls.recv().unwrap(), Some(Control::Keyframe));
        sender.frame(picture(2, false)).unwrap();
        sender.frame(picture(3, true)).unwrap();
        assert!(matches!(link.blocking_recv(), Some(Message::Frame(f)) if f.seq == 2));
        assert!(matches!(link.blocking_recv(), Some(Message::Frame(f)) if f.seq == 3));
        assert_eq!(viewer.blocking_recv().unwrap().time_us, 3000, "a new viewer starts at a keyframe");

        drop(preview);
        drop(keyboard);
        assert_eq!(controls.recv().unwrap(), Some(Control::Deactivate));

        let (keyboard, mut link) = attachments.tap_link(&name).unwrap();
        let reasons = attachments.cut_reasons(&keyboard);
        assert_eq!(controls.recv().unwrap(), Some(Control::Activate));
        drop((controls, sender));
        assert_eq!(changed.recv().unwrap(), Change::Detached(name.clone()));
        assert!(link.blocking_recv().is_none(), "a detached stream stops feeding its watchers");
        assert_eq!(reasons().as_deref(), Some(DETACHED));
        assert!(attachments.list().is_empty());
        drop(keyboard);
    }

    #[test]
    fn video_that_breaks_a_rule_cuts_its_watchers_off() {
        use glassdeck_stream::Control;
        let (attachments, address, changed) = serving();
        let name = StreamName::parse("bad").unwrap();
        let (mut controls, sender) = glassdeck_stream::attach(address, "t", "bad", WireMode::Live, WireCodec::H264).unwrap();
        changed.recv().unwrap();
        let (keyboard, mut link) = attachments.tap_link(&name).unwrap();
        let reasons = attachments.cut_reasons(&keyboard);
        assert_eq!(controls.recv().unwrap(), Some(Control::Activate));
        sender.start(StreamFormat { codec: WireCodec::H264, width: 1920, height: 1080 }).unwrap();
        sender.frame(picture(1, false)).unwrap();
        assert_eq!(controls.recv().unwrap(), Some(Control::Deactivate));
        assert!(link.blocking_recv().is_none());
        assert!(reasons().unwrap().contains("isn't a keyframe"));
        assert_eq!(attachments.list().len(), 1, "it stays attached");
    }

    #[test]
    fn a_wrong_token_is_refused() {
        let (_attachments, address, _changed) = serving();
        let refused = glassdeck_stream::attach(address, "nope", "x", WireMode::Live, WireCodec::H264);
        assert!(matches!(refused, Err(glassdeck_stream::Error::Refused(reason)) if reason.contains("token")));
    }

    #[test]
    fn frames_after_deactivate_are_dropped_and_activate_wants_a_keyframe() {
        let mut check = VideoCheck::default();
        check.activated();
        assert_eq!(check.check(&start(1920, 1080), WireCodec::H264, true), Ok(Checked::Pass));
        assert_eq!(check.check(&frame(1, 1, true), WireCodec::H264, true), Ok(Checked::Pass));
        assert_eq!(check.check(&frame(2, 2, false), WireCodec::H264, false), Ok(Checked::DropUnwatched));
        check.activated();
        assert!(check.check(&frame(3, 3, false), WireCodec::H264, true).is_err());
    }
}
