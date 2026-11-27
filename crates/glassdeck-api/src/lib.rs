//! What glassdeckd's local API sends and takes, shared by glassdeckd, the CLI and the desktop app.
//! They derive `specta::Type`; the desktop app exports their TypeScript twins with its commands.
//!
//! | Route | Takes | Answers |
//! |---|---|---|
//! | `GET /status` | | [`Status`] |
//! | `GET /streams` | | [`Streams`] |
//! | `GET /theme` | | [`Theme`] |
//! | `PUT /theme` | [`ThemePush`] | [`ThemeApplied`] |
//! | `GET /events` | | server-sent [`Event`]s, starting with [`Event::Status`] |
//!
//! `PUT /theme` makes a whole theme the active one; glassdeckd acts only on what differs from the
//! running one. glassdeckd is a bridge: the app keeps the library, and glassdeckd keeps copies of
//! only the active theme's packages, as does the keyboard. The options panel types are for the app,
//! which runs `options.luau` itself.
//!
//! Every request carries `Authorization: Bearer <token>`, with the token from [`ApiFile`]. A
//! refusal answers with an [`ApiError`] and the status code its [`ErrorCode`] names.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use specta::Type;

pub const API_FORMAT_VERSION: u32 = 4;
pub const THEME_FORMAT_VERSION: u32 = 1;

fn local_folder() -> Result<PathBuf> {
    let local = std::env::var_os("LOCALAPPDATA").context("LOCALAPPDATA isn't set")?;
    Ok(PathBuf::from(local).join("Glassdeck"))
}

/// Where glassdeckd writes [`ApiFile`] when it starts.
pub fn api_file() -> Result<PathBuf> {
    Ok(local_folder()?.join("api.json"))
}

/// Where glassdeckd keeps its copies of the active theme's packages, as `<id>.deck`. Clients may
/// read them, such as to run a package's `options.luau`; only glassdeckd writes here.
pub fn active_packages() -> Result<PathBuf> {
    Ok(local_folder()?.join("active"))
}

/// How to reach the running glassdeckd: `http://127.0.0.1:<port>` with the bearer token.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct ApiFile {
    pub format: u32,
    pub port: u16,
    pub token: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ApiError {
    pub code: ErrorCode,
    /// Says what went wrong, for showing to the user as is
    pub error: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct Status {
    pub link: LinkStatus,
    pub stream: StreamStatus,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct LinkStatus {
    /// The keyboard's address, such as `10.0.0.186:7878`
    pub keyboard: String,
    pub connected: bool,
}

/// One whole setup: the background, the stream to show when it's available, and the widgets. Each
/// entry names a package by id and carries its own chosen options.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct Theme {
    pub format: u32,
    pub background: ThemeBackground,
    /// Shown whenever it's available; otherwise the keyboard plays `background`
    #[serde(default)]
    pub stream: Option<String>,
    #[serde(default)]
    pub widgets: Vec<ThemeWidget>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct ThemeBackground {
    /// A background's qualified id, such as `background.local.glassdeck.gradient`
    pub package: String,
    /// Chosen values by option path, as in [`OptionView::value`]
    #[serde(default)]
    pub options: serde_json::Map<String, Json>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct ThemeWidget {
    /// A widget's qualified id, such as `widget.local.me.clock`
    pub package: String,
    /// Where it sits in the widget area, in panel pixels on an 8 px grid
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    #[serde(default)]
    pub options: serde_json::Map<String, Json>,
}

impl Theme {
    /// The built-in gradient and no widgets: what a fresh install shows.
    pub fn starting(built_in_background: &str) -> Self {
        Self {
            format: THEME_FORMAT_VERSION,
            background: ThemeBackground { package: built_in_background.to_owned(), options: Default::default() },
            stream: None,
            widgets: Vec::new(),
        }
    }
}

/// A theme to make active, with where each of its packages is in the app's library. A package
/// glassdeckd already holds the same copy of may be left out; one it doesn't hold must be given.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct ThemePush {
    pub theme: Theme,
    /// Package id to the path of its `.deck`, readable by glassdeckd
    #[serde(default)]
    pub packages: std::collections::BTreeMap<String, String>,
}

/// What `PUT /theme` did.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ThemeApplied {
    /// Where the theme's stream got to, when it has one
    pub stream: Option<ShowState>,
}

/// Where a theme's stream got to by the time `PUT /theme` answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ShowState {
    Shown,
    /// A stream starts once the keyboard connects
    WaitingForKeyboard,
    /// A stream starts once a large upload is answered
    AfterUpload,
    /// A stream is taking a while to start (web pages are the slow ones)
    Starting,
    /// The stream isn't there now, such as an unplugged monitor; the theme keeps it for when it is
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct Streams {
    pub streams: Vec<StreamEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct StreamEntry {
    pub name: String,
    pub mode: StreamMode,
    /// What provides it, such as `built in`
    pub from: String,
    pub shown: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum StreamMode {
    /// Shown on arrival, as with mirroring
    Live,
}

/// The source runner, which plays at most one stream at a time.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct StreamStatus {
    pub state: StreamState,
    /// The stream it's playing, or will play once it can
    pub playing: Option<String>,
    #[specta(type = Option<f64>)]
    pub worker: Option<u64>,
    /// The current worker's stream has started
    pub streaming: bool,
    pub last_ended: Option<Ended>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct Ended {
    #[specta(type = f64)]
    pub worker: u64,
    /// How it ended, such as `finished` or `failed: ...`
    pub said: String,
}

/// Whose options a view or change is about.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OptionsOwner {
    /// The theme's background
    Background { package: String },
    /// A widget, by its place in the theme
    Widget { index: u32, package: String },
}

impl std::fmt::Display for OptionsOwner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Background { package } => f.write_str(package),
            Self::Widget { index, package } => write!(f, "widget {index} ({package})"),
        }
    }
}

