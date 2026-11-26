//! The command framing the key scanner and the keyboard's stock bridge share: a header packet,
//! the payload zero-padded to whole packets, then a zero-length packet. The answer comes back in
//! the same shape, with ACK or NAK in place of the command.

use anyhow::{Context, Result, bail};
use glassdeck_scanner_proto::usb::{Header, Reply};

/// A bulk command interface.
pub trait Pipe {
    /// The device's packet size.
    const PACKET: usize;
    fn send(&mut self, bytes: &[u8]) -> Result<()>;
    /// One IN transfer: whole packets, or nothing when it was a zero-length packet.
    fn receive(&mut self) -> Result<Vec<u8>>;
}

fn request_bytes<const P: usize>(code: u8, payload: &[u8]) -> Vec<u8> {
    let mut bytes = Header::for_payload::<P>(code, payload.len())
        .encode::<P>()
        .to_vec();
    for chunk in payload.chunks(P) {
        let start = bytes.len();
        bytes.extend_from_slice(chunk);
        bytes.resize(start + P, 0);
    }
    bytes
}

/// Sends command `code` and reads its answer. Zero-length packets, which the devices put between
/// and after packets, are skipped.
pub fn request<T: Pipe>(pipe: &mut T, code: u8, payload: &[u8]) -> Result<(Reply, Vec<u8>)> {
    let bytes = match T::PACKET {
        64 => request_bytes::<64>(code, payload),
        512 => request_bytes::<512>(code, payload),
        other => bail!("no framing for {other}-byte packets"),
    };
    pipe.send(&bytes)?;
    pipe.send(&[])?;

    let mut stream = Vec::new();
    let header = loop {
        stream.extend(pipe.receive()?);
        if stream.len() >= T::PACKET {
            break Header::parse(&stream).context("an unreadable answer header")?;
        }
    };
    let reply = match header.code {
        0 => Reply::Nak,
        1 => Reply::Ack,
        other => bail!("the answer's reply code is {other}, neither ACK nor NAK"),
    };
    let total = T::PACKET * (1 + header.packets as usize);
    while stream.len() < total {
        stream.extend(pipe.receive()?);
    }
    if stream.len() > total {
        bail!(
            "the answer ran {} bytes past its {} packets",
            stream.len() - total,
            header.packets
        );
    }
    let end = T::PACKET + header.bytes as usize;
    if end > total {
        bail!(
            "the answer says {} bytes but carried {}",
            header.bytes,
            total - T::PACKET
        );
    }
    Ok((reply, stream[T::PACKET..end].to_vec()))
}
