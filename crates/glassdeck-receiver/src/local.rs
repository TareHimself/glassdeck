//! Decks played on the keyboard itself, with no PC: video decks through our own decoder.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};

use anyhow::{Context, Result, anyhow, bail};
use glassdeck_deck::{Content, Deck, Entry};
use glassdeck_proto::{Mode, StreamInfo, unix_micros};

use crate::mailbox::{Mailbox, ShownFrame, Timing};
use crate::mp4::{ReadAt, VideoTrack};
use crate::platform::{self, Decoder};
use crate::playout::{PlayoutClock, wait_until};

/// Local files need no network buffer, only enough lead for the decoder's first pictures.
const LOCAL_PLAYOUT_DELAY_US: u64 = 100_000;

/// A deck playing on its own thread until stopped or failed.
pub struct LocalDeck {
    stop: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}

impl LocalDeck {
    /// `stopped` runs if the deck ends by itself, never after `stop`.
    pub fn start(
        path: &Path,
        mailbox: Arc<Mailbox>,
        stopped: impl FnOnce(anyhow::Error) + Send + 'static,
    ) -> Result<Self> {
        let deck = Deck::open(path)?;
        let entry = match &deck.manifest().content {
            Content::Video { file, .. } => deck.entry(file).context("the deck's video is missing")?,
            Content::Web { .. } => bail!("web decks are not wired to the receiver yet"),
        };
        let stop = Arc::new(AtomicBool::new(false));
        let asked = Arc::clone(&stop);
        let thread = thread::Builder::new().name("local deck".into()).spawn(move || {
            let error = match play_video(&deck, entry, &mailbox, &asked) {
                Ok(()) => anyhow!("the deck stopped without being asked"),
                Err(e) => e,
            };
            if !asked.load(Ordering::Relaxed) {
                stopped(error);
            }
        })?;
        Ok(Self { stop, thread })
    }

    pub fn stop(self) {
        self.stop.store(true, Ordering::Relaxed);
        if self.thread.join().is_err() {
            eprintln!("error: the local deck thread panicked");
        }
    }
}

struct DeckEntry<'a> {
    deck: &'a Deck,
    entry: Entry,
}

impl ReadAt for DeckEntry<'_> {
    fn len(&self) -> u64 {
        self.entry.len
    }

    fn read_at(&self, at: u64, buf: &mut [u8]) -> Result<()> {
        let mut done = 0;
        while done < buf.len() {
            let n = self.deck.read_range(self.entry, at + done as u64, &mut buf[done..])?;
            anyhow::ensure!(n > 0, "the deck's video ends early");
            done += n;
        }
        Ok(())
    }
}

/// Loops the video forever; presentation times keep rising across loops so the decoder and clock never rewind.
fn play_video(deck: &Deck, entry: Entry, mailbox: &Mailbox, stop: &AtomicBool) -> Result<()> {
    platform::init_decoder_thread()?;
    let video = DeckEntry { deck, entry };
    let track = VideoTrack::read(&video).context("reading the deck's video")?;
    let info =
        StreamInfo { stream_id: 0, codec: track.codec, mode: Mode::Paced, width: track.width, height: track.height };
    let mut decoder = Decoder::new(&info)?;
    println!(
        "Local deck {}: {:?} {}x{}, {} frames, {:.1} s via {}",
        deck.path().display(),
        track.codec,
        track.width,
        track.height,
        track.samples.len(),
        track.duration_us as f64 / 1e6,
        decoder.name
    );

    let mut clock = None;
    let mut data = Vec::new();
    for lap in 0u64.. {
        for &sample in &track.samples {
            if stop.load(Ordering::Relaxed) {
                return Ok(());
            }
            data.resize(sample.size as usize, 0);
            video.read_at(sample.offset, &mut data)?;
            let access_unit = track.access_unit(sample, &data)?;
            let pts_us = lap * track.duration_us + sample.pts_us;
            for picture in decoder.decode(&access_unit, pts_us)? {
                let clock =
                    *clock.get_or_insert(PlayoutClock::start(picture.time_us, unix_micros(), LOCAL_PLAYOUT_DELAY_US));
                let late_us = wait_until(clock.due_us(picture.time_us));
                mailbox.put(ShownFrame { picture, timing: Timing::Paced { late_us } });
            }
        }
    }
    unreachable!("laps never run out")
}
