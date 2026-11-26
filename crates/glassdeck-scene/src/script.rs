//! A scene's `keyboard.luau`: it owns the scene's data and writes the shader's inputs through the
//! injected `inputs`. Writes are recorded, and the renderer uploads only what changed.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use anyhow::{Context, Result, bail, ensure};
use glassdeck_luau::{LuaResultExt, Vm};
use glassdeck_package::{InputField, InputKind};
use mlua::{AnyUserData, Function, Lua, Table, UserData, Value, Vector};

/// A value the script wrote to one input, ready to upload.
#[derive(Clone, Debug, PartialEq)]
pub enum InputValue {
    Float(Vec<f32>),
    Uint(u32),
    Array(Vec<f32>),
}

pub struct SceneScript {
    vm: Vm,
    settings: Table,
    frame: Table,
    on_settings: Option<Function>,
    on_frame: Option<Function>,
    inputs: Rc<RefCell<Inputs>>,
}

struct Inputs {
    kinds: BTreeMap<String, InputKind>,
    written: BTreeSet<String>,
    changed: BTreeMap<String, InputValue>,
}

/// What `setting.color(...)`, `setting.range(...)` and `setting.choice(...)` return, read back from `scene.settings`.
#[derive(Clone, Debug)]
enum Declared {
    Color(Vector),
    Range { default: f64 },
    Choice { default: String },
}

impl UserData for Declared {}

impl SceneScript {
    pub fn load(source: &str, inputs: &[InputField]) -> Result<Self> {
        let vm = Vm::new()?;
        install_library(vm.lua())?;
        vm.seal()?;
        let lua = vm.lua();
        let scene = lua.create_table().lua_context("creating scene")?;
        let state = Rc::new(RefCell::new(Inputs {
            kinds: inputs.iter().map(|f| (f.name.clone(), f.kind)).collect(),
            written: BTreeSet::new(),
            changed: BTreeMap::new(),
        }));
        let globals = lua.globals();
        globals.set("scene", &scene).lua_context("injecting scene")?;
        globals.set("inputs", inputs_table(lua, Rc::clone(&state))?).lua_context("injecting inputs")?;
        vm.load::<()>(source, "keyboard.luau")?;

        let settings = read_settings(lua, &scene)?;
        let callback = |name: &str| -> Result<Option<Function>> {
            match scene.get::<Value>(name).lua_context("reading scene")? {
                Value::Nil => Ok(None),
                Value::Function(f) => Ok(Some(f)),
                other => bail!("scene.{name} is a {}, not a function", other.type_name()),
            }
        };
        let (on_settings, on_frame) = (callback("on_settings")?, callback("on_frame")?);
        let frame = lua.create_table().lua_context("creating frame")?;
        Ok(Self { vm, settings, frame, on_settings, on_frame, inputs: state })
    }

    /// Hands the script its settings, as it gets them before the first draw and after each change.
    pub fn apply_settings(&self) -> Result<()> {
        match &self.on_settings {
            Some(f) => self.vm.call::<()>(f, &self.settings).context("in scene.on_settings"),
            None => Ok(()),
        }
    }

    pub fn frame(&self, time: f32, delta: f32, index: u32) -> Result<()> {
        let Some(on_frame) = &self.on_frame else { return Ok(()) };
        for (key, value) in [("time", f64::from(time)), ("delta", f64::from(delta)), ("index", f64::from(index))] {
            self.frame.raw_set(key, value).lua_context("filling frame")?;
        }
        self.vm.call::<()>(on_frame, (&self.frame, &self.settings)).context("in scene.on_frame")
    }

    /// The inputs written since the last call, each with its latest value.
    pub fn take_changes(&self) -> BTreeMap<String, InputValue> {
        std::mem::take(&mut self.inputs.borrow_mut().changed)
    }

    /// Inputs the script has never written.
    pub fn unwritten(&self) -> Vec<String> {
        let inputs = self.inputs.borrow();
        inputs.kinds.keys().filter(|name| !inputs.written.contains(*name)).cloned().collect()
    }
}

