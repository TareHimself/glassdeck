/// One probe's three timestamps: sent and returned on our clock, received on the receiver's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClockSample {
    pub sent_us: u64,
    pub received_us: u64,
    pub returned_us: u64,
}

impl ClockSample {
    pub fn round_trip_us(&self) -> u64 {
        self.returned_us.saturating_sub(self.sent_us)
    }

    /// Receiver clock minus ours, assuming the probe took as long out as back.
    pub fn offset_us(&self) -> i64 {
        let midpoint = self.sent_us + self.round_trip_us() / 2;
        self.received_us as i64 - midpoint as i64
    }
}

/// The probe with the shortest round trip, whose offset has the least room for asymmetric delay.
pub fn best_sample(samples: &[ClockSample]) -> Option<ClockSample> {
    samples.iter().copied().min_by_key(ClockSample::round_trip_us)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symmetric_trip_gives_the_exact_offset() {
        let sample = ClockSample { sent_us: 1_000, received_us: 6_000, returned_us: 3_000 };
        assert_eq!((sample.round_trip_us(), sample.offset_us()), (2_000, 4_000));
    }

    #[test]
    fn receiver_behind_gives_a_negative_offset() {
        let sample = ClockSample { sent_us: 10_000, received_us: 4_000, returned_us: 10_200 };
        assert_eq!(sample.offset_us(), -6_100);
    }

    #[test]
    fn picks_the_fastest_probe() {
        let slow = ClockSample { sent_us: 0, received_us: 900, returned_us: 1_000 };
        let fast = ClockSample { sent_us: 0, received_us: 150, returned_us: 200 };
        assert_eq!(best_sample(&[slow, fast]), Some(fast));
        assert_eq!(best_sample(&[]), None);
    }
}
