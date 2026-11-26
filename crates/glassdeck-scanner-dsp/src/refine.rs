//! Press refinement: a small recurrent model per key that corrects the depth estimate on the
//! refined path (Keyglass `fw_refine.c`). All accumulators are 32-bit rings that wrap.

use crate::refine_lut::{SIGMOID_Q15, TANH_Q15};
use crate::{MAX_ROW_KEYS, MAX_ROW_SENSORS};

pub const MODEL_LEN: usize = 1456;
const INPUTS: usize = 30;
const GATES: usize = 18;
const HIDDEN: usize = 6;
const FEATURES: usize = 28;

/// One model shared by every row. The matrices are stored input-major, so one changed feature
/// updates every gate without walking unrelated weights.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Model {
    pub input_weights: [[i16; GATES]; INPUTS],
    pub history_weights: [[i16; GATES]; HIDDEN],
    pub output_weights: [i16; HIDDEN],
    pub input_bias: [i32; GATES],
    pub history_bias: [i32; GATES],
    pub output_bias: i32,
}

impl Model {
    /// Unpacks a press-refinement payload. Integrity is the loader's job; every bit pattern is
    /// safe to evaluate.
    pub fn decode(payload: &[u8; MODEL_LEN]) -> Self {
        let mut r = Reader {
            bytes: payload,
            at: 0,
        };
        // File order: output weights, recurrent bias and weights, input bias and weights, output
        // bias. Each matrix is [gate][input] on the wire, transposed here once.
        let output_weights = core::array::from_fn(|_| r.i16());
        let history_bias = core::array::from_fn(|_| r.i32());
        let mut history_weights = [[0i16; GATES]; HIDDEN];
        for j in 0..GATES {
            for row in history_weights.iter_mut() {
                row[j] = r.i16();
            }
        }
        let input_bias = core::array::from_fn(|_| r.i32());
        let mut input_weights = [[0i16; GATES]; INPUTS];
        for j in 0..GATES {
            for row in input_weights.iter_mut() {
                row[j] = r.i16();
            }
        }
        let output_bias = r.i32();
        debug_assert_eq!(r.at, MODEL_LEN);
        Self {
            input_weights,
            history_weights,
            output_weights,
            input_bias,
            history_bias,
            output_bias,
        }
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn i16(&mut self) -> i16 {
        self.at += 2;
        i16::from_le_bytes([self.bytes[self.at - 2], self.bytes[self.at - 1]])
    }

    fn i32(&mut self) -> i32 {
        self.at += 4;
        i32::from_le_bytes(self.bytes[self.at - 4..self.at].try_into().unwrap())
    }
}

/// One key's recurrent state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RefineKey {
    pub input_sum: [i32; GATES],
    pub history_sum: [i32; GATES],
    pub score: i32,
    pub flux: i32,
    pub envelope: i32,
    pub features: [i16; FEATURES],
    pub hidden: [i16; HIDDEN],
    pub previous_hidden: [i16; HIDDEN],
    pub active: bool,
}

/// One row's refinement state. Zeroed at start, and again after a model, layout or calibration
/// change.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RefineRow {
    pub key: [RefineKey; MAX_ROW_KEYS],
    pub previous_residual: [i32; MAX_ROW_SENSORS],
    pub previous_provisional: [i32; MAX_ROW_KEYS],
    pub width: u8,
    pub settled: bool,
}

fn accumulate(c: i32, a: i32, b: i32) -> i32 {
    (c as u32).wrapping_add((a as u32).wrapping_mul(b as u32)) as i32
}

fn clip_feature(v: i64) -> i32 {
    v.clamp(-32767, 32767) as i32
}

/// Linear interpolation in a 513-entry table spanning -256 to 256 in Q16.
fn activation(table: &[i16; 513], input: i32) -> i32 {
    const ONE: i32 = 1 << 16;
    let ix = if input < -256 * ONE {
        0
    } else if input >= 256 * ONE {
        512 * ONE - 1
    } else {
        input + 256 * ONE
    };
    let (j, rem) = ((ix >> 16) as usize, ix & 65535);
    let (a, b) = (table[j] as i32, table[j + 1] as i32);
    a + (((b - a) * rem) >> 16)
}

