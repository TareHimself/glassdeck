//! `net`: HTTP for `host.luau`, to the hosts its package lists under `"network"`. A request runs on a
//! thread of its own; the callback that made it waits (see `glassdeck_luau::waiting`) until the answer
//! is handed back on the script's thread by `Net::deliver`.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use anyhow::{Result, anyhow, bail, ensure};
use glassdeck_luau::{LuaResultExt, Print};
use glassdeck_luau::waiting::{WaitId, Waits};
use glassdeck_package::NetworkHost;
use mlua::{Function, Lua, Table, Value};
use ureq::http::Uri;

/// Tells whoever runs the host side that an answer is waiting for `HostSide::run_answers`.
pub type Wake = Arc<dyn Fn() + Send + Sync>;

const MAX_IN_FLIGHT: usize = 4;
const MAX_RESPONSE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_REQUEST_BYTES: usize = 1024 * 1024;
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_TIMEOUT: Duration = Duration::from_secs(30);
/// Plain HTTP only reaches these, for trying a local service; everything else must be HTTPS.
const LOOPBACK: [&str; 3] = ["localhost", "127.0.0.1", "[::1]"];

/// `net.get`, `net.request` and `net.all`, on `start` (Rust) and `wait` (the scheduler's).
const NET_GLUE: &str = r#"
local start, wait = ...
local net = {}
function net.get(url) return wait({ start(url) })[1] end
function net.request(request) return wait({ start(request) })[1] end
function net.all(requests)
    local ids = {}
    for i, request in requests do
        ids[i] = start(request)
    end
    return wait(ids)
end
return net
"#;

/// A request as checked at the call: everything about it is allowed.
#[derive(Debug)]
struct Request {
    method: &'static str,
    url: String,
    headers: Vec<(String, String)>,
    body: Option<Vec<u8>>,
    timeout: Duration,
}

/// What came back, or why nothing did.
#[derive(Debug)]
enum Answer {
    Response { status: u16, headers: BTreeMap<String, String>, body: Vec<u8> },
    Failed(String),
}

/// One host side's network: its allowed hosts, and the answers on their way back.
pub struct Net {
    allowed: Vec<NetworkHost>,
    in_flight: Cell<usize>,
    answers: Sender<(WaitId, Answer)>,
    arrived: Receiver<(WaitId, Answer)>,
    wake: Option<Wake>,
    /// Where each request and how it went is told, as "net: GET api.example.com 200"
    trace: Option<Print>,
    /// Each waiting request, as the trace names it
    waiting: RefCell<HashMap<WaitId, String>>,
}

impl Net {
    pub fn new(allowed: Vec<NetworkHost>, wake: Option<Wake>, trace: Option<Print>) -> Rc<Self> {
        let (answers, arrived) = mpsc::channel();
        Rc::new(Self { allowed, in_flight: Cell::new(0), answers, arrived, wake, trace, waiting: RefCell::default() })
    }

    /// Hands each answer that arrived to the callback waiting on it; the VM resumes them after.
    pub fn deliver(&self, lua: &Lua, waits: &Waits) -> Result<()> {
        while let Ok((id, answer)) = self.arrived.try_recv() {
            self.in_flight.set(self.in_flight.get() - 1);
            let asked = self.waiting.borrow_mut().remove(&id).unwrap_or_default();
            if let Some(trace) = &self.trace {
                match &answer {
                    Answer::Response { status, .. } => trace(format!("net: {asked} {status}")),
                    Answer::Failed(why) => trace(format!("net: {asked} failed: {why}")),
                }
            }
            waits.answer(id, Value::Table(response_table(lua, answer)?));
        }
        Ok(())
    }

    fn start(&self, request: Request, id: WaitId) {
        self.in_flight.set(self.in_flight.get() + 1);
        let host = request.url.parse::<Uri>().ok().and_then(|uri| uri.host().map(str::to_owned)).unwrap_or_default();
        self.waiting.borrow_mut().insert(id, format!("{} {host}", request.method));
        let (answers, wake) = (self.answers.clone(), self.wake.clone());
        let spawned = thread::Builder::new().name("net".into()).spawn(move || {
            let answer = send(&request).unwrap_or_else(|e| Answer::Failed(format!("{e:#}")));
            // The host side may have stopped meanwhile; then nobody wants the answer.
            if answers.send((id, answer)).is_ok()
                && let Some(wake) = wake
            {
                wake();
            }
        });
        if let Err(e) = spawned {
            let _ = self.answers.send((id, Answer::Failed(format!("starting the request: {e}"))));
        }
    }

