//! The CLI's edits to the active theme: read it from glassdeckd, change one thing, and put it back.
//! Options run the package's own `options.luau` here, as the app does.

use std::path::Path;

use anyhow::{Context, Result, bail};
use std::collections::BTreeMap;

use glassdeck_api::{ShowState, Theme, ThemeApplied, ThemePush};
use glassdeck_luau::options::{Item, Kind, OptionValue, OptionsScript, Panel, Values, read_values};
use glassdeck_package::{Package, PackageId};
use serde_json::{Map, Value, json};

use crate::client;

/// What `glassdeck bg gradient` means.
const BUILT_IN_NAME: &str = "gradient";
const STREAM_PREFIX: &str = "stream:";

pub fn show_background() -> Result<()> {
    let theme: Theme = client::get("/theme")?;
    println!("Background: {}", theme.background.package);
    match &theme.stream {
        Some(stream) => println!("Stream: {stream}, shown whenever it's available"),
        None => println!("Stream: none"),
    }
    Ok(())
}

/// `gradient`, a package id glassdeckd already holds, a `.deck` file here (pushed with the theme), or `stream:<name>`.
pub fn set_background(choice: &str) -> Result<()> {
    let mut theme: Theme = client::get("/theme")?;
    let mut packages = BTreeMap::new();
    if let Some(stream) = choice.strip_prefix(STREAM_PREFIX) {
        theme.stream = Some(stream.to_owned());
    } else {
        let local = Path::new(choice);
        let package = if glassdeck_package::is_package(local) {
            let id = Package::open(local)?.id().clone();
            packages.insert(id.to_string(), absolute(local)?);
            id
        } else if choice == BUILT_IN_NAME {
            PackageId::parse(glassdeck_proto::BUILT_IN_BACKGROUND)?
        } else {
            PackageId::parse(choice)?
        };
        theme.background = glassdeck_api::ThemeBackground { package: package.to_string(), options: Map::new() };
        theme.stream = None;
    }
    let applied: ThemeApplied = client::put("/theme", &ThemePush { theme: theme.clone(), packages })?;
    let state = match applied.stream {
        None | Some(ShowState::Shown) => "",
        Some(ShowState::WaitingForKeyboard) => " (waiting for the keyboard)",
        Some(ShowState::AfterUpload) => " (after the upload)",
        Some(ShowState::Starting) => " (starting)",
        Some(ShowState::Unavailable) => " (not there now; it shows when it is)",
    };
    match &theme.stream {
        Some(stream) => println!("Stream: {stream}{state}"),
        None => println!("Background: {}", theme.background.package),
    }
    Ok(())
}

/// One theme entry's options: whose they are, the package's script, its first pass, and the stored values.
struct Owner {
    label: String,
    script: OptionsScript,
    first: Panel,
    stored: Map<String, Value>,
}

fn owner(theme: &Theme, widget: Option<usize>) -> Result<Owner> {
    let (label, package, stored) = match widget {
        None => ("the background".to_owned(), &theme.background.package, &theme.background.options),
        Some(index) => {
            let entry = theme.widgets.get(index).with_context(|| format!("the theme has {} widgets, so there's no widget {index}", theme.widgets.len()))?;
            (format!("widget {index} ({})", entry.package), &entry.package, &entry.options)
        }
    };
    let id = PackageId::parse(package)?;
    if id.as_str() == glassdeck_proto::BUILT_IN_BACKGROUND {
        bail!("{label} is the built-in gradient, which has no options");
    }
    let path = glassdeck_api::active_packages()?.join(id.file_name());
    let package = Package::open(&path).with_context(|| format!("{label}'s package"))?;
    let source = package.options_script()?.with_context(|| format!("{label} has no options"))?;
    let script = OptionsScript::new(&source);
    let first = script.first_pass()?;
    Ok(Owner { label, script, first, stored: stored.clone() })
}

pub fn show_options(widget: Option<usize>) -> Result<()> {
    let theme: Theme = client::get("/theme")?;
    let owner = owner(&theme, widget)?;
    let resolved = owner.first.resolve(&read_values(&owner.label, &owner.first, owner.stored.iter()));
    let panel = owner.script.run(&owner.first, &resolved.values)?;
    println!("Options for {}", owner.label);
    print_items(&panel.items, &resolved.values, 1);
    for dropped in &resolved.dropped {
        println!("  (dropped a stored value: {dropped})");
    }
    Ok(())
}

