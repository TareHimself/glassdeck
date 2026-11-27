//! Who's watching which source, and the encoders that feed them. A source the keyboard is playing feeds
//! its viewers from the keyboard's own encoder; any other watched source gets a preview worker of its
//! own, which stops when its last viewer leaves or the keyboard starts playing it.

use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::Result;
use glassdeck_capture::sources::SourceRequest;
use glassdeck_proto::Message;
use tokio::sync::mpsc;

use crate::worker::WorkerProcess;

/// Frames a viewer may fall behind by; past that it skips to the next keyframe.
const VIEWER_QUEUE: usize = 8;

type ViewerId = u64;

/// A viewer's hold on its source's frames.
pub trait Watching: Send + 'static {
    /// For a viewer that just joined or lost its picture (a WebRTC PLI)
    fn ask_keyframe(&self);
}

/// Watching a source through `Feeds`; dropping it leaves.
pub struct Viewing {
    feeds: Arc<Feeds>,
    id: ViewerId,
}

impl Watching for Viewing {
    fn ask_keyframe(&self) {
        self.feeds.ask_keyframe(self.id);
    }
}

impl Drop for Viewing {
    fn drop(&mut self) {
        self.feeds.leave(self.id);
    }
}

pub struct ViewerFrame {
    /// Capture time, microseconds
    pub time_us: u64,
    pub data: Arc<[u8]>,
}

pub struct Feeds {
    token: String,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    next_viewer: ViewerId,
    next_preview: u64,
    viewers: Vec<Viewer>,
    keyboard: Option<Feeding>,
    previews: Vec<Feeding>,
}

struct Feeding {
    process: Arc<WorkerProcess>,
    request: SourceRequest,
}

struct Viewer {
    id: ViewerId,
    request: SourceRequest,
    frames: mpsc::Sender<ViewerFrame>,
    waiting_for_keyframe: bool,
}

impl State {
    /// The worker whose frames `request`'s viewers get now.
    fn source_of(&self, request: &SourceRequest) -> Option<&Arc<WorkerProcess>> {
        match &self.keyboard {
            Some(keyboard) if keyboard.request == *request => Some(&keyboard.process),
            _ => self.previews.iter().find(|p| p.request == *request).map(|p| &p.process),
        }
    }

    /// The request `process` feeds viewers of, if it's the one they get frames from now.
    fn served_by(&self, process: &Arc<WorkerProcess>) -> Option<SourceRequest> {
        let feeding = self.keyboard.iter().chain(&self.previews).find(|f| Arc::ptr_eq(&f.process, process))?;
        self.source_of(&feeding.request).filter(|current| Arc::ptr_eq(current, process)).map(|_| feeding.request.clone())
    }

    fn wait_for_keyframe(&mut self, request: &SourceRequest) {
        for viewer in self.viewers.iter_mut().filter(|v| v.request == *request) {
            viewer.waiting_for_keyframe = true;
        }
    }

    /// Closing a viewer's frames ends its session.
    fn drop_viewers_of(&mut self, request: &SourceRequest, why: &str) {
        let before = self.viewers.len();
        self.viewers.retain(|v| v.request != *request);
        if self.viewers.len() < before {
            eprintln!("error: {} viewers of {} cut off: {why}", before - self.viewers.len(), request.play);
        }
    }
}

impl Feeds {
    pub fn new(token: String) -> Arc<Self> {
        Arc::new(Self { token, state: Mutex::new(State::default()) })
    }

    /// A new viewer of `request`: its frames start at the next keyframe, which is asked for now.
    pub fn watch(self: &Arc<Self>, request: SourceRequest) -> (Viewing, mpsc::Receiver<ViewerFrame>) {
        let (frames, received) = mpsc::channel(VIEWER_QUEUE);
        let mut state = self.lock();
        let id = state.next_viewer;
        state.next_viewer += 1;
        if let Some(source) = state.source_of(&request) {
            source.ask_keyframe();
        }
        state.viewers.push(Viewer { id, request, frames, waiting_for_keyframe: true });
        self.reconcile(&mut state);
        (Viewing { feeds: Arc::clone(self), id }, received)
    }

    fn leave(self: &Arc<Self>, viewer: ViewerId) {
        let mut state = self.lock();
        state.viewers.retain(|v| v.id != viewer);
        self.reconcile(&mut state);
    }

    fn ask_keyframe(&self, viewer: ViewerId) {
        let state = self.lock();
        if let Some(source) = state.viewers.iter().find(|v| v.id == viewer).and_then(|v| state.source_of(&v.request)) {
            source.ask_keyframe();
        }
    }

    /// The keyboard's worker for `request` is streaming; its viewers move over to it.
    pub fn keyboard_started(self: &Arc<Self>, process: &Arc<WorkerProcess>, request: &SourceRequest) {
        let mut state = self.lock();
        state.keyboard = Some(Feeding { process: Arc::clone(process), request: request.clone() });
        state.wait_for_keyframe(request);
        self.reconcile(&mut state);
    }

    pub fn keyboard_ended(self: &Arc<Self>, process: &Arc<WorkerProcess>) {
        let mut state = self.lock();
        let Some(keyboard) = state.keyboard.take_if(|k| Arc::ptr_eq(&k.process, process)) else { return };
        state.wait_for_keyframe(&keyboard.request);
        self.reconcile(&mut state);
    }

