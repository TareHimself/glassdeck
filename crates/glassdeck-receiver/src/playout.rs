/// How far behind the first frame's arrival paced playback starts, to absorb network jitter.
pub const PLAYOUT_DELAY_US: u64 = 150_000;

/// Maps a paced stream's presentation times onto this machine's wall clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlayoutClock {
    anchor_wall_us: u64,
    anchor_pts_us: u64,
}

impl PlayoutClock {
    pub fn start(first_pts_us: u64, now_us: u64) -> Self {
        Self { anchor_wall_us: now_us + PLAYOUT_DELAY_US, anchor_pts_us: first_pts_us }
    }

    pub fn due_us(&self, pts_us: u64) -> u64 {
        self.anchor_wall_us + pts_us.saturating_sub(self.anchor_pts_us)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_frame_waits_for_the_playout_delay() {
        let clock = PlayoutClock::start(0, 1_000_000);
        assert_eq!(clock.due_us(0), 1_000_000 + PLAYOUT_DELAY_US);
    }

    #[test]
    fn later_frames_keep_their_spacing() {
        let clock = PlayoutClock::start(500_000, 1_000_000);
        assert_eq!(clock.due_us(533_333) - clock.due_us(500_000), 33_333);
    }

    #[test]
    fn frames_before_the_anchor_are_due_at_the_anchor() {
        let clock = PlayoutClock::start(500_000, 0);
        assert_eq!(clock.due_us(400_000), clock.due_us(500_000));
    }
}
