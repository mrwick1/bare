//! The `notifications` tool: the permission of the page's origin (grant, deny, reset), kept in the profile.
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
    fn eval(&mut self, js: &str) -> String {
        self.call("eval", json!({"js": js})).0
    }
}
impl Drop for Server {
    fn drop(&mut self) { let _ = self.child.kill(); let _ = self.child.wait(); }
}
fn page(_: &str, _: &str, _: &str, _: Option<&str>) -> (u16, &'static str, Vec<u8>) {
    (200, "text/html", b"<!doctype html><body><p>chat</p></body>".to_vec())
}

#[test]
fn grant_deny_and_reset_set_the_permission_of_the_page_origin() {
    let url = common::serve_route(page);
    let mut s = Server::start();
    let (r, err) = s.call("notifications", json!({"op": "grant"}));
    assert!(err && r.contains("no live session"), "{r}");
    let (r, err) = s.call("open", json!({"url": url, "timeout_ms": 2000}));
    assert!(!err, "{r}");
    assert_eq!(s.eval("Notification.permission"), "denied");
    let (r, err) = s.call("notifications", json!({"op": "grant"}));
    assert!(!err && r.contains("granted") && r.contains("http://127.0.0.1"), "{r}");
    assert_eq!(s.eval("Notification.permission"), "granted");
    let (r, err) = s.call("notifications", json!({"op": "deny"}));
    assert!(!err && r.contains("denied"), "{r}");
    assert_eq!(s.eval("Notification.permission"), "denied");
    s.call("notifications", json!({"op": "grant"}));
    let (r, err) = s.call("notifications", json!({"op": "reset"}));
    assert!(!err && r.contains("no decision"), "{r}");
    assert_eq!(s.eval("Notification.permission"), "denied");
    let (r, err) = s.call("notifications", json!({"op": "grant", "origin": "https://other.test"}));
    assert!(!err && r.contains("https://other.test"), "{r}");
    assert_eq!(s.eval("Notification.permission"), "denied");
    let (r, err) = s.call("notifications", json!({"op": "fly"}));
    assert!(err && r.contains("op"), "{r}");
}

#[test]
fn a_grant_stays_in_the_profile_after_close() {
    let url = common::serve_route(page);
    let dir = std::env::temp_dir().join(format!("bare-mcp-notif-profile-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let profile = dir.to_str().unwrap();
    let mut s = Server::start();
    let (r, err) = s.call("open", json!({"url": url, "timeout_ms": 2000, "profile": profile}));
    assert!(!err, "{r}");
    s.call("notifications", json!({"op": "grant"}));
    let (r, err) = s.call("close", json!({}));
    assert!(!err, "{r}");
    assert!(dir.join("permissions.bin").is_file());
    let (r, err) = s.call("open", json!({"url": url, "timeout_ms": 2000, "profile": profile}));
    assert!(!err, "{r}");
    assert_eq!(s.eval("Notification.permission"), "granted");
    s.call("close", json!({}));
    let _ = std::fs::remove_dir_all(&dir);
}
