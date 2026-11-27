//! The widget area's modules: a thread that runs each placed module's `keyboard.luau` and hands the
//! compositor tiles to put on the key layer. Profile and module changes are picked up while running.


use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result};
use fontdue::Font;
use glassdeck_luau::placement::{Placement, PlacedWidget, WidgetArea, check_widgets};
use glassdeck_package::{Package, QualifiedId, is_package};
use serde::{Deserialize, Serialize};

use glassdeck_luau::LiveName;
use glassdeck_widget::{Installed, Installing, Instance, LiveCache};

use glassdeck_proto::ScriptTarget;

use crate::platform::Doorbell;
use crate::upstream::Upstream;

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
    doorbell: Arc<Doorbell>,
}

impl Tiles {
    pub fn new(doorbell: Arc<Doorbell>) -> Self {
        Self { waiting: Mutex::new(Vec::new()), doorbell }
    }

    fn put(&self, tile: Tile) {
        let mut waiting = self.waiting.lock().expect("tiles poisoned");
        waiting.retain(|t| (t.x, t.y, t.w, t.h) != (tile.x, tile.y, tile.w, tile.h));
        waiting.push(tile);
        drop(waiting);
        self.doorbell.ring();
    }

    pub fn take(&self) -> Vec<Tile> {
        std::mem::take(&mut *self.waiting.lock().expect("tiles poisoned"))
    }
}

/// The keyboard's own record of the widget layout, in its state folder
const LAYOUT_FILE: &str = "widgets.json";

pub enum Input {
    Data {
        instance: u16,
        payload: Vec<u8>,
    },
    /// A message from the instance's host half, for its `bridge.on`.
    Message {
        instance: u16,
        message: u16,
        payload: Vec<u8>,
    },
    /// A resource the instance's host half made; empty `data` removes it.
    Resource {
        instance: u16,
        name: String,
        data: Vec<u8>,
    },
    /// A large file is arriving: the widget area shows its progress instead of the widgets.
    InstallProgress {
        what: String,
        fraction: f32,
    },
    InstallEnded,
    /// The host's new layout: run these, in order, and remember them.
    SetWidgets(Vec<PlacedWidget>),
}

pub struct Config {
    pub packages: PathBuf,
    /// Where the keyboard keeps its own state: the widget layout, here
    pub state: PathBuf,
    pub font: Font,
}

pub fn start(config: Config, tiles: Arc<Tiles>, upstream: Arc<Upstream>) -> Result<Sender<Input>> {
    let (inbox, inputs) = mpsc::channel();
    thread::Builder::new().name("widgets".into()).spawn(move || Host::new(config, tiles, upstream).run(inputs))?;
    Ok(inbox)
}

struct Host {
    config: Config,
    area: WidgetArea,
    tiles: Arc<Tiles>,
    upstream: Arc<Upstream>,
    instances: Vec<Instance>,
    /// What the host last asked for, kept across restarts
    layout: Vec<PlacedWidget>,
    /// The widget packages the last scan saw, to notice installs.
    seen: Option<Vec<(PathBuf, SystemTime)>>,
    last_frame: Option<Instant>,
    /// When the widgets last drew, so a burst of inputs draws once per frame
    last_publish: Option<Instant>,
    /// Inputs or timers arrived since the last draw
    draw_due: bool,
    /// While set, widgets keep running but their redraws wait until the install ends.
    installing: Option<Installing>,
    /// Each instance's live resources, by its number in the layout.
    live: HashMap<u16, LiveCache>,
}