    /// Checks a request from Luau: a URL string (a GET) or a request table.
    fn parse(&self, value: Value) -> Result<Request> {
        let request = match value {
            Value::String(url) => Request { method: "GET", url: url.to_str().lua_context("reading the URL")?.to_owned(), headers: Vec::new(), body: None, timeout: DEFAULT_TIMEOUT },
            Value::Table(table) => request_from_table(&table)?,
            other => bail!("a request is a URL or a table with url, not a {}", other.type_name()),
        };
        self.check(&request)?;
        Ok(request)
    }

    fn check(&self, request: &Request) -> Result<()> {
        let uri: Uri = request.url.parse().map_err(|e| anyhow!("{} isn't a URL: {e}", request.url))?;
        let host = uri.host().ok_or_else(|| anyhow!("{} names no host", request.url))?.to_ascii_lowercase();
        match uri.scheme_str() {
            Some("https") => {}
            Some("http") if LOOPBACK.contains(&host.as_str()) => {}
            Some("http") => bail!("{} isn't HTTPS; plain HTTP only reaches localhost", request.url),
            _ => bail!("{} isn't an http(s) URL", request.url),
        }
        if !self.allowed.iter().any(|allowed| allowed.matches(&host)) {
            let listed = self.allowed.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ");
            bail!("{host} isn't in deck.json's \"network\" (it lists: {})", if listed.is_empty() { "nothing" } else { &listed });
        }
        ensure!(self.in_flight.get() < MAX_IN_FLIGHT, "{MAX_IN_FLIGHT} requests are already waiting; wait for one first");
        Ok(())
    }
}

fn request_from_table(table: &Table) -> Result<Request> {
    let url: String = table.get::<Option<String>>("url").lua_context("reading url")?.ok_or_else(|| anyhow!("the request has no url"))?;
    let method = match table.get::<Option<String>>("method").lua_context("reading method")?.as_deref() {
        None | Some("GET") => "GET",
        Some("POST") => "POST",
        Some("PUT") => "PUT",
        Some("PATCH") => "PATCH",
        Some("DELETE") => "DELETE",
        Some(other) => bail!("method {other} isn't one of GET, POST, PUT, PATCH, DELETE"),
    };
    let headers = match table.get::<Option<Table>>("headers").lua_context("reading headers")? {
        Some(headers) => headers.pairs::<String, String>().collect::<mlua::Result<Vec<_>>>().lua_context("reading headers (names and values are strings)")?,
        None => Vec::new(),
    };
    let body = table.get::<Option<mlua::LuaString>>("body").lua_context("reading body")?.map(|b| b.as_bytes().to_vec());
    if let Some(body) = &body {
        ensure!(!matches!(method, "GET" | "DELETE"), "a {method} has no body");
        ensure!(body.len() <= MAX_REQUEST_BYTES, "the body is {} bytes, more than the {MAX_REQUEST_BYTES} a request may send", body.len());
    }
    let timeout = match table.get::<Option<f64>>("timeout").lua_context("reading timeout")? {
        None => DEFAULT_TIMEOUT,
        Some(seconds) if seconds > 0.0 && seconds <= MAX_TIMEOUT.as_secs_f64() => Duration::from_secs_f64(seconds),
        Some(seconds) => bail!("timeout {seconds} isn't between 0 and {} seconds", MAX_TIMEOUT.as_secs()),
    };
    Ok(Request { method, url, headers, body, timeout })
}