/// Sets one option (or resets it, with `None`), checked against its kind and the script, then pushes the theme.
pub fn set_option(widget: Option<usize>, path: &str, typed: Option<&str>) -> Result<()> {
    let mut theme: Theme = client::get("/theme")?;
    let owner = owner(&theme, widget)?;
    let option = owner.first.options().into_iter().find(|o| o.path == path).with_context(|| format!("{} has no option {path}", owner.label))?;
    let value = typed.map(|text| parse_typed(&option.kind, text)).transpose().with_context(|| path.to_owned())?;
    if let Some(value) = &value
        && let Some(why) = option.kind.refuses(value)
    {
        bail!("{path}: {why}");
    }
    let mut stored = owner.stored.clone();
    match &value {
        Some(value) => stored.insert(path.to_owned(), value.to_json()),
        None => stored.remove(path),
    };
    let values = read_values(&owner.label, &owner.first, stored.iter());
    owner.script.run(&owner.first, &owner.first.resolve(&values).values)?;
    match widget {
        None => theme.background.options = stored,
        Some(index) => theme.widgets[index].options = stored,
    }
    client::put::<ThemeApplied>("/theme", &ThemePush { theme, packages: BTreeMap::new() })?;
    let shown = value.unwrap_or_else(|| option.kind.default_value()).to_json();
    println!("{} {path} = {shown}", owner.label);
    Ok(())
}

/// A value as typed on the command line (`on`, `0.5`, `#ff8000`), read for the option's kind.
fn parse_typed(kind: &Kind, text: &str) -> Result<OptionValue> {
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

/// The panel as the app would draw it: sections indented, hidden options marked.
fn print_items(items: &[Item], values: &Values, depth: usize) {
    let indent = "  ".repeat(depth);
    for item in items {
        match item {
            Item::Section { path, label, items } => {
                println!("{indent}{label} ({path})");
                print_items(items, values, depth + 1);
            }
            Item::Heading(text) => println!("{indent}-- {text} --"),
            Item::Option(option) => {
                let limits = match &option.kind {
                    Kind::Range { min, max, .. } => format!("range {min} to {max}"),
                    Kind::Choice { values, .. } => format!("one of {}", values.join(", ")),
                    Kind::Color { .. } => "colour".to_owned(),
                    Kind::Number { .. } => "number".to_owned(),
                    Kind::Toggle { .. } => "toggle".to_owned(),
                    Kind::Text { .. } => "text".to_owned(),
                };
                let value = values.get(&option.path).cloned().unwrap_or_else(|| option.kind.default_value()).to_json();
                let hidden = if option.visible { "" } else { "  [hidden]" };
                println!("{indent}{}: {value}  ({}, {limits}){hidden}", option.label, option.path);
            }
        }
    }
}

pub fn show_theme() -> Result<()> {
    let theme: Theme = client::get("/theme")?;
    println!("{}", serde_json::to_string_pretty(&theme)?);
    Ok(())
}

/// Pushes a theme file, with every package it names that `library` holds as `<id>.deck`.
pub fn push_theme_file(file: &Path, library: &Path) -> Result<()> {
    let text = std::fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?;
    let theme: Theme = serde_json::from_str(&text).with_context(|| format!("parsing {}", file.display()))?;
    let ids = std::iter::once(&theme.background.package).chain(theme.widgets.iter().map(|w| &w.package));
    let mut packages = BTreeMap::new();
    for id in ids {
        let path = library.join(PackageId::parse(id)?.file_name());
        if path.is_file() {
            packages.insert(id.clone(), absolute(&path)?);
        }
    }
    let applied: ThemeApplied = client::put("/theme", &ThemePush { theme, packages })?;
    println!("Pushed {}{}", file.display(), if applied.stream.is_some() { ", with its stream" } else { "" });
    Ok(())
}

fn absolute(path: &Path) -> Result<String> {
    Ok(std::path::absolute(path).with_context(|| format!("finding {}", path.display()))?.to_string_lossy().into_owned())
}
