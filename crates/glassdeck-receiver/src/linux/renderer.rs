//! Renderers on the keyboard itself (glassdeck-local-web, glassdeck-gpu): separate processes that
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
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use glassdeck_proto::compositor::{BufferId, FromCompositor, MAX_MESSAGE_LEN, ToCompositor};
use glassdeck_proto::{Kind, ScriptTarget};
use rustix::io::Errno;
use rustix::net::{AddressFamily, RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, SendFlags, Shutdown, SocketFlags, SocketType};
use rustix::process::{Pid, Signal, kill_process, kill_process_group};

use super::Picture;
use super::compositor::{Registrar, SharedBuffer};
use crate::mailbox::{Mailbox, ShownFrame, Timing};
use crate::upstream::Upstream;

/// The renderer finds its end of the socket here, as `--compositor-fd` says.
const RENDERER_SOCKET_FD: i32 = 3;
/// Our end is moved above the standard descriptors, so placing the child's end at 3 never overwrites it.
const FIRST_SPARE_FD: i32 = 10;
/// The unprivileged user renderers run as; it may use the GPU and display, nothing else of ours.
pub const RENDERER_USER: &str = "glassdeck-local-web";

/// The renderer user's uid and gid, from /etc/passwd (the keyboard has no runuser).
fn renderer_account() -> Result<(u32, u32)> {
    let passwd = std::fs::read_to_string("/etc/passwd").context("reading /etc/passwd")?;
    account(&passwd, RENDERER_USER).with_context(|| format!("no {RENDERER_USER} user in /etc/passwd"))
}

/// `user`'s uid and gid in an /etc/passwd text.
fn account(passwd: &str, user: &str) -> Option<(u32, u32)> {
    passwd.lines().find_map(|line| {
        let mut fields = line.split(':');
        (fields.next()? == user).then_some(())?;
        let uid = fields.nth(1)?.parse().ok()?;
        let gid = fields.next()?.parse().ok()?;
        Some((uid, gid))
    })
}
/// How long a renderer gets to exit after SIGTERM before it's killed.
const STOP_GRACE: Duration = Duration::from_secs(2);
const STOP_POLL: Duration = Duration::from_millis(10);

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
    /// The renderer, which leads its own process group
    renderer: Pid,
    thread: JoinHandle<()>,
}

impl RendererProcess {
    /// Runs `program` as the renderer user with `--compositor-fd` added. `stopped` runs if it ends by
    /// itself, never after `stop`. A canvas background's script messages go `upstream`; others have none.
    pub fn start(
        label: &'static str,
        program: &str,
        args: &[String],
        registrar: Registrar,
        mailbox: Arc<Mailbox>,
        upstream: Option<Arc<Upstream>>,
        stopped: impl FnOnce(anyhow::Error) + Send + 'static,
    ) -> Result<Self> {
        let (ours, theirs) = rustix::net::socketpair(AddressFamily::UNIX, SocketType::SEQPACKET, SocketFlags::CLOEXEC, None)
            .context("creating the compositor socket")?;
        let theirs = rustix::io::fcntl_dupfd_cloexec(&theirs, FIRST_SPARE_FD)?;
        let theirs_raw = theirs.as_raw_fd();
        let (uid, gid) = renderer_account()?;
        let mut command = Command::new(program);
        command.arg(format!("--compositor-fd={RENDERER_SOCKET_FD}")).args(args);
        // std drops every supplementary group when it sets the uid, leaving the user's own group.
        command.uid(uid).gid(gid);
        // Its own process group, so a last-resort SIGKILL takes anything it started along with it.
        command.process_group(0);
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
                let served = serve(label, &socket, &registrar, &mailbox, upstream.as_deref());
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

    /// Data for the renderer's script (glassdeck-gpu); refused if it's larger than one message holds.
    pub fn deliver(&self, payload: &[u8]) -> Result<()> {
        let message = FromCompositor::data(payload.to_vec()).map_err(|e| anyhow!("{e}"))?;
        send(&self.socket, &message).map_err(|RendererGone| anyhow!("{} hung up", self.label))
    }

    /// A message from a canvas's host half, for its script; refused if it's larger than one message holds.
    pub fn deliver_message(&self, message: u16, payload: &[u8]) -> Result<()> {
        let message = FromCompositor::script_message(Kind::KeyboardEvent, message, payload.to_vec()).map_err(|e| anyhow!("{e}"))?;
        send(&self.socket, &message).map_err(|RendererGone| anyhow!("{} hung up", self.label))
    }

    /// Asks the renderer to exit, and kills its process group if it hasn't within `STOP_GRACE`.
    pub fn stop(self) {
        self.stop.store(true, Ordering::Relaxed);
        self.signal(kill_process(self.renderer, Signal::TERM));
        if let Err(e) = rustix::net::shutdown(&*self.socket, Shutdown::Both) {
            eprintln!("error: closing {}'s compositor socket: {e}", self.label);
        }
        let deadline = Instant::now() + STOP_GRACE;
        while !self.thread.is_finished() && Instant::now() < deadline {
            thread::sleep(STOP_POLL);
        }
        if !self.thread.is_finished() {
            eprintln!("{} ignored SIGTERM for {STOP_GRACE:?}; killing it", self.label);
            self.signal(kill_process_group(self.renderer, Signal::KILL));
        }
        if self.thread.join().is_err() {
            eprintln!("error: the {} thread panicked", self.label);
        }
    }

    /// A process that already exited by itself has nothing left to signal, which is fine.
    fn signal(&self, sent: rustix::io::Result<()>) {
        match sent {
            Ok(()) | Err(Errno::SRCH) => {}
            Err(e) => eprintln!("error: signalling {}: {e}", self.label),
        }
    }
}

unsafe extern "C" {
    fn dup2(old: i32, new: i32) -> i32;
}

/// Serves one renderer until it closes the socket.
fn serve(label: &str, socket: &Arc<OwnedFd>, registrar: &Registrar, mailbox: &Mailbox, upstream: Option<&Upstream>) -> Result<()> {
    let mut buffers: HashMap<BufferId, Arc<SharedBuffer>> = HashMap::new();
    let mut next_id: BufferId = 1;
    let mut bytes = [0u8; MAX_MESSAGE_LEN];
    let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
    loop {
        let mut control = RecvAncillaryBuffer::new(&mut space);
        let received = rustix::net::recvmsg(&**socket, &mut [IoSliceMut::new(&mut bytes)], &mut control, RecvFlags::CMSG_CLOEXEC)
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
            ToCompositor::ScriptMessage { message, payload } => match upstream {
                Some(upstream) => upstream.script_message(ScriptTarget::Background, message, payload),
                None => bail!("{label} sent a script message, which only a canvas background can"),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::account;

    #[test]
    fn finds_the_renderer_users_ids() {
        let passwd = "root:x:0:0:root:/root:/bin/bash\nglassdeck-local-web:x:1000:28:Linux User,,,:/home/glassdeck-local-web:/bin/false\n";
        assert_eq!(account(passwd, "glassdeck-local-web"), Some((1000, 28)));
        assert_eq!(account(passwd, "glassdeck"), None);
        assert_eq!(account("glassdeck-local-web:x:oops:28::/:/bin/false", "glassdeck-local-web"), None);
    }
}
