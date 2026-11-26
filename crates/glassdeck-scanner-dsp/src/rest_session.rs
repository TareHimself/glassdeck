//! Recovering from keys held at boot, and re-learning resting values while idle (Keyglass
//! `fw_rest_runtime.c`).

use crate::layout::{KEY_SENSORS, MAX_COLS, ROW_LEN, ROWS, SENSOR_MAP, SENSORS};
use crate::rest::{Guard, Match, RestKey, fields};

const SUSPECT_LIFETIME_MS: u32 = 300_000;
const HELD_EXPIRY_MS: u32 = 20_000;
const LEARNING_MS: u32 = 30_000;
const INHIBIT_ALL_MS: u32 = 5_000;
const SETTLE_MS: u32 = 100;
const SUPPRESS_MS: u32 = 50;
const IDLE_AFTER_MS: u32 = 480_000;
const IDLE_SAMPLE_MS: u32 = 120_000;
const IDLE_SAMPLES: usize = 9;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SensorWatch {
    pub initial_delta: i32,
    pub since_ms: u32,
    pub raw_sum: u32,
    pub samples: u16,
    pub reacquire: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RestSession {
    pub layout: u8,
    pub inhibit_all_new: bool,
    pub inhibit: [[bool; 16]; ROWS],
    pub key: [[RestKey; 16]; ROWS],
    pub thermal_shift: bool,
    pub no_match: bool,
    pub finished: bool,
    pub tracked_keys: u16,
    pub reacquiring: u16,
    pub thermal_generation: u16,
    pub start_ms: u32,
    pub expire_from_ms: u32,
    pub last_adoption_ms: u32,
    pub suppress_window: bool,
    pub suppress_mask: [u16; ROWS],
    pub watch: [SensorWatch; KEY_SENSORS],
    pub quiet: [[bool; 16]; ROWS],
    pub quiet_from_ms: [[u32; 16]; ROWS],
    pub suppress_key_from_ms: [[u32; 16]; ROWS],
    pub last_pressed_ms: u32,
    pub last_idle_sample_ms: u32,
    pub idle_next: u8,
    pub idle_count: u8,
    pub rest_saved: bool,
    pub idle: [[u16; SENSORS]; IDLE_SAMPLES],
}

impl Default for RestSession {
    fn default() -> Self {
        Self {
            layout: 0,
            inhibit_all_new: false,
            inhibit: [[false; 16]; ROWS],
            key: [[RestKey::Clear; 16]; ROWS],
            thermal_shift: false,
            no_match: false,
            finished: false,
            tracked_keys: 0,
            reacquiring: 0,
            thermal_generation: 0,
            start_ms: 0,
            expire_from_ms: 0,
            last_adoption_ms: 0,
            suppress_window: false,
            suppress_mask: [0; ROWS],
            watch: [SensorWatch::default(); KEY_SENSORS],
            quiet: [[false; 16]; ROWS],
            quiet_from_ms: [[0; 16]; ROWS],
            suppress_key_from_ms: [[0; 16]; ROWS],
            last_pressed_ms: 0,
            last_idle_sample_ms: 0,
            idle_next: 0,
            idle_count: 0,
            rest_saved: false,
            idle: [[0; SENSORS]; IDLE_SAMPLES],
        }
    }
}

/// One field of a row in [`RestSession::recover_row`].
#[derive(Clone, Copy, Default)]
struct Field {
    positive: i32,
    negative: i32,
    occupied: bool,
    suspect: bool,
    suppress: bool,
    recovering: bool,
    stable: bool,
}

impl SensorWatch {
    fn observe(&mut self, raw: u16, delta: i32, now: u32) {
        if self.samples == 0 || (delta - self.initial_delta).abs() > 30 || self.samples == u16::MAX
        {
            self.samples = 0;
            self.initial_delta = delta;
            self.since_ms = now;
            self.raw_sum = 0;
        }
        self.raw_sum = self.raw_sum.wrapping_add(raw as u32);
        self.samples += 1;
    }
}

impl RestSession {
    /// Starts on every completed acquisition of an attached frame, not on every keypress.
    pub fn begin(
        layout: u8,
        guard: &Guard,
        bridge_connected: bool,
        qc_mode: bool,
        thermal_generation: u16,
        now_ms: u32,
    ) -> Self {
        let mut s = Self::default();
        if !(1..=3).contains(&layout) {
            return s;
        }
        s.layout = layout;
        s.start_ms = now_ms;
        s.last_pressed_ms = now_ms;
        s.expire_from_ms = now_ms;
        s.thermal_generation = thermal_generation;
        s.thermal_shift = guard.thermal_shift;
        s.no_match = guard.matched != Match::Match;
        s.key = guard.key;
        for r in 0..ROWS {
            for k in 0..ROW_LEN[layout as usize][r] as usize - 1 {
                let held = s.key[r][k] == RestKey::Held;
                s.inhibit[r][k] = held;
                if held && bridge_connected && !qc_mode {
                    s.inhibit_all_new = true;
                }
                s.tracked_keys += (s.key[r][k] != RestKey::Clear) as u16;
            }
        }
        s
    }

    fn row_len(&self, r: usize) -> usize {
        ROW_LEN[self.layout as usize][r] as usize
    }

    fn adopt(&mut self, r: usize, c: usize, now: u32, baseline: &mut [u16; SENSORS]) {
        let sensor = SENSOR_MAP[self.layout as usize][r][c] as usize;
        let w = &mut self.watch[sensor];
        if w.samples != 0 {
            baseline[sensor] = ((w.raw_sum + w.samples as u32 / 2) / w.samples as u32) as u16;
        }
        w.samples = 0;
        if w.reacquire {
            w.reacquire = false;
            self.reacquiring = self.reacquiring.wrapping_sub(1);
        }
        self.suppress_window = true;
        self.last_adoption_ms = now;
        let mut k = c.saturating_sub(1);
        while k <= c && k + 1 < self.row_len(r) {
            self.suppress_mask[r] |= 1 << k;
            self.suppress_key_from_ms[r][k] = now;
            k += 1;
        }
    }

    fn clear_key(
        &self,
        r: usize,
        c: usize,
        down: &[bool],
        pending: &[bool],
        travel: &[i32],
    ) -> bool {
        self.key[r][c] != RestKey::Held && !down[c] && !pending[c] && travel[c] < 120
    }

    #[allow(clippy::too_many_arguments)]
    fn recover_row(
        &mut self,
        row: usize,
        now: u32,
        raw: &[u16; SENSORS],
        delta: &[i32; SENSORS],
        down: &[bool],
        pending: &[bool],
        thermal_busy: bool,
        learning: bool,
        baseline: &mut [u16; SENSORS],
    ) -> bool {
        let map = SENSOR_MAP[self.layout as usize][row];
        let n = self.row_len(row);
        let (mut values, mut travel, mut labels) =
            ([0i32; MAX_COLS], [0i32; MAX_COLS], [0u8; MAX_COLS]);
        let mut fields_ = [Field::default(); MAX_COLS + 1];
        for c in 0..n {
            values[c] = delta[map[c] as usize];
        }
        let groups = fields(&values[..n], &mut travel, &mut labels);
        for c in 0..n - 1 {
            if labels[c] == 0 || labels[c] != labels[c + 1] {
                continue;
            }
            let f = &mut fields_[labels[c] as usize];
            if travel[c] > 0 {
                f.positive += travel[c];
            } else {
                f.negative -= travel[c];
            }
            let key = self.key[row][c];
            f.occupied |= down[c]
                || pending[c]
                || ((self.inhibit_all_new || key == RestKey::Held || self.thermal_shift)
                    && travel[c] > 120);
            f.suspect |= key == RestKey::Uncertain;
        }
        for f in &mut fields_[1..=groups] {
            f.suppress = !thermal_busy
                && (learning || f.suspect)
                && !f.occupied
                && f.negative > if self.thermal_shift { 200 } else { 40 }
                && f.negative > 2 * f.positive;
            f.recovering = f.suppress && f.negative > if self.thermal_shift { 200 } else { 120 };
            f.stable = f.recovering;
        }
        let mut watch = [false; MAX_COLS];
        for c in 0..n {
            watch[c] =
                fields_[labels[c] as usize].recovering || self.watch[map[c] as usize].reacquire;
        }
        let mut held_remains = false;
        for c in 0..n - 1 {
            self.inhibit[row][c] = false;
            if self.key[row][c] != RestKey::Held {
                continue;
            }
            if now.wrapping_sub(self.start_ms) >= HELD_EXPIRY_MS {
                baseline[map[c] as usize] = raw[map[c] as usize];
                baseline[map[c + 1] as usize] = raw[map[c + 1] as usize];
                self.key[row][c] = RestKey::Uncertain;
                self.expire_from_ms = now;
                continue;
            }
            let quiet = travel[c].abs() < 120;
            if quiet && !self.quiet[row][c] {
                self.quiet_from_ms[row][c] = now;
            }
            self.quiet[row][c] = quiet;
            if quiet && !thermal_busy && now.wrapping_sub(self.quiet_from_ms[row][c]) >= SETTLE_MS {
                self.key[row][c] = RestKey::Clear;
                self.tracked_keys = self.tracked_keys.wrapping_sub(1);
                for j in c..=c + 1 {
                    let w = &mut self.watch[map[j] as usize];
                    if !w.reacquire {
                        w.reacquire = true;
                        self.reacquiring = self.reacquiring.wrapping_add(1);
                    }
                }
            } else {
                self.inhibit[row][c] = true;
                held_remains = true;
            }
        }
        for c in 0..n {
            let w = &mut self.watch[map[c] as usize];
            if !watch[c] {
                w.samples = 0;
            } else {
                w.observe(raw[map[c] as usize], values[c], now);
            }
            if w.samples == 0 || now.wrapping_sub(w.since_ms) < SETTLE_MS {
                fields_[labels[c] as usize].stable = false;
            }
        }
        for i in 1..=groups {
            if !fields_[i].stable {
                continue;
            }
            for c in 0..n - 1 {
                if labels[c] as usize == i
                    && labels[c + 1] as usize == i
                    && self.key[row][c] == RestKey::Uncertain
                {
                    self.key[row][c] = RestKey::Clear;
                    self.tracked_keys = self.tracked_keys.wrapping_sub(1);
                }
            }
            for c in 0..n {
                if labels[c] as usize == i {
                    self.adopt(row, c, now, baseline);
                }
            }
            fields_[i].suppress = false;
        }
        for c in 0..n - 1 {
            let (l, r) = (
                fields_[labels[c] as usize].suppress,
                fields_[labels[c + 1] as usize].suppress,
            );
            let suppressed = if self.thermal_shift {
                l && r && labels[c] == labels[c + 1]
            } else {
                l || r
            };
            if suppressed || self.suppress_mask[row] & (1 << c) != 0 {
                self.inhibit[row][c] = true;
            }
        }
        for c in 0..n {
            let w = self.watch[map[c] as usize];
            if w.reacquire
                && w.samples != 0
                && now.wrapping_sub(w.since_ms) >= SETTLE_MS
                && values[c].abs() < 120
                && (c == 0 || self.clear_key(row, c - 1, down, pending, &travel))
                && (c + 1 == n || self.clear_key(row, c, down, pending, &travel))
            {
                self.adopt(row, c, now, baseline);
            }
        }
        held_remains
    }

    /// Runs after actuation and thermal correction each sweep. `delta` is from before row
    /// conditioning; `proposed` is the pending-down state. `thermal_busy` covers drift compensation and
    /// its hold afterwards; `thermal_generation` goes up on every adjustment. Inhibits apply to new
    /// presses from the next sweep, not to keys already held.
    #[allow(clippy::too_many_arguments)]
    pub fn recover(
        &mut self,
        now_ms: u32,
        raw: &[u16; SENSORS],
        delta: &[i32; SENSORS],
        confirmed: &[[bool; 16]; ROWS],
        proposed: &[[bool; 16]; ROWS],
        thermal_busy: bool,
        thermal_generation: u16,
        baseline: &mut [u16; SENSORS],
    ) {
        if self.layout == 0 {
            return;
        }
        if thermal_generation != self.thermal_generation {
            self.thermal_generation = thermal_generation;
            self.expire_from_ms = now_ms;
        }
        let learning = self.no_match && now_ms.wrapping_sub(self.start_ms) < LEARNING_MS;
        // Never compare an unarmed deadline: a long-lived idle keyboard crosses both the signed
        // half-range and rollover. Expired windows retire before another adoption can open one.
        if self.suppress_window {
            if now_ms.wrapping_sub(self.last_adoption_ms) >= SUPPRESS_MS {
                self.suppress_window = false;
                self.suppress_mask = [0; ROWS];
            } else {
                for r in 0..ROWS {
                    for k in 0..16 {
                        if self.suppress_mask[r] & (1 << k) != 0
                            && now_ms.wrapping_sub(self.suppress_key_from_ms[r][k]) >= SUPPRESS_MS
                        {
                            self.suppress_mask[r] &= !(1 << k);
                        }
                    }
                }
            }
        }
        if now_ms.wrapping_sub(self.expire_from_ms) >= SUSPECT_LIFETIME_MS
            && (self.tracked_keys != 0 || self.reacquiring != 0)
        {
            for r in 0..ROWS {
                for c in 0..self.row_len(r) - 1 {
                    if self.key[r][c] == RestKey::Uncertain {
                        self.key[r][c] = RestKey::Clear;
                        self.tracked_keys = self.tracked_keys.wrapping_sub(1);
                    }
                }
            }
            for w in &mut self.watch {
                w.reacquire = false;
            }
            self.reacquiring = 0;
        }
        if self.tracked_keys == 0 && self.reacquiring == 0 && !learning && !self.suppress_window {
            if !self.finished {
                self.finished = true;
                self.no_match = false;
                self.inhibit_all_new = false;
                self.inhibit = [[false; 16]; ROWS];
            }
            return;
        }
        let mut held = false;
        for r in 0..ROWS {
            held |= self.recover_row(
                r,
                now_ms,
                raw,
                delta,
                &confirmed[r],
                &proposed[r],
                thermal_busy,
                learning,
                baseline,
            );
        }
        if !held || now_ms.wrapping_sub(self.start_ms) >= INHIBIT_ALL_MS {
            self.inhibit_all_new = false;
        }
    }

    /// Runs after [`Self::recover`]. While idle long enough it re-learns `baseline` as the median of
    /// nine samples two minutes apart; returns true only when that should be saved as a new record
    /// for this layout and feel. Does no flash I/O.
    pub fn idle(
        &mut self,
        now_ms: u32,
        any_pressed: bool,
        raw: &[u16; SENSORS],
        baseline: &mut [u16; SENSORS],
    ) -> bool {
        if self.layout == 0 {
            return false;
        }
        if any_pressed {
            self.last_pressed_ms = now_ms;
            return false;
        }
        if now_ms.wrapping_sub(self.last_pressed_ms) < IDLE_AFTER_MS
            || now_ms.wrapping_sub(self.last_idle_sample_ms) < IDLE_SAMPLE_MS
        {
            return false;
        }
        self.last_idle_sample_ms = now_ms;
        self.idle[self.idle_next as usize] = *raw;
        self.idle_next = ((self.idle_next as usize + 1) % IDLE_SAMPLES) as u8;
        if (self.idle_count as usize) < IDLE_SAMPLES {
            self.idle_count += 1;
        }
        if (self.idle_count as usize) < IDLE_SAMPLES {
            return false;
        }
        for i in 0..SENSORS {
            let mut sorted: [u16; IDLE_SAMPLES] = core::array::from_fn(|j| self.idle[j][i]);
            sorted.sort_unstable();
            baseline[i] = sorted[IDLE_SAMPLES / 2];
        }
        let mut demoted = false;
        for r in 0..ROWS {
            for c in 0..self.row_len(r) - 1 {
                if self.key[r][c] == RestKey::Held {
                    self.key[r][c] = RestKey::Uncertain;
                    self.inhibit[r][c] = false;
                    demoted = true;
                }
            }
        }
        self.watch = [SensorWatch::default(); KEY_SENSORS];
        self.reacquiring = 0;
        self.inhibit_all_new = false;
        self.no_match = false;
        if demoted {
            self.expire_from_ms = now_ms;
            return false;
        }
        if self.rest_saved {
            return false;
        }
        self.rest_saved = true;
        true
    }
}
