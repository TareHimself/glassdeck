//! A canvas's `keyboard.luau`: it holds the canvas's state, takes what the host half sends in
//! `canvas.on_data`, and writes the shader's inputs through the injected `inputs`. Writes are
//! recorded, and the renderer uploads only what changed.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use anyhow::{Context, Result, bail, ensure};
use glassdeck_luau::color::color_api;
use glassdeck_luau::{LuaResultExt, Vm, decode};
use glassdeck_package::{InputField, InputKind};
use mlua::{Function, Lua, Table, Value, Vector};

/// A value the script wrote to one input, ready to upload.
#[derive(Clone, Debug, PartialEq)]
pub enum InputValue {
    Float(Vec<f32>),
    Uint(u32),
    Array(Vec<f32>),
}

pub struct CanvasScript {
    vm: Vm,
    frame: Table,
    callbacks: Rc<RefCell<Callbacks>>,
    inputs: Rc<RefCell<Inputs>>,
}

/// What the script registered with `canvas.on_data(...)` and `canvas.on_frame(...)`.
#[derive(Default)]
struct Callbacks {
    on_data: Option<Function>,
    on_frame: Option<Function>,
}

const CALLBACKS: [&str; 2] = ["on_data", "on_frame"];

struct Inputs {
    kinds: BTreeMap<String, InputKind>,
    written: BTreeSet<String>,
    changed: BTreeMap<String, InputValue>,
}

impl CanvasScript {
    pub fn load(source: &str, inputs: &[InputField]) -> Result<Self> {
        let vm = Vm::new()?;
        install_library(vm.lua())?;
        vm.seal()?;
        let lua = vm.lua();
        let canvas = lua.create_table().lua_context("creating canvas")?;
        let state = Rc::new(RefCell::new(Inputs {
            kinds: inputs.iter().map(|f| (f.name.clone(), f.kind)).collect(),
            written: BTreeSet::new(),
            changed: BTreeMap::new(),
        }));
        let callbacks = Rc::new(RefCell::new(Callbacks::default()));
        for name in CALLBACKS {
            let registered = Rc::clone(&callbacks);
            let register = lua
                .create_function(move |_, callback: Function| {
                    let mut callbacks = registered.borrow_mut();
                    let slot = if name == "on_data" { &mut callbacks.on_data } else { &mut callbacks.on_frame };
                    if slot.is_some() {
                        return Err(mlua::Error::runtime(format!("canvas.{name} was already called; register one function")));
                    }
                    *slot = Some(callback);
                    Ok(())
                })
                .lua_context("creating canvas")?;
            canvas.raw_set(name, register).lua_context("creating canvas")?;
        }
        let globals = lua.globals();
        globals.set("canvas", &canvas).lua_context("injecting canvas")?;
        globals.set("inputs", inputs_table(lua, Rc::clone(&state))?).lua_context("injecting inputs")?;
        vm.load::<()>(source, "keyboard.luau")?;
        let frame = lua.create_table().lua_context("creating frame")?;
        Ok(Self { vm, frame, callbacks, inputs: state })
    }

    /// Hands the script what the host half sent: MessagePack-encoded plain data.
    pub fn deliver(&self, payload: &[u8]) -> Result<()> {
        let on_data = self.callbacks.borrow().on_data.clone();
        let Some(on_data) = on_data else { return Ok(()) };
        let data = decode(self.vm.lua(), payload)?;
        self.vm.call::<()>(&on_data, data).context("in canvas.on_data")
    }

