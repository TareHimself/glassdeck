//! Which source owns the screen: a PC stream first, then the most recent local background, then the built-in scene.

use std::fmt;

use anyhow::{Result, ensure};

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Screen {
    /// The built-in scene. `recent` is a background that stopped on its own; it's retried when a
    /// stream or an install ends.
    Fallback { recent: Option<BackgroundName> },
    /// A PC stream; `recent` comes back when it ends. While `installing`, the built-in scene comes
    /// back instead, until the install ends.
    Stream { recent: Option<BackgroundName>, installing: bool },
    /// A background playing on the keyboard itself.
    Local(BackgroundName),
    /// A large file is arriving, shown over the built-in scene; `recent` comes back after.
    Installing { recent: Option<BackgroundName> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    StreamStarted,
    StreamEnded,
    BackgroundSelected(BackgroundName),
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
    Remember(BackgroundName),
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
    /// Where the keyboard starts: the remembered background if there is one, else the built-in scene.
    pub fn at_boot(remembered: Option<BackgroundName>) -> (Self, Vec<Effect>) {
        match remembered {
            Some(background) => (Self::Local(background.clone()), vec![Effect::Start(background)]),
            None => (Self::Fallback { recent: None }, vec![Effect::ShowFallback]),
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

        (Fallback { .. }, Event::BackgroundSelected(background)) => {
            (Local(background.clone()), vec![HideFallback, Remember(background.clone()), Start(background)])
        }
        (Local(_), Event::BackgroundSelected(background)) => {
            (Local(background.clone()), vec![Stop, Remember(background.clone()), Start(background)])
        }
        (Stream { installing, .. }, Event::BackgroundSelected(background)) => {
            (Stream { recent: Some(background.clone()), installing }, vec![Remember(background)])
        }
        (Installing { .. }, Event::BackgroundSelected(background)) => {
            (Installing { recent: Some(background.clone()) }, vec![Remember(background)])
        }

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
        let select_b = || BackgroundSelected(b.clone());
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
            (fallback.clone(), select_b(), Ok((local_b.clone(), vec![HideFallback, Remember(b.clone()), Start(b.clone())]))),
            (fallback_after_a.clone(), select_b(), Ok((local_b.clone(), vec![HideFallback, Remember(b.clone()), Start(b.clone())]))),
            (local_a.clone(), select_b(), Ok((local_b.clone(), vec![Stop, Remember(b.clone()), Start(b.clone())]))),
            (
                stream_over_a.clone(),
                select_b(),
                Ok((Screen::Stream { recent: Some(b.clone()), installing: false }, vec![Remember(b.clone())])),
            ),
            (
                stream_installing.clone(),
                select_b(),
                Ok((Screen::Stream { recent: Some(b.clone()), installing: true }, vec![Remember(b.clone())])),
            ),
            (installing.clone(), select_b(), Ok((Screen::Installing { recent: Some(b.clone()) }, vec![Remember(b.clone())]))),
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
    fn boots_into_the_remembered_background_or_the_built_in_scene() {
        let a = background("a.deckbg");
        assert_eq!(Screen::at_boot(Some(a.clone())), (Screen::Local(a.clone()), vec![Effect::Start(a)]));
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
