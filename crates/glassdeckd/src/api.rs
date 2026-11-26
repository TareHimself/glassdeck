//! The local API: HTTP on 127.0.0.1 at a random port, with the port and token written to `api.json`.
//! The routes and their types are in glassdeck-api. Handlers run glassdeckd's blocking work on
//! tokio's blocking pool; `/events` streams what changes as it happens.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use axum::extract::{FromRequest, Request, State};
use axum::http::header::AUTHORIZATION;
use axum::middleware::{self, Next};
use axum::response::sse::{self, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, put};
use axum::{Json, Router};
use glassdeck_api::{
    API_FORMAT_VERSION, ApiFile, Deploy, Deployed, ErrorCode, Event, LinkStatus, ShowState, Status, StreamEntry, StreamMode, StreamState,
    Streams, Theme, ThemeApplied,
};
use glassdeck_capture::monitors;
use glassdeck_proto::{FileTarget, LARGE_FILE_BYTES};
use tokio::sync::broadcast;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;

use crate::error::{Failure, answer, conflict};
use crate::link::Uplink;
use crate::module_host::HostCommand;
use crate::runner::{Runner, RunnerEvent};
use crate::streams::{MonitorStream, StreamName, monitor_streams};
use crate::theme::{self, ActiveTheme};

/// How long starting a source may take before the client is told it's still starting (CEF is the slow one).
const START_WAIT: Duration = Duration::from_secs(30);
/// Longer than the runner gives a worker to stop before killing it.
const STOP_WAIT: Duration = Duration::from_secs(15);
/// Events a slow `/events` client may fall behind by before it's sent the whole status again.
pub const EVENT_BACKLOG: usize = 64;
const API_THREADS: usize = 2;
const MAX_BLOCKING_THREADS: usize = 16;

pub struct Api {
    pub token: String,
    pub runner: Runner,
    pub uplink: Arc<Uplink>,
    pub keyboard: SocketAddr,
    /// glassdeckd's own copy of every package it deployed; host sides run from here
    pub packages: PathBuf,
    pub staging: PathBuf,
    /// Large uploads running now; the source stays paused until the last one is answered.
    pub large_uploads: Mutex<usize>,
    /// Where the active theme is kept across restarts
    pub theme_file: PathBuf,
    /// The active theme, as pushed and as checked. Held while a theme is applied, so two never interleave.
    pub theme: Mutex<(Theme, ActiveTheme)>,
    pub module_host: Sender<HostCommand>,
    pub events: broadcast::Sender<Event>,
}

/// Held for the length of a large upload.
struct UploadPause<'a> {
    api: &'a Api,
}

impl Drop for UploadPause<'_> {
    fn drop(&mut self) {
        let mut running = self.api.large_uploads.lock().expect("upload count poisoned");
        *running -= 1;
        if *running == 0
            && let Err(e) = self.api.runner.send(RunnerEvent::ResumeAfterUpload)
        {
            eprintln!("error: resuming the source after an upload: {e:#}");
        }
    }
}

/// A JSON body, refused as a bad request when it doesn't parse.
#[derive(FromRequest)]
#[from_request(via(Json), rejection(Failure))]
struct Body<T>(T);

type Reply<T> = Result<Json<T>, Failure>;

impl Api {
    /// Serves forever on this thread.
    pub fn serve(self, api_file: &Path) -> Result<()> {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(API_THREADS)
            .max_blocking_threads(MAX_BLOCKING_THREADS)
            .thread_name("api")
            .enable_io()
            .enable_time()
            .build()
            .context("starting the API's runtime")?
            .block_on(self.serve_on(api_file))
    }