    pub fn frame(&self, time: f32, delta: f32, index: u32) -> Result<()> {
        let on_frame = self.callbacks.borrow().on_frame.clone();
        let Some(on_frame) = on_frame else { return Ok(()) };
        for (key, value) in [("time", f64::from(time)), ("delta", f64::from(delta)), ("index", f64::from(index))] {
            self.frame.raw_set(key, value).lua_context("filling frame")?;
        }
        self.vm.call::<()>(&on_frame, &self.frame).context("in canvas.on_frame")
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
        .create_function(move |lua, (_, name, value): (Table, String, Value)| {
            let mut state = state.borrow_mut();
            let kind = *state.kinds.get(&name).ok_or_else(|| mlua::Error::runtime(format!("the shader has no input {name}")))?;
            let converted = convert(&name, kind, &value).map_err(|e| mlua::Error::runtime(format!("{e:#}")))?;
            let kept = match &converted {
                InputValue::Array(values) => Value::Table(frozen_copy(lua, values)?),
                _ => value,
            };
            state.written.insert(name.clone());
            state.changed.insert(name.clone(), converted);
            written.raw_set(name, kept)
        })
        .lua_context("creating inputs")?;
    metatable.raw_set("__newindex", write).lua_context("creating inputs")?;
    metatable.raw_set("__index", store).lua_context("creating inputs")?;
    inputs.set_metatable(Some(metatable)).lua_context("creating inputs")?;
    Ok(inputs)
}

/// What reading an array input gives back: a frozen copy, so a write into it errors instead of
/// silently never reaching the shader.
fn frozen_copy(lua: &Lua, values: &[f32]) -> mlua::Result<Table> {
    let table = lua.create_sequence_from(values.iter().copied())?;
    table.set_readonly(true);
    Ok(table)
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

/// `color` and `drift`, installed before the VM is sealed. Vectors come from Luau's own `vector.create`.
fn install_library(lua: &Lua) -> Result<()> {
    let globals = lua.globals();
    globals.set("color", color_api(lua)?).lua_context("installing color")?;
    let drift_of = lua.create_function(|_, (t, centre, amount, speed, phase): (f32, Vector, Vector, Vector, Option<Vector>)| {
        Ok(drift(t, centre, amount, speed, phase.unwrap_or_default()))
    });
    globals.set("drift", drift_of.lua_context("drift")?).lua_context("installing drift")?;
    Ok(())
}

/// Wanders around `centre` by up to `amount`: x follows `sin(t * speed.x + phase.x)`, y `cos(t * speed.y + phase.y)`.
fn drift(t: f32, centre: Vector, amount: Vector, speed: Vector, phase: Vector) -> Vector {
    let x = centre.x() + amount.x() * (t * speed.x() + phase.x()).sin();
    let y = centre.y() + amount.y() * (t * speed.y() + phase.y()).cos();
    Vector::new(x, y, 0.0, 0.0)
}

#[cfg(test)]
mod tests {
    use glassdeck_luau::encode;

    use super::*;

    fn fields() -> Vec<InputField> {
        vec![
            InputField { name: "night".into(), kind: InputKind::Float { components: 3 } },
            InputField { name: "roseAt".into(), kind: InputKind::Float { components: 2 } },
            InputField { name: "bands".into(), kind: InputKind::FloatArray { len: 2 } },
        ]
    }

    const CANVAS: &str = r##"
        inputs.night = color.hex("#ff8000")
        local speed = 0.5
        canvas.on_data(function(data) speed = data.speed end)
        canvas.on_frame(function(frame)
            inputs.roseAt = drift(frame.time * speed, vector.create(0.5, 0.3), vector.create(0.1, 0.1), vector.create(1, 1))
        end)
    "##;

    fn payload(source: &str) -> Vec<u8> {
        let vm = Vm::new().unwrap();
        let value: Value = vm.load(source, "data").unwrap();
        encode(value).unwrap()
    }

    #[test]
    fn writes_at_load_and_per_frame_are_recorded_and_data_changes_state() {
        let script = CanvasScript::load(CANVAS, &fields()).unwrap();
        let changes = script.take_changes();
        let InputValue::Float(night) = &changes["night"] else { panic!("{changes:?}") };
        assert!((night[0] - 1.0).abs() < 1e-6 && (night[1] - 128.0 / 255.0).abs() < 1e-6 && night[2] == 0.0);

        script.frame(0.0, 0.0, 0).unwrap();
        assert_eq!(script.take_changes()["roseAt"], InputValue::Float(vec![0.5, 0.4]));
        script.deliver(&payload("return { speed = 2 }")).unwrap();
        script.frame(std::f32::consts::FRAC_PI_4, 0.0, 1).unwrap();
        let InputValue::Float(at) = &script.take_changes()["roseAt"] else { panic!() };
        assert!((at[0] - 0.6).abs() < 1e-5, "speed 2 moved it a quarter turn: {at:?}");
        assert_eq!(script.unwritten(), ["bands"]);
    }

    #[test]
    fn wrong_names_and_types_are_errors_naming_the_input() {
        let script = CanvasScript::load("canvas.on_frame(function() inputs.nite = vector.create(1, 2, 3) end)", &fields()).unwrap();
        assert!(format!("{:#}", script.frame(0.0, 0.0, 0).unwrap_err()).contains("the shader has no input nite"));
        let script = CanvasScript::load("canvas.on_frame(function() inputs.night = 3 end)", &fields()).unwrap();
        assert!(format!("{:#}", script.frame(0.0, 0.0, 0).unwrap_err()).contains("input night takes a vector"));
        let script = CanvasScript::load("canvas.on_frame(function() inputs.bands = { 1 } end)", &fields()).unwrap();
        assert!(format!("{:#}", script.frame(0.0, 0.0, 0).unwrap_err()).contains("takes 2 numbers, not 1"));
    }

    #[test]
    fn an_array_input_reads_back_frozen_so_writes_into_it_error() {
        let script = CanvasScript::load("inputs.bands = { 1, 2 } inputs.bands[1] = 5", &fields());
        assert!(format!("{:#}", script.err().unwrap()).contains("readonly"));
        let script = CanvasScript::load("local b = { 1, 2 } inputs.bands = b b[1] = 5 inputs.bands = inputs.bands", &fields()).unwrap();
        assert_eq!(script.take_changes()["bands"], InputValue::Array(vec![1.0, 2.0]));
    }

    #[test]
    fn a_script_without_callbacks_does_nothing_later() {
        let script = CanvasScript::load("inputs.night = color.hex('#000')", &fields()).unwrap();
        script.take_changes();
        script.frame(1.0, 0.1, 1).unwrap();
        script.deliver(&payload("return { a = 1 }")).unwrap();
        assert!(script.take_changes().is_empty());
    }

    #[test]
    fn callbacks_are_registered_once_with_a_function() {
        assert!(CanvasScript::load("canvas.on_frame(3)", &fields()).is_err());
        let twice = CanvasScript::load("canvas.on_data(function() end) canvas.on_data(function() end)", &fields());
        assert!(format!("{:#}", twice.err().unwrap()).contains("canvas.on_data was already called"));
    }

    #[test]
    fn colours_come_from_every_form() {
        let script = CanvasScript::load("inputs.night = color.rgb(255, 0, 0)", &fields()).unwrap();
        assert_eq!(script.take_changes()["night"], InputValue::Float(vec![1.0, 0.0, 0.0]));
    }
}
