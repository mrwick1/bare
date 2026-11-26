//! The `notifications` tool for an agent: `list`, `wait`, `click` and `close`, the `act` receipt line and the step
//! `expect notification`.
#[path = "../../drive/tests/common/mod.rs"]
mod common;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

struct Server { child: Child, input: ChildStdin, output: BufReader<ChildStdout>, id: u64 }
impl Server {
    fn start() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_bare-mcp")).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
        let input = child.stdin.take().unwrap();
        let output = BufReader::new(child.stdout.take().unwrap());
        Self { child, input, output, id: 0 }
    }
    /// The text of the reply and whether it is an error.
    fn call(&mut self, name: &str, arguments: Value) -> (String, bool) {
        self.id += 1;
        writeln!(self.input, "{}", json!({"jsonrpc":"2.0", "id":self.id, "method":"tools/call", "params":{"name":name,"arguments":arguments}})).unwrap();
        let mut line = String::new();
        self.output.read_line(&mut line).unwrap();
        let reply: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(reply["id"], self.id, "{reply}");
        let r = &reply["result"];
        (r["content"][0]["text"].as_str().unwrap_or_default().to_string(), r["isError"] == true)
    }
    fn ok(&mut self, name: &str, arguments: Value) -> String {
        let (r, err) = self.call(name, arguments);
        assert!(!err, "{r}");
        r
    }
}
impl Drop for Server {
    fn drop(&mut self) { let _ = self.child.kill(); let _ = self.child.wait(); }
}

/// A chat page: the button makes a notification at once; `later(t, ms)` makes one from a timer. A click on a notification sets the
/// title and calls `window.focus()`.
fn page(_: &str, _: &str, _: &str, _: Option<&str>) -> (u16, &'static str, Vec<u8>) {
    (200, "text/html", br#"<!doctype html><title>chat</title><body><button onclick="make('Alice', 'hi')">Send</button><script>
function make(t, b) { var n = new Notification(t, { body: b, tag: t }); n.onclick = function () { document.title = 'clicked:' + t; window.focus(); }; }
function later(t, ms) { setTimeout(function () { make(t, 'later'); }, ms); }
</script></body>"#.to_vec())
}

#[test]
fn an_agent_sees_waits_for_clicks_and_closes_notifications() {
    let url = common::serve_route(page);
    let mut s = Server::start();
    s.ok("open", json!({"url": url, "timeout_ms": 3000}));
    s.ok("notifications", json!({"op": "grant"}));
    let r = s.ok("act", json!({"op": "click", "loc": "role=button name=\"Send\"", "snapshot": false}));
    assert!(r.contains("notification: #1 \"Alice\" \"hi\""), "{r}");
    let r = s.ok("notifications", json!({}));
    assert!(r.contains("shown #1 \"Alice\" \"hi\"") && r.contains("live #1 \"Alice\" \"hi\""), "{r}");
    let r = s.ok("notifications", json!({"op": "list"}));
    assert!(!r.contains("shown #1") && r.contains("live #1"), "the events were read: {r}");
    s.ok("eval", json!({"js": "later('Bob', 300); 1"}));
    let r = s.ok("notifications", json!({"op": "wait", "timeout_ms": 5000}));
    assert!(r.contains("shown #2 \"Bob\" \"later\""), "{r}");
    let r = s.ok("notifications", json!({"op": "click", "id": 2}));
    assert!(r.contains("clicked #2"), "{r}");
    assert_eq!(s.ok("eval", json!({"js": "document.title"})), "clicked:Bob");
    s.ok("notifications", json!({"op": "close", "id": 1}));
    let r = s.ok("notifications", json!({"op": "list"}));
    assert!(r.contains("closed #1") && !r.contains("live #1") && r.contains("live #2"), "{r}");
    let (r, err) = s.call("notifications", json!({"op": "click", "id": 7}));
    assert!(err && r.contains("#7"), "{r}");
    let (r, err) = s.call("notifications", json!({"op": "wait", "timeout_ms": 200}));
    assert!(err && r.contains("no notification"), "{r}");
}

#[test]
fn a_step_waits_for_a_notification_that_matches() {
    let url = common::serve_route(page);
    let mut s = Server::start();
    s.ok("open", json!({"url": url, "timeout_ms": 3000}));
    s.ok("notifications", json!({"op": "grant"}));
    s.ok("eval", json!({"js": "later('Carol', 200); 1"}));
    let r = s.ok("step", json!({"steps": "wait notification title~\"carol\" body=\"later\""}));
    assert!(r.contains("\"ok\":true"), "{r}");
    let r = s.ok("step", json!({"steps": "timeout 300\nexpect notification title=\"Dave\""}));
    assert!(r.contains("\"ok\":false") && r.contains("no live notification title=\\\"Dave\\\""), "{r}");
}
