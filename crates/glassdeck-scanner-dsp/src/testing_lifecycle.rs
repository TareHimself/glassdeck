//! The generator of `firmware/vectors/lifecycle.c`, step for step, and the tests that replay its
//! cases: keyframe detection, thermal drift, rest references and rest recovery.
#![allow(clippy::explicit_counter_loop, reason = "the clock steps like the C generator's")]

use crate::keyframe::Detector;
use crate::layout::{KEY_SENSORS, ROW_LEN, ROWS, SENSOR_MAP, SENSORS};
use crate::rest::{self, Guard, RECORD_LEN};
use crate::rest_session::RestSession;
use crate::testing::{Hash, Rng, assert_scans_in};
use crate::thermal::{KeyGrid, Thermal, ThermalKey};

const VECTORS: &str = include_str!("../../../firmware/vectors/lifecycle.txt");

fn assert_scans(name: &str, got: &[String]) {
    assert_scans_in(VECTORS, name, got);
}

fn vector(name: &str) -> &'static str {
    VECTORS
        .lines()
        .find_map(|l| l.strip_prefix(name)?.strip_prefix(' '))
        .unwrap_or_else(|| panic!("no vector {name}"))
}

fn adc(v: i32) -> u16 {
    v.clamp(0, 4095) as u16
}

fn keyframe_case(seed: u32) -> (Vec<String>, usize) {
    let mut rng = Rng(seed);
    let mut d = Detector::default();
    let (mut phase, mut burst, mut id_burst) = (0, 0, 0);
    let mut id_value = [2048i32; 2];
    let mut now = 1000u32;
    let (mut hashes, mut attached_scans) = (Vec::new(), 0);
    for scan in 0..3000 {
        if scan % 600 == 0 {
            phase = rng.pick(5);
            id_value[0] = [2700, 1900, 2300][rng.pick(3) as usize];
            id_value[1] = [2200, 1400, 1700][rng.pick(3) as usize];
        }
        if rng.pick(30) == 0 {
            burst = 2;
        }
        if rng.pick(400) == 0 {
            id_burst = 3;
        }
        let attached = phase != 0;
        let mut raw = [0u16; KEY_SENSORS];
        for (i, r) in raw.iter_mut().enumerate() {
            let mut v = 2048 + rng.pick(21) - 10;
            if i < 5 && attached {
                v += 350;
            }
            if i >= 107 && attached {
                v -= 450;
            }
            if burst != 0 && i % 23 == 7 {
                v += 200;
            }
            *r = adc(v);
        }
        raw[5] = adc(id_value[0] + rng.pick(21) - 10 + if id_burst != 0 { 400 } else { 0 });
        raw[106] = adc(id_value[1] + rng.pick(21) - 10);
        burst = (burst - 1).max(0);
        id_burst = (id_burst - 1).max(0);
        d.stress_frame = seed & 1 == 1 && phase == 0;
        let f = d.poll(&raw, now);
        now += 25;
        attached_scans += f.attached as usize;
        let mut h = Hash::new();
        for v in [
            f.attached as i32,
            f.layout as i32,
            f.feel as i32,
            f.stage as i32,
            f.changed as i32,
            f.calibrate as i32,
        ] {
            h.mix(v);
        }
        d.history.iter().flatten().for_each(|&v| h.mix(v as i32));
        d.previous.iter().for_each(|&v| h.mix(v as i32));
        for v in [
            d.last_poll_ms as i32,
            d.next as i32,
            d.samples as i32,
            d.stable_id[0] as i32,
            d.stable_id[1] as i32,
            d.stable_scans as i32,
            d.wait_scans as i32,
            d.missing_scans as i32,
        ] {
            h.mix(v);
        }
        hashes.push(h.hex());
    }
    (hashes, attached_scans)
}