/// Runs on the request's own thread. Redirects aren't followed: the 3xx comes back as it is.
fn send(request: &Request) -> Result<Answer> {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(request.timeout))
        .http_status_as_error(false)
        .max_redirects(0)
        .build()
        .new_agent();
    let mut builder = ureq::http::Request::builder().method(request.method).uri(&request.url);
    for (name, value) in &request.headers {
        builder = builder.header(name, value);
    }
    let mut response = match &request.body {
        Some(body) => agent.run(builder.body(body.clone())?),
        None => agent.run(builder.body(())?),
    }
    .map_err(|e| anyhow!("{e}"))?;
    let status = response.status().as_u16();
    let mut headers = BTreeMap::new();
    for (name, value) in response.headers() {
        let value = String::from_utf8_lossy(value.as_bytes()).into_owned();
        headers.entry(name.as_str().to_owned()).and_modify(|v: &mut String| *v = format!("{v}, {value}")).or_insert(value);
    }
    let body = response.body_mut().with_config().limit(MAX_RESPONSE_BYTES).read_to_vec().map_err(|e| match e {
        ureq::Error::BodyExceedsLimit(_) => anyhow!("the response is larger than {} MB", MAX_RESPONSE_BYTES / 1024 / 1024),
        other => anyhow!("{other}"),
    })?;
    Ok(Answer::Response { status, headers, body })
}

fn response_table(lua: &Lua, answer: Answer) -> Result<Table> {
    let table = lua.create_table().lua_context("making a response")?;
    let set = |key: &str, value: Value| table.raw_set(key, value).lua_context("making a response");
    match answer {
        Answer::Response { status, headers, body } => {
            set("ok", Value::Boolean((200..300).contains(&status)))?;
            set("status", Value::Integer(i64::from(status)))?;
            let names = lua.create_table().lua_context("making headers")?;
            for (name, value) in headers {
                names.raw_set(name, value).lua_context("making headers")?;
            }
            set("headers", Value::Table(names))?;
            set("body", Value::String(lua.create_string(&body).lua_context("making body")?))?;
        }
        Answer::Failed(why) => {
            set("ok", Value::Boolean(false))?;
            set("status", Value::Integer(0))?;
            set("headers", Value::Table(lua.create_table().lua_context("making headers")?))?;
            set("body", Value::String(lua.create_string("").lua_context("making body")?))?;
            set("error", Value::String(lua.create_string(&why).lua_context("making error")?))?;
        }
    }
    Ok(table)
}

