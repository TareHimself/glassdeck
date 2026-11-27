//! A package's host half: its options (from `options.luau`) and its `host.luau`, which turns them into
//! data for the keyboard half. A package with options but no `host.luau` gets an implicit one that
//! forwards them as they are. glassdeckd runs these for the active setup and sends what they make over
//! the Link; the desktop app runs one in the widget editor's preview.

mod api;
mod media;

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use glassdeck_luau::color::color_api;
use glassdeck_luau::options::{OptionsScript, Panel, Values, any_function, changed_table, options_table};
use glassdeck_luau::messages::{ScriptMessages, Sent, read_messages};
use glassdeck_luau::{LuaResultExt, Print, ResourceReader, Vm, encode};
use glassdeck_messages::Half;
use glassdeck_package::ResourceKind;
use mlua::{Function, Lua, Table, Value};
use sysinfo::{CpuRefreshKind, MemoryRefreshKind, RefreshKind, System};

use api::{LiveOutbox, image_api, live_api, media_api};
pub use media::{NowPlaying, SharedNowPlaying, Track, start as start_media};

/// The host's system stats, shared by every host side on one thread.
pub type SharedSystem = Rc<RefCell<System>>;

pub fn system() -> SharedSystem {
    let refresh = RefreshKind::nothing().with_cpu(CpuRefreshKind::nothing().with_usage()).with_memory(MemoryRefreshKind::nothing().with_ram());
    Rc::new(RefCell::new(System::new_with_specifics(refresh).expect("system stats are readable")))
}

/// Where a `send()` waits to be taken; a newer one replaces it.
type Slot = Rc<RefCell<Option<Vec<u8>>>>;

/// A running `host.luau`.
struct HostVm {
    messages: ScriptMessages,
    vm: Vm,
    slot: Slot,
    live: LiveOutbox,
    on_options: Rc<RefCell<Option<Function>>>,
}

/// A package's host side: what it declares the user can set, and who turns that into data.
pub struct HostSide {
    pub label: String,
    options: Option<Declared>,
    run: Run,
}

struct Declared {
    script: OptionsScript,
    first: Panel,
}

enum Run {
    Script(HostVm),
    /// No `host.luau`: the options go as they are, encoded in this VM.
    Forward {
        vm: Vm,
        slot: Slot,
    },
}

/// What a host side is made from.
pub struct Sources {
    pub label: String,
    pub options: Option<String>,
    pub host: Option<String>,
    /// Serves `host.resource` and `keyboard.resource`
    pub reader: Option<ResourceReader>,
    /// The most one `send()` may carry
    pub send_limit: Option<usize>,
    /// Where `print` goes, instead of the process's own output
    pub print: Option<Print>,
    /// `messages.luau`: what `host.luau` and the keyboard half send each other
    pub messages: Option<String>,
}

static NO_OPTIONS: Panel = Panel { items: Vec::new() };

impl HostSide {
    /// `None` when there's nothing to run: no options and no `host.luau`.
    pub fn start(sources: Sources, system: &SharedSystem, now_playing: &SharedNowPlaying) -> Result<Option<Self>> {
        let Sources { label, options, host, reader, send_limit, print, messages } = sources;
        if options.is_none() && host.is_none() {
            return Ok(None);
        }
        let options = match options {
            Some(source) => {
                let script = OptionsScript::new(&source);
                let first = script.first_pass().context("options.luau")?;
                Some(Declared { script, first })
            }
            None => None,
        };
        let run = match host {
            Some(script) => Run::Script(HostVm::start(&script, messages.as_deref(), reader, send_limit, print, system, now_playing)?),
            None => Run::Forward { vm: Vm::new()?, slot: Rc::default() },
        };
        Ok(Some(Self { label, options, run }))
    }

    pub fn first(&self) -> &Panel {
        self.options.as_ref().map_or(&NO_OPTIONS, |o| &o.first)
    }

    /// `script.on_options(options, changed)`, or the options sent as they are.
    pub fn apply_options(&self, chosen: &Values, changed: Option<&BTreeSet<String>>) -> Result<()> {
        let first = self.first();
        let resolved = first.resolve(chosen);
        for why in &resolved.dropped {
            eprintln!("Options: {}: {why}; the default applies", self.label);
        }
        if let Some(options) = &self.options {
            options.script.run(first, &resolved.values)?;
        }
        match &self.run {
            Run::Script(host) => {
                let Some(on_options) = host.on_options.borrow().clone() else { return Ok(()) };
                let lua = host.vm.lua();
                let args = (options_table(lua, first, &resolved.values)?, changed_table(lua, first, changed)?);
                host.vm.call::<()>(&on_options, args).context("in script.on_options")
            }
            Run::Forward { vm, slot } => {
                if self.options.is_none() {
                    return Ok(());
                }
                *slot.borrow_mut() = Some(encode(Value::Table(options_table(vm.lua(), first, &resolved.values)?))?);
                Ok(())
            }
        }
    }

    /// What the side sent since the last call, the latest only.
    pub fn take_sent(&self) -> Option<Vec<u8>> {
        let slot = match &self.run {
            Run::Script(host) => &host.slot,
            Run::Forward { slot, .. } => slot,
        };
        slot.borrow_mut().take()
    }

    /// What `host.luau` sent the keyboard half since the last call.
    pub fn take_messages(&self) -> Vec<Sent> {
        match &self.run {
            Run::Script(host) => host.messages.take_sent(),
            Run::Forward { .. } => Vec::new(),
        }
    }

