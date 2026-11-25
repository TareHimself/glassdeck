//! Which source owns the screen: a PC stream first, then the most recent local background, then nothing.

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
    /// Nothing to show. `recent` is a background that stopped on its own; the next stream's end retries it.
    Idle { recent: Option<BackgroundName> },
    /// A PC stream; `recent` comes back when it ends.
    Stream { recent: Option<BackgroundName> },
    /// A background playing on the keyboard itself.
    Local(BackgroundName),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    StreamStarted,
    StreamEnded,
    BackgroundSelected(BackgroundName),
    /// The running background stopped by itself: it failed, or its renderer exited.
    BackgroundStopped,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    Start(BackgroundName),
    Stop,
    /// Persist the choice, so the keyboard plays it after a restart.
    Remember(BackgroundName),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScreenError {
    NoStreamToEnd,
    NoBackgroundRunning,
}

impl fmt::Display for ScreenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoStreamToEnd => write!(f, "a stream ended while none owned the screen"),
            Self::NoBackgroundRunning => write!(f, "a background stopped while none was running"),
        }
    }
}

impl std::error::Error for ScreenError {}

impl Screen {
    /// Where the keyboard starts: the remembered background if there is one.
    pub fn at_boot(remembered: Option<BackgroundName>) -> (Self, Vec<Effect>) {
        match remembered {
            Some(background) => (Self::Local(background.clone()), vec![Effect::Start(background)]),
            None => (Self::Idle { recent: None }, vec![]),
        }
    }
}

/// Every change of screen owner goes through here; the test walks the full table.
pub fn step(screen: Screen, event: Event) -> Result<(Screen, Vec<Effect>), ScreenError> {
    use Effect::{Remember, Start, Stop};
    match (screen, event) {
        (Screen::Idle { recent } | Screen::Stream { recent }, Event::StreamStarted) => {
            Ok((Screen::Stream { recent }, vec![]))
        }
        (Screen::Local(background), Event::StreamStarted) => Ok((Screen::Stream { recent: Some(background) }, vec![Stop])),

        (Screen::Stream { recent: Some(background) }, Event::StreamEnded) => {
            Ok((Screen::Local(background.clone()), vec![Start(background)]))
        }
        (Screen::Stream { recent: None }, Event::StreamEnded) => Ok((Screen::Idle { recent: None }, vec![])),
        (Screen::Idle { .. } | Screen::Local(_), Event::StreamEnded) => Err(ScreenError::NoStreamToEnd),

        (Screen::Idle { .. }, Event::BackgroundSelected(background)) => {
            Ok((Screen::Local(background.clone()), vec![Remember(background.clone()), Start(background)]))
        }
        (Screen::Local(_), Event::BackgroundSelected(background)) => {
            Ok((Screen::Local(background.clone()), vec![Stop, Remember(background.clone()), Start(background)]))
        }
        (Screen::Stream { .. }, Event::BackgroundSelected(background)) => {
            Ok((Screen::Stream { recent: Some(background.clone()) }, vec![Remember(background)]))
        }

        (Screen::Local(background), Event::BackgroundStopped) => Ok((Screen::Idle { recent: Some(background) }, vec![])),
        (Screen::Idle { .. } | Screen::Stream { .. }, Event::BackgroundStopped) => Err(ScreenError::NoBackgroundRunning),
    }
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
        use Effect::{Remember, Start, Stop};
        use Event::{BackgroundSelected, BackgroundStopped, StreamEnded, StreamStarted};
        let (a, b) = (background("a.deckbg"), background("b.deckbg"));
        let idle = Screen::Idle { recent: None };
        let idle_after_a = Screen::Idle { recent: Some(a.clone()) };
        let stream = Screen::Stream { recent: None };
        let stream_over_a = Screen::Stream { recent: Some(a.clone()) };
        let local_a = Screen::Local(a.clone());
        let table: [Row; 17] = [
            (idle.clone(), StreamStarted, Ok((stream.clone(), vec![]))),
            (idle_after_a.clone(), StreamStarted, Ok((stream_over_a.clone(), vec![]))),
            (stream.clone(), StreamStarted, Ok((stream.clone(), vec![]))),
            (stream_over_a.clone(), StreamStarted, Ok((stream_over_a.clone(), vec![]))),
            (local_a.clone(), StreamStarted, Ok((stream_over_a.clone(), vec![Stop]))),
            (stream_over_a.clone(), StreamEnded, Ok((local_a.clone(), vec![Start(a.clone())]))),
            (stream.clone(), StreamEnded, Ok((idle.clone(), vec![]))),
            (idle.clone(), StreamEnded, Err(ScreenError::NoStreamToEnd)),
            (local_a.clone(), StreamEnded, Err(ScreenError::NoStreamToEnd)),
            (
                idle.clone(),
                BackgroundSelected(b.clone()),
                Ok((Screen::Local(b.clone()), vec![Remember(b.clone()), Start(b.clone())])),
            ),
            (
                idle_after_a.clone(),
                BackgroundSelected(b.clone()),
                Ok((Screen::Local(b.clone()), vec![Remember(b.clone()), Start(b.clone())])),
            ),
            (
                local_a.clone(),
                BackgroundSelected(b.clone()),
                Ok((Screen::Local(b.clone()), vec![Stop, Remember(b.clone()), Start(b.clone())])),
            ),
            (
                stream_over_a.clone(),
                BackgroundSelected(b.clone()),
                Ok((Screen::Stream { recent: Some(b.clone()) }, vec![Remember(b.clone())])),
            ),
            (local_a.clone(), BackgroundStopped, Ok((idle_after_a.clone(), vec![]))),
            (idle.clone(), BackgroundStopped, Err(ScreenError::NoBackgroundRunning)),
            (stream.clone(), BackgroundStopped, Err(ScreenError::NoBackgroundRunning)),
            (stream_over_a, BackgroundStopped, Err(ScreenError::NoBackgroundRunning)),
        ];
        for (from, event, expected) in table {
            assert_eq!(step(from.clone(), event.clone()), expected, "from {from:?} on {event:?}");
        }
    }

    #[test]
    fn boots_into_the_remembered_background() {
        let a = background("a.deckbg");
        assert_eq!(Screen::at_boot(Some(a.clone())), (Screen::Local(a.clone()), vec![Effect::Start(a)]));
        assert_eq!(Screen::at_boot(None), (Screen::Idle { recent: None }, vec![]));
    }

    #[test]
    fn background_names_stay_inside_the_background_folder() {
        assert!(BackgroundName::parse("Late Night_2.deckbg").is_ok());
        for bad in ["", ".deckbg", "../x.deckbg", "a/b.deckbg", ".hidden.deckbg", "x.mp4", "xdeckbg", "x.deckbg\0"] {
            assert!(BackgroundName::parse(bad).is_err(), "{bad:?} should be refused");
        }
    }
}