#[test]
fn keyframe_detection_matches_the_firmware_every_scan() {
    for (name, seed) in [("keyframe.even", 20), ("keyframe.odd", 21)] {
        let (hashes, attached) = keyframe_case(seed);
        assert!(
            attached > 300,
            "{name}: too few attached scans ({attached})"
        );
        assert_scans(name, &hashes);
    }
}

/// Resting values, a thermal gain and offset, and keys pressed on top.
struct Board {
    rest: [i32; SENSORS],
    gain_q10: i32,
    offset: i32,
    press_left: [[i32; 16]; ROWS],
    press_depth: [[i32; 16]; ROWS],
}

impl Board {
    fn new(rng: &mut Rng) -> Self {
        let mut rest = [0; SENSORS];
        for r in rest.iter_mut() {
            *r = 1400 + rng.pick(1300);
        }
        Self {
            rest,
            ..Self::empty()
        }
    }

    fn empty() -> Self {
        Self {
            rest: [0; SENSORS],
            gain_q10: 0,
            offset: 0,
            press_left: [[0; 16]; ROWS],
            press_depth: [[0; 16]; ROWS],
        }
    }

    /// Each key at rest starts a press with odds 1 in `odds` a scan.
    fn keys(&mut self, rng: &mut Rng, layout: usize, odds: u32) -> KeyGrid {
        let mut keys = [[ThermalKey::default(); 16]; ROWS];
        for r in 0..ROWS {
            for c in 0..16 {
                if c + 1 >= ROW_LEN[layout][r] as usize {
                    continue;
                }
                if self.press_left[r][c] != 0 {
                    self.press_left[r][c] -= 1;
                } else if rng.pick(odds) == 0 {
                    self.press_left[r][c] = 50 + rng.pick(150);
                    self.press_depth[r][c] = 600 + rng.pick(600);
                }
                let flux = if self.press_left[r][c] != 0 {
                    self.press_depth[r][c]
                } else {
                    rng.pick(30)
                } as u16;
                let down = flux > 650;
                let pending = down || (flux > 600 && rng.pick(2) == 0);
                let masked = rng.pick(1000) == 0;
                keys[r][c] = ThermalKey {
                    flux,
                    down,
                    pending,
                    masked,
                };
            }
        }
        keys
    }

    fn raw(&self, rng: &mut Rng, layout: usize, keys: &KeyGrid) -> [u16; SENSORS] {
        let mut v = [0i32; SENSORS];
        for (i, x) in v.iter_mut().enumerate() {
            *x = self.rest[i]
                + (self.rest[i] - 2048) * self.gain_q10 / 1024
                + self.offset
                + rng.pick(7)
                - 3;
        }
        for r in 0..ROWS {
            for c in 0..ROW_LEN[layout][r] as usize - 1 {
                let f = keys[r][c].flux as i32;
                v[SENSOR_MAP[layout][r][c] as usize] += f / 2;
                v[SENSOR_MAP[layout][r][c + 1] as usize] -= f / 2;
            }
        }
        v.map(adc)
    }
}

