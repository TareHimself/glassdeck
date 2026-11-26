//! The generator and hash of `firmware/vectors/dsp.c`, step for step, and the tests that replay
//! its cases against the vectors it printed.

use crate::classic::{self, Keys, Neighbors, Settings, Thresholds};
use crate::depth::DepthRow;
use crate::guard::{fixed_candidate, guard_key_signal};
use crate::rapid::{KeyMotion, RapidInput, RapidState, neighbor_delta, update_row_motion};
use crate::refine::{MODEL_LEN, Model, RefineRow};
use crate::{MAX_ROW_KEYS, MAX_ROW_SENSORS, condition_row_q4};

const VECTORS: &str = include_str!("../../../firmware/vectors/dsp.txt");

struct Rng(u32);

impl Rng {
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        self.0
    }

    fn pick(&mut self, n: u32) -> i32 {
        (self.next() % n) as i32
    }
}

struct Hash(u64);

impl Hash {
    fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }

    fn mix(&mut self, value: i32) {
        for byte in value.to_le_bytes() {
            self.0 ^= byte as u64;
            self.0 = self.0.wrapping_mul(0x0100_0000_01b3);
        }
    }

    fn hex(&self) -> String {
        format!("{:016x}", self.0)
    }
}

#[derive(Default)]
struct Sim {
    travel: [i32; MAX_ROW_SENSORS],
    target: [i32; MAX_ROW_SENSORS],
    speed: [i32; MAX_ROW_SENSORS],
    drift: i32,
}

impl Sim {
    /// A frozen sim picks no new targets, so keys only travel toward the ones they have.
    fn step(&mut self, rng: &mut Rng, keys: usize, frozen: bool) {
        for k in 0..keys {
            if !frozen && rng.pick(40) == 0 {
                self.target[k] = if rng.pick(4) == 0 { 0 } else { rng.pick(1400) };
                self.speed[k] = 20 + rng.pick(120);
            }
            let d = self.target[k] - self.travel[k];
            self.travel[k] += d.clamp(-self.speed[k], self.speed[k]);
        }
        self.drift = (self.drift + rng.pick(3) - 1).clamp(-150, 150);
    }

    fn sensors(&self, rng: &mut Rng, width: usize, noise: i32, out: &mut [i32]) {
        let keys = width - 1;
        for (j, o) in out[..width].iter_mut().enumerate() {
            let left = if j > 0 { self.travel[j - 1] } else { 0 };
            let right = if j < keys { self.travel[j] } else { 0 };
            let jitter = if noise != 0 {
                rng.pick((2 * noise + 1) as u32) - noise
            } else {
                0
            };
            *o = (right - left) / 2 + jitter + self.drift;
        }
    }
}

fn random_word(rng: &mut Rng) -> u32 {
    let kind = rng.pick(4) as u32;
    let release = (40 + rng.pick(400)) as u32;
    let actuation = (300 + rng.pick(900)) as u32;
    (kind << 30) | (release << 12) | actuation
}

fn expected(name: &str) -> Vec<&'static str> {
    VECTORS
        .lines()
        .filter_map(|l| l.strip_prefix(name)?.strip_prefix(' '))
        .collect()
}

/// Compares scan by scan, so a mismatch names the first scan that differs.
fn assert_scans(name: &str, got: &[String]) {
    let want = expected(name);
    assert_eq!(got.len(), want.len(), "{name}: scan count");
    if let Some(scan) = got.iter().zip(&want).position(|(g, w)| g != w) {
        panic!("{name}: first differs at scan {scan}");
    }
}

