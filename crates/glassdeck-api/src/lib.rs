//! What glassdeckd's local API sends and takes, shared by glassdeckd, the CLI and the desktop app.
//! `cargo test -p glassdeck-api` writes the TypeScript twins of these types to `bindings/`.
//!
//! | Route | Takes | Answers |
//! |---|---|---|
//! | `GET /status` | | [`Status`] |
//! | `GET /streams` | | [`Streams`] |
//! | `GET /bg` | | [`BgView`] |
//! | `PUT /bg` | [`SetBg`] | [`BgChosen`] |
//! | `GET /bg/options` | | [`OptionsView`] |
//! | `PUT /bg/options` | [`SetOption`] | [`OptionSet`] |
//! | `GET /widgets/{index}/options` | | [`OptionsView`] |
//! | `PUT /widgets/{index}/options` | [`SetOption`] | [`OptionSet`] |
//! | `PUT /deploy` | [`Deploy`] | [`Deployed`] |
//! | `GET /events` | | server-sent [`Event`]s, starting with [`Event::Status`] |
//!
//! Every request carries `Authorization: Bearer <token>`, with the token from [`ApiFile`]. A
//! refusal answers with an [`ApiError`] and the status code its [`ErrorCode`] names.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use ts_rs::TS;

pub const API_FORMAT_VERSION: u32 = 2;

/// Where glassdeckd writes [`ApiFile`] when it starts.
pub fn api_file() -> Result<PathBuf> {
    let local = std::env::var_os("LOCALAPPDATA").context("LOCALAPPDATA isn't set")?;
    Ok(PathBuf::from(local).join("Glassdeck").join("api.json"))
}

/// How to reach the running glassdeckd: `http://127.0.0.1:<port>` with the bearer token.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct ApiFile {
    pub format: u32,
    pub port: u16,
    pub token: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ErrorCode {
    /// 401: no token, or the wrong one
    Unauthorized,
    /// 400: the request itself is wrong, such as a value an option can't hold
    BadRequest,
    /// 404: no such route, stream, widget or option
    NotFound,
    /// 405: the route exists but doesn't take this method
    MethodNotAllowed,
    /// 409: glassdeckd is busy with something that has to finish first
    Conflict,
    /// 503: the keyboard isn't connected
    Unavailable,
    /// 500: something failed that the request couldn't have avoided
    Internal,
}

