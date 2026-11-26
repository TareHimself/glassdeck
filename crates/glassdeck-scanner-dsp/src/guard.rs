//! Guards on the refined path's decision signal (Keyglass `fw_press_guard.h`).

use crate::rapid::KeyMotion;

/// Applies the temporary calibration guards to the decision signal only; row motion keeps the
/// original flux. A key held through a guarded interval keeps its release protection until it's up.
#[allow(clippy::too_many_arguments)]
pub fn guard_key_signal(
    flux: u16,
    weight_q8: u16,
    down: bool,
    rapid: bool,
    peak: u16,
    require_strong_signal: bool,
    protected_held: &mut u8,
    mask_presses: bool,
) -> u16 {
    let weight = if weight_q8 != 0 {
        weight_q8 as u32
    } else {
        256
    };
    let flux32 = flux as u32;
    if !down {
        *protected_held = 0;
        if mask_presses || (require_strong_signal && flux32 < (1000 * weight) >> 8) {
            return 0;
        }
    } else if require_strong_signal || *protected_held != 0 {
        if flux32 < (600 * weight) >> 8 {
            return 0;
        }
        if rapid && peak > flux {
            return peak;
        }
    }
    flux
}

/// The most press samples, disturbed by a neighbor's motion, that one above-threshold excursion
/// may ignore.
const MAX_NEIGHBOR_VETOES: u8 = 6;

/// A fixed-mode key's candidate state (not yet committed). A sustained spatial guard can still
/// veto presses after the neighbor vetoes run out.
pub fn fixed_candidate(
    down: bool,
    flux: i32,
    actuation: i32,
    release: i32,
    neighbor_delta: i32,
    motion: &KeyMotion,
    neighbor_vetoes: &mut u8,
) -> bool {
    if down {
        return flux > release || (motion.inhibit_release != 0 && 2 * flux > release);
    }
    if flux <= actuation {
        *neighbor_vetoes = 0;
        return false;
    }
    if *neighbor_vetoes < MAX_NEIGHBOR_VETOES && neighbor_delta > 15 {
        *neighbor_vetoes += 1;
        return false;
    }
    motion.inhibit_press == 0
}
