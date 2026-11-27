//! The Overview's widget layer: every placed widget running at once, each in its own VM with its host
//! half, as the keyboard and glassdeckd run them. Each widget's tile is sent on its own, tagged with its
//! place in the layout, and the frontend positions it; moving a widget never redraws anything.

use std::path::PathBuf;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Mutex;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use glassdeck_luau::placement::Placement;
use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::AppHandle;
use tauri_specta::Event as _;

use crate::library::Dims;
use crate::preview::{self, Drawn, PreviewChange, PreviewRun, Running, Session};

const IDLE: Duration = Duration::from_secs(1);

/// One placed widget as the Overview's draft has it.
#[derive(Clone, Debug, PartialEq, Deserialize, Type)]
pub struct PlacedSource {
    /// Its qualified id
    pub package: String,
    pub size: Dims,
    pub values: serde_json::Map<String, serde_json::Value>,
}

/// Something the layout preview did, in the run (the `preview_layout` call) it belongs to.
#[derive(Clone, Debug, Serialize, Type, tauri_specta::Event)]
pub struct LayoutUpdate {
    pub run: PreviewRun,
    pub change: LayoutChange,
}

#[derive(Clone, Debug, Serialize, Type)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum LayoutChange {
    Tile { index: u32, width: u32, height: u32, png: String },
    /// The widget isn't running: it isn't in the library, or it stopped on an error
    Stopped { index: u32, reason: String },
}

pub enum Command {
    /// The layout, with each widget's library folder when it has one
    Show(PreviewRun, Vec<(Option<PathBuf>, PlacedSource)>),
    Stop,
}

pub struct LayoutPreview(Mutex<Sender<Command>>);

impl LayoutPreview {
    pub fn start(app: AppHandle) -> Self {
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("layout preview".into())
            .spawn(move || run(app, rx))
            .expect("starting the layout preview thread");
        Self(Mutex::new(tx))
    }

    pub fn send(&self, command: Command) {
        let sent = self.0.lock().expect("the layout preview sender isn't poisoned").send(command);
        if sent.is_err() {
            eprintln!("the layout preview thread has stopped");
        }
    }
}

/// A place in the layout and what runs there.
struct Slot {
    folder: Option<PathBuf>,
    source: PlacedSource,
    running: Option<Running>,
}

