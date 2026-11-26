//! Replays `firmware/vectors/runtime.c`: the firmware's main loop around the scanner runtime, on
//! a generated board, compared with the C scan by scan.
#![allow(clippy::explicit_counter_loop, reason = "the clock steps like the C loop's")]

use crate::calibration::Calibration;
use crate::layout::{KEY_SENSORS, ROW_LEN, ROWS, SENSOR_MAP, SENSORS};
use crate::refine::MODEL_LEN;
use crate::scanner::{ClassicSetup, HidScheduler, HidSink, MAX_KEYS, Runtime};
use crate::testing::{Hash, Rng, assert_scans_in};

const VECTORS: &str = include_str!("../../../firmware/vectors/runtime.txt");

fn input(name: &str) -> Vec<u8> {
    let hex = VECTORS
        .lines()
        .find_map(|l| l.strip_prefix(&format!("runtime.{name} ")))
        .unwrap();
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect()
}

fn adc(v: i32) -> u16 {
    v.clamp(0, 4095) as u16
}

struct Board {
    rest: [i32; SENSORS],
    layout: usize,
    frame_on: bool,
    travel: [[i32; 16]; ROWS],
    target: [[i32; 16]; ROWS],
    speed: [[i32; 16]; ROWS],
    hold: [[i32; 16]; ROWS],
}

impl Board {
    fn new(rng: &mut Rng, layout: usize, id0: i32, id1: i32) -> Self {
        let mut rest = [0; SENSORS];
        for r in rest.iter_mut() {
            *r = 1500 + rng.pick(1000);
        }
        rest[5] = id0;
        rest[106] = id1;
        let zero = [[0; 16]; ROWS];
        Self {
            rest,
            layout,
            frame_on: true,
            travel: zero,
            target: zero,
            speed: zero,
            hold: zero,
        }
    }

    fn step(&mut self, rng: &mut Rng) {
        for r in 0..ROWS {
            for c in 0..ROW_LEN[self.layout][r] as usize - 1 {
                if self.target[r][c] == 0 && self.travel[r][c] == 0 && rng.pick(3000) == 0 {
                    self.target[r][c] = 500 + rng.pick(900);
                    self.speed[r][c] = 40 + rng.pick(80);
                    self.hold[r][c] = 30 + rng.pick(300);
                }
                let (d, s) = (self.target[r][c] - self.travel[r][c], self.speed[r][c]);
                self.travel[r][c] += d.clamp(-s, s);
                if self.target[r][c] != 0
                    && self.travel[r][c] == self.target[r][c]
                    && self.hold[r][c] != 0
                {
                    self.hold[r][c] -= 1;
                    if self.hold[r][c] == 0 {
                        self.target[r][c] = 0;
                    }
                }
            }
        }
    }

    fn sweep(&self, rng: &mut Rng) -> [u16; SENSORS] {
        let mut v = [0i32; SENSORS];
        for (i, x) in v.iter_mut().enumerate() {
            *x = self.rest[i] + rng.pick(7) - 3;
        }
        if self.frame_on {
            v[..5].iter_mut().for_each(|x| *x += 450);
            v[107..112].iter_mut().for_each(|x| *x -= 450);
        } else {
            v[5] = 2300;
            v[106] = 1700;
        }
        for r in 0..ROWS {
            for c in 0..ROW_LEN[self.layout][r] as usize - 1 {
                v[SENSOR_MAP[self.layout][r][c] as usize] += self.travel[r][c] / 2;
                v[SENSOR_MAP[self.layout][r][c + 1] as usize] -= self.travel[r][c] / 2;
            }
        }
        v.map(adc)
    }
}

struct Sink<'a>(&'a mut Hash);

impl HidSink for Sink<'_> {
    fn keyboard(&mut self, modifiers: u8, keycodes: &[u8; 6], nkro_bitmap: &[u8; 32]) -> bool {
        self.0.mix(0x4B);
        self.0.mix(modifiers as i32);
        keycodes
            .iter()
            .chain(nkro_bitmap)
            .for_each(|&b| self.0.mix(b as i32));
        true
    }

    fn consumer(&mut self, usage: u16) -> bool {
        self.0.mix(0x43);
        self.0.mix(usage as i32);
        true
    }

    fn functions(&mut self, mask: u32) -> bool {
        self.0.mix(0x46);
        self.0.mix(mask as i32);
        true
    }
}

