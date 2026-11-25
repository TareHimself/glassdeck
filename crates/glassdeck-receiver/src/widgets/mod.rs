//! The widget area's modules: a thread that runs each placed module's `keyboard.luau` and hands the
//! compositor tiles to put on the key layer. Profile and module changes are picked up while running.

mod draw;
mod installing;
mod instance;
mod ui;

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result};
use fontdue::Font;
use glassdeck_package::{ModulePackage, is_module};
use glassdeck_luau::profile::{Placement, Profile, WidgetArea};

use installing::Installing;
use instance::Instance;

/// How often the profile and module folder are checked for changes.
const RESCAN: Duration = Duration::from_secs(1);
const FRAME: Duration = Duration::from_micros(16_667);

/// New pixels for part of the panel, premultiplied ARGB, row by row.
pub struct Tile {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    pub pixels: Vec<u32>,
}

/// Tiles waiting for the compositor. A newer tile for the same spot replaces the waiting one.
pub struct Tiles {
    waiting: Mutex<Vec<Tile>>,
}

impl Tiles {
    pub fn new() -> Self {
        Self { waiting: Mutex::new(Vec::new()) }
    }

    fn put(&self, tile: Tile) {
        let mut waiting = self.waiting.lock().expect("tiles poisoned");
        waiting.retain(|t| (t.x, t.y, t.w, t.h) != (tile.x, tile.y, tile.w, tile.h));
        waiting.push(tile);
    }

    pub fn take(&self) -> Vec<Tile> {
        std::mem::take(&mut *self.waiting.lock().expect("tiles poisoned"))
    }
}

/// A module package, and when the file was written, so a rewrite counts as a new version.
#[derive(Clone)]
pub struct Installed {
    pub package: Rc<ModulePackage>,
    pub modified: SystemTime,
}

impl Installed {
    fn same_version(&self, other: &Installed) -> bool {
        self.package.path() == other.package.path() && self.modified == other.modified
    }
}

pub enum Input {
    Data { instance: u16, payload: Vec<u8> },
    /// A large file is arriving: the widget area shows its progress instead of the widgets.
    InstallProgress { what: String, fraction: f32 },
    InstallEnded,
}

pub struct Config {
    pub modules: PathBuf,
    pub profile: PathBuf,
    pub font: Font,
}

pub fn start(config: Config, tiles: Arc<Tiles>) -> Result<Sender<Input>> {
    let (inbox, inputs) = mpsc::channel();
    thread::Builder::new().name("widgets".into()).spawn(move || Host::new(config, tiles).run(inputs))?;
    Ok(inbox)
}

struct Host {
    config: Config,
    area: WidgetArea,
    tiles: Arc<Tiles>,
    instances: Vec<Instance>,
    /// What the last scan saw, to notice changes.
    seen: Option<Snapshot>,
    last_frame: Option<Instant>,
    /// While set, widgets keep running but their redraws wait until the install ends.
    installing: Option<Installing>,
}

#[derive(PartialEq)]
struct Snapshot {
    profile: Option<SystemTime>,
    modules: Vec<(PathBuf, SystemTime)>,
}

impl Host {
    fn new(config: Config, tiles: Arc<Tiles>) -> Self {
        Self { config, area: WidgetArea::of_panel(), tiles, instances: Vec::new(), seen: None, last_frame: None, installing: None }
    }

