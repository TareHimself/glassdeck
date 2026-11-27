use std::time::Duration;

use futures_util::StreamExt;
use glassdeck_api::{ApiError, ApiFile, Event, Status, Streams, Theme, ThemeApplied, ThemePush};
use serde::Serialize;
use specta::Type;
use tauri::AppHandle;
use tauri_specta::Event as _;

const RECONNECT_AFTER: Duration = Duration::from_secs(2);

/// Why a call to glassdeckd didn't get an answer.
#[derive(Clone, Debug, Serialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GlassdeckdError {
    /// No readable `api.json`, so glassdeckd isn't running (or never started)
    NotRunning { reason: String },
    /// `api.json` is there but nothing answered at its port
    Unreachable { reason: String },
    /// glassdeckd answered with a refusal
    Refused { error: ApiError },
    /// glassdeckd answered with something that isn't what the API promises
    BadAnswer { reason: String },
}

/// What the frontend hears about the connection to glassdeckd and what it sends.
#[derive(Clone, Debug, Serialize, Type, tauri_specta::Event)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum GlassdeckdUpdate {
    Connected,
    Message { event: Event },
    Lost { error: GlassdeckdError },
}

struct Endpoint {
    base: String,
    token: String,
}

fn endpoint() -> Result<Endpoint, GlassdeckdError> {
    let not_running = |reason: String| GlassdeckdError::NotRunning { reason };
    let path = glassdeck_api::api_file().map_err(|e| not_running(format!("{e:#}")))?;
    let text = std::fs::read_to_string(&path).map_err(|e| not_running(format!("reading {}: {e}", path.display())))?;
    let file: ApiFile = serde_json::from_str(&text).map_err(|e| not_running(format!("parsing {}: {e}", path.display())))?;
    Ok(Endpoint { base: format!("http://127.0.0.1:{}", file.port), token: file.token })
}

/// A WHEP session watching a stream: the SDP answer for the page, and what ends it.
#[derive(Clone, Debug, Serialize, Type)]
pub struct Watching {
    pub stream: String,
    pub session: u32,
    pub answer: String,
}

pub struct Glassdeckd {
    http: reqwest::Client,
}

impl Glassdeckd {
    pub fn new() -> Self {
        Self { http: reqwest::Client::new() }
    }

    fn get(&self, route: &str) -> Result<reqwest::RequestBuilder, GlassdeckdError> {
        let at = endpoint()?;
        Ok(self.http.get(format!("{}{route}", at.base)).bearer_auth(at.token))
    }

    async fn fetch<T: serde::de::DeserializeOwned>(&self, route: &str) -> Result<T, GlassdeckdError> {
        let reply = self.get(route)?.send().await.map_err(unreachable)?;
        if !reply.status().is_success() {
            return Err(refusal(reply).await);
        }
        reply.json().await.map_err(|e| GlassdeckdError::BadAnswer { reason: e.to_string() })
    }

    pub async fn status(&self) -> Result<Status, GlassdeckdError> {
        self.fetch("/status").await
    }

    pub async fn theme(&self) -> Result<Theme, GlassdeckdError> {
        self.fetch("/theme").await
    }

    pub async fn streams(&self) -> Result<Streams, GlassdeckdError> {
        self.fetch("/streams").await
    }

    /// Hands glassdeckd a WebRTC offer to watch `name`'s stream (WHEP); the answer goes back to the page.
    pub async fn watch_stream(&self, name: &str, offer: String) -> Result<Watching, GlassdeckdError> {
        let at = endpoint()?;
        let reply = self
            .http
            .post(format!("{}/streams/{name}/whep", at.base))
            .bearer_auth(at.token)
            .header(reqwest::header::CONTENT_TYPE, "application/sdp")
            .body(offer)
            .send()
            .await
            .map_err(unreachable)?;
        if !reply.status().is_success() {
            return Err(refusal(reply).await);
        }
        let bad = |reason: String| GlassdeckdError::BadAnswer { reason };
        let location = reply.headers().get(reqwest::header::LOCATION).and_then(|l| l.to_str().ok()).ok_or_else(|| bad("a WHEP answer without a Location".into()))?;
        let session = location
            .rsplit_once("/whep/")
            .and_then(|(_, id)| id.parse().ok())
            .ok_or_else(|| bad(format!("a WHEP Location that isn't a session: {location}")))?;
        let answer = reply.text().await.map_err(|e| bad(e.to_string()))?;
        Ok(Watching { stream: name.to_owned(), session, answer })
    }

    pub async fn stop_watching(&self, stream: &str, session: u32) -> Result<(), GlassdeckdError> {
        let at = endpoint()?;
        let reply = self.http.delete(format!("{}/streams/{stream}/whep/{session}", at.base)).bearer_auth(at.token).send().await.map_err(unreachable)?;
        if !reply.status().is_success() {
            return Err(refusal(reply).await);
        }
        Ok(())
    }

