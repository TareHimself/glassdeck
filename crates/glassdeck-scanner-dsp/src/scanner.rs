//! The key scanner's runtime: one scan pass from 160 ADC readings to key states and a HID
//! report, the keymap, layers, macros and profiles, the frame lifecycle and the serial-link
//! messages (Keyglass `fw_scanner.c` and `fw_lifecycle.c`). Everything here is pure: the
//! firmware feeds it readings and the clock, and does the I/O.

use glassdeck_scanner_proto::keymap::{
    Keymap, MAX_BLOB, MAX_PROFILES, Profile, ProfileError, ProfileSettings,
};
use glassdeck_scanner_proto::serial::{
    self, FunctionEvents, KeyState, KeyframeLayout, KeyframePhase, SensorReadings,
};

use crate::calibration::Calibration;
use crate::classic::{self, Keys, Neighbors, Settings, Thresholds};
use crate::depth::DepthRow;
use crate::guard::{fixed_candidate, guard_key_signal};
use crate::keyframe::Detector;
use crate::layout::{KEY_SENSORS, MAX_COLS, ROW_LEN, ROWS, SENSOR_MAP, SENSORS};
use crate::rapid::{KeyMotion, RapidInput, RapidState, neighbor_delta, update_row_motion};
use crate::refine::{MODEL_LEN, Model, RefineRow};
use crate::rest;
use crate::rest_session::RestSession;
use crate::thermal::{KeyGrid, Thermal, ThermalKey};
use crate::{MAX_ROW_KEYS, condition_row_q4};

pub const MAX_KEYS: usize = 96;
const RESTING_BASELINE: u16 = 1800;
const TRANSPARENT: u32 = 0x5000_0000;
const DISABLED: u32 = 0xF000_0000;
const DEFAULT_ACTUATION: u32 = 650;
const DEFAULT_REVERSAL: u32 = 216;

/// One layout's place in the stored keymap, in words from the blob's start.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LayoutView {
    pub num_act_modes: u16,
    pub num_keys: u16,
    pub num_layers: u16,
    pub num_macros: u16,
    pub macro_pool_words: u32,
    /// Offsets from `section_word`.
    pub keycodes_word_off: u32,
    pub actuations_word_off: u32,
    pub section_word: u32,
}

/// What the classic tracker must do with its settings at the next scan.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ClassicSetup {
    /// A frame was just attached: load every key's settings.
    #[default]
    Attach,
    /// The mode changed: reload and reapply position gain.
    Reload,
    /// The keymap was replaced while attached: one scan on table 0, then the profile's mode.
    Replaced,
    Ready,
}

/// The keyboard HID report a scan wants sent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HidReport {
    pub modifiers: u8,
    pub keycodes: [u8; 6],
    pub nkro_bitmap: [u8; 32],
    pub consumer_usage: u16,
    pub of_func_mask: u32,
}

/// Frame detection, the drift tracker and boot recovery, which run between scan frames.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Lifecycle {
    pub detector: Detector,
    pub rest: RestSession,
    pub thermal: Thermal,
    pub observed: KeyGrid,
    pub confirmed: [[bool; 16]; ROWS],
    pub proposed: [[bool; 16]; ROWS],
    pub calibrated: bool,
    /// 0 absent, 1 detecting, 2 settling, 3 attached, 4 recovering.
    pub attach_phase: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Runtime {
    pub lifecycle: Lifecycle,
    pub classic_settings: [Settings; ROWS],
    pub classic_keys: [Keys; ROWS],
    pub classic_neighbors: [Neighbors; ROWS],
    pub classic_setup: ClassicSetup,
    pub refinement_active: bool,
    pub refinement: Option<Model>,
    pub depth_rows: [DepthRow; ROWS],
    pub refine_rows: [RefineRow; ROWS],
    pub cal: Calibration,
    pub baseline_adc: [u16; SENSORS],
    pub raw_adc: [u16; SENSORS],
    pub sensor_scaled: [i32; SENSORS],
    pub flux: [u16; MAX_KEYS],
    pub motion: [KeyMotion; MAX_KEYS],
    pub rapid: [RapidState; MAX_KEYS],
    pub key_pressed: [bool; MAX_KEYS],
    pub fixed_transition_scans: [u8; MAX_KEYS],
    pub neighbor_vetoes: [u8; MAX_KEYS],
    pub last_scan_mode: u16,
    pub latched_valid: [bool; MAX_KEYS],
    pub latched_action: [u32; MAX_KEYS],
    pub key_bitmap: [u8; 32],
    pub keymap_blob: [u8; MAX_BLOB],
    pub keymap_len: usize,
    /// By layout ID; index 0 is unused.
    pub layouts: [Option<LayoutView>; 4],
    pub profiles: [Profile; MAX_PROFILES],
    pub num_profiles: u32,
    pub keyframe_layout: u8,
    pub keyframe_attached: bool,
    pub switch_feel: u8,
    /// The configured, default or switched base layers.
    pub active_layer_bitfield: u32,
    /// Layers latched on by toggle keys.
    pub toggled_layer_bitfield: u32,
    /// Base, toggled and held layers together.
    pub effective_layer_bitfield: u32,
    pub active_actuation_mode: u32,
    pub language_code: u32,
    pub platform_code: u32,
    pub active_profile: u32,
    pub macro_active: bool,
    pub macro_layout: u8,
    pub macro_clock_updated: bool,
    pub macro_hid_ready: bool,
    pub macro_id: u16,
    pub macro_step_idx: u32,
    pub macro_next_ms: u32,
    pub macro_clock_ms: u32,
    pub macro_nkro_bitmap: [u8; 32],
    pub qc_mode: bool,
    pub stress_test_enabled: bool,
    /// The host's lock LEDs: 0x01 Num, 0x02 Caps, 0x04 Scroll.
    pub host_state_status: u8,
    pub platform_dirty: bool,
    pub of_event_seq: u16,
    pub of_event_ring: [u8; 16],
    pub of_event_profiles: [u8; 16],
    pub rest_ref_valid: bool,
    pub rest_ref_adc: [i16; KEY_SENSORS],
    pub uart_tx_seq: u32,
}

