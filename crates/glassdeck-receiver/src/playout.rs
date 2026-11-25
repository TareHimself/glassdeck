use std::thread;
use std::time::Duration;

use glassdeck_proto::unix_micros;

/// Maps paced presentation times onto this machine's wall clock, `delay_us` behind the first picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlayoutClock {
    anchor_wall_us: u64,
    anchor_pts_us: u64,
}

impl PlayoutClock {
    pub fn start(first_pts_us: u64, now_us: u64, delay_us: u64) -> Self {
        Self { anchor_wall_us: now_us + delay_us, anchor_pts_us: first_pts_us }
    }

    pub fn due_us(&self, pts_us: u64) -> u64 {
        self.anchor_wall_us + pts_us.saturating_sub(self.anchor_pts_us)
    }
}

/// Sleeps until `due_us`; returns how late we already were if it had passed.
pub fn wait_until(due_us: u64) -> u64 {
    let now = unix_micros();
    match due_us.checked_sub(now) {
        Some(ahead) => {
            thread::sleep(Duration::from_micros(ahead));
            0
        }
        None => now - due_us,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAYOUT_DELAY_US: u64 = 500_000;

    #[test]
    fn first_frame_waits_for_the_playout_delay() {
        let clock = PlayoutClock::start(0, 1_000_000, PLAYOUT_DELAY_US);
        assert_eq!(clock.due_us(0), 1_000_000 + PLAYOUT_DELAY_US);
    }

    #[test]
    fn later_frames_keep_their_spacing() {
        let clock = PlayoutClock::start(500_000, 1_000_000, PLAYOUT_DELAY_US);
        assert_eq!(clock.due_us(533_333) - clock.due_us(500_000), 33_333);
    }

    #[test]
    fn frames_before_the_anchor_are_due_at_the_anchor() {
        let clock = PlayoutClock::start(500_000, 0, PLAYOUT_DELAY_US);
        assert_eq!(clock.due_us(400_000), clock.due_us(500_000));
    }
}
