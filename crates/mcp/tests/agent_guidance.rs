//! The examples of `docs/agent-browser-guide.md`, run against a loopback fixture (`tools/fixtures/agent-guide/index.html`) with synthetic data.
//! Each test checks evidence in a result, not the words of the guide.

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

struct Server {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    id: u64,
}

impl Server {
    fn start() -> (Server, Value) {
        let mut child = Command::new(env!("CARGO_BIN_EXE_bare-mcp")).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
        let (stdin, stdout) = (child.stdin.take().unwrap(), BufReader::new(child.stdout.take().unwrap()));
        let mut s = Server { child, stdin, stdout, id: 0 };
        let init = s.ask("initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}));
        (s, init)
    }

    fn ask(&mut self, method: &str, params: Value) -> Value {
        self.id += 1;
        writeln!(self.stdin, "{}", json!({"jsonrpc": "2.0", "id": self.id, "method": method, "params": params})).unwrap();
        let mut line = String::new();
        self.stdout.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    }

    /// Call a tool. Gives the reply (JSON when it parses, else the text) and whether it was an error.
    fn tool(&mut self, name: &str, args: Value) -> (Value, bool) {
        let r = self.ask("tools/call", json!({"name": name, "arguments": args}));
        let text = r["result"]["content"][0]["text"].as_str().unwrap().to_string();
        (serde_json::from_str(&text).unwrap_or(Value::String(text)), r["result"]["isError"] == true)
    }

    /// Call a tool that answers with text. Panics with the reply when the call is an error.
    fn text(&mut self, name: &str, args: Value) -> String {
        let (r, error) = self.tool(name, args);
        assert!(!error, "{name} failed: {r}");
        r.as_str().map(str::to_string).unwrap_or_else(|| r.to_string())
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Serves the fixture page at every path on loopback. Gives the base URL.
fn fixture() -> String {
    let page = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../tools/fixtures/agent-guide/index.html")).unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for mut c in listener.incoming().flatten() {
            let page = page.clone();
            std::thread::spawn(move || {
                let (mut head, mut b) = (Vec::new(), [0u8; 1]);
                while !head.ends_with(b"\r\n\r\n") && c.read(&mut b).unwrap_or(0) == 1 {
                    head.push(b[0]);
                }
                let _ = write!(c, "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{page}", page.len());
            });
        }
    });
    format!("http://127.0.0.1:{port}/")
}

/// The ref (`e7`) of the first snapshot line that holds `what`.
fn ref_of(lines: &str, what: &str) -> String {
    let l = lines.lines().find(|l| l.trim_start().starts_with("[e") && l.contains(what)).unwrap_or_else(|| panic!("no line with {what} in:\n{lines}"));
    l[l.find("[e").unwrap() + 1..l.find(']').unwrap()].to_string()
}

#[test]
fn initialize_teaches_the_loop_and_names_no_report_route() {
    let (_s, init) = Server::start();
    let text = init["result"]["instructions"].as_str().unwrap();
    for tool in ["open", "snapshot", "find", "act", "read", "step", "ensure_session"] {
        assert!(text.contains(tool), "the recipe does not name {tool}");
    }
    // The host that runs the server gives the report route, not the server.
    assert!(!text.contains("GitHub") && !text.contains("report_issue"), "{text}");
}

/// Every tool and argument that the guide uses exists in `tools/list`.
#[test]
fn the_tools_and_arguments_of_the_guide_exist() {
    let (mut s, _) = Server::start();
    let tools = s.ask("tools/list", json!({}))["result"]["tools"].clone();
    let has = |tool: &str, args: &[&str]| {
        let t = tools.as_array().unwrap().iter().find(|t| t["name"] == tool).unwrap_or_else(|| panic!("no tool {tool}"));
        for a in args {
            assert!(t["inputSchema"]["properties"].get(a).is_some(), "{tool} has no argument {a}");
        }
    };
    has("open", &["url", "session", "state", "fresh", "snapshot", "report", "downloads_dir"]);
    has("snapshot", &["session", "mode", "root", "offset", "max_chars"]);
    has("find", &["role", "name", "label", "text", "placeholder", "css", "exact"]);
    has("act", &["op", "ref", "loc", "text", "key", "options", "files", "dialog", "prompt_text", "snapshot", "diff", "session"]);
    has("step", &["steps", "compact", "session"]);
    has("read", &["ref", "offset", "max_chars"]);
    has("links", &["text", "root", "external"]);
    has("screenshot", &["ref", "full_page", "path", "inline"]);
    has("ensure_session", &["name", "url", "login", "check", "file"]);
    has("save_state", &["name", "file", "session"]);
    has("close", &["session"]);
    has("downloads", &["op", "id"]);
    has("diagnose", &["session"]);
    // Check the arguments used here; additive tool arguments must remain compatible.
}

