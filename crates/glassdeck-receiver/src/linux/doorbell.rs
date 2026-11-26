//! Wakes the compose loop when there's something to show: an eventfd it polls beside the display's own fd.

use std::os::fd::{AsFd, BorrowedFd, OwnedFd};

use anyhow::{Context, Result};
use rustix::event::{EventfdFlags, eventfd};
use rustix::io::Errno;

pub struct Doorbell {
    fd: OwnedFd,
}

impl Doorbell {
    pub fn new() -> Result<Self> {
        let fd = eventfd(0, EventfdFlags::CLOEXEC | EventfdFlags::NONBLOCK).context("creating the compose loop's doorbell")?;
        Ok(Self { fd })
    }

    pub fn ring(&self) {
        match rustix::io::write(&self.fd, &1u64.to_ne_bytes()) {
            Ok(_) | Err(Errno::AGAIN) => {}
            Err(e) => eprintln!("error: ringing the compose loop: {e}"),
        }
    }

    /// Clears the rings so far, after the loop has woken.
    pub fn answer(&self) {
        let mut count = [0u8; 8];
        match rustix::io::read(&self.fd, &mut count) {
            Ok(_) | Err(Errno::AGAIN) => {}
            Err(e) => eprintln!("error: answering the compose loop's doorbell: {e}"),
        }
    }
}

impl AsFd for Doorbell {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}
