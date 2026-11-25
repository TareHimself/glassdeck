//! What the next atomic commit carries and what waits behind it: a decoded picture for the video
//! plane, a redrawn key layer, or both. Pictures are named by token; the compositor carries out the effects.

/// One commit's contents. The key layer is double-buffered, so "keys" means "show the back buffer".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Update {
    Video(u64),
    Keys,
    Both(u64),
}

impl Update {
    pub fn video(self) -> Option<u64> {
        match self {
            Update::Video(token) | Update::Both(token) => Some(token),
            Update::Keys => None,
        }
    }

    pub fn keys(self) -> bool {
        matches!(self, Update::Keys | Update::Both(_))
    }

    /// Folds a newer change into a waiting update; a picture it replaces is returned to be dropped.
    fn absorb(self, newer: Change) -> (Update, Option<u64>) {
        match (self, newer) {
            (Update::Video(old), Change::Picture(token)) => (Update::Video(token), Some(old)),
            (Update::Both(old), Change::Picture(token)) => (Update::Both(token), Some(old)),
            (Update::Keys, Change::Picture(token)) => (Update::Both(token), None),
            (Update::Video(token) | Update::Both(token), Change::Keys) => (Update::Both(token), None),
            (Update::Keys, Change::Keys) => (Update::Keys, None),
        }
    }
}

#[derive(Clone, Copy)]
enum Change {
    Picture(u64),
    Keys,
}

