//! The `notifications` tool for an agent: `op=background` puts the page in the background and brings it back.
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

/// A chat page that notifies only when it is hidden, as many chat pages do.
fn page(_: &str, _: &str, _: &str, _: Option<&str>) -> (u16, &'static str, Vec<u8>) {
    (200, "text/html", br#"<!doctype html><title>chat</title><body><script>
var seen = [];
document.addEventListener('visibilitychange', function () { seen.push(document.visibilityState); });
function message(t) { if (document.hidden) new Notification(t); }
</script></body>"#.to_vec())
}

#[test]
fn background_hides_the_page_and_a_click_on_a_notification_brings_it_back() {
    let url = common::serve_route(page);
    let mut s = Server::start();
    s.ok("open", json!({"url": url, "timeout_ms": 3000}));
    s.ok("notifications", json!({"op": "grant"}));
    let state = "document.visibilityState + ':' + document.hasFocus()";
    assert_eq!(s.ok("eval", json!({"js": state})), "visible:true");
    s.ok("eval", json!({"js": "message('front'); 1"}));
    let r = s.ok("notifications", json!({"op": "background"}));
    assert!(r.starts_with("background on"), "{r}");
    assert_eq!(s.ok("eval", json!({"js": state})), "hidden:false");
    s.ok("eval", json!({"js": "message('Alice'); 1"}));
    let r = s.ok("notifications", json!({"op": "wait", "timeout_ms": 3000}));
    assert!(r.contains("live #1 \"Alice\"") && !r.contains("front"), "{r}");
    assert_eq!(s.ok("notifications", json!({"op": "click", "id": 1})), "clicked #1");
    assert_eq!(s.ok("eval", json!({"js": state.to_owned() + " + ':' + seen.join()"})), "visible:true:hidden,visible");
    s.ok("notifications", json!({"op": "background", "on": true}));
    let r = s.ok("notifications", json!({"op": "background", "on": false}));
    assert!(r.starts_with("background off"), "{r}");
    assert_eq!(s.ok("eval", json!({"js": state})), "visible:true");
}
