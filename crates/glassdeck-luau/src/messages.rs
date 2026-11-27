//! A script's messages, from its package's `bridge.luau`: `bridge.on.<message>(fn)` hears one that
//! comes to this half, and `<other half>.bridge.<message>(value)` sends one there. A value is checked
//! against its shape when it's sent, so a mistake is reported where it's made, then packed.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::rc::Rc;

use anyhow::{Context, Result};
use glassdeck_messages::{Half, Kind, Messages, Shape, Value as Carried, decode, encode};
use mlua::{Function, Lua, Table, Value, Vector};

use crate::{LuaResultExt, Vm};

/// A message on its way to the other half: its number in `bridge.luau`, how it travels, and its packed values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sent {
    pub message: u16,
    pub kind: Kind,
    pub payload: Vec<u8>,
}

/// What's waiting to go: events in order, and for each state only its latest value.
#[derive(Default)]
struct Outbox {
    events: VecDeque<Sent>,
    states: BTreeMap<u16, Vec<u8>>,
}

pub struct ScriptMessages {
    messages: Rc<Messages>,
    heard: Rc<RefCell<HashMap<u16, Function>>>,
    /// `bridge.on.options`, on the keyboard half
    options: Rc<RefCell<Option<Function>>>,
    outbox: Rc<RefCell<Outbox>>,
}

