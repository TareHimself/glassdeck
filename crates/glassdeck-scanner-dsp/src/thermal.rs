//! Temperature drift: watches spare reference sensors and quiet keys, and re-learns resting
//! values when the whole board's field drifts (Keyglass `fw_thermal.c`).

use crate::layout::{KEY_SENSORS, MAX_COLS, ROW_LEN, ROWS, SENSOR_MAP, SENSORS};

/// One key as the scan left it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ThermalKey {
    pub flux: u16,
    pub down: bool,
    /// Proposed down (the same as `down` after a committed rapid edge).
    pub pending: bool,
    /// Masked by the boot or held-key guard.
    pub masked: bool,
}

impl ThermalKey {
    fn busy(&self) -> bool {
        self.down || self.pending || self.masked
    }
}

pub type KeyGrid = [[ThermalKey; 16]; ROWS];

const REFERENCES: usize = 12;
/// Slots away from the keys that measure board-wide change, used when no key covers them.
const REFERENCE_CANDIDATES: [u8; REFERENCES] = [7, 8, 23, 39, 38, 55, 56, 71, 72, 89, 103, 104];

/// Drift history. Zeroed at power-up and kept across sweeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Thermal {
    pub sum: [u16; REFERENCES],
    /// 16-sample totals of the reference channels.
    pub history: [[[u16; REFERENCES]; 4]; 2],
    /// The ADC sweep at each history boundary.
    pub frames: [[[i16; KEY_SENSORS]; 4]; 2],
    pub clock_ms: u64,
    pub history_ms: [u64; 2],
    pub next: [u8; 2],
    pub windows: u8,
    pub initialized: bool,
    pub module_mode: bool,
    pub evidence: u8,
    pub layout: u8,
    pub reference_count: u8,
    pub reference_channel: [u8; REFERENCES],
    pub inverse_center: [i16; KEY_SENSORS],
    pub gain_q10: i32,
    pub disturbance_until: u64,
    pub chord_until: u64,
    pub last_drift: u64,
    pub last_chord: u64,
    pub last_refresh: u64,
    pub latched_drift: bool,
    pub sustained_drift: bool,
    pub deferred_held: bool,
    pub moving: bool,
    pub require_strong: bool,
    pub refreshed: bool,
    pub protect_held: [[u8; 16]; ROWS],
    pub previous_flux: [[u16; 16]; ROWS],
    pub window_flux: [[u16; 16]; ROWS],
    /// The sum of at most sixteen 12-bit readings.
    pub collected: [u16; KEY_SENSORS],
    pub collected_count: u8,
    pub refresh_count: u16,
}

impl Default for Thermal {
    fn default() -> Self {
        Self {
            sum: [0; REFERENCES],
            history: [[[0; REFERENCES]; 4]; 2],
            frames: [[[0; KEY_SENSORS]; 4]; 2],
            clock_ms: 0,
            history_ms: [0; 2],
            next: [0; 2],
            windows: 0,
            initialized: false,
            module_mode: false,
            evidence: 0,
            layout: 0,
            reference_count: 0,
            reference_channel: [0; REFERENCES],
            inverse_center: [0; KEY_SENSORS],
            gain_q10: 0,
            disturbance_until: 0,
            chord_until: 0,
            last_drift: 0,
            last_chord: 0,
            last_refresh: 0,
            latched_drift: false,
            sustained_drift: false,
            deferred_held: false,
            moving: false,
            require_strong: false,
            refreshed: false,
            protect_held: [[0; 16]; ROWS],
            previous_flux: [[0; 16]; ROWS],
            window_flux: [[0; 16]; ROWS],
            collected: [0; KEY_SENSORS],
            collected_count: 0,
            refresh_count: 0,
        }
    }
}

/// The upper median, by the same in-place selection as Keyglass.
fn median(a: &mut [i32]) -> i32 {
    let n = a.len();
    if n == 0 {
        return 0;
    }
    let (mut lo, mut hi, mid) = (0isize, n as isize - 1, n as isize / 2);
    while lo < hi {
        let pivot = a[((lo + hi) / 2) as usize];
        let (mut i, mut j) = (lo, hi);
        while i <= j {
            while a[i as usize] < pivot {
                i += 1;
            }
            while a[j as usize] > pivot {
                j -= 1;
            }
            if i <= j {
                a.swap(i as usize, j as usize);
                i += 1;
                j -= 1;
            }
        }
        if mid <= j {
            hi = j;
        } else if mid >= i {
            lo = i;
        } else {
            break;
        }
    }
    a[mid as usize]
}

/// A view of one sweep for the detectors.
struct Sweep<'a> {
    raw: &'a [u16; SENSORS],
    baseline: &'a [u16; SENSORS],
    keys: &'a KeyGrid,
    layout: usize,
    held: u32,
}

