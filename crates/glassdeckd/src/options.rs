//! What the user chose for each background's options, kept in `options.json` until the desktop client
//! has its own store, and the panel views the API sends.

use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use glassdeck_api::{OptionKind, OptionView, PanelItem};
use glassdeck_luau::options::{Item, Kind, OptionValue, Panel, Values, format_color};
use serde_json::Value as Json;

/// Background name, then option path, then the stored value.
type Stored = BTreeMap<String, BTreeMap<String, Json>>;

pub struct OptionsStore {
    path: PathBuf,
}

impl OptionsStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// The values stored for `background`, read for each option's kind. One that doesn't read is
    /// left out with a warning, so its default applies.
    pub fn chosen(&self, background: &str, panel: &Panel) -> Result<Values> {
        let stored = self.load()?;
        Ok(stored.get(background).map_or_else(Values::new, |entries| read_values(background, panel, entries.iter())))
    }

    /// Stores `value` for `path`, refusing one the option can't hold; `None` goes back to the default.
    pub fn set(&self, background: &str, panel: &Panel, path: &str, value: Option<OptionValue>) -> Result<()> {
        let mut stored = self.load()?;
        let entries = stored.entry(background.to_owned()).or_default();
        match value {
            Some(value) => {
                let option =
                    panel.options().into_iter().find(|o| o.path == path).with_context(|| format!("{background} has no option {path}"))?;
                if let Some(why) = option.kind.refuses(&value) {
                    bail!("{path}: {why}");
                }
                entries.insert(path.to_owned(), value.to_json());
            }
            None => {
                entries.remove(path);
            }
        }
        if entries.is_empty() {
            stored.remove(background);
        }
        self.save(&stored)
    }

    fn load(&self) -> Result<Stored> {
        match fs::read_to_string(&self.path) {
            Ok(text) => serde_json::from_str(&text).with_context(|| format!("parsing {}", self.path.display())),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(Stored::new()),
            Err(e) => Err(e).with_context(|| format!("reading {}", self.path.display())),
        }
    }

    fn save(&self, stored: &Stored) -> Result<()> {
        if let Some(folder) = self.path.parent() {
            fs::create_dir_all(folder).with_context(|| format!("creating {}", folder.display()))?;
        }
        let staging = self.path.with_extension("json.tmp");
        fs::write(&staging, serde_json::to_vec_pretty(stored)?).with_context(|| format!("writing {}", staging.display()))?;
        fs::rename(&staging, &self.path).with_context(|| format!("moving {} into place", self.path.display()))
    }
}

/// Stored values by path (from `options.json` or a profile), read for each option's kind. One that
/// doesn't read is left out with a warning naming `owner`, so its default applies.
pub fn read_values<'a>(owner: &str, panel: &Panel, stored: impl Iterator<Item = (&'a String, &'a Json)>) -> Values {
    let kinds: BTreeMap<&str, &Kind> = panel.options().into_iter().map(|o| (o.path.as_str(), &o.kind)).collect();
    let mut values = Values::new();
    for (path, json) in stored {
        let Some(kind) = kinds.get(path.as_str()) else {
            eprintln!("Options: {owner} has no option {path} any more; its stored value is ignored");
            continue;
        };
        match OptionValue::from_json(kind, json) {
            Ok(value) => {
                values.insert(path.clone(), value);
            }
            Err(e) => eprintln!("Options: {owner} {path}: {e:#}; the default applies"),
        }
    }
    values
}

/// The panel with each option's current value, as the API sends it.
pub fn panel_view(panel: &Panel, values: &Values) -> Vec<PanelItem> {
    panel.items.iter().map(|item| item_view(item, values)).collect()
}

fn item_view(item: &Item, values: &Values) -> PanelItem {
    match item {
        Item::Section { path, label, items } => PanelItem::Section {
            path: path.clone(),
            label: label.clone(),
            items: items.iter().map(|item| item_view(item, values)).collect(),
        },
        Item::Heading(text) => PanelItem::Heading { text: text.clone() },
        Item::Option(option) => PanelItem::Option(OptionView {
            path: option.path.clone(),
            label: option.label.clone(),
            kind: kind_view(&option.kind),
            value: values.get(&option.path).cloned().unwrap_or_else(|| option.kind.default_value()).to_json(),
            visible: option.visible,
            hint: option.hint.clone(),
        }),
    }
}

fn kind_view(kind: &Kind) -> OptionKind {
    match kind.clone() {
        Kind::Color { default, alpha } => OptionKind::Color { default: format_color(default), alpha },
        Kind::Range { default, min, max, step, unit } => OptionKind::Range { default, min, max, step, unit },
        Kind::Number { default, min, max, step, unit } => OptionKind::Number { default, min, max, step, unit },
        Kind::Toggle { default } => OptionKind::Toggle { default },
        Kind::Text { default, placeholder, max_length } => OptionKind::Text { default, placeholder, max_length },
        Kind::Choice { default, values } => OptionKind::Choice { default, values },
    }
}

#[cfg(test)]
mod tests {
    use glassdeck_luau::options::OptionsScript;

    use super::*;

    fn panel() -> Panel {
        OptionsScript::new(r##"section("look", function() option("tint", color("#3d1c73")) option("palette", choice("a", { "a", "b" })) end) option("speed", range(1, 0, 4))"##)
            .first_pass()
            .unwrap()
    }

    #[test]
    fn values_are_stored_by_path_checked_and_reset() {
        let dir = std::env::temp_dir().join(format!("glassdeckd-options-{}", std::process::id()));
        let store = OptionsStore::new(dir.join("options.json"));
        let panel = panel();
        store
            .set("a.deckbg", &panel, "look.tint", Some(OptionValue::Color([1.0, 0.0, 0.0, 1.0])))
            .unwrap();
        store.set("a.deckbg", &panel, "speed", Some(OptionValue::Number(2.0))).unwrap();
        assert!(store.set("a.deckbg", &panel, "speed", Some(OptionValue::Number(9.0))).is_err());
        assert!(store.set("a.deckbg", &panel, "look.missing", Some(OptionValue::Bool(true))).is_err());
        let chosen = store.chosen("a.deckbg", &panel).unwrap();
        assert_eq!(chosen["look.tint"], OptionValue::Color([1.0, 0.0, 0.0, 1.0]));
        assert_eq!(chosen["speed"], OptionValue::Number(2.0));
        store.set("a.deckbg", &panel, "speed", None).unwrap();
        assert!(!store.chosen("a.deckbg", &panel).unwrap().contains_key("speed"));
        assert!(store.chosen("other.deckbg", &panel).unwrap().is_empty());
        let _ = fs::remove_dir_all(dir);
    }
}
