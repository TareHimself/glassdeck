//! A source worker: glassdeckd starts itself as `glassdeckd source` for each source, because CEF can
//! start only once per process. The worker reads its request on stdin and streams to glassdeckd over
//! a localhost socket; glassdeckd writes control bytes back (`STOP` or `KEYFRAME`), and the socket
//! closing also asks it to stop.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::os::windows::process::CommandExt;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use glassdeck_capture::pipeline::{self, StreamOut, request_keyframe, request_stop};
use glassdeck_capture::sources::{self, SourceRequest};
use glassdeck_proto::Message;

const STOP: u8 = 0;
const KEYFRAME: u8 = 1;
/// How long a worker gets to end its stream cleanly before it's killed.
const STOP_GRACE: Duration = Duration::from_secs(10);
/// How long a new worker gets to connect back (CEF takes a few seconds to start).
const CONNECT_GRACE: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(20);
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub fn run(uplink: SocketAddr, token: &str) -> Result<()> {
    let request: SourceRequest = serde_json::from_reader(std::io::stdin()).context("reading the source request")?;
    let mut socket = TcpStream::connect(uplink).with_context(|| format!("connecting to glassdeckd at {uplink}"))?;
    socket.set_nodelay(true)?;
    socket.write_all(handshake(token).as_bytes())?;
    let mut control = socket.try_clone()?;
    thread::Builder::new().name("control".into()).spawn(move || {
        let mut byte = [0u8; 1];
        while let Ok(1) = control.read(&mut byte) {
            match byte[0] {
                KEYFRAME => request_keyframe(),
                _ => break,
            }
        }
        request_stop();
    })?;
    let ending = socket.try_clone()?;
    let result = sources::build(request).and_then(|source| pipeline::run(source, Box::new(SocketOut(socket))));
    // A clean close, so glassdeckd reads the end of the stream rather than a reset.
    let _ = ending.shutdown(Shutdown::Write);
    result
}

/// What a worker sends first, so only the worker glassdeckd started can feed the Link.
pub fn handshake(token: &str) -> String {
    format!("{token}\n")
}

struct SocketOut(TcpStream);

impl StreamOut for SocketOut {
    fn send(&mut self, message: &Message) -> Result<()> {
        self.0.write_all(&message.encode()).context("sending to glassdeckd")
    }
}

/// A worker process as glassdeckd sees it. `label` names it in the log ("Source 3", "Preview 1").
pub struct WorkerProcess {
    pub label: String,
    child: Mutex<Child>,
    /// Set once the worker connects back; control bytes go here.
    socket: Mutex<Option<TcpStream>>,
    last_error: Arc<Mutex<Option<String>>>,
}

impl WorkerProcess {
    /// Starts a worker for `request`; it connects back to the returned listener.
    pub fn spawn(label: String, request: &SourceRequest, token: &str) -> Result<(Arc<Self>, TcpListener)> {
        let listener = TcpListener::bind("127.0.0.1:0").context("listening for the source worker")?;
        let address = listener.local_addr()?;
        let mut child = Command::new(std::env::current_exe()?)
            .args(["source", "--uplink", &address.to_string(), "--token", token])
            .stdin(Stdio::piped())
            .stderr(Stdio::piped())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .context("starting a source worker")?;
        let mut stdin = child.stdin.take().expect("stdin is piped");
        stdin.write_all(&serde_json::to_vec(request)?)?;
        drop(stdin);
        let last_error = forward_stderr(label.clone(), child.stderr.take().expect("stderr is piped"));
        println!("{label}: {}", request.play);
        Ok((Arc::new(Self { label, child: Mutex::new(child), socket: Mutex::new(None), last_error }), listener))
    }

    /// Waits for the worker to connect back and prove it's ours; the socket then carries its stream.
    pub fn accept(&self, listener: TcpListener, token: &str) -> Result<TcpStream> {
        listener.set_nonblocking(true)?;
        let deadline = Instant::now() + CONNECT_GRACE;
        loop {
            match listener.accept() {
                Ok((mut socket, _)) => {
                    socket.set_nonblocking(false)?;
                    let expected = handshake(token);
                    let mut got = vec![0u8; expected.len()];
                    socket.read_exact(&mut got).context("reading the worker's handshake")?;
                    if got != expected.as_bytes() {
                        bail!("something other than our worker connected");
                    }
                    *self.socket.lock().expect("worker poisoned") = Some(socket.try_clone()?);
                    return Ok(socket);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(e).context("accepting the source worker"),
            }
            if !self.running() {
                bail!("the worker exited before streaming");
            }
            if Instant::now() > deadline {
                bail!("the worker didn't connect within {} s", CONNECT_GRACE.as_secs());
            }
            thread::sleep(POLL);
        }
    }

    pub fn ask_keyframe(&self) {
        if let Some(socket) = self.socket.lock().expect("worker poisoned").as_mut() {
            let _ = socket.write_all(&[KEYFRAME]);
        }
    }

    /// Asks the worker to end its stream; a worker that hasn't connected yet is killed.
    pub fn ask_stop(&self) {
        match self.socket.lock().expect("worker poisoned").as_mut() {
            Some(socket) => {
                let _ = socket.write_all(&[STOP]);
            }
            None => self.kill(),
        }
    }

    /// Asks the worker to stop, and kills it if it hasn't within the grace time.
    pub fn stop(&self) {
        self.ask_stop();
        let deadline = Instant::now() + STOP_GRACE;
        while Instant::now() < deadline {
            if !self.running() {
                println!("{} stopped", self.label);
                return;
            }
            thread::sleep(POLL);
        }
        eprintln!("{} didn't stop within {} s; killing it", self.label, STOP_GRACE.as_secs());
        self.kill();
    }

    pub fn kill(&self) {
        let _ = self.child.lock().expect("worker poisoned").kill();
    }

    pub fn wait(&self) -> ExitStatus {
        loop {
            if let Ok(Some(exit)) = self.child.lock().expect("worker poisoned").try_wait() {
                return exit;
            }
            thread::sleep(POLL);
        }
    }

    /// The worker's last `error:` line, which explains a failure.
    pub fn last_error(&self) -> Option<String> {
        self.last_error.lock().expect("worker poisoned").clone()
    }

    fn running(&self) -> bool {
        matches!(self.child.lock().expect("worker poisoned").try_wait(), Ok(None))
    }
}

/// The worker's output goes to glassdeckd's log, and its last error line explains a failure.
fn forward_stderr(label: String, stderr: impl Read + Send + 'static) -> Arc<Mutex<Option<String>>> {
    let last_error = Arc::new(Mutex::new(None));
    let kept = Arc::clone(&last_error);
    thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            eprintln!("[{label}] {line}");
            if let Some(error) = line.strip_prefix("error: ") {
                *kept.lock().expect("worker poisoned") = Some(error.to_owned());
            }
        }
    });
    last_error
}
