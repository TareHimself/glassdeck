//! The keymap blob (`SEND_KEYMAP`, `GET_KEYMAP`) and profile settings (`FILE_FROM_HOST`,
//! `FILE_TO_HOST`), as the stock firmware checks them. All words are little-endian `u32`s.
//!
//! The firmware accepts a little more than Keyglass's `keymap-format.md` says, and so does this
//! parser: words after the last section, layout-mask bits above JIS, and any value in a
//! section's second macro count.

use crate::serial::KeyframeLayout;

pub const MAX_BLOB: usize = 16384;
pub const MAX_LAYERS: u32 = 8;
pub const MAX_KEYS: u32 = 96;
pub const MAX_MODES: u32 = 8;
pub const MAX_MACROS: u32 = 512;
pub const MAX_PROFILES: usize = 64;

const HEADER_WORDS: usize = 6;
const SECTION_WORDS: usize = 7;
const LAYOUTS: [KeyframeLayout; 3] = [
    KeyframeLayout::Ansi,
    KeyframeLayout::Iso,
    KeyframeLayout::Jis,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeymapError {
    /// Shorter than the header, longer than [`MAX_BLOB`], or not whole words.
    Length(usize),
    LayoutCount(u32),
    /// The header's layout count doesn't match its layout mask.
    CountMismatch,
    /// Header word 2 isn't 0.
    ReservedWord,
    /// A layout absent from the mask has a section offset.
    AbsentWithOffset(KeyframeLayout),
    /// A section doesn't start where the previous one ends.
    SectionOffset(KeyframeLayout),
    Truncated(KeyframeLayout),
    Count {
        layout: KeyframeLayout,
        field: &'static str,
        value: u32,
    },
    KeycodesOffset(KeyframeLayout),
    ActuationOffset(KeyframeLayout),
    /// A macro's offset or length doesn't fit the macro pool.
    Macro {
        layout: KeyframeLayout,
        index: u32,
    },
    /// The macros don't fill the pool exactly.
    MacroPool(KeyframeLayout),
}

/// A keymap blob that passed every check the firmware makes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Keymap<'a> {
    sections: [Option<Section<'a>>; 3],
}

impl<'a> Keymap<'a> {
    pub fn parse(blob: &'a [u8]) -> Result<Self, KeymapError> {
        if blob.len() < HEADER_WORDS * 4 || blob.len() > MAX_BLOB || !blob.len().is_multiple_of(4) {
            return Err(KeymapError::Length(blob.len()));
        }
        let count = word(blob, 0);
        let mask = word(blob, 1);
        if count == 0 || count > 3 {
            return Err(KeymapError::LayoutCount(count));
        }
        let mut sections = [None; 3];
        let mut present = 0;
        let mut next = 0u32;
        for (i, layout) in LAYOUTS.into_iter().enumerate() {
            let offset = word(blob, 3 + i);
            if mask & (1 << i) == 0 {
                if offset != 0 {
                    return Err(KeymapError::AbsentWithOffset(layout));
                }
                continue;
            }
            if offset != next {
                return Err(KeymapError::SectionOffset(layout));
            }
            let start = (HEADER_WORDS + offset as usize) * 4;
            let (section, words) = Section::parse(layout, &blob[start..], (start / 4) as u32)?;
            sections[i] = Some(section);
            present += 1;
            next += words;
        }
        if present != count {
            return Err(KeymapError::CountMismatch);
        }
        if word(blob, 2) != 0 {
            return Err(KeymapError::ReservedWord);
        }
        Ok(Self { sections })
    }

    pub fn section(&self, layout: KeyframeLayout) -> Option<&Section<'a>> {
        self.sections[layout as usize - 1].as_ref()
    }
}

/// One layout's layers, actuation modes and macros.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Section<'a> {
    start_word: u32,
    layers: u32,
    keys: u32,
    modes: u32,
    macros: u32,
    pool: &'a [u8],
    keycodes: &'a [u8],
    actuations: &'a [u8],
}

