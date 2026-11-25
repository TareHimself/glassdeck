/// What became of a picture handed to the compositor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// On screen from `at_us` (wall clock), the moment the display latched it
    Presented { token: u64, at_us: u64 },
    /// Replaced by a newer picture before it reached the screen
    #[cfg_attr(windows, allow(dead_code, reason = "the window presents every picture at once"))]
    Dropped { token: u64 },
}
