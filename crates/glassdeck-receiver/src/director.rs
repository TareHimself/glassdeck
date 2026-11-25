//! Carries out the screen table: starts and stops local backgrounds, and remembers the chosen one.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::{Context, Result, bail, ensure};

use crate::local::LocalBackground;
use crate::mailbox::Mailbox;
use crate::platform::Registrar;
use crate::screen::{BackgroundName, Effect, Event, Screen, step};

/// Lives in the background folder; background names can't start with a dot, so it never collides with one.
const RECENT_FILE: &str = ".recent";

pub type SharedDirector = Arc<Mutex<Director>>;

struct BackgroundStopped {
    generation: u64,
    error: anyhow::Error,
}

pub struct Director {
    screen: Screen,
    running: Option<LocalBackground>,
    /// Counts background starts, so a report from a background that was since replaced is ignored.
    generation: u64,
    backgrounds: PathBuf,
    registrar: Registrar,
    mailbox: Arc<Mailbox>,
    stopped: Sender<BackgroundStopped>,
}

impl Director {
    pub fn start(backgrounds: PathBuf, registrar: Registrar, mailbox: Arc<Mailbox>) -> Result<SharedDirector> {
        fs::create_dir_all(&backgrounds).with_context(|| format!("creating the background folder {}", backgrounds.display()))?;
        let remembered = read_recent(&backgrounds).unwrap_or_else(|e| {
            eprintln!("error: forgetting the remembered background: {e:#}");
            None
        });
        let (screen, effects) = Screen::at_boot(remembered);
        let (stopped, reports) = mpsc::channel::<BackgroundStopped>();
        let director =
            Arc::new(Mutex::new(Self { screen, running: None, generation: 0, backgrounds, registrar, mailbox, stopped }));
        lock(&director).apply(effects)?;

        let watched = Arc::clone(&director);
        thread::Builder::new().name("background watch".into()).spawn(move || {
            for report in reports {
                if let Err(e) = lock(&watched).background_stopped(report) {
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

    /// A background chosen from the PC; it must already be in the background folder.
    pub fn select(&mut self, name: &str) -> Result<()> {
        let background = BackgroundName::parse(name)?;
        let path = self.backgrounds.join(background.as_str());
        ensure!(path.is_file(), "{} is not in the background folder {}", background, self.backgrounds.display());
        println!("Background selected: {background}");
        self.handle(Event::BackgroundSelected(background))
    }

    fn background_stopped(&mut self, report: BackgroundStopped) -> Result<()> {
        if report.generation != self.generation || self.running.is_none() {
            return Ok(());
        }
        eprintln!("error: the local background stopped: {:#}", report.error);
        if let Some(background) = self.running.take() {
            background.stop();
        }
        self.handle(Event::BackgroundStopped)
    }

    fn apply(&mut self, effects: Vec<Effect>) -> Result<()> {
        let mut failed_start = None;
        for effect in effects {
            match effect {
                Effect::Stop => match self.running.take() {
                    Some(background) => background.stop(),
                    None => bail!("asked to stop a background while none was running"),
                },
                Effect::Remember(background) => write_recent(&self.backgrounds, &background)?,
                Effect::Start(background) => {
                    self.generation += 1;
                    let generation = self.generation;
                    let stopped = self.stopped.clone();
                    let report = move |error| {
                        // Only fails once the watch thread is gone, at shutdown, when nobody needs the report.
                        let _ = stopped.send(BackgroundStopped { generation, error });
                    };
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

fn read_recent(backgrounds: &Path) -> Result<Option<BackgroundName>> {
    match fs::read_to_string(backgrounds.join(RECENT_FILE)) {
        Ok(text) => BackgroundName::parse(text.trim()).map(Some),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).context("reading the remembered background"),
    }
}

fn write_recent(backgrounds: &Path, background: &BackgroundName) -> Result<()> {
    let staging = backgrounds.join(format!("{RECENT_FILE}.tmp"));
    fs::write(&staging, background.as_str()).context("remembering the background")?;
    fs::rename(&staging, backgrounds.join(RECENT_FILE)).context("remembering the background")
}
