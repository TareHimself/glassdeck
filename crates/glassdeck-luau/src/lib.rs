//! The Luau core both module hosts share: VM setup, the hang guard, timers, and the plain-data
//! encoding of `send()`, so a table sent on the host arrives identical on the keyboard.

pub mod profile;
mod resource;

pub use resource::{ResourceReader, resource_of};

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, ensure};
use mlua::{FromLuaMulti, Function, IntoLuaMulti, Lua, LuaSerdeExt, RegistryKey, Value, VmState};

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
        self.guarded(|| self.lua.load(source).set_name(chunk_name).call(()))
            .with_context(|| format!("running {chunk_name}"))
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

/// Plain data only: tables, numbers, strings and booleans. Functions and userdata are refused.
pub fn encode(lua: &Lua, value: Value) -> Result<Vec<u8>> {
    let data: rmpv::Value = lua.from_value(value).map_err(|e| anyhow!("only plain data can be sent: {e}"))?;
    let mut bytes = Vec::new();
    rmpv::encode::write_value(&mut bytes, &data)?;
    Ok(bytes)
}

pub fn decode(lua: &Lua, mut bytes: &[u8]) -> Result<Value> {
    let data = rmpv::decode::read_value(&mut bytes).context("decoding module data")?;
    ensure!(bytes.is_empty(), "module data has {} trailing bytes", bytes.len());
    lua.to_value(&data).map_err(|e| anyhow!("{e}"))
}

pub fn from_json(lua: &Lua, json: &serde_json::Value) -> Result<Value> {
    lua.to_value(json).map_err(|e| anyhow!("{e}"))
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
        let bytes = encode(vm.lua(), sent).unwrap();
        let other = sealed();
        let back = decode(other.lua(), &bytes).unwrap();
        other.lua().globals().set("data", back).unwrap();
        let check: bool = other
            .load("return data.cpu == 42.5 and data.name == 'gpu' and #data.bands == 3 and data.bands[3] == 3 and data.on", "t")
            .unwrap();
        assert!(check);
    }

    #[test]
    fn functions_cannot_be_sent() {
        let vm = sealed();
        let sent: Value = vm.load("return { f = function() end }", "t").unwrap();
        assert!(encode(vm.lua(), sent).is_err());
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