impl<'a> Section<'a> {
    /// Parses the section at the start of `rest`, which is word `start_word` of the blob; returns
    /// it and its length in words.
    fn parse(
        layout: KeyframeLayout,
        rest: &'a [u8],
        start_word: u32,
    ) -> Result<(Self, u32), KeymapError> {
        let total = (rest.len() / 4) as u64;
        if total < SECTION_WORDS as u64 {
            return Err(KeymapError::Truncated(layout));
        }
        let [layers, keys, modes, macros, _, kc, act] = core::array::from_fn(|i| word(rest, i));
        for (field, value, min, max) in [
            ("keys", keys, 1, MAX_KEYS),
            ("layers", layers, 1, MAX_LAYERS),
            ("modes", modes, 1, MAX_MODES),
            ("macros", macros, 0, MAX_MACROS),
        ] {
            if !(min..=max).contains(&value) {
                return Err(KeymapError::Count {
                    layout,
                    field,
                    value,
                });
            }
        }
        if (macros == 0 && kc != 0) || (macros > 0 && kc < macros) {
            return Err(KeymapError::KeycodesOffset(layout));
        }
        let available = total - SECTION_WORDS as u64;
        if kc as u64 > available {
            return Err(KeymapError::KeycodesOffset(layout));
        }
        let expected_act = kc as u64 + keys as u64 * layers as u64;
        if act as u64 != expected_act || expected_act > available {
            return Err(KeymapError::ActuationOffset(layout));
        }
        let act_words = modes as u64 * keys as u64;
        if act_words > available - act as u64 {
            return Err(KeymapError::Truncated(layout));
        }
        let body = &rest[SECTION_WORDS * 4..];
        let (pool, after_pool) = body.split_at(kc as usize * 4);
        let (keycodes, after_keycodes) = after_pool.split_at((act - kc) as usize * 4);
        let actuations = &after_keycodes[..act_words as usize * 4];
        check_macros(layout, pool, macros)?;
        let section = Self {
            start_word,
            layers,
            keys,
            modes,
            macros,
            pool,
            keycodes,
            actuations,
        };
        Ok((
            section,
            (SECTION_WORDS as u64 + act as u64 + act_words) as u32,
        ))
    }

    /// Where the section starts in the blob, in words. Its key codes start at word
    /// `start_word + 7 + pool_words`, layer by layer; its actuation table follows them.
    pub fn start_word(&self) -> u32 {
        self.start_word
    }

    pub fn layers(&self) -> u32 {
        self.layers
    }

    pub fn keys(&self) -> u32 {
        self.keys
    }

    pub fn modes(&self) -> u32 {
        self.modes
    }

    pub fn macro_count(&self) -> u32 {
        self.macros
    }

    /// The macro pool's length in words: the offset of the key codes from the section's word 7.
    pub fn pool_words(&self) -> u32 {
        (self.pool.len() / 4) as u32
    }

    pub fn keycode(&self, layer: u32, key: u32) -> Option<KeyCode> {
        (layer < self.layers && key < self.keys)
            .then(|| KeyCode::from_word(word(self.keycodes, (layer * self.keys + key) as usize)))
    }

    pub fn actuation(&self, mode: u32, key: u32) -> Option<Actuation> {
        (mode < self.modes && key < self.keys)
            .then(|| Actuation::from_word(word(self.actuations, (mode * self.keys + key) as usize)))
    }

    pub fn macro_steps(&self, index: u32) -> Option<impl Iterator<Item = MacroStep> + 'a> {
        if index >= self.macros {
            return None;
        }
        let pool = self.pool;
        let at = word(pool, index as usize) as usize;
        let len = word(pool, at) as usize;
        Some((0..len).map(move |i| MacroStep::from_word(word(pool, at + 1 + i))))
    }
}

/// The pool starts with one offset per macro; each macro is a length word and that many steps,
/// packed in order with no gaps.
fn check_macros(layout: KeyframeLayout, pool: &[u8], macros: u32) -> Result<(), KeymapError> {
    if pool.is_empty() {
        return Ok(());
    }
    let pool_words = (pool.len() / 4) as u32;
    let mut at = macros;
    for index in 0..macros {
        if word(pool, index as usize) != at || at >= pool_words {
            return Err(KeymapError::Macro { layout, index });
        }
        let len = word(pool, at as usize);
        if len > pool_words - at - 1 {
            return Err(KeymapError::Macro { layout, index });
        }
        at += 1 + len;
    }
    if at != pool_words {
        return Err(KeymapError::MacroPool(layout));
    }
    Ok(())
}

/// What a key does on one layer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyCode {
    /// A word of 0.
    Nothing,
    /// A HID keyboard usage, plus modifiers to add (bit 0 Left Ctrl … bit 7 Right GUI).
    Key {
        usage: u16,
        modifiers: u8,
    },
    Macro(u16),
    Layer {
        kind: LayerKind,
        target: u8,
    },
    /// A HID Consumer Page usage.
    Consumer(u16),
    /// An OpenFlux function; function 0 is `KC_TRANSPARENT`.
    Function(u8),
    /// `KC_NO`, exactly `0xF000_0000`.
    Disabled,
    /// A kind the firmware does nothing with.
    Unknown(u32),
}