/// Returns the scan hashes and how many key-down scans there were.
fn classic_case(
    seed: u32,
    layout: u8,
    row: usize,
    width: usize,
    has_table: bool,
) -> (Vec<String>, usize) {
    let mut rng = Rng(seed);
    let mut sim = Sim::default();
    let (mut cfg, mut keys, mut neighbors) =
        (Settings::default(), Keys::default(), Neighbors::default());
    let n = width - 1;
    for c in 0..n {
        let word = random_word(&mut rng);
        keys.load(&mut cfg, layout, row, c, has_table, word);
    }
    let (mut hashes, mut downs) = (Vec::new(), 0);
    for scan in 0..800 {
        if scan == 400 {
            for c in 0..n {
                let word = random_word(&mut rng);
                keys.load(&mut cfg, layout, row, c, has_table, word);
            }
        }
        if scan == 600 {
            for c in (0..n).step_by(3) {
                cfg.weight(layout, row, c);
            }
        }
        sim.step(&mut rng, n, false);
        let (mut field, mut travel) = ([0i32; MAX_ROW_SENSORS], [0i32; MAX_ROW_KEYS]);
        sim.sensors(&mut rng, width, 30, &mut field);
        classic::condition_row(row, &mut field[..width], &mut travel[..n]);
        let thresholds = has_table.then_some(Thresholds {
            actuation: &cfg.actuation,
            hysteresis: &cfg.hysteresis,
        });
        neighbors.correct(
            layout,
            row,
            &field[..width],
            &mut travel[..n],
            &keys.down,
            thresholds,
        );
        let held = keys.down[..n].iter().filter(|&&d| d).count();
        keys.step(&cfg, row, held + rng.pick(3) as usize, &travel[..n]);
        downs += keys.down[..n].iter().filter(|&&d| d).count();

        let mut h = Hash::new();
        field[..width].iter().for_each(|&f| h.mix(f));
        for i in 0..n {
            let r = keys.reversal[i];
            for v in [
                travel[i],
                keys.down[i] as i32,
                keys.pending[i] as i32,
                keys.scans[i] as i32,
                r.peak,
                r.trough,
                r.armed as i32,
                r.proposed as i32,
                r.scans as i32,
                neighbors.below_release[i] as i32,
                cfg.actuation[i] as i32,
                cfg.hysteresis[i] as i32,
                cfg.sensitivity[i] as i32,
                cfg.rapid[i] as i32,
            ] {
                h.mix(v);
            }
        }
        hashes.push(h.hex());
    }
    (hashes, downs)
}

fn rapid_case(seed: u32, n: usize) -> (Vec<String>, usize) {
    let mut rng = Rng(seed);
    let mut sim = Sim::default();
    let mut motion = [KeyMotion::default(); MAX_ROW_KEYS];
    let mut state = [RapidState::default(); MAX_ROW_KEYS];
    let (mut down, mut fixed_down) = ([false; MAX_ROW_KEYS], [false; MAX_ROW_KEYS]);
    let (mut protected_held, mut vetoes) = ([0u8; MAX_ROW_KEYS], [0u8; MAX_ROW_KEYS]);
    let (mut act, mut hyst, mut sens, mut gain) = ([0u16; 16], [0u16; 16], [0u16; 16], [0u16; 16]);
    for k in 0..n {
        gain[k] = if k % 4 == 0 {
            0
        } else {
            (200 + rng.pick(60)) as u16
        };
        act[k] = (300 + rng.pick(700)) as u16;
        hyst[k] = (20 + rng.pick(300)) as u16;
        sens[k] = (20 + rng.pick(200)) as u16;
    }
    let (mut hashes, mut downs) = (Vec::new(), 0);
    for scan in 0..800 {
        if scan == 300 {
            for s in state[..n].iter_mut().step_by(2) {
                s.rearm();
            }
        }
        sim.step(&mut rng, n, false);
        let mut flux = [0u16; MAX_ROW_KEYS];
        for k in 0..n {
            let noisy = sim.travel[k] + rng.pick(41) - 20;
            flux[k] = noisy.max(0) as u16;
        }
        update_row_motion(&mut motion[..n], &flux[..n]);
        let strong = (200..260).contains(&scan);
        let row_active = down[..n].iter().filter(|&&d| d).count() as u8;
        let mut h = Hash::new();
        for k in 0..n {
            let guarded = guard_key_signal(
                flux[k],
                gain[k],
                down[k],
                true,
                state[k].peak,
                strong,
                &mut protected_held[k],
                scan % 97 == 0,
            );
            let nd = neighbor_delta(&motion[..n], k);
            let input = RapidInput {
                flux: guarded,
                actuation: act[k],
                hysteresis: hyst[k],
                sensitivity: sens[k],
                weight_q8: gain[k],
                row_active,
                motion: &motion[k],
                neighbor_delta: nd,
            };
            down[k] = state[k].step(down[k], &input);
            let release = act[k] as i32 - hyst[k] as i32;
            fixed_down[k] = fixed_candidate(
                fixed_down[k],
                flux[k] as i32,
                act[k] as i32,
                release,
                nd,
                &motion[k],
                &mut vetoes[k],
            );
            downs += down[k] as usize;
            let (m, s) = (motion[k], state[k]);
            for v in [
                guarded as i32,
                nd,
                down[k] as i32,
                fixed_down[k] as i32,
                protected_held[k] as i32,
                vetoes[k] as i32,
                m.velocity,
                m.delta,
                m.previous_flux as i32,
                m.inhibit_press as i32,
                m.inhibit_release as i32,
                s.last_independent_step,
                s.peak as i32,
                s.trough as i32,
                s.independent_scans as i32,
                s.neighbor_scans as i32,
                s.reversing as i32,
                s.pending_down as i32,
                s.pending_scans as i32,
            ] {
                h.mix(v);
            }
        }
        hashes.push(h.hex());
    }
    (hashes, downs)
}

