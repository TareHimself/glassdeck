//! Per-key depth from a row's sensor residuals on the refined path (Keyglass `fw_depth.c`).
//!
//! Each key is modelled as two magnetic lobes: `left` raises the sensor before it and `right`
//! lowers the sensor after it. The lobes are fitted to the measured field every scan, then
//! cleaned up with what the row looked like at rest.

use crate::{MAX_ROW_KEYS, MAX_ROW_SENSORS};

/// History kept between scans. All `i32` values except `extra_depth` are Q4.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DepthHistory {
    pub skipped_for: [u8; MAX_ROW_KEYS],
    /// Whole signal units.
    pub prev_field: [i16; MAX_ROW_SENSORS],
    /// Whole signal units at sensor offsets -1 to +2 around each key.
    pub rest_window: [[i16; 4]; MAX_ROW_KEYS],
    pub stable_for: [u8; MAX_ROW_KEYS],
    pub window_peak: [i32; MAX_ROW_KEYS],
    pub previous_sum: [i32; MAX_ROW_KEYS],
    /// Previous minus new lobe sum.
    pub last_drop: [i32; MAX_ROW_KEYS],
    pub group_peak: [i32; MAX_ROW_KEYS],
    /// Whole signal units: the peak estimate kept from earlier scans.
    pub extra_depth: [i32; MAX_ROW_KEYS],
}

/// One physical row's depth estimator. Zeroed at boot and on a layout change; kept across
/// frames, idle ones included.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DepthRow {
    pub left: [i32; MAX_ROW_KEYS],
    pub right: [i32; MAX_ROW_KEYS],
    pub history: DepthHistory,
}

/// A sensor residual beyond this (Q4) marks a strong edge.
const STRONG: i32 = 1600;
/// Lobe sum (Q4) from which a key counts as pressed.
const PRESSED: i32 = 1280;
const QUIET: i32 = 480;

impl DepthRow {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// `q4` has `keys + 1` sensor residuals of the row-line fit (16 units a count); writes
    /// `keys` depth estimates in whole counts to `depth`.
    pub fn update(&mut self, q4: &[i32], depth: &mut [i32]) {
        let keys = depth.len();
        if keys == 0 || keys > MAX_ROW_KEYS {
            return;
        }
        let mut previous = [0i32; MAX_ROW_KEYS];
        let mut warm = false;
        for k in 0..keys {
            previous[k] = self.left[k] + self.right[k];
            warm |= previous[k] != 0;
        }
        let region = candidates(keys, q4, &previous);
        let fitted = warm || region != 0;
        if fitted {
            self.fit_lobes(keys, q4);
        }
        postfit(
            keys,
            q4,
            region,
            fitted,
            &mut self.left,
            &mut self.right,
            &mut self.history,
            depth,
        );
    }

    /// Shares residuals between neighbors, then repeats with the updated field estimate.
    fn fit_lobes(&mut self, keys: usize, q: &[i32]) {
        let (mut err, mut score) = ([0i32; MAX_ROW_SENSORS], [0i32; MAX_ROW_KEYS]);
        let h = &self.history;
        for _pass in 0..3 {
            residual(keys, q, &self.left, &self.right, &mut err);
            for k in 0..keys {
                let veto = ambiguous(
                    k,
                    keys,
                    &err,
                    &self.left,
                    &self.right,
                    &h.last_drop,
                    &h.extra_depth,
                    &h.group_peak,
                );
                score[k] = depth_score(
                    k,
                    keys,
                    q,
                    &err,
                    &self.left,
                    &self.right,
                    &h.rest_window[k],
                    veto,
                );
            }
            update_lobes(keys, &err, &score, &mut self.left, &mut self.right);
        }
    }
}

/// A localized positive field has two opposite lobes. Compares the uncompensated field with the
/// area under the positive, centered integral of this run of keys.
fn balanced(q: &[i32], lo: usize, hi: usize) -> bool {
    let sum: i32 = q[lo..=hi + 1].iter().sum();
    let center = sum / 2;
    let (mut integral, mut area) = (0i32, 0u32);
    for &v in &q[lo..=hi] {
        integral += v;
        if integral > center {
            area = area.wrapping_add((integral - center) as u32);
        }
    }
    // Floored and compared as i32 like Keyglass's other accumulators, so a sum above i32::MAX
    // takes the floor of 16.
    if (area as i32) < 16 {
        area = 16;
    }
    (100u32.wrapping_mul(center.unsigned_abs()) as i32) <= (35u32.wrapping_mul(area) as i32)
}

