//! Our end of the receiver's compositor socket (glassdeck-proto's `compositor` module).

use std::io::{IoSlice, IoSliceMut};
use std::mem::MaybeUninit;
use std::os::fd::{BorrowedFd, FromRawFd, OwnedFd};

use anyhow::{Context, Result, bail};
use glassdeck_proto::compositor::{BufferId, BufferLayout, FromCompositor, MAX_MESSAGE_LEN, ToCompositor};
use rustix::io::Errno;
use rustix::net::{RecvAncillaryBuffer, RecvFlags, SendAncillaryBuffer, SendAncillaryMessage, SendFlags};

pub struct Compositor {
    socket: OwnedFd,
}

pub enum Received {
    Message(FromCompositor),
    /// Nothing waiting, when not asked to wait.
    Nothing,
    /// The receiver closed the socket: it wants us gone.
    Closed,
}

impl Compositor {
    /// # Safety
    /// `fd` must be the open compositor socket the receiver handed us, owned by nothing else.
    pub unsafe fn from_fd(fd: i32) -> Self {
        // SAFETY: as the caller promised.
        Self { socket: unsafe { OwnedFd::from_raw_fd(fd) } }
    }

    pub fn register(&self, dma_buf: BorrowedFd<'_>, layout: BufferLayout) -> Result<BufferId> {
        let bytes = ToCompositor::Register(layout).encode();
        let fds = [dma_buf];
        let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
        let mut control = SendAncillaryBuffer::new(&mut space);
        control.push(SendAncillaryMessage::ScmRights(&fds));
        rustix::net::sendmsg(&self.socket, &[IoSlice::new(&bytes)], &mut control, SendFlags::NOSIGNAL)
            .context("registering a buffer")?;
        match self.receive(true)? {
            Received::Message(FromCompositor::Registered(id)) => Ok(id),
            Received::Message(FromCompositor::Refused { reason }) => bail!("the receiver refused our buffer: {reason}"),
            Received::Message(FromCompositor::Released(id)) => bail!("the receiver released buffer {id} before any was shown"),
            Received::Nothing | Received::Closed => bail!("the receiver closed the socket while we registered"),
        }
    }

    pub fn present(&self, id: BufferId) -> Result<()> {
        rustix::net::send(&self.socket, &ToCompositor::Present(id).encode(), SendFlags::NOSIGNAL).context("presenting")?;
        Ok(())
    }

    pub fn receive(&self, wait: bool) -> Result<Received> {
        let mut bytes = [0u8; MAX_MESSAGE_LEN];
        let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
        let mut control = RecvAncillaryBuffer::new(&mut space);
        let flags = if wait { RecvFlags::CMSG_CLOEXEC } else { RecvFlags::CMSG_CLOEXEC | RecvFlags::DONTWAIT };
        match rustix::net::recvmsg(&self.socket, &mut [IoSliceMut::new(&mut bytes)], &mut control, flags) {
            Ok(received) if received.bytes == 0 => Ok(Received::Closed),
            Ok(received) => Ok(Received::Message(FromCompositor::decode(&bytes[..received.bytes])?)),
            Err(Errno::AGAIN) => Ok(Received::Nothing),
            Err(Errno::CONNRESET) => Ok(Received::Closed),
            Err(e) => Err(e).context("reading from the receiver"),
        }
    }
}
