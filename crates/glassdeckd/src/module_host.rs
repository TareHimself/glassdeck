//! The module host: runs the host side of each widget in the profile and of the selected canvas
//! background, and hands what each sends to the Link. A host side is a package's options (from
//! `options.luau`) and its `host.luau`, which turns them into data for the keyboard; a package with
//! options but no `host.luau` gets an implicit one that forwards them as they are. Picks up profile
//! and module changes while running, like the keyboard, and restarts only what changed.

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
    widgets: BTreeMap<u16, Widget>,
    canvas: Option<RunningCanvas>,
    seen: Option<Snapshot>,
}

/// The selected canvas's host side, and which copy of the canvas it was started from.
struct RunningCanvas {
    /// Its name on the keyboard, which is also its key in the options store
    name: String,
    modified: Option<SystemTime>,
    side: HostSide,
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
    entry: WidgetEntry,
    side: HostSide,
}

/// What a widget's host side runs from: its module's package as installed, and its stored options.
#[derive(Clone, Debug, PartialEq)]
struct WidgetEntry {
    module: String,
    package: PathBuf,
    modified: SystemTime,
    options: serde_json::Map<String, serde_json::Value>,
}

/// How one place in the profile changes between scans.
#[derive(Debug, PartialEq)]
enum WidgetChange {
    /// A new widget, or one whose module or package changed
    Start(u16),
    /// Only stored options changed: tell the running host side which
    Reapply { number: u16, changed: BTreeSet<String> },
    /// Gone from the profile
    Stop(u16),
}