    /// A message from the keyboard half, to `host.luau`'s `script.on` callback.
    pub fn deliver_message(&self, message: u16, payload: &[u8]) -> Result<()> {
        match &self.run {
            Run::Script(host) => host.messages.deliver(&host.vm, message, payload),
            Run::Forward { .. } => Ok(()),
        }
    }

    pub fn take_live(&self) -> BTreeMap<String, Vec<u8>> {
        match &self.run {
            Run::Script(host) => std::mem::take(&mut *host.live.borrow_mut()),
            Run::Forward { .. } => BTreeMap::new(),
        }
    }

    /// Runs due timers; an error means the script failed and should stop.
    pub fn run_timers(&self, now: Instant) -> Result<()> {
        let Run::Script(host) = &self.run else { return Ok(()) };
        host.vm.run_due_timers(now)
    }

    pub fn next_timer(&self) -> Option<Instant> {
        match &self.run {
            Run::Script(host) => host.vm.next_timer(),
            Run::Forward { .. } => None,
        }
    }
}

fn check_size(len: usize, limit: Option<usize>) -> Result<()> {
    if let Some(limit) = limit
        && len > limit
    {
        bail!("{len} bytes is more than the {limit} one send may be");
    }
    Ok(())
}

impl HostVm {
    /// `reader` serves `host.resource` and `keyboard.resource`; `send_limit` caps one `send()`.
    fn start(
        script: &str,
        messages: Option<&str>,
        reader: Option<ResourceReader>,
        send_limit: Option<usize>,
        print: Option<Print>,
        system: &SharedSystem,
        now_playing: &SharedNowPlaying,
    ) -> Result<Self> {
        let vm = Vm::new()?;
        if let Some(print) = print {
            vm.install_print(print)?;
        }
        let slot: Slot = Rc::new(RefCell::new(None));
        let lua = vm.lua();
        let globals = lua.globals();
        globals.set("every", vm.every_function()?).lua_context("installing every")?;
        let outgoing = Rc::clone(&slot);
        let send = lua
            .create_function(move |_, data: Value| {
                let bytes = encode(data).map_err(|e| mlua::Error::runtime(format!("send: {e:#}")))?;
                check_size(bytes.len(), send_limit).map_err(|e| mlua::Error::runtime(format!("send: {e:#}")))?;
                *outgoing.borrow_mut() = Some(bytes);
                Ok(())
            })
            .lua_context("creating send")?;
        globals.set("send", send).lua_context("installing send")?;
        globals.set("system", system_api(lua, system)?).lua_context("installing system")?;
        let live: LiveOutbox = Rc::default();
        globals.set("live", live_api(lua, &live)?).lua_context("installing live")?;
        globals.set("image", image_api(lua)?).lua_context("installing image")?;
        globals.set("media", media_api(lua, now_playing)?).lua_context("installing media")?;
        globals.set("color", color_api(lua)?).lua_context("installing color")?;
        globals.set("any", any_function(lua)?).lua_context("installing any")?;
        if let Some(reader) = reader {
            vm.install_resources(&ResourceKind::ALL, reader)?;
        }
        let on_options = Rc::new(RefCell::new(None));
        install_on_options(lua, &on_options)?;
        let messages = ScriptMessages::install(lua, read_messages(messages)?, Half::Host)?;
        vm.seal()?;
        vm.load::<()>(script, "host.luau")?;
        Ok(Self { messages, vm, slot, live, on_options })
    }
}

/// `script.on_options(fn)`: what this script does when the options are set.
fn install_on_options(lua: &Lua, slot: &Rc<RefCell<Option<Function>>>) -> Result<()> {
    let globals = lua.globals();
    let script: Table = match globals.get::<Value>("script").lua_context("reading script")? {
        Value::Table(table) => table,
        _ => {
            let table = lua.create_table().lua_context("creating script")?;
            globals.set("script", &table).lua_context("installing script")?;
            table
        }
    };
    let registered = Rc::clone(slot);
    let register = lua
        .create_function(move |_, callback: Function| {
            let mut slot = registered.borrow_mut();
            if slot.is_some() {
                return Err(mlua::Error::runtime("script.on_options was already called; register one function"));
            }
            *slot = Some(callback);
            Ok(())
        })
        .lua_context("creating script.on_options")?;
    script.set("on_options", register).lua_context("installing script.on_options")
}

fn system_api(lua: &Lua, system: &SharedSystem) -> Result<Table> {
    let api = lua.create_table().lua_context("creating system")?;
    let cpu = Rc::clone(system);
    let cpu_percent = lua.create_function(move |_, ()| {
        let mut system = cpu.borrow_mut();
        system.refresh_cpu_usage();
        Ok(system.global_cpu_usage() as f64)
    });
    api.set("cpu_percent", cpu_percent.lua_context("creating cpu_percent")?).lua_context("installing cpu_percent")?;
    let memory = Rc::clone(system);
    let memory_percent = lua.create_function(move |_, ()| {
        let mut system = memory.borrow_mut();
        system.refresh_memory_specifics(MemoryRefreshKind::nothing().with_ram());
        Ok(system.used_memory() as f64 * 100.0 / system.total_memory().max(1) as f64)
    });
    api.set("memory_percent", memory_percent.lua_context("creating memory_percent")?).lua_context("installing memory_percent")?;
    Ok(api)
}

