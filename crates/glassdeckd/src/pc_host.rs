//! The PC host: runs the `pc.luau` half of each widget in the profile, one VM per instance, and hands
//! what each `send()`s to the Link. Picks up profile and module changes while running, like the keyboard.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, bail};
use glassdeck_luau::profile::{Profile, WidgetArea};
use glassdeck_luau::{LuaResultExt, Vm, encode};
use glassdeck_package::{ModulePackage, is_module};
use mlua::{Lua, Table, Value};
use sysinfo::{CpuRefreshKind, MemoryRefreshKind, RefreshKind, System};

use crate::link::Uplink;

/// How often the profile and module folder are checked for changes.
const RESCAN: Duration = Duration::from_secs(1);

pub fn start(profile: PathBuf, modules: PathBuf, uplink: Arc<Uplink>) -> Result<()> {
    thread::Builder::new().name("pc host".into()).spawn(move || Host::new(profile, modules, uplink).run())?;
    Ok(())
}

struct Host {
    profile: PathBuf,
    modules: PathBuf,
    uplink: Arc<Uplink>,
    system: Rc<RefCell<System>>,
    instances: Vec<PcInstance>,
    seen: Option<Snapshot>,
}

#[derive(PartialEq)]
struct Snapshot {
    profile: Option<SystemTime>,
    modules: Vec<(PathBuf, SystemTime)>,
}

/// The latest unsent value of one instance.
type Slot = Rc<RefCell<Option<Vec<u8>>>>;

struct PcInstance {
    label: String,
    number: u16,
    slot: Slot,
    vm: Vm,
}

impl Host {
    fn new(profile: PathBuf, modules: PathBuf, uplink: Arc<Uplink>) -> Self {
        let refresh = RefreshKind::nothing().with_cpu(CpuRefreshKind::nothing().with_usage()).with_memory(MemoryRefreshKind::nothing().with_ram());
        let system = Rc::new(RefCell::new(System::new_with_specifics(refresh).expect("system stats are readable")));
        Self { profile, modules, uplink, system, instances: Vec::new(), seen: None }
    }

    fn run(mut self) {
        let mut next_scan = Instant::now();
        loop {
            let now = Instant::now();
            if now >= next_scan {
                self.rescan();
                next_scan = now + RESCAN;
            }
            self.instances.retain(|instance| match instance.vm.run_due_timers(now) {
                Ok(()) => true,
                Err(e) => {
                    eprintln!("error: {}: {e:#}; it stops, the others keep running", instance.label);
                    false
                }
            });
            for instance in &self.instances {
                if let Some(payload) = instance.slot.borrow_mut().take() {
                    self.uplink.put_module_data(instance.number, payload);
                }
            }
            let wake = self.instances.iter().filter_map(|i| i.vm.next_timer()).min().map_or(next_scan, |at| at.min(next_scan));
            thread::sleep(wake.saturating_duration_since(Instant::now()));
        }
    }

    fn rescan(&mut self) {
        let snapshot = snapshot(&self.profile, &self.modules);
        if self.seen.as_ref() == Some(&snapshot) {
            return;
        }
        self.seen = Some(snapshot);
        self.instances.clear();
        self.uplink.forget_module_data();
        match self.load() {
            Ok(instances) => {
                println!("PC host: {} PC halves running", instances.len());
                self.instances = instances;
            }
            Err(e) => eprintln!("error: PC host: {e:#}"),
        }
    }

    fn load(&self) -> Result<Vec<PcInstance>> {
        if !self.profile.exists() {
            println!("PC host: no profile at {}, so no widgets", self.profile.display());
            return Ok(Vec::new());
        }
        let packages = installed(&self.modules)?;
        let profile = Profile::read(&self.profile, WidgetArea::of_panel().size, |id| packages.get(id).map(|p| p.manifest().clone()))?;
        let mut instances = Vec::new();
        for (index, widget) in profile.widgets.iter().enumerate() {
            if let Some(script) = packages[&widget.module].pc_script()? {
                let label = format!("widget {index} ({})", widget.module);
                let number = u16::try_from(index).expect("profiles hold at most u16::MAX widgets");
                match PcInstance::start(label.clone(), number, &script, &self.system) {
                    Ok(instance) => instances.push(instance),
                    Err(e) => eprintln!("error: {label}: {e:#}"),
                }
            }
        }
        Ok(instances)
    }
}

impl PcInstance {
    fn start(label: String, number: u16, script: &str, system: &Rc<RefCell<System>>) -> Result<Self> {
        let vm = Vm::new()?;
        let slot: Slot = Rc::new(RefCell::new(None));
        let lua = vm.lua();
        let globals = lua.globals();
        globals.set("every", vm.every_function()?).lua_context("installing every")?;
        let outgoing = Rc::clone(&slot);
        let send = lua
            .create_function(move |lua, data: Value| {
                let bytes = encode(lua, data).map_err(|e| mlua::Error::runtime(format!("send: {e:#}")))?;
                *outgoing.borrow_mut() = Some(bytes);
                Ok(())
            })
            .lua_context("creating send")?;
        globals.set("send", send).lua_context("installing send")?;
        globals.set("system", system_api(lua, system)?).lua_context("installing system")?;
        vm.seal()?;
        vm.load::<()>(script, "pc.luau")?;
        Ok(Self { label, number, slot, vm })
    }
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

fn installed(folder: &Path) -> Result<HashMap<String, ModulePackage>> {
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