impl From<Change> for Update {
    fn from(change: Change) -> Self {
        match change {
            Change::Picture(token) => Update::Video(token),
            Change::Keys => Update::Keys,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scanout {
    /// No commit in flight. `on_screen` stays until a newer picture replaces it.
    Steady { on_screen: Option<u64> },
    /// A commit waits for vblank, and at most one newer update waits behind it.
    Flipping { on_screen: Option<u64>, incoming: Update, queued: Option<Update> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    Submitted(u64),
    /// The key layer's back buffer has new content to show
    KeysChanged,
    FlipDone,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    /// Put this update on screen with a non-blocking commit
    Commit(Update),
    /// This picture reached the screen
    Presented(u64),
    /// This picture left the screen; free its framebuffer and decoder buffer
    Release(u64),
    /// A newer picture replaced this one before it was ever committed
    Drop(u64),
    /// The key layer's back buffer is now on screen, so the buffers swap roles
    KeysFlipped,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FlipWithoutCommit;

impl std::fmt::Display for FlipWithoutCommit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the display reported a page flip nobody committed")
    }
}

impl std::error::Error for FlipWithoutCommit {}

/// Every move between scanout states goes through here; the test walks the full table.
pub fn step(state: Scanout, event: Event) -> Result<(Scanout, Vec<Effect>), FlipWithoutCommit> {
    use Effect::{Commit, KeysFlipped, Presented, Release};
    use Scanout::{Flipping, Steady};
    Ok(match (state, event) {
        (Steady { on_screen }, Event::Submitted(token)) => {
            (Flipping { on_screen, incoming: Update::Video(token), queued: None }, vec![Commit(Update::Video(token))])
        }
        (Steady { on_screen }, Event::KeysChanged) => {
            (Flipping { on_screen, incoming: Update::Keys, queued: None }, vec![Commit(Update::Keys)])
        }
        (Steady { .. }, Event::FlipDone) => return Err(FlipWithoutCommit),
        (Flipping { on_screen, incoming, queued }, Event::Submitted(token)) => queue(on_screen, incoming, queued, Change::Picture(token)),
        (Flipping { on_screen, incoming, queued }, Event::KeysChanged) => queue(on_screen, incoming, queued, Change::Keys),
        (Flipping { on_screen, incoming, queued }, Event::FlipDone) => {
            let mut effects = Vec::new();
            let on_screen = match incoming.video() {
                Some(token) => {
                    effects.push(Presented(token));
                    effects.extend(on_screen.map(Release));
                    Some(token)
                }
                None => on_screen,
            };
            if incoming.keys() {
                effects.push(KeysFlipped);
            }
            match queued {
                Some(next) => {
                    effects.push(Commit(next));
                    (Flipping { on_screen, incoming: next, queued: None }, effects)
                }
                None => (Steady { on_screen }, effects),
            }
        }
    })
}

fn queue(on_screen: Option<u64>, incoming: Update, queued: Option<Update>, change: Change) -> (Scanout, Vec<Effect>) {
    let (queued, dropped) = match queued {
        Some(waiting) => waiting.absorb(change),
        None => (change.into(), None),
    };
    (Scanout::Flipping { on_screen, incoming, queued: Some(queued) }, dropped.map(Effect::Drop).into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::Effect::{Commit, Drop, KeysFlipped, Presented, Release};
    use super::Scanout::{Flipping, Steady};
    use super::Update::{Both, Keys, Video};
    use super::*;

    fn flipping(on_screen: Option<u64>, incoming: Update, queued: Option<Update>) -> Scanout {
        Flipping { on_screen, incoming, queued }
    }

    #[test]
    fn walks_the_whole_transition_table() {
        let table: Vec<(Scanout, Event, Result<(Scanout, Vec<Effect>), FlipWithoutCommit>)> = vec![
            (Steady { on_screen: None }, Event::Submitted(1), Ok((flipping(None, Video(1), None), vec![Commit(Video(1))]))),
            (Steady { on_screen: Some(1) }, Event::Submitted(2), Ok((flipping(Some(1), Video(2), None), vec![Commit(Video(2))]))),
            (Steady { on_screen: Some(1) }, Event::KeysChanged, Ok((flipping(Some(1), Keys, None), vec![Commit(Keys)]))),
            (Steady { on_screen: None }, Event::FlipDone, Err(FlipWithoutCommit)),
            (Steady { on_screen: Some(1) }, Event::FlipDone, Err(FlipWithoutCommit)),
            (flipping(Some(1), Video(2), None), Event::Submitted(3), Ok((flipping(Some(1), Video(2), Some(Video(3))), vec![]))),
            (flipping(Some(1), Video(2), None), Event::KeysChanged, Ok((flipping(Some(1), Video(2), Some(Keys)), vec![]))),
            (
                flipping(Some(1), Video(2), Some(Video(3))),
                Event::Submitted(4),
                Ok((flipping(Some(1), Video(2), Some(Video(4))), vec![Drop(3)])),
            ),
            (flipping(Some(1), Video(2), Some(Video(3))), Event::KeysChanged, Ok((flipping(Some(1), Video(2), Some(Both(3))), vec![]))),
            (flipping(None, Keys, Some(Keys)), Event::Submitted(5), Ok((flipping(None, Keys, Some(Both(5))), vec![]))),
            (flipping(None, Keys, Some(Keys)), Event::KeysChanged, Ok((flipping(None, Keys, Some(Keys)), vec![]))),
            (flipping(None, Keys, Some(Both(5))), Event::Submitted(6), Ok((flipping(None, Keys, Some(Both(6))), vec![Drop(5)]))),
            (flipping(None, Keys, Some(Both(5))), Event::KeysChanged, Ok((flipping(None, Keys, Some(Both(5))), vec![]))),
            (flipping(None, Video(1), None), Event::FlipDone, Ok((Steady { on_screen: Some(1) }, vec![Presented(1)]))),
            (flipping(Some(1), Video(2), None), Event::FlipDone, Ok((Steady { on_screen: Some(2) }, vec![Presented(2), Release(1)]))),
            (
                flipping(Some(1), Video(2), Some(Video(3))),
                Event::FlipDone,
                Ok((flipping(Some(2), Video(3), None), vec![Presented(2), Release(1), Commit(Video(3))])),
            ),
            (flipping(Some(1), Keys, None), Event::FlipDone, Ok((Steady { on_screen: Some(1) }, vec![KeysFlipped]))),
            (
                flipping(Some(1), Both(2), Some(Keys)),
                Event::FlipDone,
                Ok((flipping(Some(2), Keys, None), vec![Presented(2), Release(1), KeysFlipped, Commit(Keys)])),
            ),
        ];
        for (from, event, expected) in table {
            assert_eq!(step(from, event), expected, "from {from:?} on {event:?}");
        }
    }
}
