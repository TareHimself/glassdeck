//! The module host: runs the host side of each widget in the profile and of the selected canvas
//! background, and hands what each sends to the Link. A host side is a package's options (from
//! `options.luau`) and its `host.luau`, which turns them into data for the keyboard; a package with
//! options but no `host.luau` gets an implicit one that forwards them as they are. Picks up profile
//! and module changes while running, like the keyboard.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, bail};
use glassdeck_luau::color::color_api;
use glassdeck_luau::options::{OptionsScript, Panel, Values, any_function, changed_table, options_table};
use glassdeck_luau::profile::{Profile, WidgetArea};
use glassdeck_luau::{LuaResultExt, ResourceReader, Vm, encode};
use glassdeck_package::{Background, ModulePackage, ResourceKind, CanvasFiles, is_module};
use glassdeck_proto::compositor::MAX_DATA_LEN;
use mlua::{Function, Lua, Table, Value};
use sysinfo::{CpuRefreshKind, MemoryRefreshKind, RefreshKind, System};

use crate::host_api::{LiveOutbox, image_api, live_api, media_api};
use crate::link::Uplink;
use crate::media::SharedNowPlaying;
use crate::options::{OptionsStore, read_values};

/// How often the profile and module folder are checked for changes.
const RESCAN: Duration = Duration::from_secs(1);

/// What the API tells the module host.
pub enum HostCommand {
    /// The selected background, when it's a canvas with options or a host half; `None` otherwise.
    Canvas(Option<CanvasSelection>),
    /// The user changed these options of the selected canvas.
    CanvasOptionsChanged(BTreeSet<String>),
}

pub struct CanvasSelection {
    /// Its name on the keyboard, which is also its key in the options store
    pub name: String,
    /// glassdeckd's own copy
    pub path: PathBuf,
}

pub fn start(
    profile: PathBuf,
    modules: PathBuf,
    options: PathBuf,
    uplink: Arc<Uplink>,
    now_playing: SharedNowPlaying,
) -> Result<Sender<HostCommand>> {
    let (commands, inbox) = mpsc::channel();
    thread::Builder::new()
        .name("module host".into())
        .spawn(move || Host::new(profile, modules, OptionsStore::new(options), uplink, now_playing).run(&inbox))?;
    Ok(commands)
}

struct Host {
    profile: PathBuf,
    modules: PathBuf,
    options: OptionsStore,
    uplink: Arc<Uplink>,
    system: Rc<RefCell<System>>,
    now_playing: SharedNowPlaying,
    widgets: Vec<Widget>,
    canvas: Option<HostSide>,
    seen: Option<Snapshot>,
}

#[derive(PartialEq)]
struct Snapshot {
    profile: Option<SystemTime>,
    modules: Vec<(PathBuf, SystemTime)>,
}

/// The latest unsent value of one host side.
type Slot = Rc<RefCell<Option<Vec<u8>>>>;

/// A running `host.luau`.
struct HostVm {
    vm: Vm,
    slot: Slot,
    live: LiveOutbox,
    on_options: Rc<RefCell<Option<Function>>>,
}

struct Widget {
    number: u16,
    side: HostSide,
}

/// A package's host side: what it declares the user can set, and who turns that into data.
struct HostSide {
    label: String,
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
struct Sources {
    label: String,
    options: Option<String>,
    host: Option<String>,
    reader: Option<ResourceReader>,
    send_limit: Option<usize>,
}

impl Host {
    fn new(profile: PathBuf, modules: PathBuf, options: OptionsStore, uplink: Arc<Uplink>, now_playing: SharedNowPlaying) -> Self {
        let refresh =
            RefreshKind::nothing().with_cpu(CpuRefreshKind::nothing().with_usage()).with_memory(MemoryRefreshKind::nothing().with_ram());
        let system = Rc::new(RefCell::new(System::new_with_specifics(refresh).expect("system stats are readable")));
        Self { profile, modules, options, uplink, system, now_playing, widgets: Vec::new(), canvas: None, seen: None }
    }

