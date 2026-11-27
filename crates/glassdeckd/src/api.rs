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
use axum::routing::get;
use axum::{Json, Router};
use glassdeck_api::{
    API_FORMAT_VERSION, ApiFile, ErrorCode, Event, LinkStatus, ShowState, Status, StreamEntry, StreamMode, StreamState, Streams, Theme,
    ThemeApplied, ThemePush,
};
use glassdeck_capture::monitors;
use glassdeck_proto::{FileTarget, LARGE_FILE_BYTES};
use tokio::sync::broadcast;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;

use crate::active::ActivePackages;
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
/// A stream snapshot's width: the app's keyboard preview shows it at about this size
const SNAPSHOT_WIDTH: u32 = 960;
const SNAPSHOT_QUALITY: u8 = 80;
/// Events a slow `/events` client may fall behind by before it's sent the whole status again.
pub const EVENT_BACKLOG: usize = 64;
const API_THREADS: usize = 2;
const MAX_BLOCKING_THREADS: usize = 16;

pub struct Api {
    pub token: String,
    pub runner: Runner,
    pub uplink: Arc<Uplink>,
    pub keyboard: SocketAddr,
    /// Copies of the active theme's packages, which host sides run from. Locked after `theme`.
    pub active: Mutex<ActivePackages>,
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
    pub fn serve(self: Arc<Self>, api_file: &Path) -> Result<()> {
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

    async fn serve_on(self: Arc<Self>, api_file: &Path) -> Result<()> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.context("starting the local API")?;
        let port = listener.local_addr()?.port();
        if let Some(folder) = api_file.parent() {
            std::fs::create_dir_all(folder)?;
        }
        let file = ApiFile { format: API_FORMAT_VERSION, port, token: self.token.clone() };
        std::fs::write(api_file, serde_json::to_vec_pretty(&file)?).with_context(|| format!("writing {}", api_file.display()))?;
        println!("API: http://127.0.0.1:{port} (token in {})", api_file.display());
        axum::serve(listener, router(self)).await.context("serving the local API")
    }

    fn publish(&self, event: Event) {
        let _ = self.events.send(event);
    }

