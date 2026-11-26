use super::ROWS;
use crate::MAX_ROW_SENSORS;

/// Shrinks toward zero without changing sign.
fn deadband(value: i32, band: i32) -> i32 {
    if value > band {
        value - band
    } else if value < -band {
        value + band
    } else {
        0
    }
}

const BALANCE_LIMIT: i32 = 180;
const ACTIVE: i32 = 20;

/// Removes leftover common field, mostly from the active sensors. The first correction is
/// proportional to field strength, at most 180 counts a sensor; what's left is split equally
/// among the sensors that didn't hit that limit.
fn balance(field: &mut [i32]) {
    let (mut total, mut weight) = (0i32, 0i32);
    for &f in field.iter() {
        total += f;
        if f.abs() > ACTIVE {
            weight += f.abs();
        }
    }
    if total == 0 || weight == 0 {
        return;
    }
    let (mut remainder, mut eligible, mut eligible_mask) = (total, 0i32, 0u32);
    for (i, f) in field.iter_mut().enumerate() {
        let magnitude = f.abs();
        if magnitude <= ACTIVE {
            continue;
        }
        // Keyglass multiplies in u32 so an overflowing product wraps the same way everywhere.
        let product = (total as u32).wrapping_mul(magnitude as u32) as i32;
        let mut share = product / weight;
        if share >= BALANCE_LIMIT {
            share = BALANCE_LIMIT;
        } else if share <= -BALANCE_LIMIT {
            share = -BALANCE_LIMIT;
        } else {
            eligible_mask |= 1 << i;
            eligible += 1;
        }
        remainder -= share;
        *f -= share;
    }
    if eligible != 0 && (remainder >= eligible || remainder <= -eligible) {
        let share = remainder / eligible;
        for (i, f) in field.iter_mut().enumerate() {
            if eligible_mask & (1 << i) != 0 {
                *f -= share;
            }
        }
    }
}

/// Turns one row's sensors into travel estimates between them, with no row history. `field`
/// holds the signed calibrated deltas, left to right (2 to 17 of them), and is conditioned in
/// place; `flux` gets one travel value per key (`field.len() - 1`), before switch-feel gain and
/// neighbor correction.
pub fn condition_row(row: usize, field: &mut [i32], flux: &mut [i32]) {
    let width = field.len();
    if row >= ROWS || !(2..=MAX_ROW_SENSORS).contains(&width) {
        return;
    }
    let (mut sum, mut tilt, mut silent) = (0i32, 0i32, 0u32);
    for (i, &s) in field.iter().enumerate() {
        sum += s;
        silent += (s == 0) as u32;
        // An odd-width row's middle sensor is left out.
        if i < width / 2 {
            tilt += s;
        } else if i >= width.div_ceil(2) {
            tilt -= s;
        }
    }
    let offset = if silent <= 3 && !(-100..=100).contains(&sum) {
        sum / width as i32
    } else {
        0
    };
    let band = if tilt > 100 { 100 - 20 * row as i32 } else { 0 };
    for f in field.iter_mut() {
        *f = deadband(*f - offset, band);
    }
    balance(field);

    let (mut first, mut last) = (width, 0);
    sum = 0;
    for (i, f) in field.iter_mut().enumerate() {
        *f = deadband(*f, 50);
        sum += *f;
        if *f != 0 {
            first = first.min(i);
            last = i;
        }
    }
    // A key sees the field to its left minus the field to its right. Outside the occupied span,
    // that difference is only row imbalance.
    let mut left = 0;
    for i in 0..width - 1 {
        left += field[i];
        flux[i] = if i >= first && i < last {
            2 * left - sum
        } else {
            0
        };
    }
}