impl Host {
    fn new(config: Config, tiles: Arc<Tiles>, upstream: Arc<Upstream>) -> Self {
        let layout = read_layout(&config.state).unwrap_or_else(|e| {
            eprintln!("error: forgetting the saved widget layout: {e:#}");
            Vec::new()
        });
        Self {
            config,
            area: WidgetArea::of_panel(),
            tiles,
            upstream,
            instances: Vec::new(),
            layout,
            seen: None,
            last_frame: None,
            last_publish: None,
            draw_due: true,
            installing: None,
            live: HashMap::new(),
        }
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
                Ok(input) => self.handle(input),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
            loop {
                match inputs.try_recv() {
                    Ok(input) => self.handle(input),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => return,
                }
            }
            self.draw_due = true;
            let now = Instant::now();
            if self.last_publish.is_none_or(|at| now >= at + FRAME) {
                self.tick(now);
                self.publish();
                self.last_publish = Some(now);
                self.draw_due = false;
            }
            self.send_messages();
        }
    }

    /// What each widget's script sent its host half.
    fn send_messages(&mut self) {
        for (number, instance) in self.instances.iter_mut().enumerate() {
            let number = u16::try_from(number).expect("layouts hold at most u16::MAX widgets");
            for sent in instance.take_messages() {
                self.upstream.script_message(ScriptTarget::Widget(number), sent.message, sent.payload);
            }
        }
    }

    fn handle(&mut self, input: Input) {
        match input {
            Input::Data { instance, payload } => match self.instances.get_mut(usize::from(instance)) {
                Some(target) => target.deliver_options(&payload),
                None => eprintln!("error: data for widget {instance}, but the profile has {}", self.instances.len()),
            },
            Input::Message { instance, message, payload } => match self.instances.get_mut(usize::from(instance)) {
                Some(target) => target.deliver_message(message, &payload),
                None => eprintln!("error: a message for widget {instance}, but the profile has {}", self.instances.len()),
            },
            Input::Resource { instance, name, data } => self.store_live(instance, &name, data),
            Input::InstallProgress { what, fraction } => match &mut self.installing {
                Some(installing) if installing.what == what => installing.fraction = fraction,
                _ => self.installing = Some(Installing::new(what, fraction)),
            },
            Input::InstallEnded => self.end_install(),
            Input::SetWidgets(layout) => self.set_layout(layout),
        }
    }

    fn next_wake(&self) -> Option<Instant> {
        let timers = self.instances.iter().filter_map(Instance::next_timer).min();
        let frame = self.instances.iter().any(Instance::wants_frame).then(|| self.last_frame.map_or_else(Instant::now, |at| at + FRAME));
        let held_draw = self.draw_due.then(|| self.last_publish.map_or_else(Instant::now, |at| at + FRAME));
        timers.into_iter().chain(frame).chain(held_draw).min()
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
        let none = LiveCache::default();
        for (number, instance) in self.instances.iter_mut().enumerate() {
            let live = u16::try_from(number).ok().and_then(|n| self.live.get(&n)).unwrap_or(&none);
            if let Some(tile) = instance.draw(&self.config.font, live) {
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

    /// Kept by instance number, not in the instance, so a resource that arrives before its widget
    /// starts, or across a restart, is still there to draw.
    fn store_live(&mut self, instance: u16, name: &str, data: Vec<u8>) {
        let stored = LiveName::parse(name).and_then(|name| {
            let dropped = self.live.entry(instance).or_default().put(name.clone(), data)?;
            Ok(dropped.into_iter().chain([name]).collect::<Vec<_>>())
        });
        match stored {
            Ok(changed) => {
                if let Some(target) = self.instances.get_mut(usize::from(instance)) {
                    target.live_changed(&changed);
                }
            }
            Err(e) => eprintln!("error: widget {instance}: live resource: {e:#}"),
        }
    }

    fn clear(&self, placement: Placement) {
        let Placement { x, y, w, h } = placement;
        self.tiles.put(Tile { x: self.area.x + x, y: self.area.y + y, w, h, pixels: vec![0; (w * h) as usize] });
    }

    /// Saves a new layout from the host and runs it; an unchanged one isn't written again.
    fn set_layout(&mut self, layout: Vec<PlacedWidget>) {
        if layout == self.layout {
            return;
        }
        if let Err(e) = write_layout(&self.config.state, &layout) {
            eprintln!("error: saving the widget layout: {e:#}");
        }
        self.layout = layout;
        self.seen = None;
        self.rescan();
    }

    fn rescan(&mut self) {
        let snapshot = self.snapshot();
        if self.seen.as_ref() == Some(&snapshot) {
            return;
        }
        self.seen = Some(snapshot);
        match self.load() {
            Ok(packages) => self.apply(self.layout.clone(), packages),
            Err(e) => {
                eprintln!("error: widgets: {e:#}");
                self.apply(Vec::new(), HashMap::new());
            }
        }
    }

    fn snapshot(&self) -> Vec<(PathBuf, SystemTime)> {
        let modified = |path: &Path| fs::metadata(path).and_then(|m| m.modified()).ok();
        let mut packages: Vec<(PathBuf, SystemTime)> = fs::read_dir(&self.config.packages)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| is_package(path))
            .filter_map(|path| modified(&path).map(|at| (path, at)))
            .collect();
        packages.sort();
        packages
    }

    /// The installed widget packages, once the layout checks out against them.
    fn load(&self) -> Result<HashMap<QualifiedId, Installed>> {
        let mut packages: HashMap<QualifiedId, Installed> = HashMap::new();
        let folder = &self.config.packages;
        let entries = fs::read_dir(folder).with_context(|| format!("reading the packages folder {}", folder.display()))?;
        for entry in entries {
            let path = entry?.path();
            if !is_package(&path) {
                continue;
            }
            let opened = fs::metadata(&path)
                .and_then(|m| m.modified())
                .map_err(anyhow::Error::from)
                .and_then(|modified| Package::open(&path).map(|package| Installed { package: Rc::new(package), modified }));
            match opened {
                Ok(installed) if installed.package.manifest().widget().is_some() => {
                    packages.insert(installed.package.id().clone(), installed);
                }
                Ok(_) => {}
                Err(e) => eprintln!("error: {e:#}"),
            }
        }
        check_widgets(&self.layout, self.area.size, |id| packages.get(id).and_then(|i| i.package.manifest().widget().copied()))?;
        println!("Widgets: {} placed from {} installed widget packages", self.layout.len(), packages.len());
        Ok(packages)
    }

    /// Keeps instances whose module didn't change, telling them about moves and resizes; their options
    /// reach them from the host half as data. Others restart, carrying over what `unload` saved when it is the same module.
    fn apply(&mut self, layout: Vec<PlacedWidget>, packages: HashMap<QualifiedId, Installed>) {
        let mut old: Vec<Option<Instance>> = std::mem::take(&mut self.instances).into_iter().map(Some).collect();
        for (index, widget) in layout.iter().enumerate() {
            let installed = packages[&widget.package].clone();
            let previous = old.get_mut(index).and_then(Option::take);
            let instance = match previous {
                Some(mut kept) if kept.package == widget.package.as_str() && kept.installed.same_version(&installed) => {
                    if kept.placement != widget.placement {
                        self.clear(kept.placement);
                        match (kept.placement.w, kept.placement.h) == (widget.placement.w, widget.placement.h) {
                            true => kept.move_to(widget.placement),
                            false => kept.resize(widget.placement),
                        }
                    }
                    kept
                }
                Some(replaced) => {
                    self.clear(replaced.placement);
                    let same_module = replaced.package == widget.package.as_str();
                    let saved = replaced.unload();
                    Instance::start(index, widget, installed, if same_module { saved } else { None }, None)
                }
                None => Instance::start(index, widget, installed, None, None),
            };
            self.instances.push(instance);
        }
        for removed in old.into_iter().flatten() {
            self.clear(removed.placement);
            drop(removed.unload());
        }
        let count = self.instances.len();
        self.live.retain(|number, _| usize::from(*number) < count);
    }
}

#[derive(Serialize, Deserialize)]
struct SavedWidget {
    package: String,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
}

fn read_layout(state: &Path) -> Result<Vec<PlacedWidget>> {
    let path = state.join(LAYOUT_FILE);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let saved: Vec<SavedWidget> = serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    saved
        .into_iter()
        .map(|w| Ok(PlacedWidget { package: QualifiedId::parse(&w.package)?, placement: Placement { x: w.x, y: w.y, w: w.w, h: w.h } }))
        .collect()
}

fn write_layout(state: &Path, layout: &[PlacedWidget]) -> Result<()> {
    fs::create_dir_all(state).with_context(|| format!("creating {}", state.display()))?;
    let saved: Vec<SavedWidget> = layout
        .iter()
        .map(|w| SavedWidget { package: w.package.to_string(), x: w.placement.x, y: w.placement.y, w: w.placement.w, h: w.placement.h })
        .collect();
    let path = state.join(LAYOUT_FILE);
    let staging = state.join(format!("{LAYOUT_FILE}.tmp"));
    fs::write(&staging, serde_json::to_vec(&saved)?).with_context(|| format!("writing {}", staging.display()))?;
    fs::rename(&staging, &path).with_context(|| format!("moving {} into place", path.display()))
}
