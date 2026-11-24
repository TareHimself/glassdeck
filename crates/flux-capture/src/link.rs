use std::time::{Duration, Instant};

/// Models a serial link (USB 2.0 bulk) so localhost tests see realistic transfer times.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SimulatedLink {
    bytes_per_sec: u64,
    free_at: Instant,
}

impl SimulatedLink {
    pub fn new(bytes_per_sec: u64, now: Instant) -> Self {
        assert!(bytes_per_sec > 0, "link rate must be positive");
        Self { bytes_per_sec, free_at: now }
    }

    pub fn transfer(self, bytes: usize, now: Instant) -> (Self, Instant) {
        let start = self.free_at.max(now);
        let duration = Duration::from_secs_f64(bytes as f64 / self.bytes_per_sec as f64);
        let done = start + duration;
        (Self { free_at: done, ..self }, done)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MB: u64 = 1_000_000;

    #[test]
    fn idle_link_takes_size_over_rate() {
        let t0 = Instant::now();
        let (_, done) = SimulatedLink::new(10 * MB, t0).transfer(1_000_000, t0);
        assert_eq!(done - t0, Duration::from_millis(100));
    }

    #[test]
    fn busy_link_queues_behind_previous_transfer() {
        let t0 = Instant::now();
        let link = SimulatedLink::new(10 * MB, t0);
        let (link, _) = link.transfer(1_000_000, t0);
        let (_, done) = link.transfer(1_000_000, t0 + Duration::from_millis(50));
        assert_eq!(done - t0, Duration::from_millis(200));
    }

    #[test]
    fn link_idle_since_long_ago_starts_now() {
        let t0 = Instant::now();
        let later = t0 + Duration::from_secs(5);
        let (_, done) = SimulatedLink::new(10 * MB, t0).transfer(100_000, later);
        assert_eq!(done - later, Duration::from_millis(10));
    }
}
