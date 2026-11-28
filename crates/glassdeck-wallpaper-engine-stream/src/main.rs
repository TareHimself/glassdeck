//! Attaches `wallpaper-engine` to glassdeckd over stream protocol v1: the wallpaper Wallpaper Engine
//! shows on its first monitor, rendered by Wallpaper Engine in a hidden window of the panel's size and
//! encoded only while someone watches. Runs until stopped, attaching again whenever it can.

mod scene;
mod still;

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use glassdeck_capture::pipeline::{self, EncoderSettings, Source, StreamOut, clear_stop_request, request_keyframe, request_stop};
use glassdeck_proto::Message;
use glassdeck_stream::{Codec, Control, Frame, Mode, Sender, StreamFormat, ToHost};
use glassdeck_wallpaper_engine::engine::{self, Install};
use glassdeck_wallpaper_engine::Item;

use scene::SceneSource;

const NAME: &str = "wallpaper-engine";
/// H.264, so the app previews it live: glassdeckd can't re-encode another program's stream.
const CODEC: Codec = Codec::H264;
const MBPS: u32 = 15;
const FPS: u32 = 60;
const RETRY: Duration = Duration::from_secs(5);
/// How often the shown wallpaper and Wallpaper Engine itself are looked at.
const LOOK_EVERY: Duration = Duration::from_secs(2);

fn main() {
    let mut said = String::new();
    loop {
        let now = match attend() {
            Ok(why) => why,
            Err(e) => format!("error: {e:#}"),
        };
        if now != said {
            eprintln!("{now}");
            said = now;
        }
        thread::sleep(RETRY);
    }
}

/// One attachment, from waiting for Wallpaper Engine to the stream detaching. Returns why it ended.
fn attend() -> Result<String> {
    let install = engine::install()?.context("Wallpaper Engine isn't installed in any Steam library")?;
    if !engine_running()? {
        return Ok("Waiting for Wallpaper Engine to start".to_owned());
    }
    let (controls, sender) = glassdeck_stream::attach_local(NAME, Mode::Live, CODEC)?;
    println!("Attached {NAME}");
    let events = follow(controls)?;
    let mut session = Session { install, sender, active: false, shown: None, encoding: None };
    let ended = session.run(&events);
    session.stop_encoding()?;
    ended
}

enum Event {
    Control(Control),
    Closed(String),
    Look,
}

fn follow(mut controls: glassdeck_stream::Controls) -> Result<mpsc::Receiver<Event>> {
    let (to, events) = mpsc::channel();
    thread::Builder::new().name("controls".into()).spawn(move || {
        loop {
            let event = match controls.recv() {
                Ok(Some(control)) => Event::Control(control),
                Ok(None) => Event::Closed("glassdeckd closed the stream".to_owned()),
                Err(e) => Event::Closed(format!("glassdeckd's connection: {e}")),
            };
            let closed = matches!(event, Event::Closed(_));
            if to.send(event).is_err() || closed {
                return;
            }
        }
    })?;
    Ok(events)
}

struct Encoding {
    folder: PathBuf,
    thread: JoinHandle<Result<()>>,
}

struct Session {
    install: Install,
    sender: Sender,
    active: bool,
    shown: Option<Item>,
    encoding: Option<Encoding>,
}

impl Session {
    fn run(&mut self, events: &mpsc::Receiver<Event>) -> Result<String> {
        loop {
            let event = match events.recv_timeout(LOOK_EVERY) {
                Ok(event) => event,
                Err(RecvTimeoutError::Timeout) => Event::Look,
                Err(RecvTimeoutError::Disconnected) => bail!("lost glassdeckd's requests"),
            };
            match event {
                Event::Control(Control::Activate) => self.active = true,
                Event::Control(Control::Deactivate) => self.active = false,
                Event::Control(Control::Keyframe) => request_keyframe(),
                Event::Closed(why) => return Ok(why),
                Event::Look => {
                    if !engine_running()? {
                        return Ok("Wallpaper Engine stopped; detached".to_owned());
                    }
                }
            }
            self.look()?;
        }
    }

