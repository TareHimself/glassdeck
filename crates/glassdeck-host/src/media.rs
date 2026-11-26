//! What the host is playing, from Windows' media controls (what the volume-key overlay shows). A
//! thread polls them, so `media.current()` in a script only reads the latest snapshot.

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSessionManager as SessionManager, GlobalSystemMediaTransportControlsSessionPlaybackStatus as Status,
};
use windows::Storage::Streams::DataReader;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};

const POLL: Duration = Duration::from_secs(1);

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Track {
    pub title: String,
    pub artist: String,
    pub album: String,
}

#[derive(Clone, Debug)]
pub struct NowPlaying {
    pub track: Track,
    pub playing: bool,
    /// The cover as the player gave it, usually JPEG or PNG.
    pub cover: Option<Arc<Vec<u8>>>,
    /// Changes whenever the track does, so a script knows when to send a new cover.
    pub track_number: u64,
}

pub type SharedNowPlaying = Arc<Mutex<Option<NowPlaying>>>;

pub fn start() -> Result<SharedNowPlaying> {
    let shared: SharedNowPlaying = Arc::default();
    let writing = Arc::clone(&shared);
    thread::Builder::new().name("media".into()).spawn(move || {
        if let Err(e) = watch(&writing) {
            eprintln!("error: media: {e:#}; media.current() stays empty");
        }
    })?;
    Ok(shared)
}

fn watch(shared: &SharedNowPlaying) -> Result<()> {
    // SAFETY: once, on this thread, before any WinRT call.
    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.ok().context("starting COM")?;
    let manager = SessionManager::RequestAsync()?.join().context("opening the media controls")?;
    let mut last: Option<NowPlaying> = None;
    let mut next_number = 0;
    loop {
        last = match read(&manager) {
            Ok(Some((track, playing, session))) => Some(match last {
                Some(same) if same.track == track => NowPlaying { playing, ..same },
                _ => {
                    next_number += 1;
                    let cover = cover_of(&session).unwrap_or_else(|e| {
                        eprintln!("error: media: reading the cover of {:?}: {e:#}", track.title);
                        None
                    });
                    NowPlaying { track, playing, cover, track_number: next_number }
                }
            }),
            Ok(None) => None,
            Err(e) => {
                eprintln!("error: media: {e:#}");
                None
            }
        };
        shared.lock().expect("media poisoned").clone_from(&last);
        thread::sleep(POLL);
    }
}

type Session = windows::Media::Control::GlobalSystemMediaTransportControlsSession;

/// `None` when nothing is playing or paused anywhere.
fn read(manager: &SessionManager) -> Result<Option<(Track, bool, Session)>> {
    let Ok(session) = manager.GetCurrentSession() else { return Ok(None) };
    let properties = session.TryGetMediaPropertiesAsync()?.join().context("reading the track")?;
    let track = Track { title: properties.Title()?.to_string(), artist: properties.Artist()?.to_string(), album: properties.AlbumTitle()?.to_string() };
    let playing = session.GetPlaybackInfo()?.PlaybackStatus()? == Status::Playing;
    Ok(Some((track, playing, session)))
}

fn cover_of(session: &Session) -> Result<Option<Arc<Vec<u8>>>> {
    let properties = session.TryGetMediaPropertiesAsync()?.join()?;
    let Ok(thumbnail) = properties.Thumbnail() else { return Ok(None) };
    let stream = thumbnail.OpenReadAsync()?.join().context("opening the cover")?;
    let size = u32::try_from(stream.Size()?).context("the cover is over 4 GB")?;
    let reader = DataReader::CreateDataReader(&stream)?;
    reader.LoadAsync(size)?.join().context("reading the cover")?;
    let mut bytes = vec![0; size as usize];
    reader.ReadBytes(&mut bytes)?;
    Ok(Some(Arc::new(bytes)))
}
