//! The Link over USB. glassdeckd's Link speaks TCP, so a local listener stands in for the keyboard:
//! each connection to it opens the receiver's own USB function, starts a session there with a vendor
//! request, and copies bytes both ways until either side closes.

use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use glassdeck_proto::usb::{INTERFACE_CLASS, INTERFACE_PROTOCOL, INTERFACE_SUBCLASS, NEW_SESSION, needs_zero_length_packet};
use nusb::descriptors::TransferType;
use nusb::transfer::{Buffer, Bulk, ControlOut, ControlType, Direction, In, Out, Recipient, TransferError};
use nusb::{Interface, MaybeFuture};

/// The keyboard's USB device: the stock bridge's VID and PID, with our function beside garfield's.
const KEYBOARD_VID: u16 = 0x3761;
const KEYBOARD_PID: u16 = 0x0104;
const CONTROL_TIMEOUT: Duration = Duration::from_secs(2);
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
/// How often the reader looks up to see whether the session has ended.
const READ_POLL: Duration = Duration::from_millis(500);
const CHUNK: usize = 64 * 1024;

/// Starts the bridge and returns the address the Link connects to.
pub fn start() -> Result<SocketAddr> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).context("listening for the Link's USB bridge")?;
    let address = listener.local_addr()?;
    thread::Builder::new().name("usb-link".into()).spawn(move || {
        for connection in listener.incoming() {
            match connection {
                Ok(link) => {
                    if let Err(e) = session(link) {
                        eprintln!("USB link: {e:#}");
                    }
                }
                Err(e) => eprintln!("USB link: accepting the Link's connection: {e}"),
            }
        }
    })?;
    Ok(address)
}

/// The receiver's function on the keyboard, claimed, with its bulk endpoints' addresses.
struct Function {
    interface: Interface,
    out: u8,
    input: u8,
}

fn open() -> Result<Function> {
    let device = nusb::list_devices()
        .wait()
        .context("listing USB devices")?
        .find(|d| d.vendor_id() == KEYBOARD_VID && d.product_id() == KEYBOARD_PID)
        .with_context(|| format!("the keyboard isn't on USB (looked for {KEYBOARD_VID:04x}:{KEYBOARD_PID:04x})"))?;
    let number = device
        .interfaces()
        .find(|i| (i.class(), i.subclass(), i.protocol()) == (INTERFACE_CLASS, INTERFACE_SUBCLASS, INTERFACE_PROTOCOL))
        .map(|i| i.interface_number())
        .context("the keyboard has no Glassdeck USB function; is Glassdeck running on it?")?;
    let interface = device.open().wait().context("opening the keyboard")?.claim_interface(number).wait().context("claiming the Glassdeck USB function")?;
    let descriptor = interface.descriptor().context("the Glassdeck USB function has no descriptor")?;
    let bulk = |direction: Direction| {
        descriptor
            .endpoints()
            .find(|e| e.transfer_type() == TransferType::Bulk && e.direction() == direction)
            .map(|e| e.address())
            .with_context(|| format!("the Glassdeck USB function has no bulk {direction:?} endpoint"))
    };
    let (out, input) = (bulk(Direction::Out)?, bulk(Direction::In)?);
    Ok(Function { interface, out, input })
}

fn session(link: TcpStream) -> Result<()> {
    let function = match open() {
        Ok(function) => function,
        Err(e) => {
            let _ = link.shutdown(Shutdown::Both);
            return Err(e);
        }
    };
    let request = ControlOut {
        control_type: ControlType::Vendor,
        recipient: Recipient::Interface,
        request: NEW_SESSION,
        value: 0,
        index: u16::from(function.interface.interface_number()),
        data: &[],
    };
    function.interface.control_out(request, CONTROL_TIMEOUT).wait().context("starting a session on the keyboard")?;
    link.set_nodelay(true)?;
    let ended = Arc::new(AtomicBool::new(false));
    let to_keyboard = function.interface.endpoint::<Bulk, Out>(function.out)?;
    let from_keyboard = function.interface.endpoint::<Bulk, In>(function.input)?;
    let reader = {
        let (link, ended) = (link.try_clone()?, Arc::clone(&ended));
        thread::Builder::new().name("usb-from-keyboard".into()).spawn(move || from_the_keyboard(from_keyboard, link, &ended))?
    };
    let sent = to_the_keyboard(to_keyboard, &link);
    ended.store(true, Ordering::Relaxed);
    let _ = link.shutdown(Shutdown::Both);
    let _ = reader.join();
    sent
}

/// Until the Link closes its end.
fn to_the_keyboard(mut endpoint: nusb::Endpoint<Bulk, Out>, mut link: &TcpStream) -> Result<()> {
    let mut chunk = vec![0u8; CHUNK];
    loop {
        let read = link.read(&mut chunk).context("reading from the Link")?;
        if read == 0 {
            return Ok(());
        }
        let mut buffer = Buffer::new(read);
        buffer.extend_from_slice(&chunk[..read]);
        endpoint.transfer_blocking(buffer, WRITE_TIMEOUT).status.context("sending to the keyboard")?;
        if needs_zero_length_packet(read) {
            endpoint.transfer_blocking(Buffer::new(0), WRITE_TIMEOUT).status.context("ending a transfer to the keyboard")?;
        }
    }
}

/// Until the session ends. A read that times out still hands back what arrived before it did.
fn from_the_keyboard(mut endpoint: nusb::Endpoint<Bulk, In>, mut link: TcpStream, ended: &AtomicBool) {
    while !ended.load(Ordering::Relaxed) {
        let completion = endpoint.transfer_blocking(Buffer::new(CHUNK), READ_POLL);
        let arrived = &completion.buffer[..completion.actual_len.min(completion.buffer.len())];
        if !arrived.is_empty()
            && let Err(e) = link.write_all(arrived)
        {
            eprintln!("USB link: passing the keyboard's answers to the Link: {e}");
            break;
        }
        match completion.status {
            Ok(()) | Err(TransferError::Cancelled) => {}
            Err(e) => {
                eprintln!("USB link: reading from the keyboard: {e}");
                break;
            }
        }
    }
    let _ = link.shutdown(Shutdown::Both);
}
