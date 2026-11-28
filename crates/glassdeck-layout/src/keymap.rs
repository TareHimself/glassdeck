//! What each key does, as the key scanner's keymap says, and the label that follows from it.
//! `keymaps/default.json` is Keyglass's default keymap document (Copyright (c) 2026 The OpenFlux
//! Contributors, MIT OR Apache-2.0), the same format as the `keymap.json` a keyboard keeps.

use std::collections::HashMap;

use anyhow::{Context, Result, bail};
use crate::Variant;
use serde::Deserialize;

const DEFAULT_KEYMAP: &str = include_str!("../keymaps/default.json");

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyAction {
    /// A key, media control or keyboard function, by its QMK name, plus modifiers to add (bit 0
    /// Left Ctrl to bit 7 Right GUI)
    Key { code: String, modifiers: u8 },
    Macro(u32),
    Layer { control: LayerControl, target: u8 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
pub enum LayerControl {
    /// While the key is held
    #[serde(rename = "momentaryLayer")]
    Momentary,
    #[serde(rename = "toggleLayer")]
    Toggle,
    #[serde(rename = "switchToLayer")]
    SwitchTo,
}

/// One layout's layers, each with an action per key in the layout's key order.
#[derive(Clone, Debug)]
pub struct Keymap {
    layers: Vec<Vec<KeyAction>>,
}

#[derive(Deserialize)]
struct Document {
    keymaps: HashMap<String, RawLayout>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawLayout {
    layers: Vec<RawLayer>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawLayer {
    key_actions: Vec<RawAction>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawAction {
    #[serde(rename = "type")]
    kind: u8,
    key_code: Option<String>,
    modifiers: Option<u8>,
    macro_id: Option<u32>,
    layer_control_type: Option<LayerControl>,
    target_layer: Option<u8>,
}

impl RawAction {
    fn parse(self) -> Result<KeyAction> {
        Ok(match self.kind {
            0 => KeyAction::Key { code: self.key_code.context("a key action has no keyCode")?, modifiers: self.modifiers.unwrap_or(0) },
            1 => KeyAction::Macro(self.macro_id.context("a macro action has no macroId")?),
            2 => KeyAction::Layer {
                control: self.layer_control_type.context("a layer action has no layerControlType")?,
                target: self.target_layer.context("a layer action has no targetLayer")?,
            },
            other => bail!("unknown key action type {other}"),
        })
    }
}

impl Keymap {
    /// The keymap a keyboard has before anyone changes it.
    pub fn default_for(variant: Variant) -> Self {
        Self::parse(DEFAULT_KEYMAP, variant).expect("the built-in keymap parses")
    }

    pub fn parse(document: &str, variant: Variant) -> Result<Self> {
        let document: Document = serde_json::from_str(document).context("reading the keymap")?;
        let name = match variant {
            Variant::Ansi => "ansi",
            Variant::Iso => "iso",
        };
        let layout = document.keymaps.into_iter().find(|(n, _)| n == name).with_context(|| format!("the keymap has no {name} layout"))?.1;
        let layers = layout
            .layers
            .into_iter()
            .map(|layer| layer.key_actions.into_iter().map(RawAction::parse).collect::<Result<Vec<_>>>())
            .collect::<Result<Vec<_>>>()?;
        Ok(Self { layers })
    }

    /// The label for `key` on `layer`; a transparent key shows the layer beneath it.
    pub fn label(&self, layer: usize, key: usize) -> String {
        for below in (0..=layer.min(self.layers.len().saturating_sub(1))).rev() {
            match self.layers[below].get(key) {
                Some(KeyAction::Key { code, .. }) if code == "KC_TRANSPARENT" => continue,
                Some(action) => return legend(action),
                None => return String::new(),
            }
        }
        String::new()
    }

    pub fn keys(&self) -> usize {
        self.layers.first().map_or(0, Vec::len)
    }
}

const MODIFIER_NAMES: [&str; 8] = ["Ctrl", "Shift", "Alt", "Win", "Ctrl", "Shift", "Alt", "Win"];

/// The label a key's action gets when nothing overrides it.
pub fn legend(action: &KeyAction) -> String {
    match action {
        KeyAction::Layer { target: 1, .. } => "Fn".into(),
        KeyAction::Layer { target, .. } => format!("L{target}"),
        KeyAction::Macro(id) => format!("M{id}"),
        KeyAction::Key { code, modifiers } => {
            let mut held: Vec<&str> = (0..8).filter(|bit| modifiers & (1 << bit) != 0).map(|bit| MODIFIER_NAMES[bit]).collect();
            held.dedup();
            let name = key_name(code);
            if held.is_empty() || name.is_empty() { name } else { format!("{}+{name}", held.join("+")) }
        }
    }
}

fn key_name(code: &str) -> String {
    let name = code.strip_prefix("KC_").unwrap_or(code);
    let known = match name {
        "NO" | "TRANSPARENT" => "",
        "ESCAPE" => "Esc",
        "BACKSPACE" => "Backspace",
        "DELETE" => "Delete",
        "INSERT" => "Ins",
        "CAPS_LOCK" => "Caps",
        "TAB" => "Tab",
        "ENTER" => "Enter",
        "SPACE" => "Space",
        "LEFT_SHIFT" | "RIGHT_SHIFT" => "Shift",
        "LEFT_CTRL" | "RIGHT_CTRL" => "Ctrl",
        "LEFT_ALT" | "RIGHT_ALT" => "Alt",
        "LEFT_GUI" | "RIGHT_GUI" => "Win",
        "GLOBE" => "Globe",
        "GRAVE" => "`",
        "MINUS" => "-",
        "EQUAL" => "=",
        "LEFT_BRACKET" => "[",
        "RIGHT_BRACKET" => "]",
        "BACKSLASH" | "NONUS_BACKSLASH" => "\\",
        "NONUS_HASH" => "#",
        "SEMICOLON" => ";",
        "QUOTE" => "'",
        "COMMA" => ",",
        "DOT" => ".",
        "SLASH" => "/",
        "HOME" => "Home",
        "END" => "End",
        "PAGE_UP" => "PgUp",
        "PAGE_DOWN" => "PgDn",
        "PRINT_SCREEN" => "PrtSc",
        "UP" => "\u{2191}",
        "DOWN" => "\u{2193}",
        "LEFT" => "\u{2190}",
        "RIGHT" => "\u{2192}",
        "AUDIO_MUTE" => "Mute",
        "AUDIO_VOL_UP" => "Vol+",
        "AUDIO_VOL_DOWN" => "Vol-",
        "MEDIA_PLAY_PAUSE" => "Play",
        "MEDIA_NEXT_TRACK" => "Next",
        "MEDIA_PREV_TRACK" => "Prev",
        "BRIGHTNESS_UP" | "OF_BRIGHTNESS_UP" => "Bright+",
        "BRIGHTNESS_DOWN" | "OF_BRIGHTNESS_DOWN" => "Bright-",
        "MISSION_CONTROL" => "Windows",
        "LAUNCHPAD" => "Apps",
        "OF_START_PAUSE_POMODORO" => "Focus",
        "OF_RESET_POMODORO" => "Reset",
        _ => return single_or_spelled(name),
    };
    known.to_owned()
}

/// Letters, digits and F-keys as they read; anything else spelled out from its QMK name.
fn single_or_spelled(name: &str) -> String {
    if name.len() == 1 || (name.starts_with('F') && name[1..].parse::<u8>().is_ok()) {
        return name.to_owned();
    }
    let name = name.strip_prefix("OF_").unwrap_or(name);
    let mut words = name.split('_').map(|w| w.to_ascii_lowercase());
    let first = words.next().map(|w| w[..1].to_ascii_uppercase() + &w[1..]).unwrap_or_default();
    std::iter::once(first).chain(words).collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(variant: Variant, layer: usize) -> Vec<String> {
        let keymap = Keymap::default_for(variant);
        (0..keymap.keys()).map(|key| keymap.label(layer, key)).collect()
    }

    #[test]
    fn the_default_keymap_fits_the_layouts() {
        for variant in [Variant::Ansi, Variant::Iso] {
            assert_eq!(Keymap::default_for(variant).keys(), crate::Layout::new(variant).keys.len(), "{variant:?}");
        }
    }

    #[test]
    fn the_bottom_row_reads_as_the_keys_do() {
        let ansi = labels(Variant::Ansi, 0);
        assert_eq!(ansi[74..], ["Ctrl", "Fn", "Win", "Alt", "Space", "Alt", "Fn", "Ctrl", "\u{2190}", "\u{2193}", "\u{2192}"]);
        assert_eq!(ansi[..4], ["Esc", "F1", "F2", "F3"]);
        assert_eq!(ansi[13..16], ["PrtSc", "Ins", "Delete"]);
        assert_eq!(ansi[29..32], ["Backspace", "Home", "Tab"]);
    }

    #[test]
    fn the_fn_layer_shows_its_own_keys_and_the_base_layer_through_transparent_ones() {
        let fn_layer = labels(Variant::Ansi, 1);
        assert_eq!(fn_layer[8], "Play");
        assert_eq!(fn_layer[10], "Mute");
        assert_eq!(fn_layer[0], "Esc");
        assert_eq!(fn_layer[25], "Focus");
    }

    #[test]
    fn modifiers_and_unknown_codes_get_readable_labels() {
        assert_eq!(legend(&KeyAction::Key { code: "KC_T".into(), modifiers: 0b11 }), "Ctrl+Shift+T");
        assert_eq!(legend(&KeyAction::Key { code: "KC_F13".into(), modifiers: 0 }), "F13");
        assert_eq!(legend(&KeyAction::Key { code: "KC_OF_PLATFORM_WINDOWS".into(), modifiers: 0 }), "Platform windows");
        assert_eq!(legend(&KeyAction::Layer { control: LayerControl::Toggle, target: 3 }), "L3");
    }
}
