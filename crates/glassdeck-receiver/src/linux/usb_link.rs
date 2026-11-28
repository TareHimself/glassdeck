//! The USB bridge's I/O: writes the descriptors to the FunctionFS mount, answers ep0's events, and
//! copies bytes between the endpoints and a local TCP connection to the receiver's own listener, one
//! connection per session the PC starts.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use glassdeck_proto::usb::needs_zero_length_packet;
use rustix::io::Errno;

use crate::usb_link::{EVENT_LEN, Effect, Event, decode_event, descriptors, effects, strings};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const READ_CHUNK: usize = 64 * 1024;
/// FunctionFS answers a deliberate stall with EL2HLT.
const STALLED: Errno = Errno::L2HLT;
/// An endpoint read or write while the PC has the function deconfigured.
const SHUT_DOWN: Errno = Errno::SHUTDOWN;
const RETRY_AFTER: Duration = Duration::from_millis(200);

/// The current session's connection to the receiver's listener; `None` between sessions.
type Session = Arc<Mutex<Option<TcpStream>>>;

/// Sets the function up and starts its threads. `function` is the FunctionFS mount; the launcher
/// binds the USB controller once ep1 appears, which happens when the descriptors are accepted.
pub fn start_usb_link(function: &Path, listener: SocketAddr) -> Result<()> {
    let local = SocketAddr::from(([127, 0, 0, 1], listener.port()));
    let ep0_path = function.join("ep0");
    let mut ep0 = OpenOptions::new().read(true).write(true).open(&ep0_path).with_context(|| format!("opening {}", ep0_path.display()))?;
    write_whole(&mut ep0, &descriptors(), "the USB descriptors")?;
    write_whole(&mut ep0, &strings(), "the USB strings")?;
    let from_pc = File::open(function.join("ep1")).context("opening ep1, the endpoint from the PC")?;
    let to_pc = OpenOptions::new().write(true).open(function.join("ep2")).context("opening ep2, the endpoint to the PC")?;
    let to_pc = Arc::new(Mutex::new(to_pc));
    let session: Session = Arc::default();
    println!("USB link: ready at {}", function.display());

    {
        let session = Arc::clone(&session);
        thread::Builder::new().name("usb-ep0".into()).spawn(move || {
            if let Err(e) = control(ep0, &session, &to_pc, local) {
                eprintln!("error: the USB link's control endpoint: {e:#}");
            }
        })?;
    }
    thread::Builder::new().name("usb-from-pc".into()).spawn(move || from_the_pc(from_pc, &session))?;
    Ok(())
}

/// FunctionFS takes each blob in one write.
fn write_whole(ep0: &mut File, blob: &[u8], what: &str) -> Result<()> {
    let written = ep0.write(blob).with_context(|| format!("writing {what}"))?;
    if written != blob.len() {
        bail!("FunctionFS took {written} of the {} bytes of {what}", blob.len());
    }
    Ok(())
}

fn control(mut ep0: File, session: &Session, to_pc: &Arc<Mutex<File>>, local: SocketAddr) -> Result<()> {
    let mut buffer = [0u8; EVENT_LEN * 4];
    loop {
        let read = ep0.read(&mut buffer).context("reading ep0")?;
        if read % EVENT_LEN != 0 {
            bail!("ep0 gave {read} bytes, not whole events");
        }
        for chunk in buffer[..read].chunks_exact(EVENT_LEN) {
            let event = decode_event(chunk.try_into().expect("chunks_exact"))?;
            for &effect in effects(&event) {
                apply(effect, &event, &ep0, session, to_pc, local)?;
            }
        }
    }
}

fn apply(effect: Effect, event: &Event, ep0: &File, session: &Session, to_pc: &Arc<Mutex<File>>, local: SocketAddr) -> Result<()> {
    match effect {
        Effect::EndSession => {
            if let Some(stream) = lock(session).take() {
                let _ = stream.shutdown(Shutdown::Both);
                println!("USB link: session ended");
            }
        }
        Effect::StartSession => {
            let stream = TcpStream::connect_timeout(&local, CONNECT_TIMEOUT).with_context(|| format!("connecting to the receiver at {local}"))?;
            stream.set_nodelay(true)?;
            let reading = stream.try_clone()?;
            *lock(session) = Some(stream);
            let to_pc = Arc::clone(to_pc);
            thread::Builder::new().name("usb-to-pc".into()).spawn(move || to_the_pc(reading, &to_pc))?;
            println!("USB link: session started");
        }
        Effect::Ack => {
            rustix::io::read(ep0, &mut [0u8; 0][..]).context("acknowledging a request on ep0")?;
        }
        Effect::Stall => {
            let Event::Setup(setup) = event else { bail!("only a request can be stalled") };
            let stalled = if setup.device_to_host() { rustix::io::read(ep0, &mut [0u8; 0][..]) } else { rustix::io::write(ep0, &[]) };
            match stalled {
                Err(e) if e == STALLED => {}
                Ok(_) => {}
                Err(e) => return Err(e).context("stalling a request on ep0"),
            }
            println!("USB link: refused request {setup:?}");
        }
    }
    Ok(())
}

/// Copies what the PC sends into the current session; bytes that arrive with no session are dropped.
fn from_the_pc(mut from_pc: File, session: &Session) {
    let mut buffer = vec![0u8; READ_CHUNK];
    loop {
        let read = match from_pc.read(&mut buffer) {
            Ok(read) => read,
            Err(e) if Errno::from_io_error(&e) == Some(SHUT_DOWN) => {
                thread::sleep(RETRY_AFTER);
                continue;
            }
            Err(e) => {
                eprintln!("error: reading from the PC over USB: {e}");
                thread::sleep(RETRY_AFTER);
                continue;
            }
        };
        if read == 0 {
            continue;
        }
        let mut current = lock(session);
        if let Some(stream) = current.as_mut()
            && let Err(e) = stream.write_all(&buffer[..read])
        {
            eprintln!("error: passing the PC's bytes to the receiver: {e}");
            let _ = stream.shutdown(Shutdown::Both);
            *current = None;
        }
    }
}

/// Copies what the receiver sends back to the PC until the session's connection closes.
fn to_the_pc(mut reading: TcpStream, to_pc: &Mutex<File>) {
    let mut buffer = vec![0u8; READ_CHUNK];
    loop {
        let read = match reading.read(&mut buffer) {
            Ok(0) => return,
            Ok(read) => read,
            Err(e) => {
                eprintln!("error: reading the receiver's answers: {e}");
                return;
            }
        };
        if let Err(e) = send(&mut lock(to_pc), &buffer[..read]) {
            eprintln!("error: sending to the PC over USB: {e:#}");
            let _ = reading.shutdown(Shutdown::Both);
            return;
        }
    }
}

fn send(to_pc: &mut File, bytes: &[u8]) -> Result<()> {
    let written = to_pc.write(bytes)?;
    if written != bytes.len() {
        bail!("ep2 took {written} of {} bytes", bytes.len());
    }
    if needs_zero_length_packet(bytes.len()) {
        rustix::io::write(&*to_pc, &[]).context("ending the transfer with a zero-length packet")?;
    }
    Ok(())
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().expect("a USB link lock was poisoned")
}
