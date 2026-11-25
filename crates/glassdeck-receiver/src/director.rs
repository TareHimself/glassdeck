//! Carries out the screen table: starts and stops local backgrounds and the built-in scene, and remembers the chosen background.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::{Context, Result, bail, ensure};

use crate::local::LocalBackground;
use crate::mailbox::Mailbox;
use crate::platform::{FallbackScene, Registrar};
use crate::screen::{Effect, Event, Screen, Selection, step};

/// Lives in the background folder; background names can't start with a dot, so it never collides with one.
const RECENT_FILE: &str = ".recent";

pub type SharedDirector = Arc<Mutex<Director>>;

#[derive(Clone, Copy)]
enum Runner {
    Background,
    Fallback,
}

struct Stopped {
    runner: Runner,
    generation: u64,
    error: anyhow::Error,
}

pub struct Director {
    screen: Screen,
    running: Option<LocalBackground>,
    /// The built-in scene. `None` while the screen shows it means its renderer died; that's logged,
    /// and the screen stays black until something else owns it.
    fallback: Option<FallbackScene>,
    /// Counts starts, so a report from a background or scene that was since replaced is ignored.
    generation: u64,
    backgrounds: PathBuf,
    registrar: Registrar,
    mailbox: Arc<Mailbox>,
    stopped: Sender<Stopped>,
}

impl Director {
    pub fn start(backgrounds: PathBuf, registrar: Registrar, mailbox: Arc<Mailbox>) -> Result<SharedDirector> {
        fs::create_dir_all(&backgrounds).with_context(|| format!("creating the background folder {}", backgrounds.display()))?;
        let remembered = read_recent(&backgrounds).unwrap_or_else(|e| {
            eprintln!("error: forgetting the remembered background: {e:#}");
            None
        });
        let (screen, effects) = Screen::at_boot(remembered);
        let (stopped, reports) = mpsc::channel::<Stopped>();
        let director = Arc::new(Mutex::new(Self {
            screen,
            running: None,
            fallback: None,
            generation: 0,
            backgrounds,
            registrar,
            mailbox,
            stopped,
        }));
        lock(&director).apply(effects)?;

        let watched = Arc::clone(&director);
        thread::Builder::new().name("background watch".into()).spawn(move || {
            for report in reports {
                if let Err(e) = lock(&watched).stopped_by_itself(report) {
                    eprintln!("error: {e:#}");
                }
            }
        })?;
        Ok(director)
    }

    pub fn handle(&mut self, event: Event) -> Result<()> {
        let (next, effects) = step(self.screen.clone(), event)?;
        self.screen = next;
        self.apply(effects)
    }

    /// A background chosen from the host: the built-in scene, or a file already in the background folder.
    pub fn select(&mut self, name: &str) -> Result<()> {
        let selection = Selection::parse(name)?;
        let Selection::File(background) = selection else {
            println!("Background selected: the built-in scene");
            return self.handle(Event::BackgroundSelected(Selection::BuiltIn));
        };
        let path = self.backgrounds.join(background.as_str());
        ensure!(path.is_file(), "{} is not in the background folder {}", background, self.backgrounds.display());
        println!("Background selected: {background}");
        self.handle(Event::BackgroundSelected(Selection::File(background)))
    }

    /// A large file started or finished arriving.
    pub fn install(&mut self, started: bool) -> Result<()> {
        self.handle(if started { Event::InstallStarted } else { Event::InstallEnded })
    }

    fn stopped_by_itself(&mut self, report: Stopped) -> Result<()> {
        if report.generation != self.generation {
            return Ok(());
        }
        match report.runner {
            Runner::Background => {
                let Some(background) = self.running.take() else { return Ok(()) };
                eprintln!("error: the local background stopped: {:#}", report.error);
                background.stop();
                self.handle(Event::BackgroundStopped)
            }
            Runner::Fallback => {
                if let Some(scene) = self.fallback.take() {
                    eprintln!("error: the built-in scene stopped: {:#}", report.error);
                    scene.stop();
                }
                Ok(())
            }
        }
    }

    /// What a background or scene runs if it stops by itself.
    fn report_for(&mut self, runner: Runner) -> impl FnOnce(anyhow::Error) + Send + 'static {
        self.generation += 1;
        let generation = self.generation;
        let stopped = self.stopped.clone();
        // Only fails once the watch thread is gone, at shutdown, when nobody needs the report.
        move |error| drop(stopped.send(Stopped { runner, generation, error }))
    }

    fn apply(&mut self, effects: Vec<Effect>) -> Result<()> {
        let mut failed_start = None;
        for effect in effects {
            match effect {
                Effect::Stop => match self.running.take() {
                    Some(background) => background.stop(),
                    None => bail!("asked to stop a background while none was running"),
                },
                Effect::Remember(selection) => write_recent(&self.backgrounds, &selection)?,
                Effect::Start(background) => {
                    let report = self.report_for(Runner::Background);
                    match LocalBackground::start(
                        &self.backgrounds.join(background.as_str()),
                        self.registrar.clone(),
                        Arc::clone(&self.mailbox),
                        report,
                    ) {
                        Ok(running) => self.running = Some(running),
                        Err(e) => failed_start = Some(e.context(format!("starting {background}"))),
                    }
                }
                Effect::ShowFallback => {
                    ensure!(self.fallback.is_none(), "asked to show the built-in scene while it was showing");
                    let report = self.report_for(Runner::Fallback);
                    match FallbackScene::start(self.registrar.clone(), Arc::clone(&self.mailbox), report) {
                        Ok(scene) => self.fallback = Some(scene),
                        Err(e) => eprintln!("error: showing the built-in scene: {e:#}"),
                    }
                }
                Effect::HideFallback => {
                    if let Some(scene) = self.fallback.take() {
                        scene.stop();
                    }
                }
            }
        }
        match failed_start {
            Some(e) => {
                eprintln!("error: {e:#}");
                self.handle(Event::BackgroundStopped)
            }
            None => Ok(()),
        }
    }
}

pub fn lock(director: &SharedDirector) -> std::sync::MutexGuard<'_, Director> {
    director.lock().expect("director poisoned")
}

fn read_recent(backgrounds: &Path) -> Result<Option<Selection>> {
    match fs::read_to_string(backgrounds.join(RECENT_FILE)) {
        Ok(text) => Selection::parse(text.trim()).map(Some),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).context("reading the remembered background"),
    }
}

fn write_recent(backgrounds: &Path, selection: &Selection) -> Result<()> {
    let staging = backgrounds.join(format!("{RECENT_FILE}.tmp"));
    fs::write(&staging, selection.as_str()).context("remembering the background")?;
    fs::rename(&staging, backgrounds.join(RECENT_FILE)).context("remembering the background")
}
