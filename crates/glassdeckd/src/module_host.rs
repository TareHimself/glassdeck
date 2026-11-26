//! The module host: runs the host side of the active theme's background and of each of its widgets,
//! and hands what each sends to the Link. A host side is a package's options (from
//! `options.luau`) and its `host.luau`, which turns them into data for the keyboard; a package with
//! options but no `host.luau` gets an implicit one that forwards them as they are. Picks up new themes
//! and package installs while running, and restarts only what changed.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, bail};
use glassdeck_luau::color::color_api;
use glassdeck_luau::options::{OptionsScript, Panel, Values, any_function, changed_table, options_table, read_values};
use glassdeck_luau::{LuaResultExt, ResourceReader, Vm, encode};
use glassdeck_package::{Package, PackageId, ResourceKind, is_package};
use glassdeck_proto::compositor::MAX_DATA_LEN;
use mlua::{Function, Lua, Table, Value};
use sysinfo::{CpuRefreshKind, MemoryRefreshKind, RefreshKind, System};

use crate::host_api::{LiveOutbox, image_api, live_api, media_api};
use crate::link::Uplink;
use crate::media::SharedNowPlaying;
use crate::theme::{self, ActiveTheme, Chosen};

/// How often the profile and module folder are checked for changes.
const RESCAN: Duration = Duration::from_secs(1);

/// What the API tells the module host.
pub enum HostCommand {
    /// A new active theme
    Theme(ActiveTheme),
}

pub fn start(packages: PathBuf, theme: ActiveTheme, uplink: Arc<Uplink>, now_playing: SharedNowPlaying) -> Result<Sender<HostCommand>> {
    let (commands, inbox) = mpsc::channel();
    thread::Builder::new()
        .name("module host".into())
        .spawn(move || Host::new(packages, theme, uplink, now_playing).run(&inbox))?;
    Ok(commands)
}

struct Host {
    packages: PathBuf,
    theme: ActiveTheme,
    uplink: Arc<Uplink>,
    system: Rc<RefCell<System>>,
    now_playing: SharedNowPlaying,
    background: Option<Running>,
    widgets: BTreeMap<u16, Running>,
    /// The packages folder as the last scan saw it, to notice installs.
    seen: Option<Vec<(PathBuf, SystemTime)>>,
}

/// A running host side, and what it was started from.
struct Running {
    entry: SideEntry,
    side: HostSide,
}

/// What a host side runs from: its package as installed, and the user's chosen options.
#[derive(Clone, Debug, PartialEq)]
struct SideEntry {
    package: PackageId,
    modified: SystemTime,
    options: Chosen,
}

/// How one host side changes between what runs and what's wanted.
#[derive(Debug, PartialEq)]
enum Change {
    /// A new one, or one whose package changed
    Start,
    /// Only chosen options changed: tell the running host side which
    Reapply(BTreeSet<String>),
    Stop,
}

/// Compares a running host side with the one wanted; `None` means it keeps running as it is.
fn side_change(running: Option<&SideEntry>, wanted: Option<&SideEntry>) -> Option<Change> {
    match (running, wanted) {
        (None, None) => None,
        (Some(_), None) => Some(Change::Stop),
        (None, Some(_)) => Some(Change::Start),
        (Some(now), Some(entry)) if (&now.package, now.modified) != (&entry.package, entry.modified) => Some(Change::Start),
        (Some(now), Some(entry)) => {
            let changed: BTreeSet<String> = now
                .options
                .keys()
                .chain(entry.options.keys())
                .filter(|path| now.options.get(*path) != entry.options.get(*path))
                .cloned()
                .collect();
            (!changed.is_empty()).then_some(Change::Reapply(changed))
        }
    }
}

/// Each widget's change, by its place in the theme.
fn widget_changes(running: &BTreeMap<u16, SideEntry>, wanted: &BTreeMap<u16, SideEntry>) -> Vec<(u16, Change)> {
    let numbers: BTreeSet<u16> = running.keys().chain(wanted.keys()).copied().collect();
    numbers.into_iter().filter_map(|n| side_change(running.get(&n), wanted.get(&n)).map(|change| (n, change))).collect()
}

