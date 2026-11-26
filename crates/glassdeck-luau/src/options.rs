//! `options.luau`: what the user can set for a package, declared with calls like
//! `option("speed", range(0.08, 0, 0.5))`. It runs in a VM holding only the builders and `current`
//! (the values so far), first with no stored values to learn the defaults, then again whenever a
//! value changes. Every run must declare the same options; values only change what's shown.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use anyhow::{Context, Result, bail, ensure};
use mlua::{AnyUserData, Function, Lua, MetaMethod, Table, UserData, UserDataMethods, Value, Vector};

use crate::{LuaResultExt, Vm};

/// A value an option holds: what `host.luau` gets and what the desktop client stores.
#[derive(Clone, Debug, PartialEq)]
pub enum OptionValue {
    Color([f32; 4]),
    Number(f64),
    Bool(bool),
    Text(String),
}

/// What kind of option it is: its default, limits and control.
#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    Color { default: [f32; 4], alpha: bool },
    Range { default: f64, min: f64, max: f64, step: Option<f64>, unit: Option<String> },
    Number { default: f64, min: Option<f64>, max: Option<f64>, step: Option<f64>, unit: Option<String> },
    Toggle { default: bool },
    Text { default: String, placeholder: Option<String>, max_length: Option<u32> },
    Choice { default: String, values: Vec<String> },
}

#[derive(Clone, Debug, PartialEq)]
pub struct OptionItem {
    /// Its key with its sections', such as `motion.rings.speed`
    pub path: String,
    pub label: String,
    pub kind: Kind,
    pub visible: bool,
    pub hint: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    Section { path: String, label: String, items: Vec<Item> },
    Option(OptionItem),
    Heading(String),
}

/// One run's result: the panel to draw, in the order the script declared it.
#[derive(Clone, Debug, PartialEq)]
pub struct Panel {
    pub items: Vec<Item>,
}

/// Values by path.
pub type Values = BTreeMap<String, OptionValue>;

/// Chosen values checked against a panel: what to use, and what was dropped and why.
#[derive(Clone, Debug, PartialEq)]
pub struct Resolved {
    pub values: Values,
    pub dropped: Vec<String>,
}

impl Kind {
    pub fn default_value(&self) -> OptionValue {
        match self {
            Self::Color { default, .. } => OptionValue::Color(*default),
            Self::Range { default, .. } | Self::Number { default, .. } => OptionValue::Number(*default),
            Self::Toggle { default } => OptionValue::Bool(*default),
            Self::Text { default, .. } | Self::Choice { default, .. } => OptionValue::Text(default.clone()),
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Color { .. } => "color",
            Self::Range { .. } => "range",
            Self::Number { .. } => "number",
            Self::Toggle { .. } => "toggle",
            Self::Text { .. } => "text",
            Self::Choice { .. } => "choice",
        }
    }

    /// Why `value` can't be this option's, or `None` if it can.
    pub fn refuses(&self, value: &OptionValue) -> Option<String> {
        match (self, value) {
            (Self::Color { .. }, OptionValue::Color(_)) | (Self::Toggle { .. }, OptionValue::Bool(_)) => None,
            (Self::Range { min, max, .. }, OptionValue::Number(n)) => {
                (!(min..=max).contains(&n)).then(|| format!("{n} is outside {min} to {max}"))
            }
            (Self::Number { min, max, .. }, OptionValue::Number(n)) => {
                let low = min.is_some_and(|min| *n < min);
                let high = max.is_some_and(|max| *n > max);
                (low || high).then(|| format!("{n} is outside its limits"))
            }
            (Self::Text { max_length, .. }, OptionValue::Text(t)) => {
                max_length.filter(|max| t.chars().count() > *max as usize).map(|max| format!("it's longer than {max} characters"))
            }
            (Self::Choice { values, .. }, OptionValue::Text(t)) => {
                (!values.contains(t)).then(|| format!("{t:?} isn't one of {}", values.join(", ")))
            }
            (kind, value) => Some(format!("a {} option can't hold {value:?}", kind.name())),
        }
    }
}

