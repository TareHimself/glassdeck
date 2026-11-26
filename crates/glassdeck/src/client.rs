//! Talks to glassdeckd's local API, using the port and token it writes to `api.json`.

use anyhow::{Context, Result, anyhow, bail, ensure};
use glassdeck_api::{API_FORMAT_VERSION, ApiError, ApiFile};
use serde::Serialize;
use serde::de::DeserializeOwned;

pub fn get<T: DeserializeOwned>(path: &str) -> Result<T> {
    call(path, None::<&()>)
}

pub fn put<T: DeserializeOwned>(path: &str, body: &impl Serialize) -> Result<T> {
    call(path, Some(body))
}

fn call<T: DeserializeOwned>(path: &str, body: Option<&impl Serialize>) -> Result<T> {
    let file = glassdeck_api::api_file()?;
    let not_running = || anyhow!("glassdeckd isn't running; start it with `glassdeckd run`");
    let text = std::fs::read_to_string(&file).map_err(|_| not_running())?;
    let api: ApiFile = serde_json::from_str(&text).with_context(|| format!("parsing {}", file.display()))?;
    ensure!(api.format == API_FORMAT_VERSION, "api.json format {} is not the {API_FORMAT_VERSION} this build reads", api.format);

    let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
    let url = format!("http://127.0.0.1:{}{path}", api.port);
    let auth = format!("Bearer {}", api.token);
    let sent = match body {
        None => agent.get(&url).header("Authorization", &auth).call(),
        Some(body) => agent.put(&url).header("Authorization", &auth).send_json(body),
    };
    let mut response = match sent {
        Ok(response) => response,
        Err(ureq::Error::Io(_) | ureq::Error::ConnectionFailed) => return Err(not_running()),
        Err(e) => return Err(e).context("calling glassdeckd"),
    };
    if !response.status().is_success() {
        let refused: ApiError = response.body_mut().read_json().context("reading glassdeckd's refusal")?;
        bail!("{}", refused.error);
    }
    response.body_mut().read_json().context("reading glassdeckd's answer")
}