/// Where a host side's data goes on the Link.
#[derive(Clone, Copy)]
enum Target {
    Background,
    Widget(u16),
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
    fn new(packages: PathBuf, theme: ActiveTheme, uplink: Arc<Uplink>, now_playing: SharedNowPlaying) -> Self {
        let refresh =
            RefreshKind::nothing().with_cpu(CpuRefreshKind::nothing().with_usage()).with_memory(MemoryRefreshKind::nothing().with_ram());
        let system = Rc::new(RefCell::new(System::new_with_specifics(refresh).expect("system stats are readable")));
        Self { packages, theme, uplink, system, now_playing, background: None, widgets: BTreeMap::new(), seen: None }
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
            if self.background.as_ref().is_some_and(|b| !b.side.run_timers(now)) {
                self.background = None;
            }
            self.flush();
            let sides = self.widgets.values().chain(&self.background).map(|r| &r.side);
            let wake = sides.filter_map(HostSide::next_timer).min().map_or(next_scan, |at| at.min(next_scan));
            match commands.recv_timeout(wake.saturating_duration_since(Instant::now())) {
                Ok(HostCommand::Theme(theme)) => {
                    self.theme = theme;
                    self.sync();
                }
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
        if let Some(Running { side, .. }) = &self.background {
            if !side.take_live().is_empty() {
                eprintln!("error: {}: backgrounds can't use live resources", side.label);
            }
            if let Some(payload) = side.slot().borrow_mut().take() {
                self.uplink.put_background_data(payload);
            }
        }
    }

    fn rescan(&mut self) {
        let snapshot = snapshot(&self.packages);
        if self.seen.as_ref() == Some(&snapshot) {
            return;
        }
        self.seen = Some(snapshot);
        self.sync();
    }

    /// Brings the running host sides in line with the theme and the installed packages.
    fn sync(&mut self) {
        let mut installed = match theme::installed(&self.packages) {
            Ok(installed) => installed,
            Err(e) => return eprintln!("error: module host: {e:#}"),
        };
        let mut entry_of = |entry: &theme::Entry| -> Option<(SideEntry, Rc<Package>)> {
            let found = installed.get(&entry.package).filter(|i| i.package.runs_on_host())?;
            let side = SideEntry { package: entry.package.clone(), modified: found.modified, options: entry.options.clone() };
            let package = installed.remove(&entry.package).map(|i| Rc::new(i.package))?;
            Some((side, package))
        };
        let background = entry_of(&self.theme.background);
        let mut wanted: BTreeMap<u16, (SideEntry, Rc<Package>)> = BTreeMap::new();
        for (index, (_, entry)) in self.theme.widgets.iter().enumerate() {
            let number = u16::try_from(index).expect("themes hold at most u16::MAX widgets");
            if let Some(found) = entry_of(entry) {
                wanted.insert(number, found);
            }
        }

        let running_background = self.background.as_ref().map(|r| &r.entry);
        if let Some(change) = side_change(running_background, background.as_ref().map(|(e, _)| e)) {
            let old = self.background.take();
            self.background = self.changed(Target::Background, old, change, background);
        }
        let running: BTreeMap<u16, SideEntry> = self.widgets.iter().map(|(&n, r)| (n, r.entry.clone())).collect();
        let entries: BTreeMap<u16, SideEntry> = wanted.iter().map(|(&n, (e, _))| (n, e.clone())).collect();
        for (number, change) in widget_changes(&running, &entries) {
            let old = self.widgets.remove(&number);
            if matches!(change, Change::Start | Change::Stop) {
                self.uplink.forget_module_instance(number);
            }
            if let Some(kept) = self.changed(Target::Widget(number), old, change, wanted.remove(&number)) {
                self.widgets.insert(number, kept);
            }
        }
    }

    /// Carries out one change, and returns what then runs in that place.
    fn changed(&self, target: Target, old: Option<Running>, change: Change, wanted: Option<(SideEntry, Rc<Package>)>) -> Option<Running> {
        match change {
            Change::Stop => None,
            Change::Start => {
                drop(old);
                let (entry, package) = wanted.expect("a start has what it starts");
                self.start_side(target, entry, &package)
            }
            Change::Reapply(changed) => {
                let mut running = old.expect("only running host sides are reapplied");
                running.entry = wanted.expect("a reapply has its new options").0;
                let label = &running.side.label;
                let chosen = read_values(label, running.side.first(), running.entry.options.iter());
                if let Err(e) = running.side.apply_options(&chosen, Some(&changed)) {
                    eprintln!("error: {label}'s options: {e:#}");
                }
                Some(running)
            }
        }
    }

    fn start_side(&self, target: Target, entry: SideEntry, package: &Rc<Package>) -> Option<Running> {
        let label = match target {
            Target::Background => format!("background ({})", entry.package),
            Target::Widget(number) => format!("widget {number} ({})", entry.package),
        };
        let started = (|| -> Result<Option<HostSide>> {
            let readable = Rc::clone(package);
            let (reader, send_limit): (Option<ResourceReader>, _) = match target {
                Target::Background => (None, Some(MAX_DATA_LEN)),
                Target::Widget(_) => (Some(Rc::new(move |resource| readable.resource(resource))), None),
            };
            let sources = Sources { label: label.clone(), options: package.options_script()?, host: package.host_script()?, reader, send_limit };
            let Some(side) = HostSide::start(sources, &self.system, &self.now_playing)? else { return Ok(None) };
            side.apply_options(&read_values(&label, side.first(), entry.options.iter()), None)?;
            Ok(Some(side))
        })();
        match started {
            Ok(side) => {
                println!("Module host: started {label}");
                side.map(|side| Running { entry, side })
            }
            Err(e) => {
                eprintln!("error: {label}: {e:#}");
                None
            }
        }
    }
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

fn snapshot(packages: &Path) -> Vec<(PathBuf, SystemTime)> {
    let modified = |path: &Path| fs::metadata(path).and_then(|m| m.modified()).ok();
    let mut found: Vec<(PathBuf, SystemTime)> = fs::read_dir(packages)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| is_package(path))
        .filter_map(|path| modified(&path).map(|at| (path, at)))
        .collect();
    found.sort();
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(package: &str, at: u64, options: serde_json::Value) -> SideEntry {
        SideEntry {
            package: PackageId::parse(package).unwrap(),
            modified: SystemTime::UNIX_EPOCH + Duration::from_secs(at),
            options: options.as_object().cloned().unwrap_or_default(),
        }
    }

    #[test]
    fn only_what_changed_restarts_or_hears_about_it() {
        let clock = entry("dev.glassdeck.clock", 1, serde_json::json!({ "seconds": true }));
        let running = BTreeMap::from([
            (0, clock.clone()),
            (1, entry("dev.glassdeck.cpu", 1, serde_json::json!({}))),
            (2, entry("dev.glassdeck.pomodoro", 1, serde_json::json!({}))),
        ]);
        let wanted = BTreeMap::from([
            (0, entry("dev.glassdeck.clock", 1, serde_json::json!({ "seconds": false, "hours24": true }))),
            (1, entry("dev.glassdeck.cpu", 2, serde_json::json!({}))),
            (3, entry("dev.glassdeck.now-playing", 1, serde_json::json!({}))),
        ]);
        assert_eq!(widget_changes(&running, &wanted), vec![
            (0, Change::Reapply(BTreeSet::from(["hours24".to_owned(), "seconds".to_owned()]))),
            (1, Change::Start),
            (2, Change::Stop),
            (3, Change::Start),
        ]);
        assert!(widget_changes(&running, &running).is_empty());
        let swapped = entry("dev.glassdeck.cpu", 1, serde_json::json!({ "seconds": true }));
        assert_eq!(side_change(Some(&clock), Some(&swapped)), Some(Change::Start));
        assert_eq!(side_change(Some(&clock), None), Some(Change::Stop));
        assert_eq!(side_change(None, None), None);
    }
}
