//! Resting references (Keyglass `fw_rest.c`, `fw_rest_internal.h`): the stored record of what
//! every sensor reads at rest for a layout and feel, and the check of a fresh boot acquisition
//! against it.

use crate::layout::{KEY_SENSORS, MAX_COLS, ROW_LEN, ROWS, SENSOR_MAP, SENSORS};

pub const RECORD_LEN: usize = 236;
const MAGIC: u32 = 0x5245_5354;
const VERSION: u16 = 1;
const CHECKSUM_AT: usize = 232;

fn get16(p: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([p[at], p[at + 1]])
}

fn get32(p: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(p[at..at + 4].try_into().unwrap())
}

fn record_checksum(p: &[u8; RECORD_LEN]) -> u32 {
    let start = get32(p, 0)
        .wrapping_add(get16(p, 4) as u32)
        .wrapping_add(p[6] as u32)
        .wrapping_add(p[7] as u32);
    (0..KEY_SENSORS).fold(start, |h, i| {
        h.wrapping_mul(31).wrapping_add(get16(p, 8 + 2 * i) as u32)
    })
}

pub fn encode(layout: u8, feel: u8, reference: &[i16; KEY_SENSORS]) -> [u8; RECORD_LEN] {
    let mut out = [0u8; RECORD_LEN];
    out[0..4].copy_from_slice(&MAGIC.to_le_bytes());
    out[4..6].copy_from_slice(&VERSION.to_le_bytes());
    out[6] = layout;
    out[7] = feel;
    for (i, r) in reference.iter().enumerate() {
        out[8 + 2 * i..10 + 2 * i].copy_from_slice(&r.to_le_bytes());
    }
    let checksum = record_checksum(&out);
    out[CHECKSUM_AT..].copy_from_slice(&checksum.to_le_bytes());
    out
}

/// Reads a record for this layout and feel. Also accepts the byte-wise checksum OpenFlux
/// development builds wrote; [`encode`] never writes it.
pub fn decode(record: &[u8; RECORD_LEN], layout: u8, feel: u8) -> Option<[i16; KEY_SENSORS]> {
    if get32(record, 0) != MAGIC
        || get16(record, 4) != VERSION
        || record[6] != layout
        || record[7] != feel
    {
        return None;
    }
    let stored = get32(record, CHECKSUM_AT);
    if record_checksum(record) != stored {
        let bytes = record[..CHECKSUM_AT]
            .iter()
            .fold(0u32, |h, &b| h.wrapping_mul(31).wrapping_add(b as u32));
        if stored != bytes {
            return None;
        }
    }
    Some(core::array::from_fn(|i| get16(record, 8 + 2 * i) as i16))
}

/// A field is the non-quiet sensors with gaps of at most three quiet ones. Inside a field, travel
/// is the signal summed to the key's left minus half the field's total. Labels count from 1; 0 is
/// outside every field. Returns the number of fields.
pub(crate) fn fields(delta: &[i32], travel: &mut [i32], label: &mut [u8]) -> usize {
    let n = delta.len();
    travel[..n].fill(0);
    label[..n].fill(0);
    let loud = |v: i32| !(-40..=40).contains(&v);
    let mut id = 0;
    let mut first = 0;
    while first < n {
        if !loud(delta[first]) {
            first += 1;
            continue;
        }
        let mut last = first;
        let mut c = first + 1;
        while c < n && c <= last + 4 {
            if loud(delta[c]) {
                last = c;
            }
            c += 1;
        }
        id += 1;
        let total: i32 = delta[first..=last].iter().sum();
        label[first..=last].fill(id as u8);
        let mut prefix = 0;
        for c in first..last {
            prefix += delta[c];
            travel[c] = prefix - total / 2;
        }
        first = last + 1;
    }
    id
}

/// How a fresh acquisition compares with the stored reference.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Match {
    #[default]
    NoReference,
    Match,
    /// Too many keys disagree: the reference belongs to another frame.
    OtherFrame,
}

/// A key's state at boot, as the guard sees it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum RestKey {
    #[default]
    Clear = 0,
    /// Pressed at boot, for certain.
    Held = 1,
    Uncertain = 2,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Guard {
    pub matched: Match,
    pub thermal_gain_q10: i32,
    pub thermal_offset: i32,
    pub thermal_shift: bool,
    /// Keys off their reference, counted before rejecting a reference from another frame.
    pub off_keys: u8,
    pub key: [[RestKey; 16]; ROWS],
}

fn upper_median(v: &mut [i32]) -> i32 {
    v.sort_unstable();
    if v.is_empty() { 0 } else { v[v.len() / 2] }
}

/// Slots away from the keys, which measure board-wide temperature change whatever the keys do.
const BACKGROUND_SLOTS: [usize; 12] = [7, 8, 23, 38, 39, 55, 56, 71, 72, 89, 103, 104];

fn background_drift(mean: &[u16; SENSORS], reference: &[i16; KEY_SENSORS]) -> bool {
    let mut change = BACKGROUND_SLOTS.map(|s| (mean[s] as i32 - reference[s] as i32).abs());
    upper_median(&mut change) > 8
}

