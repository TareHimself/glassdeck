use super::{DEFAULT_ACTUATION, DEFAULT_HYSTERESIS, ISO, ROWS, SPACE_ROW};
use crate::MAX_ROW_KEYS;

/// One row's resolved key settings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Settings {
    /// Position-scaled depths, which neighbor correction also reads.
    pub actuation: [u16; MAX_ROW_KEYS],
    pub hysteresis: [u16; MAX_ROW_KEYS],
    /// Unscaled rapid-trigger reversal distance.
    pub sensitivity: [u16; MAX_ROW_KEYS],
    pub rapid: [bool; MAX_ROW_KEYS],
}

/// A rapid key's travel since its last committed edge.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Reversal {
    pub peak: i32,
    pub trough: i32,
    pub armed: bool,
    pub proposed: bool,
    pub scans: u8,
}

const ARMED: Reversal = Reversal {
    peak: 0,
    trough: 0,
    armed: true,
    proposed: false,
    scans: 0,
};

/// One row's key decisions. Zeroed is at rest.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Keys {
    /// Confirmed state.
    pub down: [bool; MAX_ROW_KEYS],
    /// Proposed state; rapid keys only publish committed edges.
    pub pending: [bool; MAX_ROW_KEYS],
    pub scans: [u8; MAX_ROW_KEYS],
    pub reversal: [Reversal; MAX_ROW_KEYS],
}

const ROW_GAIN: [u16; ROWS] = [256, 253, 250, 246, 242, 234];
/// Keys whose wide cap puts the switch off center, by layout (ISO, other) and row.
const WIDE: [[u16; ROWS]; 2] = [
    [0, 1 << 13, 1 | (1 << 13), 1 | (1 << 12), 1 | (1 << 11), 1],
    [0, 1 << 13, 1 | (1 << 13), 1 | (1 << 13), 1 | (1 << 12), 1],
];
/// Rapid-trigger travel above which a key stays down whatever it reverses by.
const DEEP: i32 = 820;
const MIN_RESET: i32 = 120;
/// Consecutive proposals before a rapid key's partial release or re-press commits.
const RAPID_CONFIRM: u8 = 4;

impl Settings {
    /// Applies the row gain, then the extra gain for off-center wide keys, to thresholds that are
    /// already resolved. Leaves key state alone.
    pub fn weight(&mut self, layout: u8, row: usize, col: usize) {
        if !(1..=3).contains(&layout) || row >= ROWS || col >= MAX_ROW_KEYS {
            return;
        }
        let mut gain = ROW_GAIN[row] as u32;
        if WIDE[(layout == ISO) as usize][row] & (1 << col) != 0 {
            gain = (gain * 224) >> 8;
        }
        self.actuation[col] = ((self.actuation[col] as u32 * gain) >> 8) as u16;
        self.hysteresis[col] = ((self.hysteresis[col] as u32 * gain) >> 8) as u16;
    }
}

impl Keys {
    /// Decodes one key's actuation word on a layout, keymap or mode load, even a zero word.
    /// `has_table` is false when the keymap has no section for the layout (unscaled defaults).
    /// A mode change clears reversal history but keeps `down`. Fixed loads also cancel a
    /// pending fixed confirmation; rapid loads keep it until a rapid edge.
    pub fn load(
        &mut self,
        settings: &mut Settings,
        layout: u8,
        row: usize,
        col: usize,
        has_table: bool,
        word: u32,
    ) {
        if row >= ROWS || col >= MAX_ROW_KEYS {
            return;
        }
        let kind = if has_table { word >> 30 } else { 0 };
        let specified = kind == 1 || kind == 2;
        settings.actuation[col] = if specified {
            (word & 0xFFF) as u16
        } else {
            DEFAULT_ACTUATION
        };
        settings.hysteresis[col] = if specified {
            ((word >> 12) & 0xFFF) as u16
        } else {
            DEFAULT_HYSTERESIS
        };
        settings.rapid[col] = kind == 2;
        self.reversal[col] = ARMED;
        if kind == 2 {
            // Reversals use unweighted travel. Switching to rapid doesn't cancel a fixed
            // proposal; it still counts toward row occupancy until a rapid edge commits.
            settings.sensitivity[col] = settings.hysteresis[col];
        } else {
            self.pending[col] = self.down[col];
            self.scans[col] = 0;
        }
        if has_table {
            settings.weight(layout, row, col);
        }
    }

