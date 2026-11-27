//! The Luau core both module hosts share: VM setup, the hang guard, timers, the `bridge` between a
//! package's halves, and the plain-data encoding the implicit host forwards options in.

pub mod color;
pub mod messages;
pub mod options;
pub mod placement;
mod resource;

pub use resource::{LiveName, MAX_LIVE_RESOURCE_BYTES, ResourceReader, ResourceRef, resource_of};

/// Where a script's `print` goes, when not to the process's own output.
pub type Print = std::rc::Rc<dyn Fn(String)>;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail, ensure};
use mlua::{FromLuaMulti, Function, IntoLuaMulti, Lua, RegistryKey, Value, VmState};

/// Any single call into a script must return within this, or it is stopped with an error.
pub const CALL_LIMIT: Duration = Duration::from_millis(100);

/// One module instance's VM. Hosts install their API, then `seal` it before loading the script.
pub struct Vm {
    lua: Lua,
    deadline: Rc<Cell<Option<Instant>>>,
    timers: Rc<RefCell<Vec<Timer>>>,
}

struct Timer {
    every: Duration,
    due: Instant,
    callback: RegistryKey,
}

impl Vm {
    pub fn new() -> Result<Self> {
        let lua = Lua::new();
        let deadline = Rc::new(Cell::new(None::<Instant>));
        let watched = Rc::clone(&deadline);
        lua.set_interrupt(move |_| match watched.get() {
            Some(deadline) if Instant::now() > deadline => {
                Err(mlua::Error::runtime(format!("took longer than {} ms", CALL_LIMIT.as_millis())))
            }
            _ => Ok(VmState::Continue),
        });
        Ok(Self { lua, deadline, timers: Rc::new(RefCell::new(Vec::new())) })
    }

    pub fn lua(&self) -> &Lua {
        &self.lua
    }

    /// Sends what `print` gets to `print_to`, as Luau would print it: each value through `tostring`,
    /// separated by tabs. Call before `seal`.
    pub fn install_print(&self, print_to: Print) -> Result<()> {
        let print = self
            .lua
            .create_function(move |lua, values: mlua::Variadic<Value>| {
                let tostring: Function = lua.globals().get("tostring")?;
                let parts = values.into_iter().map(|v| tostring.call::<String>(v)).collect::<mlua::Result<Vec<_>>>()?;
                print_to(parts.join("\t"));
                Ok(())
            })
            .lua_context("creating print")?;
        self.lua.globals().set("print", print).lua_context("installing print")
    }

    /// `every(seconds, fn)`: calls `fn` every `seconds`, for the host to expose under its own name.
    pub fn every_function(&self) -> Result<Function> {
        let timers = Rc::clone(&self.timers);
        let every = self.lua.create_function(move |lua, (seconds, callback): (f64, Function)| {
            if !(seconds.is_finite() && seconds > 0.0) {
                return Err(mlua::Error::runtime(format!("every: interval must be a positive number of seconds, got {seconds}")));
            }
            let every = Duration::from_secs_f64(seconds);
            let callback = lua.create_registry_value(callback)?;
            timers.borrow_mut().push(Timer { every, due: Instant::now() + every, callback });
            Ok(())
        });
        every.lua_context("creating every")
    }

    /// Makes the globals read-only, so a script can't break the API for itself.
    pub fn seal(&self) -> Result<()> {
        self.lua.sandbox(true).lua_context("sandboxing the VM")
    }

    pub fn load<R: FromLuaMulti>(&self, source: &str, chunk_name: &str) -> Result<R> {
        self.guarded(|| self.lua.load(source).set_name(chunk_name).call(())).with_context(|| format!("running {chunk_name}"))
    }

    pub fn call<R: FromLuaMulti>(&self, function: &Function, args: impl IntoLuaMulti) -> Result<R> {
        self.guarded(|| function.call(args))
    }

    /// Runs every timer that is due; one that fell far behind skips ahead instead of firing repeatedly.
    pub fn run_due_timers(&self, now: Instant) -> Result<()> {
        let due: Vec<Function> = {
            let mut timers = self.timers.borrow_mut();
            let mut due = Vec::new();
            for timer in timers.iter_mut().filter(|t| t.due <= now) {
                timer.due = (timer.due + timer.every).max(now);
                due.push(self.lua.registry_value(&timer.callback).lua_context("finding a timer")?);
            }
            due
        };
        for callback in due {
            self.call::<()>(&callback, ())?;
        }
        Ok(())
    }