pub const TRANSPARENT: u32 = 0x5000_0000;
pub const DISABLED: u32 = 0xF000_0000;

impl KeyCode {
    pub fn from_word(word: u32) -> Self {
        match (word >> 28, word) {
            (_, 0) => Self::Nothing,
            (_, DISABLED) => Self::Disabled,
            (0x0, _) => Self::Key {
                usage: word as u16,
                modifiers: (word >> 16) as u8,
            },
            (0x1, _) => Self::Macro(word as u16),
            (0x2, _) => Self::Layer {
                kind: LayerKind::from_bits((word >> 24) as u8 & 0xF),
                target: word as u8,
            },
            (0x4, _) => Self::Consumer(word as u16),
            (0x5, _) => Self::Function(word as u8),
            _ => Self::Unknown(word),
        }
    }
}

/// Bits 24-27 of a layer key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayerKind {
    /// Active while held (1, and 0, which the firmware treats the same).
    Momentary,
    /// Flips the layer on and off (2).
    Toggle,
    /// Makes the layer the only base layer and clears toggles (3).
    SwitchTo,
    /// Turns the layer off if toggled on, else makes it the only toggled layer (4).
    ToggleOnly,
    Other(u8),
}

impl LayerKind {
    fn from_bits(bits: u8) -> Self {
        match bits {
            0 | 1 => Self::Momentary,
            2 => Self::Toggle,
            3 => Self::SwitchTo,
            4 => Self::ToggleOnly,
            other => Self::Other(other),
        }
    }
}

pub const DEFAULT_ACTUATION: u16 = 650;
pub const DEFAULT_RELEASE_DEPTH: u16 = 216;

/// When a key triggers in one actuation mode, in calibrated key-signal units (0 to 4095),
/// before the firmware's per-position gain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Actuation {
    /// Type 0 or 3: fixed, at [`DEFAULT_ACTUATION`] releasing [`DEFAULT_RELEASE_DEPTH`] below it.
    Default,
    /// Presses above `actuation`; a held key releases at or below `actuation - release_depth`.
    Fixed { actuation: u16, release_depth: u16 },
    /// Presses above `actuation`; releases once the reading falls `sensitivity` below its peak.
    Rapid { actuation: u16, sensitivity: u16 },
}