impl Default for Runtime {
    fn default() -> Self {
        Self {
            lifecycle: Lifecycle::default(),
            classic_settings: [Settings::default(); ROWS],
            classic_keys: [Keys::default(); ROWS],
            classic_neighbors: [Neighbors::default(); ROWS],
            classic_setup: ClassicSetup::Attach,
            refinement_active: false,
            refinement: None,
            depth_rows: [DepthRow::default(); ROWS],
            refine_rows: [RefineRow::default(); ROWS],
            cal: Calibration::default(),
            baseline_adc: [RESTING_BASELINE; SENSORS],
            raw_adc: [0; SENSORS],
            sensor_scaled: [0; SENSORS],
            flux: [0; MAX_KEYS],
            motion: [KeyMotion::default(); MAX_KEYS],
            rapid: [RapidState::default(); MAX_KEYS],
            key_pressed: [false; MAX_KEYS],
            fixed_transition_scans: [0; MAX_KEYS],
            neighbor_vetoes: [0; MAX_KEYS],
            last_scan_mode: 0,
            latched_valid: [false; MAX_KEYS],
            latched_action: [0; MAX_KEYS],
            key_bitmap: [0; 32],
            keymap_blob: [0; MAX_BLOB],
            keymap_len: 0,
            layouts: [None; 4],
            profiles: [Profile::default(); MAX_PROFILES],
            num_profiles: 0,
            keyframe_layout: 0,
            keyframe_attached: false,
            switch_feel: 0,
            active_layer_bitfield: 1,
            toggled_layer_bitfield: 0,
            effective_layer_bitfield: 1,
            active_actuation_mode: 0,
            language_code: 0,
            platform_code: 0,
            active_profile: 0,
            macro_active: false,
            macro_layout: 0,
            macro_clock_updated: false,
            macro_hid_ready: true,
            macro_id: 0,
            macro_step_idx: 0,
            macro_next_ms: 0,
            macro_clock_ms: 0,
            macro_nkro_bitmap: [0; 32],
            qc_mode: false,
            stress_test_enabled: false,
            host_state_status: 0,
            platform_dirty: false,
            of_event_seq: 0,
            of_event_ring: [0; 16],
            of_event_profiles: [0; 16],
            rest_ref_valid: false,
            rest_ref_adc: [0; KEY_SENSORS],
            uart_tx_seq: 0,
        }
    }
}

/// OpenFlux function codes the scanner handles itself.
mod function {
    pub const PROFILE_CYCLE_NEXT: u8 = 0x04;
    pub const PROFILE_CYCLE_PREV: u8 = 0x05;
    pub const ACTUATION_MODE_NEXT: u8 = 0x06;
    pub const ACTUATION_MODE_PREV: u8 = 0x07;
    pub const PLATFORM_LINUX: u8 = 0x08;
    pub const PLATFORM_MACOS: u8 = 0x09;
    pub const PLATFORM_WINDOWS: u8 = 0x0A;
}

const ROW_GAIN: [u16; ROWS] = [256, 253, 250, 246, 242, 234];
/// Wide off-center keys by layout (ISO, other) and row, which get a further 224/256 gain.
const REDUCED: [[u16; ROWS]; 2] = [
    [0, 1 << 13, 1 | (1 << 13), 1 | (1 << 12), 1 | (1 << 11), 1],
    [0, 1 << 13, 1 | (1 << 13), 1 | (1 << 13), 1 | (1 << 12), 1],
];

/// Position gains are Q8: the row gain, and a second gain for the wider off-center keys.
fn position_gain(layout: u8, row: usize, col: usize) -> u16 {
    if REDUCED[(layout == 2) as usize][row] & (1 << col) != 0 {
        ((ROW_GAIN[row] as u32 * 224) >> 8) as u16
    } else {
        ROW_GAIN[row]
    }
}

fn report_flux(value: i32) -> u16 {
    value.clamp(0, u16::MAX as i32) as u16
}

fn apply_feel(flux: i32, stronger: bool) -> i32 {
    if stronger {
        ((flux as u32).wrapping_mul(307) as i32) >> 8
    } else {
        flux
    }
}

fn set_bit(bitmap: &mut [u8; 32], bit: usize) {
    bitmap[bit >> 3] |= 1 << (bit & 7);
}

impl Runtime {
    fn word(&self, index: u32) -> u32 {
        let at = index as usize * 4;
        u32::from_le_bytes(self.keymap_blob[at..at + 4].try_into().unwrap())
    }

    fn section_word(&self, lv: &LayoutView, i: u32) -> u32 {
        self.word(lv.section_word + i)
    }

    fn setting_word(&self, lv: Option<&LayoutView>, mode: u32, key: usize) -> u32 {
        match lv {
            Some(lv) if key < lv.num_keys as usize => self.section_word(
                lv,
                lv.actuations_word_off + mode * lv.num_keys as u32 + key as u32,
            ),
            _ => 0,
        }
    }

    fn keycode_word(&self, lv: &LayoutView, layer: u32, key: usize) -> u32 {
        self.section_word(
            lv,
            lv.keycodes_word_off + layer * lv.num_keys as u32 + key as u32,
        )
    }

    /// The pool word `i` of `lv`'s macros (offset table, lengths and steps).
    fn macro_word(&self, lv: &LayoutView, i: u32) -> u32 {
        self.section_word(lv, 7 + i)
    }

    /// Restarts signal history after a calibration, without releasing held actions.
    pub fn reset_tracking(&mut self) {
        self.depth_rows = [DepthRow::default(); ROWS];
        self.refine_rows = [RefineRow::default(); ROWS];
        for k in 0..MAX_KEYS {
            if !self.key_pressed[k] {
                self.rapid[k].rearm();
                self.fixed_transition_scans[k] = 0;
            }
        }
    }