impl Sweep<'_> {
    fn row_len(&self, r: usize) -> usize {
        ROW_LEN[self.layout][r] as usize
    }

    fn sensor(&self, r: usize, c: usize) -> usize {
        SENSOR_MAP[self.layout][r][c] as usize
    }

    fn quiet(&self, r: usize, c: usize) -> bool {
        (c == 0 || self.keys[r][c - 1].flux <= 100)
            && (c + 1 >= self.row_len(r) || self.keys[r][c].flux <= 100)
    }
}

impl Thermal {
    /// Call after every new resting acquisition, held-at-boot substitutions included.
    pub fn acquired(&mut self, baseline: &[u16; SENSORS]) {
        self.latched_drift = false;
        self.deferred_held = false;
        self.require_strong = false;
        self.protect_held = [[0; 16]; ROWS];
        self.reset_window(baseline);
    }

    /// Relative gain is poorly conditioned near the ADC's midpoint, so those sensors are left out
    /// rather than turning a few counts of noise into a large apparent gain change.
    fn reset_window(&mut self, baseline: &[u16; SENSORS]) {
        for (inv, &b) in self.inverse_center.iter_mut().zip(baseline) {
            let scale = b as i32 - 2048;
            *inv = if scale.abs() < 200 {
                0
            } else {
                (1_048_576 / scale) as i16
            };
        }
        self.sum = [0; REFERENCES];
        self.initialized = false;
        self.windows = 0;
        self.gain_q10 = 0;
        self.sustained_drift = false;
    }

    fn select_reference_channels(&mut self, layout: usize) {
        let mut mapped = [false; KEY_SENSORS];
        for r in 0..ROWS {
            for c in 0..ROW_LEN[layout][r] as usize {
                mapped[SENSOR_MAP[layout][r][c] as usize] = true;
            }
        }
        self.reference_count = 0;
        for &candidate in &REFERENCE_CANDIDATES {
            if !mapped[candidate as usize] {
                self.reference_channel[self.reference_count as usize] = candidate;
                self.reference_count += 1;
            }
        }
        self.layout = layout as u8;
        self.sum = [0; REFERENCES];
        self.initialized = false;
        self.windows = 0;
    }

    fn estimate_gain(&self, f: &Sweep<'_>) -> i32 {
        let mut values = [0i32; KEY_SENSORS];
        let mut n = 0;
        for r in 0..ROWS {
            for c in 0..f.row_len(r) {
                let i = f.sensor(r, c);
                if f.quiet(r, c) && self.inverse_center[i] != 0 {
                    values[n] = (self.inverse_center[i] as i32
                        * (f.raw[i] as i32 - f.baseline[i] as i32))
                        >> 10;
                    n += 1;
                }
            }
        }
        median(&mut values[..n])
    }

    /// Whether the change follows the signed resting field, with support in the bottom rows rather
    /// than a disturbance confined to the top.
    fn field_correlation(&self, f: &Sweep<'_>, reference: Option<&[i16; KEY_SENSORS]>) -> bool {
        let (mut dot, mut field_energy, mut change_energy) = (0i64, 0i64, 0i64);
        let (mut top, mut bottom, mut top_n, mut bottom_n) = (0i32, 0i32, 0i32, 0i32);
        for r in 0..ROWS {
            for c in 0..f.row_len(r) {
                let i = f.sensor(r, c);
                if !f.quiet(r, c) || self.inverse_center[i] == 0 {
                    continue;
                }
                let field = f.baseline[i] as i32 - 2048;
                let change =
                    f.raw[i] as i32 - reference.map_or(f.baseline[i] as i32, |rf| rf[i] as i32);
                dot += field as i64 * change as i64;
                field_energy += field as i64 * field as i64;
                change_energy += change as i64 * change as i64;
                let gain = ((self.inverse_center[i] as i32 * change) >> 10).abs();
                if r < 2 {
                    top += gain;
                    top_n += 1;
                } else if r >= 4 {
                    bottom += gain;
                    bottom_n += 1;
                }
            }
        }
        change_energy != 0
            && top_n != 0
            && bottom_n != 0
            && dot * dot >= ((change_energy * field_energy) >> 10) * 205
            && top * bottom_n <= 2 * bottom * top_n
    }