    async fn serve_on(self, api_file: &Path) -> Result<()> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.context("starting the local API")?;
        let port = listener.local_addr()?.port();
        if let Some(folder) = api_file.parent() {
            std::fs::create_dir_all(folder)?;
        }
        let file = ApiFile { format: API_FORMAT_VERSION, port, token: self.token.clone() };
        std::fs::write(api_file, serde_json::to_vec_pretty(&file)?).with_context(|| format!("writing {}", api_file.display()))?;
        println!("API: http://127.0.0.1:{port} (token in {})", api_file.display());
        axum::serve(listener, router(Arc::new(self))).await.context("serving the local API")
    }

    fn publish(&self, event: Event) {
        let _ = self.events.send(event);
    }

    fn status(&self) -> Status {
        Status {
            link: LinkStatus { keyboard: self.keyboard.to_string(), connected: self.uplink.is_connected() },
            stream: self.runner.status(),
        }
    }

    fn streams(&self) -> Result<Streams> {
        let shown = self.theme.lock().expect("theme poisoned").1.stream.clone().filter(|_| self.runner.status().worker.is_some());
        let streams = monitor_streams(&monitors::enumerate()?)
            .into_iter()
            .map(|s| StreamEntry {
                name: s.name.as_str().to_owned(),
                mode: StreamMode::Live,
                from: "built in".to_owned(),
                shown: shown.as_ref() == Some(&s.name),
            })
            .collect();
        Ok(Streams { streams })
    }

    fn active_theme(&self) -> Theme {
        self.theme.lock().expect("theme poisoned").0.clone()
    }

    /// What glassdeckd kept from last time, applied at start-up without waiting: a stream starts once
    /// the keyboard connects, and the background and widgets go on connect.
    pub fn restore(&self) -> Result<()> {
        let guard = self.theme.lock().expect("theme poisoned");
        let active = &guard.1;
        self.uplink.select_background(active.background.package.to_string());
        self.uplink.set_widgets(active.slots());
        if let Some(name) = &active.stream {
            match self.find_stream(name)? {
                Some(stream) => self.runner.send(RunnerEvent::Start(stream.request()))?,
                None => println!("Theme: {name} isn't there now"),
            }
        }
        println!("Theme: background {}, {} widgets", active.background.package, active.widgets.len());
        Ok(())
    }

    /// Makes `theme` the active one, acting only on what differs from the running theme.
    fn apply_theme(&self, theme: Theme) -> Result<ThemeApplied> {
        let installed = theme::installed(&self.packages)?;
        let active = ActiveTheme::check(&theme, &installed)?;
        let mut guard = self.theme.lock().expect("theme poisoned");
        let before = guard.1.clone();
        if active.background.package != before.background.package {
            self.uplink.select_background(active.background.package.to_string());
        }
        if active.slots() != before.slots() {
            self.uplink.set_widgets(active.slots());
        }
        let stream = match &active.stream {
            Some(name) if before.stream.as_ref() != Some(name) || self.runner.status().worker.is_none() => Some(self.show(name)?),
            Some(_) => Some(ShowState::Shown),
            None if before.stream.is_some() => {
                self.stop_stream()?;
                None
            }
            None => None,
        };
        theme::save(&self.theme_file, &theme)?;
        if self.module_host.send(HostCommand::Theme(active.clone())).is_err() {
            eprintln!("error: the module host has stopped");
        }
        *guard = (theme.clone(), active);
        drop(guard);
        self.publish(Event::Theme { theme });
        Ok(ThemeApplied { stream })
    }

    /// Installs a package on the keyboard, as its keyboard copy, then keeps glassdeckd's own copy.
    /// Answers once the keyboard has it in place.
    fn deploy(&self, path: &Path) -> Result<Deployed> {
        let package = theme::deployable(path)?;
        let copy = theme::keyboard_copy(&package, &self.staging)?;
        let removing = RemoveOnDrop(&copy);
        let target = FileTarget::Package(package.id().file_name());
        let size = std::fs::metadata(&copy).with_context(|| format!("reading {}", copy.display()))?.len();
        let _pause = (size >= LARGE_FILE_BYTES).then(|| self.pause_for_upload()).transpose()?;
        let answered = self.uplink.put_file(target.clone(), &copy)?;
        answered.recv().context("the Link stopped before the keyboard answered")??;
        drop(removing);
        keep_locally(path, &self.packages.join(package.id().file_name()))?;
        println!("Deployed {target}");
        let deployed = target.to_string();
        self.publish(Event::Deployed { deployed: deployed.clone() });
        Ok(Deployed { deployed })
    }

    /// The keyboard falls back to its own background, and the transfer gets the whole Link.
    fn pause_for_upload(&self) -> Result<UploadPause<'_>> {
        let mut running = self.large_uploads.lock().expect("upload count poisoned");
        if *running == 0 {
            self.runner.send(RunnerEvent::PauseForUpload)?;
        }
        *running += 1;
        Ok(UploadPause { api: self })
    }

    /// `None` when no monitor has that stream's name now.
    fn find_stream(&self, name: &StreamName) -> Result<Option<MonitorStream>> {
        Ok(monitor_streams(&monitors::enumerate()?).into_iter().find(|s| s.name == *name))
    }

    fn stop_stream(&self) -> Result<()> {
        self.runner.send(RunnerEvent::Stop)?;
        let deadline = Instant::now() + STOP_WAIT;
        match self.runner.status.wait_for(deadline, |now| (now.state == StreamState::Idle).then_some(())) {
            Some(()) => Ok(()),
            None => Err(conflict("the stream is still stopping")),
        }
    }

    /// Starts the stream and waits until it shows, fails, or is still starting after `START_WAIT`.
    fn show(&self, name: &StreamName) -> Result<ShowState> {
        let Some(stream) = self.find_stream(name)? else {
            println!("Theme: {name} isn't there now");
            return Ok(ShowState::Unavailable);
        };
        let before = self.runner.status();
        self.runner.send(RunnerEvent::Start(stream.request()))?;
        let reached = self.runner.status.wait_for(Instant::now() + START_WAIT, |now| {
            if *now == before {
                return None;
            }
            let new_worker = now.worker.is_some() && now.worker != before.worker;
            if new_worker && now.streaming {
                return Some(Ok(ShowState::Shown));
            }
            if let Some(ended) = &now.last_ended
                && now.last_ended != before.last_ended
                && Some(ended.worker) != before.worker
            {
                return Some(Err(ended.said.clone()));
            }
            match now.state {
                StreamState::WaitingForKeyboard => Some(Ok(ShowState::WaitingForKeyboard)),
                StreamState::PausedForUpload => Some(Ok(ShowState::AfterUpload)),
                StreamState::Idle | StreamState::Running => None,
            }
        });
        match reached {
            Some(Ok(state)) => Ok(state),
            Some(Err(said)) => bail!("stream:{name} {said}"),
            None => Ok(ShowState::Starting),
        }
    }
}

