//! At most one source at a time, each in its own worker process. Starting another switches to it; a
//! source cut off by the Link going down starts again when the keyboard is back.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::windows::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use glassdeck_capture::sources::SourceRequest;
use glassdeck_proto::Message;
use serde::Serialize;

use crate::link::Uplink;
use crate::worker::handshake;

/// How long a worker gets to end its stream cleanly before it's killed.
const STOP_GRACE: Duration = Duration::from_secs(10);
/// How long a new worker gets to connect back (CEF takes a few seconds to start).
const CONNECT_GRACE: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(20);
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
pub const PAUSED_FOR_UPLOAD: &str = "paused for an upload";

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

/// What `glassdeck status` shows about sources.
#[derive(Clone, Debug, Default, Serialize)]
pub struct SourceStatus {
    pub state: &'static str,
    pub playing: Option<String>,
    pub worker: Option<u64>,
    /// The current worker has sent its StreamStart.
    pub streaming: bool,
    pub last_ended: Option<(u64, String)>,
}

#[derive(Clone)]
pub struct Runner {
    events: Sender<RunnerEvent>,
    pub status: Arc<Mutex<SourceStatus>>,
}

impl Runner {
    /// `events` and `inbox` are one channel, made by the caller so the Link can report LinkUp into it.
    pub fn start(uplink: Arc<Uplink>, token: String, events: Sender<RunnerEvent>, inbox: Receiver<RunnerEvent>) -> Result<Self> {
        let status = Arc::new(Mutex::new(SourceStatus { state: "idle", ..SourceStatus::default() }));
        let runner = Self { events: events.clone(), status: Arc::clone(&status) };
        let looped = events.clone();
        thread::Builder::new()
            .name("sources".into())
            .spawn(move || Loop { uplink, token, status, events: looped, workers: HashMap::new() }.run(inbox))?;
        Ok(runner)
    }

    pub fn send(&self, event: RunnerEvent) -> Result<()> {
        self.events.send(event).context("the source runner has stopped")
    }

    pub fn status(&self) -> SourceStatus {
        self.status.lock().expect("status poisoned").clone()
    }
}

struct Loop {
    uplink: Arc<Uplink>,
    token: String,
    status: Arc<Mutex<SourceStatus>>,
    events: Sender<RunnerEvent>,
    workers: HashMap<u64, Arc<Worker>>,
}

impl Loop {
    fn run(mut self, inbox: Receiver<RunnerEvent>) {
        let mut state = RunnerState::Idle;
        let mut next_worker = 1;
        for event in inbox {
            if let RunnerEvent::WorkerEnded { worker, .. } = event {
                self.workers.remove(&worker);
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
                    RunnerEffect::StopWorker(worker) => self.stop(worker),
                    RunnerEffect::Ended { worker, outcome } => {
                        let said = match outcome {
                            Outcome::Finished => "finished".to_owned(),
                            Outcome::Failed(why) => format!("failed: {why}"),
                            Outcome::LinkLost => "cut off: the keyboard disconnected; it starts again when it's back".to_owned(),
                        };
                        println!("Source {worker} {said}");
                        self.status.lock().expect("status poisoned").last_ended = Some((worker, said));
                    }
                }
            }
            self.show(&state);
        }
    }

    fn show(&self, state: &RunnerState) {
        let mut status = self.status.lock().expect("status poisoned");
        let (name, playing, worker) = match state {
            RunnerState::Idle => ("idle", None, None),
            RunnerState::Running { request, worker } => ("running", Some(request.play.to_string()), Some(*worker)),
            RunnerState::WaitingForLink { request } => ("waiting for the keyboard", Some(request.play.to_string()), None),
            RunnerState::PausedForUpload { request } => (PAUSED_FOR_UPLOAD, Some(request.play.to_string()), None),
        };
        if status.worker != worker {
            status.streaming = false;
        }
        (status.state, status.playing, status.worker) = (name, playing, worker);
    }

    fn spawn(&mut self, id: u64, request: SourceRequest) -> Result<()> {
        let listener = TcpListener::bind("127.0.0.1:0").context("listening for the source worker")?;
        let address = listener.local_addr()?;
        let mut child = Command::new(std::env::current_exe()?)
            .args(["source", "--uplink", &address.to_string(), "--token", &self.token])
            .stdin(Stdio::piped())
            .stderr(Stdio::piped())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .context("starting a source worker")?;
        let mut stdin = child.stdin.take().expect("stdin is piped");
        stdin.write_all(&serde_json::to_vec(&request)?)?;
        drop(stdin);
        let last_error = forward_stderr(id, child.stderr.take().expect("stderr is piped"));
        println!("Source {id}: {}", request.play);
        let worker = Arc::new(Worker { child: Mutex::new(child), socket: Mutex::new(None) });
        self.workers.insert(id, Arc::clone(&worker));
        let (uplink, status, events, token) = (Arc::clone(&self.uplink), Arc::clone(&self.status), self.events.clone(), self.token.clone());
        thread::Builder::new().name(format!("source {id}")).spawn(move || {
            let outcome = worker.forward(id, listener, &token, &uplink, &status, &last_error);
            let _ = events.send(RunnerEvent::WorkerEnded { worker: id, outcome });
        })?;
        Ok(())
    }

    /// Asks the worker to end its stream, and kills it if it doesn't within the grace time.
    fn stop(&mut self, id: u64) {
        let Some(worker) = self.workers.get(&id) else { return };
        match worker.socket.lock().expect("worker poisoned").as_mut() {
            Some(socket) => {
                let _ = socket.write_all(&[0]);
            }
            None => {
                let _ = worker.child.lock().expect("worker poisoned").kill();
                println!("Source {id} stopped before it began streaming");
                return;
            }
        }
        let deadline = Instant::now() + STOP_GRACE;
        while Instant::now() < deadline {
            if !matches!(worker.child.lock().expect("worker poisoned").try_wait(), Ok(None)) {
                println!("Source {id} stopped");
                return;
            }
            thread::sleep(POLL);
        }
        eprintln!("Source {id} didn't stop within {} s; killing it", STOP_GRACE.as_secs());
        let _ = worker.child.lock().expect("worker poisoned").kill();
    }
}