fn run(app: AppHandle, rx: Receiver<Command>) {
    let current = Cell::new(PreviewRun(0));
    let emit = |change: LayoutChange| {
        if let Err(e) = (LayoutUpdate { run: current.get(), change }).emit(&app) {
            eprintln!("emitting a layout update: {e}");
        }
    };
    let cx = match Session::new(Rc::new(|update| {
        if let PreviewChange::Log { source, text, .. } = update {
            eprintln!("overview widget, {source:?}: {text}");
        }
    })) {
        Ok(cx) => cx,
        Err(e) => return eprintln!("layout preview: {e:#}"),
    };
    let mut slots: Vec<Slot> = Vec::new();
    loop {
        let now = Instant::now();
        let wait = slots.iter().filter_map(|s| s.running.as_ref()).map(|r| preview::wait_for(r, now)).min().unwrap_or(IDLE);
        match rx.recv_timeout(wait) {
            Ok(Command::Show(run, layout)) => {
                current.set(run);
                slots = arrange(&cx, &emit, slots, layout);
            }
            Ok(Command::Stop) => slots.clear(),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        for (index, slot) in slots.iter_mut().enumerate() {
            let Some(running) = &mut slot.running else { continue };
            match preview::step(&cx, running) {
                Drawn::Nothing => {}
                Drawn::Frame { width, height, png } => emit(LayoutChange::Tile { index: index as u32, width, height, png }),
                Drawn::Stopped => {
                    let reason = running.reported.clone().unwrap_or_default();
                    emit(LayoutChange::Stopped { index: index as u32, reason });
                    slot.running = None;
                }
            }
        }
    }
}

/// Keeps what's already running where the same widget stays in the same place, resizing it or giving it
/// new options as needed, and starts the rest.
fn arrange(cx: &Session, emit: &dyn Fn(LayoutChange), mut old: Vec<Slot>, layout: Vec<(Option<PathBuf>, PlacedSource)>) -> Vec<Slot> {
    old.truncate(layout.len());
    let mut kept: Vec<Option<Slot>> = old.into_iter().map(Some).collect();
    kept.resize_with(layout.len(), || None);
    layout
        .into_iter()
        .zip(kept)
        .enumerate()
        .map(|(index, ((folder, source), previous))| match previous {
            Some(mut slot) if slot.folder == folder && slot.source.package == source.package && slot.running.is_some() => {
                let running = slot.running.as_mut().expect("checked above");
                if slot.source.size != source.size {
                    running.instance.resize(Placement { x: 0, y: 0, w: source.size.w, h: source.size.h });
                }
                if slot.source.values != source.values {
                    preview::apply_options(cx, running, &source.values);
                }
                Slot { folder, source, running: slot.running }
            }
            previous => {
                drop(previous);
                let running = start(cx, emit, index, folder.as_ref(), &source);
                Slot { folder, source, running }
            }
        })
        .collect()
}

fn start(cx: &Session, emit: &dyn Fn(LayoutChange), index: usize, folder: Option<&PathBuf>, source: &PlacedSource) -> Option<Running> {
    let stopped = |reason: String| emit(LayoutChange::Stopped { index: index as u32, reason });
    let Some(folder) = folder else {
        stopped(format!("{} isn't in the library", source.package));
        return None;
    };
    match preview::show(cx, &format!("slot-{index}"), folder, &[], source.size, &source.values) {
        Ok(running) => Some(running),
        Err(e) => {
            stopped(format!("{e:#}"));
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::fs;

    use super::*;
    use crate::library::{NewWidget, Sizing, create_widget};

    fn placed(package: &str, w: u32) -> PlacedSource {
        PlacedSource { package: package.into(), size: Dims { w, h: 160 }, values: serde_json::Map::new() }
    }

    #[test]
    fn each_widget_draws_in_its_own_slot_and_a_resize_keeps_it_running() {
        let root = std::env::temp_dir().join(format!("glassdeck-layout-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        for (title, id) in [("Sky", "local.user.sky"), ("Sea", "local.user.sea")] {
            create_widget(&root, NewWidget { title: title.into(), id: id.into(), sizing: Sizing::Any { size: Dims { w: 320, h: 160 } } }).unwrap();
        }
        let seen = Rc::new(RefCell::new(Vec::new()));
        let sink = Rc::clone(&seen);
        let emit = move |update: LayoutChange| sink.borrow_mut().push(update);
        let cx = Session::new(Rc::new(|_| {})).unwrap();
        let folder = |id: &str| Some(root.join(id));
        let layout = vec![
            (folder("widget.local.user.sky"), placed("widget.local.user.sky", 320)),
            (folder("widget.local.user.sea"), placed("widget.local.user.sea", 320)),
            (None, placed("widget.local.user.gone", 320)),
        ];
        let mut slots = arrange(&cx, &emit, Vec::new(), layout);
        assert!(slots[0].running.is_some() && slots[1].running.is_some() && slots[2].running.is_none());
        assert!(matches!(&seen.borrow()[0], LayoutChange::Stopped { index: 2, .. }), "a widget not in the library says so");
        for slot in &mut slots {
            if let Some(running) = &mut slot.running {
                assert!(matches!(preview::step(&cx, running), Drawn::Frame { width: 320, .. }));
            }
        }
        let resized = vec![(folder("widget.local.user.sky"), placed("widget.local.user.sky", 480))];
        let mut slots = arrange(&cx, &emit, slots, resized);
        assert_eq!(slots.len(), 1);
        let running = slots[0].running.as_mut().unwrap();
        assert!(matches!(preview::step(&cx, running), Drawn::Frame { width: 480, .. }), "resized in place");
        drop(slots);
        fs::remove_dir_all(&root).unwrap();
    }
}
