//! The `notifications` tool with a service worker notification (`registration.showNotification`): the list shows its buttons,
//! and a click on a button runs `notificationclick` in the service worker.
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

/// A chat page with a service worker. `make(t)` shows a notification with a "reply" button. A click sends the action to the page,
/// which puts it in the title.
fn route(_: &str, target: &str, _: &str, _: Option<&str>) -> (u16, &'static str, Vec<u8>) {
    match target {
        "/sw.js" => (200, "text/javascript", b"self.onnotificationclick = function (e) { e.waitUntil(clients.matchAll({includeUncontrolled: true}).then(function (l) { l[0].postMessage(e.action + ':' + e.notification.title) })) };".to_vec()),
        _ => (200, "text/html", br#"<!doctype html><title>chat</title><body><script>
navigator.serviceWorker.register('/sw.js');
navigator.serviceWorker.onmessage = function (e) { document.title = e.data; };
function make(t) { navigator.serviceWorker.ready.then(function (r) { r.showNotification(t, { body: 'hi', actions: [{ action: 'reply', title: 'Reply' }] }); }); }
</script></body>"#.to_vec()),
    }
}

#[test]
fn an_agent_clicks_a_button_of_a_service_worker_notification() {
    let url = common::serve_route(route);
    let mut s = Server::start();
    s.ok("open", json!({"url": url, "timeout_ms": 3000}));
    s.ok("notifications", json!({"op": "grant"}));
    s.ok("eval", json!({"js": "make('Alice'); 1"}));
    let r = s.ok("notifications", json!({"op": "wait", "timeout_ms": 5000}));
    assert!(r.contains("shown #1 \"Alice\" \"hi\"") && r.contains("live #1 \"Alice\" \"hi\"") && r.contains("action=\"reply\":\"Reply\""), "{r}");
    let (r, err) = s.call("notifications", json!({"op": "click", "id": 1, "action": "nope"}));
    assert!(err && r.contains("action"), "{r}");
    let r = s.ok("notifications", json!({"op": "click", "id": 1, "action": "reply"}));
    assert!(r.contains("clicked #1"), "{r}");
    let r = s.ok("step", json!({"steps": "wait title=\"reply:Alice\""}));
    assert!(r.contains("\"ok\":true"), "{r}");
}