fn thermal_case(seed: u32, layout: usize, modules: bool) -> (Vec<String>, u16) {
    let mut rng = Rng(seed);
    let mut b = Board::new(&mut rng);
    let mut s = Thermal::default();
    let mut baseline = b.rest.map(|r| r as u16);
    s.acquired(&baseline);
    let mut now = 5000u32;
    let mut hashes = Vec::new();
    for scan in 0..3200 {
        if (300..700).contains(&scan) && scan % 8 == 0 {
            b.gain_q10 += 1;
        }
        if (1400..1800).contains(&scan) && scan % 10 == 0 {
            b.offset += 1;
        }
        if scan == 1200 {
            s.acquired(&baseline);
        }
        let keys = b.keys(&mut rng, layout, 20000);
        let raw = b.raw(&mut rng, layout, &keys);
        let module = modules && (800..1600).contains(&scan);
        let refreshed = s.after_scan(layout as u8, now, &raw, &mut baseline, &keys, module);
        now += 1;
        let mut h = Hash::new();
        h.mix(refreshed as i32);
        baseline.iter().for_each(|&v| h.mix(v as i32));
        s.sum.iter().for_each(|&v| h.mix(v as i32));
        for g in 0..2 {
            for k in 0..4 {
                s.history[g][k].iter().for_each(|&v| h.mix(v as i32));
                s.frames[g][k].iter().for_each(|&v| h.mix(v as i32));
            }
        }
        for v in [
            s.clock_ms as i32,
            s.history_ms[0] as i32,
            s.history_ms[1] as i32,
            s.next[0] as i32,
            s.next[1] as i32,
            s.windows as i32,
            s.initialized as i32,
            s.module_mode as i32,
            s.evidence as i32,
            s.layout as i32,
            s.reference_count as i32,
        ] {
            h.mix(v);
        }
        s.reference_channel.iter().for_each(|&v| h.mix(v as i32));
        s.inverse_center.iter().for_each(|&v| h.mix(v as i32));
        for v in [
            s.gain_q10,
            s.disturbance_until as i32,
            s.chord_until as i32,
            s.last_drift as i32,
            s.last_chord as i32,
            s.last_refresh as i32,
            s.latched_drift as i32,
            s.sustained_drift as i32,
            s.deferred_held as i32,
            s.moving as i32,
            s.require_strong as i32,
            s.refreshed as i32,
        ] {
            h.mix(v);
        }
        for r in 0..ROWS {
            for c in 0..16 {
                h.mix(s.protect_held[r][c] as i32);
                h.mix(s.previous_flux[r][c] as i32);
                h.mix(s.window_flux[r][c] as i32);
            }
        }
        s.collected.iter().for_each(|&v| h.mix(v as i32));
        h.mix(s.collected_count as i32);
        h.mix(s.refresh_count as i32);
        hashes.push(h.hex());
    }
    (hashes, s.refresh_count)
}

#[test]
fn thermal_tracking_matches_the_firmware_every_scan() {
    let mut refreshes = 0;
    for (name, seed, layout, modules) in [
        ("thermal.ansi", 22, 1, false),
        ("thermal.iso_modules", 23, 2, true),
        ("thermal.jis", 24, 3, false),
    ] {
        let (hashes, refreshed) = thermal_case(seed, layout, modules);
        refreshes += refreshed;
        assert_scans(name, &hashes);
    }
    assert!(refreshes > 0, "no case rebaselined the board");
}

struct Acquisition {
    layout: u8,
    mean: [u16; SENSORS],
    unstable: [bool; KEY_SENSORS],
    reference: [i16; KEY_SENSORS],
    has_reference: bool,
    has_unstable: bool,
    held: Vec<(usize, usize)>,
}

fn acquisition(rng: &mut Rng) -> Acquisition {
    let layout = 1 + rng.pick(3) as u8;
    let has_reference = rng.pick(4) != 0;
    let has_unstable = rng.pick(5) != 0;
    let gain = rng.pick(81) - 40;
    let offset = rng.pick(21) - 10;
    let other = rng.pick(8) == 0;
    let mut reference = [0i16; KEY_SENSORS];
    for r in reference.iter_mut() {
        *r = (1500 + rng.pick(1100)) as i16;
    }
    let mut mean = [0u16; SENSORS];
    for (i, m) in mean.iter_mut().enumerate() {
        let v = if i < KEY_SENSORS && has_reference && !other {
            let r = reference[i] as i32;
            r + (r - 2048) * gain / 1024 + offset + rng.pick(9) - 4
        } else {
            1500 + rng.pick(1100)
        };
        *m = adc(v);
    }
    let mut held = Vec::new();
    if rng.pick(3) == 0 {
        let count = 1 + rng.pick(3);
        for _ in 0..count {
            let r = rng.pick(6) as usize;
            let c = rng.pick(ROW_LEN[layout as usize][r] as u32 - 1) as usize;
            let bump = 300 + rng.pick(500);
            held.push((r, c));
            let left = SENSOR_MAP[layout as usize][r][c] as usize;
            let right = SENSOR_MAP[layout as usize][r][c + 1] as usize;
            mean[left] = adc(mean[left] as i32 + bump);
            mean[right] = adc(mean[right] as i32 - bump);
        }
    }
    let mut unstable = [false; KEY_SENSORS];
    for u in unstable.iter_mut() {
        *u = rng.pick(30) == 0;
    }
    Acquisition {
        layout,
        mean,
        unstable,
        reference,
        has_reference,
        has_unstable,
        held,
    }
}