    fn status(&self) -> Status {
        Status {
            link: LinkStatus { keyboard: self.keyboard.to_string(), connection: self.uplink.link() },
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
    /// the keyboard connects, and the keyboard is brought up to date when it says what it holds.
    pub fn restore(&self) -> Result<()> {
        let guard = self.theme.lock().expect("theme poisoned");
        let active = &guard.1;
        if let Some(name) = &active.stream {
            match self.find_stream(name)? {
                Some(stream) => self.runner.send(RunnerEvent::Start(stream.request()))?,
                None => println!("Theme: {name} isn't there now"),
            }
        }
        println!("Theme: background {}, {} widgets", active.background.package, active.widgets.len());
        Ok(())
    }

    /// Makes `push.theme` the active one: takes in the packages it brings, brings the keyboard up to
    /// date, and drops what the theme no longer uses, here and on the keyboard.
    fn apply_theme(&self, push: ThemePush) -> Result<ThemeApplied> {
        let ThemePush { theme, packages } = push;
        let mut guard = self.theme.lock().expect("theme poisoned");
        let mut held = self.active.lock().expect("active packages poisoned");
        for (id, path) in &packages {
            held.take_in(id, Path::new(path))?;
        }
        let active = ActiveTheme::check(&theme, &theme::installed(held.folder())?)?;
        let before = guard.1.clone();
        self.sync_keyboard(&active, &held)?;
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
        held.keep_only(&active.package_ids())?;
        *guard = (theme.clone(), active);
        drop((held, guard));
        self.publish(Event::Theme { theme });
        Ok(ThemeApplied { stream })
    }

    /// Run when the keyboard says what it holds: brings it up to the active theme.
    pub fn keyboard_reported(&self) {
        let guard = self.theme.lock().expect("theme poisoned");
        let held = self.active.lock().expect("active packages poisoned");
        if let Err(e) = self.sync_keyboard(&guard.1, &held) {
            eprintln!("error: bringing the keyboard up to date: {e:#}");
        }
    }

    /// Uploads what the keyboard lacks for `active`, switches it over, then removes what it no longer
    /// needs. Without a connection, the choice waits for the next connect, which runs this again.
    fn sync_keyboard(&self, active: &ActiveTheme, held: &ActivePackages) -> Result<()> {
        let needed: Vec<String> = active.package_ids().into_iter().collect();
        if let Some(inventory) = self.uplink.inventory() {
            for id in &needed {
                let id = glassdeck_package::QualifiedId::parse(id)?;
                let (copy, digest) = held.keyboard_copy(&id).with_context(|| format!("glassdeckd holds no copy of {id}"))?;
                let name = id.file_name();
                if inventory.get(&name) != Some(&digest) {
                    self.upload(&name, &copy)?;
                    self.uplink.record_installed(name, digest);
                }
            }
        }
        self.uplink.select_background(active.background.package.to_string());
        self.uplink.set_widgets(active.slots());
        if let Some(inventory) = self.uplink.inventory() {
            let wanted: std::collections::BTreeSet<String> =
                needed.iter().map(|id| format!("{id}.{}", glassdeck_package::EXTENSION)).collect();
            let unused: Vec<String> = inventory.into_keys().filter(|name| !wanted.contains(name)).collect();
            if !unused.is_empty() {
                println!("Keyboard: removing {}", unused.join(", "));
                self.uplink.remove_packages(unused);
            }
        }
        Ok(())
    }

    /// Sends one keyboard copy, and waits for the keyboard to have it in place.
    fn upload(&self, name: &str, copy: &Path) -> Result<()> {
        let target = FileTarget::Package(name.to_owned());
        let size = std::fs::metadata(copy).with_context(|| format!("reading {}", copy.display()))?.len();
        let _pause = (size >= LARGE_FILE_BYTES).then(|| self.pause_for_upload()).transpose()?;
        let answered = self.uplink.put_file(target.clone(), copy)?;
        answered.recv().context("the Link stopped before the keyboard answered")??;
        println!("Keyboard: installed {target}");
        Ok(())
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
    /// What the stream's monitor shows now, as a JPEG at most `SNAPSHOT_WIDTH` wide: the app's preview of a
    /// stream before it's applied, when nothing is encoding it yet.
    fn stream_snapshot(&self, name: &str) -> Result<Vec<u8>> {
        let name = StreamName::parse(name).map_err(|e| crate::error::bad_request(format!("{e:#}")))?;
        let stream = self.find_stream(&name)?.ok_or_else(|| crate::error::not_found(format!("no stream {name} is attached")))?;
        let found = monitors::find(&monitors::MonitorQuery::Index(stream.monitor.index))?;
        let (bgra, (width, height)) = glassdeck_capture::capture_still(found.monitor)?;
        let rgb: Vec<u8> = bgra.chunks_exact(4).flat_map(|p| [p[2], p[1], p[0]]).collect();
        let mut image = image::RgbImage::from_raw(width, height, rgb).context("the frame isn't its size")?;
        if width > SNAPSHOT_WIDTH {
            image = image::imageops::thumbnail(&image, SNAPSHOT_WIDTH, height * SNAPSHOT_WIDTH / width);
        }
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, SNAPSHOT_QUALITY).encode_image(&image).context("encoding the snapshot")?;
        Ok(jpeg)
    }

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

fn router(api: Arc<Api>) -> Router {
    Router::new()
        .route("/status", get(status))
        .route("/streams", get(list_streams))
        .route("/streams/{name}/snapshot", get(stream_snapshot))
        .route("/theme", get(get_theme).put(put_theme))
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

async fn stream_snapshot(State(api): State<Arc<Api>>, axum::extract::Path(name): axum::extract::Path<String>) -> Result<Response, Failure> {
    let jpeg = match tokio::task::spawn_blocking(move || api.stream_snapshot(&name)).await {
        Ok(result) => result?,
        Err(e) => return Err(anyhow!("the request's handler panicked: {e}").into()),
    };
    Ok(([(axum::http::header::CONTENT_TYPE, "image/jpeg")], jpeg).into_response())
}

async fn get_theme(State(api): State<Arc<Api>>) -> Reply<Theme> {
    blocking(api, |api| Ok(api.active_theme())).await
}

async fn put_theme(State(api): State<Arc<Api>>, Body(push): Body<ThemePush>) -> Reply<ThemeApplied> {
    blocking(api, move |api| api.apply_theme(push)).await
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
