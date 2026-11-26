//! Which source owns the screen: a PC stream first, then the most recent local background, then the built-in canvas.

use std::fmt;

use anyhow::{Result, ensure};
use glassdeck_proto::BUILT_IN_BACKGROUND;

/// A background's file name inside the background folder, checked so it can't name anything outside it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackgroundName(String);

impl BackgroundName {
    pub fn parse(name: &str) -> Result<Self> {
        let is_plain = !name.is_empty()
            && !name.starts_with('.')
            && name.chars().all(|c| c.is_ascii_alphanumeric() || "-_. ".contains(c));
        ensure!(is_plain, "{name:?} is not a plain file name");
        ensure!(
            name.strip_suffix(glassdeck_package::EXTENSION).is_some_and(|stem| stem.ends_with('.') && stem.len() > 1),
            "{name:?} does not end in .{}",
            glassdeck_package::EXTENSION
        );
        Ok(Self(name.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for BackgroundName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The background the user chose for when nothing streams: the built-in canvas, which is the default
/// and always on the keyboard, or a file in the background folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Selection {
    BuiltIn,
    File(BackgroundName),
}

impl Selection {
    pub fn parse(name: &str) -> Result<Self> {
        match name {
            BUILT_IN_BACKGROUND => Ok(Self::BuiltIn),
            file => BackgroundName::parse(file).map(Self::File),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::BuiltIn => BUILT_IN_BACKGROUND,
            Self::File(name) => name.as_str(),
        }
    }

    /// The file to come back to after a stream or an install; none means the built-in canvas.
    fn file(&self) -> Option<BackgroundName> {
        match self {
            Self::BuiltIn => None,
            Self::File(name) => Some(name.clone()),
        }
    }
}

impl fmt::Display for Selection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Screen {
    /// The built-in canvas: chosen, or shown because nothing else can be. `recent` is a background that
    /// stopped on its own; it's retried when a stream or an install ends.
    Fallback { recent: Option<BackgroundName> },
    /// A PC stream; `recent` comes back when it ends. While `installing`, the built-in canvas comes
    /// back instead, until the install ends.
    Stream { recent: Option<BackgroundName>, installing: bool },
    /// A background playing on the keyboard itself.
    Local(BackgroundName),
    /// A large file is arriving, shown over the built-in canvas; `recent` comes back after.
    Installing { recent: Option<BackgroundName> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    StreamStarted,
    StreamEnded,
    BackgroundSelected(Selection),
    /// The running background stopped by itself: it failed, or its renderer exited.
    BackgroundStopped,
    /// A large file started arriving; its background may be the one being replaced.
    InstallStarted,
    InstallEnded,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    Start(BackgroundName),
    Stop,
    /// Persist the choice, so the keyboard plays it after a restart.
    Remember(Selection),
    ShowFallback,
    HideFallback,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScreenError {
    NoStreamToEnd,
    NoBackgroundRunning,
    AlreadyInstalling,
    NoInstallToEnd,
}

impl fmt::Display for ScreenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoStreamToEnd => write!(f, "a stream ended while none owned the screen"),
            Self::NoBackgroundRunning => write!(f, "a background stopped while none was running"),
            Self::AlreadyInstalling => write!(f, "an install started while another was running"),
            Self::NoInstallToEnd => write!(f, "an install ended while none was running"),
        }
    }
}

impl std::error::Error for ScreenError {}

impl Screen {
    /// Where the keyboard starts: the remembered background, or the built-in canvas by default.
    pub fn at_boot(remembered: Option<Selection>) -> (Self, Vec<Effect>) {
        match remembered {
            Some(Selection::File(background)) => (Self::Local(background.clone()), vec![Effect::Start(background)]),
            Some(Selection::BuiltIn) | None => (Self::Fallback { recent: None }, vec![Effect::ShowFallback]),
        }
    }
}

/// Every change of screen owner goes through here; the test walks the full table.
pub fn step(screen: Screen, event: Event) -> Result<(Screen, Vec<Effect>), ScreenError> {
    use Effect::{HideFallback, Remember, ShowFallback, Start, Stop};
    use Screen::{Fallback, Installing, Local, Stream};
    Ok(match (screen, event) {
        (Fallback { recent }, Event::StreamStarted) => (Stream { recent, installing: false }, vec![HideFallback]),
        (Installing { recent }, Event::StreamStarted) => (Stream { recent, installing: true }, vec![HideFallback]),
        (stream @ Stream { .. }, Event::StreamStarted) => (stream, vec![]),
        (Local(background), Event::StreamStarted) => (Stream { recent: Some(background), installing: false }, vec![Stop]),

        (Stream { recent, installing: true }, Event::StreamEnded) => (Installing { recent }, vec![ShowFallback]),
        (Stream { recent: Some(background), installing: false }, Event::StreamEnded) => {
            (Local(background.clone()), vec![Start(background)])
        }
        (Stream { recent: None, installing: false }, Event::StreamEnded) => (Fallback { recent: None }, vec![ShowFallback]),
        (Fallback { .. } | Local(_) | Installing { .. }, Event::StreamEnded) => return Err(ScreenError::NoStreamToEnd),

        (Fallback { .. }, Event::BackgroundSelected(Selection::File(background))) => {
            (Local(background.clone()), vec![HideFallback, Remember(Selection::File(background.clone())), Start(background)])
        }
        (Fallback { .. }, Event::BackgroundSelected(Selection::BuiltIn)) => (Fallback { recent: None }, vec![Remember(Selection::BuiltIn)]),
        (Local(_), Event::BackgroundSelected(Selection::File(background))) => {
            (Local(background.clone()), vec![Stop, Remember(Selection::File(background.clone())), Start(background)])
        }
        (Local(_), Event::BackgroundSelected(Selection::BuiltIn)) => {
            (Fallback { recent: None }, vec![Stop, Remember(Selection::BuiltIn), ShowFallback])
        }
        (Stream { installing, .. }, Event::BackgroundSelected(selection)) => {
            (Stream { recent: selection.file(), installing }, vec![Remember(selection)])
        }
        (Installing { .. }, Event::BackgroundSelected(selection)) => (Installing { recent: selection.file() }, vec![Remember(selection)]),

        (Local(background), Event::BackgroundStopped) => (Fallback { recent: Some(background) }, vec![ShowFallback]),
        (Fallback { .. } | Stream { .. } | Installing { .. }, Event::BackgroundStopped) => {
            return Err(ScreenError::NoBackgroundRunning);
        }

        (Fallback { recent }, Event::InstallStarted) => (Installing { recent }, vec![]),
        (Local(background), Event::InstallStarted) => (Installing { recent: Some(background) }, vec![Stop, ShowFallback]),
        (Stream { recent, installing: false }, Event::InstallStarted) => (Stream { recent, installing: true }, vec![]),
        (Installing { .. } | Stream { installing: true, .. }, Event::InstallStarted) => {
            return Err(ScreenError::AlreadyInstalling);
        }

        (Installing { recent: Some(background) }, Event::InstallEnded) => {
            (Local(background.clone()), vec![HideFallback, Start(background)])
        }
        (Installing { recent: None }, Event::InstallEnded) => (Fallback { recent: None }, vec![]),
        (Stream { recent, installing: true }, Event::InstallEnded) => (Stream { recent, installing: false }, vec![]),
        (Fallback { .. } | Local(_) | Stream { installing: false, .. }, Event::InstallEnded) => {
            return Err(ScreenError::NoInstallToEnd);
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    type Row = (Screen, Event, Result<(Screen, Vec<Effect>), ScreenError>);

    fn background(name: &str) -> BackgroundName {
        BackgroundName::parse(name).unwrap()
    }

    #[test]
    fn walks_the_whole_transition_table() {
        use Effect::{HideFallback, Remember, ShowFallback, Start, Stop};
        use Event::{BackgroundSelected, BackgroundStopped, InstallEnded, InstallStarted, StreamEnded, StreamStarted};
        use ScreenError::{AlreadyInstalling, NoBackgroundRunning, NoInstallToEnd, NoStreamToEnd};
        let (a, b) = (background("a.deckbg"), background("b.deckbg"));
        let fallback = Screen::Fallback { recent: None };
        let fallback_after_a = Screen::Fallback { recent: Some(a.clone()) };
        let stream = Screen::Stream { recent: None, installing: false };
        let stream_over_a = Screen::Stream { recent: Some(a.clone()), installing: false };
        let stream_installing = Screen::Stream { recent: None, installing: true };
        let stream_over_a_installing = Screen::Stream { recent: Some(a.clone()), installing: true };
        let local_a = Screen::Local(a.clone());
        let local_b = Screen::Local(b.clone());
        let installing = Screen::Installing { recent: None };
        let installing_over_a = Screen::Installing { recent: Some(a.clone()) };
        let select_b = || BackgroundSelected(Selection::File(b.clone()));
        let select_built_in = || BackgroundSelected(Selection::BuiltIn);
        let remember_b = || Remember(Selection::File(b.clone()));
        let table: Vec<Row> = vec![
            (fallback.clone(), StreamStarted, Ok((stream.clone(), vec![HideFallback]))),
            (fallback_after_a.clone(), StreamStarted, Ok((stream_over_a.clone(), vec![HideFallback]))),
            (installing_over_a.clone(), StreamStarted, Ok((stream_over_a_installing.clone(), vec![HideFallback]))),
            (stream.clone(), StreamStarted, Ok((stream.clone(), vec![]))),
            (stream_installing.clone(), StreamStarted, Ok((stream_installing.clone(), vec![]))),
            (local_a.clone(), StreamStarted, Ok((stream_over_a.clone(), vec![Stop]))),
            (stream_over_a.clone(), StreamEnded, Ok((local_a.clone(), vec![Start(a.clone())]))),
            (stream.clone(), StreamEnded, Ok((fallback.clone(), vec![ShowFallback]))),
            (stream_over_a_installing.clone(), StreamEnded, Ok((installing_over_a.clone(), vec![ShowFallback]))),
            (fallback.clone(), StreamEnded, Err(NoStreamToEnd)),
            (local_a.clone(), StreamEnded, Err(NoStreamToEnd)),
            (installing.clone(), StreamEnded, Err(NoStreamToEnd)),
            (fallback.clone(), select_b(), Ok((local_b.clone(), vec![HideFallback, remember_b(), Start(b.clone())]))),
            (fallback_after_a.clone(), select_b(), Ok((local_b.clone(), vec![HideFallback, remember_b(), Start(b.clone())]))),
            (local_a.clone(), select_b(), Ok((local_b.clone(), vec![Stop, remember_b(), Start(b.clone())]))),
            (
                stream_over_a.clone(),
                select_b(),
                Ok((Screen::Stream { recent: Some(b.clone()), installing: false }, vec![remember_b()])),
            ),
            (
                stream_installing.clone(),
                select_b(),
                Ok((Screen::Stream { recent: Some(b.clone()), installing: true }, vec![remember_b()])),
            ),
            (installing.clone(), select_b(), Ok((Screen::Installing { recent: Some(b.clone()) }, vec![remember_b()]))),
            (fallback_after_a.clone(), select_built_in(), Ok((fallback.clone(), vec![Remember(Selection::BuiltIn)]))),
            (local_a.clone(), select_built_in(), Ok((fallback.clone(), vec![Stop, Remember(Selection::BuiltIn), ShowFallback]))),
            (stream_over_a_installing.clone(), select_built_in(), Ok((stream_installing.clone(), vec![Remember(Selection::BuiltIn)]))),
            (installing_over_a.clone(), select_built_in(), Ok((installing.clone(), vec![Remember(Selection::BuiltIn)]))),
            (local_a.clone(), BackgroundStopped, Ok((fallback_after_a.clone(), vec![ShowFallback]))),
            (fallback.clone(), BackgroundStopped, Err(NoBackgroundRunning)),
            (stream.clone(), BackgroundStopped, Err(NoBackgroundRunning)),
            (installing.clone(), BackgroundStopped, Err(NoBackgroundRunning)),
            (fallback.clone(), InstallStarted, Ok((installing.clone(), vec![]))),
            (fallback_after_a.clone(), InstallStarted, Ok((installing_over_a.clone(), vec![]))),
            (local_a.clone(), InstallStarted, Ok((installing_over_a.clone(), vec![Stop, ShowFallback]))),
            (stream_over_a.clone(), InstallStarted, Ok((stream_over_a_installing.clone(), vec![]))),
            (installing.clone(), InstallStarted, Err(AlreadyInstalling)),
            (stream_installing.clone(), InstallStarted, Err(AlreadyInstalling)),
            (installing_over_a.clone(), InstallEnded, Ok((local_a.clone(), vec![HideFallback, Start(a.clone())]))),
            (installing.clone(), InstallEnded, Ok((fallback.clone(), vec![]))),
            (stream_over_a_installing.clone(), InstallEnded, Ok((stream_over_a.clone(), vec![]))),
            (fallback.clone(), InstallEnded, Err(NoInstallToEnd)),
            (local_a.clone(), InstallEnded, Err(NoInstallToEnd)),
            (stream, InstallEnded, Err(NoInstallToEnd)),
        ];
        for (from, event, expected) in table {
            assert_eq!(step(from.clone(), event.clone()), expected, "from {from:?} on {event:?}");
        }
    }

    #[test]
    fn boots_into_the_remembered_background_or_the_built_in_canvas() {
        let a = background("a.deckbg");
        assert_eq!(Screen::at_boot(Some(Selection::File(a.clone()))), (Screen::Local(a.clone()), vec![Effect::Start(a)]));
        assert_eq!(Screen::at_boot(Some(Selection::BuiltIn)), (Screen::Fallback { recent: None }, vec![Effect::ShowFallback]));
        assert_eq!(Screen::at_boot(None), (Screen::Fallback { recent: None }, vec![Effect::ShowFallback]));
    }

    #[test]
    fn background_names_stay_inside_the_background_folder() {
        assert!(BackgroundName::parse("Late Night_2.deckbg").is_ok());
        for bad in ["", ".deckbg", "../x.deckbg", "a/b.deckbg", ".hidden.deckbg", "x.mp4", "xdeckbg", "x.deckbg\0"] {
            assert!(BackgroundName::parse(bad).is_err(), "{bad:?} should be refused");
        }
    }
}
