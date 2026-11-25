//! Talks to glassdeckd's local API, using the port and token it writes to `api.json`.

use std::path::PathBuf;

use anyhow::{Context, Result, anyhow, bail, ensure};
use serde::Deserialize;
use serde_json::Value;

const API_FORMAT_VERSION: u32 = 1;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApiFile {
    format: u32,
    port: u16,
    token: String,
}

fn api_file() -> Result<PathBuf> {
    let local = std::env::var_os("LOCALAPPDATA").context("LOCALAPPDATA isn't set")?;
    Ok(PathBuf::from(local).join("Glassdeck").join("api.json"))
}

pub fn call(method: &str, path: &str, body: Option<Value>) -> Result<Value> {
    let file = api_file()?;
    let not_running = || anyhow!("glassdeckd isn't running; start it with `glassdeckd run`");
    let text = std::fs::read_to_string(&file).map_err(|_| not_running())?;
    let api: ApiFile = serde_json::from_str(&text).with_context(|| format!("parsing {}", file.display()))?;
    ensure!(api.format == API_FORMAT_VERSION, "api.json format {} is not the {API_FORMAT_VERSION} this build reads", api.format);

    let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
    let url = format!("http://127.0.0.1:{}{path}", api.port);
    let auth = format!("Bearer {}", api.token);
    let sent = match (method, body) {
        ("GET", None) => agent.get(&url).header("Authorization", &auth).call(),
        ("DELETE", None) => agent.delete(&url).header("Authorization", &auth).call(),
        ("PUT", Some(body)) => agent.put(&url).header("Authorization", &auth).send_json(&body),
        (method, body) => bail!("no client call {method} {path} with body {body:?}"),
    };
    let mut response = match sent {
        Ok(response) => response,
        Err(ureq::Error::Io(_) | ureq::Error::ConnectionFailed) => return Err(not_running()),
        Err(e) => return Err(e).context("calling glassdeckd"),
    };
    let status = response.status();
    let answer: Value = response.body_mut().read_json().context("reading glassdeckd's answer")?;
    if !status.is_success() {
        bail!("{}", answer["error"].as_str().unwrap_or("glassdeckd refused the request"));
    }
    Ok(answer)
}
