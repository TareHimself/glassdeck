//! A package's host half: its options (from `options.luau`) and its `host.luau`, which talks to the
//! keyboard half over `bridge`. A package with options but no `host.luau` gets an implicit one that
//! forwards them as they are, to the keyboard's `bridge.on.options`. glassdeckd runs these for the active setup and sends what they make over
//! the Link; the desktop app runs one in the widget editor's preview.

mod api;
mod audio;
mod media;
mod net;

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use glassdeck_luau::color::color_api;
use glassdeck_luau::json::json_api;
use glassdeck_luau::waiting::Waits;
use glassdeck_luau::options::{OptionsScript, Panel, Values, any_function, changed_table, options_table};
use glassdeck_luau::messages::{ScriptMessages, Sent, read_messages};
use glassdeck_luau::{LuaResultExt, Print, ResourceReader, Vm, encode};
use glassdeck_messages::Half;
use glassdeck_package::{NetworkHost, ResourceKind};
use mlua::{Function, Lua, Table, Value};
use sysinfo::{CpuRefreshKind, MemoryRefreshKind, RefreshKind, System};

use api::{LiveOutbox, image_api, live_api, media_api};
pub use audio::{SharedAudio, audio_tap};
pub use media::{NowPlaying, SharedNowPlaying, Track, start as start_media};
pub use net::Wake;
use net::{Net, net_api};

/// The host's system stats, shared by every host side on one thread.
pub type SharedSystem = Rc<RefCell<System>>;

/// What every host side on one thread shares.
pub struct Services {
    pub system: SharedSystem,
    pub now_playing: SharedNowPlaying,
    pub audio: SharedAudio,
}

impl Services {
    pub fn new(now_playing: SharedNowPlaying) -> Self {
        Self { system: system(), now_playing, audio: audio_tap() }
    }
}

pub fn system() -> SharedSystem {
    let refresh = RefreshKind::nothing().with_cpu(CpuRefreshKind::nothing().with_usage()).with_memory(MemoryRefreshKind::nothing().with_ram());
    Rc::new(RefCell::new(System::new_with_specifics(refresh).expect("system stats are readable")))
}

/// The implicit host's latest options, waiting to be taken; newer ones replace them.
type Slot = Rc<RefCell<Option<Vec<u8>>>>;

/// A running `host.luau`.
struct HostVm {
    messages: ScriptMessages,
    net: Rc<Net>,
    waits: Waits,
    vm: Vm,
    live: LiveOutbox,
    on_options: Rc<RefCell<Option<Function>>>,
    /// What `keyboard.connected()` answers; every script starts as if the keyboard were there.
    keyboard: Rc<Cell<bool>>,
    on_keyboard: Rc<RefCell<Option<Function>>>,
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
    /// The most one message's packed values may be
    pub message_limit: Option<usize>,
    /// Where `print` goes, instead of the process's own output
    pub print: Option<Print>,
    /// `bridge.luau`: what `host.luau` and the keyboard half send each other
    pub messages: Option<String>,
    /// The hosts `net` may reach, from `deck.json`
    pub network: Vec<NetworkHost>,
    /// Called from another thread when a `net` answer is ready for `run_answers`
    pub wake: Option<Wake>,
}

static NO_OPTIONS: Panel = Panel { items: Vec::new() };