    pub fn install_refinement(&mut self, payload: &[u8; MODEL_LEN]) {
        self.refinement = Some(Model::decode(payload));
        self.refinement_active = true;
        self.reset_tracking();
    }

    /// Records the attached keyframe (layout 1 ANSI, 2 ISO, 3 JIS) and resets key state if it
    /// changed. A layout out of range while attached is ignored.
    pub fn set_keyframe(&mut self, attached: bool, layout: u8, feel: u8) {
        let (layout, feel) = if attached { (layout, feel) } else { (0, 0) };
        if attached && !(1..=3).contains(&layout) {
            return;
        }
        if self.keyframe_attached == attached
            && self.keyframe_layout == layout
            && self.switch_feel == feel
        {
            return;
        }
        self.keyframe_attached = attached;
        self.keyframe_layout = layout;
        self.lifecycle.attach_phase = if attached { 3 } else { 0 };
        self.switch_feel = feel;
        self.rest_ref_valid = false;
        self.lifecycle.calibrated = false;
        self.classic_setup = ClassicSetup::Attach;
        self.classic_keys = [Keys::default(); ROWS];
        self.classic_neighbors = [Neighbors::default(); ROWS];
        self.key_pressed = [false; MAX_KEYS];
        self.latched_valid = [false; MAX_KEYS];
        self.latched_action = [0; MAX_KEYS];
        self.rapid = [RapidState::default(); MAX_KEYS];
        self.motion = [KeyMotion::default(); MAX_KEYS];
        self.neighbor_vetoes = [0; MAX_KEYS];
        self.key_bitmap = [0; 32];
        self.flux = [0; MAX_KEYS];
        self.effective_layer_bitfield = self.base_layers() | self.toggled_layer_bitfield;
        self.reset_tracking();
    }

    fn base_layers(&self) -> u32 {
        if self.active_layer_bitfield != 0 {
            self.active_layer_bitfield
        } else {
            1
        }
    }

    pub fn set_time(&mut self, now_ms: u32) {
        self.macro_clock_ms = now_ms;
        self.macro_clock_updated = true;
    }

    /// Tells macro playback whether the keyboard endpoint can take a report this scan.
    pub fn set_hid_ready(&mut self, ready: bool) {
        self.macro_hid_ready = ready;
    }

    fn reload_actuation(&mut self) {
        if self.classic_setup == ClassicSetup::Ready {
            self.classic_setup = ClassicSetup::Reload;
        }
        self.fixed_transition_scans = [0; MAX_KEYS];
        for r in &mut self.rapid {
            r.rearm();
        }
    }

    /// Checks and loads a keymap blob. Leaves the runtime alone when it doesn't parse.
    pub fn load_keymap(
        &mut self,
        blob: &[u8],
    ) -> Result<(), glassdeck_scanner_proto::keymap::KeymapError> {
        let keymap = Keymap::parse(blob)?;
        let mut layouts = [None; 4];
        for layout in [
            KeyframeLayout::Ansi,
            KeyframeLayout::Iso,
            KeyframeLayout::Jis,
        ] {
            if let Some(s) = keymap.section(layout) {
                let kc = 7 + s.pool_words();
                layouts[layout as usize] = Some(LayoutView {
                    num_act_modes: s.modes() as u16,
                    num_keys: s.keys() as u16,
                    num_layers: s.layers() as u16,
                    num_macros: s.macro_count() as u16,
                    macro_pool_words: s.pool_words(),
                    keycodes_word_off: kc,
                    actuations_word_off: kc + s.keys() * s.layers(),
                    section_word: s.start_word(),
                });
            }
        }
        self.keymap_blob[..blob.len()].copy_from_slice(blob);
        self.toggled_layer_bitfield = 0;
        self.macro_active = false;
        self.macro_nkro_bitmap = [0; 32];
        self.reload_actuation();
        if self.keyframe_attached {
            self.classic_setup = ClassicSetup::Replaced;
        }
        self.keymap_len = blob.len();
        self.layouts = layouts;
        Ok(())
    }

    pub fn load_profile_settings(&mut self, upload: &[u8]) -> Result<(), ProfileError> {
        let settings = ProfileSettings::parse_upload(upload)?;
        self.num_profiles = settings.profiles().len() as u32;
        self.profiles[..settings.profiles().len()].copy_from_slice(settings.profiles());
        self.set_active_profile(self.active_profile);
        Ok(())
    }

    pub fn profile_settings(&self) -> ProfileSettings {
        ProfileSettings::new(&self.profiles[..self.num_profiles as usize])
            .expect("at most MAX_PROFILES are stored")
    }

    /// Selects a profile's actuation mode and default layers. Like the stock firmware, an index
    /// past the stored profiles is kept but selects nothing.
    pub fn set_active_profile(&mut self, index: u32) {
        self.active_profile = index;
        self.toggled_layer_bitfield = 0;
        if index < self.num_profiles {
            let p = self.profiles[index as usize];
            self.active_actuation_mode = p.actuation_mode;
            if p.default_layers != 0 {
                self.active_layer_bitfield = p.default_layers;
            }
        }
        self.effective_layer_bitfield = self.base_layers();
    }

    /// Handles a function key; returns whether the active profile changed.
    fn handle_function(&mut self, lv: &LayoutView, code: u8) -> bool {
        match code {
            function::PROFILE_CYCLE_NEXT | function::PROFILE_CYCLE_PREV
                if self.num_profiles > 0 =>
            {
                let n = self.num_profiles;
                let next = if code == function::PROFILE_CYCLE_NEXT {
                    (self.active_profile.wrapping_add(1)) % n
                } else {
                    (self.active_profile.wrapping_add(n - 1)) % n
                };
                self.set_active_profile(next);
                true
            }
            function::ACTUATION_MODE_NEXT | function::ACTUATION_MODE_PREV
                if lv.num_act_modes > 0 =>
            {
                let n = lv.num_act_modes as u32;
                self.active_actuation_mode = if code == function::ACTUATION_MODE_NEXT {
                    (self.active_actuation_mode.wrapping_add(1)) % n
                } else {
                    (self.active_actuation_mode.wrapping_add(n - 1)) % n
                };
                false
            }
            function::PLATFORM_LINUX | function::PLATFORM_MACOS | function::PLATFORM_WINDOWS => {
                let platform = match code {
                    function::PLATFORM_LINUX => 3,
                    function::PLATFORM_MACOS => 2,
                    _ => 1,
                };
                if self.platform_code != platform {
                    self.platform_code = platform;
                    self.platform_dirty = true;
                }
                false
            }
            _ => false,
        }
    }