/// The keyboard copy made for an upload, removed once it's sent or the upload fails.
struct RemoveOnDrop<'a>(&'a Path);

impl Drop for RemoveOnDrop<'_> {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_file(self.0) {
            eprintln!("error: removing {}: {e}", self.0.display());
        }
    }
}

/// glassdeckd keeps every deployed package, unless it's deploying the copy it already keeps.
fn keep_locally(from: &Path, place: &Path) -> Result<()> {
    let same = match (std::fs::canonicalize(from), std::fs::canonicalize(place)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    };
    if same {
        return Ok(());
    }
    if let Some(folder) = place.parent() {
        std::fs::create_dir_all(folder).with_context(|| format!("creating {}", folder.display()))?;
    }
    let staging = place.with_extension("deck.tmp");
    std::fs::copy(from, &staging).with_context(|| format!("copying {} to {}", from.display(), staging.display()))?;
    std::fs::rename(&staging, place).with_context(|| format!("moving {} into place", place.display()))
}

fn router(api: Arc<Api>) -> Router {
    Router::new()
        .route("/status", get(status))
        .route("/streams", get(list_streams))
        .route("/theme", get(get_theme).put(put_theme))
        .route("/deploy", put(deploy_file))
        .route("/events", get(events))
        .fallback(|| async { answer(ErrorCode::NotFound, "no such route".to_owned()) })
        .method_not_allowed_fallback(|| async { answer(ErrorCode::MethodNotAllowed, "that route doesn't take this method".to_owned()) })
        .layer(middleware::from_fn_with_state(Arc::clone(&api), authorize))
        .with_state(api)
}

async fn authorize(State(api): State<Arc<Api>>, request: Request, next: Next) -> Response {
    let expected = format!("Bearer {}", api.token);
    match request.headers().get(AUTHORIZATION) {
        Some(given) if given.as_bytes() == expected.as_bytes() => next.run(request).await,
        _ => answer(ErrorCode::Unauthorized, "missing or wrong token; it's in api.json".to_owned()),
    }
}

/// Runs glassdeckd's blocking work for one request off the async threads.
async fn blocking<T: Send + 'static>(api: Arc<Api>, work: impl FnOnce(&Api) -> Result<T> + Send + 'static) -> Reply<T> {
    match tokio::task::spawn_blocking(move || work(&api)).await {
        Ok(result) => Ok(Json(result?)),
        Err(e) => Err(anyhow!("the request's handler panicked: {e}").into()),
    }
}

async fn status(State(api): State<Arc<Api>>) -> Reply<Status> {
    blocking(api, |api| Ok(api.status())).await
}

async fn list_streams(State(api): State<Arc<Api>>) -> Reply<Streams> {
    blocking(api, Api::streams).await
}

async fn get_theme(State(api): State<Arc<Api>>) -> Reply<Theme> {
    blocking(api, |api| Ok(api.active_theme())).await
}

async fn put_theme(State(api): State<Arc<Api>>, Body(theme): Body<Theme>) -> Reply<ThemeApplied> {
    blocking(api, move |api| api.apply_theme(theme)).await
}

async fn deploy_file(State(api): State<Arc<Api>>, Body(Deploy { path }): Body<Deploy>) -> Reply<Deployed> {
    blocking(api, move |api| api.deploy(Path::new(&path))).await
}

/// The whole status first, then each change. A client that falls behind gets the whole status again.
async fn events(State(api): State<Arc<Api>>) -> impl IntoResponse {
    let changes = BroadcastStream::new(api.events.subscribe());
    let first = Event::Status { status: api.status() };
    let resync = Arc::clone(&api);
    let events = tokio_stream::once(first)
        .chain(changes.map(move |change| change.unwrap_or_else(|_lagged| Event::Status { status: resync.status() })))
        .map(|event| sse::Event::default().json_data(event).map_err(|e| anyhow!("encoding an event: {e}")));
    Sse::new(events).keep_alive(KeepAlive::default())
}