#[test]
fn classic_tracker_matches_the_firmware_every_scan() {
    for (name, seed, layout, row, width, has_table) in [
        ("classic.ansi_top", 1, 1, 0, 17, true),
        ("classic.iso_row3", 2, 2, 3, 16, true),
        ("classic.jis_space", 3, 3, 5, 14, true),
        ("classic.ansi_space", 4, 1, 5, 12, true),
        ("classic.no_table", 5, 2, 2, 16, false),
    ] {
        let (hashes, downs) = classic_case(seed, layout, row, width, has_table);
        assert!(
            downs > 100,
            "{name}: too few presses ({downs}) to exercise the tracker"
        );
        assert_scans(name, &hashes);
    }
}

#[test]
fn rapid_trigger_and_guards_match_the_firmware_every_scan() {
    for (name, seed, n) in [("rapid.full_row", 6, 16), ("rapid.short_row", 7, 5)] {
        let (hashes, downs) = rapid_case(seed, n);
        assert!(downs > 100, "{name}: too few presses ({downs})");
        assert_scans(name, &hashes);
    }
}

#[test]
fn row_conditioning_matches_the_firmware() {
    let mut rng = Rng(8);
    let mut got = Vec::new();
    for _ in 0..400 {
        let n = 1 + rng.pick(17) as usize;
        let mut row = [0i32; MAX_ROW_SENSORS];
        let base = rng.pick(2000) - 1000;
        let slope = rng.pick(81) - 40;
        let shape = rng.pick(4);
        for (c, value) in row[..n].iter_mut().enumerate() {
            *value = if shape == 0 {
                base
            } else {
                base + slope * c as i32 + rng.pick(41) - 20
            };
            if shape == 2 && rng.pick(4) == 0 {
                *value += rng.pick(3000) - 1500;
            }
        }
        condition_row_q4(&mut row[..n]);
        let mut h = Hash::new();
        row[..n].iter().for_each(|&v| h.mix(v));
        got.push(h.hex());
    }
    assert_scans("condition", &got);
}

