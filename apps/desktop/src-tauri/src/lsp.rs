//! Language servers for the editors. Each runs as a child process speaking LSP over stdio; this relays
//! its JSON-RPC messages to the webview and back unchanged, and knows nothing about any language.

use std::collections::HashMap;
use std::ffi::OsString;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use specta::Type;

use glassdeck_package::PackageType;
use glassdeck_slang::pack::TYPES_FOLDER;

use crate::library::LibraryError;

/// Which server an editor tab talks to. Each Luau script kind has its own, because each runs with
/// different globals and luau-lsp takes definition files per server, not per file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum LspServer {
    LuauKeyboard,
    LuauHost,
    LuauOptions,
    Slang,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Type)]
#[serde(transparent)]
pub struct LspSession(u32);

/// A started server: its session, the `file://` URI of the folder it works in (documents sit under it),
/// and the settings to answer its `workspace/configuration` requests with, by section.
#[derive(Clone, Debug, Serialize, Type)]
pub struct LspStarted {
    pub session: LspSession,
    pub root: String,
    pub settings: serde_json::Map<String, serde_json::Value>,
}

/// What `server` asks the editor for: slangd finds the `glassdeck` module in `.glassdeck/`.
pub fn settings(server: LspServer, folder: &Path) -> serde_json::Map<String, serde_json::Value> {
    let mut settings = serde_json::Map::new();
    if server == LspServer::Slang {
        let types = folder.join(TYPES_FOLDER).to_string_lossy().into_owned();
        settings.insert("slang.additionalSearchPaths".into(), serde_json::json!([types]));
    }
    settings
}

/// `path` as a `file://` URI, the form LSP names documents and folders by.
pub fn file_uri(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let mut uri = String::from("file://");
    if !text.starts_with('/') {
        uri.push('/');
    }
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' | b':' => uri.push(byte as char),
            other => uri.push_str(&format!("%{other:02X}")),
        }
    }
    uri
}

#[derive(Clone, Debug, PartialEq, Serialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LspError {
    /// The server isn't installed beside the app, or wouldn't start
    Unavailable { reason: String },
    /// The package the editor asked about can't be found in the library
    Library { error: LibraryError },
    /// The session has stopped, or never existed
    NoSession { session: LspSession },
    Io { reason: String },
}

/// One JSON-RPC message from a server.
#[derive(Clone, Debug, Serialize, Type, tauri_specta::Event)]
pub struct LspMessage {
    pub session: LspSession,
    pub message: String,
}

/// A server's process ended; its session is gone.
#[derive(Clone, Debug, Serialize, Type, tauri_specta::Event)]
pub struct LspExited {
    pub session: LspSession,
    pub reason: String,
}

/// What the sessions report, for the app to emit.
#[derive(Clone, Debug)]
pub enum Output {
    Message(LspMessage),
    Exited(LspExited),
}

/// How to run a server: its program, arguments, and the folder it works in.
#[derive(Clone, Debug)]
pub struct Launch {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub folder: PathBuf,
}

/// The `.glassdeck/` definition files a Luau script kind is typed by, the same ones its save-time check uses.
pub fn definitions(server: LspServer, package: PackageType) -> &'static [&'static str] {
    match (server, package) {
        (LspServer::LuauKeyboard, PackageType::Widget) => &["common.d.luau", "keyboard.d.luau"],
        (LspServer::LuauKeyboard, PackageType::Background) => &["canvas.d.luau", "inputs.d.luau"],
        (LspServer::LuauHost, _) => &["options.d.luau", "common.d.luau", "host.d.luau"],
        (LspServer::LuauOptions, _) => &["options-api.d.luau", "options.d.luau", "current.d.luau"],
        (LspServer::Slang, _) => &[],
    }
}

/// The program for `server`, from the native libraries folder, working in the package's `folder`.
/// `.glassdeck/` must already be written there, which checking the package does.
pub fn launch(server: LspServer, package: PackageType, folder: &Path, native: &Path) -> Result<Launch, LspError> {
    let name = match server {
        LspServer::Slang => "slangd",
        _ => "luau-lsp",
    };
    let program = native.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    if !program.is_file() {
        return Err(LspError::Unavailable { reason: format!("{name} isn't installed in {}", native.display()) });
    }
    let mut args: Vec<OsString> = Vec::new();
    if server != LspServer::Slang {
        args.extend(["lsp", "--stdio"].map(OsString::from));
        for file in definitions(server, package) {
            let path = folder.join(TYPES_FOLDER).join(file);
            let stem = file.trim_end_matches(".d.luau");
            let mut arg = OsString::from(format!("--definitions:@{stem}="));
            arg.push(&path);
            args.push(arg);
        }
    }
    Ok(Launch { program, args, folder: folder.to_path_buf() })
}