    pub fn next_timer(&self) -> Option<Instant> {
        self.timers.borrow().iter().map(|t| t.due).min()
    }

    pub fn used_memory(&self) -> usize {
        self.lua.used_memory()
    }

    fn guarded<R>(&self, run: impl FnOnce() -> mlua::Result<R>) -> Result<R> {
        self.deadline.set(Some(Instant::now() + CALL_LIMIT));
        let result = run();
        self.deadline.set(None);
        result.map_err(|e| anyhow!("{e}"))
    }
}

/// mlua's errors aren't `Send`, so they can't become `anyhow` errors directly.
pub trait LuaResultExt<T> {
    fn lua_context(self, what: &str) -> Result<T>;
}

impl<T> LuaResultExt<T> for mlua::Result<T> {
    fn lua_context(self, what: &str) -> Result<T> {
        self.map_err(|e| anyhow!("{what}: {e}"))
    }
}

/// MessagePack extension type for a Luau vector (colours, positions): its four f32s, little-endian.
const VECTOR_EXT: i8 = 1;
/// Deep enough for any real data, shallow enough that a table holding itself fails fast.
const MAX_DEPTH: usize = 32;

/// Plain data only: tables, numbers, strings, booleans and vectors. Functions and userdata are refused.
/// A table whose keys are exactly 1 to n goes as an array, any other as a map.
pub fn encode(value: Value) -> Result<Vec<u8>> {
    let data = to_msgpack(&value, 0).map_err(|e| anyhow!("only plain data can be sent: {e:#}"))?;
    let mut bytes = Vec::new();
    rmpv::encode::write_value(&mut bytes, &data)?;
    Ok(bytes)
}

pub fn decode(lua: &Lua, mut bytes: &[u8]) -> Result<Value> {
    let data = rmpv::decode::read_value(&mut bytes).context("decoding module data")?;
    ensure!(bytes.is_empty(), "module data has {} trailing bytes", bytes.len());
    from_msgpack(lua, &data)
}

fn to_msgpack(value: &Value, depth: usize) -> Result<rmpv::Value> {
    ensure!(depth < MAX_DEPTH, "tables nest deeper than {MAX_DEPTH}");
    Ok(match value {
        Value::Nil => rmpv::Value::Nil,
        Value::Boolean(b) => rmpv::Value::Boolean(*b),
        Value::Integer(n) => rmpv::Value::from(*n),
        Value::Number(n) => rmpv::Value::F64(*n),
        Value::Vector(v) => {
            let bytes = [v.x(), v.y(), v.z(), v.w()].iter().flat_map(|c| c.to_le_bytes()).collect();
            rmpv::Value::Ext(VECTOR_EXT, bytes)
        }
        Value::String(s) => match s.to_str() {
            Ok(text) => rmpv::Value::String(text.to_owned().into()),
            Err(_) => rmpv::Value::Binary(s.as_bytes().to_vec()),
        },
        Value::Table(table) => {
            let pairs: Vec<(Value, Value)> = table.pairs::<Value, Value>().collect::<mlua::Result<_>>().lua_context("reading a table")?;
            let length = table.raw_len();
            if !pairs.is_empty()
                && pairs.len() == length
                && pairs.iter().all(|(k, _)| matches!(k, Value::Integer(i) if *i >= 1 && *i as usize <= length))
            {
                let mut items = vec![rmpv::Value::Nil; length];
                for (key, value) in &pairs {
                    let Value::Integer(i) = key else { unreachable!("checked above") };
                    items[*i as usize - 1] = to_msgpack(value, depth + 1)?;
                }
                rmpv::Value::Array(items)
            } else {
                rmpv::Value::Map(
                    pairs.iter().map(|(k, v)| Ok((to_msgpack(k, depth + 1)?, to_msgpack(v, depth + 1)?))).collect::<Result<_>>()?,
                )
            }
        }
        other => bail!("a {} can't be sent", other.type_name()),
    })
}