/// A package's messages from its `bridge.luau`, or none without one; every problem in it, each at its line.
pub fn read_messages(source: Option<&str>) -> Result<Rc<Messages>> {
    let Some(source) = source.filter(|s| !glassdeck_package::is_blank_script(s)) else { return Ok(Rc::new(Messages::default())) };
    glassdeck_messages::parse(source)
        .map(Rc::new)
        .map_err(|problems| anyhow::anyhow!("{}", problems.iter().map(ToString::to_string).collect::<Vec<_>>().join("
")))
}

/// The global table named `name`, made if the API hasn't made it yet.
fn global_table(lua: &Lua, name: &str) -> Result<Table> {
    let globals = lua.globals();
    match globals.get::<Value>(name).lua_context(&format!("reading {name}"))? {
        Value::Table(table) => Ok(table),
        _ => {
            let table = lua.create_table().lua_context(&format!("creating {name}"))?;
            globals.set(name, &table).lua_context(&format!("installing {name}"))?;
            Ok(table)
        }
    }
}

/// A table whose missing keys are errors naming what's there, so a misspelt message fails loudly.
fn strict(lua: &Lua, table: &Table, what: &'static str, names: Vec<String>) -> Result<()> {
    let meta = lua.create_table().lua_context("creating a message table")?;
    let index = lua
        .create_function(move |_, (_, key): (Value, String)| -> mlua::Result<Value> {
            let known = if names.is_empty() { "none".to_owned() } else { names.join(", ") };
            Err(mlua::Error::runtime(format!("{key} isn't a message {what} (bridge.luau has: {known})")))
        })
        .lua_context("creating a message table")?;
    meta.set("__index", index).lua_context("creating a message table")?;
    table.set_metatable(Some(meta)).lua_context("creating a message table")
}

impl ScriptMessages {
    /// Installs `bridge.on` and the other half's `bridge` for `half`. Call after the API has made its
    /// own `script` table, and before `seal`.
    pub fn install(lua: &Lua, messages: Rc<Messages>, half: Half) -> Result<Self> {
        let heard = Rc::new(RefCell::new(HashMap::new()));
        let outbox = Rc::new(RefCell::new(Outbox::default()));
        let (here, there): (Vec<_>, Vec<_>) = messages.0.iter().enumerate().partition(|(_, m)| m.kind.to() == half);

        let on = lua.create_table().lua_context("creating bridge.on")?;
        let options = Rc::new(RefCell::new(None));
        let mut heard_names: Vec<String> = here.iter().map(|(_, m)| m.name.clone()).collect();
        if half == Half::Keyboard {
            let registered = Rc::clone(&options);
            let register = lua
                .create_function(move |_, callback: Function| {
                    if registered.borrow_mut().replace(callback).is_some() {
                        return Err(mlua::Error::runtime("bridge.on.options was already called; register one function"));
                    }
                    Ok(())
                })
                .lua_context("creating bridge.on.options")?;
            on.raw_set(glassdeck_messages::OPTIONS, register).lua_context("creating bridge.on")?;
            heard_names.push(glassdeck_messages::OPTIONS.to_owned());
        }
        for (number, message) in &here {
            let (number, name, registered) = (*number as u16, message.name.clone(), Rc::clone(&heard));
            let register = lua
                .create_function(move |_, callback: Function| {
                    if registered.borrow_mut().insert(number, callback).is_some() {
                        return Err(mlua::Error::runtime(format!("bridge.on.{name} was already called; register one function")));
                    }
                    Ok(())
                })
                .lua_context("creating bridge.on")?;
            on.raw_set(message.name.as_str(), register).lua_context("creating bridge.on")?;
        }
        strict(lua, &on, "this half hears", heard_names)?;
        global_table(lua, "bridge")?.raw_set("on", on).lua_context("installing bridge.on")?;

        let (other, other_name) = match half {
            Half::Keyboard => ("host", "host.bridge"),
            Half::Host => ("keyboard", "keyboard.bridge"),
        };
        let send = lua.create_table().lua_context("creating the other half's script")?;
        for (number, message) in &there {
            let (number, message, outbox) = (*number as u16, (*message).clone(), Rc::clone(&outbox));
            let name = message.name.clone();
            let sender = lua
                .create_function(move |_, value: Value| {
                    let carried = carried(&message.shape, &value, &message.name).map_err(mlua::Error::runtime)?;
                    let payload = encode(&message.shape, &carried);
                    let mut outbox = outbox.borrow_mut();
                    match message.kind {
                        Kind::KeyboardState => {
                            outbox.states.insert(number, payload);
                        }
                        Kind::KeyboardEvent | Kind::HostEvent => outbox.events.push_back(Sent { message: number, kind: message.kind, payload }),
                    }
                    Ok(())
                })
                .lua_context(&format!("creating {other_name}.{name}"))?;
            send.raw_set(name.as_str(), sender).lua_context("creating the other half's script")?;
        }
        strict(lua, &send, "this half sends", there.iter().map(|(_, m)| m.name.clone()).collect())?;
        global_table(lua, other)?.raw_set("bridge", send).lua_context(&format!("installing {other_name}"))?;
        Ok(Self { messages, heard, options, outbox })
    }

    /// Everything sent since the last call: events in the order they were sent, then each state's latest value.
    pub fn take_sent(&self) -> Vec<Sent> {
        let mut outbox = self.outbox.borrow_mut();
        let mut sent: Vec<Sent> = outbox.events.drain(..).collect();
        sent.extend(std::mem::take(&mut outbox.states).into_iter().map(|(message, payload)| Sent { message, kind: Kind::KeyboardState, payload }));
        sent
    }

    /// The options from the implicit host (MessagePack, as `send` packs them), to `bridge.on.options`;
    /// whether the script registered one to take them.
    pub fn deliver_options(&self, vm: &Vm, payload: &[u8]) -> Result<bool> {
        let Some(callback) = self.options.borrow().clone() else { return Ok(false) };
        let options = crate::decode(vm.lua(), payload)?;
        vm.call::<()>(&callback, options).context("in bridge.on.options")?;
        Ok(true)
    }

    /// A message from the other half, to its `bridge.on` callback; nothing if the script didn't register one.
    pub fn deliver(&self, vm: &Vm, message: u16, payload: &[u8]) -> Result<()> {
        let known = self.messages.0.get(usize::from(message)).with_context(|| format!("message {message} isn't in bridge.luau"))?;
        let value = decode(&known.shape, payload).map_err(|e| anyhow::anyhow!("{} doesn't read: {}", known.name, e.0))?;
        let Some(callback) = self.heard.borrow().get(&message).cloned() else { return Ok(()) };
        let value = to_lua(vm.lua(), &known.shape, value).lua_context("unpacking a message")?;
        vm.call::<()>(&callback, value).with_context(|| format!("in bridge.on.{}", known.name))
    }
}

/// `value` as `shape` describes, or what's wrong with it, at its place (`nowPlaying.title`).
fn carried(shape: &Shape, value: &Value, at: &str) -> Result<Carried, String> {
    let wrong = |expected: &str| format!("{at}: expected {expected}, got {}", value.type_name());
    match shape {
        Shape::Optional(inner) => match value {
            Value::Nil => Ok(Carried::Absent),
            present => carried(inner, present, at),
        },
        Shape::Number => match value {
            Value::Number(n) => Ok(Carried::Number(*n)),
            Value::Integer(n) => Ok(Carried::Number(*n as f64)),
            _ => Err(wrong("a number")),
        },
        Shape::Boolean => match value {
            Value::Boolean(b) => Ok(Carried::Boolean(*b)),
            _ => Err(wrong("a boolean")),
        },
        Shape::String => match value {
            Value::String(s) => Ok(Carried::String(s.to_str().map_err(|_| format!("{at}: isn't UTF-8 text"))?.to_owned())),
            _ => Err(wrong("a string")),
        },
        Shape::Color | Shape::Vector => match value {
            Value::Vector(v) => Ok(Carried::Vector([v.x(), v.y(), v.z(), v.w()])),
            _ => Err(wrong(if matches!(shape, Shape::Color) { "a Color" } else { "a vector" })),
        },
        Shape::List(item) => {
            let Value::Table(table) = value else { return Err(wrong("a list")) };
            let items = table.sequence_values::<Value>().collect::<mlua::Result<Vec<_>>>().map_err(|e| format!("{at}: {e}"))?;
            if items.len() != table.pairs::<Value, Value>().count() {
                return Err(format!("{at}: a list has only numbered items, 1 up"));
            }
            items.iter().enumerate().map(|(i, v)| carried(item, v, &format!("{at}[{}]", i + 1))).collect::<Result<_, _>>().map(Carried::List)
        }
        Shape::Record(fields) => {
            let Value::Table(table) = value else { return Err(wrong("a table")) };
            for pair in table.pairs::<Value, Value>() {
                let (key, _) = pair.map_err(|e| format!("{at}: {e}"))?;
                let known = matches!(&key, Value::String(k) if fields.iter().any(|(name, _)| k.to_str().is_ok_and(|k| *k == **name)));
                if !known {
                    return Err(format!("{at}: has no field {}", key.to_string().unwrap_or_else(|_| key.type_name().to_owned())));
                }
            }
            fields
                .iter()
                .map(|(name, field)| {
                    let v = table.raw_get::<Value>(name.as_str()).map_err(|e| format!("{at}.{name}: {e}"))?;
                    if v == Value::Nil && !matches!(field, Shape::Optional(_)) {
                        return Err(format!("{at}.{name}: missing"));
                    }
                    carried(field, &v, &format!("{at}.{name}"))
                })
                .collect::<Result<_, _>>()
                .map(Carried::Record)
        }
    }
}

fn to_lua(lua: &Lua, shape: &Shape, value: Carried) -> mlua::Result<Value> {
    Ok(match (shape, value) {
        (_, Carried::Absent) => Value::Nil,
        (Shape::Optional(inner), present) => to_lua(lua, inner, present)?,
        (_, Carried::Number(n)) => Value::Number(n),
        (_, Carried::Boolean(b)) => Value::Boolean(b),
        (_, Carried::String(s)) => Value::String(lua.create_string(&s)?),
        (_, Carried::Vector([x, y, z, w])) => Value::Vector(Vector::new(x, y, z, w)),
        (Shape::List(item), Carried::List(items)) => {
            let table = lua.create_table()?;
            for v in items {
                table.raw_push(to_lua(lua, item, v)?)?;
            }
            Value::Table(table)
        }
        (Shape::Record(fields), Carried::Record(values)) => {
            let table = lua.create_table()?;
            for ((name, field), v) in fields.iter().zip(values) {
                table.raw_set(name.as_str(), to_lua(lua, field, v)?)?;
            }
            Value::Table(table)
        }
        _ => return Err(mlua::Error::runtime("a message's value doesn't match its shape")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MESSAGES: &str = "export type palette = KeyboardState<{ colours: { Color }, speed: number }>\nexport type trackChanged = KeyboardEvent<{ title: string, length: number? }>\nexport type tapped = HostEvent<{ key: number }>\n";

    fn half(half: Half) -> (Vm, ScriptMessages) {
        let vm = Vm::new().unwrap();
        let messages = Rc::new(glassdeck_messages::parse(MESSAGES).unwrap());
        let api = ScriptMessages::install(vm.lua(), messages, half).unwrap();
        (vm, api)
    }

    #[test]
    fn a_message_sent_on_one_half_arrives_typed_on_the_other() {
        let (host, host_api) = half(Half::Host);
        let (keyboard, keyboard_api) = half(Half::Keyboard);
        keyboard
            .load::<()>("bridge.on.trackChanged(function(t) heard = t.title .. ':' .. tostring(t.length) end)", "keyboard.luau")
            .unwrap();
        host.load::<()>("keyboard.bridge.trackChanged({ title = 'Low Tide' })", "host.luau").unwrap();
        for sent in host_api.take_sent() {
            keyboard_api.deliver(&keyboard, sent.message, &sent.payload).unwrap();
        }
        assert_eq!(keyboard.lua().globals().get::<String>("heard").unwrap(), "Low Tide:nil");

        keyboard.load::<()>("host.bridge.tapped({ key = 3 })", "keyboard.luau").unwrap();
        let sent = keyboard_api.take_sent();
        assert_eq!(sent, vec![Sent { message: 2, kind: Kind::HostEvent, payload: vec![1, 6] }]);
    }

    #[test]
    fn only_a_states_latest_value_goes_but_every_event_does() {
        let (host, api) = half(Half::Host);
        host.load::<()>(
            "keyboard.bridge.palette({ colours = {}, speed = 1 })\nkeyboard.bridge.palette({ colours = {}, speed = 2 })\nkeyboard.bridge.trackChanged({ title = 'a' })\nkeyboard.bridge.trackChanged({ title = 'b' })",
            "host.luau",
        )
        .unwrap();
        let sent: Vec<u16> = api.take_sent().iter().map(|s| s.message).collect();
        assert_eq!(sent, [1, 1, 0], "both events in order, then the palette once");
    }

    #[test]
    fn a_wrong_value_or_name_is_an_error_where_its_made() {
        let (host, _api) = half(Half::Host);
        let error = |script: &str| format!("{:#}", host.load::<()>(script, "host.luau").unwrap_err());
        assert!(error("keyboard.bridge.palette({ colours = { 'red' }, speed = 1 })").contains("palette.colours[1]: expected a Color, got string"));
        assert!(error("keyboard.bridge.trackChanged({ title = 'x', lenght = 3 })").contains("has no field lenght"));
        assert!(error("keyboard.bridge.palete({})").contains("palete isn't a message this half sends"));
        assert!(error("keyboard.bridge.tapped({ key = 1 })").contains("tapped isn't a message this half sends"));
    }
}