    /// Advances one scan, after the whole row's corrected flux is known. Every row must get the
    /// held count from before this scan.
    pub fn step(&mut self, settings: &Settings, row: usize, held_before: usize, flux: &[i32]) {
        let n = flux.len();
        if row >= ROWS || n > MAX_ROW_KEYS {
            return;
        }
        let occupied = (0..n).filter(|&i| self.down[i] || self.pending[i]).count();
        for (i, &f) in flux.iter().enumerate() {
            let (a, h) = (settings.actuation[i] as i32, settings.hysteresis[i] as i32);
            let press = press_threshold(a, row, occupied);
            if settings.rapid[i] {
                let next = self.reversal[i].step(
                    self.down[i],
                    f,
                    press,
                    a,
                    h,
                    settings.sensitivity[i] as i32,
                );
                if next != self.down[i] {
                    self.down[i] = next;
                    self.pending[i] = next;
                    self.scans[i] = 0;
                }
            } else {
                let next = f > if self.down[i] { a - h } else { press };
                let duration = if held_before > 3 { 16 } else { 2 };
                if next == self.down[i] {
                    self.pending[i] = next;
                    self.scans[i] = 0;
                } else if next != self.pending[i] {
                    self.pending[i] = next;
                    self.scans[i] = 1;
                } else if self.scans[i] < duration {
                    self.scans[i] += 1;
                } else {
                    self.down[i] = next;
                }
            }
        }
    }
}

/// A busier row needs more travel to press.
fn press_threshold(a: i32, row: usize, occupied: usize) -> i32 {
    let (extra, cap) = match (row == SPACE_ROW, occupied) {
        (true, 2..) => (400, 1000),
        (false, 8..) => (1000, 1200),
        (false, 4..) => (400, 800),
        _ => return a,
    };
    (a + extra).min(cap)
}

impl Reversal {
    /// An armed first press is immediate. A partial release or re-press needs five proposals in a
    /// row. Very deep in its travel a key stays down whatever small reversal it makes.
    fn step(&mut self, down: bool, f: i32, press: i32, a: i32, h: i32, sensitivity: i32) -> bool {
        let reset = (a - h).max(MIN_RESET);
        let (mut proposed, mut immediate) = (down, false);
        if f > DEEP {
            proposed = true;
            if !down && self.armed {
                self.armed = false;
                self.trough = f;
                immediate = true;
            }
            self.peak = self.peak.max(f);
        } else if down {
            self.peak = self.peak.max(f);
            if f < (self.peak as u32).wrapping_sub(sensitivity as u32) as i32
                || (f <= a && f < reset)
            {
                proposed = false;
            }
        } else if self.armed && f > press {
            self.armed = false;
            self.trough = f;
            proposed = true;
            immediate = true;
        } else {
            if !self.armed {
                self.trough = self.trough.min(f);
                proposed = f > (self.trough as u32).wrapping_add(sensitivity as u32) as i32;
            }
            if f <= press && f < reset {
                proposed = false;
                self.armed = true;
                self.trough = 0;
                self.peak = 0;
            }
        }

        if proposed == down {
            self.proposed = down;
            self.scans = 0;
        } else if immediate {
            self.peak = f;
            return true;
        } else if self.proposed != proposed {
            self.proposed = proposed;
            self.scans = 1;
        } else if self.scans < RAPID_CONFIRM {
            self.scans += 1;
        } else {
            if proposed {
                self.peak = f;
            } else {
                self.trough = f;
            }
            return proposed;
        }
        down
    }
}