/// A strong edge can implicate several keys. The gap to the next strong run is bridged only
/// when there's motion in it, unless the gap is quiet and both sides explain their own fields.
fn candidates(keys: usize, q: &[i32], prior: &[i32]) -> u32 {
    let mut mask = 0u32;
    let (mut prior_lo, mut prior_hi, mut first) = (0usize, 0usize, true);
    let strong = |i: usize| q[i] > STRONG || q[i + 1] < -STRONG;
    let mut i = 0;
    while i < keys {
        if !strong(i) {
            i += 1;
            continue;
        }
        let lo = i;
        i += 1;
        while i < keys && strong(i) {
            i += 1;
        }
        let hi = i - 1;
        let mut merge = false;
        if !first {
            let mut quiet = true;
            for j in prior_hi + 1..lo {
                if prior[j] >= PRESSED {
                    merge = true;
                }
                if q[j + 1].abs() >= 640 {
                    quiet = false;
                }
            }
            if merge && quiet && balanced(q, prior_lo, prior_hi) && balanced(q, lo, hi) {
                merge = false;
            }
        }
        if !merge {
            prior_lo = lo;
        }
        prior_hi = hi;
        for j in prior_lo..=hi {
            mask |= 1 << j;
        }
        first = false;
    }
    mask
}

/// Measured field minus this key's left lobe plus the previous key's right lobe, in Q4.
/// The end sensors have one neighbor.
fn residual(keys: usize, q: &[i32], left: &[i32], right: &[i32], out: &mut [i32]) {
    for i in 0..=keys {
        out[i] = q[i] - if i < keys { left[i] } else { 0 } + if i > 0 { right[i - 1] } else { 0 };
    }
}

/// A Q8 share of a field. Multiplied in u32 so an overflowing product wraps the same way
/// everywhere; the signed result is then shifted.
fn share_q8(field: i32, weight: u32) -> i32 {
    ((field as u32).wrapping_mul(weight) as i32) >> 8
}

/// Gives each sensor's residual to its two lobes in proportion to the new evidence; with none,
/// favors the larger existing lobe pair. Each correction's fraction rounds down, even negative.
fn update_lobes(keys: usize, resid: &[i32], evidence: &[i32], left: &mut [i32], right: &mut [i32]) {
    for i in 0..=keys {
        let mut a = if i < keys { evidence[i] } else { 0 };
        let mut b = if i > 0 { evidence[i - 1] } else { 0 };
        if a == 0 && b == 0 {
            a = if i < keys { left[i] + right[i] + 1 } else { 0 };
            b = if i > 0 {
                left[i - 1] + right[i - 1] + 1
            } else {
                0
            };
        }
        // A wrapped or nonpositive total isn't a usable weight. A lobe can still be negative
        // mid-sweep, so the weight isn't clamped until both lobes have their corrections.
        let total = (a as u32).wrapping_add(b as u32) as i32;
        let mut w = 0i32;
        if total > 0 {
            w = if (i32::MIN / 256..=i32::MAX / 256).contains(&a) {
                a * 256 / total
            } else {
                (a as i64 * 256 / total as i64) as i32
            };
        }
        if i < keys {
            left[i] += share_q8(resid[i], w as u32);
        }
        if i > 0 {
            right[i - 1] -= share_q8(resid[i], 256u32.wrapping_sub(w as u32));
        }
    }
    for i in 0..keys {
        let (a, b) = (left[i].max(0), right[i].max(0));
        let (mean, bias) = ((a + b) / 2, (a - b) / 2);
        left[i] = mean + bias;
        right[i] = mean - bias;
    }
}

/// A tridiagonal nearest-neighbor solve: eight forward Gauss-Seidel sweeps.
fn decouple(n: usize, signal: &[i32], estimate: &mut [i32]) {
    estimate[..n].fill(0);
    for _ in 0..8 {
        for i in 0..n {
            let neighbors = (i > 0) as i32 + (i + 1 < n) as i32;
            let rest = if i > 0 { estimate[i - 1] } else { 0 }
                + if i + 1 < n { estimate[i + 1] } else { 0 };
            estimate[i] = (signal[i] - rest) / (5 + neighbors);
        }
    }
}

