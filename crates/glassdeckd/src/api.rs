//! The local API: HTTP on 127.0.0.1, a random port and token written to `api.json`. Routes exist for
//! what the CLI does; more come with the clients that need them.

use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use glassdeck_capture::monitors;
use glassdeck_luau::options::{OptionsScript, Panel};
use glassdeck_package::{Background, CanvasFiles};
use glassdeck_proto::LARGE_FILE_BYTES;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tiny_http::{Header, Method, Request, Response, Server};

use crate::deploy;
use crate::link::Uplink;
use crate::module_host::{HostCommand, CanvasSelection};
use crate::options::{OptionsStore, panel_json, parse_typed};
use crate::runner::{PAUSED_FOR_UPLOAD, Runner, RunnerEvent};
use crate::streams::{self, Choice, Effect, MonitorStream, StreamName, effects, monitor_streams};

pub const API_FORMAT_VERSION: u32 = 1;
/// How long starting a source may take before the CLI is told it's still starting (CEF is the slow one).
const START_WAIT: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(100);
/// Longer than the runner gives a worker to stop before killing it.
const STOP_WAIT: Duration = Duration::from_secs(15);

#[derive(Serialize)]
struct ApiFile<'a> {
    format: u32,
    port: u16,
    token: &'a str,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetBg {
    bg: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Deploy {
    path: PathBuf,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetOption {
    /// A widget's place in the profile; without it, the selected background
    #[serde(default)]
    widget: Option<usize>,
    path: String,
    /// As typed; `null` goes back to the default
    value: Option<String>,
}

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
    /// glassdeckd's own copies of canvas backgrounds that have options or a host half
    pub canvases: PathBuf,
    pub options: Mutex<OptionsStore>,
    pub module_host: Mutex<Sender<HostCommand>>,
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

impl Api {
    /// Serves forever on this thread.
    pub fn serve(self, api_file: &Path) -> Result<()> {
        let server = Server::http("127.0.0.1:0").map_err(|e| anyhow!("starting the local API: {e}"))?;
        let port = server.server_addr().to_ip().context("the API listens on an IP address")?.port();
        if let Some(folder) = api_file.parent() {
            std::fs::create_dir_all(folder)?;
        }
        let file = ApiFile { format: API_FORMAT_VERSION, port, token: &self.token };
        std::fs::write(api_file, serde_json::to_vec_pretty(&file)?).with_context(|| format!("writing {}", api_file.display()))?;
        println!("API: http://127.0.0.1:{port} (token in {})", api_file.display());
        let api = Arc::new(self);
        for request in server.incoming_requests() {
            let api = Arc::clone(&api);
            thread::spawn(move || api.handle(request));
        }
        Ok(())
    }

    fn handle(&self, mut request: Request) {
        let expected = format!("Bearer {}", self.token);
        let authorized = request.headers().iter().any(|h| h.field.equiv("Authorization") && h.value.as_str() == expected);
        let (status, body) = if authorized {
            match self.route(&mut request) {
                Ok(body) => (200, body),
                Err(e) => (400, json!({ "error": format!("{e:#}") })),
            }
        } else {
            (401, json!({ "error": "missing or wrong token; it's in api.json" }))
        };
        let header = Header::from_bytes("Content-Type", "application/json").expect("a valid header");
        let _ = request.respond(Response::from_string(body.to_string()).with_status_code(status).with_header(header));
    }

    fn route(&self, request: &mut Request) -> Result<serde_json::Value> {
        match (request.method(), request.url()) {
            (Method::Get, "/status") => Ok(json!({
                "link": { "keyboard": self.keyboard.to_string(), "connected": self.uplink.is_connected() },
                "bg": self.bg_json(),
                "stream": self.runner.status(),
            })),
            (Method::Get, "/streams") => {
                let shown = match &*self.bg.lock().expect("bg poisoned") {
                    Some(Choice::Stream(name)) if self.runner.status().worker.is_some() => Some(name.clone()),
                    _ => None,
                };
                let streams: Vec<serde_json::Value> = monitor_streams(&monitors::enumerate()?)
                    .into_iter()
                    .map(|s| json!({ "name": s.name.as_str(), "mode": "live", "from": "built in", "shown": shown.as_ref() == Some(&s.name) }))
                    .collect();
                Ok(json!({ "streams": streams }))
            }
            (Method::Get, "/bg") => Ok(self.bg_json()),
            (Method::Put, "/bg") => {
                let SetBg { bg } = serde_json::from_slice(&body(request)?).context("reading the bg request")?;
                self.choose(Choice::parse(&bg)?)
            }
            (Method::Get, "/options") => self.options_view(),
            (Method::Get, url) if url.starts_with("/options?widget=") => {
                let index = url.trim_start_matches("/options?widget=").parse().context("a widget's place in the profile is a number")?;
                self.widget_options_view(index)
            }
            (Method::Put, "/options") => {
                let SetOption { widget, path, value } = serde_json::from_slice(&body(request)?).context("reading the options request")?;
                match widget {
                    Some(index) => self.set_widget_option(index, &path, value.as_deref()),
                    None => self.set_option(&path, value.as_deref()),
                }
            }
            (Method::Put, "/deploy") => {
                let Deploy { path } = serde_json::from_slice(&body(request)?).context("reading the deploy request")?;
                self.deploy(&path)
            }
            (method, url) => bail!("no route {method} {url}"),
        }
    }

    /// Installs a background, module or profile on the keyboard, then keeps glassdeckd's own copy.
    /// Answers once the keyboard has the file in place.
    fn deploy(&self, path: &Path) -> Result<serde_json::Value> {
        let target = deploy::target_for(path, &self.modules)?;
        let sending = deploy::keyboard_file(&target, path, &self.staging)?;
        let size = std::fs::metadata(sending.path()).with_context(|| format!("reading {}", sending.path().display()))?.len();
        let _pause = (size >= LARGE_FILE_BYTES).then(|| self.pause_for_upload()).transpose()?;
        let answered = self.uplink.put_file(target.clone(), sending.path())?;
        answered.recv().context("the Link stopped before the keyboard answered")??;
        deploy::keep_locally(&target, path, &self.modules, &self.profile, &self.canvases)?;
        println!("Deployed {target}");
        if let glassdeck_proto::FileTarget::Background(name) = &target
            && matches!(&*self.bg.lock().expect("bg poisoned"), Some(Choice::Background(chosen)) if chosen == name)
        {
            self.tell_module_host(HostCommand::Canvas(self.canvas_selection(&Choice::Background(name.clone()))));
        }
        Ok(json!({ "deployed": target.to_string() }))
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

    fn bg_json(&self) -> serde_json::Value {
        match &*self.bg.lock().expect("bg poisoned") {
            Some(choice) => json!({ "bg": choice.to_string(), "runs_on": choice.runs_on() }),
            None => json!({ "bg": null, "runs_on": "keyboard" }),
        }
    }

    /// What glassdeckd remembered, applied at start-up without waiting: a stream starts once the
    /// keyboard connects, and a keyboard choice is sent on connect.
    pub fn restore(&self) -> Result<()> {
        let Some(choice) = streams::load(&self.bg_file)? else { return Ok(()) };
        for effect in effects(&choice) {
            match effect {
                Effect::StopStream => {}
                Effect::SelectOnKeyboard(name) => self.uplink.select_background(name),
                Effect::ShowStream(name) => self.runner.send(RunnerEvent::Start(self.find_stream(&name)?.request()))?,
            }
        }
        println!("Background: {choice} (on the {})", choice.runs_on());
        self.tell_module_host(HostCommand::Canvas(self.canvas_selection(&choice)));
        *self.bg.lock().expect("bg poisoned") = Some(choice);
        Ok(())
    }

    /// Shows `choice` and remembers it. A stream is looked up first, so an unknown name changes nothing.
    fn choose(&self, choice: Choice) -> Result<serde_json::Value> {
        let stream = match &choice {
            Choice::Stream(name) => Some(self.find_stream(name)?),
            Choice::BuiltIn | Choice::Background(_) => None,
        };
        let mut state = "shown";
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
        Ok(json!({ "bg": choice.to_string(), "runs_on": choice.runs_on(), "state": state }))
    }

    /// The selected canvas for the module host, when glassdeckd has a copy (canvases with options or a host half).
    fn canvas_selection(&self, choice: &Choice) -> Option<CanvasSelection> {
        let Choice::Background(name) = choice else { return None };
        let path = self.canvases.join(name);
        path.is_file().then(|| CanvasSelection { name: name.clone(), path })
    }

    fn tell_module_host(&self, command: HostCommand) {
        if self.module_host.lock().expect("module host sender poisoned").send(command).is_err() {
            eprintln!("error: the module host has stopped");
        }
    }

    /// The selected background's options: its name, its options.luau and the first pass.
    fn selected_options(&self) -> Result<(String, OptionsScript, Panel)> {
        let choice = self.bg.lock().expect("bg poisoned").clone();
        let Some(Choice::Background(name)) = choice else { bail!("the background isn't a canvas, so it has no options") };
        let path = self.canvases.join(&name);
        if !path.is_file() {
            bail!("{name} has no options");
        }
        let background = Background::open(&path)?;
        let files = CanvasFiles::read(&background)?;
        let entry = files.plan.options.as_deref().with_context(|| format!("{name} has no options"))?;
        let entry = background.entry(entry).with_context(|| format!("{name} has no {entry}"))?;
        let script = OptionsScript::new(&String::from_utf8(background.read_entry(entry)?).context("options.luau isn't UTF-8")?);
        let first = script.first_pass()?;
        Ok((name, script, first))
    }

    /// The panel as the user would see it now: rerun with the stored values, so visibility is current.
    fn options_view(&self) -> Result<serde_json::Value> {
        let (name, script, first) = self.selected_options()?;
        let resolved = first.resolve(&self.options.lock().expect("options poisoned").chosen(&name, &first)?);
        let panel = script.run(&first, &resolved.values)?;
        let mut view = panel_json(&panel, &resolved.values);
        view["background"] = json!(name);
        view["dropped"] = json!(resolved.dropped);
        Ok(view)
    }

    fn set_option(&self, path: &str, typed: Option<&str>) -> Result<serde_json::Value> {
        let (name, script, first) = self.selected_options()?;
        let option = first.options().into_iter().find(|o| o.path == path).with_context(|| format!("{name} has no option {path}"))?;
        let value = typed.map(|text| parse_typed(&option.kind, text)).transpose().with_context(|| path.to_owned())?;
        let store = self.options.lock().expect("options poisoned");
        store.set(&name, &first, path, value)?;
        let resolved = first.resolve(&store.chosen(&name, &first)?);
        drop(store);
        script.run(&first, &resolved.values)?;
        self.tell_module_host(HostCommand::CanvasOptionsChanged(BTreeSet::from([path.to_owned()])));
        Ok(json!({ "background": name, "option": path, "value": resolved.values.get(path).map(|v| v.to_json()) }))
    }

    /// A placed widget's options: its label, options.luau, first pass, and the values in its profile entry.
    fn widget_options(&self, index: usize) -> Result<(String, OptionsScript, Panel, glassdeck_luau::options::Values)> {
        let packages = crate::module_host::installed(&self.modules)?;
        let profile = glassdeck_luau::profile::Profile::read(&self.profile, glassdeck_luau::profile::WidgetArea::of_panel().size, |id| {
            packages.get(id).map(|p| p.manifest().clone())
        })?;
        let widget = profile
            .widgets
            .get(index)
            .with_context(|| format!("the profile has {} widgets, so there's no widget {index}", profile.widgets.len()))?;
        let label = format!("widget {index} ({})", widget.module);
        let source = packages[&widget.module].options_script()?.with_context(|| format!("{label} has no options"))?;
        let script = OptionsScript::new(&source);
        let first = script.first_pass()?;
        let chosen = crate::options::read_values(&label, &first, widget.options.iter());
        Ok((label, script, first, chosen))
    }

    fn widget_options_view(&self, index: usize) -> Result<serde_json::Value> {
        let (label, script, first, chosen) = self.widget_options(index)?;
        let resolved = first.resolve(&chosen);
        let panel = script.run(&first, &resolved.values)?;
        let mut view = panel_json(&panel, &resolved.values);
        view["background"] = json!(label);
        view["dropped"] = json!(resolved.dropped);
        Ok(view)
    }

    /// Writes the value into glassdeckd's profile; the module host sees the change and restarts that widget's host side.
    fn set_widget_option(&self, index: usize, path: &str, typed: Option<&str>) -> Result<serde_json::Value> {
        let (label, script, first, _) = self.widget_options(index)?;
        let option = first.options().into_iter().find(|o| o.path == path).with_context(|| format!("{label} has no option {path}"))?;
        let value = typed.map(|text| parse_typed(&option.kind, text)).transpose().with_context(|| path.to_owned())?;
        if let Some(value) = &value
            && let Some(why) = option.kind.refuses(value)
        {
            bail!("{path}: {why}");
        }
        let text = std::fs::read_to_string(&self.profile).with_context(|| format!("reading {}", self.profile.display()))?;
        let mut profile: serde_json::Value = serde_json::from_str(&text).with_context(|| format!("parsing {}", self.profile.display()))?;
        let entry = profile["widgets"].get_mut(index).with_context(|| format!("the profile has no widget {index}"))?;
        let options = entry.as_object_mut().context("a widget entry is an object")?.entry("options").or_insert_with(|| json!({}));
        let options = options.as_object_mut().context("a widget's options are an object")?;
        match &value {
            Some(value) => options.insert(path.to_owned(), value.to_json()),
            None => options.remove(path),
        };
        let staging = self.profile.with_extension("json.tmp");
        std::fs::write(&staging, serde_json::to_vec_pretty(&profile)?).with_context(|| format!("writing {}", staging.display()))?;
        std::fs::rename(&staging, &self.profile).with_context(|| format!("moving {} into place", self.profile.display()))?;
        let (_, _, _, chosen) = self.widget_options(index)?;
        script.run(&first, &first.resolve(&chosen).values)?;
        let shown = value.map_or_else(|| option.kind.default_value().to_json(), |v| v.to_json());
        Ok(json!({ "background": label, "option": path, "value": shown }))
    }

    fn find_stream(&self, name: &StreamName) -> Result<MonitorStream> {
        let streams = monitor_streams(&monitors::enumerate()?);
        let listed = streams.iter().map(|s| s.name.as_str()).collect::<Vec<_>>().join(", ");
        streams.into_iter().find(|s| s.name == *name).ok_or_else(|| anyhow!("no stream named {name}; there are: {listed}"))
    }

    fn stop_stream(&self) -> Result<()> {
        self.runner.send(RunnerEvent::Stop)?;
        let deadline = Instant::now() + STOP_WAIT;
        while self.runner.status().state != "idle" {
            if Instant::now() > deadline {
                bail!("the stream is still stopping");
            }
            thread::sleep(POLL);
        }
        Ok(())
    }

    /// Starts the stream and waits until it shows, fails, or is still starting after `START_WAIT`.
    fn show(&self, stream: &MonitorStream) -> Result<&'static str> {
        let before = self.runner.status();
        self.runner.send(RunnerEvent::Start(stream.request()))?;
        let deadline = Instant::now() + START_WAIT;
        while Instant::now() < deadline {
            thread::sleep(POLL);
            let now = self.runner.status();
            let new_worker = now.worker.is_some() && now.worker != before.worker;
            if new_worker && now.streaming {
                return Ok("shown");
            }
            if let Some((worker, said)) = &now.last_ended
                && now.last_ended != before.last_ended
                && Some(*worker) != before.worker
            {
                bail!("stream:{} {said}", stream.name);
            }
            if now.state == "waiting for the keyboard" {
                return Ok("waiting for the keyboard");
            }
            if now.state == PAUSED_FOR_UPLOAD {
                return Ok("after the upload");
            }
        }
        Ok("starting")
    }
}

fn body(request: &mut Request) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    request.as_reader().read_to_end(&mut bytes).context("reading the request body")?;
    Ok(bytes)
}