impl Panel {
    /// Every option, in order, sections flattened.
    pub fn options(&self) -> Vec<&OptionItem> {
        fn walk<'a>(items: &'a [Item], out: &mut Vec<&'a OptionItem>) {
            for item in items {
                match item {
                    Item::Option(option) => out.push(option),
                    Item::Section { items, .. } => walk(items, out),
                    Item::Heading(_) => {}
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.items, &mut out);
        out
    }

    pub fn defaults(&self) -> Values {
        self.options().into_iter().map(|o| (o.path.clone(), o.kind.default_value())).collect()
    }

    /// Chosen values over the defaults; a value that no longer fits is dropped, with why.
    pub fn resolve(&self, chosen: &Values) -> Resolved {
        let options: BTreeMap<&str, &OptionItem> = self.options().into_iter().map(|o| (o.path.as_str(), o)).collect();
        let mut values = self.defaults();
        let mut dropped = Vec::new();
        for (path, value) in chosen {
            match options.get(path.as_str()) {
                None => dropped.push(format!("{path}: there's no such option any more")),
                Some(option) => match option.kind.refuses(value) {
                    Some(why) => dropped.push(format!("{path}: {why}")),
                    None => {
                        values.insert(path.clone(), value.clone());
                    }
                },
            }
        }
        Resolved { values, dropped }
    }

    /// Which options exist and their kinds: what every run must keep the same.
    fn signature(&self) -> Vec<(String, &'static str)> {
        self.options().into_iter().map(|o| (o.path.clone(), o.kind.name())).collect()
    }
}

impl Panel {
    /// `options.d.luau`: `Options` (what `host.luau` and `current` hold) and `Changed` (the same shape, options as booleans).
    pub fn definitions(&self) -> String {
        fn fields(items: &[Item], depth: usize, leaf: &dyn Fn(&Kind) -> String, out: &mut String) {
            let indent = "    ".repeat(depth);
            for item in items {
                match item {
                    Item::Option(option) => {
                        let key = option.path.rsplit('.').next().expect("a path has a key");
                        out.push_str(&format!("{indent}{key}: {},\n", leaf(&option.kind)));
                    }
                    Item::Section { path, items, .. } => {
                        let key = path.rsplit('.').next().expect("a path has a key");
                        out.push_str(&format!("{indent}{key}: {{\n"));
                        fields(items, depth + 1, leaf, out);
                        out.push_str(&format!("{indent}}},\n"));
                    }
                    Item::Heading(_) => {}
                }
            }
        }
        let value = |kind: &Kind| match kind {
            Kind::Color { .. } => "Color".to_owned(),
            Kind::Range { .. } | Kind::Number { .. } => "number".to_owned(),
            Kind::Toggle { .. } => "boolean".to_owned(),
            Kind::Text { .. } => "string".to_owned(),
            Kind::Choice { values, .. } => values.iter().map(|v| format!("{v:?}")).collect::<Vec<_>>().join(" | "),
        };
        let mut out = String::from(
            "-- Generated by glassdeck pack from options.luau; rewritten on every pack, so don't edit it.\nexport type Color = vector\n\nexport type Options = {\n",
        );
        fields(&self.items, 1, &value, &mut out);
        out.push_str("}\n\nexport type Changed = {\n");
        fields(&self.items, 1, &|_| "boolean".to_owned(), &mut out);
        out.push_str("}\n");
        out
    }
}

pub struct OptionsScript {
    source: String,
}

impl OptionsScript {
    pub fn new(source: &str) -> Self {
        Self { source: source.to_owned() }
    }

    /// The run with no stored values: every default, and the set of options later runs must keep.
    pub fn first_pass(&self) -> Result<Panel> {
        execute(&self.source, &Values::new())
    }

