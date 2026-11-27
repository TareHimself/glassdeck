//! At most one source at a time on the keyboard, each in its own worker process. Starting another
//! switches to it; a source cut off by the Link going down starts again when the keyboard is back.

use std::collections::HashMap;
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Instant;

use anyhow::{Context, Result};
use glassdeck_api::{Ended, StreamState, StreamStatus};
use glassdeck_capture::sources::SourceRequest;
use glassdeck_proto::Message;

use crate::feeds::Feeds;
use crate::link::Uplink;
use crate::worker::WorkerProcess;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunnerState {
    Idle,
    Running { request: SourceRequest, worker: u64 },
    /// The Link dropped mid-stream; this starts again once the keyboard is back.
    WaitingForLink { request: SourceRequest },
    /// A large file is going to the keyboard, which shows its own background meanwhile; this starts
    /// again when the upload is answered.
    PausedForUpload { request: SourceRequest },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Finished,
    Failed(String),
    LinkLost,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunnerEvent {
    Start(SourceRequest),
    Stop,
    WorkerEnded { worker: u64, outcome: Outcome },
    LinkUp,
    PauseForUpload,
    ResumeAfterUpload,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunnerEffect {
    Spawn { worker: u64, request: SourceRequest },
    StopWorker(u64),
    Ended { worker: u64, outcome: Outcome },
}

/// Every move between runner states goes through here; the test walks the table. `next_worker`
/// numbers a worker this move would start.
pub fn step(state: RunnerState, event: RunnerEvent, next_worker: u64) -> (RunnerState, Vec<RunnerEffect>) {
    use RunnerEffect::{Ended, Spawn, StopWorker};
    use RunnerState::{Idle, PausedForUpload, Running, WaitingForLink};
    let spawn = |request: SourceRequest| (Running { request: request.clone(), worker: next_worker }, Spawn { worker: next_worker, request });
    match (state, event) {
        (PausedForUpload { .. }, RunnerEvent::Start(request)) => (PausedForUpload { request }, vec![]),
        (Running { request, worker }, RunnerEvent::PauseForUpload) => (PausedForUpload { request }, vec![StopWorker(worker)]),
        (PausedForUpload { request }, RunnerEvent::ResumeAfterUpload) => {
            let (state, effect) = spawn(request);
            (state, vec![effect])
        }
        (state, RunnerEvent::PauseForUpload | RunnerEvent::ResumeAfterUpload) => (state, vec![]),
        (Idle | WaitingForLink { .. }, RunnerEvent::Start(request)) => {
            let (state, effect) = spawn(request);
            (state, vec![effect])
        }
        (Running { worker, .. }, RunnerEvent::Start(request)) => {
            let (state, effect) = spawn(request);
            (state, vec![StopWorker(worker), effect])
        }
        (Running { worker, .. }, RunnerEvent::Stop) => (Idle, vec![StopWorker(worker)]),
        (Idle | WaitingForLink { .. } | PausedForUpload { .. }, RunnerEvent::Stop) => (Idle, vec![]),
        (Running { request, worker }, RunnerEvent::WorkerEnded { worker: ended, outcome }) if ended == worker => match outcome {
            Outcome::LinkLost => (WaitingForLink { request }, vec![Ended { worker, outcome: Outcome::LinkLost }]),
            outcome => (Idle, vec![Ended { worker, outcome }]),
        },
        (WaitingForLink { request }, RunnerEvent::LinkUp) => {
            let (state, effect) = spawn(request);
            (state, vec![effect])
        }
        (state, RunnerEvent::WorkerEnded { .. } | RunnerEvent::LinkUp) => (state, vec![]),
    }
}

/// The runner's status, which waiters block on and every change of is published.
pub struct StatusBoard {
    status: Mutex<StreamStatus>,
    changed: Condvar,
    publish: Box<dyn Fn(&StreamStatus) + Send + Sync>,
}

impl StatusBoard {
    pub fn new(publish: impl Fn(&StreamStatus) + Send + Sync + 'static) -> Self {
        Self { status: Mutex::new(StreamStatus::default()), changed: Condvar::new(), publish: Box::new(publish) }
    }

    pub fn get(&self) -> StreamStatus {
        self.status.lock().expect("status poisoned").clone()
    }

    fn update(&self, change: impl FnOnce(&mut StreamStatus)) {
        let mut status = self.status.lock().expect("status poisoned");
        let before = status.clone();
        change(&mut status);
        if *status != before {
            self.changed.notify_all();
            (self.publish)(&status);
        }
    }

    /// Waits until `check` finds what it looks for in the status, or `deadline` passes.
    pub fn wait_for<T>(&self, deadline: Instant, mut check: impl FnMut(&StreamStatus) -> Option<T>) -> Option<T> {
        let mut status = self.status.lock().expect("status poisoned");
        loop {
            if let Some(found) = check(&status) {
                return Some(found);
            }
            let left = deadline.checked_duration_since(Instant::now())?;
            status = self.changed.wait_timeout(status, left).expect("status poisoned").0;
        }
    }
}

#[derive(Clone)]
pub struct Runner {
    events: Sender<RunnerEvent>,
    pub status: Arc<StatusBoard>,
}

impl Runner {
    /// `events` and `inbox` are one channel, made by the caller so the Link can report LinkUp into it.
    pub fn start(
        uplink: Arc<Uplink>,
        token: String,
        events: Sender<RunnerEvent>,
        inbox: Receiver<RunnerEvent>,
        status: Arc<StatusBoard>,
        feeds: Arc<Feeds>,
    ) -> Result<Self> {
        let runner = Self { events: events.clone(), status: Arc::clone(&status) };
        let looped = events.clone();
        thread::Builder::new()
            .name("sources".into())
            .spawn(move || Loop { uplink, token, status, events: looped, feeds, workers: HashMap::new() }.run(inbox))?;
        Ok(runner)
    }

    pub fn send(&self, event: RunnerEvent) -> Result<()> {
        self.events.send(event).context("the source runner has stopped")
    }

    pub fn status(&self) -> StreamStatus {
        self.status.get()
    }
}

struct Loop {
    uplink: Arc<Uplink>,
    token: String,
    status: Arc<StatusBoard>,
    events: Sender<RunnerEvent>,
    feeds: Arc<Feeds>,
    workers: HashMap<u64, Arc<WorkerProcess>>,
}

impl Loop {
    fn run(mut self, inbox: Receiver<RunnerEvent>) {
        let mut state = RunnerState::Idle;
        let mut next_worker = 1;
        for event in inbox {
            if let RunnerEvent::WorkerEnded { worker, .. } = event
                && let Some(process) = self.workers.remove(&worker)
            {
                self.feeds.keyboard_ended(&process);
            }
            let (next, effects) = step(state, event, next_worker);
            state = next;
            for effect in effects {
                match effect {
                    RunnerEffect::Spawn { worker, request } => {
                        next_worker = worker + 1;
                        if let Err(e) = self.spawn(worker, request) {
                            let _ = self.events.send(RunnerEvent::WorkerEnded { worker, outcome: Outcome::Failed(format!("{e:#}")) });
                        }
                    }
                    RunnerEffect::StopWorker(worker) => {
                        if let Some(process) = self.workers.get(&worker) {
                            process.stop();
                        }
                    }
                    RunnerEffect::Ended { worker, outcome } => {
                        let said = match outcome {
                            Outcome::Finished => "finished".to_owned(),
                            Outcome::Failed(why) => format!("failed: {why}"),
                            Outcome::LinkLost => "cut off: the keyboard disconnected; it starts again when it's back".to_owned(),
                        };
                        println!("Source {worker} {said}");
                        self.status.update(|status| status.last_ended = Some(Ended { worker, said }));
                    }
                }
            }
            self.show(&state);
        }
    }

    fn show(&self, state: &RunnerState) {
        let (state, playing, worker) = match state {
            RunnerState::Idle => (StreamState::Idle, None, None),
            RunnerState::Running { request, worker } => (StreamState::Running, Some(request.play.to_string()), Some(*worker)),
            RunnerState::WaitingForLink { request } => (StreamState::WaitingForKeyboard, Some(request.play.to_string()), None),
            RunnerState::PausedForUpload { request } => (StreamState::PausedForUpload, Some(request.play.to_string()), None),
        };
        self.status.update(|status| {
            if status.worker != worker {
                status.streaming = false;
            }
            (status.state, status.playing, status.worker) = (state, playing, worker);
        });
    }

    fn spawn(&mut self, id: u64, request: SourceRequest) -> Result<()> {
        let (process, listener) = WorkerProcess::spawn(format!("Source {id}"), &request, &self.token)?;
        self.workers.insert(id, Arc::clone(&process));
        let (uplink, status, events, token, feeds) =
            (Arc::clone(&self.uplink), Arc::clone(&self.status), self.events.clone(), self.token.clone(), Arc::clone(&self.feeds));
        thread::Builder::new().name(format!("source {id}")).spawn(move || {
            let outcome = forward(id, &process, listener, &token, &request, &uplink, &status, &feeds);
            let _ = events.send(RunnerEvent::WorkerEnded { worker: id, outcome });
        })?;
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
fn forward(
    id: u64,
    process: &Arc<WorkerProcess>,
    listener: TcpListener,
    token: &str,
    request: &SourceRequest,
    uplink: &Uplink,
    status: &StatusBoard,
    feeds: &Arc<Feeds>,
) -> Outcome {
    let socket = match process.accept(listener, token) {
        Ok(socket) => socket,
        Err(e) => {
            process.kill();
            return Outcome::Failed(format!("{e:#}"));
        }
    };
    feeds.keyboard_started(process, request);
    let link_lost = relay(id, socket, process, uplink, status, feeds);
    let exit = process.wait();
    match (link_lost, exit.success()) {
        (true, _) => Outcome::LinkLost,
        (false, true) => Outcome::Finished,
        (false, false) => Outcome::Failed(process.last_error().unwrap_or_else(|| format!("the worker exited with {exit}"))),
    }
}

/// Hands the worker's messages to the Link and its viewers until the worker ends. Returns whether the
/// Link dropped mid-stream; then the worker is asked to stop and the rest only goes to viewers.
fn relay(id: u64, mut socket: TcpStream, process: &Arc<WorkerProcess>, uplink: &Uplink, status: &StatusBoard, feeds: &Feeds) -> bool {
    let mut link_lost = false;
    loop {
        let message = match Message::read_from(&mut socket) {
            Ok(Some(message)) => message,
            Ok(None) => return link_lost,
            Err(e) => {
                eprintln!("Source {id}: reading from the worker: {e}");
                return link_lost;
            }
        };
        feeds.publish(process, &message);
        if link_lost {
            continue;
        }
        let starts = matches!(message, Message::StreamStart(_));
        if uplink.push_stream(message).is_err() {
            link_lost = true;
            process.ask_stop();
            continue;
        }
        if starts {
            status.update(|status| {
                if status.worker == Some(id) {
                    status.streaming = true;
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::RunnerEffect::{Ended, Spawn, StopWorker};
    use super::RunnerState::{Idle, PausedForUpload, Running, WaitingForLink};
    use super::*;
    use glassdeck_capture::sources::Play;

    fn request(name: &str) -> SourceRequest {
        SourceRequest { play: Play::File { path: name.into(), looping: false }, seconds: None }
    }

    #[test]
    fn walks_the_whole_transition_table() {
        let (a, b) = (request("a"), request("b"));
        let running = Running { request: a.clone(), worker: 1 };
        let waiting = WaitingForLink { request: a.clone() };
        let paused = PausedForUpload { request: a.clone() };
        let ended = |worker, outcome| RunnerEvent::WorkerEnded { worker, outcome };
        let table = vec![
            (Idle, RunnerEvent::Start(b.clone()), Running { request: b.clone(), worker: 9 }, vec![Spawn { worker: 9, request: b.clone() }]),
            (running.clone(), RunnerEvent::Start(b.clone()), Running { request: b.clone(), worker: 9 }, vec![StopWorker(1), Spawn { worker: 9, request: b.clone() }]),
            (waiting.clone(), RunnerEvent::Start(b.clone()), Running { request: b.clone(), worker: 9 }, vec![Spawn { worker: 9, request: b.clone() }]),
            (Idle, RunnerEvent::Stop, Idle, vec![]),
            (running.clone(), RunnerEvent::Stop, Idle, vec![StopWorker(1)]),
            (waiting.clone(), RunnerEvent::Stop, Idle, vec![]),
            (running.clone(), ended(1, Outcome::Finished), Idle, vec![Ended { worker: 1, outcome: Outcome::Finished }]),
            (running.clone(), ended(1, Outcome::Failed("x".into())), Idle, vec![Ended { worker: 1, outcome: Outcome::Failed("x".into()) }]),
            (running.clone(), ended(1, Outcome::LinkLost), waiting.clone(), vec![Ended { worker: 1, outcome: Outcome::LinkLost }]),
            (running.clone(), ended(2, Outcome::Finished), running.clone(), vec![]),
            (Idle, ended(1, Outcome::Finished), Idle, vec![]),
            (waiting.clone(), RunnerEvent::LinkUp, Running { request: a.clone(), worker: 9 }, vec![Spawn { worker: 9, request: a.clone() }]),
            (running.clone(), RunnerEvent::LinkUp, running.clone(), vec![]),
            (Idle, RunnerEvent::LinkUp, Idle, vec![]),
            (running.clone(), RunnerEvent::PauseForUpload, paused.clone(), vec![StopWorker(1)]),
            (Idle, RunnerEvent::PauseForUpload, Idle, vec![]),
            (waiting.clone(), RunnerEvent::PauseForUpload, waiting.clone(), vec![]),
            (paused.clone(), RunnerEvent::PauseForUpload, paused.clone(), vec![]),
            (paused.clone(), RunnerEvent::ResumeAfterUpload, Running { request: a.clone(), worker: 9 }, vec![Spawn { worker: 9, request: a.clone() }]),
            (Idle, RunnerEvent::ResumeAfterUpload, Idle, vec![]),
            (running.clone(), RunnerEvent::ResumeAfterUpload, running.clone(), vec![]),
            (waiting.clone(), RunnerEvent::ResumeAfterUpload, waiting.clone(), vec![]),
            (paused.clone(), RunnerEvent::Start(b.clone()), PausedForUpload { request: b.clone() }, vec![]),
            (paused.clone(), RunnerEvent::Stop, Idle, vec![]),
            (paused.clone(), ended(1, Outcome::Finished), paused.clone(), vec![]),
            (paused.clone(), RunnerEvent::LinkUp, paused.clone(), vec![]),
        ];
        for (from, event, to, effects) in table {
            assert_eq!(step(from.clone(), event.clone(), 9), (to, effects), "from {from:?} on {event:?}");
        }
    }
}
