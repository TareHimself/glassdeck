//! The module host: runs the host side of the active theme's background and of each of its widgets,
//! and hands what each sends to the Link. A host side is a package's options (from
//! `options.luau`) and its `host.luau`, which turns them into data for the keyboard; a package with
//! options but no `host.luau` gets an implicit one that forwards them as they are. Picks up new themes
//! and package installs while running, and restarts only what changed.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use anyhow::Result;
use glassdeck_luau::messages::Sent;
use glassdeck_luau::options::read_values;
use glassdeck_luau::ResourceReader;
use glassdeck_package::{Package, QualifiedId, is_package};
use glassdeck_proto::ScriptTarget;
use glassdeck_proto::compositor::MAX_DATA_LEN;

use glassdeck_host::{HostSide, SharedNowPlaying, SharedSystem, Sources};
use crate::link::Uplink;
use crate::theme::{self, ActiveTheme, Chosen};

/// How often the profile and module folder are checked for changes.
const RESCAN: Duration = Duration::from_secs(1);

/// What the API tells the module host.
pub enum HostCommand {
    /// A new active theme
    Theme(ActiveTheme),
    /// What a keyboard script sent its host half
    Message(ScriptTarget, Sent),
}

pub fn start(packages: PathBuf, theme: ActiveTheme, uplink: Arc<Uplink>, now_playing: SharedNowPlaying, inbox: Receiver<HostCommand>) -> Result<()> {
    thread::Builder::new()
        .name("module host".into())
        .spawn(move || Host::new(packages, theme, uplink, now_playing).run(&inbox))?;
    Ok(())
}

struct Host {
    packages: PathBuf,
    theme: ActiveTheme,
    uplink: Arc<Uplink>,
    system: SharedSystem,
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
    package: QualifiedId,
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


impl Host {
    fn new(packages: PathBuf, theme: ActiveTheme, uplink: Arc<Uplink>, now_playing: SharedNowPlaying) -> Self {
        let system = glassdeck_host::system();
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
            self.widgets.retain(|_, widget| keeps_running(&widget.side, now));
            if self.background.as_ref().is_some_and(|b| !keeps_running(&b.side, now)) {
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
                Ok(HostCommand::Message(from, sent)) => self.deliver(from, &sent),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    }

    /// A message from a keyboard script, to its host half; one that fails is reported, and the side keeps running.
    fn deliver(&self, from: ScriptTarget, sent: &Sent) {
        let running = match from {
            ScriptTarget::Background => self.background.as_ref(),
            ScriptTarget::Widget(number) => self.widgets.get(&number),
        };
        let Some(Running { side, .. }) = running else { return };
        if let Err(e) = side.deliver_message(sent.message, &sent.payload) {
            eprintln!("error: {}: {e:#}", side.label);
        }
    }

    fn flush(&self) {
        for (&number, widget) in &self.widgets {
            for sent in widget.side.take_messages() {
                self.uplink.put_script_message(ScriptTarget::Widget(number), sent);
            }
            if let Some(payload) = widget.side.take_sent() {
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
            if let Some(payload) = side.take_sent() {
                self.uplink.put_background_data(payload);
            }
            for sent in side.take_messages() {
                self.uplink.put_script_message(ScriptTarget::Background, sent);
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
            if matches!(change, Change::Start | Change::Stop) {
                self.uplink.forget_background_messages();
            }
            let old = self.background.take();
            self.background = self.changed(ScriptTarget::Background, old, change, background);
        }
        let running: BTreeMap<u16, SideEntry> = self.widgets.iter().map(|(&n, r)| (n, r.entry.clone())).collect();
        let entries: BTreeMap<u16, SideEntry> = wanted.iter().map(|(&n, (e, _))| (n, e.clone())).collect();
        for (number, change) in widget_changes(&running, &entries) {
            let old = self.widgets.remove(&number);
            if matches!(change, Change::Start | Change::Stop) {
                self.uplink.forget_module_instance(number);
            }
            if let Some(kept) = self.changed(ScriptTarget::Widget(number), old, change, wanted.remove(&number)) {
                self.widgets.insert(number, kept);
            }
        }
    }

    /// Carries out one change, and returns what then runs in that place.
    fn changed(&self, target: ScriptTarget, old: Option<Running>, change: Change, wanted: Option<(SideEntry, Rc<Package>)>) -> Option<Running> {
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

    fn start_side(&self, target: ScriptTarget, entry: SideEntry, package: &Rc<Package>) -> Option<Running> {
        let label = match target {
            ScriptTarget::Background => format!("background ({})", entry.package),
            ScriptTarget::Widget(number) => format!("widget {number} ({})", entry.package),
        };
        let started = (|| -> Result<Option<HostSide>> {
            let readable = Rc::clone(package);
            let (reader, send_limit): (Option<ResourceReader>, _) = match target {
                ScriptTarget::Background => (None, Some(MAX_DATA_LEN)),
                ScriptTarget::Widget(_) => (Some(Rc::new(move |resource| readable.resource(resource))), None),
            };
            let sources = Sources {
                label: label.clone(),
                options: package.options_script()?,
                host: package.host_script()?,
                reader,
                send_limit,
                print: None,
                messages: package.bridge_script()?,
            };
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

/// Runs a side's due timers; a failed one is reported and stops, and the others keep running.
fn keeps_running(side: &HostSide, now: Instant) -> bool {
    match side.run_timers(now) {
        Ok(()) => true,
        Err(e) => {
            eprintln!("error: {}: {e:#}; it stops, the others keep running", side.label);
            false
        }
    }
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
            package: QualifiedId::parse(package).unwrap(),
            modified: SystemTime::UNIX_EPOCH + Duration::from_secs(at),
            options: options.as_object().cloned().unwrap_or_default(),
        }
    }

    #[test]
    fn only_what_changed_restarts_or_hears_about_it() {
        let clock = entry("widget.local.user.clock", 1, serde_json::json!({ "seconds": true }));
        let running = BTreeMap::from([
            (0, clock.clone()),
            (1, entry("widget.local.user.cpu", 1, serde_json::json!({}))),
            (2, entry("widget.local.user.pomodoro", 1, serde_json::json!({}))),
        ]);
        let wanted = BTreeMap::from([
            (0, entry("widget.local.user.clock", 1, serde_json::json!({ "seconds": false, "hours24": true }))),
            (1, entry("widget.local.user.cpu", 2, serde_json::json!({}))),
            (3, entry("widget.local.user.now-playing", 1, serde_json::json!({}))),
        ]);
        assert_eq!(widget_changes(&running, &wanted), vec![
            (0, Change::Reapply(BTreeSet::from(["hours24".to_owned(), "seconds".to_owned()]))),
            (1, Change::Start),
            (2, Change::Stop),
            (3, Change::Start),
        ]);
        assert!(widget_changes(&running, &running).is_empty());
        let swapped = entry("widget.local.user.cpu", 1, serde_json::json!({ "seconds": true }));
        assert_eq!(side_change(Some(&clock), Some(&swapped)), Some(Change::Start));
        assert_eq!(side_change(Some(&clock), None), Some(Change::Stop));
        assert_eq!(side_change(None, None), None);
    }
}