fn mix_bytes(h: &mut Hash, bytes: &[u8]) {
    bytes.iter().for_each(|&b| h.mix(b as i32));
}

/// Returns the scan hashes and how many scans sent a keyboard report with a key in it.
fn runtime_case(
    seed: u32,
    layout: usize,
    id0: i32,
    id1: i32,
    refined: bool,
    detach: bool,
) -> (Vec<String>, usize, Runtime) {
    let mut rng = Rng(seed);
    let mut b = Board::new(&mut rng, layout, id0, id1);
    let mut rt = Box::<Runtime>::default();
    rt.cal = Calibration::load(&input("cal_info"), &input("cal_factors")).unwrap();
    rt.load_keymap(&input("keymap1")).unwrap();
    rt.load_profile_settings(&input("profiles")).unwrap();
    if refined {
        let model: [u8; MODEL_LEN] = input("model").try_into().unwrap();
        rt.install_refinement(&model);
    }
    let keymap2 = input("keymap2");
    let mut saved = [[None::<[i16; KEY_SENSORS]>; 3]; 4];
    let mut raw = [0u16; SENSORS];
    let mut sched = HidScheduler::default();
    let (mut last_bitmap, mut last_leds) = ([0u8; 32], 0u8);
    let (mut last_profile, mut last_mode, mut last_platform) = (
        rt.active_profile,
        rt.active_actuation_mode,
        rt.platform_code,
    );
    let (mut last_layers, mut last_event) = (rt.effective_layer_bitfield, rt.of_event_seq);
    let mut now = 1000u32;
    let (mut last_full, mut last_uart) = (now, now - 1000);
    let (mut hashes, mut typing) = (Vec::new(), 0);
    for scan in 0..9000 {
        match scan {
            4000 => rt.qc_mode = true,
            4300 => rt.qc_mode = false,
            5000 => rt.set_active_profile(1),
            5500 => rt.load_keymap(&keymap2).unwrap(),
            7000 => rt.host_state_status = 2,
            _ => {}
        }
        if detach {
            b.frame_on = !(4500..4800).contains(&scan);
        }
        let mut h = Hash::new();
        b.step(&mut rng);
        let full = now - last_full >= 16;
        let sweep = b.sweep(&mut rng);
        let count = if full { SENSORS } else { KEY_SENSORS };
        raw[..count].copy_from_slice(&sweep[..count]);
        if full {
            last_full = now;
        }
        let recal = rt.poll_frame(&raw, now);
        h.mix(recal as i32);
        if recal {
            let mut sums = [0u32; SENSORS];
            let (mut low, mut high) = ([0u16; KEY_SENSORS], [0u16; KEY_SENSORS]);
            for pass in 0..8 {
                raw = b.sweep(&mut rng);
                for i in 0..SENSORS {
                    sums[i] += raw[i] as u32;
                    if i < KEY_SENSORS {
                        if pass == 0 || raw[i] < low[i] {
                            low[i] = raw[i];
                        }
                        if pass == 0 || raw[i] > high[i] {
                            high[i] = raw[i];
                        }
                    }
                }
            }
            let mean = sums.map(|s| (s / 8) as u16);
            let unstable: [bool; KEY_SENSORS] = core::array::from_fn(|i| high[i] - low[i] > 40);
            last_full = now;
            rt.rest_ref_valid = false;
            if rt.keyframe_attached
                && let Some(reference) = saved[rt.keyframe_layout as usize][rt.switch_feel as usize]
            {
                rt.rest_ref_adc = reference;
                rt.rest_ref_valid = true;
            }
            rt.calibrate(&mean, &unstable, now, true);
        }
        let kbd_ready = scan % 4 != 3;
        rt.set_time(now);
        rt.set_hid_ready(kbd_ready);
        let hid = rt.process_scan(&raw);
        let save = rt.finish_scan(now);
        h.mix(save as i32);
        if save {
            saved[rt.keyframe_layout as usize][rt.switch_feel as usize] = Some(rt.rest_ref_adc);
        }
        h.mix(rt.platform_dirty as i32);
        rt.platform_dirty = false;
        sched.step(&hid, kbd_ready, scan % 2 == 0, &mut Sink(&mut h));
        let changed = last_bitmap != rt.key_bitmap
            || rt.host_state_status != last_leds
            || rt.active_profile != last_profile
            || rt.active_actuation_mode != last_mode
            || rt.platform_code != last_platform
            || rt.effective_layer_bitfield != last_layers
            || rt.of_event_seq != last_event;
        if changed || now - last_uart >= 1000 {
            mix_bytes(&mut h, &rt.key_state(now).encode());
            last_bitmap = rt.key_bitmap;
            last_event = rt.of_event_seq;
            last_leds = rt.host_state_status;
            last_profile = rt.active_profile;
            last_mode = rt.active_actuation_mode;
            last_platform = rt.platform_code;
            last_layers = rt.effective_layer_bitfield;
            last_uart = now;
        }
        if rt.qc_mode && full {
            mix_bytes(&mut h, &rt.sensor_readings().encode());
        }
        typing += hid.keycodes.iter().any(|&k| k != 0) as usize;
        h.mix(hid.modifiers as i32);
        mix_bytes(&mut h, &hid.keycodes);
        mix_bytes(&mut h, &hid.nkro_bitmap);
        h.mix(hid.consumer_usage as i32);
        h.mix(hid.of_func_mask as i32);
        for k in 0..MAX_KEYS {
            for v in [
                rt.key_pressed[k] as i32,
                rt.flux[k] as i32,
                rt.latched_valid[k] as i32,
                rt.latched_action[k] as i32,
                rt.fixed_transition_scans[k] as i32,
                rt.neighbor_vetoes[k] as i32,
            ] {
                h.mix(v);
            }
        }
        mix_bytes(&mut h, &rt.key_bitmap);
        rt.baseline_adc.iter().for_each(|&v| h.mix(v as i32));
        for v in [
            rt.active_layer_bitfield as i32,
            rt.toggled_layer_bitfield as i32,
            rt.effective_layer_bitfield as i32,
            rt.active_actuation_mode as i32,
            rt.active_profile as i32,
            rt.platform_code as i32,
            rt.macro_active as i32,
            rt.macro_id as i32,
            rt.macro_step_idx as i32,
            rt.macro_next_ms as i32,
            rt.macro_clock_ms as i32,
        ] {
            h.mix(v);
        }
        mix_bytes(&mut h, &rt.macro_nkro_bitmap);
        let setup = match rt.classic_setup {
            ClassicSetup::Attach => 0,
            ClassicSetup::Reload => 1,
            ClassicSetup::Replaced => 2,
            ClassicSetup::Ready => 3,
        };
        for v in [
            rt.keyframe_attached as i32,
            rt.keyframe_layout as i32,
            rt.switch_feel as i32,
            rt.lifecycle.attach_phase as i32,
            rt.lifecycle.calibrated as i32,
            rt.rest_ref_valid as i32,
            rt.of_event_seq as i32,
            rt.uart_tx_seq as i32,
            setup,
            rt.last_scan_mode as i32,
        ] {
            h.mix(v);
        }
        hashes.push(h.hex());
        now += 1;
    }
    (hashes, typing, *rt)
}

#[test]
fn the_runtime_matches_the_firmware_every_scan() {
    for (name, seed, layout, id0, id1, refined, detach) in [
        ("runtime.ansi_classic", 30, 1, 2700, 2200, false, false),
        ("runtime.iso_refined", 31, 2, 1900, 1400, true, false),
        ("runtime.ansi_detach", 32, 1, 2700, 1400, false, true),
    ] {
        let (hashes, typing, rt) = runtime_case(seed, layout, id0, id1, refined, detach);
        eprintln!(
            "{name}: {typing} scans typing, {} events, layout {}",
            rt.of_event_seq, rt.keyframe_layout
        );
        assert!(
            rt.keyframe_attached && rt.keyframe_layout == layout as u8,
            "{name}: never attached"
        );
        assert!(typing > 500, "{name}: too little typing ({typing})");
        assert_scans_in(VECTORS, name, &hashes);
    }
}