impl Actuation {
    pub fn from_word(word: u32) -> Self {
        let (a, r) = ((word & 0xFFF) as u16, ((word >> 12) & 0xFFF) as u16);
        match word >> 30 {
            1 => Self::Fixed {
                actuation: a,
                release_depth: r,
            },
            2 => Self::Rapid {
                actuation: a,
                sensitivity: r,
            },
            _ => Self::Default,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MacroStep {
    pub down: bool,
    /// The wait before this step.
    pub delay_ms: u16,
    /// A HID keyboard usage.
    pub usage: u8,
}

impl MacroStep {
    pub fn from_word(word: u32) -> Self {
        Self {
            down: word >> 31 != 0,
            delay_ms: (word >> 8) as u16,
            usage: word as u8,
        }
    }
}

/// Which actuation mode and default layers a profile selects.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Profile {
    /// An index beyond the keymap's modes selects mode 0.
    pub actuation_mode: u32,
    /// `1 << layer` for each default layer; 0 keeps the current layers.
    pub default_layers: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProfileError {
    Length(usize),
    /// The upload's file type isn't 0 (profile settings).
    FileType(u32),
    TooMany(u32),
}

/// Up to [`MAX_PROFILES`] profiles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProfileSettings {
    count: usize,
    profiles: [Profile; MAX_PROFILES],
}

const UPLOAD_HEADER: usize = 16;
const TABLE_HEADER: usize = 16;
const PROFILE_LEN: usize = 8;

impl ProfileSettings {
    pub const MAX_EXPORT_LEN: usize = TABLE_HEADER + MAX_PROFILES * PROFILE_LEN;
    pub const MAX_UPLOAD_LEN: usize = UPLOAD_HEADER + Self::MAX_EXPORT_LEN;

    pub fn new(profiles: &[Profile]) -> Result<Self, ProfileError> {
        if profiles.len() > MAX_PROFILES {
            return Err(ProfileError::TooMany(profiles.len() as u32));
        }
        let mut all = [Profile::default(); MAX_PROFILES];
        all[..profiles.len()].copy_from_slice(profiles);
        Ok(Self {
            count: profiles.len(),
            profiles: all,
        })
    }

    pub fn profiles(&self) -> &[Profile] {
        &self.profiles[..self.count]
    }

    /// A `FILE_FROM_HOST` payload: a 16-byte file header (version and reserved words are
    /// ignored, as the firmware does), then the table.
    pub fn parse_upload(payload: &[u8]) -> Result<Self, ProfileError> {
        if payload.len() < UPLOAD_HEADER + TABLE_HEADER {
            return Err(ProfileError::Length(payload.len()));
        }
        let file_type = word(payload, 1);
        if file_type != 0 {
            return Err(ProfileError::FileType(file_type));
        }
        Self::parse_table(&payload[UPLOAD_HEADER..]).map_err(|e| match e {
            ProfileError::Length(len) => ProfileError::Length(len + UPLOAD_HEADER),
            other => other,
        })
    }

    /// A `FILE_TO_HOST` answer: the table without the file header.
    pub fn parse_export(payload: &[u8]) -> Result<Self, ProfileError> {
        if payload.len() < TABLE_HEADER {
            return Err(ProfileError::Length(payload.len()));
        }
        Self::parse_table(payload)
    }

    fn parse_table(table: &[u8]) -> Result<Self, ProfileError> {
        let count = word(table, 0);
        if count as usize > MAX_PROFILES {
            return Err(ProfileError::TooMany(count));
        }
        if table.len() != TABLE_HEADER + count as usize * PROFILE_LEN {
            return Err(ProfileError::Length(table.len()));
        }
        let mut profiles = [Profile::default(); MAX_PROFILES];
        for (i, profile) in profiles[..count as usize].iter_mut().enumerate() {
            let at = (TABLE_HEADER + i * PROFILE_LEN) / 4;
            *profile = Profile {
                actuation_mode: word(table, at),
                default_layers: word(table, at + 1),
            };
        }
        Ok(Self {
            count: count as usize,
            profiles,
        })
    }

    /// Writes the `FILE_TO_HOST` answer into `out` and returns its length.
    pub fn encode_export(&self, out: &mut [u8; Self::MAX_EXPORT_LEN]) -> usize {
        out.fill(0);
        out[..4].copy_from_slice(&(self.count as u32).to_le_bytes());
        for (i, profile) in self.profiles().iter().enumerate() {
            let at = TABLE_HEADER + i * PROFILE_LEN;
            out[at..at + 4].copy_from_slice(&profile.actuation_mode.to_le_bytes());
            out[at + 4..at + 8].copy_from_slice(&profile.default_layers.to_le_bytes());
        }
        TABLE_HEADER + self.count * PROFILE_LEN
    }

    /// Writes a `FILE_FROM_HOST` payload into `out` and returns its length.
    pub fn encode_upload(&self, out: &mut [u8; Self::MAX_UPLOAD_LEN]) -> usize {
        out[..UPLOAD_HEADER].fill(0);
        let table: &mut [u8; Self::MAX_EXPORT_LEN] =
            (&mut out[UPLOAD_HEADER..]).try_into().unwrap();
        UPLOAD_HEADER + self.encode_export(table)
    }
}

fn word(bytes: &[u8], index: usize) -> u32 {
    u32::from_le_bytes(bytes[index * 4..index * 4 + 4].try_into().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Vectors, hex, unhex};

    const VECTORS: Vectors = Vectors(include_str!("../../../firmware/vectors/keymap.txt"));

    /// The firmware's view of a section: `layers:keys:modes:macros:pool:kc_word:act_word`.
    fn view(section: Option<&Section<'_>>) -> String {
        section.map_or("-".into(), |s| {
            let kc = SECTION_WORDS as u32 + s.pool_words();
            let act = kc + s.keys() * s.layers();
            format!(
                "{}:{}:{}:{}:{}:{kc}:{act}",
                s.layers(),
                s.keys(),
                s.modes(),
                s.macro_count(),
                s.pool_words()
            )
        })
    }

    #[test]
    fn every_keymap_is_judged_as_the_firmware_judges_it() {
        let cases: Vec<_> = VECTORS
            .0
            .lines()
            .filter_map(|l| l.strip_prefix("keymap.")?.split_once(' '))
            .filter(|(n, _)| !n.ends_with(".view"))
            .collect();
        assert_eq!(cases.len(), 28);
        for (name, blob) in cases {
            let blob = unhex(blob);
            let got = match Keymap::parse(&blob) {
                Ok(keymap) => LAYOUTS.map(|l| view(keymap.section(l))).join(" "),
                Err(_) => "refused".into(),
            };
            assert_eq!(got, VECTORS.get(&format!("keymap.{name}.view")), "{name}");
        }
    }

    #[test]
    fn refusals_say_why() {
        let blob = unhex(VECTORS.get("keymap.nine_layers"));
        assert_eq!(
            Keymap::parse(&blob),
            Err(KeymapError::Count {
                layout: KeyframeLayout::Iso,
                field: "layers",
                value: 9
            })
        );
        let blob = unhex(VECTORS.get("keymap.macro_pool_gap"));
        assert_eq!(
            Keymap::parse(&blob),
            Err(KeymapError::Macro {
                layout: KeyframeLayout::Iso,
                index: 1
            })
        );
        assert_eq!(Keymap::parse(&[0; 20]), Err(KeymapError::Length(20)));
    }

    #[test]
    fn entries_decode_as_written() {
        let blob = unhex(VECTORS.get("keymap.all"));
        let keymap = Keymap::parse(&blob).unwrap();
        let ansi = keymap.section(KeyframeLayout::Ansi).unwrap();
        assert_eq!(
            ansi.keycode(0, 0),
            Some(KeyCode::Key {
                usage: 4,
                modifiers: 2
            })
        );
        assert_eq!(ansi.keycode(0, 1), Some(KeyCode::Macro(0)));
        assert_eq!(
            ansi.keycode(0, 2),
            Some(KeyCode::Layer {
                kind: LayerKind::Momentary,
                target: 1
            })
        );
        assert_eq!(ansi.keycode(0, 3), Some(KeyCode::Consumer(0xE9)));
        assert_eq!(ansi.keycode(0, 4), Some(KeyCode::Function(4)));
        assert_eq!(ansi.keycode(0, 5), Some(KeyCode::Disabled));
        assert_eq!(ansi.keycode(0, 6), Some(KeyCode::Nothing));
        assert_eq!(ansi.keycode(0, 7), Some(KeyCode::Function(0)));
        assert_eq!(ansi.keycode(0, 8), Some(KeyCode::Unknown(0x3200_0001)));
        assert_eq!(ansi.keycode(2, 0), None);
        assert_eq!(ansi.actuation(0, 0), Some(Actuation::Default));
        assert_eq!(
            ansi.actuation(0, 1),
            Some(Actuation::Fixed {
                actuation: 613,
                release_depth: 7
            })
        );
        assert_eq!(
            ansi.actuation(0, 2),
            Some(Actuation::Rapid {
                actuation: 626,
                sensitivity: 14
            })
        );
        assert_eq!(ansi.actuation(0, 3), Some(Actuation::Default));
        let iso = keymap.section(KeyframeLayout::Iso).unwrap();
        let steps: Vec<_> = iso.macro_steps(2).unwrap().collect();
        assert_eq!(steps.len(), 5);
        assert_eq!(
            steps[1],
            MacroStep {
                down: true,
                delay_ms: 12,
                usage: 5
            }
        );
        assert_eq!(iso.macro_steps(1).unwrap().count(), 0);
        assert!(iso.macro_steps(3).is_none());
    }

    #[test]
    fn profile_settings_match_the_firmware() {
        let cases: Vec<_> = VECTORS
            .0
            .lines()
            .filter_map(|l| l.strip_prefix("profiles.")?.split_once(' '))
            .filter(|(n, _)| !n.ends_with(".export"))
            .collect();
        assert_eq!(cases.len(), 7);
        for (name, upload) in cases {
            let got = match ProfileSettings::parse_upload(&unhex(upload)) {
                Ok(settings) => {
                    let mut out = [0; ProfileSettings::MAX_EXPORT_LEN];
                    let n = settings.encode_export(&mut out);
                    assert_eq!(
                        ProfileSettings::parse_export(&out[..n]),
                        Ok(settings),
                        "{name}"
                    );
                    hex(&out[..n])
                }
                Err(_) => "refused".into(),
            };
            assert_eq!(
                got,
                VECTORS.get(&format!("profiles.{name}.export")),
                "{name}"
            );
        }
    }

    #[test]
    fn an_encoded_upload_parses_back() {
        let profiles = [
            Profile {
                actuation_mode: 1,
                default_layers: 1,
            },
            Profile {
                actuation_mode: 0,
                default_layers: 4,
            },
        ];
        let settings = ProfileSettings::new(&profiles).unwrap();
        let mut out = [0xAA; ProfileSettings::MAX_UPLOAD_LEN];
        let n = settings.encode_upload(&mut out);
        assert_eq!(n, 32 + 16);
        assert_eq!(
            ProfileSettings::parse_upload(&out[..n]).unwrap().profiles(),
            profiles
        );
        assert_eq!(
            ProfileSettings::new(&[Profile::default(); 65]),
            Err(ProfileError::TooMany(65))
        );
    }
}