/// The `net` table: requests are checked at the call, so misuse fails where it's written.
pub fn net_api(lua: &Lua, net: &Rc<Net>, waits: &Waits, wait: Function) -> Result<Table> {
    let (started, handle) = (Rc::clone(net), waits.clone());
    let start = lua
        .create_function(move |lua, value: Value| {
            handle.check_can_wait(lua, "net")?;
            let request = started.parse(value).map_err(|e| mlua::Error::runtime(format!("net: {e:#}")))?;
            let id = handle.next_id();
            started.start(request, id);
            Ok(id)
        })
        .lua_context("creating net")?;
    lua.load(NET_GLUE).set_name("net").call((start, wait)).lua_context("creating net")
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::time::Instant;

    use glassdeck_luau::Vm;

    use super::*;

    /// Answers each connection with `200 OK`, echoing the method, path and body as JSON.
    fn echo_server() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                thread::spawn(move || {
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    let mut parts = line.split_whitespace();
                    let (method, path) = (parts.next().unwrap().to_owned(), parts.next().unwrap().to_owned());
                    let mut length = 0;
                    loop {
                        let mut header = String::new();
                        reader.read_line(&mut header).unwrap();
                        if header.trim().is_empty() {
                            break;
                        }
                        if let Some(value) = header.to_ascii_lowercase().strip_prefix("content-length:") {
                            length = value.trim().parse().unwrap();
                        }
                    }
                    let mut body = vec![0; length];
                    reader.read_exact(&mut body).unwrap();
                    let status = if path == "/missing" { "404 Not Found" } else { "200 OK" };
                    let reply = serde_json_like(&method, &path, &String::from_utf8(body).unwrap());
                    let mut stream = stream;
                    write!(stream, "HTTP/1.1 {status}\r\ncontent-type: application/json\r\nx-test: yes\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}", reply.len()).unwrap();
                });
            }
        });
        port
    }

    fn serde_json_like(method: &str, path: &str, body: &str) -> String {
        format!(r#"{{"method":"{method}","path":"{path}","body":"{}"}}"#, body.replace('"', "\\\""))
    }

    fn host(allowed: &[&str]) -> (Vm, Waits, Rc<Net>) {
        let mut vm = Vm::new().unwrap();
        let (waits, wait) = vm.allow_waiting().unwrap();
        let net = Net::new(allowed.iter().map(|h| NetworkHost::parse(h).unwrap()).collect(), None, None);
        let lua = vm.lua();
        lua.globals().set("net", net_api(lua, &net, &waits, wait).unwrap()).unwrap();
        lua.globals().set("json", glassdeck_luau::json::json_api(lua).unwrap()).unwrap();
        vm.seal().unwrap();
        (vm, waits, net)
    }

    /// Delivers answers until no callback waits, or fails after a few seconds.
    fn settle(vm: &Vm, waits: &Waits, net: &Net) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while vm.is_waiting() {
            assert!(Instant::now() < deadline, "no answer came");
            thread::sleep(Duration::from_millis(5));
            net.deliver(vm.lua(), waits).unwrap();
            vm.resume_answered().unwrap();
        }
    }

    fn run(vm: &Vm, source: &str) {
        vm.load::<()>(&format!("function go() {source} end"), "t").unwrap();
        vm.run(&vm.lua().globals().get::<Function>("go").unwrap(), ()).unwrap();
    }

    #[test]
    fn a_callback_waits_for_its_response_and_all_runs_requests_together() {
        let port = echo_server();
        let (vm, waits, net) = host(&["127.0.0.1"]);
        let base = format!("http://127.0.0.1:{port}");
        run(&vm, &format!(
            r#"local r = net.get("{base}/one")
            got = r.status .. " " .. json.decode(r.body).path .. " " .. r.headers["x-test"]
            local posted = net.request({{ method = "POST", url = "{base}/two", body = json.encode({{ n = 1 }}) }})
            body = json.decode(posted.body).body
            local both = net.all({{ "{base}/a", "{base}/missing" }})
            all = json.decode(both[1].body).path .. " " .. tostring(both[2].ok) .. " " .. both[2].status"#
        ));
        settle(&vm, &waits, &net);
        let read = |name: &str| vm.lua().globals().get::<String>(name).unwrap();
        assert_eq!(read("got"), "200 /one yes");
        assert_eq!(read("body"), r#"{"n":1}"#);
        assert_eq!(read("all"), "/a false 404");
    }

    #[test]
    fn unlisted_hosts_plain_http_and_bad_requests_fail_at_the_call() {
        let (vm, _, _) = host(&["api.example.com"]);
        let error = |source: &str| {
            vm.load::<()>(&format!("function go() {source} end"), "t").unwrap();
            format!("{:#}", vm.run(&vm.lua().globals().get::<Function>("go").unwrap(), ()).unwrap_err())
        };
        assert!(error(r#"net.get("https://evil.example.org/")"#).contains("isn't in deck.json's \"network\" (it lists: api.example.com)"));
        assert!(error(r#"net.get("http://api.example.com/")"#).contains("isn't HTTPS"));
        assert!(error(r#"net.request({ url = "https://api.example.com/", body = "x" })"#).contains("a GET has no body"));
        assert!(error(r#"net.request({ method = "TRACE", url = "https://api.example.com/" })"#).contains("isn't one of"));
        let top = format!("{:#}", vm.load::<Value>(r#"return net.get("https://api.example.com/")"#, "t").unwrap_err());
        assert!(top.contains("only works inside a callback"), "{top}");
    }

    #[test]
    fn no_answer_comes_back_as_a_failed_response() {
        let closed = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let (vm, waits, net) = host(&["127.0.0.1"]);
        run(&vm, &format!(r#"local r = net.get("http://127.0.0.1:{closed}/") failed = tostring(r.ok) .. " " .. r.status .. " " .. tostring(r.error ~= nil)"#));
        settle(&vm, &waits, &net);
        assert_eq!(vm.lua().globals().get::<String>("failed").unwrap(), "false 0 true");
    }
}
