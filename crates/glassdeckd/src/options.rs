//! What the user chose for each background's options, kept in `options.json` until the desktop client
//! has its own store, and the views of a panel the CLI shows.

use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use glassdeck_luau::options::{Item, Kind, OptionValue, Panel, Values, format_color};
use serde_json::{Value as Json, json};

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

/// A value typed on the command line, read for the option's kind.
pub fn parse_typed(kind: &Kind, text: &str) -> Result<OptionValue> {
    let json = match kind {
        Kind::Range { .. } | Kind::Number { .. } => json!(text.parse::<f64>().with_context(|| format!("{text:?} isn't a number"))?),
        Kind::Toggle { .. } => match text {
            "true" | "on" | "yes" => json!(true),
            "false" | "off" | "no" => json!(false),
            _ => bail!("{text:?} isn't true or false"),
        },
        Kind::Color { .. } | Kind::Text { .. } | Kind::Choice { .. } => json!(text),
    };
    OptionValue::from_json(kind, &json)
}

/// The panel for the CLI: sections and options in order, with each option's current value.
pub fn panel_json(panel: &Panel, values: &Values) -> Json {
    fn entries(items: &[Item], values: &Values) -> Vec<Json> {
        items
            .iter()
            .map(|item| match item {
                Item::Section { path, label, items: inner } => json!({ "section": path, "label": label, "items": entries(inner, values) }),
                Item::Heading(text) => json!({ "heading": text }),
                Item::Option(option) => {
                    let value = values.get(&option.path).cloned().unwrap_or_else(|| option.kind.default_value());
                    json!({
                        "option": option.path,
                        "label": option.label,
                        "kind": kind_json(&option.kind),
                        "value": value.to_json(),
                        "visible": option.visible,
                        "hint": option.hint,
                    })
                }
            })
            .collect()
    }
    json!({ "items": entries(&panel.items, values) })
}

fn kind_json(kind: &Kind) -> Json {
    match kind {
        Kind::Color { default, alpha } => json!({ "type": "color", "default": format_color(*default), "alpha": alpha }),
        Kind::Range { default, min, max, step, unit } => {
            json!({ "type": "range", "default": default, "min": min, "max": max, "step": step, "unit": unit })
        }
        Kind::Number { default, min, max, step, unit } => {
            json!({ "type": "number", "default": default, "min": min, "max": max, "step": step, "unit": unit })
        }
        Kind::Toggle { default } => json!({ "type": "toggle", "default": default }),
        Kind::Text { default, placeholder, max_length } => {
            json!({ "type": "text", "default": default, "placeholder": placeholder, "max_length": max_length })
        }
        Kind::Choice { default, values } => json!({ "type": "choice", "default": default, "values": values }),
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
            .set("a.deckbg", &panel, "look.tint", Some(parse_typed(&Kind::Color { default: [0.0; 4], alpha: true }, "#ff0000").unwrap()))
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

    #[test]
    fn typed_values_are_read_for_their_kind() {
        assert_eq!(parse_typed(&Kind::Toggle { default: false }, "on").unwrap(), OptionValue::Bool(true));
        assert!(parse_typed(&Kind::Range { default: 0.0, min: 0.0, max: 1.0, step: None, unit: None }, "fast").is_err());
    }
}