    fn run(mut self, commands: &Receiver<HostCommand>) {
        let mut next_scan = Instant::now();
        loop {
            let now = Instant::now();
            if now >= next_scan {
                self.rescan();
                next_scan = now + RESCAN;
            }
            self.widgets.retain(|widget| widget.side.run_timers(now));
            if self.canvas.as_ref().is_some_and(|canvas| !canvas.run_timers(now)) {
                self.canvas = None;
            }
            self.flush();
            let sides = self.widgets.iter().map(|w| &w.side).chain(&self.canvas);
            let wake = sides.filter_map(HostSide::next_timer).min().map_or(next_scan, |at| at.min(next_scan));
            match commands.recv_timeout(wake.saturating_duration_since(Instant::now())) {
                Ok(command) => self.obey(command),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    }

    fn flush(&self) {
        for widget in &self.widgets {
            if let Some(payload) = widget.side.slot().borrow_mut().take() {
                self.uplink.put_module_data(widget.number, payload);
            }
            for (name, data) in widget.side.take_live() {
                self.uplink.put_module_resource(widget.number, name, data);
            }
        }
        if let Some(canvas) = &self.canvas {
            if !canvas.take_live().is_empty() {
                eprintln!("error: {}: canvases can't use live resources", canvas.label);
            }
            if let Some(payload) = canvas.slot().borrow_mut().take() {
                self.uplink.put_background_data(payload);
            }
        }
    }

    fn obey(&mut self, command: HostCommand) {
        match command {
            HostCommand::Canvas(None) => self.canvas = None,
            HostCommand::Canvas(Some(selection)) => {
                self.canvas = None;
                match canvas_sources(&selection).and_then(|sources| HostSide::start(sources, &self.system, &self.now_playing)) {
                    Ok(Some(side)) => {
                        println!("Module host: running {}'s host side", selection.name);
                        self.canvas = Some(side);
                        self.apply_canvas_options(&selection.name, None);
                    }
                    Ok(None) => {}
                    Err(e) => eprintln!("error: the canvas {}: {e:#}", selection.name),
                }
            }
            HostCommand::CanvasOptionsChanged(changed) => {
                if let Some(name) = self.canvas.as_ref().map(|s| s.label.clone()) {
                    self.apply_canvas_options(&name, Some(&changed));
                }
            }
        }
    }

    /// The canvas's stored options to its host side: every option changed on the first call.
    fn apply_canvas_options(&self, name: &str, changed: Option<&BTreeSet<String>>) {
        let Some(canvas) = &self.canvas else { return };
        let result = self.options.chosen(name, canvas.first()).and_then(|chosen| canvas.apply_options(&chosen, changed));
        if let Err(e) = result {
            eprintln!("error: {name}'s options: {e:#}");
        }
    }

    fn rescan(&mut self) {
        let snapshot = snapshot(&self.profile, &self.modules);
        if self.seen.as_ref() == Some(&snapshot) {
            return;
        }
        self.seen = Some(snapshot);
        self.widgets.clear();
        self.uplink.forget_module_data();
        match self.load() {
            Ok(widgets) => {
                println!("Module host: {} widget host sides running", widgets.len());
                self.widgets = widgets;
            }
            Err(e) => eprintln!("error: module host: {e:#}"),
        }
    }

    fn load(&self) -> Result<Vec<Widget>> {
        if !self.profile.exists() {
            println!("Module host: no profile at {}, so no widgets", self.profile.display());
            return Ok(Vec::new());
        }
        let packages: HashMap<String, Rc<ModulePackage>> = installed(&self.modules)?.into_iter().map(|(id, p)| (id, Rc::new(p))).collect();
        let profile = Profile::read(&self.profile, WidgetArea::of_panel().size, |id| packages.get(id).map(|p| p.manifest().clone()))?;
        let mut widgets = Vec::new();
        for (index, widget) in profile.widgets.iter().enumerate() {
            let package = &packages[&widget.module];
            let label = format!("widget {index} ({})", widget.module);
            let number = u16::try_from(index).expect("profiles hold at most u16::MAX widgets");
            let readable = Rc::clone(package);
            let started = (|| -> Result<Option<HostSide>> {
                let sources = Sources {
                    label: label.clone(),
                    options: package.options_script()?,
                    host: package.host_script()?,
                    reader: Some(Rc::new(move |resource| readable.resource(resource))),
                    send_limit: None,
                };
                let Some(side) = HostSide::start(sources, &self.system, &self.now_playing)? else { return Ok(None) };
                let chosen = read_values(&label, side.first(), widget.options.iter());
                side.apply_options(&chosen, None)?;
                Ok(Some(side))
            })();
            match started {
                Ok(Some(side)) => widgets.push(Widget { number, side }),
                Ok(None) => {}
                Err(e) => eprintln!("error: {label}: {e:#}"),
            }
        }
        Ok(widgets)
    }
}

/// What the selected canvas's host side is made from, read from glassdeckd's copy.
fn canvas_sources(selection: &CanvasSelection) -> Result<Sources> {
    let background = Background::open(&selection.path)?;
    let plan = CanvasFiles::read(&background)?.plan;
    let read = |name: &Option<String>| -> Result<Option<String>> {
        let Some(name) = name else { return Ok(None) };
        let entry = background.entry(name).with_context(|| format!("the canvas has no {name}"))?;
        Ok(Some(String::from_utf8(background.read_entry(entry)?).with_context(|| format!("{name} isn't UTF-8"))?))
    };
    Ok(Sources {
        label: selection.name.clone(),
        options: read(&plan.options)?,
        host: read(&plan.host)?,
        reader: None,
        send_limit: Some(MAX_DATA_LEN),
    })
}

static NO_OPTIONS: Panel = Panel { items: Vec::new() };

impl HostSide {
    /// `None` when there's nothing to run: no options and no `host.luau`.
    fn start(sources: Sources, system: &Rc<RefCell<System>>, now_playing: &SharedNowPlaying) -> Result<Option<Self>> {
        let Sources { label, options, host, reader, send_limit } = sources;
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
            Some(script) => Run::Script(HostVm::start(&script, reader, send_limit, system, now_playing)?),
            None => Run::Forward { vm: Vm::new()?, slot: Rc::default() },
        };
        Ok(Some(Self { label, options, run }))
    }

    fn first(&self) -> &Panel {
        self.options.as_ref().map_or(&NO_OPTIONS, |o| &o.first)
    }

    /// `host.on_options(options, changed)`, or the options sent as they are.
    fn apply_options(&self, chosen: &Values, changed: Option<&BTreeSet<String>>) -> Result<()> {
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
                host.vm.call::<()>(&on_options, args).context("in host.on_options")
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

    fn slot(&self) -> &Slot {
        match &self.run {
            Run::Script(host) => &host.slot,
            Run::Forward { slot, .. } => slot,
        }
    }

    fn take_live(&self) -> BTreeMap<String, Vec<u8>> {
        match &self.run {
            Run::Script(host) => std::mem::take(&mut *host.live.borrow_mut()),
            Run::Forward { .. } => BTreeMap::new(),
        }
    }

    /// Runs due timers; `false` means the script failed and stopped.
    fn run_timers(&self, now: Instant) -> bool {
        let Run::Script(host) = &self.run else { return true };
        match host.vm.run_due_timers(now) {
            Ok(()) => true,
            Err(e) => {
                eprintln!("error: {}: {e:#}; it stops, the others keep running", self.label);
                false
            }
        }
    }

    fn next_timer(&self) -> Option<Instant> {
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
        reader: Option<ResourceReader>,
        send_limit: Option<usize>,
        system: &Rc<RefCell<System>>,
        now_playing: &SharedNowPlaying,
    ) -> Result<Self> {
        let vm = Vm::new()?;
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
        vm.seal()?;
        vm.load::<()>(script, "host.luau")?;
        Ok(Self { vm, slot, live, on_options })
    }
}

/// `host.on_options(fn)`, beside `host.resource` when the package has resources.
fn install_on_options(lua: &Lua, slot: &Rc<RefCell<Option<Function>>>) -> Result<()> {
    let globals = lua.globals();
    let host: Table = match globals.get::<Value>("host").lua_context("reading host")? {
        Value::Table(table) => table,
        _ => {
            let table = lua.create_table().lua_context("creating host")?;
            globals.set("host", &table).lua_context("installing host")?;
            table
        }
    };
    let registered = Rc::clone(slot);
    let register = lua
        .create_function(move |_, callback: Function| {
            let mut slot = registered.borrow_mut();
            if slot.is_some() {
                return Err(mlua::Error::runtime("host.on_options was already called; register one function"));
            }
            *slot = Some(callback);
            Ok(())
        })
        .lua_context("creating host.on_options")?;
    host.set("on_options", register).lua_context("installing host.on_options")
}

fn system_api(lua: &Lua, system: &Rc<RefCell<System>>) -> Result<Table> {
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

fn snapshot(profile: &Path, modules: &Path) -> Snapshot {
    let modified = |path: &Path| fs::metadata(path).and_then(|m| m.modified()).ok();
    let mut found: Vec<(PathBuf, SystemTime)> = fs::read_dir(modules)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| is_module(path))
        .filter_map(|path| modified(&path).map(|at| (path, at)))
        .collect();
    found.sort();
    Snapshot { profile: modified(profile), modules: found }
}

pub fn installed(folder: &Path) -> Result<HashMap<String, ModulePackage>> {
    let mut packages = HashMap::new();
    let entries = match fs::read_dir(folder) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(packages),
        Err(e) => return Err(e).with_context(|| format!("reading {}", folder.display())),
    };
    for entry in entries {
        let path = entry?.path();
        if is_module(&path) {
            let package = ModulePackage::open(&path)?;
            let id = package.manifest().id.clone();
            if let Some(other) = packages.insert(id.clone(), package) {
                bail!("{} and {} both have the id {id}", other.path().display(), path.display());
            }
        }
    }
    Ok(packages)
}