/// Per-feature changes smaller than this are ignored, so noise doesn't re-run the model.
const TOLERANCE: [i32; 16] = [
    64, 64, 64, 64, 64, 192, 192, 192, 192, 16, 16, 16, 16, 16, 16, 256,
];
/// Input columns of the constant left- and right-edge flags, folded into the starting bias.
const LEFT_EDGE: usize = 12;
const RIGHT_EDGE: usize = 13;
/// Travel envelope above which a key stays active.
const ENVELOPE_ACTIVE: i32 = 400;

impl RefineKey {
    /// Sets feature `f`; returns whether it changed (and so the model must step).
    fn update_feature(&mut self, model: &Model, f: usize, value: i32, tolerance: i32) -> bool {
        let cache = self.features[f] as i32;
        let mut value = value;
        if (-tolerance..=tolerance).contains(&value) {
            value = 0;
        } else if (-tolerance..=tolerance).contains(&(value - cache)) {
            return false;
        }
        let delta = value - cache;
        if delta == 0 {
            return false;
        }
        self.features[f] = value as i16;
        let w = &model.input_weights[f + if f >= LEFT_EDGE { 2 } else { 0 }];
        for (sum, &weight) in self.input_sum.iter_mut().zip(w) {
            *sum = accumulate(*sum, delta, weight as i32);
        }
        true
    }

    /// Six hidden states: reset and update gates, then six candidate projections.
    fn gru_step(&mut self, model: &Model) {
        let mut score = model.output_bias;
        for i in 0..HIDDEN {
            let reset = activation(
                &SIGMOID_Q15,
                accumulate(self.input_sum[i], 1, self.history_sum[i]),
            ) >> 5;
            let retain = activation(
                &SIGMOID_Q15,
                accumulate(self.input_sum[6 + i], 1, self.history_sum[6 + i]),
            );
            let candidate = activation(
                &TANH_Q15,
                accumulate(
                    self.input_sum[12 + i],
                    reset,
                    self.history_sum[12 + i] >> 10,
                ),
            ) >> 5;
            let h = candidate + (((self.hidden[i] as i32 - candidate) * retain) >> 15);
            self.hidden[i] = clip_feature(h as i64) as i16;
            score = accumulate(score, model.output_weights[i] as i32, self.hidden[i] as i32);
        }
        self.score = score;
    }
}

impl RefineRow {
    fn init(&mut self, model: &Model, residual: &[i32], provisional: &[i32]) {
        let n = provisional.len();
        *self = Self::default();
        self.width = (n + 1) as u8;
        self.previous_residual[..=n].copy_from_slice(residual);
        self.previous_provisional[..n].copy_from_slice(provisional);
        for (k, key) in self.key[..n].iter_mut().enumerate() {
            for i in 0..GATES {
                let mut bias = model.input_bias[i];
                if k == 0 {
                    bias = accumulate(bias, model.input_weights[LEFT_EDGE][i] as i32, 1024);
                }
                if k + 1 == n {
                    bias = accumulate(bias, model.input_weights[RIGHT_EDGE][i] as i32, 1024);
                }
                key.input_sum[i] = bias;
                key.history_sum[i] = model.history_bias[i];
            }
        }
    }

    /// A settled row keeps its recurrent state while only the travel envelopes decay. The field
    /// is checked as well as the travel: a run of small residuals can still add up to a gradient.
    fn quiet_input(&self, residual: &[i32], provisional: &[i32]) -> bool {
        for (i, &p) in provisional.iter().enumerate() {
            if !(-16..=16).contains(&p) || p != self.previous_provisional[i] {
                return false;
            }
        }
        let mut sum = 0;
        for (i, &field) in residual.iter().enumerate() {
            let previous = self.previous_residual[i];
            if !(-16..=16).contains(&field) || previous < field - 6 || previous > field + 6 {
                return false;
            }
            sum += field;
        }
        let mut gradient = -sum;
        for &r in &residual[..provisional.len()] {
            gradient += 2 * r;
            if !(-32..=32).contains(&gradient) {
                return false;
            }
        }
        true
    }