/// open, find, act by ref, read: the basic loop, with an overlay in the way.
#[test]
fn open_find_act_read_and_an_overlay_is_closed_before_the_page_is_used() {
    let (mut s, _) = Server::start();
    let url = fixture();
    let snap = s.text("open", json!({"url": url, "snapshot": true}));
    assert!(snap.contains("Accept notice"), "{snap}");
    let found = s.text("find", json!({"role": "button", "name": "Accept notice"}));
    let accept = ref_of(&found, "Accept notice");
    s.text("act", json!({"op": "click", "ref": accept}));
    // The evidence is the page, not the receipt: the overlay is gone.
    let page = s.text("read", json!({}));
    assert!(page.contains("Synthetic shop") && !page.contains("We use synthetic cookies"), "{page}");
}

/// Replace a field with `type`, select, check, submit with Enter, and verify with `step expect`.
#[test]
fn a_form_is_filled_and_the_result_is_verified_with_step_expect() {
    let (mut s, _) = Server::start();
    s.text("open", json!({"url": fixture()}));
    s.text("act", json!({"op": "click", "loc": "role=button name=\"Accept notice\""}));
    s.text("act", json!({"op": "type", "loc": "label=\"Nickname\"", "text": "Ada"}));
    s.text("act", json!({"op": "select", "loc": "label=\"Colour\"", "text": "Blue"}));
    s.text("act", json!({"op": "check", "loc": "label=\"Terms\""}));
    s.text("act", json!({"op": "press", "loc": "label=\"Nickname\"", "key": "Enter"}));
    let (rep, error) = s.tool("step", json!({"steps": "expect label=\"Nickname\" value~\"Ada\"\nexpect label=\"Terms\" checked\nexpect text=\"Saved Ada\" visible"}));
    assert!(!error && rep["ok"] == true, "{rep}");
}

/// A result that arrives later: wait with `expect`, do not read once and conclude.
#[test]
fn a_delayed_result_is_awaited_with_expect() {
    let (mut s, _) = Server::start();
    s.text("open", json!({"url": fixture()}));
    s.text("act", json!({"op": "click", "loc": "role=button name=\"Accept notice\""}));
    s.text("act", json!({"op": "click", "loc": "role=button name=\"Load report\""}));
    let (rep, error) = s.tool("step", json!({"steps": "expect text=\"Report ready: 3 rows\" visible"}));
    assert!(!error && rep["ok"] == true, "{rep}");
    // A failed expect is an error, and the session stays open for the next step.
    let (rep, error) = s.tool("step", json!({"steps": "timeout 500\nexpect text=\"Report ready: 9 rows\" visible"}));
    assert!(error || rep["ok"] == false, "{rep}");
    let page = s.text("read", json!({}));
    assert!(page.contains("Report ready: 3 rows"), "{page}");
}

/// A named session runs beside `main`; a new `open` of `main` does not end it.
#[test]
fn a_named_second_session_runs_beside_main() {
    let (mut s, _) = Server::start();
    let url = fixture();
    s.text("open", json!({"url": url, "session": "main"}));
    s.text("open", json!({"url": format!("{url}?second"), "session": "other"}));
    let list = s.text("sessions", json!({}));
    assert!(list.contains("main") && list.contains("other"), "{list}");
    s.text("act", json!({"op": "click", "loc": "role=button name=\"Accept notice\"", "session": "other"}));
    let other = s.text("read", json!({"session": "other"}));
    let main = s.text("read", json!({"session": "main"}));
    assert!(!other.contains("We use synthetic cookies"), "{other}");
    assert!(main.contains("We use synthetic cookies"), "{main}");
    let closed = s.text("close", json!({"session": "other"}));
    assert!(!closed.is_empty());
    assert!(!s.text("sessions", json!({})).contains("other"));
}

/// The fixture shows what `act` reports for a covered target and for a locator that matches twice.
#[test]
fn a_covered_target_and_a_duplicate_locator_are_errors_with_a_hint() {
    let (mut s, _) = Server::start();
    s.text("open", json!({"url": fixture(), "timeout_ms": 1500}));
    // The notice covers the form.
    let (r, error) = s.tool("act", json!({"op": "click", "loc": "label=\"Terms\""}));
    assert!(error && r.as_str().unwrap().contains("FAILED covered"), "{r}");
    // Close it with its ref, then the same call works.
    let found = s.text("find", json!({"text": "Accept notice"}));
    s.text("act", json!({"op": "click", "ref": ref_of(&found, "Accept notice")}));
    s.text("act", json!({"op": "check", "loc": "label=\"Terms\""}));
    // `role=button` alone matches several buttons.
    let (r, error) = s.tool("act", json!({"op": "click", "loc": "role=button"}));
    assert!(error && r.as_str().unwrap().contains("FAILED duplicate"), "{r}");
}
