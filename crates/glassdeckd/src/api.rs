//! The local API: HTTP on 127.0.0.1, a random port and token written to `api.json`. Routes exist for
//! what the CLI does; more come with the clients that need them.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use glassdeck_capture::monitors;
use glassdeck_proto::LARGE_FILE_BYTES;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tiny_http::{Header, Method, Request, Response, Server};

use crate::deploy;
use crate::link::Uplink;
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
        deploy::keep_locally(&target, path, &self.modules, &self.profile)?;
        println!("Deployed {target}");
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
        *self.bg.lock().expect("bg poisoned") = Some(choice.clone());
        Ok(json!({ "bg": choice.to_string(), "runs_on": choice.runs_on(), "state": state }))
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