    /// Hands a worker's frame to the viewers it feeds; a viewer that's behind skips to the next keyframe.
    pub fn publish(&self, process: &Arc<WorkerProcess>, message: &Message) {
        let Message::Frame(frame) = message else { return };
        let mut state = self.lock();
        let Some(request) = state.served_by(process) else { return };
        let mut data: Option<Arc<[u8]>> = None;
        for viewer in state.viewers.iter_mut().filter(|v| v.request == request) {
            if !takes_frame(viewer.waiting_for_keyframe, frame.keyframe) {
                continue;
            }
            let data = data.get_or_insert_with(|| frame.payload.as_slice().into());
            let sent = viewer.frames.try_send(ViewerFrame { time_us: frame.time_us, data: Arc::clone(data) });
            viewer.waiting_for_keyframe = sent.is_err();
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().expect("feeds poisoned")
    }

    /// Starts and stops preview workers to match who's watching what.
    fn reconcile(self: &Arc<Self>, state: &mut State) {
        let watched: Vec<&SourceRequest> = state.viewers.iter().map(|v| &v.request).collect();
        let needed = previews_needed(&watched, state.keyboard.as_ref().map(|k| &k.request));
        let running: Vec<SourceRequest> = state.previews.iter().map(|p| p.request.clone()).collect();
        let (start, stop) = preview_changes(&needed, &running);
        for request in stop {
            let at = state.previews.iter().position(|p| p.request == request).expect("a running preview");
            let stopped = state.previews.remove(at);
            thread::spawn(move || stopped.process.stop());
        }
        for request in start {
            if let Err(e) = self.start_preview(state, &request) {
                state.drop_viewers_of(&request, &format!("{e:#}"));
            }
        }
    }

    fn start_preview(self: &Arc<Self>, state: &mut State, request: &SourceRequest) -> Result<()> {
        state.next_preview += 1;
        let (process, listener) = WorkerProcess::spawn(format!("Preview {}", state.next_preview), request, &self.token)?;
        state.previews.push(Feeding { process: Arc::clone(&process), request: request.clone() });
        let feeds = Arc::clone(self);
        thread::Builder::new().name(process.label.to_lowercase()).spawn(move || {
            let failure = match process.accept(listener, &feeds.token) {
                Ok(mut socket) => {
                    loop {
                        match Message::read_from(&mut socket) {
                            Ok(Some(message)) => feeds.publish(&process, &message),
                            Ok(None) => break,
                            Err(e) => {
                                eprintln!("{}: reading from the worker: {e}", process.label);
                                break;
                            }
                        }
                    }
                    let exit = process.wait();
                    (!exit.success()).then(|| process.last_error().unwrap_or_else(|| format!("the worker exited with {exit}")))
                }
                Err(e) => {
                    process.kill();
                    Some(format!("{e:#}"))
                }
            };
            feeds.preview_ended(&process, failure);
        })?;
        Ok(())
    }

    /// A preview that ended without being asked to takes its viewers with it.
    fn preview_ended(&self, process: &Arc<WorkerProcess>, failure: Option<String>) {
        let mut state = self.lock();
        let Some(at) = state.previews.iter().position(|p| Arc::ptr_eq(&p.process, process)) else { return };
        let ended = state.previews.remove(at);
        state.drop_viewers_of(&ended.request, failure.as_deref().unwrap_or("its source ended"));
    }
}

/// A viewer waiting for a keyframe skips everything else.
fn takes_frame(waiting_for_keyframe: bool, keyframe: bool) -> bool {
    keyframe || !waiting_for_keyframe
}

/// Every watched source the keyboard isn't playing needs a preview worker, one each.
fn previews_needed(watched: &[&SourceRequest], keyboard: Option<&SourceRequest>) -> Vec<SourceRequest> {
    let mut needed: Vec<SourceRequest> = Vec::new();
    for request in watched {
        if Some(*request) != keyboard && !needed.contains(request) {
            needed.push((*request).clone());
        }
    }
    needed
}

/// The previews to start and to stop to get from `running` to `needed`.
fn preview_changes(needed: &[SourceRequest], running: &[SourceRequest]) -> (Vec<SourceRequest>, Vec<SourceRequest>) {
    let start = needed.iter().filter(|r| !running.contains(r)).cloned().collect();
    let stop = running.iter().filter(|r| !needed.contains(r)).cloned().collect();
    (start, stop)
}

#[cfg(test)]
mod tests {
    use super::*;
    use glassdeck_capture::sources::Play;

    fn request(name: &str) -> SourceRequest {
        SourceRequest { play: Play::File { path: name.into(), looping: false }, seconds: None }
    }

    #[test]
    fn previews_run_for_watched_sources_the_keyboard_isnt_playing() {
        let (a, b) = (request("a"), request("b"));
        assert_eq!(previews_needed(&[], None), vec![]);
        assert_eq!(previews_needed(&[&a, &a, &b], None), vec![a.clone(), b.clone()]);
        assert_eq!(previews_needed(&[&a, &b], Some(&a)), vec![b.clone()]);
        assert_eq!(previews_needed(&[&a], Some(&a)), vec![]);
    }

    #[test]
    fn changes_start_whats_missing_and_stop_whats_unwatched() {
        let (a, b, c) = (request("a"), request("b"), request("c"));
        assert_eq!(preview_changes(&[a.clone(), b.clone()], &[b.clone(), c.clone()]), (vec![a.clone()], vec![c.clone()]));
        assert_eq!(preview_changes(&[a.clone()], &[a.clone()]), (vec![], vec![]));
        assert_eq!(preview_changes(&[], &[a.clone()]), (vec![], vec![a]));
    }

    #[test]
    fn a_waiting_viewer_takes_only_a_keyframe() {
        assert!(takes_frame(false, false));
        assert!(takes_frame(false, true));
        assert!(takes_frame(true, true));
        assert!(!takes_frame(true, false));
    }
}