fn mix_guard(h: &mut Hash, g: &Guard) {
    let matched = match g.matched {
        rest::Match::NoReference => 0,
        rest::Match::Match => 1,
        rest::Match::OtherFrame => 2,
    };
    for v in [
        matched,
        g.thermal_gain_q10,
        g.thermal_offset,
        g.thermal_shift as i32,
        g.off_keys as i32,
    ] {
        h.mix(v);
    }
    g.key.iter().flatten().for_each(|&k| h.mix(k as i32));
}

#[test]
fn rest_acquisitions_are_judged_as_the_firmware_judges_them() {
    let mut rng = Rng(25);
    let (mut got, mut held_guards, mut other_frames) = (Vec::new(), 0, 0);
    for _ in 0..400 {
        let mut a = acquisition(&mut rng);
        let unstable = a.has_unstable.then_some(&a.unstable);
        let reference = a.has_reference.then_some(&a.reference);
        let g = rest::apply(a.layout, &mut a.mean, unstable, reference);
        held_guards += g.key.iter().flatten().any(|&k| k == rest::RestKey::Held) as usize;
        other_frames += (g.matched == rest::Match::OtherFrame) as usize;
        let mut h = Hash::new();
        mix_guard(&mut h, &g);
        a.mean.iter().for_each(|&v| h.mix(v as i32));
        got.push(h.hex());
    }
    assert!(
        held_guards > 10 && other_frames > 10,
        "held {held_guards}, other frames {other_frames}"
    );
    assert_scans("rest.apply", &got);
}

fn decode_hash(record: &[u8; RECORD_LEN], layout: u8, feel: u8) -> String {
    match rest::decode(record, layout, feel) {
        None => "refused".into(),
        Some(values) => {
            let mut h = Hash::new();
            values.iter().for_each(|&v| h.mix(v as i32));
            h.hex()
        }
    }
}

fn unhex(text: &str) -> [u8; RECORD_LEN] {
    core::array::from_fn(|i| u8::from_str_radix(&text[2 * i..2 * i + 2], 16).unwrap())
}

#[test]
fn rest_records_match_the_firmware() {
    let mut rng = Rng(26);
    let reference: [i16; KEY_SENSORS] = core::array::from_fn(|_| (rng.pick(5000) - 500) as i16);
    let record = rest::encode(2, 1, &reference);
    assert_eq!(record, unhex(vector("rest.record")));
    assert_eq!(rest::decode(&record, 2, 1), Some(reference));
    for (name, layout, feel) in [
        ("record", 2, 1),
        ("wrong_layout", 1, 1),
        ("wrong_feel", 2, 2),
    ] {
        assert_eq!(
            decode_hash(&record, layout, feel),
            vector(&format!("rest.{name}.decode")),
            "{name}"
        );
    }
    for name in ["corrupt", "byte_checksum"] {
        let r = unhex(vector(&format!("rest.{name}")));
        assert_eq!(
            decode_hash(&r, 2, 1),
            vector(&format!("rest.{name}.decode")),
            "{name}"
        );
    }
}

