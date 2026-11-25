//! The PC host: runs the `pc.luau` half of each widget in a profile and sends what it `send()`s
//! over the Link as `ModuleData`. One VM per instance; each instance has one outgoing slot where the
//! latest value wins, so a slow link never queues stale readings.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use glassdeck_package::{ModulePackage, is_module, pack_folder};
use glassdeck_luau::profile::{Profile, WidgetArea};
use glassdeck_luau::{LuaResultExt, Vm, encode};
use glassdeck_proto::Message;
use mlua::{Lua, Table, Value};
use sysinfo::{CpuRefreshKind, MemoryRefreshKind, RefreshKind, System};

use crate::link::Link;
use crate::pipeline::stop_requested;

/// Longest sleep between checks for Ctrl+C or the end of `--seconds`.
const MAX_WAIT: Duration = Duration::from_millis(200);

#[derive(clap::Subcommand)]
pub enum ModuleAction {
    /// Pack a module folder (module.toml, keyboard.luau, optional pc.luau and assets/) into a .deckmod
    Pack {
        folder: PathBuf,
        /// Defaults to <folder>.deckmod
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Run the PC halves of a profile's widgets and send their data to the keyboard
    Run {
        #[arg(long)]
        profile: PathBuf,
        /// Folder of .deckmod files, the same ones installed on the keyboard
        #[arg(long)]
        modules: PathBuf,
        #[arg(long, default_value = "127.0.0.1:7878")]
        to: std::net::SocketAddr,
        #[arg(long)]
        seconds: Option<u64>,
    },
}

pub fn run(action: ModuleAction) -> Result<()> {
    match action {
        ModuleAction::Pack { folder, out } => {
            let out = out.unwrap_or_else(|| folder.with_extension(glassdeck_package::MODULE_EXTENSION));
            let package = pack_folder(&folder, &out)?;
            println!("Packed {} into {}", package.manifest().id, out.display());
            Ok(())
        }
        ModuleAction::Run { profile, modules, to, seconds } => run_pc_halves(&profile, &modules, to, seconds.map(Duration::from_secs)),
    }
}

/// The latest unsent value of one instance.
type Slot = Rc<RefCell<Option<Vec<u8>>>>;

struct PcInstance {
    label: String,
    number: u16,
    slot: Slot,
    vm: Vm,
}

fn run_pc_halves(profile: &Path, modules: &Path, to: std::net::SocketAddr, duration: Option<Duration>) -> Result<()> {
    let packages = installed(modules)?;
    let profile = Profile::read(profile, WidgetArea::of_panel().size, |id| packages.get(id).map(|p| p.manifest().clone()))?;
    let refresh = RefreshKind::nothing().with_cpu(CpuRefreshKind::nothing().with_usage()).with_memory(MemoryRefreshKind::nothing().with_ram());
    let system = Rc::new(RefCell::new(System::new_with_specifics(refresh).context("reading system stats")?));
    let mut instances = Vec::new();
    for (index, widget) in profile.widgets.iter().enumerate() {
        let package = &packages[&widget.module];
        if let Some(script) = package.pc_script()? {
            let label = format!("widget {index} ({})", widget.module);
            let number = u16::try_from(index).expect("profiles hold at most u16::MAX widgets");
            let instance = PcInstance::start(label, number, &script, &system).with_context(|| format!("starting widget {index}"))?;
            instances.push(instance);
        }
    }
    println!("Running {} PC halves for {} widgets, sending to {to}", instances.len(), profile.widgets.len());
    let mut link = Link::connect(to, None)?;
    let started = Instant::now();
    while !stop_requested() && duration.is_none_or(|d| started.elapsed() < d) {
        let now = Instant::now();
        instances.retain(|instance| match instance.vm.run_due_timers(now) {
            Ok(()) => true,
            Err(e) => {
                eprintln!("error: {}: {e:#}; it stops, the others keep running", instance.label);
                false
            }
        });
        for instance in &instances {
            if let Some(payload) = instance.slot.borrow_mut().take() {
                link.send(&Message::ModuleData { instance: instance.number, payload })?;
            }
        }
        let next = instances.iter().filter_map(|i| i.vm.next_timer()).min();
        let wait = next.map_or(MAX_WAIT, |at| at.saturating_duration_since(Instant::now()).min(MAX_WAIT));
        thread::sleep(wait);
    }
    Ok(())
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

fn installed(folder: &Path) -> Result<HashMap<String, ModulePackage>> {
    let mut packages = HashMap::new();
    for entry in std::fs::read_dir(folder).with_context(|| format!("reading {}", folder.display()))? {
        let path = entry?.path();
        if is_module(&path) {
            let package = ModulePackage::open(&path)?;
            let id = package.manifest().id.clone();
            if let Some(other) = packages.insert(id.clone(), package) {
                anyhow::bail!("{} and {} both have the id {id}", other.path().display(), path.display());
            }
        }
    }
    Ok(packages)
}
