//! Which keyframe is attached, from the field at the ends of the board and two ID sensors
//! (Keyglass `fw_keyframe.c`).

use crate::layout::KEY_SENSORS;

/// What the detector concluded. Detached reports layout and feel 0, as does startup.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Frame {
    pub attached: bool,
    pub layout: u8,
    pub feel: u8,
    /// 0 absent, 1 field detected, 2 IDs settling, 3 attached.
    pub stage: u8,
    pub changed: bool,
    pub calibrate: bool,
}

const HISTORY: usize = 30;
/// The two sensors that read the frame's layout and switch-feel magnets.
const ID_SENSORS: [usize; 2] = [5, 106];
const POLL_MS: u32 = 100;

/// Zeroed at power-up, not on detach.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Detector {
    pub history: [[u16; HISTORY]; 2],
    pub previous: [u16; KEY_SENSORS],
    pub last_poll_ms: u32,
    pub next: u8,
    pub samples: u8,
    pub stable_id: [u8; 2],
    pub stable_scans: u8,
    pub wait_scans: u8,
    pub missing_scans: u8,
    /// Test mode: report an ANSI frame when there's no field.
    pub stress_frame: bool,
    pub frame: Frame,
}

impl Default for Detector {
    fn default() -> Self {
        Self {
            history: [[0; HISTORY]; 2],
            previous: [0; KEY_SENSORS],
            last_poll_ms: 0,
            next: 0,
            samples: 0,
            stable_id: [0; 2],
            stable_scans: 0,
            wait_scans: 0,
            missing_scans: 0,
            stress_frame: false,
            frame: Frame::default(),
        }
    }
}

fn upper_median(values: &[u16]) -> i32 {
    let mut sorted = [0u16; HISTORY];
    let sorted = &mut sorted[..values.len()];
    sorted.copy_from_slice(values);
    sorted.sort_unstable();
    sorted[values.len() / 2] as i32
}

impl Detector {
    /// Call after every sensor sweep with milliseconds since boot. Only samples at least 100 ms
    /// apart enter the detector.
    pub fn poll(&mut self, raw: &[u16; KEY_SENSORS], now_ms: u32) -> Frame {
        let old = self.frame;
        if self.last_poll_ms != 0 && now_ms.wrapping_sub(self.last_poll_ms) < POLL_MS {
            return Frame {
                changed: false,
                calibrate: false,
                ..old
            };
        }
        self.last_poll_ms = now_ms;
        let id = ID_SENSORS.map(|s| raw[s]);
        let next = self.next as usize;
        self.history[0][next] = id[0];
        self.history[1][next] = id[1];
        self.next = ((next + 1) % HISTORY) as u8;
        if (self.samples as usize) < HISTORY {
            self.samples += 1;
        }

        let (mut deviation, mut age) = ([0u32; 2], [HISTORY as u32; 2]);
        for i in 0..2 {
            if (self.samples as usize) < HISTORY {
                continue;
            }
            let median = upper_median(&self.history[i]);
            for j in 0..HISTORY {
                let delta = (self.history[i][j] as i32 - median).unsigned_abs();
                deviation[i] = deviation[i].max(delta);
                let since = ((self.next as usize + 59 - j) % HISTORY) as u32;
                if delta > 31 && age[i] > since {
                    age[i] = since;
                }
            }
        }

        let one_moved = old.attached && ((deviation[0] > 250) != (deviation[1] > 250));
        let polarity = [
            if id[0] > 2540 {
                1
            } else if id[0] < 2037 {
                2
            } else {
                0
            },
            if id[1] > 2020 {
                1
            } else if id[1] < 1518 {
                2
            } else {
                0
            },
        ];
        for i in 0..2 {
            let (high_floor, low_ceiling) = if i == 1 { (1919, 1618) } else { (2439, 2137) };
            if deviation[i] > 31 {
                if deviation[i] > 250
                    && !one_moved
                    && (!old.attached || self.stable_id[i] != polarity[i])
                {
                    self.stable_id[i] = 0;
                }
            } else if polarity[i] != 0 {
                self.stable_id[i] = polarity[i];
            } else if (self.stable_id[i] == 1 && id[i] <= high_floor)
                || (self.stable_id[i] == 2 && id[i] > low_ceiling)
            {
                self.stable_id[i] = 0;
            }
        }

        let moving = (0..KEY_SENSORS)
            .filter(|&i| {
                !ID_SENSORS.contains(&i) && (raw[i] as i32 - self.previous[i] as i32).abs() > 60
            })
            .count();
        self.previous = *raw;
        self.stable_scans = if moving > 1 {
            0
        } else {
            self.stable_scans.saturating_add(1)
        };

        let left = upper_median(&raw[..5]);
        let right = upper_median(&raw[107..112]);
        let field = if old.attached {
            left > 2100 && right <= 1899
        } else {
            left > 2200 && right < 1800
        };
        let stress = self.stress_frame as u8;
        let layout = if field { self.stable_id[0] } else { stress };
        let feel = if field { self.stable_id[1] } else { stress };
        let mut frame = old;
        if layout != 0 && feel != 0 {
            if old.attached || self.stable_scans >= 3 || self.wait_scans >= 10 {
                frame.attached = true;
                frame.layout = layout;
                frame.feel = feel;
                self.missing_scans = 0;
                self.wait_scans = 0;
            } else {
                self.wait_scans += 1;
            }
        } else if !old.attached || {
            self.missing_scans += 1;
            self.missing_scans >= 4
        } {
            frame.attached = false;
            frame.layout = 0;
            frame.feel = 0;
            self.missing_scans = 0;
            if !old.attached {
                self.wait_scans = 0;
            }
        }
        frame.stage = if frame.attached {
            3
        } else if !field {
            0
        } else if polarity[0] != 0 && polarity[1] != 0 && age[0] > 9 && age[1] > 9 {
            2
        } else {
            1
        };
        frame.changed =
            frame.attached != old.attached || frame.layout != old.layout || frame.feel != old.feel;
        frame.calibrate = frame.changed && frame.attached;
        self.frame = frame;
        frame
    }
}