    fn row_drift(&self, f: &Sweep<'_>, crowded: bool, settled: &mut bool) -> bool {
        let (mut rows, mut fast, mut either) = (0u32, 0u32, 0u32);
        let near = &self.frames[0][self.next[0] as usize];
        let far = &self.frames[1][self.next[1] as usize];
        for r in 0..ROWS {
            let (mut a, mut b) = ([0i32; MAX_COLS], [0i32; MAX_COLS]);
            let mut n = 0;
            for c in 0..f.row_len(r) {
                let i = f.sensor(r, c);
                if !f.quiet(r, c) || self.inverse_center[i] == 0 {
                    continue;
                }
                let inv = self.inverse_center[i] as i32;
                a[n] = (inv * (f.raw[i] as i32 - near[i] as i32)) >> 10;
                b[n] = (inv * (f.raw[i] as i32 - far[i] as i32)) >> 10;
                n += 1;
            }
            if n >= 3 {
                let recent = median(&mut a[..n]).abs() > 10;
                either += (recent || median(&mut b[..n]).abs() > 16) as u32;
                fast += recent as u32;
                rows += 1;
            }
        }
        *settled = either == 0;
        if rows < 3 {
            return false;
        }
        if fast * 3 >= rows * 2 {
            return self.field_correlation(f, Some(near));
        }
        !crowded && either * 3 >= rows * 2 && self.field_correlation(f, Some(far))
    }

    fn reference_change(&self, f: &Sweep<'_>, history: Option<&[u16; REFERENCES]>) -> i32 {
        let mut deltas = [0i32; REFERENCES];
        let count = self.reference_count as usize;
        for j in 0..count {
            let previous = history.map_or(
                f.baseline[self.reference_channel[j] as usize] as i32 * 16,
                |h| h[j] as i32,
            );
            deltas[j] = (self.sum[j] as i32 - previous).abs();
        }
        median(&mut deltas[..count]) / 16
    }

    fn snapshot(&mut self, raw: &[u16; SENSORS], age: usize, slot: usize) {
        self.history[age][slot] = self.sum;
        for i in 0..KEY_SENSORS {
            self.frames[age][slot][i] = raw[i] as i16;
        }
    }

    /// Every 16 sweeps compares the background field with rolling 100 ms and 500 ms windows. With an
    /// accessory over the reference sensors, it uses changes shared by the quiet rows instead. A chord
    /// needs much more evidence.
    fn detect(&mut self, f: &Sweep<'_>, ms: u64, module: bool) {
        for j in 0..self.reference_count as usize {
            self.sum[j] = self.sum[j].wrapping_add(f.raw[self.reference_channel[j] as usize]);
        }
        self.windows += 1;
        if self.windows < 16 {
            return;
        }
        self.windows = 0;
        self.gain_q10 = self.estimate_gain(f);
        if !self.initialized {
            for g in 0..2 {
                for k in 0..4 {
                    self.snapshot(f.raw, g, k);
                }
                self.history_ms[g] = ms;
            }
            self.initialized = true;
        }
        let near_history = self.history[0][self.next[0] as usize];
        let far_history = self.history[1][self.next[1] as usize];
        let near = self.reference_change(f, Some(&near_history));
        let far = self.reference_change(f, Some(&far_history));
        self.sustained_drift = self.reference_count >= 6 && self.reference_change(f, None) > 8;
        let mut drift = self.reference_count >= 6 && (near > 6 || far > 8);
        let mut settled = far < 4;
        if f.held > 2 {
            self.chord_until = ms.wrapping_add(600);
        }
        if self.module_mode != module {
            self.evidence = 0;
        }
        self.module_mode = module;
        let chord = self.chord_until > ms;
        if module {
            drift = self.row_drift(f, chord, &mut settled);
            self.sustained_drift =
                !chord && self.gain_q10.abs() > 25 && self.field_correlation(f, None);
        }
        self.evidence = if drift {
            self.evidence.saturating_add(1)
        } else {
            0
        };
        for g in 0..2 {
            if ms.wrapping_sub(self.history_ms[g]) >= if g == 1 { 125 } else { 25 } {
                self.snapshot(f.raw, g, self.next[g] as usize);
                self.next[g] = (self.next[g] + 1) & 3;
                self.history_ms[g] = ms;
            }
        }
        self.sum = [0; REFERENCES];
        let needed = if module {
            if chord { 10 } else { 3 }
        } else {
            2
        };
        if self.evidence >= needed {
            self.disturbance_until = ms.wrapping_add(150);
            self.latched_drift = true;
        }
        self.moving = self.disturbance_until > ms;
        if self.moving || !settled {
            self.last_drift = ms;
        }
        self.require_strong = self.moving
            || self.latched_drift
            || self.deferred_held
            || (self.gain_q10.abs() > 25 && self.sustained_drift)
            || (self.refreshed && ms.wrapping_sub(self.last_refresh) < 1000);
    }

