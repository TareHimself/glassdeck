//! Which source owns the screen: a PC stream first, then the most recent local deck, then nothing.

use std::fmt;

use anyhow::{Result, ensure};

/// A deck's file name inside the deck folder, checked so it can't name anything outside it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeckName(String);

impl DeckName {
    pub fn parse(name: &str) -> Result<Self> {
        let is_plain = !name.is_empty()
            && !name.starts_with('.')
            && name.chars().all(|c| c.is_ascii_alphanumeric() || "-_. ".contains(c));
        ensure!(is_plain, "{name:?} is not a plain file name");
        ensure!(
            name.strip_suffix(glassdeck_deck::EXTENSION).is_some_and(|stem| stem.ends_with('.') && stem.len() > 1),
            "{name:?} does not end in .{}",
            glassdeck_deck::EXTENSION
        );
        Ok(Self(name.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DeckName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Screen {
    /// Nothing to show. `recent` is a deck that stopped on its own; the next stream's end retries it.
    Idle { recent: Option<DeckName> },
    /// A PC stream; `recent` comes back when it ends.
    Stream { recent: Option<DeckName> },
    /// A deck playing on the keyboard itself.
    Local(DeckName),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    StreamStarted,
    StreamEnded,
    DeckSelected(DeckName),
    /// The running deck stopped by itself: it failed, or its renderer exited.
    DeckStopped,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    Start(DeckName),
    Stop,
    /// Persist the choice, so the keyboard plays it after a restart.
    Remember(DeckName),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScreenError {
    NoStreamToEnd,
    NoDeckRunning,
}

impl fmt::Display for ScreenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoStreamToEnd => write!(f, "a stream ended while none owned the screen"),
            Self::NoDeckRunning => write!(f, "a deck stopped while none was running"),
        }
    }
}

impl std::error::Error for ScreenError {}

impl Screen {
    /// Where the keyboard starts: the remembered deck if there is one.
    pub fn at_boot(remembered: Option<DeckName>) -> (Self, Vec<Effect>) {
        match remembered {
            Some(deck) => (Self::Local(deck.clone()), vec![Effect::Start(deck)]),
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
        (Screen::Local(deck), Event::StreamStarted) => Ok((Screen::Stream { recent: Some(deck) }, vec![Stop])),

        (Screen::Stream { recent: Some(deck) }, Event::StreamEnded) => {
            Ok((Screen::Local(deck.clone()), vec![Start(deck)]))
        }
        (Screen::Stream { recent: None }, Event::StreamEnded) => Ok((Screen::Idle { recent: None }, vec![])),
        (Screen::Idle { .. } | Screen::Local(_), Event::StreamEnded) => Err(ScreenError::NoStreamToEnd),

        (Screen::Idle { .. }, Event::DeckSelected(deck)) => {
            Ok((Screen::Local(deck.clone()), vec![Remember(deck.clone()), Start(deck)]))
        }
        (Screen::Local(_), Event::DeckSelected(deck)) => {
            Ok((Screen::Local(deck.clone()), vec![Stop, Remember(deck.clone()), Start(deck)]))
        }
        (Screen::Stream { .. }, Event::DeckSelected(deck)) => {
            Ok((Screen::Stream { recent: Some(deck.clone()) }, vec![Remember(deck)]))
        }

        (Screen::Local(deck), Event::DeckStopped) => Ok((Screen::Idle { recent: Some(deck) }, vec![])),
        (Screen::Idle { .. } | Screen::Stream { .. }, Event::DeckStopped) => Err(ScreenError::NoDeckRunning),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type Row = (Screen, Event, Result<(Screen, Vec<Effect>), ScreenError>);

    fn deck(name: &str) -> DeckName {
        DeckName::parse(name).unwrap()
    }

    #[test]
    fn walks_the_whole_transition_table() {
        use Effect::{Remember, Start, Stop};
        use Event::{DeckSelected, DeckStopped, StreamEnded, StreamStarted};
        let (a, b) = (deck("a.deck"), deck("b.deck"));
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
                DeckSelected(b.clone()),
                Ok((Screen::Local(b.clone()), vec![Remember(b.clone()), Start(b.clone())])),
            ),
            (
                idle_after_a.clone(),
                DeckSelected(b.clone()),
                Ok((Screen::Local(b.clone()), vec![Remember(b.clone()), Start(b.clone())])),
            ),
            (
                local_a.clone(),
                DeckSelected(b.clone()),
                Ok((Screen::Local(b.clone()), vec![Stop, Remember(b.clone()), Start(b.clone())])),
            ),
            (
                stream_over_a.clone(),
                DeckSelected(b.clone()),
                Ok((Screen::Stream { recent: Some(b.clone()) }, vec![Remember(b.clone())])),
            ),
            (local_a.clone(), DeckStopped, Ok((idle_after_a.clone(), vec![]))),
            (idle.clone(), DeckStopped, Err(ScreenError::NoDeckRunning)),
            (stream.clone(), DeckStopped, Err(ScreenError::NoDeckRunning)),
            (stream_over_a, DeckStopped, Err(ScreenError::NoDeckRunning)),
        ];
        for (from, event, expected) in table {
            assert_eq!(step(from.clone(), event.clone()), expected, "from {from:?} on {event:?}");
        }
    }

    #[test]
    fn boots_into_the_remembered_deck() {
        let a = deck("a.deck");
        assert_eq!(Screen::at_boot(Some(a.clone())), (Screen::Local(a.clone()), vec![Effect::Start(a)]));
        assert_eq!(Screen::at_boot(None), (Screen::Idle { recent: None }, vec![]));
    }

    #[test]
    fn deck_names_stay_inside_the_deck_folder() {
        assert!(DeckName::parse("Late Night_2.deck").is_ok());
        for bad in ["", ".deck", "../x.deck", "a/b.deck", ".hidden.deck", "x.mp4", "xdeck", "x.deck\0"] {
            assert!(DeckName::parse(bad).is_err(), "{bad:?} should be refused");
        }
    }
}