fn inputs_table(lua: &Lua, state: Rc<RefCell<Inputs>>) -> Result<Table> {
    let inputs = lua.create_table().lua_context("creating inputs")?;
    let store = lua.create_table().lua_context("creating inputs")?;
    let metatable = lua.create_table().lua_context("creating inputs")?;
    let written = store.clone();
    let write = lua
        .create_function(move |_, (_, name, value): (Table, String, Value)| {
            let mut state = state.borrow_mut();
            let kind = *state.kinds.get(&name).ok_or_else(|| mlua::Error::runtime(format!("the shader has no input {name}")))?;
            let converted = convert(&name, kind, &value).map_err(|e| mlua::Error::runtime(format!("{e:#}")))?;
            state.written.insert(name.clone());
            state.changed.insert(name.clone(), converted);
            written.raw_set(name, value)
        })
        .lua_context("creating inputs")?;
    metatable.raw_set("__newindex", write).lua_context("creating inputs")?;
    metatable.raw_set("__index", store).lua_context("creating inputs")?;
    inputs.set_metatable(Some(metatable)).lua_context("creating inputs")?;
    Ok(inputs)
}

fn convert(name: &str, kind: InputKind, value: &Value) -> Result<InputValue> {
    Ok(match (kind, value) {
        (InputKind::Float { components: 1 }, _) => InputValue::Float(vec![number(name, value)? as f32]),
        (InputKind::Float { components }, Value::Vector(v)) => {
            let all = [v.x(), v.y(), v.z(), v.w()];
            InputValue::Float(all[..usize::from(components)].to_vec())
        }
        (InputKind::Float { components }, other) => bail!("input {name} takes a vector ({components} wide), not a {}", other.type_name()),
        (InputKind::Uint, _) => {
            let n = number(name, value)?;
            ensure!(n >= 0.0 && n.fract() == 0.0 && n <= f64::from(u32::MAX), "input {name} takes a whole number from 0, not {n}");
            InputValue::Uint(n as u32)
        }
        (InputKind::FloatArray { len }, Value::Table(t)) => {
            let values: Vec<f32> = t
                .sequence_values::<f64>()
                .map(|v| v.map(|v| v as f32))
                .collect::<mlua::Result<_>>()
                .lua_context(&format!("input {name}"))?;
            ensure!(values.len() == len as usize, "input {name} takes {len} numbers, not {}", values.len());
            InputValue::Array(values)
        }
        (InputKind::FloatArray { len }, other) => bail!("input {name} takes a table of {len} numbers, not a {}", other.type_name()),
    })
}

fn number(name: &str, value: &Value) -> Result<f64> {
    match value {
        Value::Number(n) => Ok(*n),
        Value::Integer(n) => Ok(*n as f64),
        other => bail!("input {name} takes a number, not a {}", other.type_name()),
    }
}

/// `scene.settings` as the values `on_settings` gets. The keyboard has no chosen values yet, so these are the defaults.
fn read_settings(lua: &Lua, scene: &Table) -> Result<Table> {
    let values = lua.create_table().lua_context("creating settings")?;
    let declared: Option<Table> = scene.get("settings").lua_context("scene.settings must be a table")?;
    let Some(declared) = declared else { return Ok(values) };
    for pair in declared.pairs::<String, Value>() {
        let (name, value) = pair.lua_context("scene.settings must be keyed by name")?;
        let value = match value {
            Value::UserData(declared) => match declared_setting(&declared)? {
                Declared::Color(c) => Value::Vector(c),
                Declared::Range { default } => Value::Number(default),
                Declared::Choice { default } => Value::String(lua.create_string(&default).lua_context("reading settings")?),
            },
            plain @ (Value::Number(_) | Value::Integer(_) | Value::Boolean(_) | Value::String(_)) => plain,
            other => bail!("setting {name} is a {}; use a number, boolean, string or setting.*", other.type_name()),
        };
        values.raw_set(name, value).lua_context("reading settings")?;
    }
    Ok(values)
}

fn declared_setting(value: &AnyUserData) -> Result<Declared> {
    Ok(value.borrow::<Declared>().lua_context("a setting must be made by setting.*")?.clone())
}

