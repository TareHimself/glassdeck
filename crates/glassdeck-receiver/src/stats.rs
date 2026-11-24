#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameTimes {
    pub capture_us: u64,
    pub received_us: u64,
    pub decoded_us: u64,
    pub displayed_us: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LatencyStats {
    frames: u32,
    replaced: u32,
    total_us: u64,
    max_total_us: u64,
    send_us: u64,
    decode_us: u64,
    display_us: u64,
    paced_frames: u32,
    late_frames: u32,
    max_late_us: u64,
}

/// A paced frame shown more than this after its due time counts as late (about a third of a 60 Hz frame).
const LATE_THRESHOLD_US: u64 = 5_000;

impl LatencyStats {
    pub fn record_paced(self, late_us: u64) -> Self {
        Self {
            paced_frames: self.paced_frames + 1,
            late_frames: self.late_frames + (late_us > LATE_THRESHOLD_US) as u32,
            max_late_us: self.max_late_us.max(late_us),
            ..self
        }
    }

    pub fn record(self, t: FrameTimes) -> Self {
        let total = t.displayed_us.saturating_sub(t.capture_us);
        Self {
            frames: self.frames + 1,
            total_us: self.total_us + total,
            max_total_us: self.max_total_us.max(total),
            send_us: self.send_us + t.received_us.saturating_sub(t.capture_us),
            decode_us: self.decode_us + t.decoded_us.saturating_sub(t.received_us),
            display_us: self.display_us + t.displayed_us.saturating_sub(t.decoded_us),
            ..self
        }
    }

    pub fn replaced(self, count: u32) -> Self {
        Self { replaced: self.replaced + count, ..self }
    }

    pub fn summary(&self, secs: f64) -> Option<String> {
        let avg = |sum: u64| sum as f64 / self.frames.max(1) as f64 / 1000.0;
        let mut parts = Vec::new();
        if self.frames > 0 {
            parts.push(format!(
                "live {:5.1} fps | capture->screen avg {:5.1} ms, max {:5.1} | encode+send {:4.1} | decode {:4.1} | display {:4.1}",
                self.frames as f64 / secs,
                avg(self.total_us),
                self.max_total_us as f64 / 1000.0,
                avg(self.send_us),
                avg(self.decode_us),
                avg(self.display_us),
            ));
        }
        if self.paced_frames > 0 {
            parts.push(format!(
                "paced {:5.1} fps | late {} (worst {:.1} ms)",
                self.paced_frames as f64 / secs,
                self.late_frames,
                self.max_late_us as f64 / 1000.0,
            ));
        }
        if parts.is_empty() {
            return None;
        }
        parts.push(format!("skipped {}", self.replaced));
        Some(parts.join(" | "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_latency_into_stages() {
        let t = FrameTimes { capture_us: 1_000, received_us: 4_000, decoded_us: 9_000, displayed_us: 11_000 };
        let stats = LatencyStats::default().record(t).record(t);
        assert_eq!(stats.frames, 2);
        assert_eq!(stats.total_us, 20_000);
        assert_eq!(stats.max_total_us, 10_000);
        assert_eq!((stats.send_us, stats.decode_us, stats.display_us), (6_000, 10_000, 4_000));
    }

    #[test]
    fn counts_only_frames_past_the_threshold_as_late() {
        let stats = LatencyStats::default().record_paced(0).record_paced(LATE_THRESHOLD_US + 1);
        assert_eq!((stats.paced_frames, stats.late_frames), (2, 1));
    }

    #[test]
    fn no_frames_means_no_summary() {
        assert_eq!(LatencyStats::default().summary(1.0), None);
    }
}