    /// Brings the still and the encoding up to date with what Wallpaper Engine shows now.
    fn look(&mut self) -> Result<()> {
        if let Some(encoding) = self.encoding.take_if(|e| e.thread.is_finished()) {
            finished(encoding)?;
            bail!("the encoding stopped by itself");
        }
        let shown = self.install.shown()?;
        if folder_of(shown.as_ref()) != folder_of(self.shown.as_ref()) {
            if let Some(item) = &shown {
                println!("Showing {}", item.dir.display());
                match still::of(item) {
                    Ok(jpeg) => self.sender.still(jpeg)?,
                    Err(e) => eprintln!("error: no still for {}: {e:#}", item.id),
                }
            }
            self.shown = shown;
        }
        match next(self.active, self.encoding.as_ref().map(|e| e.folder.as_path()), folder_of(self.shown.as_ref())) {
            Next::Keep => {}
            Next::Stop => self.stop_encoding()?,
            Next::Start => self.start_encoding()?,
            Next::Restart => {
                self.stop_encoding()?;
                self.start_encoding()?;
            }
        }
        Ok(())
    }

    fn start_encoding(&mut self) -> Result<()> {
        let item = self.shown.clone().context("nothing to encode")?;
        let source = SceneSource::new(&item, settings(), None)?;
        let out = Box::new(Out(self.sender.clone()));
        clear_stop_request();
        let thread = thread::Builder::new()
            .name("encoding".into())
            .spawn(move || pipeline::run(Source::Frames(Box::new(source)), out))?;
        self.encoding = Some(Encoding { folder: item.dir, thread });
        Ok(())
    }

    fn stop_encoding(&mut self) -> Result<()> {
        let Some(encoding) = self.encoding.take() else { return Ok(()) };
        request_stop();
        finished(encoding)
    }
}

fn finished(encoding: Encoding) -> Result<()> {
    encoding.thread.join().map_err(|_| anyhow::anyhow!("the encoding panicked"))?.context("encoding the wallpaper")
}

fn folder_of(item: Option<&Item>) -> Option<&Path> {
    item.map(|item| item.dir.as_path())
}

#[derive(Debug, PartialEq, Eq)]
enum Next {
    Keep,
    Stop,
    Start,
    Restart,
}

/// Encode while watched, always the wallpaper shown now.
fn next(active: bool, encoding: Option<&Path>, shown: Option<&Path>) -> Next {
    match (active, encoding, shown) {
        (true, None, Some(_)) => Next::Start,
        (true, Some(now), Some(shown)) if now != shown => Next::Restart,
        (true, Some(_), Some(_)) | (_, None, _) => Next::Keep,
        (false, Some(_), _) | (true, Some(_), None) => Next::Stop,
    }
}

/// The panel's size, so Wallpaper Engine renders exactly what the keyboard shows.
fn settings() -> EncoderSettings {
    let (width, height) = glassdeck_package::PANEL;
    EncoderSettings {
        width: width.into(),
        height: height.into(),
        codec: glassdeck_proto::Codec::H264,
        fps: FPS,
        bitrate: MBPS * 1_000_000,
        keyframe_every: FPS * 2,
        vbv_frames: None,
    }
}

fn engine_running() -> Result<bool> {
    let mut system = sysinfo::System::new().context("listing processes")?;
    system.refresh_processes(sysinfo::ProcessesToUpdate::All, false);
    Ok(system.processes_by_exact_name(OsStr::new(engine::EXE)).next().is_some())
}

/// The encoder's stream, as stream protocol v1.
struct Out(Sender);

impl StreamOut for Out {
    fn send(&mut self, message: &Message) -> Result<()> {
        let message = match message {
            Message::StreamStart(info) => ToHost::StreamStart(StreamFormat { codec: wire_codec(info.codec), width: info.width, height: info.height }),
            Message::Frame(frame) => ToHost::Frame(Frame { seq: frame.seq, time_us: frame.time_us, keyframe: frame.keyframe, payload: frame.payload.clone() }),
            Message::StreamEnd { .. } => ToHost::StreamEnd,
            other => bail!("the encoder sent {other:?}, which a stream doesn't carry"),
        };
        self.0.send(&message).context("sending to glassdeckd")
    }
}

fn wire_codec(codec: glassdeck_proto::Codec) -> Codec {
    match codec {
        glassdeck_proto::Codec::H264 => Codec::H264,
        glassdeck_proto::Codec::Hevc => Codec::Hevc,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_the_shown_wallpaper_only_while_watched() {
        let (a, b) = (Some(Path::new("a")), Some(Path::new("b")));
        let table = [
            (true, None, a, Next::Start),
            (true, a, a, Next::Keep),
            (true, a, b, Next::Restart),
            (true, a, None, Next::Stop),
            (true, None, None, Next::Keep),
            (false, a, a, Next::Stop),
            (false, a, None, Next::Stop),
            (false, None, a, Next::Keep),
            (false, None, None, Next::Keep),
        ];
        for (active, encoding, shown, expected) in table {
            assert_eq!(next(active, encoding, shown), expected, "active {active}, encoding {encoding:?}, shown {shown:?}");
        }
    }
}
