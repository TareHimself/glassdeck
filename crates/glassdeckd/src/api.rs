//! The local API: HTTP on 127.0.0.1 at a random port, with the port and token written to `api.json`.
//! The routes and their types are in glassdeck-api. Handlers run glassdeckd's blocking work on
//! tokio's blocking pool; `/events` streams what changes as it happens.

use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use axum::extract::{FromRequest, FromRequestParts, Request, State};
use axum::http::header::AUTHORIZATION;
use axum::middleware::{self, Next};
use axum::response::sse::{self, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, put};
use axum::{Json, Router};
use glassdeck_api::{
    API_FORMAT_VERSION, ApiFile, BgChosen, BgView, Deploy, Deployed, ErrorCode, Event, LinkStatus, OptionSet, OptionsOwner,
    OptionsView, RunsOn, SetBg, SetOption, ShowState, Status, StreamEntry, StreamMode, StreamState, Streams,
};
use glassdeck_capture::monitors;
use glassdeck_luau::options::{OptionValue, OptionsScript, Panel, Values};
use glassdeck_package::{Background, CanvasFiles};
use glassdeck_proto::LARGE_FILE_BYTES;
use serde_json::json;
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;

use crate::deploy;
use crate::error::{Failure, answer, bad_request, conflict, not_found};
use crate::link::Uplink;
use crate::module_host::{CanvasSelection, HostCommand};
use crate::options::{OptionsStore, panel_view};
use crate::runner::{Runner, RunnerEvent};
use crate::streams::{self, Choice, Effect, MonitorStream, StreamName, effects, monitor_streams};

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
    pub modules: PathBuf,
    pub profile: PathBuf,
    pub staging: PathBuf,
    /// Large uploads running now; the source stays paused until the last one is answered.
    pub large_uploads: Mutex<usize>,
    /// Where the `bg` choice is remembered across restarts
    pub bg_file: PathBuf,
    pub bg: Mutex<Option<Choice>>,
    /// Held while a background choice is carried out, so two choices never interleave their effects
    pub choosing: Mutex<()>,
    /// glassdeckd's own copies of canvas backgrounds that have options or a host half
    pub canvases: PathBuf,
    pub options: Mutex<OptionsStore>,
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