/// LSP's base protocol: each message is `Content-Length: N`, a blank line, then N bytes of JSON.
pub mod frame {
    use super::*;

    pub fn encode(message: &str) -> Vec<u8> {
        let mut out = format!("Content-Length: {}\r\n\r\n", message.len()).into_bytes();
        out.extend_from_slice(message.as_bytes());
        out
    }

    /// The next message, or `None` when the stream ends between messages.
    pub fn read(reader: &mut impl BufRead) -> io::Result<Option<String>> {
        let mut length = None;
        let mut line = String::new();
        loop {
            line.clear();
            if reader.read_line(&mut line)? == 0 {
                return match length {
                    None => Ok(None),
                    Some(_) => Err(io::Error::new(io::ErrorKind::UnexpectedEof, "the stream ended inside a message's headers")),
                };
            }
            let header = line.trim_end_matches(['\r', '\n']);
            if header.is_empty() {
                break;
            }
            if let Some((name, value)) = header.split_once(':')
                && name.trim().eq_ignore_ascii_case("content-length")
            {
                let parsed = value.trim().parse::<usize>().map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("Content-Length {value:?}: {e}")))?;
                length = Some(parsed);
            }
        }
        let length = length.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "a message without Content-Length"))?;
        let mut body = vec![0; length];
        reader.read_exact(&mut body)?;
        String::from_utf8(body).map(Some).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }
}

struct Running {
    child: Child,
    stdin: ChildStdin,
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Every running server, by session.
pub struct Sessions {
    next: AtomicU32,
    running: Arc<Mutex<HashMap<LspSession, Running>>>,
    out: Arc<dyn Fn(Output) + Send + Sync>,
}

impl Sessions {
    pub fn new(out: impl Fn(Output) + Send + Sync + 'static) -> Self {
        Self { next: AtomicU32::new(1), running: Arc::default(), out: Arc::new(out) }
    }

    /// Starts the server; its messages arrive as `Output::Message` until `Output::Exited`.
    pub fn start(&self, launch: &Launch) -> Result<LspSession, LspError> {
        let mut command = Command::new(&launch.program);
        command.args(&launch.args).current_dir(&launch.folder).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        hide_console(&mut command);
        let mut child = command.spawn().map_err(|e| LspError::Unavailable { reason: format!("starting {}: {e}", launch.program.display()) })?;
        let (Some(stdin), Some(stdout), Some(stderr)) = (child.stdin.take(), child.stdout.take(), child.stderr.take()) else {
            unreachable!("all three pipes were asked for");
        };
        let session = LspSession(self.next.fetch_add(1, Ordering::Relaxed));
        self.lock().insert(session, Running { child, stdin });
        let name = launch.program.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let (running, out) = (Arc::clone(&self.running), Arc::clone(&self.out));
        let spawned = std::thread::Builder::new().name(format!("lsp {name} {}", session.0)).spawn(move || relay(session, stdout, &running, &*out));
        if let Err(e) = spawned {
            self.lock().remove(&session);
            return Err(LspError::Io { reason: format!("starting the reader thread: {e}") });
        }
        let _ = std::thread::Builder::new().name(format!("lsp {name} {} stderr", session.0)).spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                eprintln!("{name} ({}): {line}", session.0);
            }
        });
        Ok(session)
    }

    pub fn send(&self, session: LspSession, message: &str) -> Result<(), LspError> {
        let mut running = self.lock();
        let server = running.get_mut(&session).ok_or(LspError::NoSession { session })?;
        server.stdin.write_all(&frame::encode(message)).and_then(|()| server.stdin.flush()).map_err(|e| LspError::Io { reason: format!("writing to the server: {e}") })
    }

    /// Ends the server; its `Output::Exited` says it was stopped.
    pub fn stop(&self, session: LspSession) -> Result<(), LspError> {
        let stopped = self.lock().remove(&session).ok_or(LspError::NoSession { session })?;
        drop(stopped);
        Ok(())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<LspSession, Running>> {
        self.running.lock().expect("the LSP sessions aren't poisoned")
    }
}