    /// Runs after a sweep's key decisions, before the idle and held guards. Baseline changes and
    /// guards take effect on the next sweep. Returns true when the depth tracker and rapid travel
    /// histories must restart. `module_fields`: an accessory is over the reference sensors.
    pub fn after_scan(
        &mut self,
        layout: u8,
        now_ms: u32,
        raw: &[u16; SENSORS],
        baseline: &mut [u16; SENSORS],
        keys: &KeyGrid,
        module_fields: bool,
    ) -> bool {
        // Extends the wrapping hardware clock, so an expired guard can't come back after 49 days.
        self.clock_ms = self
            .clock_ms
            .wrapping_add(now_ms.wrapping_sub(self.clock_ms as u32) as u64);
        let ms = self.clock_ms;
        if !(1..=3).contains(&layout) {
            return false;
        }
        let layout = layout as usize;
        if self.layout as usize != layout {
            self.select_reference_channels(layout);
        }
        let mut held = 0;
        let mut occupied = false;
        for r in 0..ROWS {
            for c in 0..ROW_LEN[layout][r] as usize - 1 {
                held += keys[r][c].down as u32;
                occupied |= keys[r][c].busy();
            }
        }
        {
            let frozen = *baseline;
            let sweep = Sweep {
                raw,
                baseline: &frozen,
                keys,
                layout,
                held,
            };
            self.detect(&sweep, ms, module_fields);
        }
        // During drift, follows quiet sensors by one count a sweep, never one next to a held,
        // masked or still-moving key.
        let mut stable = true;
        for r in 0..ROWS {
            let n = ROW_LEN[layout][r] as usize;
            for c in 0..n {
                if self.require_strong {
                    let mut at_rest = true;
                    let mut k = c.saturating_sub(1);
                    while k <= c && k + 1 < n {
                        if keys[r][k].busy()
                            || (keys[r][k].flux as i32 - self.previous_flux[r][k] as i32).abs() > 4
                        {
                            at_rest = false;
                        }
                        k += 1;
                    }
                    if at_rest {
                        let i = SENSOR_MAP[layout][r][c] as usize;
                        let step = (raw[i] > baseline[i]) as i32 - (raw[i] < baseline[i]) as i32;
                        baseline[i] = (baseline[i] as i32 + step) as u16;
                    }
                }
                if c + 1 < n {
                    let k = &keys[r][c];
                    if k.down != k.pending
                        || (self.collected_count != 0
                            && (k.flux as i32 - self.window_flux[r][c] as i32).abs() > 30)
                    {
                        stable = false;
                    }
                }
            }
        }
        for r in 0..ROWS {
            for c in 0..16 {
                self.previous_flux[r][c] = keys[r][c].flux;
            }
        }
        if held > 2 {
            self.last_chord = ms;
        }
        if self.moving
            || ms.wrapping_sub(self.last_drift) < 300
            || !stable
            || ms.wrapping_sub(self.last_chord) < 600
            || !(self.latched_drift
                || (self.gain_q10.abs() > 25 && self.sustained_drift)
                || (self.deferred_held && !occupied))
        {
            self.collected_count = 0;
            return false;
        }
        if self.collected_count == 0 {
            self.window_flux = self.previous_flux;
            self.collected = [0; KEY_SENSORS];
        }
        for i in 0..KEY_SENSORS {
            self.collected[i] = self.collected[i].wrapping_add(raw[i]);
        }
        self.collected_count += 1;
        if self.collected_count < 16 {
            return false;
        }
        // A whole stable window can rebaseline the board. A held sensor can't be measured at rest,
        // so it gets only the proportional change estimated from the others.
        let mut blocked = [false; KEY_SENSORS];
        for r in 0..ROWS {
            for c in 0..ROW_LEN[layout][r] as usize - 1 {
                if keys[r][c].busy() {
                    blocked[SENSOR_MAP[layout][r][c] as usize] = true;
                    blocked[SENSOR_MAP[layout][r][c + 1] as usize] = true;
                }
            }
        }
        for i in 0..KEY_SENSORS {
            let b = baseline[i] as i32;
            let next = if blocked[i] {
                b + self.gain_q10 * (b - 2048) / 1024
            } else {
                (self.collected[i] as i32 + 8) / 16
            };
            baseline[i] = next.clamp(0, 4095) as u16;
        }
        self.collected_count = 0;
        self.refresh_count = self.refresh_count.wrapping_add(1);
        self.refreshed = true;
        self.last_refresh = ms;
        self.latched_drift = false;
        self.require_strong = occupied;
        self.deferred_held = occupied;
        for r in 0..ROWS {
            for c in 0..16 {
                self.protect_held[r][c] = keys[r][c].down as u8;
            }
        }
        self.reset_window(baseline);
        true
    }
}