/// `setting`, `color`, `vector` and `drift`, installed before the VM is sealed.
fn install_library(lua: &Lua) -> Result<()> {
    let globals = lua.globals();
    let setting = lua.create_table().lua_context("creating setting")?;
    setting
        .set(
            "color",
            lua.create_function(|_, value: Value| Ok(Declared::Color(color_of(&value).map_err(runtime)?))).lua_context("setting.color")?,
        )
        .lua_context("setting.color")?;
    setting
        .set(
            "range",
            lua.create_function(|_, (default, min, max): (f64, f64, f64)| {
                if !(min <= default && default <= max) {
                    return Err(runtime(anyhow::anyhow!("setting.range: {default} isn't between {min} and {max}")));
                }
                Ok(Declared::Range { default })
            })
            .lua_context("setting.range")?,
        )
        .lua_context("setting.range")?;
    setting
        .set(
            "choice",
            lua.create_function(|_, (default, options): (String, Vec<String>)| {
                if !options.contains(&default) {
                    return Err(runtime(anyhow::anyhow!("setting.choice: {default:?} isn't one of the options")));
                }
                Ok(Declared::Choice { default })
            })
            .lua_context("setting.choice")?,
        )
        .lua_context("setting.choice")?;
    globals.set("setting", setting).lua_context("installing setting")?;

    let color = lua.create_table().lua_context("creating color")?;
    color
        .set("hex", lua.create_function(|_, hex: String| parse_hex(&hex).map_err(runtime)).lua_context("color.hex")?)
        .lua_context("color.hex")?;
    color
        .set(
            "rgb",
            lua.create_function(|_, (r, g, b, a): (f32, f32, f32, Option<f32>)| {
                Ok(Vector::new(r / 255.0, g / 255.0, b / 255.0, a.unwrap_or(255.0) / 255.0))
            })
            .lua_context("color.rgb")?,
        )
        .lua_context("color.rgb")?;
    color
        .set(
            "hsv",
            lua.create_function(|_, (h, s, v, a): (f32, f32, f32, Option<f32>)| Ok(hsv(h, s, v, a.unwrap_or(1.0))))
                .lua_context("color.hsv")?,
        )
        .lua_context("color.hsv")?;
    globals.set("color", color).lua_context("installing color")?;

    globals
        .set(
            "vector",
            lua.create_function(|_, (x, y, z, w): (f32, f32, Option<f32>, Option<f32>)| {
                Ok(Vector::new(x, y, z.unwrap_or(0.0), w.unwrap_or(0.0)))
            })
            .lua_context("vector")?,
        )
        .lua_context("installing vector")?;
    globals
        .set(
            "drift",
            lua.create_function(|_, (t, centre, amount, speed, phase): (f32, Vector, Vector, Vector, Option<Vector>)| {
                Ok(drift(t, centre, amount, speed, phase.unwrap_or_default()))
            })
            .lua_context("drift")?,
        )
        .lua_context("installing drift")?;
    Ok(())
}

fn runtime(e: anyhow::Error) -> mlua::Error {
    mlua::Error::runtime(format!("{e:#}"))
}

/// Wanders around `centre` by up to `amount`: x follows `sin(t * speed.x + phase.x)`, y `cos(t * speed.y + phase.y)`.
fn drift(t: f32, centre: Vector, amount: Vector, speed: Vector, phase: Vector) -> Vector {
    let x = centre.x() + amount.x() * (t * speed.x() + phase.x()).sin();
    let y = centre.y() + amount.y() * (t * speed.y() + phase.y()).cos();
    Vector::new(x, y, 0.0, 0.0)
}

fn color_of(value: &Value) -> Result<Vector> {
    match value {
        Value::String(s) => parse_hex(&s.to_str().lua_context("a colour string must be UTF-8")?),
        Value::Vector(v) => Ok(*v),
        other => bail!("setting.color takes \"#rrggbb\", color.rgb(...) or color.hsv(...), not a {}", other.type_name()),
    }
}

/// `#rgb`, `#rrggbb` or `#rrggbbaa`, as sRGB from 0 to 1.
fn parse_hex(text: &str) -> Result<Vector> {
    let digits = text.strip_prefix('#').with_context(|| format!("colour {text:?} must start with #"))?;
    let expanded: String = match digits.len() {
        3 => digits.chars().flat_map(|c| [c, c]).chain("ff".chars()).collect(),
        6 => format!("{digits}ff"),
        8 => digits.to_owned(),
        _ => bail!("colour {text:?} must be #rgb, #rrggbb or #rrggbbaa"),
    };
    let channel = |i: usize| -> Result<f32> {
        let byte =
            u8::from_str_radix(&expanded[i * 2..i * 2 + 2], 16).with_context(|| format!("colour {text:?} has a digit that isn't hex"))?;
        Ok(f32::from(byte) / 255.0)
    };
    Ok(Vector::new(channel(0)?, channel(1)?, channel(2)?, channel(3)?))
}