/// An options panel as the user would see it now, with each option's current value.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Type)]
pub struct OptionsView {
    pub owner: OptionsOwner,
    pub items: Vec<PanelItem>,
    /// Stored values that no longer fit, with why; their defaults apply
    pub dropped: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Type)]
#[serde(tag = "item", rename_all = "snake_case")]
pub enum PanelItem {
    Section { path: String, label: String, items: Vec<PanelItem> },
    Heading { text: String },
    Option(OptionView),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Type)]
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Type)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OptionKind {
    Color { default: String, alpha: bool },
    Range { default: f64, min: f64, max: f64, step: Option<f64>, unit: Option<String> },
    Number { default: f64, min: Option<f64>, max: Option<f64>, step: Option<f64>, unit: Option<String> },
    Toggle { default: bool },
    Text { default: String, placeholder: Option<String>, max_length: Option<u32> },
    Choice { default: String, values: Vec<String> },
}

/// What `GET /events` sends, each as one server-sent event's JSON data.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Type)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// Everything at once: first on connecting, and again if this client fell behind
    Status { status: Status },
    Link { connected: bool },
    Stream { status: StreamStatus },
    /// A new active theme
    Theme { theme: Theme },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_shapes_are_tagged_and_snake_case() {
        let event = Event::Link { connected: true };
        assert_eq!(serde_json::to_value(&event).unwrap(), serde_json::json!({ "type": "link", "connected": true }));
        let owner = OptionsOwner::Widget { index: 1, package: "widget.local.me.clock".into() };
        assert_eq!(serde_json::to_value(&owner).unwrap(), serde_json::json!({ "kind": "widget", "index": 1, "package": "widget.local.me.clock" }));
        let item = PanelItem::Heading { text: "Look".into() };
        assert_eq!(serde_json::to_value(&item).unwrap(), serde_json::json!({ "item": "heading", "text": "Look" }));
        let kind = OptionKind::Toggle { default: true };
        assert_eq!(serde_json::to_value(&kind).unwrap(), serde_json::json!({ "type": "toggle", "default": true }));
        assert_eq!(serde_json::to_value(StreamState::WaitingForKeyboard).unwrap(), "waiting_for_keyboard");
    }

    #[test]
    fn unknown_fields_in_requests_are_refused() {
        assert!(serde_json::from_str::<ThemePush>(r#"{"theme":{"format":1,"background":{"package":"a.b"}},"extra":1}"#).is_err());
        let theme: Theme = serde_json::from_str(r#"{"format":1,"background":{"package":"background.local.glassdeck.gradient"}}"#).unwrap();
        assert_eq!(theme, Theme::starting("background.local.glassdeck.gradient"));
        assert!(serde_json::from_str::<Theme>(r#"{"format":1,"background":{"package":"a.b"},"effects":[]}"#).is_err());
    }
}