    /// A marginal transition during a chord needs a longer unbroken run of evidence. Uses the held
    /// count from the start of the scan, so key order can't change it.
    #[allow(clippy::too_many_arguments)]
    fn confirm_fixed_key(
        &mut self,
        k: usize,
        flux: i32,
        act: u16,
        release: i32,
        held_count: usize,
        neighbor_delta: i32,
    ) {
        let down = self.key_pressed[k];
        let proposed = fixed_candidate(
            down,
            flux,
            act as i32,
            release,
            neighbor_delta,
            &self.motion[k],
            &mut self.neighbor_vetoes[k],
        );
        if proposed == down {
            self.fixed_transition_scans[k] = 0;
            return;
        }
        let strong = if down {
            100 * flux < 75 * release
        } else {
            100 * flux > 175 * act as i32
        };
        let required = if held_count > 2 && !strong { 17 } else { 3 };
        self.fixed_transition_scans[k] += 1;
        if self.fixed_transition_scans[k] >= required {
            self.key_pressed[k] = proposed;
            self.fixed_transition_scans[k] = 0;
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn guard_flux(
        &mut self,
        row: usize,
        col: usize,
        flux: u16,
        gain_q8: u16,
        down: bool,
        rapid: bool,
        peak: u16,
    ) -> u16 {
        let l = &mut self.lifecycle;
        if !l.calibrated {
            return flux;
        }
        let mask = l.rest.inhibit_all_new || l.rest.inhibit[row][col];
        guard_key_signal(
            flux,
            gain_q8,
            down,
            rapid,
            peak,
            l.thermal.require_strong,
            &mut l.thermal.protect_held[row][col],
            mask,
        )
    }

    fn observe_key(
        &mut self,
        row: usize,
        col: usize,
        tracked_flux: u16,
        down: bool,
        proposed: bool,
    ) {
        self.lifecycle.observed[row][col] = ThermalKey {
            flux: tracked_flux,
            down,
            pending: proposed,
            masked: false,
        };
    }

    #[allow(clippy::too_many_arguments)]
    fn evaluate_key(
        &mut self,
        k: usize,
        layout: u8,
        row: usize,
        col: usize,
        row_keys: usize,
        word: u32,
        held_count: usize,
        row_active: u8,
    ) {
        let kind = word >> 30;
        let has_setting = kind == 1 || kind == 2;
        let gain = position_gain(layout, row, col);
        let act = (((if has_setting {
            word & 0xFFF
        } else {
            DEFAULT_ACTUATION
        }) * gain as u32)
            >> 8) as u16;
        let reversal = if has_setting {
            ((word >> 12) & 0xFFF) as u16
        } else {
            DEFAULT_REVERSAL as u16
        };
        let reset = ((reversal as u32 * gain as u32) >> 8) as u16;
        let flux = self.guard_flux(
            row,
            col,
            self.flux[k],
            gain,
            self.key_pressed[k],
            kind == 2,
            self.rapid[k].peak,
        );
        let nd = neighbor_delta(&self.motion[k - col..k - col + row_keys], col);
        if kind == 2 {
            let input = RapidInput {
                flux,
                actuation: act,
                hysteresis: reset,
                sensitivity: reversal,
                weight_q8: gain,
                row_active,
                motion: &self.motion[k],
                neighbor_delta: nd,
            };
            let down = self.key_pressed[k];
            self.key_pressed[k] = self.rapid[k].step(down, &input);
        } else {
            self.confirm_fixed_key(
                k,
                flux as i32,
                act,
                act as i32 - reset as i32,
                held_count,
                nd,
            );
        }
        let down = self.key_pressed[k];
        let proposed = if kind != 2 && self.fixed_transition_scans[k] != 0 {
            !down
        } else {
            down
        };
        self.observe_key(row, col, self.flux[k], down, proposed);
        if down {
            set_bit(&mut self.key_bitmap, k);
        }
    }

    /// The classic tracker over every row: conditioning, neighbor correction, then decisions
    /// against the previous scan's occupancy. Returns the keys evaluated.
    fn scan_classic(&mut self, lv: Option<LayoutView>, mode: u32, stronger: bool) -> usize {
        let layout = self.keyframe_layout;
        let replaced = self.classic_setup == ClassicSetup::Replaced;
        let table_mode = if replaced { 0 } else { mode };
        let held = self.key_pressed.iter().filter(|&&p| p).count();
        let mut base = 0usize;
        for r in 0..ROWS {
            let width = ROW_LEN[layout as usize][r] as usize;
            if !(2..=MAX_COLS).contains(&width) {
                continue;
            }
            let n = width - 1;
            let mut field = [0i32; MAX_COLS];
            let mut travel = [0i32; MAX_ROW_KEYS];
            for c in 0..width {
                let s = SENSOR_MAP[layout as usize][r][c] as usize;
                field[c] = if s < SENSORS {
                    self.sensor_scaled[s]
                } else {
                    0
                };
            }
            if self.classic_setup != ClassicSetup::Ready {
                for c in 0..n {
                    let k = base + c;
                    let has_table = lv.is_some_and(|lv| k < lv.num_keys as usize);
                    let word = self.setting_word(lv.as_ref(), table_mode, k);
                    self.classic_keys[r].load(
                        &mut self.classic_settings[r],
                        layout,
                        r,
                        c,
                        has_table,
                        word,
                    );
                    // Changing the active table while attached reapplies its position gain.
                    if self.classic_setup != ClassicSetup::Attach {
                        self.classic_settings[r].weight(layout, r, c);
                    }
                }
            }
            classic::condition_row(r, &mut field[..width], &mut travel[..n]);
            for t in &mut travel[..n] {
                *t = apply_feel(*t, stronger);
            }
            let cfg = self.classic_settings[r];
            let thresholds = Some(Thresholds {
                actuation: &cfg.actuation,
                hysteresis: &cfg.hysteresis,
            });
            let down = self.classic_keys[r].down;
            self.classic_neighbors[r].correct(
                layout,
                r,
                &field[..width],
                &mut travel[..n],
                &down,
                thresholds,
            );
            for c in 0..n {
                let f = report_flux(travel[c]);
                self.flux[base + c] = f;
                let keys = &self.classic_keys[r];
                let (down, rapid, peak) = (
                    keys.down[c],
                    cfg.rapid[c],
                    report_flux(keys.reversal[c].peak),
                );
                let guarded =
                    self.guard_flux(r, c, f, position_gain(layout, r, c), down, rapid, peak);
                if guarded != f {
                    travel[c] = guarded as i32;
                }
            }
            self.classic_keys[r].step(&cfg, r, held, &travel[..n]);
            for c in 0..n {
                let k = base + c;
                let (down, pending) = (
                    self.classic_keys[r].down[c],
                    self.classic_keys[r].pending[c],
                );
                self.key_pressed[k] = down;
                if down {
                    set_bit(&mut self.key_bitmap, k);
                }
                self.observe_key(r, c, self.flux[k], down, pending);
            }
            base += n;
        }
        self.classic_setup = if replaced && mode != 0 {
            ClassicSetup::Reload
        } else {
            ClassicSetup::Ready
        };
        base
    }

    /// The refined path: line fit, depth and refinement per row, then a decision per key.
    fn scan_refined(&mut self, lv: Option<LayoutView>, act_mode: u32, stronger: bool) -> usize {
        let layout = self.keyframe_layout;
        let model = self
            .refinement
            .expect("refinement is active only with a model");
        let mut logical_k = 0;
        for row in 0..ROWS {
            let rlen = ROW_LEN[layout as usize][row] as usize;
            if rlen <= 1 || rlen > MAX_COLS {
                continue;
            }
            let mut rvals = [0i32; MAX_COLS];
            for c in 0..rlen {
                let s = SENSOR_MAP[layout as usize][row][c] as usize;
                rvals[c] = if s < SENSORS {
                    self.sensor_scaled[s]
                } else {
                    0
                };
            }
            condition_row_q4(&mut rvals[..rlen]);
            let mut travel = [0i32; MAX_ROW_KEYS];
            self.depth_rows[row].update(&rvals[..rlen], &mut travel[..rlen - 1]);
            for v in &mut rvals[..rlen] {
                *v /= 16;
            }
            self.refine_rows[row].step(&model, &rvals[..rlen], &mut travel[..rlen - 1]);
            for c in 0..rlen - 1 {
                self.flux[logical_k] = report_flux(apply_feel(travel[c], stronger));
                logical_k += 1;
            }
        }
        let n_keys = logical_k;
        let held_count = self.key_pressed[..n_keys].iter().filter(|&&p| p).count();
        let mut k = 0;
        for row in 0..ROWS {
            let len = ROW_LEN[layout as usize][row] as usize;
            if len <= 1 || len > MAX_COLS || k >= n_keys {
                continue;
            }
            let row_keys = (len - 1).min(n_keys - k);
            update_row_motion(
                &mut self.motion[k..k + row_keys],
                &self.flux[k..k + row_keys],
            );
            let row_active = (0..row_keys)
                .filter(|&c| self.key_pressed[k + c] || self.fixed_transition_scans[k + c] != 0)
                .count() as u8;
            for col in 0..row_keys {
                let word = self.setting_word(lv.as_ref(), act_mode, k);
                self.evaluate_key(k, layout, row, col, row_keys, word, held_count, row_active);
                k += 1;
            }
        }
        n_keys
    }

    /// Looks a newly pressed key up from the highest active layer down; transparent falls through.
    fn lookup(&self, lv: &LayoutView, lookup_mask: u32, k: usize) -> u32 {
        for l in (0..lv.num_layers as u32).rev() {
            if (lookup_mask >> l) & 1 == 0 {
                continue;
            }
            let candidate = self.keycode_word(lv, l, k);
            if candidate != TRANSPARENT {
                return candidate;
            }
        }
        0
    }

    fn clear_actions(&mut self) {
        self.latched_valid = [false; MAX_KEYS];
        self.latched_action = [0; MAX_KEYS];
        self.macro_active = false;
        self.macro_nkro_bitmap = [0; 32];
        self.effective_layer_bitfield = self.base_layers() | self.toggled_layer_bitfield;
    }

    /// One scan pass over all 160 readings: key states, layers, macros and the HID report.
    pub fn process_scan(&mut self, raw_adc: &[u16; SENSORS]) -> HidReport {
        self.raw_adc = *raw_adc;
        let mut out = HidReport::default();
        self.key_bitmap = [0; 32];
        self.flux = [0; MAX_KEYS];
        if !self.keyframe_attached {
            self.fixed_transition_scans = [0; MAX_KEYS];
            self.key_pressed = [false; MAX_KEYS];
            self.rapid = [RapidState::default(); MAX_KEYS];
            self.motion = [KeyMotion::default(); MAX_KEYS];
            self.neighbor_vetoes = [0; MAX_KEYS];
            self.clear_actions();
            return out;
        }
        if !self.macro_clock_updated {
            self.macro_clock_ms = self.macro_clock_ms.wrapping_add(1);
        }
        self.macro_clock_updated = false;

        // A signed calibrated change per physical sensor, with a ±2 count dead band.
        for s in 0..SENSORS {
            let d = raw_adc[s] as i32 - self.baseline_adc[s] as i32;
            let scaled = (d * self.cal.factors[s] as i32) >> 8;
            self.sensor_scaled[s] = match scaled {
                -2..=2 => 0,
                3.. => scaled - 3,
                _ => scaled + 3,
            };
        }
        let lid = self.keyframe_layout;
        if !(1..=3).contains(&lid) {
            return out;
        }
        let lv = self.layouts[lid as usize];
        let mut act_mode = self.active_actuation_mode;
        if let Some(lv) = lv
            && act_mode >= lv.num_act_modes as u32
        {
            act_mode = 0;
        }
        if self.last_scan_mode as u32 != act_mode {
            self.reload_actuation();
            self.last_scan_mode = act_mode as u16;
        }
        let stronger = self.switch_feel == 2 && self.cal.loaded_from_flash && self.cal.version == 2;
        let mut n_keys = if self.refinement_active {
            self.scan_refined(lv, act_mode, stronger)
        } else {
            self.scan_classic(lv, act_mode, stronger)
        };

        let Some(lv) = lv else {
            self.clear_actions();
            return out;
        };
        n_keys = n_keys.min(lv.num_keys as usize);

        // Releases free their latched actions; held momentary layer keys collect their layers.
        let mut eff_mask = self.base_layers() | self.toggled_layer_bitfield;
        let mut held_layer_mask = 0u32;
        for k in 0..n_keys {
            if !self.key_pressed[k] {
                self.latched_valid[k] = false;
                self.latched_action[k] = 0;
            } else if self.latched_valid[k] {
                let kc = self.latched_action[k];
                if (kc >> 28) & 0xF == 0x2 {
                    let (switch, target) = ((kc >> 24) & 0xF, kc & 0xFF);
                    if switch <= 1 && target < lv.num_layers as u32 {
                        held_layer_mask |= 1 << target;
                        eff_mask |= 1 << target;
                    }
                }
            }
        }

        // Newly pressed layer keys latch first, until no more change (a key never chains off itself),
        // then the other newly pressed keys latch against the complete layer mask.
        let mut lookup_mask = eff_mask | 1;
        for _pass in 0..lv.num_layers {
            let mut changed = false;
            for k in 0..n_keys {
                if !self.key_pressed[k] || self.latched_valid[k] {
                    continue;
                }
                let kc = self.lookup(&lv, lookup_mask, k);
                if (kc >> 28) & 0xF == 0x2 {
                    self.latched_action[k] = kc;
                    self.latched_valid[k] = true;
                    let (switch, target) = ((kc >> 24) & 0xF, kc & 0xFF);
                    if target < lv.num_layers as u32 {
                        match switch {
                            2 => self.toggled_layer_bitfield ^= 1 << target,
                            3 => {
                                self.active_layer_bitfield = 1 << target;
                                self.toggled_layer_bitfield = 0;
                            }
                            4 => {
                                self.toggled_layer_bitfield =
                                    if self.toggled_layer_bitfield & (1 << target) != 0 {
                                        self.toggled_layer_bitfield & !(1 << target)
                                    } else {
                                        1 << target
                                    };
                            }
                            _ => held_layer_mask |= 1 << target,
                        }
                        eff_mask =
                            self.base_layers() | self.toggled_layer_bitfield | held_layer_mask;
                        lookup_mask = eff_mask | 1;
                    }
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        for k in 0..n_keys {
            if !self.key_pressed[k] || self.latched_valid[k] {
                continue;
            }
            let kc = self.lookup(&lv, lookup_mask, k);
            self.latched_action[k] = kc;
            self.latched_valid[k] = true;
            match (kc >> 28) & 0xF {
                0x1 => {
                    let id = (kc & 0xFFFF) as u16;
                    if id < lv.num_macros {
                        let at = self.macro_word(&lv, id as u32);
                        let len = self.macro_word(&lv, at);
                        self.macro_nkro_bitmap = [0; 32];
                        if len > 0 {
                            let first = self.macro_word(&lv, at + 1);
                            self.macro_active = true;
                            self.macro_layout = lid;
                            self.macro_id = id;
                            self.macro_step_idx = 0;
                            self.macro_next_ms =
                                self.macro_clock_ms.wrapping_add((first >> 8) & 0xFFFF);
                        } else {
                            self.macro_active = false;
                        }
                    }
                }
                0x5 => {
                    let code = (kc & 0xFF) as u8;
                    if self.handle_function(&lv, code) {
                        eff_mask =
                            self.base_layers() | self.toggled_layer_bitfield | held_layer_mask;
                    }
                    if code > 0 && code < 32 {
                        let slot = (self.of_event_seq & 15) as usize;
                        self.of_event_ring[slot] = code;
                        self.of_event_profiles[slot] = self.active_profile as u8;
                        self.of_event_seq = self.of_event_seq.wrapping_add(1);
                    }
                }
                _ => {}
            }
        }
        self.effective_layer_bitfield = eff_mask;
        self.advance_macro(&lv);
        self.build_report(&lv, n_keys, &mut out);
        out
    }

    /// Plays at most one macro step a scan, and only when the keyboard endpoint can send this scan's
    /// report, so no press or release in between is lost.
    fn advance_macro(&mut self, lv: &LayoutView) {
        if !self.macro_active {
            return;
        }
        if self.macro_layout != self.keyframe_layout || self.macro_id >= lv.num_macros {
            self.macro_active = false;
            self.macro_nkro_bitmap = [0; 32];
            return;
        }
        if !self.macro_hid_ready {
            return;
        }
        let at = self.macro_word(lv, self.macro_id as u32);
        let len = self.macro_word(lv, at);
        if self.macro_step_idx >= len {
            self.macro_active = false;
            self.macro_nkro_bitmap = [0; 32];
        } else if self.macro_clock_ms.wrapping_sub(self.macro_next_ms) as i32 >= 0 {
            let step = self.macro_word(lv, at + 1 + self.macro_step_idx);
            let usage = (step & 0xFF) as usize;
            if usage > 0 {
                if step >> 31 != 0 {
                    set_bit(&mut self.macro_nkro_bitmap, usage);
                } else {
                    self.macro_nkro_bitmap[usage >> 3] &= !(1 << (usage & 7));
                }
            }
            self.macro_step_idx += 1;
            if self.macro_step_idx < len {
                let next = self.macro_word(lv, at + 1 + self.macro_step_idx);
                self.macro_next_ms = self.macro_clock_ms.wrapping_add((next >> 8) & 0xFFFF);
            }
        }
    }

    fn build_report(&self, _lv: &LayoutView, n_keys: usize, out: &mut HidReport) {
        let mut nk = 0;
        for k in 0..n_keys {
            if !self.key_pressed[k] || !self.latched_valid[k] {
                continue;
            }
            let kc = self.latched_action[k];
            if kc == DISABLED || kc == 0 {
                continue;
            }
            match (kc >> 28) & 0xF {
                0x0 => {
                    let hid = (kc & 0xFFFF) as usize;
                    let mods = ((kc >> 16) & 0xFF) as u8;
                    out.modifiers |= mods;
                    for bit in 0..8 {
                        if (mods >> bit) & 1 != 0 {
                            set_bit(&mut out.nkro_bitmap, 0xE0 + bit);
                        }
                    }
                    if (0xE0..=0xE7).contains(&hid) {
                        out.modifiers |= 1 << (hid - 0xE0);
                        set_bit(&mut out.nkro_bitmap, hid);
                    } else if hid > 0 && hid <= 0xFF {
                        set_bit(&mut out.nkro_bitmap, hid);
                        if hid < 0xE0 && nk < 6 {
                            out.keycodes[nk] = hid as u8;
                            nk += 1;
                        }
                    }
                }
                0x4 if out.consumer_usage == 0 => out.consumer_usage = (kc & 0xFFFF) as u16,
                0x5 => {
                    let sub = kc & 0xFF;
                    if sub > 0 && sub < 32 {
                        out.of_func_mask |= 1 << sub;
                    }
                }
                _ => {}
            }
        }
        if self.macro_active {
            for u in 1..=0xFFusize {
                if self.macro_nkro_bitmap[u >> 3] & (1 << (u & 7)) == 0 {
                    continue;
                }
                set_bit(&mut out.nkro_bitmap, u);
                if (0xE0..=0xE7).contains(&u) {
                    out.modifiers |= 1 << (u - 0xE0);
                } else if u < 0xE0 && nk < 6 && !out.keycodes[..nk].contains(&(u as u8)) {
                    out.keycodes[nk] = u as u8;
                    nk += 1;
                }
            }
        }
    }

    /// The key-state message, which bumps the message sequence number.
    pub fn key_state(&mut self, timestamp_ms: u32) -> KeyState {
        self.uart_tx_seq = self.uart_tx_seq.wrapping_add(1);
        let layers = if self.effective_layer_bitfield != 0 {
            self.effective_layer_bitfield
        } else {
            self.base_layers()
        };
        KeyState {
            seq: self.uart_tx_seq,
            pressed: self.key_bitmap,
            uptime_ms: timestamp_ms,
            keyframe_layout: match self.keyframe_layout {
                1 => Some(KeyframeLayout::Ansi),
                2 => Some(KeyframeLayout::Iso),
                3 => Some(KeyframeLayout::Jis),
                _ => None,
            },
            keyframe_attached: self.keyframe_attached,
            switch_feel: self.switch_feel,
            active_layers: layers,
            actuation_mode: self.active_actuation_mode,
            function_events: Some(FunctionEvents {
                seq: self.of_event_seq,
                codes: self.of_event_ring,
                profiles: self.of_event_profiles,
            }),
            host_leds: self.host_state_status,
            language_code: self.language_code,
            platform_code: self.platform_code,
            active_profile: self.active_profile,
            keyframe_phase: match self.lifecycle.attach_phase {
                0 => KeyframePhase::Absent,
                1 => KeyframePhase::FieldDetected,
                2 => KeyframePhase::IdsSettling,
                3 => KeyframePhase::Attached,
                _ => KeyframePhase::Recovering,
            },
            module_types: 0,
            module_buttons: 0,
        }
    }

    /// The sensor-readings message, which bumps the message sequence number.
    pub fn sensor_readings(&mut self) -> SensorReadings {
        self.uart_tx_seq = self.uart_tx_seq.wrapping_add(1);
        SensorReadings {
            seq: self.uart_tx_seq,
            raw: self.raw_adc,
        }
    }

    /// Applies the sensor-mode and stress-test flags of a response to a scanner message.
    pub fn handle_response(&mut self, response: &serial::Response) {
        self.qc_mode = response.sensor_mode;
        self.stress_test_enabled = response.stress_test;
    }

    /// Runs on every sweep. Returns true when the frame changed to an attached one and needs a new
    /// resting measurement, after loading the rest record for the reported layout and feel.
    pub fn poll_frame(&mut self, raw: &[u16; SENSORS], now_ms: u32) -> bool {
        let last_poll = self.lifecycle.detector.last_poll_ms;
        self.lifecycle.detector.stress_frame = self.stress_test_enabled;
        let key_raw: &[u16; KEY_SENSORS] = raw[..KEY_SENSORS].try_into().unwrap();
        let frame = self.lifecycle.detector.poll(key_raw, now_ms);
        if last_poll != 0 && self.lifecycle.detector.last_poll_ms == last_poll {
            return false;
        }
        let changed = frame.changed
            || self.keyframe_attached != frame.attached
            || self.keyframe_layout != frame.layout
            || self.switch_feel != frame.feel;
        if changed {
            self.set_keyframe(frame.attached, frame.layout, frame.feel);
        }
        let l = &mut self.lifecycle;
        l.attach_phase = if frame.attached && l.calibrated && l.rest.inhibit_all_new {
            4
        } else {
            frame.stage
        };
        changed && frame.attached
    }

    /// Takes a fresh resting measurement: `mean` over eight sweeps, and the channels whose range
    /// was over 40.
    pub fn calibrate(
        &mut self,
        mean: &[u16; SENSORS],
        unstable: &[bool; KEY_SENSORS],
        now_ms: u32,
        bridge_connected: bool,
    ) {
        self.baseline_adc = *mean;
        if !self.keyframe_attached {
            self.lifecycle.calibrated = false;
            return;
        }
        let reference = self.rest_ref_valid.then_some(&self.rest_ref_adc);
        let guard = rest::apply(
            self.keyframe_layout,
            &mut self.baseline_adc,
            Some(unstable),
            reference,
        );
        let l = &mut self.lifecycle;
        l.thermal.acquired(&self.baseline_adc);
        l.rest = RestSession::begin(
            self.keyframe_layout,
            &guard,
            bridge_connected,
            self.qc_mode,
            l.thermal.refresh_count,
            now_ms,
        );
        l.observed = [[ThermalKey::default(); 16]; ROWS];
        self.reset_tracking();
        self.lifecycle.calibrated = true;
    }

    /// Finishes an attached scan after the key decisions. Returns true only when `rest_ref_adc` is
    /// ready to save for this layout and feel. Does no flash I/O.
    pub fn finish_scan(&mut self, now_ms: u32) -> bool {
        if !self.lifecycle.calibrated || !self.keyframe_attached {
            return false;
        }
        let l = &mut self.lifecycle;
        let mut any_pressed = false;
        for row in 0..ROWS {
            for col in 0..16 {
                let key = &mut l.observed[row][col];
                key.masked = l.rest.inhibit[row][col];
                l.confirmed[row][col] = key.down;
                l.proposed[row][col] = key.pending;
                any_pressed |= key.down;
            }
        }
        // Accessory module fields aren't detected.
        let observed = l.observed;
        if l.thermal.after_scan(
            self.keyframe_layout,
            now_ms,
            &self.raw_adc,
            &mut self.baseline_adc,
            &observed,
            false,
        ) {
            self.reset_tracking();
        }
        let l = &mut self.lifecycle;
        let busy = l.thermal.require_strong || l.thermal.moving;
        let generation = l.thermal.refresh_count;
        let (confirmed, proposed) = (l.confirmed, l.proposed);
        l.rest.recover(
            now_ms,
            &self.raw_adc,
            &self.sensor_scaled,
            &confirmed,
            &proposed,
            busy,
            generation,
            &mut self.baseline_adc,
        );
        // A shorter or replaced keymap mustn't keep observations of keys it no longer evaluates.
        l.observed = [[ThermalKey::default(); 16]; ROWS];
        if !l
            .rest
            .idle(now_ms, any_pressed, &self.raw_adc, &mut self.baseline_adc)
        {
            return false;
        }
        for i in 0..KEY_SENSORS {
            self.rest_ref_adc[i] = self.baseline_adc[i] as i16;
        }
        self.rest_ref_valid = true;
        true
    }
}

/// Sends at most one keyboard or consumer report per ready opportunity on the keyboard endpoint,
/// and drains the vendor-HID function queue whenever its own endpoint is ready.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HidScheduler {
    pub initialized: bool,
    pub of_initialized: bool,
    pub last_modifiers: u8,
    pub last_keycodes: [u8; 6],
    pub last_nkro_bitmap: [u8; 32],
    pub last_consumer_usage: u16,
    pub last_of_func_mask: u32,
    /// The latest 16 function-mask states waiting for the vendor endpoint.
    pub of_queue: [u32; 16],
    pub of_q_head: u8,
    pub of_q_tail: u8,
}

/// The endpoints a [`HidScheduler`] writes to. Each returns whether the report was taken.
pub trait HidSink {
    fn keyboard(&mut self, modifiers: u8, keycodes: &[u8; 6], nkro_bitmap: &[u8; 32]) -> bool;
    fn consumer(&mut self, usage: u16) -> bool;
    fn functions(&mut self, mask: u32) -> bool;
}

impl HidScheduler {
    pub fn step(
        &mut self,
        desired: &HidReport,
        ep_ready: bool,
        of_ep_ready: bool,
        sink: &mut impl HidSink,
    ) {
        let queued = self.of_q_tail.wrapping_sub(self.of_q_head);
        let last_queued = if queued > 0 {
            self.of_queue[(self.of_q_tail.wrapping_sub(1) & 15) as usize]
        } else {
            self.last_of_func_mask
        };
        if !self.of_initialized || last_queued != desired.of_func_mask {
            if queued >= 16 {
                self.of_q_head = self.of_q_head.wrapping_add(1);
            }
            self.of_queue[(self.of_q_tail & 15) as usize] = desired.of_func_mask;
            self.of_q_tail = self.of_q_tail.wrapping_add(1);
            self.of_initialized = true;
        }
        if of_ep_ready && self.of_q_head != self.of_q_tail {
            let mask = self.of_queue[(self.of_q_head & 15) as usize];
            if sink.functions(mask) {
                self.last_of_func_mask = mask;
                self.of_q_head = self.of_q_head.wrapping_add(1);
            }
        }
        if !ep_ready {
            return;
        }
        let kbd_dirty = !self.initialized
            || self.last_modifiers != desired.modifiers
            || self.last_keycodes != desired.keycodes
            || self.last_nkro_bitmap != desired.nkro_bitmap;
        let cons_dirty = !self.initialized || self.last_consumer_usage != desired.consumer_usage;
        if kbd_dirty {
            if sink.keyboard(desired.modifiers, &desired.keycodes, &desired.nkro_bitmap) {
                self.last_modifiers = desired.modifiers;
                self.last_keycodes = desired.keycodes;
                self.last_nkro_bitmap = desired.nkro_bitmap;
                self.initialized = true;
            }
            // The keyboard endpoint is busy with this report; the consumer update waits a tick.
            return;
        }
        if cons_dirty && sink.consumer(desired.consumer_usage) {
            self.last_consumer_usage = desired.consumer_usage;
            self.initialized = true;
        }
    }
}