    /// A run for `values` (already resolved), refused if it declares different options than `first`.
    pub fn run(&self, first: &Panel, values: &Values) -> Result<Panel> {
        let panel = execute(&self.source, values)?;
        let (expected, got) = (first.signature(), panel.signature());
        if expected != got {
            let missing: Vec<_> = expected.iter().filter(|e| !got.contains(e)).map(|e| e.0.as_str()).collect();
            let extra: Vec<_> = got.iter().filter(|g| !expected.contains(g)).map(|g| g.0.as_str()).collect();
            bail!(
                "options.luau declared different options for these values (missing: {}; new: {}); every run must declare the same options, and only visibility may change",
                list_or_none(&missing),
                list_or_none(&extra)
            );
        }
        Ok(panel)
    }
}

fn list_or_none(paths: &[&str]) -> String {
    if paths.is_empty() { "none".to_owned() } else { paths.join(", ") }
}

/// The label an option or section gets when the script doesn't name one: `drift_speed` reads "Drift speed".
pub fn label_from_key(key: &str) -> String {
    let spaced = key.replace('_', " ");
    let mut chars = spaced.chars();
    chars.next().map(|first| first.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

fn check_key(key: &str) -> Result<()> {
    let mut chars = key.chars();
    let starts = chars.next().is_some_and(|c| c.is_ascii_lowercase() || c == '_');
    ensure!(
        starts && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
        "{key:?} isn't a key: lowercase letters, digits and _, not starting with a digit"
    );
    Ok(())
}

/// `#rgb`, `#rrggbb` or `#rrggbbaa`, as sRGB from 0 to 1.
pub fn parse_color(text: &str) -> Result<[f32; 4]> {
    let digits = text.strip_prefix('#').with_context(|| format!("colour {text:?} must start with #"))?;
    ensure!(digits.bytes().all(|b| b.is_ascii_hexdigit()), "colour {text:?} has a digit that isn't hex");
    let expanded: String = match digits.len() {
        3 => digits.chars().flat_map(|c| [c, c]).chain("ff".chars()).collect(),
        6 => format!("{digits}ff"),
        8 => digits.to_owned(),
        _ => bail!("colour {text:?} must be #rgb, #rrggbb or #rrggbbaa"),
    };
    let mut channels = [0.0; 4];
    for (i, channel) in channels.iter_mut().enumerate() {
        let byte = u8::from_str_radix(&expanded[i * 2..i * 2 + 2], 16).expect("checked as hex digits");
        *channel = f32::from(byte) / 255.0;
    }
    Ok(channels)
}

/// `#rrggbbaa`, how colours are stored.
pub fn format_color(color: [f32; 4]) -> String {
    let byte = |c: f32| (c.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}{:02x}", byte(color[0]), byte(color[1]), byte(color[2]), byte(color[3]))
}

impl OptionValue {
    pub fn to_lua(&self, lua: &Lua) -> mlua::Result<Value> {
        Ok(match self {
            Self::Color([r, g, b, a]) => Value::Vector(Vector::new(*r, *g, *b, *a)),
            Self::Number(n) => Value::Number(*n),
            Self::Bool(b) => Value::Boolean(*b),
            Self::Text(t) => Value::String(lua.create_string(t)?),
        })
    }
}

impl OptionValue {
    /// A stored value, read back for its kind: colours as `"#rrggbbaa"`, the rest as JSON's own types.
    pub fn from_json(kind: &Kind, json: &serde_json::Value) -> Result<Self> {
        Ok(match (kind, json) {
            (Kind::Color { .. }, serde_json::Value::String(s)) => Self::Color(parse_color(s)?),
            (Kind::Range { .. } | Kind::Number { .. }, serde_json::Value::Number(n)) => {
                Self::Number(n.as_f64().context("a number JSON can't hold")?)
            }
            (Kind::Toggle { .. }, serde_json::Value::Bool(b)) => Self::Bool(*b),
            (Kind::Text { .. } | Kind::Choice { .. }, serde_json::Value::String(s)) => Self::Text(s.clone()),
            (kind, json) => bail!("a {} option can't hold {json}", kind.name()),
        })
    }

    pub fn to_json(&self) -> serde_json::Value {
        match self {
            Self::Color(c) => serde_json::Value::String(format_color(*c)),
            Self::Number(n) => serde_json::json!(n),
            Self::Bool(b) => serde_json::Value::Bool(*b),
            Self::Text(t) => serde_json::Value::String(t.clone()),
        }
    }
}

/// `options` for `host.luau`: the values, nested by section like the panel.
pub fn options_table(lua: &Lua, panel: &Panel, values: &Values) -> Result<Table> {
    nested(lua, &panel.items, &|option| values.get(&option.path).cloned().unwrap_or_else(|| option.kind.default_value()).to_lua(lua))
}

/// `changed` for `host.luau`: the same shape, each option `true` if its path is in `changed`, or every
/// option `true` when `changed` is `None` (the first call).
pub fn changed_table(lua: &Lua, panel: &Panel, changed: Option<&BTreeSet<String>>) -> Result<Table> {
    nested(lua, &panel.items, &|option| Ok(Value::Boolean(changed.is_none_or(|c| c.contains(&option.path)))))
}

fn nested(lua: &Lua, items: &[Item], leaf: &dyn Fn(&OptionItem) -> mlua::Result<Value>) -> Result<Table> {
    let table = lua.create_table().lua_context("creating options")?;
    for item in items {
        match item {
            Item::Option(option) => {
                let key = option.path.rsplit('.').next().expect("a path has a key");
                table.raw_set(key, leaf(option).lua_context("filling options")?).lua_context("filling options")?;
            }
            Item::Section { path, items, .. } => {
                let key = path.rsplit('.').next().expect("a path has a key");
                table.raw_set(key, nested(lua, items, leaf)?).lua_context("filling options")?;
            }
            Item::Heading(_) => {}
        }
    }
    Ok(table)
}

/// `any(section)`: whether any `true` sits anywhere in a section of `changed`.
pub fn any_function(lua: &Lua) -> Result<Function> {
    fn any(table: &Table) -> mlua::Result<bool> {
        for pair in table.pairs::<Value, Value>() {
            match pair?.1 {
                Value::Boolean(true) => return Ok(true),
                Value::Table(inner) if any(&inner)? => return Ok(true),
                _ => {}
            }
        }
        Ok(false)
    }
    lua.create_function(|_, section: Value| match section {
        Value::Table(table) => any(&table),
        Value::Boolean(b) => Ok(b),
        other => Err(mlua::Error::runtime(format!("any takes a section of changed, not a {}", other.type_name()))),
    })
    .lua_context("creating any")
}

/// A builder: the kind of an option, made by `color(...)`, `range(...)` and the rest.
#[derive(Clone)]
struct Builder(Kind);

impl UserData for Builder {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        fn chain(
            name: &'static str,
            apply: fn(&mut Kind, Value) -> Result<(), String>,
        ) -> impl Fn(&Lua, (AnyUserData, Value)) -> mlua::Result<AnyUserData> {
            move |_, (builder, value): (AnyUserData, Value)| {
                apply(&mut builder.borrow_mut::<Builder>()?.0, value).map_err(|why| mlua::Error::runtime(format!(":{name}: {why}")))?;
                Ok(builder)
            }
        }
        methods.add_function(
            "step",
            chain("step", |kind, value| match kind {
                Kind::Range { step, .. } | Kind::Number { step, .. } => {
                    let n = positive(&value)?;
                    *step = Some(n);
                    Ok(())
                }
                other => Err(format!("doesn't apply to a {} option", other.name())),
            }),
        );
        methods.add_function(
            "unit",
            chain("unit", |kind, value| match kind {
                Kind::Range { unit, .. } | Kind::Number { unit, .. } => {
                    *unit = Some(text(&value)?);
                    Ok(())
                }
                other => Err(format!("doesn't apply to a {} option", other.name())),
            }),
        );
        methods.add_function(
            "min",
            chain("min", |kind, value| match kind {
                Kind::Number { min, .. } => {
                    *min = Some(number(&value)?);
                    Ok(())
                }
                other => Err(format!("doesn't apply to a {} option", other.name())),
            }),
        );
        methods.add_function(
            "max",
            chain("max", |kind, value| match kind {
                Kind::Number { max, .. } => {
                    *max = Some(number(&value)?);
                    Ok(())
                }
                other => Err(format!("doesn't apply to a {} option", other.name())),
            }),
        );
        methods.add_function(
            "placeholder",
            chain("placeholder", |kind, value| match kind {
                Kind::Text { placeholder, .. } => {
                    *placeholder = Some(text(&value)?);
                    Ok(())
                }
                other => Err(format!("doesn't apply to a {} option", other.name())),
            }),
        );
        methods.add_function(
            "max_length",
            chain("max_length", |kind, value| match kind {
                Kind::Text { max_length, .. } => {
                    let n = positive(&value)?;
                    if n.fract() != 0.0 {
                        return Err(format!("{n} isn't a whole number"));
                    }
                    *max_length = Some(n as u32);
                    Ok(())
                }
                other => Err(format!("doesn't apply to a {} option", other.name())),
            }),
        );
        methods.add_function(
            "alpha",
            chain("alpha", |kind, value| match kind {
                Kind::Color { alpha, .. } => match value {
                    Value::Boolean(b) => {
                        *alpha = b;
                        Ok(())
                    }
                    other => Err(format!("takes a boolean, not a {}", other.type_name())),
                },
                other => Err(format!("doesn't apply to a {} option", other.name())),
            }),
        );
    }
}

fn number(value: &Value) -> Result<f64, String> {
    match value {
        Value::Number(n) => Ok(*n),
        Value::Integer(n) => Ok(*n as f64),
        other => Err(format!("takes a number, not a {}", other.type_name())),
    }
}

fn positive(value: &Value) -> Result<f64, String> {
    let n = number(value)?;
    if n > 0.0 { Ok(n) } else { Err(format!("takes a number above 0, not {n}")) }
}

fn text(value: &Value) -> Result<String, String> {
    match value {
        Value::String(s) => s.to_str().map(|s| s.to_owned()).map_err(|e| e.to_string()),
        other => Err(format!("takes a string, not a {}", other.type_name())),
    }
}

/// What `option(...)` returns, for `:visible(...)` and `:hint(...)`.
struct Handle {
    state: Rc<RefCell<State>>,
    index: usize,
}

impl UserData for Handle {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_function("visible", |_, (handle, shown): (AnyUserData, bool)| {
            {
                let h = handle.borrow::<Handle>()?;
                h.state.borrow_mut().options[h.index].visible = shown;
            }
            Ok(handle)
        });
        methods.add_function("hint", |_, (handle, hint): (AnyUserData, String)| {
            {
                let h = handle.borrow::<Handle>()?;
                h.state.borrow_mut().options[h.index].hint = Some(hint);
            }
            Ok(handle)
        });
    }
}

/// A section being built; options are indexes into `State::options`.
struct Frame {
    path: String,
    label: String,
    items: Vec<Built>,
}

enum Built {
    Section(Frame),
    Option(usize),
    Heading(String),
}

struct State {
    stack: Vec<Frame>,
    options: Vec<OptionItem>,
    paths: BTreeSet<String>,
}

fn execute(source: &str, values: &Values) -> Result<Panel> {
    let vm = Vm::new()?;
    let state = Rc::new(RefCell::new(State {
        stack: vec![Frame { path: String::new(), label: String::new(), items: Vec::new() }],
        options: Vec::new(),
        paths: BTreeSet::new(),
    }));
    let lua = vm.lua();
    install_builders(lua)?;
    remove_impure(lua)?;
    vm.seal()?;
    let current = guarded_table(lua, "current")?;
    lua.globals().set("current", &current).lua_context("injecting current")?;
    install_structure(lua, &state, &current, values)?;
    vm.load::<()>(source, "options.luau")?;

    let mut state = state.replace(State { stack: Vec::new(), options: Vec::new(), paths: BTreeSet::new() });
    ensure!(state.stack.len() == 1, "a section didn't finish");
    for option in &state.options {
        check_kind(option).with_context(|| format!("option {}", option.path))?;
    }
    let root = state.stack.pop().expect("the root frame");
    Ok(Panel { items: finish(root.items, &state.options) })
}

fn finish(items: Vec<Built>, options: &[OptionItem]) -> Vec<Item> {
    items
        .into_iter()
        .map(|built| match built {
            Built::Option(index) => Item::Option(options[index].clone()),
            Built::Heading(text) => Item::Heading(text),
            Built::Section(frame) => Item::Section { path: frame.path, label: frame.label, items: finish(frame.items, options) },
        })
        .collect()
}

fn check_kind(option: &OptionItem) -> Result<()> {
    match &option.kind {
        Kind::Range { default, min, max, .. } => {
            ensure!(min < max, "its range {min} to {max} is empty");
            ensure!((min..=max).contains(&default), "its default {default} is outside {min} to {max}");
        }
        Kind::Number { default, min, max, .. } => {
            if let (Some(min), Some(max)) = (min, max) {
                ensure!(min <= max, "its limits {min} to {max} are the wrong way round");
            }
            ensure!(option.kind.refuses(&option.kind.default_value()).is_none(), "its default {default} is outside its limits");
        }
        Kind::Choice { default, values } => {
            ensure!(!values.is_empty(), "it has no values to choose from");
            let unique: BTreeSet<&String> = values.iter().collect();
            ensure!(unique.len() == values.len(), "a value is listed twice");
            ensure!(values.contains(default), "its default {default:?} isn't one of {}", values.join(", "));
        }
        Kind::Text { .. } => {
            ensure!(option.kind.refuses(&option.kind.default_value()).is_none(), "its default is longer than its max_length")
        }
        Kind::Color { .. } | Kind::Toggle { .. } => {}
    }
    Ok(())
}

/// A table whose missing keys are an error naming the path, so a misspelt or later option is reported.
fn guarded_table(lua: &Lua, path: &str) -> Result<Table> {
    let table = lua.create_table().lua_context("creating current")?;
    let metatable = lua.create_table().lua_context("creating current")?;
    let path = path.to_owned();
    let missing = lua
        .create_function(move |_, (_, key): (Table, String)| -> mlua::Result<()> {
            Err(mlua::Error::runtime(format!("{path}.{key} isn't declared yet; options read only those declared above them")))
        })
        .lua_context("creating current")?;
    metatable.raw_set(MetaMethod::Index.name(), missing).lua_context("creating current")?;
    table.set_metatable(Some(metatable)).lua_context("creating current")?;
    Ok(table)
}

/// A run must depend only on `current`, so the app's diffs are real: no clock, no randomness.
fn remove_impure(lua: &Lua) -> Result<()> {
    let globals = lua.globals();
    globals.set("os", Value::Nil).lua_context("removing os")?;
    let math: Table = globals.get("math").lua_context("finding math")?;
    for name in ["random", "randomseed"] {
        math.set(name, Value::Nil).lua_context("removing math.random")?;
    }
    Ok(())
}

fn install_builders(lua: &Lua) -> Result<()> {
    let globals = lua.globals();
    let builder = |kind: Kind| Builder(kind);
    let functions: [(&str, Function); 6] = [
        (
            "color",
            lua.create_function(move |_, value: Value| match value {
                Value::String(s) => Ok(builder(Kind::Color { default: parse_color(&s.to_str()?).map_err(runtime)?, alpha: true })),
                Value::Vector(v) => Ok(builder(Kind::Color { default: [v.x(), v.y(), v.z(), v.w()], alpha: true })),
                other => Err(mlua::Error::runtime(format!("color takes \"#rrggbb\" or a vector, not a {}", other.type_name()))),
            })
            .lua_context("creating color")?,
        ),
        (
            "range",
            lua.create_function(|_, (default, min, max): (f64, f64, f64)| {
                Ok(Builder(Kind::Range { default, min, max, step: None, unit: None }))
            })
            .lua_context("creating range")?,
        ),
        (
            "number",
            lua.create_function(|_, default: f64| Ok(Builder(Kind::Number { default, min: None, max: None, step: None, unit: None })))
                .lua_context("creating number")?,
        ),
        ("toggle", lua.create_function(|_, default: bool| Ok(Builder(Kind::Toggle { default }))).lua_context("creating toggle")?),
        (
            "text",
            lua.create_function(|_, default: String| Ok(Builder(Kind::Text { default, placeholder: None, max_length: None })))
                .lua_context("creating text")?,
        ),
        (
            "choice",
            lua.create_function(|_, (default, values): (String, Vec<String>)| Ok(Builder(Kind::Choice { default, values })))
                .lua_context("creating choice")?,
        ),
    ];
    for (name, function) in functions {
        globals.set(name, function).lua_context("installing the option builders")?;
    }
    Ok(())
}

fn runtime(e: anyhow::Error) -> mlua::Error {
    mlua::Error::runtime(format!("{e:#}"))
}

/// `option`, `section` and `heading`, which record into `state` and fill `current` as they go.
fn install_structure(lua: &Lua, state: &Rc<RefCell<State>>, current: &Table, values: &Values) -> Result<()> {
    let globals = lua.globals();
    let tables: Rc<RefCell<BTreeMap<String, Table>>> = Rc::new(RefCell::new(BTreeMap::from([(String::new(), current.clone())])));

    let recording = Rc::clone(state);
    let known = Rc::clone(&tables);
    let values = values.clone();
    let option = lua
        .create_function(move |lua, (key, second, third): (String, Value, Option<Value>)| {
            check_key(&key).map_err(runtime)?;
            let (label, builder) = match third {
                Some(builder) => (text(&second).map_err(|why| mlua::Error::runtime(format!("option {key}: the label {why}")))?, builder),
                None => (label_from_key(&key), second),
            };
            let Value::UserData(builder) = builder else {
                return Err(mlua::Error::runtime(format!("option {key} needs a builder such as range(...) or color(...)")));
            };
            let kind = builder
                .borrow::<Builder>()
                .map_err(|_| mlua::Error::runtime(format!("option {key} needs a builder such as range(...) or color(...)")))?
                .0
                .clone();
            let mut state = recording.borrow_mut();
            let parent = state.stack.last().expect("the root frame").path.clone();
            let path = if parent.is_empty() { key.clone() } else { format!("{parent}.{key}") };
            if !state.paths.insert(path.clone()) {
                return Err(mlua::Error::runtime(format!("{path} is declared twice")));
            }
            let value = values.get(&path).cloned().unwrap_or_else(|| kind.default_value());
            known.borrow()[&parent].raw_set(key, value.to_lua(lua)?)?;
            let index = state.options.len();
            state.options.push(OptionItem { path, label, kind, visible: true, hint: None });
            state.stack.last_mut().expect("the root frame").items.push(Built::Option(index));
            Ok(Handle { state: Rc::clone(&recording), index })
        })
        .lua_context("creating option")?;
    globals.set("option", option).lua_context("installing option")?;

    let recording = Rc::clone(state);
    let known = Rc::clone(&tables);
    let section = lua
        .create_function(move |lua, (key, second, third): (String, Value, Option<Function>)| {
            check_key(&key).map_err(runtime)?;
            let (label, body) = match (third, second) {
                (Some(body), label) => (text(&label).map_err(|why| mlua::Error::runtime(format!("section {key}: the label {why}")))?, body),
                (None, Value::Function(body)) => (label_from_key(&key), body),
                (None, _) => return Err(mlua::Error::runtime(format!("section {key} needs a function holding its options"))),
            };
            let path = {
                let mut state = recording.borrow_mut();
                let parent = state.stack.last().expect("the root frame").path.clone();
                let path = if parent.is_empty() { key.clone() } else { format!("{parent}.{key}") };
                if !state.paths.insert(path.clone()) {
                    return Err(mlua::Error::runtime(format!("{path} is declared twice")));
                }
                let table = guarded_table(lua, &format!("current.{path}")).map_err(runtime)?;
                known.borrow()[&parent].raw_set(key, &table)?;
                known.borrow_mut().insert(path.clone(), table);
                state.stack.push(Frame { path: path.clone(), label, items: Vec::new() });
                path
            };
            body.call::<()>(())?;
            let mut state = recording.borrow_mut();
            let frame = state.stack.pop().expect("the section's frame");
            debug_assert_eq!(frame.path, path);
            state.stack.last_mut().expect("the root frame").items.push(Built::Section(frame));
            Ok(())
        })
        .lua_context("creating section")?;
    globals.set("section", section).lua_context("installing section")?;

    let recording = Rc::clone(state);
    let heading = lua
        .create_function(move |_, text: String| {
            recording.borrow_mut().stack.last_mut().expect("the root frame").items.push(Built::Heading(text));
            Ok(())
        })
        .lua_context("creating heading")?;
    globals.set("heading", heading).lua_context("installing heading")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const GRADIENT: &str = r##"
        section("colours", function()
            option("night", "Night sky", color("#0f0a29"))
            option("palette", choice("dusk", { "dusk", "dawn", "custom" }))
            option("accent", color("#d66b78")):visible(current.colours.palette == "custom")
        end)
        heading("Movement")
        section("motion", function()
            option("drift_speed", range(0.08, 0, 0.5):step(0.01))
            section("rings", "Ring effects", function()
                option("on", toggle(true))
                option("speed", range(1, 0, 4)):visible(current.motion.rings.on):hint("How fast rings grow")
            end)
        end)
    "##;

    fn script() -> OptionsScript {
        OptionsScript::new(GRADIENT)
    }

    #[test]
    fn the_first_pass_records_the_panel_in_order_with_defaults() {
        let panel = script().first_pass().unwrap();
        let paths: Vec<&str> = panel.options().iter().map(|o| o.path.as_str()).collect();
        assert_eq!(
            paths,
            ["colours.night", "colours.palette", "colours.accent", "motion.drift_speed", "motion.rings.on", "motion.rings.speed"]
        );
        let options = panel.options();
        assert_eq!(options[0].label, "Night sky");
        assert_eq!(options[3].label, "Drift speed");
        assert!(!options[2].visible, "the accent shows only for the custom palette");
        assert!(options[5].visible);
        assert_eq!(options[5].hint.as_deref(), Some("How fast rings grow"));
        assert!(matches!(panel.items[1], Item::Heading(ref t) if t == "Movement"));
        let Item::Section { items, .. } = &panel.items[2] else { panic!("motion is a section") };
        assert!(matches!(&items[1], Item::Section { label, .. } if label == "Ring effects"));
        assert_eq!(panel.defaults()["motion.drift_speed"], OptionValue::Number(0.08));
    }

    #[test]
    fn a_rerun_with_values_changes_only_what_is_shown() {
        let script = script();
        let first = script.first_pass().unwrap();
        let chosen = Values::from([
            ("colours.palette".to_owned(), OptionValue::Text("custom".into())),
            ("motion.rings.on".to_owned(), OptionValue::Bool(false)),
        ]);
        let resolved = first.resolve(&chosen);
        assert!(resolved.dropped.is_empty());
        let panel = script.run(&first, &resolved.values).unwrap();
        let options = panel.options();
        assert!(options[2].visible && !options[5].visible);
    }

    #[test]
    fn a_run_that_declares_different_options_is_refused() {
        let script = OptionsScript::new(
            r#"option("mode", choice("a", { "a", "b" })) if current.mode == "b" then option("extra", toggle(true)) end"#,
        );
        let first = script.first_pass().unwrap();
        let values = first.resolve(&Values::from([("mode".to_owned(), OptionValue::Text("b".into()))])).values;
        let error = format!("{:#}", script.run(&first, &values).unwrap_err());
        assert!(error.contains("new: extra") && error.contains("only visibility may change"), "{error}");
    }

    #[test]
    fn values_that_no_longer_fit_are_dropped_with_why() {
        let first = script().first_pass().unwrap();
        let chosen = Values::from([
            ("motion.drift_speed".to_owned(), OptionValue::Number(9.0)),
            ("colours.palette".to_owned(), OptionValue::Text("noon".into())),
            ("gone".to_owned(), OptionValue::Bool(true)),
            ("colours.night".to_owned(), OptionValue::Number(1.0)),
        ]);
        let resolved = first.resolve(&chosen);
        assert_eq!(resolved.dropped.len(), 4, "{:?}", resolved.dropped);
        assert_eq!(resolved.values["motion.drift_speed"], OptionValue::Number(0.08));
    }

    #[test]
    fn mistakes_are_named() {
        let error = |source: &str| format!("{:#}", OptionsScript::new(source).first_pass().unwrap_err());
        assert!(error(r#"option("a", range(5, 0, 1))"#).contains("default 5 is outside 0 to 1"));
        assert!(error(r#"option("a", toggle(true):step(1))"#).contains(":step: doesn't apply to a toggle option"));
        assert!(error(r#"option("a", toggle(true)) option("a", toggle(false))"#).contains("a is declared twice"));
        assert!(error(r#"option("Bad", toggle(true))"#).contains("isn't a key"));
        assert!(error(r#"option("a", toggle(current.b))"#).contains("current.b isn't declared yet"));
        assert!(error(r#"option("c", choice("x", { "y" }))"#).contains("isn't one of y"));
        assert!(error(r#"option("c", color("red"))"#).contains("must start with #"));
        let clock = error("local t = os.clock()");
        assert!(clock.contains("attempt to index nil"), "the VM has no clock: {clock}");
        let random = error("local n = math.random()");
        assert!(random.contains("attempt to call a nil value"), "the VM has no randomness: {random}");
    }

    #[test]
    fn definitions_mirror_the_sections_with_exact_choices() {
        let definitions = script().first_pass().unwrap().definitions();
        assert!(definitions.contains("export type Options = {\n    colours: {\n        night: Color,\n        palette: \"dusk\" | \"dawn\" | \"custom\",\n        accent: Color,\n    },\n    motion: {\n        drift_speed: number,\n        rings: {\n            on: boolean,\n            speed: number,\n        },\n    },\n}\n"), "{definitions}");
        assert!(definitions.contains("export type Changed = {\n    colours: {\n        night: boolean,"), "{definitions}");
    }

    #[test]
    fn options_and_changed_nest_by_section_for_the_host() {
        let first = script().first_pass().unwrap();
        let values = first.resolve(&Values::from([("motion.rings.speed".to_owned(), OptionValue::Number(3.0))])).values;
        let lua = Lua::new();
        lua.globals().set("options", options_table(&lua, &first, &values).unwrap()).unwrap();
        let changed = BTreeSet::from(["motion.rings.speed".to_owned()]);
        lua.globals().set("changed", changed_table(&lua, &first, Some(&changed)).unwrap()).unwrap();
        lua.globals().set("any", any_function(&lua).unwrap()).unwrap();
        let check: bool = lua
            .load("return options.motion.rings.speed == 3 and options.colours.palette == 'dusk' and changed.motion.rings.speed and not changed.colours.night and any(changed.motion) and not any(changed.colours)")
            .eval()
            .unwrap();
        assert!(check);
        let all = changed_table(&lua, &first, None).unwrap();
        lua.globals().set("all", all).unwrap();
        assert!(lua.load("return all.colours.night and all.motion.rings.on").eval::<bool>().unwrap());
    }

    #[test]
    fn stored_values_round_trip_through_json_for_their_kind() {
        let color = Kind::Color { default: [0.0; 4], alpha: true };
        let value = OptionValue::from_json(&color, &serde_json::json!("#0f0a29ff")).unwrap();
        assert_eq!(value.to_json(), serde_json::json!("#0f0a29ff"));
        assert!(OptionValue::from_json(&Kind::Toggle { default: true }, &serde_json::json!("yes")).is_err());
    }

    #[test]
    fn labels_and_colours_round_trip() {
        assert_eq!(label_from_key("drift_speed"), "Drift speed");
        assert_eq!(label_from_key("on"), "On");
        let color = parse_color("#0f0a29").unwrap();
        assert_eq!(format_color(color), "#0f0a29ff");
        for bad in ["#aéaaa", "#éa", "#+fffff", "#12345"] {
            assert!(parse_color(bad).is_err(), "{bad}");
        }
    }
}