/// Reads the server's messages until its output ends, then reports why.
fn relay(session: LspSession, stdout: impl Read, running: &Mutex<HashMap<LspSession, Running>>, out: &dyn Fn(Output)) {
    let mut reader = BufReader::new(stdout);
    let ended = loop {
        match frame::read(&mut reader) {
            Ok(Some(message)) => out(Output::Message(LspMessage { session, message })),
            Ok(None) => break None,
            Err(e) => break Some(format!("unreadable output: {e}")),
        }
    };
    let still_running = running.lock().expect("the LSP sessions aren't poisoned").remove(&session);
    let reason = match (still_running, ended) {
        (None, _) => "stopped".to_owned(),
        (Some(mut server), problem) => {
            let status = server.child.wait().map(|s| s.to_string()).unwrap_or_else(|e| format!("unknown ({e})"));
            problem.map_or(status.clone(), |p| format!("{p}; {status}"))
        }
    };
    out(Output::Exited(LspExited { session, reason }));
}

#[cfg(windows)]
fn hide_console(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_console(_: &mut Command) {}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;

    #[test]
    fn frames_round_trip_and_tolerate_other_headers() {
        let mut bytes = frame::encode(r#"{"id":1}"#);
        bytes.extend_from_slice(b"Content-Type: application/vscode-jsonrpc; charset=utf-8\r\ncontent-length: 8\r\n\r\n{\"id\":2}");
        let mut reader = io::Cursor::new(bytes);
        assert_eq!(frame::read(&mut reader).unwrap().as_deref(), Some(r#"{"id":1}"#));
        assert_eq!(frame::read(&mut reader).unwrap().as_deref(), Some(r#"{"id":2}"#));
        assert_eq!(frame::read(&mut reader).unwrap(), None, "a clean end between messages");
    }

    #[test]
    fn broken_frames_are_errors() {
        let read = |bytes: &[u8]| frame::read(&mut io::Cursor::new(bytes.to_vec()));
        assert!(read(b"Content-Type: x\r\n\r\n{}").is_err(), "no length");
        assert!(read(b"Content-Length: ten\r\n\r\n").is_err(), "a length that isn't a number");
        assert!(read(b"Content-Length: 10\r\n\r\n{}").is_err(), "a body cut short");
        assert!(read(b"Content-Length: 2\r\n").is_err(), "the stream ends in the headers");
    }

    /// `examples/fake_lsp.rs`, which `cargo test` builds into the profile's `examples` folder above the test binary.
    fn fake_server() -> Launch {
        let name = format!("fake_lsp{}", std::env::consts::EXE_SUFFIX);
        let test_binary = std::env::current_exe().unwrap();
        let program = test_binary
            .ancestors()
            .map(|folder| folder.join("examples").join(&name))
            .find(|candidate| candidate.is_file())
            .unwrap_or_else(|| panic!("no examples/{name} above {}; cargo test builds it", test_binary.display()));
        Launch { program, args: Vec::new(), folder: std::env::temp_dir() }
    }

    fn sessions() -> (Sessions, mpsc::Receiver<Output>) {
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        // A test may finish before a server's last message; that one has nowhere to go
        (Sessions::new(move |output| drop(tx.lock().unwrap().send(output))), rx)
    }

    fn next(rx: &mpsc::Receiver<Output>) -> Output {
        rx.recv_timeout(Duration::from_secs(10)).expect("the server answered")
    }

    #[test]
    fn two_sessions_get_their_own_answers() {
        let (sessions, rx) = sessions();
        let (a, b) = (sessions.start(&fake_server()).unwrap(), sessions.start(&fake_server()).unwrap());
        sessions.send(a, r#"{"to":"a"}"#).unwrap();
        sessions.send(b, r#"{"to":"b"}"#).unwrap();
        let mut answers: Vec<(LspSession, String)> = (0..2)
            .map(|_| match next(&rx) {
                Output::Message(m) => (m.session, m.message),
                other => panic!("expected a message, got {other:?}"),
            })
            .collect();
        answers.sort_by_key(|(s, _)| s.0);
        assert_eq!(answers, vec![(a, r#"{"echo":{"to":"a"}}"#.to_owned()), (b, r#"{"echo":{"to":"b"}}"#.to_owned())]);
    }

    #[test]
    fn a_server_that_exits_is_reported_and_forgotten() {
        let (sessions, rx) = sessions();
        let session = sessions.start(&fake_server()).unwrap();
        sessions.send(session, r#"{"method":"exit"}"#).unwrap();
        match next(&rx) {
            Output::Exited(exited) => {
                assert_eq!(exited.session, session);
                assert!(exited.reason.contains('3'), "the exit code is in {:?}", exited.reason);
            }
            other => panic!("expected an exit, got {other:?}"),
        }
        assert_eq!(sessions.send(session, "{}"), Err(LspError::NoSession { session }));
    }

    #[test]
    fn stopping_a_server_ends_it() {
        let (sessions, rx) = sessions();
        let session = sessions.start(&fake_server()).unwrap();
        sessions.stop(session).unwrap();
        match next(&rx) {
            Output::Exited(exited) => assert_eq!((exited.session, exited.reason.as_str()), (session, "stopped")),
            other => panic!("expected an exit, got {other:?}"),
        }
        assert_eq!(sessions.stop(session), Err(LspError::NoSession { session }));
    }

    /// `target/native`, where `native/build.py` puts the servers.
    fn native() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../target/native")
    }

    /// The next message `matches` accepts, skipping the rest (logs, progress, other notifications).
    fn wait_for(rx: &mpsc::Receiver<Output>, matches: impl Fn(&serde_json::Value) -> bool) -> serde_json::Value {
        loop {
            match rx.recv_timeout(Duration::from_secs(30)).expect("the server kept answering") {
                Output::Message(m) => {
                    let value: serde_json::Value = serde_json::from_str(&m.message).expect("JSON-RPC");
                    if matches(&value) {
                        return value;
                    }
                }
                Output::Exited(e) => panic!("the server exited: {}", e.reason),
            }
        }
    }

    #[test]
    fn luau_lsp_reports_errors_and_completes_from_our_definitions() {
        let native = native();
        let folder = std::env::temp_dir().join(format!("glassdeck-lsp-test-{}", std::process::id()));
        std::fs::create_dir_all(folder.join(TYPES_FOLDER)).unwrap();
        std::fs::write(folder.join(TYPES_FOLDER).join("common.d.luau"), glassdeck_luau_check::definitions::COMMON).unwrap();
        std::fs::write(folder.join(TYPES_FOLDER).join("keyboard.d.luau"), glassdeck_luau_check::definitions::KEYBOARD).unwrap();
        let Ok(launch) = launch(LspServer::LuauKeyboard, PackageType::Widget, &folder, &native) else {
            eprintln!("skipped: luau-lsp isn't built in {} (run native/build.py)", native.display());
            return;
        };
        let (sessions, rx) = sessions();
        let session = sessions.start(&launch).unwrap();
        let root = file_uri(&folder);
        let script = format!("{root}/keyboard.luau");
        let send = |value: serde_json::Value| sessions.send(session, &value.to_string()).unwrap();

        send(serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "processId": null, "rootUri": root, "workspaceFolders": [{ "uri": root, "name": "widget" }],
            "capabilities": { "textDocument": { "publishDiagnostics": {}, "completion": { "completionItem": {} } } }
        }}));
        wait_for(&rx, |v| v["id"] == 1);
        send(serde_json::json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }));
        let text = "--!strict\nlocal count: number = \"three\"\nwidget.\n";
        send(serde_json::json!({ "jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {
            "textDocument": { "uri": script, "languageId": "luau", "version": 1, "text": text }
        }}));
        let published = wait_for(&rx, |v| v["method"] == "textDocument/publishDiagnostics" && v["params"]["diagnostics"].as_array().is_some_and(|d| !d.is_empty()));
        let messages: Vec<&str> = published["params"]["diagnostics"].as_array().unwrap().iter().filter_map(|d| d["message"].as_str()).collect();
        assert!(messages.iter().any(|m| m.contains("string") && m.contains("number")), "a type error for the string: {messages:?}");

        send(serde_json::json!({ "jsonrpc": "2.0", "id": 2, "method": "textDocument/completion", "params": {
            "textDocument": { "uri": script }, "position": { "line": 2, "character": 7 },
            "context": { "triggerKind": 2, "triggerCharacter": "." }
        }}));
        let completion = wait_for(&rx, |v| v["id"] == 2);
        let items = completion["result"]["items"].as_array().or_else(|| completion["result"].as_array()).cloned().unwrap_or_default();
        let labels: Vec<&str> = items.iter().filter_map(|i| i["label"].as_str()).collect();
        assert!(labels.contains(&"on_draw") && labels.contains(&"every"), "the widget API from keyboard.d.luau: {labels:?}");
        sessions.stop(session).unwrap();
        let _ = std::fs::remove_dir_all(folder);
    }

    #[test]
    fn slangd_finds_the_glassdeck_module_through_its_settings() {
        let native = native();
        let folder = std::env::temp_dir().join(format!("glassdeck-slangd-test-{}", std::process::id()));
        std::fs::create_dir_all(folder.join(TYPES_FOLDER)).unwrap();
        std::fs::write(folder.join(TYPES_FOLDER).join("glassdeck.slang"), include_str!("../../../../crates/glassdeck-slang/slang/glassdeck.slang")).unwrap();
        let Ok(launch) = launch(LspServer::Slang, PackageType::Background, &folder, &native) else {
            eprintln!("skipped: slangd isn't built in {} (run native/build.py)", native.display());
            return;
        };
        let settings = settings(LspServer::Slang, &folder);
        let (sessions, rx) = sessions();
        let session = sessions.start(&launch).unwrap();
        let root = file_uri(&folder);
        let canvas = format!("{root}/canvas.slang");
        let send = |value: serde_json::Value| sessions.send(session, &value.to_string()).unwrap();
        // Answers the server's settings requests the way the editor's client does, until `wanted` arrives
        let answer_until = |wanted: &dyn Fn(&serde_json::Value) -> bool| -> serde_json::Value {
            loop {
                let message = wait_for(&rx, |_| true);
                if message["method"] == "workspace/configuration" {
                    let sections = message["params"]["items"].as_array().unwrap().iter().map(|i| settings.get(i["section"].as_str().unwrap_or("")).cloned().unwrap_or(serde_json::Value::Null));
                    send(serde_json::json!({ "jsonrpc": "2.0", "id": message["id"], "result": sections.collect::<Vec<_>>() }));
                } else if message.get("method").is_some() && message.get("id").is_some() {
                    send(serde_json::json!({ "jsonrpc": "2.0", "id": message["id"], "result": null }));
                } else if wanted(&message) {
                    return message;
                }
            }
        };

        send(serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "processId": null, "rootUri": root, "workspaceFolders": [{ "uri": root, "name": "canvas" }],
            "capabilities": { "workspace": { "configuration": true }, "textDocument": { "completion": {} } }
        }}));
        answer_until(&|v| v["id"] == 1);
        send(serde_json::json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }));
        let text = "import glassdeck;\n[Canvas]\nhalf4 c(Pixel p) {\n    \n    return half4(0);\n}\n";
        send(serde_json::json!({ "jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {
            "textDocument": { "uri": canvas, "languageId": "slang", "version": 1, "text": text }
        }}));
        // slangd applies its settings and loads imports in the background, answering from what it has so far
        let mut labels = Vec::new();
        for id in 2..60 {
            send(serde_json::json!({ "jsonrpc": "2.0", "id": id, "method": "textDocument/completion", "params": {
                "textDocument": { "uri": canvas }, "position": { "line": 3, "character": 4 }
            }}));
            let completion = answer_until(&|v| v["id"] == id);
            let items = completion["result"]["items"].as_array().or_else(|| completion["result"].as_array()).cloned().unwrap_or_default();
            labels = items.iter().filter_map(|i| i["label"].as_str().map(str::to_owned)).collect();
            if labels.iter().any(|l| l == "Frame") {
                break;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        assert!(labels.iter().any(|l| l == "Frame") && labels.iter().any(|l| l == "CanvasAttribute"), "the glassdeck module's types: {labels:?}");
        sessions.stop(session).unwrap();
        let _ = std::fs::remove_dir_all(folder);
    }

    #[test]
    fn file_uris_escape_what_they_must() {
        assert_eq!(file_uri(Path::new(r"C:\Users\A B\library\widget.local.user.clock")), "file:///C:/Users/A%20B/library/widget.local.user.clock");
        assert_eq!(file_uri(Path::new("/home/a/x#1")), "file:///home/a/x%231");
    }

    #[test]
    fn a_missing_server_is_unavailable() {
        let missing = std::env::temp_dir().join("glassdeck-no-servers-here");
        assert!(matches!(launch(LspServer::Slang, PackageType::Background, Path::new("."), &missing), Err(LspError::Unavailable { .. })));
    }
}