/// The floor of a square root, without floats.
fn isqrt32(n: u32) -> u32 {
    let (mut lo, mut hi) = (0u32, 65535u32);
    while lo < hi {
        let mid = lo + (hi - lo).div_ceil(2);
        if mid * mid <= n {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    lo
}

/// A key's evidence from its two opposite-polarity sensor residuals. `suppress_geom` drops the
/// geometric part when a neighboring key makes it ambiguous.
#[allow(clippy::too_many_arguments)]
fn depth_score(
    k: usize,
    keys: usize,
    q: &[i32],
    residual: &[i32],
    left: &[i32],
    right: &[i32],
    recent: &[i16; 4],
    suppress_geom: bool,
) -> i32 {
    let d = left[k] + right[k];
    let (l, r) = (residual[k] / 16, residual[k + 1] / 16);
    let recent = recent.map(i32::from);
    let localized = (q[k] / 16 - recent[1] > 10 && q[k + 1] / 16 - recent[2] < -10)
        && (k == 0 || (q[k - 1] / 16 - recent[0]).abs() <= 10)
        && (k + 1 == keys || (q[k + 2] / 16 - recent[3]).abs() <= 10);
    let floor = if d >= 2400 || localized { 1 } else { 4 };
    let mut score = 0;
    let opposed_product = 0u32.wrapping_sub((l as u32).wrapping_mul(r as u32));
    if !suppress_geom && l.abs() > floor && r.abs() > floor && opposed_product <= i32::MAX as u32 {
        score = 16 * isqrt32(opposed_product) as i32;
    }
    if d < QUIET {
        if (k == 0 || right[k - 1] < QUIET) && residual[k] > 0 {
            score += residual[k];
        }
        if (k + 1 == keys || left[k + 1] < QUIET) && residual[k + 1] < 0 {
            score -= residual[k + 1];
        }
    }
    score
}

/// Whether key `j` is too far below its earlier field strength for that position to back up
/// the current residual pattern.
fn fading(j: isize, keys: usize, l: &[i32], r: &[i32], drop: &[i32], peak: &[i32]) -> bool {
    if j < 0 || j as usize >= keys {
        return false;
    }
    let j = j as usize;
    drop[j] > 16 && 100 * (l[j] as i64 + r[j] as i64) < 80 * peak[j] as i64
}

/// What one side of a weak key between two much deeper ones says about it. A release seen on
/// only one sensor needs corroboration.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    Neutral,
    Support,
    WeakRelease,
    Release,
    PairedRelease,
}

#[allow(clippy::too_many_arguments)]
fn flank(
    k: isize,
    dir: isize,
    keys: usize,
    err: &[i32],
    l: &[i32],
    r: &[i32],
    drop: &[i32],
    peak: &[i32],
) -> Side {
    let inner = k + (dir == 1) as isize;
    let polarity = -dir as i32;
    let outer = err[(inner + dir) as usize] * polarity;
    if outer > 32 && !fading(k + 2 * dir, keys, l, r, drop, peak) {
        return Side::Support;
    }
    if outer >= -32 {
        return Side::Neutral;
    }
    let far = inner + 2 * dir;
    if far >= 0
        && far as usize <= keys
        && err[far as usize] * polarity > 32
        && !fading(k + 3 * dir, keys, l, r, drop, peak)
    {
        return Side::Neutral;
    }
    if drop[(k + dir) as usize] <= 16 {
        return Side::WeakRelease;
    }
    if err[inner as usize] * polarity > 32 {
        Side::PairedRelease
    } else {
        Side::Release
    }
}

fn key_strength(i: usize, l: &[i32], r: &[i32], prior: &[i32]) -> i32 {
    ((l[i] + r[i]) / 16).max(prior[i])
}

#[allow(clippy::too_many_arguments)]
fn ambiguous(
    k: usize,
    keys: usize,
    residual: &[i32],
    left: &[i32],
    right: &[i32],
    drop: &[i32],
    prior: &[i32],
    peak: &[i32],
) -> bool {
    if k == 0 || k + 1 == keys {
        return false;
    }
    let d = key_strength(k, left, right, prior);
    if key_strength(k - 1, left, right, prior) <= d * 2
        || key_strength(k + 1, left, right, prior) <= d * 2
    {
        return false;
    }
    let a = flank(k as isize, -1, keys, residual, left, right, drop, peak);
    let b = flank(k as isize, 1, keys, residual, left, right, drop, peak);
    if a == Side::Support || b == Side::Support {
        return false;
    }
    if a == Side::PairedRelease || b == Side::PairedRelease {
        return true;
    }
    (a == Side::Release && (b == Side::Release || b == Side::WeakRelease))
        || (b == Side::Release && a == Side::WeakRelease)
}

fn candidate(mask: u32, i: isize) -> bool {
    i >= 0 && mask & (1 << i) != 0
}

fn settle(n: usize, q: &[i32], mask: u32, l: &mut [i32], r: &mut [i32], age: &mut [u8]) {
    for i in 0..n {
        age[i] = if candidate(mask, i as isize) {
            0
        } else {
            age[i].saturating_add(1)
        };
        if age[i] > 60 {
            l[i] -= l[i] >> 3;
            r[i] -= r[i] >> 3;
        }
    }
    for i in 0..n {
        let ii = i as isize;
        if candidate(mask, ii - 1) || candidate(mask, ii + 1) {
            continue;
        }
        if !candidate(mask, ii) {
            l[i] = 0;
            r[i] = 0;
        } else if (i == 0 || l[i - 1] + r[i - 1] < 160) && (i + 1 == n || l[i + 1] + r[i + 1] < 160)
        {
            l[i] = q[i].max(0);
            r[i] = (-q[i + 1]).max(0);
        }
    }
}

/// The four sensors around key `i` (offsets -1 to +2), 0 past the row's ends.
fn window(n: usize, q: &[i32], i: usize) -> [i32; 4] {
    core::array::from_fn(|j| {
        let k = i as isize + j as isize - 1;
        if k < 0 || k as usize > n {
            0
        } else {
            q[k as usize]
        }
    })
}

fn learn_window(n: usize, q: &[i32], l: &mut [i32], r: &mut [i32], s: &mut DepthHistory) {
    for i in 0..n {
        let z = window(n, q, i);
        let sum = l[i] + r[i];
        let rest = s.rest_window[i].map(i32::from);
        // A bipolar change just emerging at this key mustn't be learned as its new rest level.
        let emerging = sum < QUIET
            && (i == 0 || (z[0] / 16 - rest[0]).abs() <= 10)
            && (i + 1 == n || (z[3] / 16 - rest[3]).abs() <= 10)
            && z[1] / 16 - rest[1] > 1
            && z[2] / 16 - rest[2] < -1;
        let (mut still, mut recovered) = (true, true);
        for j in 0..4 {
            let k = i as isize + j as isize - 1;
            let v = z[j] / 16;
            let quiet = !emerging
                && (k < 0 || k as usize > n || (v - s.prev_field[k as usize] as i32).abs() <= 10);
            if quiet && sum < QUIET {
                s.rest_window[i][j] = v as i16;
            }
            still &= quiet;
            recovered &= (v - s.rest_window[i][j] as i32).abs() <= 10;
        }
        s.stable_for[i] = if still {
            s.stable_for[i].saturating_add(1)
        } else {
            0
        };
        if sum < QUIET {
            s.window_peak[i] = 0;
            continue;
        }
        s.window_peak[i] = s.window_peak[i].max(sum);
        if !recovered {
            if s.stable_for[i] < 8
                || sum as i64 * 100 >= s.window_peak[i] as i64 * 20
                || z[1] >= -STRONG
                || z[2] <= STRONG
                || (i > 0 && (-z[1]) as i64 * 100 < z[0] as i64 * 70)
                || (i + 1 < n && z[2] as i64 * 100 < (-z[3]) as i64 * 70)
            {
                continue;
            }
            for j in 0..4 {
                s.rest_window[i][j] = (z[j] / 16) as i16;
            }
        }
        // Equal facing lobes at a shared sensor cancel in the measured field.
        if i > 0 {
            let d = l[i].min(r[i - 1]);
            l[i] -= d;
            r[i - 1] -= d;
        }
        if i + 1 < n {
            let d = r[i].min(l[i + 1]);
            r[i] -= d;
            l[i + 1] -= d;
        }
    }
    for i in 0..=n {
        s.prev_field[i] = (q[i] / 16) as i16;
    }
}

/// Compares the centered integral from spreading the row's net imbalance over its active
/// sensors with the one from decoupling neighboring fields, and keeps the lower estimate.
fn lift_group(n: usize, x: &[i32], lobes: &[i32], peak: &[i32], out: &mut [i32]) {
    let (mut sum, mut abs_sum) = (0i32, 0i32);
    for &v in &x[..=n] {
        sum += v;
        abs_sum += v.abs();
    }
    if abs_sum == 0 {
        return;
    }
    let (mut integrals, mut last) = ([0i32; MAX_ROW_KEYS], 0i32);
    for i in 0..=n {
        last += x[i] - (sum as i64 * x[i].abs() as i64 / abs_sum as i64) as i32;
        if i < n {
            integrals[i] = last;
        }
    }
    let (mut rhs, mut offset, mut shape) = (
        [0i32; MAX_ROW_KEYS],
        [0i32; MAX_ROW_KEYS],
        [0i32; MAX_ROW_KEYS],
    );
    for i in 0..n {
        rhs[i] = if i > 0 { x[i] } else { 0 } + if i + 1 < n { x[i + 1] } else { 0 };
    }
    decouple(n, &rhs, &mut offset);
    rhs[..n].fill(65536);
    decouple(n, &rhs, &mut shape);
    let (mut error, mut norm) = (sum / 2, 0i32);
    for i in 0..n {
        error -= offset[i];
        norm += shape[i];
    }
    let (mut running, mut prior) = (0i32, 0i32);
    for i in 0..n {
        let correction = offset[i] + (error as i64 * shape[i] as i64 / norm as i64) as i32;
        running += x[i];
        let f = (integrals[i] - last / 2).min(running - correction - 2 * prior);
        prior += correction;
        let gradient = (x[i] - x[i + 1]) / 2;
        if f <= 0
            || ((gradient as u32).wrapping_mul(100) as i32)
                < (0u32.wrapping_sub((f as u32).wrapping_mul(100)) as i32)
            || lobes[i] < PRESSED
            || (lobes[i] as i64) * 100 < peak[i] as i64 * 15
        {
            continue;
        }
        let raw = lobes[i] / 16;
        let mut adjusted = f / 8;
        if raw < 140 && adjusted > raw {
            adjusted = raw + ((adjusted - raw) as i64 * (raw - 80) as i64 / 60) as i32;
        }
        out[i] = out[i].max(adjusted);
    }
}

#[allow(clippy::too_many_arguments)]
fn postfit(
    n: usize,
    q: &[i32],
    mask: u32,
    fitted: bool,
    l: &mut [i32],
    r: &mut [i32],
    s: &mut DepthHistory,
    out: &mut [i32],
) {
    let mut lobes = [0i32; MAX_ROW_KEYS];
    if fitted {
        settle(n, q, mask, l, r, &mut s.skipped_for);
        learn_window(n, q, l, r, s);
    } else {
        for i in 0..=n {
            s.prev_field[i] = (q[i] / 16) as i16;
        }
        for i in 0..n {
            for j in 0..4 {
                let k = i as isize + j as isize - 1;
                s.rest_window[i][j] = if k < 0 || k as usize > n {
                    0
                } else {
                    s.prev_field[k as usize]
                };
            }
        }
    }
    for i in 0..n {
        lobes[i] = l[i] + r[i];
        if fitted {
            s.last_drop[i] = s.previous_sum[i] - lobes[i];
            s.previous_sum[i] = lobes[i];
        }
        let pressed_region = candidate(mask, i as isize) && lobes[i] >= PRESSED;
        s.group_peak[i] = if pressed_region {
            s.group_peak[i].max(lobes[i])
        } else {
            0
        };
        out[i] = lobes[i] / 16;
    }
    let mut i = 0;
    while i < n {
        if !candidate(mask, i as isize) {
            i += 1;
            continue;
        }
        let first = i;
        i += 1;
        while i < n && candidate(mask, i as isize) {
            i += 1;
        }
        if i - first > 1 {
            lift_group(
                i - first,
                &q[first..],
                &lobes[first..],
                &s.group_peak[first..],
                &mut out[first..],
            );
        }
    }
    for i in 0..n {
        if candidate(mask, i as isize)
            && lobes[i] >= PRESSED
            && lobes[i] as i64 * 100 >= s.group_peak[i] as i64 * 15
        {
            out[i] = out[i].max(s.extra_depth[i] - (s.extra_depth[i] >> 5));
        }
        out[i] = out[i].max(0);
        s.extra_depth[i] = if out[i] > lobes[i] / 16 { out[i] } else { 0 };
    }
}