impl HostSide {
    /// `None` when there's nothing to run: no options and no `host.luau`.
    pub fn start(sources: Sources, services: &Services) -> Result<Option<Self>> {
        let Sources { label, options, host, reader, message_limit, print, messages, network, wake } = sources;
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
            Some(script) => Run::Script(HostVm::start(&script, messages.as_deref(), Net::new(network, wake, print.clone()), reader, message_limit, print, services)?),
            None => Run::Forward { vm: Vm::new()?, slot: Rc::default() },
        };
        Ok(Some(Self { label, options, run }))
    }

    /// The keyboard came or went: `keyboard.connected()` follows it, and `script.on_keyboard` hears
    /// each change. A host side without `host.luau` has nobody to tell.
    pub fn set_keyboard(&self, connected: bool) -> Result<()> {
        let Run::Script(host) = &self.run else { return Ok(()) };
        let Some(connected) = keyboard_change(host.keyboard.get(), connected) else { return Ok(()) };
        host.keyboard.set(connected);
        let Some(on_keyboard) = host.on_keyboard.borrow().clone() else { return Ok(()) };
        host.vm.run(&on_keyboard, connected).context("in script.on_keyboard")
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
                host.vm.run(&on_options, args).context("in script.on_options")
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

    /// The implicit host's options since the last call, the latest only; a `host.luau` sends its own messages.
    pub fn take_options(&self) -> Option<Vec<u8>> {
        match &self.run {
            Run::Script(_) => None,
            Run::Forward { slot, .. } => slot.borrow_mut().take(),
        }
    }

    /// What `host.luau` sent the keyboard half since the last call.
    pub fn take_messages(&self) -> Vec<Sent> {
        match &self.run {
            Run::Script(host) => host.messages.take_sent(),
            Run::Forward { .. } => Vec::new(),
        }
    }

    /// A message from the keyboard half, to `host.luau`'s `bridge.on` callback.
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

    /// Resumes the callbacks whose `net` answers have arrived; an error means the script failed and should stop.
    pub fn run_answers(&self) -> Result<()> {
        let Run::Script(host) = &self.run else { return Ok(()) };
        host.net.deliver(host.vm.lua(), &host.waits)?;
        host.vm.resume_answered()
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

impl HostVm {
    /// `reader` serves `host.resource` and `keyboard.resource`; `message_limit` caps one message.
    fn start(
        script: &str,
        messages: Option<&str>,
        net: Rc<Net>,
        reader: Option<ResourceReader>,
        message_limit: Option<usize>,
        print: Option<Print>,
        services: &Services,
    ) -> Result<Self> {
        let mut vm = Vm::new()?;
        if let Some(print) = print {
            vm.install_print(print)?;
        }
        let (waits, wait) = vm.allow_waiting()?;
        let lua = vm.lua();
        globals_set_net(lua, &net, &waits, wait)?;
        let globals = lua.globals();
        globals.set("every", vm.every_function()?).lua_context("installing every")?;
        globals.set("system", system_api(lua, &services.system)?).lua_context("installing system")?;
        let live: LiveOutbox = Rc::default();
        globals.set("live", live_api(lua, &live)?).lua_context("installing live")?;
        globals.set("image", image_api(lua)?).lua_context("installing image")?;
        globals.set("media", media_api(lua, &services.now_playing)?).lua_context("installing media")?;
        globals.set("audio", audio_api(lua, &services.audio)?).lua_context("installing audio")?;
        globals.set("color", color_api(lua)?).lua_context("installing color")?;
        globals.set("any", any_function(lua)?).lua_context("installing any")?;
        if let Some(reader) = reader {
            vm.install_resources(&ResourceKind::ALL, reader)?;
        }
        let on_options = Rc::new(RefCell::new(None));
        install_callback(lua, "on_options", &on_options)?;
        let on_keyboard = Rc::new(RefCell::new(None));
        install_callback(lua, "on_keyboard", &on_keyboard)?;
        let messages = ScriptMessages::install(lua, read_messages(messages)?, Half::Host, message_limit)?;
        let keyboard = Rc::new(Cell::new(true));
        install_connected(lua, &keyboard)?;
        vm.seal()?;
        vm.load::<()>(script, "host.luau")?;
        Ok(Self { messages, net, waits, vm, live, on_options, keyboard, on_keyboard })
    }
}

fn globals_set_net(lua: &Lua, net: &Rc<Net>, waits: &Waits, wait: Function) -> Result<()> {
    let globals = lua.globals();
    globals.set("net", net_api(lua, net, waits, wait)?).lua_context("installing net")?;
    globals.set("json", json_api(lua)?).lua_context("installing json")
}

/// `script.<name>(fn)`, which registers what this script does when that happens; once only.
fn install_callback(lua: &Lua, name: &'static str, slot: &Rc<RefCell<Option<Function>>>) -> Result<()> {
    let script = global_table(lua, "script")?;
    let registered = Rc::clone(slot);
    let register = lua
        .create_function(move |_, callback: Function| {
            let mut slot = registered.borrow_mut();
            if slot.is_some() {
                return Err(mlua::Error::runtime(format!("script.{name} was already called; register one function")));
            }
            *slot = Some(callback);
            Ok(())
        })
        .lua_context(&format!("creating script.{name}"))?;
    script.set(name, register).lua_context(&format!("installing script.{name}"))
}

/// `keyboard.connected()`: whether the keyboard is there now.
fn install_connected(lua: &Lua, keyboard: &Rc<Cell<bool>>) -> Result<()> {
    let state = Rc::clone(keyboard);
    let connected = lua.create_function(move |_, ()| Ok(state.get())).lua_context("creating keyboard.connected")?;
    global_table(lua, "keyboard")?.set("connected", connected).lua_context("installing keyboard.connected")
}

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

/// The news for a script that knew `known` when the keyboard is now `now`: only a change.
fn keyboard_change(known: bool, now: bool) -> Option<bool> {
    (known != now).then_some(now)
}

fn audio_api(lua: &Lua, audio: &SharedAudio) -> Result<Table> {
    let api = lua.create_table().lua_context("creating audio")?;
    let tap = Arc::clone(audio);
    let bands = lua
        .create_function(move |_, count: usize| tap.bands(count).map_err(|e| mlua::Error::runtime(format!("audio.bands: {e:#}"))))
        .lua_context("creating audio.bands")?;
    api.set("bands", bands).lua_context("installing audio.bands")?;
    Ok(api)
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


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_script_hears_only_when_the_keyboard_comes_or_goes() {
        assert_eq!(keyboard_change(true, true), None);
        assert_eq!(keyboard_change(true, false), Some(false));
        assert_eq!(keyboard_change(false, true), Some(true));
        assert_eq!(keyboard_change(false, false), None);
    }

    fn host(script: &str) -> HostSide {
        let sources = Sources {
            label: "test".into(),
            options: None,
            host: Some(script.into()),
            reader: None,
            message_limit: None,
            print: None,
            messages: None,
            network: Vec::new(),
            wake: None,
        };
        HostSide::start(sources, &Services::new(Arc::default())).unwrap().unwrap()
    }

    fn global<T: mlua::FromLua>(side: &HostSide, name: &str) -> T {
        let Run::Script(host) = &side.run else { panic!("a script runs") };
        host.vm.lua().globals().get(name).unwrap()
    }

    #[test]
    fn on_keyboard_follows_the_keyboard_and_connected_answers_now() {
        let side = host(
            "heard = {}\nscript.on_keyboard(function(connected) table.insert(heard, connected); seen = keyboard.connected() end)\nat_start = keyboard.connected()",
        );
        for connected in [true, false, false, true] {
            side.set_keyboard(connected).unwrap();
        }
        assert!(global::<bool>(&side, "at_start"));
        let heard: Vec<bool> = global::<Table>(&side, "heard").sequence_values().map(|v| v.unwrap()).collect();
        assert_eq!(heard, [false, true]);
        assert!(global::<bool>(&side, "seen"));
    }

    #[test]
    fn a_failing_on_keyboard_is_reported() {
        let side = host("script.on_keyboard(function() error('boom') end)");
        let error = side.set_keyboard(false).unwrap_err();
        assert!(format!("{error:#}").contains("script.on_keyboard"), "{error:#}");
    }
}