impl ErrorCode {
    pub fn http_status(self) -> u16 {
        match self {
            Self::Unauthorized => 401,
            Self::BadRequest => 400,
            Self::NotFound => 404,
            Self::MethodNotAllowed => 405,
            Self::Conflict => 409,
            Self::Unavailable => 503,
            Self::Internal => 500,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ApiError {
    pub code: ErrorCode,
    /// Says what went wrong, for showing to the user as is
    pub error: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Status {
    pub link: LinkStatus,
    pub bg: BgView,
    pub stream: StreamStatus,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct LinkStatus {
    /// The keyboard's address, such as `10.0.0.186:7878`
    pub keyboard: String,
    pub connected: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum RunsOn {
    Keyboard,
    Host,
}

/// The chosen background: `gradient`, a `.deckbg` name, or `stream:<name>`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct BgView {
    /// Nothing chosen yet, so the keyboard shows its own
    pub bg: Option<String>,
    pub runs_on: RunsOn,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct SetBg {
    pub bg: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct BgChosen {
    pub bg: String,
    pub runs_on: RunsOn,
    pub state: ShowState,
}

/// Where a chosen background got to by the time `PUT /bg` answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ShowState {
    Shown,
    /// A stream starts once the keyboard connects
    WaitingForKeyboard,
    /// A stream starts once a large upload is answered
    AfterUpload,
    /// A stream is taking a while to start (web pages are the slow ones)
    Starting,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Streams {
    pub streams: Vec<StreamEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct StreamEntry {
    pub name: String,
    pub mode: StreamMode,
    /// What provides it, such as `built in`
    pub from: String,
    pub shown: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum StreamMode {
    /// Shown on arrival, as with mirroring
    Live,
}

/// The source runner, which plays at most one stream at a time.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct StreamStatus {
    pub state: StreamState,
    /// The stream it's playing, or will play once it can
    pub playing: Option<String>,
    #[ts(type = "number | null")]
    pub worker: Option<u64>,
    /// The current worker's stream has started
    pub streaming: bool,
    pub last_ended: Option<Ended>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum StreamState {
    #[default]
    Idle,
    Running,
    /// The Link dropped mid-stream; it starts again once the keyboard is back
    WaitingForKeyboard,
    PausedForUpload,
}

impl StreamState {
    pub fn describe(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Running => "running",
            Self::WaitingForKeyboard => "waiting for the keyboard",
            Self::PausedForUpload => "paused for an upload",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Ended {
    #[ts(type = "number")]
    pub worker: u64,
    /// How it ended, such as `finished` or `failed: ...`
    pub said: String,
}

/// Whose options a view or change is about.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export)]
pub enum OptionsOwner {
    /// The selected background, a canvas
    Background { name: String },
    /// A widget, by its place in the profile
    Widget { index: u32, module: String },
}

impl std::fmt::Display for OptionsOwner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Background { name } => f.write_str(name),
            Self::Widget { index, module } => write!(f, "widget {index} ({module})"),
        }
    }
}

/// An options panel as the user would see it now, with each option's current value.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct OptionsView {
    pub owner: OptionsOwner,
    pub items: Vec<PanelItem>,
    /// Stored values that no longer fit, with why; their defaults apply
    pub dropped: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "item", rename_all = "snake_case")]
#[ts(export)]
pub enum PanelItem {
    Section { path: String, label: String, items: Vec<PanelItem> },
    Heading { text: String },
    Option(OptionView),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct OptionView {
    /// Its key with its sections', such as `motion.rings.speed`
    pub path: String,
    pub label: String,
    pub kind: OptionKind,
    /// A number, boolean or string; colours are `#rrggbbaa`
    pub value: Json,
    /// Hidden options still hold values; the panel just doesn't draw them
    pub visible: bool,
    pub hint: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export)]
pub enum OptionKind {
    Color { default: String, alpha: bool },
    Range { default: f64, min: f64, max: f64, step: Option<f64>, unit: Option<String> },
    Number { default: f64, min: Option<f64>, max: Option<f64>, step: Option<f64>, unit: Option<String> },
    Toggle { default: bool },
    Text { default: String, placeholder: Option<String>, max_length: Option<u32> },
    Choice { default: String, values: Vec<String> },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct SetOption {
    pub path: String,
    /// The value for the option's kind, as in [`OptionView::value`]; `null` goes back to the default
    pub value: Option<Json>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct OptionSet {
    pub owner: OptionsOwner,
    pub path: String,
    /// The value now in use, the default after a reset
    pub value: Json,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct Deploy {
    /// A `.deckbg`, a `.deckmod` or a profile, as a path glassdeckd can read
    pub path: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Deployed {
    /// What the keyboard installed, such as `module dev.glassdeck.clock.deckmod`
    pub deployed: String,
}

/// What `GET /events` sends, each as one server-sent event's JSON data.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export)]
pub enum Event {
    /// Everything at once: first on connecting, and again if this client fell behind
    Status { status: Status },
    Link { connected: bool },
    Stream { status: StreamStatus },
    Bg { bg: BgView },
    /// An owner's chosen values changed; fetch its view again to see them
    Options { owner: OptionsOwner },
    Deployed { deployed: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_shapes_are_tagged_and_snake_case() {
        let event = Event::Options { owner: OptionsOwner::Widget { index: 1, module: "dev.glassdeck.clock".into() } };
        assert_eq!(
            serde_json::to_value(&event).unwrap(),
            serde_json::json!({ "type": "options", "owner": { "kind": "widget", "index": 1, "module": "dev.glassdeck.clock" } })
        );
        let item = PanelItem::Heading { text: "Look".into() };
        assert_eq!(serde_json::to_value(&item).unwrap(), serde_json::json!({ "item": "heading", "text": "Look" }));
        let kind = OptionKind::Toggle { default: true };
        assert_eq!(serde_json::to_value(&kind).unwrap(), serde_json::json!({ "type": "toggle", "default": true }));
        assert_eq!(serde_json::to_value(StreamState::WaitingForKeyboard).unwrap(), "waiting_for_keyboard");
    }

    #[test]
    fn unknown_fields_in_requests_are_refused() {
        assert!(serde_json::from_str::<SetBg>(r#"{"bg":"gradient","extra":1}"#).is_err());
        let set: SetOption = serde_json::from_str(r#"{"path":"seconds","value":null}"#).unwrap();
        assert_eq!(set.value, None);
    }
}
