//! The key scanner's serial link (`/dev/ttyS3`): key state from Flux's own firmware, which the
//! stock UI used to read. Only Shift is followed for now, to relabel the keys.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use glassdeck_scanner_proto::flux::FluxKeyState;
use glassdeck_scanner_proto::hdlc::{Decoder, SERIAL_MAX_PAYLOAD};
use rustix::termios::{self, OptionalActions};

const BAUD: u32 = 921_600;
/// How long to wait before opening the port again after it failed or went away.
const RETRY: Duration = Duration::from_secs(5);

/// Follows the link on its own thread for as long as the receiver runs, calling `shift` with
/// each change of the Shift keys (and once with the first state read).
pub fn follow_scanner(path: PathBuf, shift: impl FnMut(bool) + Send + 'static) -> Result<()> {
    thread::Builder::new().name("scanner-uart".into()).spawn(move || run(&path, shift))?;
    Ok(())
}

fn run(path: &Path, mut shift: impl FnMut(bool)) {
    let mut held: Option<bool> = None;
    loop {
        if let Err(e) = read(path, &mut |now| {
            if held != Some(now) {
                held = Some(now);
                shift(now);
            }
        }) {
            eprintln!("error: the key scanner's serial link {}: {e:#}; trying again in {RETRY:?}", path.display());
        }
        thread::sleep(RETRY);
    }
}

/// Reads until the port fails; returns only with that failure.
fn read(path: &Path, shift: &mut impl FnMut(bool)) -> Result<()> {
    let mut port = File::options().read(true).write(true).open(path).with_context(|| format!("opening {}", path.display()))?;
    configure(&port)?;
    println!("Key scanner: reading {} at {BAUD} baud", path.display());
    let mut decoder = Decoder::<SERIAL_MAX_PAYLOAD>::default();
    let mut buffer = [0u8; 512];
    loop {
        let read = port.read(&mut buffer).context("reading")?;
        anyhow::ensure!(read > 0, "the port closed");
        for &byte in &buffer[..read] {
            match decoder.push(byte) {
                None => {}
                Some(Ok(payload)) => match FluxKeyState::parse(payload) {
                    Ok(state) => shift(state.shift()),
                    Err(e) => eprintln!("error: an unreadable key state from the key scanner: {e:?}"),
                },
                Some(Err(e)) => eprintln!("error: a damaged frame from the key scanner: {e:?}"),
            }
        }
    }
}

/// Raw bytes at the firmware's 921600 8N1: nothing the terminal layer would otherwise rewrite.
fn configure(port: &File) -> Result<()> {
    let mut settings = termios::tcgetattr(port).context("reading the port's settings")?;
    settings.make_raw();
    settings.set_speed(BAUD).context("setting 921600 baud")?;
    termios::tcsetattr(port, OptionalActions::Now, &settings).context("applying the port's settings")
}
