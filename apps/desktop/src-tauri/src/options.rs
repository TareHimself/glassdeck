//! A widget's options panel, from its `options.luau` and the values chosen so far, as the app draws it.

use glassdeck_api::{OptionKind, OptionView, PanelItem};
use glassdeck_luau::options::{Item, Kind, OptionsScript, Panel, Values, format_color, read_values};
use serde::Serialize;
use specta::Type;

#[derive(Clone, Debug, Serialize, Type)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum OptionsPanel {
    Ready { items: Vec<PanelItem> },
    /// `options.luau` didn't run; the message says why, for showing as it is
    Failed { reason: String },
}

fn kind_of(kind: &Kind) -> OptionKind {
    match kind.clone() {
        Kind::Color { default, alpha } => OptionKind::Color { default: format_color(default), alpha },
        Kind::Range { default, min, max, step, unit } => OptionKind::Range { default, min, max, step, unit },
        Kind::Number { default, min, max, step, unit } => OptionKind::Number { default, min, max, step, unit },
        Kind::Toggle { default } => OptionKind::Toggle { default },
        Kind::Text { default, placeholder, max_length } => OptionKind::Text { default, placeholder, max_length },
        Kind::Choice { default, values } => OptionKind::Choice { default, values },
    }
}

fn items_of(items: &[Item], values: &Values) -> Vec<PanelItem> {
    items
        .iter()
        .map(|item| match item {
            Item::Section { path, label, items } => PanelItem::Section { path: path.clone(), label: label.clone(), items: items_of(items, values) },
            Item::Heading(text) => PanelItem::Heading { text: text.clone() },
            Item::Option(option) => PanelItem::Option(OptionView {
                path: option.path.clone(),
                label: option.label.clone(),
                kind: kind_of(&option.kind),
                value: values.get(&option.path).cloned().unwrap_or_else(|| option.kind.default_value()).to_json(),
                visible: option.visible,
                hint: option.hint.clone(),
            }),
        })
        .collect()
}

fn panel(source: &str, chosen: &serde_json::Map<String, serde_json::Value>) -> anyhow::Result<Vec<PanelItem>> {
    let script = OptionsScript::new(source);
    let first: Panel = script.first_pass()?;
    let values = first.resolve(&read_values("editor", &first, chosen.iter())).values;
    let shown = script.run(&first, &values)?;
    Ok(items_of(&shown.items, &values))
}

pub fn options_panel(source: &str, chosen: &serde_json::Map<String, serde_json::Value>) -> OptionsPanel {
    match panel(source, chosen) {
        Ok(items) => OptionsPanel::Ready { items },
        Err(e) => OptionsPanel::Failed { reason: format!("{e:#}") },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chosen_values_show_and_bad_scripts_say_why() {
        let source = r#"option("seconds", "Show seconds", toggle(false))"#;
        let chosen = serde_json::json!({ "seconds": true });
        let OptionsPanel::Ready { items } = options_panel(source, chosen.as_object().unwrap()) else { panic!("didn't run") };
        let [PanelItem::Option(view)] = items.as_slice() else { panic!("one option") };
        assert_eq!((view.label.as_str(), &view.value), ("Show seconds", &serde_json::json!(true)));
        assert!(matches!(options_panel("option(", &serde_json::Map::new()), OptionsPanel::Failed { .. }));
    }
}