    /// Advances one scan, every scan, idle ones included. `residual` has the row's 2 to 17
    /// conditioned sensor signals; `travel` holds one provisional flux per key going in and the
    /// refined flux coming out.
    pub fn step(&mut self, model: &Model, residual: &[i32], travel: &mut [i32]) {
        let width = residual.len();
        if !(2..=MAX_ROW_KEYS + 1).contains(&width) || travel.len() != width - 1 {
            return;
        }
        let n = width - 1;
        let mut provisional = [0i32; MAX_ROW_KEYS];
        provisional[..n].copy_from_slice(travel);
        let provisional = &provisional[..n];
        if width as u8 != self.width {
            self.init(model, residual, provisional);
        }
        // Both ends are padded so edge and interior keys see the same neighborhood.
        let (mut value, mut velocity) = ([0i32; MAX_ROW_KEYS + 4], [0i32; MAX_ROW_KEYS + 4]);
        let (mut base, mut previous_flux) = ([0i32; MAX_ROW_KEYS + 2], [0i32; MAX_ROW_KEYS + 2]);
        let (mut sum, mut prefix) = (0i64, 0i64);
        let mut advance = [false; MAX_ROW_KEYS];
        for i in 0..width {
            value[1 + i] = clip_feature(4 * residual[i] as i64);
            velocity[1 + i] =
                clip_feature(32 * (residual[i] as i64 - self.previous_residual[i] as i64));
            sum += residual[i] as i64;
        }
        let mut engaged = false;
        for i in 0..n {
            base[i + 1] = clip_feature(provisional[i] as i64);
            previous_flux[i + 1] = clip_feature(self.key[i].flux as i64);
            engaged |= provisional[i] > 20
                || self.key[i].active
                || self.key[i].envelope >= ENVELOPE_ACTIVE;
        }
        let quiet = engaged && self.quiet_input(residual, provisional);
        let evaluate = engaged && !(quiet && self.settled);
        let mut any_change = false;
        for i in 0..if evaluate { n } else { 0 } {
            prefix += residual[i] as i64;
            // The recurrent step isn't needed for isolated travel at rest.
            let active = base[i].max(base[i + 1]).max(base[i + 2]) > 20
                || self.key[i].envelope >= ENVELOPE_ACTIVE;
            let mut changed = active != self.key[i].active;
            self.key[i].active = active;
            if !active {
                continue;
            }
            // The two sensors around the key and their outer neighbors (values, then
            // velocities), then the prior and provisional travel of this key and its neighbors.
            let neighborhood = [
                value[i + 1],
                value[i + 2],
                value[i],
                value[i + 3],
                clip_feature(2 * (2 * prefix - sum)),
                velocity[i + 1],
                velocity[i + 2],
                velocity[i],
                velocity[i + 3],
                previous_flux[i],
                previous_flux[i + 1],
                previous_flux[i + 2],
                base[i + 1],
                base[i],
                base[i + 2],
                clip_feature(16 * (provisional[i] as i64 - self.previous_provisional[i] as i64)),
            ];
            for (f, &v) in neighborhood.iter().enumerate() {
                changed |= self.key[i].update_feature(model, f, v, TOLERANCE[f]);
            }
            for h in 0..HIDDEN {
                let left = if i > 0 {
                    self.key[i - 1].hidden[h] as i32
                } else {
                    0
                };
                let right = if i + 1 < n {
                    self.key[i + 1].hidden[h] as i32
                } else {
                    0
                };
                let key = &mut self.key[i];
                changed |= key.update_feature(model, 16 + h, left, 0);
                changed |= key.update_feature(model, 22 + h, right, 0);
                let delta = key.hidden[h] as i32 - key.previous_hidden[h] as i32;
                if delta != 0 {
                    for (sum, &w) in key.history_sum.iter_mut().zip(&model.history_weights[h]) {
                        *sum = accumulate(*sum, delta, w as i32);
                    }
                    changed = true;
                    key.previous_hidden[h] = key.hidden[h];
                }
            }
            advance[i] = changed;
            any_change |= changed;
        }
        if evaluate {
            self.settled = quiet && !any_change;
        }
        // Every key above read its neighbors' previous states; only now install this scan's.
        self.previous_residual[..width].copy_from_slice(residual);
        self.previous_provisional[..n].copy_from_slice(provisional);
        for i in 0..n {
            let key = &mut self.key[i];
            if advance[i] {
                key.gru_step(model);
            }
            let baseline = self.previous_provisional[i].max(0);
            key.envelope = baseline.max(key.envelope - 4);
            let cap = baseline as i64 + (baseline - 160).max(0) as i64;
            let upper = (if key.envelope >= ENVELOPE_ACTIVE {
                key.envelope
            } else {
                0
            })
            .max(cap.min(i32::MAX as i64) as i32);
            let delta = (25 * activation(&TANH_Q15, key.score)) >> 11;
            let adjusted = baseline as i64 + delta as i64;
            key.flux = upper.min(adjusted.min(i32::MAX as i64) as i32).max(0);
            travel[i] = key.flux;
        }
    }
}