/// Compares the running widgets with what the profile now asks for. Unchanged widgets keep running.
fn widget_changes(running: &BTreeMap<u16, WidgetEntry>, wanted: &BTreeMap<u16, WidgetEntry>) -> Vec<WidgetChange> {
    let stopped = running.keys().filter(|number| !wanted.contains_key(number)).map(|&number| WidgetChange::Stop(number));
    let started_or_changed = wanted.iter().filter_map(|(&number, entry)| match running.get(&number) {
        None => Some(WidgetChange::Start(number)),
        Some(now) if (&now.module, &now.package, now.modified) != (&entry.module, &entry.package, entry.modified) => {
            Some(WidgetChange::Start(number))
        }
        Some(now) => {
            let changed: BTreeSet<String> = now
                .options
                .keys()
                .chain(entry.options.keys())
                .filter(|path| now.options.get(*path) != entry.options.get(*path))
                .cloned()
                .collect();
            (!changed.is_empty()).then_some(WidgetChange::Reapply { number, changed })
        }
    });
    stopped.chain(started_or_changed).collect()
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
        Self { profile, modules, options, uplink, system, now_playing, widgets: BTreeMap::new(), canvas: None, seen: None }
    }

    fn run(mut self, commands: &Receiver<HostCommand>) {
        let mut next_scan = Instant::now();
        loop {
            let now = Instant::now();
            if now >= next_scan {
                self.rescan();
                next_scan = now + RESCAN;
            }
            self.widgets.retain(|_, widget| widget.side.run_timers(now));
            if self.canvas.as_ref().is_some_and(|canvas| !canvas.side.run_timers(now)) {
                self.canvas = None;
            }
            self.flush();
            let sides = self.widgets.values().map(|w| &w.side).chain(self.canvas.as_ref().map(|c| &c.side));
            let wake = sides.filter_map(HostSide::next_timer).min().map_or(next_scan, |at| at.min(next_scan));
            match commands.recv_timeout(wake.saturating_duration_since(Instant::now())) {
                Ok(command) => self.obey(command),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    }

    fn flush(&self) {
        for (&number, widget) in &self.widgets {
            if let Some(payload) = widget.side.slot().borrow_mut().take() {
                self.uplink.put_module_data(number, payload);
            }
            for (name, data) in widget.side.take_live() {
                self.uplink.put_module_resource(number, name, data);
            }
        }
        if let Some(RunningCanvas { side, .. }) = &self.canvas {
            if !side.take_live().is_empty() {
                eprintln!("error: {}: canvases can't use live resources", side.label);
            }
            if let Some(payload) = side.slot().borrow_mut().take() {
                self.uplink.put_background_data(payload);
            }
        }
    }

    fn obey(&mut self, command: HostCommand) {
        match command {
            HostCommand::Canvas(None) => self.canvas = None,
            HostCommand::Canvas(Some(selection)) => {
                let modified = fs::metadata(&selection.path).and_then(|m| m.modified()).ok();
                if self.canvas.as_ref().is_some_and(|c| c.name == selection.name && c.modified == modified) {
                    return;
                }
                self.canvas = None;
                match canvas_sources(&selection).and_then(|sources| HostSide::start(sources, &self.system, &self.now_playing)) {
                    Ok(Some(side)) => {
                        println!("Module host: running {}'s host side", selection.name);
                        self.canvas = Some(RunningCanvas { name: selection.name, modified, side });
                        self.apply_canvas_options(None);
                    }
                    Ok(None) => {}
                    Err(e) => eprintln!("error: the canvas {}: {e:#}", selection.name),
                }
            }
            HostCommand::CanvasOptionsChanged(changed) => self.apply_canvas_options(Some(&changed)),
        }
    }

    /// The canvas's stored options to its host side: every option changed on the first call.
    fn apply_canvas_options(&self, changed: Option<&BTreeSet<String>>) {
        let Some(RunningCanvas { name, side, .. }) = &self.canvas else { return };
        let result = self.options.chosen(name, side.first()).and_then(|chosen| side.apply_options(&chosen, changed));
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
        let wanted = match self.wanted() {
            Ok(wanted) => wanted,
            Err(e) => return eprintln!("error: module host: {e:#}"),
        };
        let running = self.widgets.iter().map(|(&number, widget)| (number, widget.entry.clone())).collect();
        let changes = widget_changes(&running, &wanted.iter().map(|(&n, (entry, _))| (n, entry.clone())).collect());
        for change in changes {
            match change {
                WidgetChange::Stop(number) => {
                    self.widgets.remove(&number);
                    self.uplink.forget_module_instance(number);
                }
                WidgetChange::Start(number) => {
                    self.widgets.remove(&number);
                    self.uplink.forget_module_instance(number);
                    let (entry, package) = &wanted[&number];
                    if let Some(widget) = self.start_widget(number, entry, package) {
                        self.widgets.insert(number, widget);
                    }
                }
                WidgetChange::Reapply { number, changed } => {
                    let widget = self.widgets.get_mut(&number).expect("only running widgets are reapplied");
                    widget.entry = wanted[&number].0.clone();
                    let label = &widget.side.label;
                    let chosen = read_values(label, widget.side.first(), widget.entry.options.iter());
                    if let Err(e) = widget.side.apply_options(&chosen, Some(&changed)) {
                        eprintln!("error: {label}'s options: {e:#}");
                    }
                }
            }
        }
    }

    /// The widgets the profile asks for, by number, with the package each runs from.
    fn wanted(&self) -> Result<BTreeMap<u16, (WidgetEntry, Rc<ModulePackage>)>> {
        if !self.profile.exists() {
            println!("Module host: no profile at {}, so no widgets", self.profile.display());
            return Ok(BTreeMap::new());
        }
        let packages: HashMap<String, Rc<ModulePackage>> = installed(&self.modules)?.into_iter().map(|(id, p)| (id, Rc::new(p))).collect();
        let profile = Profile::read(&self.profile, WidgetArea::of_panel().size, |id| packages.get(id).map(|p| p.manifest().clone()))?;
        let mut wanted = BTreeMap::new();
        for (index, widget) in profile.widgets.into_iter().enumerate() {
            let number = u16::try_from(index).expect("profiles hold at most u16::MAX widgets");
            let package = Rc::clone(&packages[&widget.module]);
            let modified = fs::metadata(package.path()).and_then(|m| m.modified()).with_context(|| format!("reading {}", package.path().display()))?;
            let entry = WidgetEntry { module: widget.module, package: package.path().to_owned(), modified, options: widget.options };
            wanted.insert(number, (entry, package));
        }
        Ok(wanted)
    }

    fn start_widget(&self, number: u16, entry: &WidgetEntry, package: &Rc<ModulePackage>) -> Option<Widget> {
        let label = format!("widget {number} ({})", entry.module);
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
            side.apply_options(&read_values(&label, side.first(), entry.options.iter()), None)?;
            Ok(Some(side))
        })();
        match started {
            Ok(side) => {
                println!("Module host: started {label}");
                side.map(|side| Widget { entry: entry.clone(), side })
            }
            Err(e) => {
                eprintln!("error: {label}: {e:#}");
                None
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(module: &str, at: u64, options: serde_json::Value) -> WidgetEntry {
        WidgetEntry {
            module: module.into(),
            package: PathBuf::from(format!("{module}.deckmod")),
            modified: SystemTime::UNIX_EPOCH + Duration::from_secs(at),
            options: options.as_object().cloned().unwrap_or_default(),
        }
    }

    #[test]
    fn only_what_changed_restarts_or_hears_about_it() {
        let clock = entry("clock", 1, serde_json::json!({ "seconds": true }));
        let running = BTreeMap::from([(0, clock.clone()), (1, entry("cpu", 1, serde_json::json!({}))), (2, entry("pomodoro", 1, serde_json::json!({})))]);
        let wanted = BTreeMap::from([
            (0, entry("clock", 1, serde_json::json!({ "seconds": false, "hours24": true }))),
            (1, entry("cpu", 2, serde_json::json!({}))),
            (3, entry("now-playing", 1, serde_json::json!({}))),
        ]);
        assert_eq!(widget_changes(&running, &wanted), vec![
            WidgetChange::Stop(2),
            WidgetChange::Reapply { number: 0, changed: BTreeSet::from(["hours24".to_owned(), "seconds".to_owned()]) },
            WidgetChange::Start(1),
            WidgetChange::Start(3),
        ]);
        assert!(widget_changes(&running, &running).is_empty());
        let swapped = BTreeMap::from([(0, entry("cpu", 1, serde_json::json!({ "seconds": true })))]);
        assert_eq!(widget_changes(&BTreeMap::from([(0, clock)]), &swapped), vec![WidgetChange::Start(0)]);
    }
}
