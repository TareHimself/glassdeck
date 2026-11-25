//! Renderers on the keyboard itself (glassdeck-local-web, glassdeck-scene): separate processes that
//! draw into their own scanout buffers and present them over the compositor socket.

use std::collections::HashMap;
use std::io::IoSliceMut;
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};

use anyhow::{Context, Result, anyhow, bail};
use glassdeck_proto::compositor::{BufferId, FromCompositor, MAX_MESSAGE_LEN, ToCompositor};
use rustix::io::Errno;
use rustix::net::{
    AddressFamily, RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, SendFlags, Shutdown, SocketFlags, SocketType,
};
use rustix::process::{Pid, Signal, kill_process};

use super::Picture;
use super::compositor::{Registrar, SharedBuffer};
use crate::mailbox::{Mailbox, ShownFrame, Timing};

/// The renderer finds its end of the socket here, as `--compositor-fd` says.
const RENDERER_SOCKET_FD: i32 = 3;
/// Our end is moved above the standard descriptors, so placing the child's end at 3 never overwrites it.
const FIRST_SPARE_FD: i32 = 10;
/// The unprivileged user renderers run as; it may use the GPU and display, nothing else of ours.
pub const RENDERER_USER: &str = "glassdeck-local-web";

/// A renderer's picture on its way to the screen; dropping it hands the buffer back to the renderer.
pub struct SharedPicture {
    pub buffer: Arc<SharedBuffer>,
    id: BufferId,
    socket: Arc<OwnedFd>,
}

impl Drop for SharedPicture {
    fn drop(&mut self) {
        match send(&self.socket, &FromCompositor::Released(self.id)) {
            Ok(()) | Err(RendererGone) => {}
        }
    }
}

/// The renderer closed its end; its buffers no longer need handing back.
struct RendererGone;

fn send(socket: &OwnedFd, message: &FromCompositor) -> Result<(), RendererGone> {
    match rustix::net::send(socket, &message.encode(), SendFlags::NOSIGNAL) {
        Ok(_) => Ok(()),
        Err(Errno::PIPE | Errno::CONNRESET | Errno::NOTCONN) => Err(RendererGone),
        Err(e) => {
            eprintln!("error: answering a renderer: {e}");
            Err(RendererGone)
        }
    }
}

pub struct RendererProcess {
    label: &'static str,
    stop: Arc<AtomicBool>,
    socket: Arc<OwnedFd>,
    renderer: Pid,
    thread: JoinHandle<()>,
}

impl RendererProcess {
    /// Runs `program` as the renderer user with `--compositor-fd` added. `stopped` runs if it ends by
    /// itself, never after `stop`.
    pub fn start(
        label: &'static str,
        program: &str,
        args: &[String],
        registrar: Registrar,
        mailbox: Arc<Mailbox>,
        stopped: impl FnOnce(anyhow::Error) + Send + 'static,
    ) -> Result<Self> {
        let (ours, theirs) =
            rustix::net::socketpair(AddressFamily::UNIX, SocketType::SEQPACKET, SocketFlags::CLOEXEC, None)
                .context("creating the compositor socket")?;
        let theirs = rustix::io::fcntl_dupfd_cloexec(&theirs, FIRST_SPARE_FD)?;
        let theirs_raw = theirs.as_raw_fd();
        let mut command = Command::new("runuser");
        command
            .args(["-u", RENDERER_USER, "--", program])
            .arg(format!("--compositor-fd={RENDERER_SOCKET_FD}"))
            .args(args);
        // SAFETY: dup2 is async-signal-safe, and nothing else runs between fork and exec.
        unsafe {
            command.pre_exec(move || match dup2(theirs_raw, RENDERER_SOCKET_FD) {
                -1 => Err(std::io::Error::last_os_error()),
                _ => Ok(()),
            });
        }
        let mut child = command.spawn().with_context(|| format!("starting {label}"))?;
        let renderer = Pid::from_child(&child);
        drop(theirs);
        println!("{label} started (pid {})", child.id());

        let stop = Arc::new(AtomicBool::new(false));
        let socket = Arc::new(ours);
        let thread = {
            let (stop, socket) = (Arc::clone(&stop), Arc::clone(&socket));
            thread::Builder::new().name(label.into()).spawn(move || {
                let served = serve(label, &socket, &registrar, &mailbox);
                let status = child.wait();
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                let error = match (served, status) {
                    (Err(e), _) => e,
                    (Ok(()), Ok(status)) => anyhow!("{label} exited ({status})"),
                    (Ok(()), Err(e)) => anyhow!("{label} vanished: {e}"),
                };
                stopped(error);
            })?
        };
        Ok(Self { label, stop, socket, renderer, thread })
    }

    pub fn stop(self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Err(e) = kill_process(self.renderer, Signal::TERM) {
            eprintln!("error: stopping {}: {e}", self.label);
        }
        if let Err(e) = rustix::net::shutdown(&*self.socket, Shutdown::Both) {
            eprintln!("error: closing {}'s compositor socket: {e}", self.label);
        }
        if self.thread.join().is_err() {
            eprintln!("error: the {} thread panicked", self.label);
        }
    }
}

unsafe extern "C" {
    fn dup2(old: i32, new: i32) -> i32;
}

/// Serves one renderer until it closes the socket.
fn serve(label: &str, socket: &Arc<OwnedFd>, registrar: &Registrar, mailbox: &Mailbox) -> Result<()> {
    let mut buffers: HashMap<BufferId, Arc<SharedBuffer>> = HashMap::new();
    let mut next_id: BufferId = 1;
    let mut bytes = [0u8; MAX_MESSAGE_LEN];
    let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
    loop {
        let mut control = RecvAncillaryBuffer::new(&mut space);
        let received =
            rustix::net::recvmsg(&**socket, &mut [IoSliceMut::new(&mut bytes)], &mut control, RecvFlags::CMSG_CLOEXEC)
                .with_context(|| format!("reading from {label}"))?;
        if received.bytes == 0 {
            return Ok(());
        }
        let mut fds: Vec<OwnedFd> = control
            .drain()
            .flat_map(|message| match message {
                RecvAncillaryMessage::ScmRights(fds) => fds.collect(),
                _ => Vec::new(),
            })
            .collect();
        match ToCompositor::decode(&bytes[..received.bytes])? {
            ToCompositor::Register(layout) => {
                let fd = fds.pop().with_context(|| format!("{label} registered a buffer without sending it"))?;
                match registrar.register(&fd, layout) {
                    Ok(buffer) => {
                        let id = next_id;
                        next_id += 1;
                        buffers.insert(id, Arc::new(buffer));
                        send(socket, &FromCompositor::Registered(id)).map_err(|RendererGone| anyhow!("{label} hung up"))?;
                    }
                    Err(e) => {
                        let reason = format!("{e:#}");
                        let _ = send(socket, &FromCompositor::Refused { reason: reason.clone() });
                        bail!("refused a {label} buffer: {reason}");
                    }
                }
            }
            ToCompositor::Present(id) => {
                let buffer = Arc::clone(buffers.get(&id).with_context(|| format!("{label} presented unknown buffer {id}"))?);
                let picture = Picture::Shared(SharedPicture { buffer, id, socket: Arc::clone(socket) });
                mailbox.put(ShownFrame { picture, timing: Timing::Paced { late_us: 0 } });
            }
        }
    }
}