struct Worker {
    child: Mutex<Child>,
    /// Set once the worker connects back; writing to it asks the worker to stop.
    socket: Mutex<Option<TcpStream>>,
}

impl Worker {
    fn forward(&self, id: u64, listener: TcpListener, token: &str, uplink: &Uplink, status: &Mutex<SourceStatus>, last_error: &Mutex<Option<String>>) -> Outcome {
        let result = self.accept(listener, token).and_then(|socket| {
            *self.socket.lock().expect("worker poisoned") = Some(socket.try_clone()?);
            Ok(relay(id, socket, uplink, status))
        });
        let link_lost = match result {
            Ok(link_lost) => link_lost,
            Err(e) => {
                let _ = self.child.lock().expect("worker poisoned").kill();
                return Outcome::Failed(format!("{e:#}"));
            }
        };
        let exit = loop {
            if let Ok(Some(exit)) = self.child.lock().expect("worker poisoned").try_wait() {
                break exit;
            }
            thread::sleep(POLL);
        };
        match (link_lost, exit.success()) {
            (true, _) => Outcome::LinkLost,
            (false, true) => Outcome::Finished,
            (false, false) => {
                let why = last_error.lock().expect("worker poisoned").clone();
                Outcome::Failed(why.unwrap_or_else(|| format!("the worker exited with {exit}")))
            }
        }
    }

    fn accept(&self, listener: TcpListener, token: &str) -> Result<TcpStream> {
        listener.set_nonblocking(true)?;
        let deadline = Instant::now() + CONNECT_GRACE;
        loop {
            match listener.accept() {
                Ok((mut socket, _)) => {
                    socket.set_nonblocking(false)?;
                    let expected = handshake(token);
                    let mut got = vec![0u8; expected.len()];
                    socket.read_exact(&mut got).context("reading the worker's handshake")?;
                    if got != expected.as_bytes() {
                        bail!("something other than our worker connected");
                    }
                    return Ok(socket);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(e).context("accepting the source worker"),
            }
            if !matches!(self.child.lock().expect("worker poisoned").try_wait(), Ok(None)) {
                bail!("the worker exited before streaming");
            }
            if Instant::now() > deadline {
                bail!("the worker didn't connect within {} s", CONNECT_GRACE.as_secs());
            }
            thread::sleep(POLL);
        }
    }
}

/// Hands the worker's messages to the Link until the worker ends. Returns whether the Link dropped
/// mid-stream; then the rest is read and discarded while the worker winds down.
fn relay(id: u64, mut socket: TcpStream, uplink: &Uplink, status: &Mutex<SourceStatus>) -> bool {
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
        if link_lost {
            continue;
        }
        let starts = matches!(message, Message::StreamStart(_));
        if uplink.push_stream(message).is_err() {
            link_lost = true;
            let _ = socket.write_all(&[0]);
            continue;
        }
        if starts {
            let mut status = status.lock().expect("status poisoned");
            if status.worker == Some(id) {
                status.streaming = true;
            }
        }
    }
}

/// The worker's output goes to glassdeckd's log, and its last error line explains a failure.
fn forward_stderr(id: u64, stderr: impl Read + Send + 'static) -> Arc<Mutex<Option<String>>> {
    let last_error = Arc::new(Mutex::new(None));
    let kept = Arc::clone(&last_error);
    thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            eprintln!("[source {id}] {line}");
            if let Some(error) = line.strip_prefix("error: ") {
                *kept.lock().expect("worker poisoned") = Some(error.to_owned());
            }
        }
    });
    last_error
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