fn align_reference(
    layout: usize,
    mean: &[u16; SENSORS],
    rest: &[i16; KEY_SENSORS],
    adjusted: &mut [i32; KEY_SENSORS],
    g: &mut Guard,
) {
    let mut samples = [0i32; KEY_SENSORS];
    let mut n = 0;
    for r in 0..ROWS {
        for c in 0..ROW_LEN[layout][r] as usize {
            let s = SENSOR_MAP[layout][r][c] as usize;
            let x = rest[s] as i32 - 2048;
            if x.abs() >= 200 {
                samples[n] = (mean[s] as i32 - rest[s] as i32) * 1024 / x;
                n += 1;
            }
        }
    }
    let gain = upper_median(&mut samples[..n]);
    for s in 0..KEY_SENSORS {
        adjusted[s] = rest[s] as i32 + ((gain as i64 * (rest[s] as i64 - 2048)) / 1024) as i32;
    }
    n = 0;
    for r in 0..ROWS {
        for c in 0..ROW_LEN[layout][r] as usize {
            let s = SENSOR_MAP[layout][r][c] as usize;
            samples[n] = mean[s] as i32 - adjusted[s];
            n += 1;
        }
    }
    g.thermal_gain_q10 = gain;
    g.thermal_offset = upper_median(&mut samples[..n]);
    for a in adjusted.iter_mut() {
        *a += g.thermal_offset;
    }
}

fn adc_range(x: i32) -> u16 {
    x.clamp(0, 4095) as u16
}

/// Consecutive excursions, with gaps of up to three quiet sensors, are one local field. Each key
/// inside it is judged on both its sides against the field's center.
#[allow(clippy::too_many_arguments)]
fn classify_row(
    row: usize,
    map: &[u8],
    mean: &[u16; SENSORS],
    adjusted: &[i32; KEY_SENSORS],
    threshold: i32,
    thermal: bool,
    replace: &mut [bool; KEY_SENSORS],
    g: &mut Guard,
) {
    let n = map.len();
    let (mut delta, mut travel, mut label) = ([0i32; MAX_COLS], [0i32; MAX_COLS], [0u8; MAX_COLS]);
    let mut certain = [false; MAX_COLS + 1];
    for c in 0..n {
        let s = map[c] as usize;
        delta[c] = mean[s] as i32 - adjusted[s];
    }
    fields(&delta[..n], &mut travel, &mut label);
    for c in 0..n - 1 {
        if !thermal && travel[c] > 300 {
            certain[label[c] as usize] = true;
        }
    }
    for c in 0..n {
        if certain[label[c] as usize] {
            replace[map[c] as usize] = true;
        }
        if travel[c] > threshold {
            g.key[row][c] = if certain[label[c] as usize] {
                RestKey::Held
            } else {
                RestKey::Uncertain
            };
            g.off_keys = g.off_keys.wrapping_add(1);
        }
    }
}

/// Turns a full eight-sweep acquisition into this session's baselines. `mean` has all 160 channels;
/// `unstable` marks channels whose range over the sweeps was over 40. `reference` belongs to the
/// detected layout and feel. Only `mean` and the returned guard change; substituted baselines stay
/// within the ADC's range. Nothing here should be saved.
pub fn apply(
    layout: u8,
    mean: &mut [u16; SENSORS],
    unstable: Option<&[bool; KEY_SENSORS]>,
    reference: Option<&[i16; KEY_SENSORS]>,
) -> Guard {
    let mut g = Guard::default();
    if !(1..=3).contains(&layout) {
        return g;
    }
    let layout = layout as usize;
    let mut adjusted = [0i32; KEY_SENSORS];
    if let Some(reference) = reference {
        align_reference(layout, mean, reference, &mut adjusted, &mut g);
        g.matched = Match::Match;
        g.thermal_shift = g.thermal_gain_q10.abs() > 40 || background_drift(mean, reference);
        let mut count = 0u32;
        let mut replace = [false; KEY_SENSORS];
        for row in 0..ROWS {
            let n = ROW_LEN[layout][row] as usize;
            let threshold = if g.thermal_shift { 200 } else { 120 };
            let thermal = g.thermal_shift;
            classify_row(
                row,
                &SENSOR_MAP[layout][row][..n],
                mean,
                &adjusted,
                threshold,
                thermal,
                &mut replace,
                &mut g,
            );
            count += n as u32 - 1;
        }
        if g.off_keys as u32 * 100 > count * 30 {
            g.matched = Match::OtherFrame;
            g.thermal_shift = false;
            g.key = [[RestKey::Clear; 16]; ROWS];
        } else {
            for s in 0..KEY_SENSORS {
                if replace[s] {
                    mean[s] = adc_range(adjusted[s]);
                }
            }
            // A noisy sensor is judged against the corrected frame. Only the recomputed `adjusted`
            // is used; the guard keeps the fit from the uncorrected frame.
            if unstable.is_some() {
                let mut discarded = Guard::default();
                align_reference(layout, mean, reference, &mut adjusted, &mut discarded);
            }
        }
    }
    let Some(unstable) = unstable else { return g };
    for row in 0..ROWS {
        let n = ROW_LEN[layout][row] as usize;
        for c in 0..n {
            let s = SENSOR_MAP[layout][row][c] as usize;
            if !unstable[s] {
                continue;
            }
            if g.matched == Match::Match {
                mean[s] = adc_range(adjusted[s]);
            } else {
                if c > 0 {
                    g.key[row][c - 1] = RestKey::Uncertain;
                }
                if c + 1 < n {
                    g.key[row][c] = RestKey::Uncertain;
                }
            }
        }
    }
    g
}
