//! Structured failures of `bare-mcp` (`docs/plan-agent-recovery.md`): a stable code from the source of the failure, redacted evidence and
//! a conservative `next_action`, in `structuredContent.failure`. Offline and headless, against a loopback page of this test.

#[path = "../../drive/tests/common/mod.rs"]
mod common;

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

struct Server {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    id: u64,
}

impl Server {
    fn start() -> Server {
        let mut child = Command::new(env!("CARGO_BIN_EXE_bare-mcp")).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
        let (stdin, stdout) = (child.stdin.take().unwrap(), BufReader::new(child.stdout.take().unwrap()));
        Server { child, stdin, stdout, id: 0 }
    }

    /// The whole result of a tool call, and the id check of the reply.
    fn call(&mut self, name: &str, args: Value) -> Value {
        self.id += 1;
        writeln!(self.stdin, "{}", json!({"jsonrpc": "2.0", "id": self.id, "method": "tools/call", "params": {"name": name, "arguments": args}})).unwrap();
        let mut line = String::new();
        self.stdout.read_line(&mut line).unwrap();
        let reply: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(reply["id"], self.id, "{line}");
        reply["result"].clone()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn page(_: &str, _: &str, _: &str, _: Option<&str>) -> (u16, &'static str, Vec<u8>) {
    let html = "<!doctype html><title>R</title><body>\
        <button>Save</button><button>Save</button>\
        <button id=off disabled>Off</button>\
        <button id=gone>Gone</button>\
        <button id=hid>Hid</button>\
        <button id=go>Go</button>\
        <div id=cover role=dialog aria-label=\"Cookie consent\" style=\"position:fixed;inset:0;background:rgba(0,0,0,.4)\"><p>We use cookies</p></div>\
        <script>document.getElementById('hid').style.display='none'</script></body>";
    (200, "text/html", html.as_bytes().to_vec())
}

fn failure(r: &Value) -> &Value {
    assert_eq!(r["isError"], true, "{r}");
    &r["structuredContent"]["failure"]
}

fn open(s: &mut Server) {
    let base = common::serve_route(page);
    let r = s.call("open", json!({"url": format!("{base}/"), "timeout_ms": 3000}));
    assert!(r["isError"] != true, "{r}");
}

#[test]
fn driver_kinds_become_stable_codes_with_conservative_actions() {
    let mut s = Server::start();
    open(&mut s);
    // (act arguments, code, next_action)
    let cases = [
        (json!({"op": "click", "loc": "role=button name=\"Save\""}), "duplicate", "refine_target"),
        (json!({"op": "click", "loc": "role=button name=\"Nope\""}), "not_found", "inspect_page"),
        (json!({"op": "click", "loc": "css=\"#hid\""}), "hidden", "inspect_page"),
        (json!({"op": "click", "loc": "css=\"#off\""}), "disabled", "wait_for_condition"),
        (json!({"op": "click", "loc": "css=\"#go\""}), "covered", "dismiss_overlay"),
    ];
    for (args, code, action) in cases {
        let r = s.call("act", json!({"op": args["op"], "loc": args["loc"], "snapshot": false}));
        let f = failure(&r);
        assert_eq!(f["code"], code, "{r}");
        assert_eq!(f["next_action"], action, "{r}");
        // The input may have run before the failure: the server does not say that it did not.
        assert_eq!(f["input_dispatched"], "unknown", "{r}");
        // The text receipt stays.
        assert!(r["content"][0]["text"].as_str().unwrap().contains(&format!("FAILED {code}")), "{r}");
    }
}

#[test]
fn a_covered_failure_keeps_the_covering_element_from_the_driver() {
    let mut s = Server::start();
    open(&mut s);
    let r = s.call("act", json!({"op": "click", "loc": "css=\"#go\"", "snapshot": false}));
    assert!(failure(&r)["message"].as_str().unwrap().contains("Cookie consent"), "{r}");
}

#[test]
fn a_stale_or_unknown_ref_asks_for_a_new_observation() {
    let mut s = Server::start();
    open(&mut s);
    s.call("snapshot", json!({}));
    let r = s.call("act", json!({"op": "click", "ref": "e9999", "snapshot": false}));
    let f = failure(&r);
    assert_eq!(f["code"], "stale_ref", "{r}");
    assert_eq!(f["next_action"], "refresh_observation", "{r}");
}

#[test]
fn bad_arguments_and_unknown_names_fail_before_dispatch() {
    let mut s = Server::start();
    open(&mut s);
    for (name, args, code, action) in [
        ("act", json!({"op": "click"}), "invalid_arguments", "correct_arguments"),
        ("act", json!({"op": "dance", "ref": "e1"}), "invalid_arguments", "correct_arguments"),
        ("act", json!({"opp": "click"}), "invalid_arguments", "correct_arguments"),
        ("snapshot", json!({"session": "other"}), "missing_session", "open_session"),
        ("act", json!({"op": "click", "ref": "e1", "session": "other"}), "missing_session", "open_session"),
        ("nope", json!({}), "unknown_tool", "correct_arguments"),
    ] {
        let r = s.call(name, args);
        let f = failure(&r);
        assert_eq!((&f["code"], &f["next_action"], &f["input_dispatched"]), (&json!(code), &json!(action), &json!("no")), "{name}: {r}");
    }
}

#[test]
fn an_untyped_internal_error_gets_the_documented_fallback() {
    let mut s = Server::start();
    // `storage` has no typed source for a bad area: the fallback, and the text is still there.
    open(&mut s);
    let r = s.call("storage", json!({"area": "nope"}));
    let f = failure(&r);
    assert_eq!((&f["code"], &f["next_action"]), (&json!("internal"), &json!("diagnose")), "{r}");
}

#[test]
fn a_failed_call_leaves_no_failure_for_the_next_call() {
    let mut s = Server::start();
    open(&mut s);
    let bad = s.call("act", json!({"op": "click", "loc": "css=\"#off\"", "snapshot": false}));
    assert_eq!(failure(&bad)["code"], "disabled");
    // A valid call after it: no error and no failure; the session survived.
    let ok = s.call("snapshot", json!({}));
    assert!(ok["isError"] != true && ok.get("structuredContent").is_none(), "{ok}");
    // A different failure gets its own code, and the ids stay in step (checked by `call`).
    let bad = s.call("act", json!({"op": "click", "loc": "role=button name=\"Save\"", "snapshot": false}));
    assert_eq!(failure(&bad)["code"], "duplicate");
}

#[test]
fn a_failed_step_has_the_failure_beside_the_report() {
    let mut s = Server::start();
    open(&mut s);
    let r = s.call("step", json!({"steps": "click role=button name=\"Nope\""}));
    assert!(r["isError"] != true, "{r}");
    assert_eq!(r["structuredContent"]["failure"]["code"], "not_found", "{r}");
}

#[test]
fn secrets_do_not_reach_the_evidence() {
    let dir = std::env::temp_dir().join(format!("bare-recovery-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let secrets = dir.join("secrets");
    std::fs::write(&secrets, "TOKEN=zq-secret-77\n").unwrap();
    let mut s = Server::start();
    let base = common::serve_route(page);
    s.call("open", json!({"url": format!("{base}/"), "secrets_file": secrets.to_str().unwrap(), "timeout_ms": 3000}));
    let r = s.call("act", json!({"op": "click", "loc": "role=button name=\"zq-secret-77\"", "snapshot": false}));
    let f = failure(&r);
    assert_eq!(f["code"], "not_found", "{r}");
    assert!(!f.to_string().contains("zq-secret-77") && f["message"].as_str().unwrap().contains("[secret]"), "{f}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_generic_page_failure_is_not_called_an_unsupported_feature() {
    let mut s = Server::start();
    open(&mut s);
    let r = s.call("act", json!({"op": "goto", "url": "http://127.0.0.1:1/", "snapshot": false}));
    let text = r.to_string();
    assert!(!text.contains("unsupported_browser_feature"), "{r}");
    if r["isError"] == true {
        assert!(["page", "not_found", "internal", "expect"].contains(&r["structuredContent"]["failure"]["code"].as_str().unwrap()), "{r}");
    }
}
