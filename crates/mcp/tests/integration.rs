//! Contracts shared by action outcomes, typed recovery, observations and session workers.
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
    fn call(&mut self, name: &str, arguments: Value) -> Value {
        self.id += 1;
        writeln!(self.input, "{}", json!({"jsonrpc":"2.0", "id":self.id, "method":"tools/call", "params":{"name":name,"arguments":arguments}})).unwrap();
        let mut line = String::new();
        self.output.read_line(&mut line).unwrap();
        let reply: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(reply["id"], self.id, "{reply}");
        reply["result"].clone()
    }
    fn open(&mut self) {
        let url = common::serve_route(page);
        let r = self.call("open", json!({"url":url,"timeout_ms":1000,"snapshot":true}));
        assert_ne!(r["isError"], true, "{r}");
    }
}
impl Drop for Server {
    fn drop(&mut self) { let _ = self.child.kill(); let _ = self.child.wait(); }
}
fn page(_: &str, _: &str, _: &str, _: Option<&str>) -> (u16, &'static str, Vec<u8>) {
    (200, "text/html", b"<!doctype html><body><button onclick='window.count += 1'>Save</button><label>Password<input type=password></label><script>window.count=0</script></body>".to_vec())
}

#[test]
fn failed_verification_keeps_execution_recovery_and_observation_evidence() {
    let mut s = Server::start();
    s.open();
    let r = s.call("act", json!({"op":"click","loc":"role=button name=\"Save\"","expect":"expect url=\"https://invalid.example/\""}));
    let status = &r["structuredContent"];
    assert_eq!(r["isError"], true, "{r}");
    assert_eq!(status["action_status"], "completed", "{r}");
    assert_eq!(status["verification"], "failed", "{r}");
    assert_eq!(status["failure"]["code"], "expect", "{r}");
    assert_eq!(status["failure"]["next_action"], "inspect_page", "{r}");
    assert_eq!(status["failure"]["input_dispatched"], "unknown", "{r}");
    assert!(r["content"][0]["text"].as_str().unwrap().contains("doc: d"), "{r}");
    let count = s.call("eval", json!({"js":"window.count"}));
    assert_eq!(count["content"][0]["text"], "1", "{count}");
    let next = s.call("act", json!({"op":"scroll"}));
    assert!(next["structuredContent"].get("failure").is_none(), "{next}");
}

#[test]
fn password_learned_by_act_is_redacted_in_failed_postcondition_metadata() {
    let mut s = Server::start();
    s.open();
    let r = s.call("act", json!({"op":"type","loc":"label=\"Password\"","text":"synthetic-new-password","expect":"expect text=\"synthetic-new-password\" visible"}));
    assert_eq!(r["structuredContent"]["action_status"], "completed", "{r}");
    assert_eq!(r["structuredContent"]["verification"], "failed", "{r}");
    assert!(r["structuredContent"]["failure"].is_object(), "{r}");
    assert!(!r.to_string().contains("synthetic-new-password"), "{r}");
    assert!(r.to_string().contains("[secret]"), "{r}");
}

#[test]
fn predispatch_errors_keep_both_outcomes_and_recovery_at_transport_boundary() {
    let mut s = Server::start();
    for args in [json!({"op":"click","loc":"text=\"Save\""}), json!({"op":"click","unexpected":true})] {
        let r = s.call("act", args);
        assert_eq!(r["structuredContent"]["action_status"], "not_started", "{r}");
        assert_eq!(r["structuredContent"]["failure"]["input_dispatched"], "no", "{r}");
        assert!(r["structuredContent"]["failure"]["code"].is_string(), "{r}");
    }
}