fn from_msgpack(lua: &Lua, data: &rmpv::Value) -> Result<Value> {
    Ok(match data {
        rmpv::Value::Nil => Value::Nil,
        rmpv::Value::Boolean(b) => Value::Boolean(*b),
        rmpv::Value::Integer(n) => match n.as_i64() {
            Some(i) => Value::Integer(i),
            None => Value::Number(n.as_f64().context("an integer too large to read")?),
        },
        rmpv::Value::F32(n) => Value::Number(f64::from(*n)),
        rmpv::Value::F64(n) => Value::Number(*n),
        rmpv::Value::String(s) => Value::String(lua.create_string(s.as_bytes()).lua_context("reading a string")?),
        rmpv::Value::Binary(b) => Value::String(lua.create_string(b).lua_context("reading a string")?),
        rmpv::Value::Ext(VECTOR_EXT, bytes) if bytes.len() == 16 => {
            let c = |i: usize| f32::from_le_bytes(bytes[i * 4..i * 4 + 4].try_into().expect("4 bytes"));
            Value::Vector(mlua::Vector::new(c(0), c(1), c(2), c(3)))
        }
        rmpv::Value::Ext(kind, bytes) => bail!("unknown extension type {kind} ({} bytes)", bytes.len()),
        rmpv::Value::Array(items) => {
            let table = lua.create_table_with_capacity(items.len(), 0).lua_context("reading an array")?;
            for (i, item) in items.iter().enumerate() {
                table.raw_set(i + 1, from_msgpack(lua, item)?).lua_context("reading an array")?;
            }
            Value::Table(table)
        }
        rmpv::Value::Map(pairs) => {
            let table = lua.create_table_with_capacity(0, pairs.len()).lua_context("reading a map")?;
            for (key, value) in pairs {
                table.raw_set(from_msgpack(lua, key)?, from_msgpack(lua, value)?).lua_context("reading a map")?;
            }
            Value::Table(table)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sealed() -> Vm {
        let vm = Vm::new().unwrap();
        vm.lua().globals().set("every", vm.every_function().unwrap()).unwrap();
        vm.seal().unwrap();
        vm
    }

    #[test]
    fn plain_data_survives_the_trip() {
        let vm = sealed();
        let sent: Value = vm.load("return { cpu = 42.5, name = 'gpu', bands = { 1, 2, 3 }, on = true }", "t").unwrap();
        let bytes = encode(sent).unwrap();
        let other = sealed();
        let back = decode(other.lua(), &bytes).unwrap();
        other.lua().globals().set("data", back).unwrap();
        let check: bool = other
            .load("return data.cpu == 42.5 and data.name == 'gpu' and #data.bands == 3 and data.bands[3] == 3 and data.on", "t")
            .unwrap();
        assert!(check);
    }

    #[test]
    fn vectors_arrive_as_vectors_and_arrays_as_arrays() {
        let vm = sealed();
        let sent: Value =
            vm.load("return { tint = vector.create(0.25, 0.5, 1, 0.75), bands = { 1, 2, 3 }, sparse = { [2] = 1 } }", "t").unwrap();
        let bytes = encode(sent).unwrap();
        let other = sealed();
        other.lua().globals().set("data", decode(other.lua(), &bytes).unwrap()).unwrap();
        let check: bool = other
            .load("return typeof(data.tint) == 'vector' and data.tint.x == 0.25 and data.tint.z == 1 and #data.bands == 3 and data.sparse[2] == 1 and data.sparse[1] == nil", "t")
            .unwrap();
        assert!(check);
    }

    #[test]
    fn functions_cannot_be_sent() {
        let vm = sealed();
        let sent: Value = vm.load("return { f = function() end }", "t").unwrap();
        assert!(encode(sent).is_err());
    }

    #[test]
    fn an_endless_loop_is_stopped() {
        let vm = sealed();
        let spin: Function = vm.load("return function() while true do end end", "t").unwrap();
        let started = Instant::now();
        let error = vm.call::<()>(&spin, ()).unwrap_err();
        assert!(error.to_string().contains("took longer than"), "{error}");
        assert!(started.elapsed() < CALL_LIMIT * 5);
        assert_eq!(vm.call::<f64>(&vm.load::<Function>("return function() return 1 end", "t").unwrap(), ()).unwrap(), 1.0);
    }

    #[test]
    fn timers_fire_when_due() {
        let vm = sealed();
        let count: Function = vm.load("local n = 0; every(0.5, function() n += 1 end); return function() return n end", "t").unwrap();
        let start = Instant::now();
        vm.run_due_timers(start).unwrap();
        assert_eq!(vm.call::<f64>(&count, ()).unwrap(), 0.0);
        vm.run_due_timers(start + Duration::from_secs(1)).unwrap();
        assert_eq!(vm.call::<f64>(&count, ()).unwrap(), 1.0);
        assert!(vm.next_timer().unwrap() >= start + Duration::from_secs(1));
    }

    #[test]
    fn globals_are_read_only() {
        let vm = sealed();
        assert!(vm.load::<()>("string.format = nil", "t").is_err());
    }
}