    /// What the stream's monitor shows now, as JPEG bytes.
    pub async fn stream_snapshot(&self, name: &str) -> Result<Vec<u8>, GlassdeckdError> {
        let reply = self.get(&format!("/streams/{name}/snapshot"))?.send().await.map_err(unreachable)?;
        if !reply.status().is_success() {
            return Err(refusal(reply).await);
        }
        Ok(reply.bytes().await.map_err(unreachable)?.to_vec())
    }

    /// Makes `push` the active theme: glassdeckd copies the packages it doesn't hold and sends them on.
    pub async fn put_theme(&self, push: &ThemePush) -> Result<ThemeApplied, GlassdeckdError> {
        let at = endpoint()?;
        let reply = self.http.put(format!("{}/theme", at.base)).bearer_auth(at.token).json(push).send().await.map_err(unreachable)?;
        if !reply.status().is_success() {
            return Err(refusal(reply).await);
        }
        reply.json().await.map_err(|e| GlassdeckdError::BadAnswer { reason: e.to_string() })
    }

    /// Relays `/events` to the frontend for as long as the app runs, reconnecting after each loss.
    pub async fn relay_events(&self, app: AppHandle) {
        loop {
            let ended = self.follow_events(&app).await;
            emit(&app, GlassdeckdUpdate::Lost { error: ended });
            tokio::time::sleep(RECONNECT_AFTER).await;
        }
    }

    async fn follow_events(&self, app: &AppHandle) -> GlassdeckdError {
        let reply = match self.get("/events") {
            Ok(request) => match request.send().await {
                Ok(reply) => reply,
                Err(e) => return unreachable(e),
            },
            Err(e) => return e,
        };
        if !reply.status().is_success() {
            return refusal(reply).await;
        }
        emit(app, GlassdeckdUpdate::Connected);
        let mut body = reply.bytes_stream();
        let mut pending = String::new();
        while let Some(chunk) = body.next().await {
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(e) => return unreachable(e),
            };
            pending.push_str(&String::from_utf8_lossy(&chunk));
            let (frames, rest) = split_frames(&pending);
            pending = rest;
            for data in frames {
                match serde_json::from_str::<Event>(&data) {
                    Ok(event) => emit(app, GlassdeckdUpdate::Message { event }),
                    Err(e) => return GlassdeckdError::BadAnswer { reason: format!("an event that doesn't parse: {e}") },
                }
            }
        }
        GlassdeckdError::Unreachable { reason: "glassdeckd closed the event stream".into() }
    }
}

fn emit(app: &AppHandle, update: GlassdeckdUpdate) {
    if let Err(e) = update.emit(app) {
        eprintln!("emitting a glassdeckd update: {e}");
    }
}

fn unreachable(e: reqwest::Error) -> GlassdeckdError {
    GlassdeckdError::Unreachable { reason: e.to_string() }
}

async fn refusal(reply: reqwest::Response) -> GlassdeckdError {
    let status = reply.status();
    match reply.json::<ApiError>().await {
        Ok(error) => GlassdeckdError::Refused { error },
        Err(e) => GlassdeckdError::BadAnswer { reason: format!("HTTP {status} without an ApiError body: {e}") },
    }
}

/// Splits server-sent events text into each complete frame's data and the unfinished rest.
fn split_frames(text: &str) -> (Vec<String>, String) {
    let text = text.replace("\r\n", "\n");
    let Some(end) = text.rfind("\n\n") else {
        return (Vec::new(), text);
    };
    let frames = text[..end]
        .split("\n\n")
        .filter_map(|frame| {
            let data: Vec<&str> = frame
                .lines()
                .filter_map(|line| line.strip_prefix("data:"))
                .map(|value| value.strip_prefix(' ').unwrap_or(value))
                .collect();
            (!data.is_empty()).then(|| data.join("\n"))
        })
        .collect();
    (frames, text[end + 2..].to_owned())
}

#[cfg(test)]
mod tests {
    use super::split_frames;

    #[test]
    fn complete_frames_come_out_and_the_rest_waits() {
        let (frames, rest) = split_frames("data: {\"a\":1}\n\n:\n\ndata: {\"b\"");
        assert_eq!(frames, vec!["{\"a\":1}".to_owned()]);
        assert_eq!(rest, "data: {\"b\"");
    }

    #[test]
    fn keep_alive_comments_and_crlf_are_ignored() {
        let (frames, rest) = split_frames(":\r\n\r\ndata:{\"c\":2}\r\n\r\n");
        assert_eq!(frames, vec!["{\"c\":2}".to_owned()]);
        assert_eq!(rest, "");
    }

    #[test]
    fn multi_line_data_is_joined() {
        let (frames, _) = split_frames("data: one\ndata: two\n\n");
        assert_eq!(frames, vec!["one\ntwo".to_owned()]);
    }
}
