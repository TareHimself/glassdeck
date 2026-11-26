//! Row motion hints and rapid-trigger decisions for the refined path (Keyglass `fw_rapid.c`).

/// One key's movement between scans. Zeroed is at rest.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KeyMotion {
    pub velocity: i32,
    pub delta: i32,
    pub previous_flux: u16,
    /// Scans left in which a press is ignored because both neighbors are moving down.
    pub inhibit_press: u8,
    /// Scans left in which a release is ignored because both neighbors are moving up.
    pub inhibit_release: u8,
}

const GUARD_SCANS: u8 = 30;

fn guard_window(left: u8, renew: bool) -> u8 {
    if renew {
        GUARD_SCANS
    } else {
        left.saturating_sub(1)
    }
}

/// Updates a whole row (every key, not only rapid ones) with this scan's unguarded signals,
/// before any key in it is evaluated.
pub fn update_row_motion(row: &mut [KeyMotion], flux: &[u16]) {
    let n = flux.len();
    for (m, &f) in row.iter_mut().zip(flux) {
        m.delta = f as i32 - m.previous_flux as i32;
        m.previous_flux = f;
        m.velocity += m.delta - (m.velocity >> 3);
    }
    for i in 0..n {
        let l = if i > 0 { row[i - 1].velocity } else { 0 };
        let r = if i + 1 < n { row[i + 1].velocity } else { 0 };
        let m = &mut row[i];
        let v = m.velocity;
        // Similar movement on both sides can disturb a key between them. Strong independent
        // movement overrides a newly raised guard before it can mask a real stroke.
        let between = 2 * l.abs() > v.abs() && 2 * r.abs() > v.abs();
        m.inhibit_press = guard_window(m.inhibit_press, between && l < -40 && r < -40);
        m.inhibit_release = guard_window(m.inhibit_release, between && l > 40 && r > 40);
        let largest = l.abs().max(r.abs());
        if v > 40 && v > 2 * largest && m.inhibit_press > 25 {
            m.inhibit_press = 0;
        }
        if v < -40 && -v > 2 * largest && m.inhibit_release > 25 {
            m.inhibit_release = 0;
        }
    }
}

/// The size of the larger immediate neighbor's change since the previous scan.
pub fn neighbor_delta(row: &[KeyMotion], col: usize) -> i32 {
    let l = if col > 0 { row[col - 1].delta.abs() } else { 0 };
    let r = row.get(col + 1).map_or(0, |m| m.delta.abs());
    l.max(r)
}

/// One rapid-trigger key's decision state. Zeroed is at rest.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RapidState {
    pub last_independent_step: i32,
    pub peak: u16,
    pub trough: u16,
    pub independent_scans: u8,
    pub neighbor_scans: u8,
    pub reversing: bool,
    pub pending_down: bool,
    pub pending_scans: u8,
}

#[derive(Clone, Copy, Debug)]
pub struct RapidInput<'a> {
    /// The decision signal, after the guards in [`crate::guard`].
    pub flux: u16,
    /// Position-weighted first press threshold.
    pub actuation: u16,
    /// Position-weighted distance from actuation to reset.
    pub hysteresis: u16,
    /// Unweighted rise and fall distance for partial re-triggers.
    pub sensitivity: u16,
    /// Position gain, or 0 for unity.
    pub weight_q8: u16,
    /// Keys in this row held, or pending a fixed-mode press, before this scan.
    pub row_active: u8,
    pub motion: &'a KeyMotion,
    pub neighbor_delta: i32,
}

/// Travel at which a key counts as bottomed out, before position gain.
const BOTTOM: i32 = 1162;
const MIN_REARM: i32 = 120;
const INDEPENDENT_SCANS: u8 = 24;
const NEIGHBOR_SCANS: u8 = 2;

impl RapidState {
    /// Clears travel and pending state, keeping motion history, after a keymap or mode load.
    pub fn rearm(&mut self) {
        self.peak = 0;
        self.trough = 0;
        self.reversing = false;
        self.pending_down = false;
        self.pending_scans = 0;
    }

    fn track_independent_motion(&mut self, step: i32, neighbor: i32) {
        let independent = step > 20 && step > 2 * neighbor;
        let previous = self.last_independent_step;
        if previous > 40 && 4 * (previous + step).abs() < previous {
            self.independent_scans = 0;
        } else if independent {
            self.independent_scans = INDEPENDENT_SCANS;
        } else {
            self.independent_scans = self.independent_scans.saturating_sub(1);
        }
        self.last_independent_step = if independent { step } else { 0 };
    }

    fn neighbor_moving(&mut self, neighbor: i32) -> bool {
        if neighbor > 20 {
            self.neighbor_scans = NEIGHBOR_SCANS;
            return true;
        }
        if self.neighbor_scans == 0 {
            return false;
        }
        self.neighbor_scans -= 1;
        true
    }

    /// Advances one scan and returns whether the key is down.
    pub fn step(&mut self, down: bool, input: &RapidInput<'_>) -> bool {
        let (f, a) = (input.flux as i32, input.actuation as i32);
        let rearm = (a - input.hysteresis as i32).max(MIN_REARM);
        let delta = input.motion.delta;
        self.track_independent_motion(if down { -delta } else { delta }, input.neighbor_delta);
        if (!down && input.motion.inhibit_press != 0)
            || (down && input.motion.inhibit_release != 0 && f >= rearm)
        {
            return down;
        }

        let sensitivity = input.sensitivity as i32;
        let reversal = if input.row_active > 1 && self.independent_scans == 0 {
            2 * sensitivity
        } else {
            sensitivity
        };
        let (mut want, mut first, mut bottom_release) = (down, false, false);
        if down {
            if f > self.peak as i32 {
                self.peak = f as u16;
            }
            want = f >= self.peak as i32 - reversal && (f > a || f >= rearm);
            // Above bottom travel the peak is capped. A release crossing back through that cap
            // uses the nominal sensitivity even when other keys in the row are held.
            let weight = if input.weight_q8 != 0 {
                input.weight_q8 as i32
            } else {
                256
            };
            let bottom = (BOTTOM * weight) >> 8;
            if f >= bottom {
                self.peak = bottom as u16;
                want = true;
            } else if self.peak as i32 == bottom && f < bottom - sensitivity {
                bottom_release = true;
                want = false;
            }
        } else if !self.reversing && f > a {
            self.reversing = true;
            self.trough = f as u16;
            want = true;
            first = true;
        } else {
            if self.reversing {
                if f < self.trough as i32 {
                    self.trough = f as u16;
                }
                want = f > self.trough as i32 + reversal && (f > a || f >= rearm);
            }
            if f <= a && f < rearm {
                self.reversing = false;
                self.peak = 0;
                self.trough = 0;
            }
        }

        if !first
            && !(bottom_release && self.independent_scans != 0)
            && f >= rearm
            && self.neighbor_moving(input.neighbor_delta)
        {
            want = down;
            if input.neighbor_delta > 3 * delta.abs() {
                self.peak = f as u16;
                self.trough = f as u16;
            }
        }
        if want == down {
            self.pending_down = down;
            self.pending_scans = 0;
            return down;
        }
        if !first {
            if self.pending_down != want {
                self.pending_down = want;
                self.pending_scans = 1;
                return down;
            }
            if self.pending_scans < if bottom_release { 2 } else { 6 } {
                self.pending_scans += 1;
                return down;
            }
        }
        if want {
            self.peak = f as u16;
        } else {
            self.trough = f as u16;
        }
        want
    }
}