fn session_case(seed: u32) -> (Vec<String>, usize) {
    let mut rng = Rng(seed);
    let a = loop {
        let a = acquisition(&mut rng);
        if a.has_reference && !a.held.is_empty() {
            break a;
        }
    };
    let layout = a.layout as usize;
    let mut baseline = a.mean;
    let g = rest::apply(
        a.layout,
        &mut baseline,
        Some(&a.unstable),
        Some(&a.reference),
    );
    let mut s = RestSession::begin(a.layout, &g, true, false, 0, 1000);
    let mut b = Board::empty();
    for i in 0..SENSORS {
        b.rest[i] = if i < KEY_SENSORS {
            a.reference[i] as i32
        } else {
            a.mean[i] as i32
        };
    }
    let (mut generation, mut now) = (0u16, 1000u32);
    let (mut hashes, mut saves) = (Vec::new(), 0);
    for scan in 0..3540 {
        let idle_phase = scan >= 3500;
        now += if idle_phase { 60000 } else { 10 };
        if scan == 1500 {
            generation += 1;
        }
        let keys = if idle_phase {
            [[ThermalKey::default(); 16]; ROWS]
        } else {
            b.keys(&mut rng, layout, 400)
        };
        let mut raw = b.raw(&mut rng, layout, &keys);
        if scan < 300 {
            for &(r, c) in &a.held {
                let left = SENSOR_MAP[layout][r][c] as usize;
                let right = SENSOR_MAP[layout][r][c + 1] as usize;
                raw[left] = adc(raw[left] as i32 + 500);
                raw[right] = adc(raw[right] as i32 - 500);
            }
        }
        let delta: [i32; SENSORS] = core::array::from_fn(|i| raw[i] as i32 - baseline[i] as i32);
        let confirmed = keys.map(|row| row.map(|k| k.down));
        let proposed = keys.map(|row| row.map(|k| k.pending));
        let any = confirmed.iter().flatten().any(|&d| d);
        let busy = (900..1000).contains(&scan);
        s.recover(
            now,
            &raw,
            &delta,
            &confirmed,
            &proposed,
            busy,
            generation,
            &mut baseline,
        );
        let save = s.idle(now, any, &raw, &mut baseline);
        saves += save as usize;

        let mut h = Hash::new();
        h.mix(save as i32);
        baseline.iter().for_each(|&v| h.mix(v as i32));
        h.mix(s.layout as i32);
        h.mix(s.inhibit_all_new as i32);
        for r in 0..ROWS {
            for c in 0..16 {
                for v in [
                    s.inhibit[r][c] as i32,
                    s.key[r][c] as i32,
                    s.quiet[r][c] as i32,
                    s.quiet_from_ms[r][c] as i32,
                    s.suppress_key_from_ms[r][c] as i32,
                ] {
                    h.mix(v);
                }
            }
        }
        for v in [
            s.thermal_shift as i32,
            s.no_match as i32,
            s.finished as i32,
            s.tracked_keys as i32,
            s.reacquiring as i32,
            s.thermal_generation as i32,
            s.start_ms as i32,
            s.expire_from_ms as i32,
            s.last_adoption_ms as i32,
            s.suppress_window as i32,
        ] {
            h.mix(v);
        }
        s.suppress_mask.iter().for_each(|&v| h.mix(v as i32));
        for w in &s.watch {
            for v in [
                w.initial_delta,
                w.since_ms as i32,
                w.raw_sum as i32,
                w.samples as i32,
                w.reacquire as i32,
            ] {
                h.mix(v);
            }
        }
        for v in [
            s.last_pressed_ms as i32,
            s.last_idle_sample_ms as i32,
            s.idle_next as i32,
            s.idle_count as i32,
            s.rest_saved as i32,
        ] {
            h.mix(v);
        }
        s.idle.iter().flatten().for_each(|&v| h.mix(v as i32));
        hashes.push(h.hex());
    }
    (hashes, saves)
}

#[test]
fn rest_recovery_matches_the_firmware_every_scan() {
    let mut saves = 0;
    for (name, seed) in [("session.a", 27), ("session.b", 28)] {
        let (hashes, saved) = session_case(seed);
        saves += saved;
        assert_scans(name, &hashes);
    }
    assert!(saves > 0, "no session asked to save a rest record");
}
