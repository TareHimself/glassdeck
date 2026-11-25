//! Which decoded picture is on screen, which is flipping in at the next vblank, and which waits.
//! Pictures are named by token; the compositor carries out the effects.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scanout {
    /// No commit in flight. `on_screen` stays until a newer picture replaces it.
    Steady { on_screen: Option<u64> },
    /// A commit waits for vblank, and at most one newer picture waits behind it.
    Flipping { on_screen: Option<u64>, incoming: u64, queued: Option<u64> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    Submitted(u64),
    FlipDone,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    /// Put this picture on the video plane with a non-blocking commit
    Commit(u64),
    /// This picture reached the screen
    Presented(u64),
    /// This picture left the screen; free its framebuffer and decoder buffer
    Release(u64),
    /// A newer picture replaced this one before it was ever committed
    Drop(u64),
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
    use Effect::{Commit, Drop, Presented, Release};
    use Scanout::{Flipping, Steady};
    Ok(match (state, event) {
        (Steady { on_screen }, Event::Submitted(token)) => {
            (Flipping { on_screen, incoming: token, queued: None }, vec![Commit(token)])
        }
        (Flipping { on_screen, incoming, queued }, Event::Submitted(token)) => {
            let dropped = queued.map(Drop);
            (Flipping { on_screen, incoming, queued: Some(token) }, dropped.into_iter().collect())
        }
        (Flipping { on_screen, incoming, queued }, Event::FlipDone) => {
            let mut effects = vec![Presented(incoming)];
            effects.extend(on_screen.map(Release));
            match queued {
                Some(next) => {
                    effects.push(Commit(next));
                    (Flipping { on_screen: Some(incoming), incoming: next, queued: None }, effects)
                }
                None => (Steady { on_screen: Some(incoming) }, effects),
            }
        }
        (Steady { .. }, Event::FlipDone) => return Err(FlipWithoutCommit),
    })
}

#[cfg(test)]
mod tests {
    use super::Effect::{Commit, Drop, Presented, Release};
    use super::Scanout::{Flipping, Steady};
    use super::*;

    #[test]
    fn walks_the_whole_transition_table() {
        let table: [(Scanout, Event, Result<(Scanout, Vec<Effect>), FlipWithoutCommit>); 9] = [
            (Steady { on_screen: None }, Event::Submitted(1), Ok((Flipping { on_screen: None, incoming: 1, queued: None }, vec![Commit(1)]))),
            (Steady { on_screen: Some(1) }, Event::Submitted(2), Ok((Flipping { on_screen: Some(1), incoming: 2, queued: None }, vec![Commit(2)]))),
            (Steady { on_screen: None }, Event::FlipDone, Err(FlipWithoutCommit)),
            (Steady { on_screen: Some(1) }, Event::FlipDone, Err(FlipWithoutCommit)),
            (
                Flipping { on_screen: Some(1), incoming: 2, queued: None },
                Event::Submitted(3),
                Ok((Flipping { on_screen: Some(1), incoming: 2, queued: Some(3) }, vec![])),
            ),
            (
                Flipping { on_screen: Some(1), incoming: 2, queued: Some(3) },
                Event::Submitted(4),
                Ok((Flipping { on_screen: Some(1), incoming: 2, queued: Some(4) }, vec![Drop(3)])),
            ),
            (Flipping { on_screen: None, incoming: 1, queued: None }, Event::FlipDone, Ok((Steady { on_screen: Some(1) }, vec![Presented(1)]))),
            (
                Flipping { on_screen: Some(1), incoming: 2, queued: None },
                Event::FlipDone,
                Ok((Steady { on_screen: Some(2) }, vec![Presented(2), Release(1)])),
            ),
            (
                Flipping { on_screen: Some(1), incoming: 2, queued: Some(3) },
                Event::FlipDone,
                Ok((Flipping { on_screen: Some(2), incoming: 3, queued: None }, vec![Presented(2), Release(1), Commit(3)])),
            ),
        ];
        for (from, event, expected) in table {
            assert_eq!(step(from, event), expected, "from {from:?} on {event:?}");
        }
    }
}