/// A model with random coefficients, in the payload's file order.
fn random_model(rng: &mut Rng) -> Model {
    let mut bytes = Vec::with_capacity(MODEL_LEN);
    let put16 = |bytes: &mut Vec<u8>, v: i32| bytes.extend_from_slice(&(v as i16).to_le_bytes());
    let put32 = |bytes: &mut Vec<u8>, v: i32| bytes.extend_from_slice(&v.to_le_bytes());
    for _ in 0..6 {
        put16(&mut bytes, rng.pick(6001) - 3000);
    }
    for _ in 0..18 {
        put32(&mut bytes, rng.pick(200_001) - 100_000);
    }
    for _ in 0..18 * 6 {
        put16(&mut bytes, rng.pick(4001) - 2000);
    }
    for _ in 0..18 {
        put32(&mut bytes, rng.pick(200_001) - 100_000);
    }
    for _ in 0..18 * 30 {
        put16(&mut bytes, rng.pick(4001) - 2000);
    }
    put32(&mut bytes, rng.pick(100_001) - 50_000);
    Model::decode(bytes.as_slice().try_into().unwrap())
}

/// Returns the scan hashes, the key-depth scans above 300 counts, and the settled scans.
fn refined_case(seed: u32, width: usize) -> (Vec<String>, usize, usize) {
    let mut rng = Rng(seed);
    let model = random_model(&mut rng);
    let mut sim = Sim::default();
    let (mut depth, mut refine) = (DepthRow::default(), RefineRow::default());
    let n = width - 1;
    let (mut hashes, mut deep, mut settled) = (Vec::new(), 0, 0);
    for scan in 0..800 {
        if scan == 500 {
            depth.reset();
            refine = RefineRow::default();
        }
        let idle = scan >= 650;
        if idle {
            sim.target[..n].fill(0);
            sim.speed[..n].fill(200);
        }
        sim.step(&mut rng, n, idle);
        let (mut row, mut travel) = ([0i32; MAX_ROW_SENSORS], [0i32; MAX_ROW_KEYS]);
        sim.sensors(&mut rng, width, if idle { 0 } else { 30 }, &mut row);
        condition_row_q4(&mut row[..width]);
        depth.update(&row[..width], &mut travel[..n]);
        deep += travel[..n].iter().filter(|&&t| t > 300).count();
        for v in &mut row[..width] {
            *v /= 16;
        }
        refine.step(&model, &row[..width], &mut travel[..n]);
        settled += refine.settled as usize;

        let mut h = Hash::new();
        row[..width].iter().for_each(|&v| h.mix(v));
        let hist = &depth.history;
        hist.prev_field[..=n].iter().for_each(|&v| h.mix(v as i32));
        for i in 0..n {
            for v in [
                travel[i],
                depth.left[i],
                depth.right[i],
                hist.skipped_for[i] as i32,
            ] {
                h.mix(v);
            }
            hist.rest_window[i].iter().for_each(|&v| h.mix(v as i32));
            for v in [
                hist.stable_for[i] as i32,
                hist.window_peak[i],
                hist.previous_sum[i],
                hist.last_drop[i],
                hist.group_peak[i],
                hist.extra_depth[i],
            ] {
                h.mix(v);
            }
            let k = &refine.key[i];
            for j in 0..18 {
                h.mix(k.input_sum[j]);
                h.mix(k.history_sum[j]);
            }
            for v in [k.score, k.flux, k.envelope] {
                h.mix(v);
            }
            k.features.iter().for_each(|&v| h.mix(v as i32));
            for j in 0..6 {
                h.mix(k.hidden[j] as i32);
                h.mix(k.previous_hidden[j] as i32);
            }
            h.mix(k.active as i32);
            h.mix(refine.previous_provisional[i]);
        }
        refine.previous_residual[..width]
            .iter()
            .for_each(|&v| h.mix(v));
        h.mix(refine.width as i32);
        h.mix(refine.settled as i32);
        hashes.push(h.hex());
    }
    (hashes, deep, settled)
}

#[test]
fn depth_and_refinement_match_the_firmware_every_scan() {
    for (name, seed, width) in [
        ("refined.full_row", 9, 17),
        ("refined.iso_row", 10, 16),
        ("refined.short_row", 11, 6),
    ] {
        let (hashes, deep, settled) = refined_case(seed, width);
        assert!(deep > 100, "{name}: too few deep keys ({deep})");
        eprintln!("{name}: {deep} deep key-scans, {settled} settled scans");
        assert_scans(name, &hashes);
    }
}
