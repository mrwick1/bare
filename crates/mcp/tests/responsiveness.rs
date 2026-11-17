#![cfg(target_os = "linux")]
//! Responsiveness of `bare-mcp` over real stdio: a slow call in one session must not block another session or the control path
//! (`ping`, `tools/list`), calls of one session stay in order, and `notifications/cancelled` reaches the request.
//! The slow call is a page that the fixture server holds until the test releases it. The test waits for an event (the request reached the
//! server), never for a fixed time; the only clocks are deadlines that turn a hang into a failure.

#[path = "../../drive/tests/common/mod.rs"]
mod common;

use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Mutex;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

/// A request that came to `/slow/NAME` (arrived) and whether the test let it go (released).
static GATES: Mutex<Option<HashMap<String, (bool, bool)>>> = Mutex::new(None);

fn gate(name: &str, f: impl FnOnce(&mut (bool, bool))) -> (bool, bool) {
    let mut g = GATES.lock().unwrap();
    let e = g.get_or_insert_with(HashMap::new).entry(name.to_string()).or_default();
    f(e);
    *e
}

fn route(_: &str, target: &str, _: &str, _: Option<&str>) -> (u16, &'static str, Vec<u8>) {
    let path = target.split('?').next().unwrap_or("/");
    if let Some(name) = path.strip_prefix("/slow/") {
        gate(name, |g| g.0 = true);
        let end = Instant::now() + Duration::from_secs(60);
        while !gate(name, |_| {}).1 && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    (200, "text/html", b"<!DOCTYPE html><title>ok</title><button>Go</button>".to_vec())
}

fn wait_arrived(name: &str) {
    let end = Instant::now() + Duration::from_secs(20);
    while !gate(name, |_| {}).0 {
        assert!(Instant::now() < end, "the request to /slow/{name} never arrived");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn release(name: &str) {
    gate(name, |g| g.1 = true);
}

/// A client that never blocks on a read: a thread reads the lines, and `reply` waits for the line of one request id.
struct Client {
    child: Child,
    stdin: Option<ChildStdin>,
    rx: Receiver<Value>,
    seen: Vec<Value>,
    id: u64,
}

impl Client {
    fn start() -> Client {
        Client::start_with(&[])
    }

    fn start_with(env: &[(&str, &str)]) -> Client {
        let mut c = Command::new(env!("CARGO_BIN_EXE_bare-mcp"));
        c.stdin(Stdio::piped()).stdout(Stdio::piped());
        for (k, v) in env {
            c.env(k, v);
        }
        let mut child = c.spawn().unwrap();
        let (stdin, out) = (child.stdin.take().unwrap(), BufReader::new(child.stdout.take().unwrap()));
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for l in out.lines().map_while(Result::ok) {
                let _ = tx.send(serde_json::from_str::<Value>(&l).unwrap_or_else(|e| panic!("not JSON ({e}): {l}")));
            }
        });
        let mut c = Client { child, stdin: Some(stdin), rx, seen: Vec::new(), id: 0 };
        let id = c.send("initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "0"}}));
        c.reply(id);
        c.notify("notifications/initialized", json!({}));
        c
    }

    fn write(&mut self, v: Value) {
        writeln!(self.stdin.as_mut().unwrap(), "{v}").unwrap();
    }

    fn send(&mut self, method: &str, params: Value) -> u64 {
        self.id += 1;
        let id = self.id;
        self.write(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        id
    }

    fn call(&mut self, name: &str, args: Value) -> u64 {
        self.send("tools/call", json!({"name": name, "arguments": args}))
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.write(json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    fn cancel(&mut self, id: u64) {
        self.notify("notifications/cancelled", json!({"requestId": id, "reason": "test"}));
    }

    /// The reply of request `id`, within 20 s. Other replies wait in `seen`.
    fn try_reply(&mut self, id: u64, within: Duration) -> Option<Value> {
        let end = Instant::now() + within;
        loop {
            if let Some(i) = self.seen.iter().position(|v| v["id"] == id) {
                return Some(self.seen.remove(i));
            }
            match self.rx.recv_timeout(end.saturating_duration_since(Instant::now())) {
                Ok(v) => self.seen.push(v),
                Err(RecvTimeoutError::Timeout) => return None,
                Err(RecvTimeoutError::Disconnected) => panic!("the server closed its output while waiting for id {id}; seen {:?}", self.seen),
            }
        }
    }

    fn reply(&mut self, id: u64) -> Value {
        self.try_reply(id, Duration::from_secs(20)).unwrap_or_else(|| panic!("no reply for id {id}: the server is blocked"))
    }

    fn text(&mut self, id: u64) -> (String, bool) {
        let r = self.reply(id);
        (r["result"]["content"][0]["text"].as_str().unwrap_or("").to_string(), r["result"]["isError"] == true)
    }

    fn tool(&mut self, name: &str, args: Value) -> (String, bool) {
        let id = self.call(name, args);
        self.text(id)
    }

    /// A ping that gets its reply: the control path works.
    fn ping(&mut self) {
        let id = self.send("ping", json!({}));
        assert_eq!(self.reply(id)["result"], json!({}));
    }

    fn open(&mut self, session: &str, base: &str) {
        let (t, e) = self.tool("open", json!({"url": format!("{base}/ok"), "session": session, "timeout_ms": 30000}));
        assert!(!e, "{t}");
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The ids of the processes whose parent is `pid`, with their command names.
fn children(pid: u32) -> Vec<(u32, String)> {
    let mut out = Vec::new();
    for e in std::fs::read_dir("/proc").unwrap().flatten() {
        let Some(p) = e.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) else { continue };
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{p}/stat")) else { continue };
        // `pid (comm) state ppid ...`: the name may hold spaces, so cut at the last `)`.
        let Some((head, rest)) = stat.rsplit_once(')') else { continue };
        if rest.split_whitespace().nth(1).and_then(|s| s.parse::<u32>().ok()) == Some(pid) {
            out.push((p, head.split_once('(').map_or("", |h| h.1).to_string()));
        }
    }
    out
}

fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(20);
    while !f() {
        assert!(Instant::now() < end, "timed out: {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn a_slow_call_in_one_session_does_not_block_another_session_or_the_control_path() {
    let base = common::serve_route(route);
    let mut c = Client::start();
    c.open("a", &base);
    c.open("b", &base);
    let slow = c.call("act", json!({"session": "a", "op": "goto", "url": format!("{base}/slow/block")}));
    wait_arrived("block");
    // The control path and the other session answer while session a waits for the page.
    c.ping();
    let id = c.send("tools/list", json!({}));
    assert!(c.reply(id)["result"]["tools"].is_array());
    let (t, e) = c.tool("snapshot", json!({"session": "b"}));
    assert!(!e && t.contains("Go"), "{t}");
    // A reply of the slow call is not there yet: the other replies came first and out of order.
    assert!(c.try_reply(slow, Duration::from_millis(1)).is_none());
    release("block");
    let (t, e) = c.text(slow);
    assert!(!e, "{t}");
}

#[test]
fn calls_of_one_session_run_in_order_and_each_gets_one_reply() {
    let base = common::serve_route(route);
    let mut c = Client::start();
    c.open("a", &base);
    let first = c.call("act", json!({"session": "a", "op": "goto", "url": format!("{base}/slow/order")}));
    wait_arrived("order");
    let second = c.call("step", json!({"session": "a", "steps": "cookie set order 1", "compact": true}));
    let third = c.call("storage", json!({"session": "a", "area": "cookies"}));
    // Nothing of session a is answered while its first call runs.
    assert!(c.try_reply(second, Duration::from_millis(200)).is_none());
    release("order");
    assert!(!c.text(first).1);
    assert!(!c.text(second).1);
    let (t, e) = c.text(third);
    assert!(!e && t.contains("order=1"), "the third call ran before the second: {t}");
    assert!(c.seen.is_empty(), "a reply came twice or without a request: {:?}", c.seen);
}

#[test]
fn a_cancelled_call_stops_at_its_wait_and_gets_no_reply() {
    let base = common::serve_route(route);
    let mut c = Client::start();
    c.open("a", &base);
    // The time limit (30 s) is longer than the whole test: only the cancellation can end the wait in time.
    let slow = c.call("act", json!({"session": "a", "op": "goto", "url": format!("{base}/slow/wait")}));
    wait_arrived("wait");
    c.cancel(slow);
    // The session answers its next call, so the cancelled one has ended; it gets no reply.
    // Within 5 s: a wait of the page that cancellation does not reach (the load wait is 10 s) would show here.
    let id = c.call("console", json!({"session": "a", "level": "all"}));
    let r = c.try_reply(id, Duration::from_secs(5)).expect("the session is still busy 5 s after the cancel");
    assert!(r["result"]["isError"] != true, "{r}");
    assert!(c.try_reply(slow, Duration::from_millis(100)).is_none(), "a cancelled request got a reply");
    release("wait");
    c.ping();
}

#[test]
fn a_call_cancelled_while_queued_never_runs() {
    let base = common::serve_route(route);
    let mut c = Client::start();
    c.open("a", &base);
    let first = c.call("act", json!({"session": "a", "op": "goto", "url": format!("{base}/slow/queued")}));
    wait_arrived("queued");
    let queued = c.call("step", json!({"session": "a", "steps": "cookie set ghost 1", "compact": true}));
    let after = c.call("storage", json!({"session": "a", "area": "cookies"}));
    c.cancel(queued);
    // The cancel is in the pipe before this ping: when the ping answers, the cancel is read.
    c.ping();
    release("queued");
    assert!(!c.text(first).1);
    let (t, e) = c.text(after);
    assert!(!e && !t.contains("ghost"), "a cancelled call ran: {t}");
    assert!(c.try_reply(queued, Duration::from_millis(100)).is_none());
}

#[test]
fn cancelling_an_unknown_or_finished_request_changes_nothing() {
    let base = common::serve_route(route);
    let mut c = Client::start();
    c.open("a", &base);
    c.cancel(9999);
    c.notify("notifications/cancelled", json!({}));
    c.notify("notifications/cancelled", json!({"requestId": {"x": 1}}));
    let id = c.call("sessions", json!({}));
    let (t, e) = c.text(id);
    assert!(!e && t.starts_with("a: "), "{t}");
    // A cancel that arrives after the reply is ignored.
    c.cancel(id);
    let (t, e) = c.tool("snapshot", json!({"session": "a"}));
    assert!(!e && t.contains("Go"), "{t}");
}

#[test]
fn close_while_calls_are_queued_ends_the_session_after_the_running_call() {
    let base = common::serve_route(route);
    let mut c = Client::start();
    c.open("a", &base);
    let first = c.call("act", json!({"session": "a", "op": "goto", "url": format!("{base}/slow/close")}));
    wait_arrived("close");
    let close = c.call("close", json!({"session": "a"}));
    let later = c.call("snapshot", json!({"session": "a"}));
    c.ping();
    release("close");
    assert!(!c.text(first).1, "the running call must end before the close");
    assert!(!c.text(close).1);
    let (t, e) = c.text(later);
    assert!(e && t.contains("no live session a"), "{t}");
    // The name is free again.
    c.open("a", &base);
}

#[test]
fn sessions_cookies_stay_apart_while_both_run() {
    let base = common::serve_route(route);
    let mut c = Client::start();
    c.open("a", &base);
    c.open("b", &base);
    assert!(!c.tool("step", json!({"session": "a", "steps": "cookie set who a", "compact": true})).1);
    assert!(!c.tool("step", json!({"session": "b", "steps": "cookie set who b", "compact": true})).1);
    let (a, _) = c.tool("storage", json!({"session": "a", "area": "cookies"}));
    let (b, _) = c.tool("storage", json!({"session": "b", "area": "cookies"}));
    assert!(a.contains("who=a") && !a.contains("who=b"), "{a}");
    assert!(b.contains("who=b") && !b.contains("who=a"), "{b}");
}

/// A suite whose only worker program is a script that sleeps: the worker is a child process of the server that the test can count.
fn sleeper_suite(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("bare-mcp-resp-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("tests")).unwrap();
    std::fs::write(dir.join("tests/a.steps"), "# own\nopen http://127.0.0.1:1/\n").unwrap();
    let exe = dir.join("worker.sh");
    std::fs::write(&exe, "#!/bin/sh\nexec sleep 300\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    (dir, exe)
}

#[test]
fn cancelling_a_suite_ends_and_reaps_its_workers_and_leaves_live_sessions() {
    let base = common::serve_route(route);
    let (dir, exe) = sleeper_suite("cancel");
    let mut c = Client::start_with(&[("BARE_DRIVE", exe.to_str().unwrap())]);
    c.open("a", &base);
    let pid = c.child.id();
    let suite = c.call("run_suite", json!({"dir": dir.join("tests").to_str().unwrap(), "workers": 1, "test_timeout_ms": 120000}));
    wait_for("a suite worker starts", || children(pid).iter().any(|(_, n)| n == "sleep"));
    // The control path and the live session answer while the suite runs.
    c.ping();
    assert!(!c.tool("snapshot", json!({"session": "a"})).1);
    c.cancel(suite);
    wait_for("the suite workers end and are reaped", || children(pid).is_empty());
    assert!(c.try_reply(suite, Duration::from_millis(100)).is_none(), "a cancelled suite got a reply");
    assert!(!c.tool("snapshot", json!({"session": "a"})).1);
}

#[test]
fn the_end_of_input_stops_the_server_and_its_suite_workers() {
    let (dir, exe) = sleeper_suite("eof");
    let mut c = Client::start_with(&[("BARE_DRIVE", exe.to_str().unwrap())]);
    let pid = c.child.id();
    c.call("run_suite", json!({"dir": dir.join("tests").to_str().unwrap(), "workers": 1, "test_timeout_ms": 120000}));
    wait_for("a suite worker starts", || children(pid).iter().any(|(_, n)| n == "sleep"));
    let workers = children(pid);
    drop(c.stdin.take());
    let end = Instant::now() + Duration::from_secs(20);
    loop {
        if c.child.try_wait().unwrap().is_some() {
            break;
        }
        assert!(Instant::now() < end, "the server did not exit at the end of its input");
        std::thread::sleep(Duration::from_millis(10));
    }
    for (p, _) in workers {
        wait_for("a suite worker ends with the server", || !std::path::Path::new(&format!("/proc/{p}")).exists());
    }
}

#[test]
fn queued_close_reopen_and_later_calls_keep_their_order() {
    let base = common::serve_route(route);
    let mut c = Client::start();
    c.open("a", &base);
    let first = c.call("act", json!({"session": "a", "op": "goto", "url": format!("{base}/slow/reopen")}));
    wait_arrived("reopen");
    let close = c.call("close", json!({"session": "a"}));
    let reopen = c.call("open", json!({"session": "a", "url": format!("{base}/ok")}));
    let write = c.call("step", json!({"session": "a", "steps": "cookie set reopened yes", "compact": true}));
    let read = c.call("storage", json!({"session": "a", "area": "cookies"}));
    c.ping();
    release("reopen");
    for id in [first, close, reopen, write] {
        let (text, error) = c.text(id);
        assert!(!error, "{text}");
    }
    let (text, error) = c.text(read);
    assert!(!error && text.contains("reopened=yes"), "{text}");
}