    fn run(mut self, inputs: Receiver<Input>) {
        let mut next_scan = Instant::now();
        loop {
            let now = Instant::now();
            if now >= next_scan {
                self.rescan();
                next_scan = now + RESCAN;
            }
            let wake = self.next_wake().map_or(next_scan, |at| at.min(next_scan));
            match inputs.recv_timeout(wake.saturating_duration_since(Instant::now())) {
                Ok(Input::Data { instance, payload }) => match self.instances.get_mut(usize::from(instance)) {
                    Some(target) => target.on_data(&payload),
                    None => eprintln!("error: data for widget {instance}, but the profile has {}", self.instances.len()),
                },
                Ok(Input::InstallProgress { what, fraction }) => match &mut self.installing {
                    Some(installing) if installing.what == what => installing.fraction = fraction,
                    _ => self.installing = Some(Installing::new(what, fraction)),
                },
                Ok(Input::InstallEnded) => self.end_install(),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
            self.tick(Instant::now());
            self.publish();
        }
    }

    fn next_wake(&self) -> Option<Instant> {
        let timers = self.instances.iter().filter_map(Instance::next_timer).min();
        let frame = self.instances.iter().any(Instance::wants_frame).then(|| self.last_frame.map_or_else(Instant::now, |at| at + FRAME));
        timers.into_iter().chain(frame).min()
    }

    fn tick(&mut self, now: Instant) {
        let animating = self.instances.iter().any(Instance::wants_frame);
        let frame_dt = match (animating, self.last_frame) {
            (false, _) => {
                self.last_frame = None;
                None
            }
            (true, None) => Some(FRAME),
            (true, Some(last)) if now >= last + FRAME => Some(now - last),
            (true, Some(_)) => None,
        };
        if frame_dt.is_some() {
            self.last_frame = Some(now);
        }
        for instance in &mut self.instances {
            instance.tick(now, frame_dt);
        }
    }

    fn publish(&mut self) {
        if let Some(installing) = &mut self.installing {
            let size = (self.area.size.w, self.area.size.h);
            match installing.draw(size, &self.config.font) {
                Ok(Some(pixels)) => self.tiles.put(Tile { x: self.area.x, y: self.area.y, w: size.0, h: size.1, pixels }),
                Ok(None) => {}
                Err(e) => eprintln!("error: drawing the install progress: {e:#}"),
            }
            return;
        }
        for instance in &mut self.instances {
            if let Some(tile) = instance.draw(&self.config.font) {
                let p = instance.placement;
                self.tiles.put(Tile { x: self.area.x + p.x, y: self.area.y + p.y, w: p.w, h: p.h, pixels: tile.pixels });
            }
        }
    }

    /// Takes the progress panel down and has every widget draw itself again where it was.
    fn end_install(&mut self) {
        if self.installing.take().is_none() {
            return;
        }
        let (w, h) = (self.area.size.w, self.area.size.h);
        self.clear(Placement { x: 0, y: 0, w, h });
        for instance in &mut self.instances {
            instance.request_redraw();
        }
    }

    fn clear(&self, placement: Placement) {
        let Placement { x, y, w, h } = placement;
        self.tiles.put(Tile { x: self.area.x + x, y: self.area.y + y, w, h, pixels: vec![0; (w * h) as usize] });
    }

    fn rescan(&mut self) {
        let snapshot = self.snapshot();
        if self.seen.as_ref() == Some(&snapshot) {
            return;
        }
        self.seen = Some(snapshot);
        match self.load() {
            Ok((profile, packages)) => self.apply(profile, packages),
            Err(e) => {
                eprintln!("error: widgets: {e:#}");
                self.apply(Profile { widgets: Vec::new() }, HashMap::new());
            }
        }
    }

    fn snapshot(&self) -> Snapshot {
        let modified = |path: &std::path::Path| fs::metadata(path).and_then(|m| m.modified()).ok();
        let mut modules: Vec<(PathBuf, SystemTime)> = fs::read_dir(&self.config.modules)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| is_module(path))
            .filter_map(|path| modified(&path).map(|at| (path, at)))
            .collect();
        modules.sort();
        Snapshot { profile: modified(&self.config.profile), modules }
    }

    fn load(&self) -> Result<(Profile, HashMap<String, Installed>)> {
        let mut packages: HashMap<String, Installed> = HashMap::new();
        let folder = &self.config.modules;
        let entries = fs::read_dir(folder).with_context(|| format!("reading the module folder {}", folder.display()))?;
        for entry in entries {
            let path = entry?.path();
            if !is_module(&path) {
                continue;
            }
            let opened = fs::metadata(&path).and_then(|m| m.modified()).map_err(anyhow::Error::from).and_then(|modified| {
                ModulePackage::open(&path).map(|package| Installed { package: Rc::new(package), modified })
            });
            match opened {
                Ok(installed) => {
                    let id = installed.package.manifest().id.clone();
                    if let Some(other) = packages.insert(id.clone(), installed) {
                        anyhow::bail!("{} and {} both have the id {id}", other.package.path().display(), path.display());
                    }
                }
                Err(e) => eprintln!("error: {e:#}"),
            }
        }
        let profile = Profile::read(&self.config.profile, self.area.size, |id| packages.get(id).map(|i| i.package.manifest().clone()))?;
        println!("Widgets: {} placed from {} installed modules", profile.widgets.len(), packages.len());
        Ok((profile, packages))
    }

    /// Keeps instances whose module didn't change, telling them about moves, resizes and settings.
    /// Others restart, carrying over what `unload` saved when it is the same module.
    fn apply(&mut self, profile: Profile, packages: HashMap<String, Installed>) {
        let mut old: Vec<Option<Instance>> = std::mem::take(&mut self.instances).into_iter().map(Some).collect();
        for (index, widget) in profile.widgets.iter().enumerate() {
            let installed = packages[&widget.module].clone();
            let previous = old.get_mut(index).and_then(Option::take);
            let instance = match previous {
                Some(mut kept) if kept.module == widget.module && kept.installed.same_version(&installed) => {
                    if kept.placement != widget.placement {
                        self.clear(kept.placement);
                        match (kept.placement.w, kept.placement.h) == (widget.placement.w, widget.placement.h) {
                            true => kept.move_to(widget.placement),
                            false => kept.resize(widget.placement),
                        }
                    }
                    if kept.settings != widget.settings {
                        kept.settings_changed(widget.settings.clone());
                    }
                    kept
                }
                Some(replaced) => {
                    self.clear(replaced.placement);
                    let same_module = replaced.module == widget.module;
                    let saved = replaced.unload();
                    Instance::start(index, widget, installed, if same_module { saved } else { None })
                }
                None => Instance::start(index, widget, installed, None),
            };
            self.instances.push(instance);
        }
        for removed in old.into_iter().flatten() {
            self.clear(removed.placement);
            drop(removed.unload());
        }
    }
}
