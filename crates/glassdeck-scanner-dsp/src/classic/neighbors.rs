use super::{DEFAULT_ACTUATION, DEFAULT_HYSTERESIS, ISO, ROWS};
use crate::MAX_ROW_KEYS;

/// The resolved, position-scaled thresholds of a row's keys.
#[derive(Clone, Copy, Debug)]
pub struct Thresholds<'a> {
    pub actuation: &'a [u16],
    pub hysteresis: &'a [u16],
}

/// Neighboring-key correction for one physical row. Zeroed at boot.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Neighbors {
    pub below_release: [u8; MAX_ROW_KEYS],
}

/// Keys whose position makes them pick up more of their neighbors' field, by layout (ISO, other)
/// and row: bit `i` is key `i`.
const SENSITIVE: [[u16; ROWS]; 2] = [
    [0, 0x1000, 0x1002, 0x0802, 0x1402, 0x0028],
    [0, 0x1000, 0x1002, 0x1002, 0x2802, 0x0028],
];

impl Neighbors {
    /// Corrects a whole row before any key decision this scan. `field` has `keys + 1`
    /// conditioned sensor counts; `flux` has `keys` provisional travel values (switch-feel gain
    /// included) and is corrected in place. `down` is the previous scan's confirmed state.
    /// `thresholds` is `None` for a keymap that isn't loaded (unscaled defaults).
    pub fn correct(
        &mut self,
        layout: u8,
        row: usize,
        field: &[i32],
        flux: &mut [i32],
        down: &[bool],
        thresholds: Option<Thresholds<'_>>,
    ) {
        let keys = flux.len();
        if keys == 0 || keys > MAX_ROW_KEYS || row >= ROWS || !(1..=3).contains(&layout) {
            return;
        }
        reject_plateaus(field, flux, down);
        reject_spill(layout, row, field, flux, thresholds.map(|t| t.actuation));
        self.stabilize_held(field, flux, down, thresholds);
    }

    /// While a row still carries field, holds a down key with nonzero evidence through a short
    /// dip below release. At true zero it doesn't prop the key up.
    fn stabilize_held(
        &mut self,
        field: &[i32],
        flux: &mut [i32],
        down: &[bool],
        thresholds: Option<Thresholds<'_>>,
    ) {
        let keys = flux.len();
        let Some(first) = (0..keys).find(|&k| down[k]) else {
            return;
        };
        let total = field[..=keys]
            .iter()
            .fold(0u32, |t, f| t.wrapping_add(f.unsigned_abs()));
        if (total as i32) < 150 {
            return;
        }
        for k in first..keys {
            if !down[k] {
                continue;
            }
            if flux[k] == 0 {
                self.below_release[k] = 0;
                continue;
            }
            let act = thresholds.map_or(DEFAULT_ACTUATION, |t| t.actuation[k]) as i32;
            let depth = thresholds.map_or(DEFAULT_HYSTERESIS, |t| t.hysteresis[k]) as i32;
            let release = act - depth;
            if flux[k] < release {
                self.below_release[k] = self.below_release[k].saturating_add(1);
                if self.below_release[k] > 15 {
                    continue;
                }
            } else {
                self.below_release[k] = 0;
            }
            let floor = (release + if depth < 32 { 8 } else { depth / 4 }).min(act - 16);
            if flux[k] < floor {
                flux[k] = floor;
            }
        }
    }
}

/// Sensors measure the field at the boundaries between keys, so a run of identical travel is
/// ambiguous without a local drop in that field.
fn reject_plateaus(field: &[i32], flux: &mut [i32], down: &[bool]) {
    let keys = flux.len();
    let mut start = 0;
    while start < keys {
        let mut end = start + 1;
        while end < keys && flux[end] == flux[start] {
            end += 1;
        }
        let length = end - start;
        if length >= 3 || (length == 2 && end < keys) {
            let limit = if length == 2 { 560 } else { 800 };
            // The last key has no following key to compare its field with.
            for i in start..end.min(keys - 1) {
                let rise = (field[i + 1] as u32).wrapping_sub(field[i] as u32) as i32;
                if rise >= -79 && flux[i] < limit && !down[i] {
                    flux[i] = 0;
                }
            }
        }
        start = end;
    }
}

/// Cuts a center key's apparent travel that two active neighbors can explain. Uses a snapshot,
/// so correcting one center doesn't change the evidence for the next.
fn reject_spill(
    layout: u8,
    row: usize,
    field: &[i32],
    flux: &mut [i32],
    actuation: Option<&[u16]>,
) {
    let keys = flux.len();
    let mut before = [0i32; MAX_ROW_KEYS];
    before[..keys].copy_from_slice(flux);
    let threshold = |i: usize| actuation.map_or(DEFAULT_ACTUATION, |a| a[i]) as i32;
    for i in 1..keys.saturating_sub(1) {
        let mut f = before[i];
        if f <= 0 || before[i - 1] <= threshold(i - 1) || before[i + 1] <= threshold(i + 1) {
            continue;
        }
        let inside = field[i]
            .unsigned_abs()
            .wrapping_add(field[i + 1].unsigned_abs()) as i32;
        let outside = field[i - 1]
            .unsigned_abs()
            .wrapping_add(field[i + 2].unsigned_abs()) as i32;
        if outside <= inside {
            continue;
        }
        let strong = SENSITIVE[(layout == ISO) as usize][row] & (1 << i) != 0;
        let (knee, ceiling) = if strong { (1200, 1300) } else { (600, 1000) };
        if f >= ceiling {
            continue;
        }
        f = if f <= knee {
            f * f / (2 * knee)
        } else {
            f * (ceiling - knee + f - knee) / (2 * (ceiling - knee))
        };
        if strong && f < 500 {
            f = 2 * f - 500;
        }
        flux[i] = f;
    }
    for f in flux.iter_mut() {
        *f = (*f).max(0);
    }
}