/// Hue in degrees, saturation and value from 0 to 1.
fn hsv(h: f32, s: f32, v: f32, a: f32) -> Vector {
    let h = h.rem_euclid(360.0) / 60.0;
    let c = v * s;
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    Vector::new(r + m, g + m, b + m, a)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields() -> Vec<InputField> {
        vec![
            InputField { name: "night".into(), kind: InputKind::Float { components: 3 } },
            InputField { name: "roseAt".into(), kind: InputKind::Float { components: 2 } },
            InputField { name: "bands".into(), kind: InputKind::FloatArray { len: 2 } },
        ]
    }

    const GRADIENT: &str = r##"
        scene.settings = { night = setting.color("#ff8000"), speed = setting.range(0.5, 0, 1), title = "hi" }
        function scene.on_settings(settings) inputs.night = settings.night end
        function scene.on_frame(frame, settings)
            inputs.roseAt = drift(frame.time * settings.speed, vector(0.5, 0.3), vector(0.1, 0.1), vector(1, 1))
        end
    "##;

    #[test]
    fn settings_feed_inputs_only_when_applied_and_frames_only_write_what_they_touch() {
        let script = SceneScript::load(GRADIENT, &fields()).unwrap();
        assert!(script.take_changes().is_empty());
        script.apply_settings().unwrap();
        let changes = script.take_changes();
        assert_eq!(changes.len(), 1);
        let InputValue::Float(night) = &changes["night"] else { panic!("{changes:?}") };
        assert_eq!(night.len(), 3);
        assert!((night[0] - 1.0).abs() < 1e-6 && (night[1] - 128.0 / 255.0).abs() < 1e-6 && night[2] == 0.0);

        script.frame(0.0, 0.0, 0).unwrap();
        let changes = script.take_changes();
        assert_eq!(changes.keys().collect::<Vec<_>>(), ["roseAt"]);
        assert_eq!(changes["roseAt"], InputValue::Float(vec![0.5, 0.4]));
        assert_eq!(script.unwritten(), ["bands"]);
    }

    #[test]
    fn wrong_names_and_types_are_errors_naming_the_input() {
        let script = SceneScript::load("function scene.on_frame() inputs.nite = vector(1, 2, 3) end", &fields()).unwrap();
        assert!(format!("{:#}", script.frame(0.0, 0.0, 0).unwrap_err()).contains("the shader has no input nite"));
        let script = SceneScript::load("function scene.on_frame() inputs.night = 3 end", &fields()).unwrap();
        assert!(format!("{:#}", script.frame(0.0, 0.0, 0).unwrap_err()).contains("input night takes a vector"));
        let script = SceneScript::load("function scene.on_frame() inputs.bands = { 1 } end", &fields()).unwrap();
        assert!(format!("{:#}", script.frame(0.0, 0.0, 0).unwrap_err()).contains("takes 2 numbers, not 1"));
    }

    #[test]
    fn a_script_without_on_frame_does_nothing_per_frame() {
        let script = SceneScript::load("scene.settings = { night = setting.color('#000') }", &fields()).unwrap();
        script.frame(1.0, 0.1, 1).unwrap();
        assert!(script.take_changes().is_empty());
    }

    #[test]
    fn bad_settings_are_refused_when_loading() {
        assert!(SceneScript::load("scene.settings = { a = setting.color('red') }", &fields()).is_err());
        assert!(SceneScript::load("scene.settings = { a = setting.range(2, 0, 1) }", &fields()).is_err());
        assert!(SceneScript::load("scene.settings = { a = {} }", &fields()).is_err());
        assert!(SceneScript::load("scene.on_frame = 3", &fields()).is_err());
    }

    #[test]
    fn colours_parse_from_every_form() {
        assert_eq!(parse_hex("#fff").unwrap(), Vector::new(1.0, 1.0, 1.0, 1.0));
        assert_eq!(parse_hex("#00000080").unwrap().w(), 128.0 / 255.0);
        assert!(parse_hex("#12345").is_err() && parse_hex("123456").is_err() && parse_hex("#gg0000").is_err());
        let red = hsv(0.0, 1.0, 1.0, 1.0);
        assert_eq!((red.x(), red.y(), red.z()), (1.0, 0.0, 0.0));
        let script = SceneScript::load("scene.settings = { c = setting.color(color.rgb(255, 0, 0)) }", &fields()).unwrap();
        assert_eq!(script.settings.get::<Vector>("c").unwrap(), Vector::new(1.0, 0.0, 0.0, 1.0));
    }
}