/// A path parameter, refused as a bad request when it doesn't parse.
#[derive(FromRequestParts)]
#[from_request(via(axum::extract::Path), rejection(Failure))]
struct Param<T>(T);

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
            bg: self.bg_view(),
            stream: self.runner.status(),
        }
    }

    fn streams(&self) -> Result<Streams> {
        let shown = match &*self.bg.lock().expect("bg poisoned") {
            Some(Choice::Stream(name)) if self.runner.status().worker.is_some() => Some(name.clone()),
            _ => None,
        };
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

    /// Installs a background, module or profile on the keyboard, then keeps glassdeckd's own copy.
    /// Answers once the keyboard has the file in place.
    fn deploy(&self, path: &Path) -> Result<Deployed> {
        let target = deploy::target_for(path, &self.modules).map_err(|e| bad_request(format!("{e:#}")))?;
        let sending = deploy::keyboard_file(&target, path, &self.staging)?;
        let size = std::fs::metadata(sending.path()).with_context(|| format!("reading {}", sending.path().display()))?.len();
        let _pause = (size >= LARGE_FILE_BYTES).then(|| self.pause_for_upload()).transpose()?;
        let answered = self.uplink.put_file(target.clone(), sending.path())?;
        answered.recv().context("the Link stopped before the keyboard answered")??;
        deploy::keep_locally(&target, path, &self.modules, &self.profile, &self.canvases)?;
        println!("Deployed {target}");
        let _choosing = self.choosing.lock().expect("choosing poisoned");
        if let glassdeck_proto::FileTarget::Background(name) = &target
            && matches!(&*self.bg.lock().expect("bg poisoned"), Some(Choice::Background(chosen)) if chosen == name)
        {
            self.tell_module_host(HostCommand::Canvas(self.canvas_selection(&Choice::Background(name.clone()))));
        }
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

    fn bg_view(&self) -> BgView {
        match &*self.bg.lock().expect("bg poisoned") {
            Some(choice) => BgView { bg: Some(choice.to_string()), runs_on: choice.runs_on() },
            None => BgView { bg: None, runs_on: RunsOn::Keyboard },
        }
    }

    /// What glassdeckd remembered, applied at start-up without waiting: a stream starts once the
    /// keyboard connects, and a keyboard choice is sent on connect.
    pub fn restore(&self) -> Result<()> {
        let _choosing = self.choosing.lock().expect("choosing poisoned");
        let Some(choice) = streams::load(&self.bg_file)? else { return Ok(()) };
        for effect in effects(&choice) {
            match effect {
                Effect::StopStream => {}
                Effect::SelectOnKeyboard(name) => self.uplink.select_background(name),
                Effect::ShowStream(name) => self.runner.send(RunnerEvent::Start(self.find_stream(&name)?.request()))?,
            }
        }
        let on = match choice.runs_on() {
            RunsOn::Keyboard => "keyboard",
            RunsOn::Host => "host",
        };
        println!("Background: {choice} (on the {on})");
        self.tell_module_host(HostCommand::Canvas(self.canvas_selection(&choice)));
        *self.bg.lock().expect("bg poisoned") = Some(choice);
        Ok(())
    }

    /// Shows `choice` and remembers it. A stream is looked up first, so an unknown name changes nothing.
    fn choose(&self, choice: Choice) -> Result<BgChosen> {
        let _choosing = self.choosing.lock().expect("choosing poisoned");
        let stream = match &choice {
            Choice::Stream(name) => Some(self.find_stream(name)?),
            Choice::BuiltIn | Choice::Background(_) => None,
        };
        let mut state = ShowState::Shown;
        for effect in effects(&choice) {
            match effect {
                Effect::StopStream => self.stop_stream()?,
                Effect::SelectOnKeyboard(name) => self.uplink.select_background(name),
                Effect::ShowStream(_) => state = self.show(stream.as_ref().expect("looked up above"))?,
            }
        }
        streams::save(&self.bg_file, &choice)?;
        self.tell_module_host(HostCommand::Canvas(self.canvas_selection(&choice)));
        *self.bg.lock().expect("bg poisoned") = Some(choice.clone());
        self.publish(Event::Bg { bg: self.bg_view() });
        Ok(BgChosen { bg: choice.to_string(), runs_on: choice.runs_on(), state })
    }

    /// The selected canvas for the module host, when glassdeckd has a copy (canvases with options or a host half).
    fn canvas_selection(&self, choice: &Choice) -> Option<CanvasSelection> {
        let Choice::Background(name) = choice else { return None };
        let path = self.canvases.join(name);
        path.is_file().then(|| CanvasSelection { name: name.clone(), path })
    }

    fn tell_module_host(&self, command: HostCommand) {
        if self.module_host.send(command).is_err() {
            eprintln!("error: the module host has stopped");
        }
    }

    /// The selected background's options: its name, its options.luau and the first pass.
    fn selected_options(&self) -> Result<(String, OptionsScript, Panel)> {
        let choice = self.bg.lock().expect("bg poisoned").clone();
        let Some(Choice::Background(name)) = choice else { return Err(not_found("the background isn't a canvas, so it has no options")) };
        let path = self.canvases.join(&name);
        if !path.is_file() {
            return Err(not_found(format!("{name} has no options")));
        }
        let background = Background::open(&path)?;
        let files = CanvasFiles::read(&background)?;
        let entry = files.plan.options.as_deref().ok_or_else(|| not_found(format!("{name} has no options")))?;
        let entry = background.entry(entry).with_context(|| format!("{name} has no {entry}"))?;
        let script = OptionsScript::new(&String::from_utf8(background.read_entry(entry)?).context("options.luau isn't UTF-8")?);
        let first = script.first_pass()?;
        Ok((name, script, first))
    }

    /// The panel as the user would see it now: rerun with the stored values, so visibility is current.
    fn bg_options(&self) -> Result<OptionsView> {
        let (name, script, first) = self.selected_options()?;
        let resolved = first.resolve(&self.options.lock().expect("options poisoned").chosen(&name, &first)?);
        let panel = script.run(&first, &resolved.values)?;
        Ok(OptionsView { owner: OptionsOwner::Background { name }, items: panel_view(&panel, &resolved.values), dropped: resolved.dropped })
    }

    fn set_bg_option(&self, set: &SetOption) -> Result<OptionSet> {
        let (name, script, first) = self.selected_options()?;
        let owner = OptionsOwner::Background { name: name.clone() };
        let (value, shown) = checked_value(&owner, &first, set)?;
        let store = self.options.lock().expect("options poisoned");
        let resolved = first.resolve(&with_choice(store.chosen(&name, &first)?, &set.path, value.clone()));
        script.run(&first, &resolved.values)?;
        store.set(&name, &first, &set.path, value)?;
        drop(store);
        self.tell_module_host(HostCommand::CanvasOptionsChanged(BTreeSet::from([set.path.clone()])));
        self.publish(Event::Options { owner: owner.clone() });
        Ok(OptionSet { owner, path: set.path.clone(), value: shown })
    }

    /// A placed widget's options: whose they are, its options.luau, first pass, and the values in its profile entry.
    fn widget_options_of(&self, index: u32) -> Result<(OptionsOwner, OptionsScript, Panel, Values)> {
        let packages = crate::module_host::installed(&self.modules)?;
        let profile = glassdeck_luau::profile::Profile::read(&self.profile, glassdeck_luau::profile::WidgetArea::of_panel().size, |id| {
            packages.get(id).map(|p| p.manifest().clone())
        })?;
        let widget = profile
            .widgets
            .get(index as usize)
            .ok_or_else(|| not_found(format!("the profile has {} widgets, so there's no widget {index}", profile.widgets.len())))?;
        let owner = OptionsOwner::Widget { index, module: widget.module.to_string() };
        let source = packages[&widget.module].options_script()?.ok_or_else(|| not_found(format!("{owner} has no options")))?;
        let script = OptionsScript::new(&source);
        let first = script.first_pass()?;
        let chosen = crate::options::read_values(&owner.to_string(), &first, widget.options.iter());
        Ok((owner, script, first, chosen))
    }

    fn widget_options(&self, index: u32) -> Result<OptionsView> {
        let (owner, script, first, chosen) = self.widget_options_of(index)?;
        let resolved = first.resolve(&chosen);
        let panel = script.run(&first, &resolved.values)?;
        Ok(OptionsView { owner, items: panel_view(&panel, &resolved.values), dropped: resolved.dropped })
    }

    /// Writes the value into glassdeckd's profile; the module host sees the change and tells that widget's host side.
    fn set_widget_option(&self, index: u32, set: &SetOption) -> Result<OptionSet> {
        let (owner, script, first, chosen) = self.widget_options_of(index)?;
        let (value, shown) = checked_value(&owner, &first, set)?;
        script.run(&first, &first.resolve(&with_choice(chosen, &set.path, value.clone())).values)?;
        let text = std::fs::read_to_string(&self.profile).with_context(|| format!("reading {}", self.profile.display()))?;
        let mut profile: serde_json::Value = serde_json::from_str(&text).with_context(|| format!("parsing {}", self.profile.display()))?;
        let entry = profile["widgets"].get_mut(index as usize).with_context(|| format!("the profile has no widget {index}"))?;
        let options = entry.as_object_mut().context("a widget entry is an object")?.entry("options").or_insert_with(|| json!({}));
        let options = options.as_object_mut().context("a widget's options are an object")?;
        match &value {
            Some(value) => options.insert(set.path.clone(), value.to_json()),
            None => options.remove(&set.path),
        };
        let staging = self.profile.with_extension("json.tmp");
        std::fs::write(&staging, serde_json::to_vec_pretty(&profile)?).with_context(|| format!("writing {}", staging.display()))?;
        std::fs::rename(&staging, &self.profile).with_context(|| format!("moving {} into place", self.profile.display()))?;
        self.publish(Event::Options { owner: owner.clone() });
        Ok(OptionSet { owner, path: set.path.clone(), value: shown })
    }

    fn find_stream(&self, name: &StreamName) -> Result<MonitorStream> {
        let streams = monitor_streams(&monitors::enumerate()?);
        let listed = streams.iter().map(|s| s.name.as_str()).collect::<Vec<_>>().join(", ");
        streams.into_iter().find(|s| s.name == *name).ok_or_else(|| not_found(format!("no stream named {name}; there are: {listed}")))
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
    fn show(&self, stream: &MonitorStream) -> Result<ShowState> {
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
            Some(Err(said)) => bail!("stream:{} {said}", stream.name),
            None => Ok(ShowState::Starting),
        }
    }
}

fn router(api: Arc<Api>) -> Router {
    Router::new()
        .route("/status", get(status))
        .route("/streams", get(list_streams))
        .route("/bg", get(bg).put(set_bg))
        .route("/bg/options", get(bg_options).put(set_bg_option))
        .route("/widgets/{index}/options", get(widget_options).put(set_widget_option))
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

async fn bg(State(api): State<Arc<Api>>) -> Reply<BgView> {
    blocking(api, |api| Ok(api.bg_view())).await
}

async fn set_bg(State(api): State<Arc<Api>>, Body(SetBg { bg }): Body<SetBg>) -> Reply<BgChosen> {
    blocking(api, move |api| api.choose(Choice::parse(&bg).map_err(|e| bad_request(format!("{e:#}")))?)).await
}

async fn bg_options(State(api): State<Arc<Api>>) -> Reply<OptionsView> {
    blocking(api, Api::bg_options).await
}

async fn set_bg_option(State(api): State<Arc<Api>>, Body(set): Body<SetOption>) -> Reply<OptionSet> {
    blocking(api, move |api| api.set_bg_option(&set)).await
}

async fn widget_options(State(api): State<Arc<Api>>, Param(index): Param<u32>) -> Reply<OptionsView> {
    blocking(api, move |api| api.widget_options(index)).await
}

async fn set_widget_option(State(api): State<Arc<Api>>, Param(index): Param<u32>, Body(set): Body<SetOption>) -> Reply<OptionSet> {
    blocking(api, move |api| api.set_widget_option(index, &set)).await
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

/// The value `set` asks for, checked against the option's kind, and the value that will show.
fn checked_value(owner: &OptionsOwner, first: &Panel, set: &SetOption) -> Result<(Option<OptionValue>, serde_json::Value)> {
    let path = &set.path;
    let option = first.options().into_iter().find(|o| o.path == *path).ok_or_else(|| not_found(format!("{owner} has no option {path}")))?;
    let value = match &set.value {
        Some(json) => Some(OptionValue::from_json(&option.kind, json).map_err(|e| bad_request(format!("{path}: {e:#}")))?),
        None => None,
    };
    if let Some(value) = &value
        && let Some(why) = option.kind.refuses(value)
    {
        return Err(bad_request(format!("{path}: {why}")));
    }
    let shown = value.clone().unwrap_or_else(|| option.kind.default_value()).to_json();
    Ok((value, shown))
}

/// `chosen` with `path` set to `value`, or back to its default when `value` is `None`.
fn with_choice(mut chosen: Values, path: &str, value: Option<OptionValue>) -> Values {
    match value {
        Some(value) => chosen.insert(path.to_owned(), value),
        None => chosen.remove(path),
    };
    chosen
}
