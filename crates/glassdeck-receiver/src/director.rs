//! Carries out the screen table: starts and stops local decks, and remembers the chosen one.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::{Context, Result, bail, ensure};

use crate::local::LocalDeck;
use crate::mailbox::Mailbox;
use crate::platform::Registrar;
use crate::screen::{DeckName, Effect, Event, Screen, step};

/// Lives in the deck folder; deck names can't start with a dot, so it never collides with one.
const RECENT_FILE: &str = ".recent";

pub type SharedDirector = Arc<Mutex<Director>>;

struct DeckStopped {
    generation: u64,
    error: anyhow::Error,
}

pub struct Director {
    screen: Screen,
    running: Option<LocalDeck>,
    /// Counts deck starts, so a report from a deck that was since replaced is ignored.
    generation: u64,
    decks: PathBuf,
    registrar: Registrar,
    mailbox: Arc<Mailbox>,
    stopped: Sender<DeckStopped>,
}

impl Director {
    pub fn start(decks: PathBuf, registrar: Registrar, mailbox: Arc<Mailbox>) -> Result<SharedDirector> {
        fs::create_dir_all(&decks).with_context(|| format!("creating the deck folder {}", decks.display()))?;
        let remembered = read_recent(&decks).unwrap_or_else(|e| {
            eprintln!("error: forgetting the remembered deck: {e:#}");
            None
        });
        let (screen, effects) = Screen::at_boot(remembered);
        let (stopped, reports) = mpsc::channel::<DeckStopped>();
        let director =
            Arc::new(Mutex::new(Self { screen, running: None, generation: 0, decks, registrar, mailbox, stopped }));
        lock(&director).apply(effects)?;

        let watched = Arc::clone(&director);
        thread::Builder::new().name("deck watch".into()).spawn(move || {
            for report in reports {
                if let Err(e) = lock(&watched).deck_stopped(report) {
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

    /// A deck chosen from the PC; it must already be in the deck folder.
    pub fn select(&mut self, name: &str) -> Result<()> {
        let deck = DeckName::parse(name)?;
        let path = self.decks.join(deck.as_str());
        ensure!(path.is_file(), "{} is not in the deck folder {}", deck, self.decks.display());
        println!("Deck selected: {deck}");
        self.handle(Event::DeckSelected(deck))
    }

    fn deck_stopped(&mut self, report: DeckStopped) -> Result<()> {
        if report.generation != self.generation || self.running.is_none() {
            return Ok(());
        }
        eprintln!("error: the local deck stopped: {:#}", report.error);
        if let Some(deck) = self.running.take() {
            deck.stop();
        }
        self.handle(Event::DeckStopped)
    }

    fn apply(&mut self, effects: Vec<Effect>) -> Result<()> {
        let mut failed_start = None;
        for effect in effects {
            match effect {
                Effect::Stop => match self.running.take() {
                    Some(deck) => deck.stop(),
                    None => bail!("asked to stop a deck while none was running"),
                },
                Effect::Remember(deck) => write_recent(&self.decks, &deck)?,
                Effect::Start(deck) => {
                    self.generation += 1;
                    let generation = self.generation;
                    let stopped = self.stopped.clone();
                    let report = move |error| {
                        // Only fails once the watch thread is gone, at shutdown, when nobody needs the report.
                        let _ = stopped.send(DeckStopped { generation, error });
                    };
                    match LocalDeck::start(
                        &self.decks.join(deck.as_str()),
                        self.registrar.clone(),
                        Arc::clone(&self.mailbox),
                        report,
                    ) {
                        Ok(running) => self.running = Some(running),
                        Err(e) => failed_start = Some(e.context(format!("starting {deck}"))),
                    }
                }
            }
        }
        match failed_start {
            Some(e) => {
                eprintln!("error: {e:#}");
                self.handle(Event::DeckStopped)
            }
            None => Ok(()),
        }
    }
}

pub fn lock(director: &SharedDirector) -> std::sync::MutexGuard<'_, Director> {
    director.lock().expect("director poisoned")
}

fn read_recent(decks: &Path) -> Result<Option<DeckName>> {
    match fs::read_to_string(decks.join(RECENT_FILE)) {
        Ok(text) => DeckName::parse(text.trim()).map(Some),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).context("reading the remembered deck"),
    }
}

fn write_recent(decks: &Path, deck: &DeckName) -> Result<()> {
    let staging = decks.join(format!("{RECENT_FILE}.tmp"));
    fs::write(&staging, deck.as_str()).context("remembering the deck")?;
    fs::rename(&staging, decks.join(RECENT_FILE)).context("remembering the deck")
}
