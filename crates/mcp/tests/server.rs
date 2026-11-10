//! `bare-mcp` against the fixture app of the driver (`crates/drive/tests/common`), offline and headless: the staging journey through
//! `run_steps`, the same journey one line at a time through a live session, and the protocol errors. No secret may leak into a reply.

#[path = "../../drive/tests/common/mod.rs"]
mod common;

use common::{EMAIL, PASSWORD};
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
        let mut s = Server { child, stdin, stdout, id: 0 };
        let init = s.ask("initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}));
        assert_eq!(init["result"]["serverInfo"]["name"], "bare-mcp");
        assert!(init["result"]["instructions"].as_str().unwrap().contains("label bare-mcp"));
        writeln!(s.stdin, "{}", json!({"jsonrpc": "2.0", "method": "notifications/initialized"})).unwrap();
        s
    }

    fn ask(&mut self, method: &str, params: Value) -> Value {
        self.id += 1;
        writeln!(self.stdin, "{}", json!({"jsonrpc": "2.0", "id": self.id, "method": method, "params": params})).unwrap();
        let mut line = String::new();
        self.stdout.read_line(&mut line).unwrap();
        for s in [EMAIL, PASSWORD, "app-token-"] {
            assert!(!line.contains(s), "a secret leaked: {s} in {line}");
        }
        let reply: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(reply["id"], self.id, "{line}");
        reply
    }

    /// Call a tool. Gives the report (the JSON in the text) and whether the call was an error.
    fn tool(&mut self, name: &str, args: Value) -> (Value, bool) {
        let r = self.ask("tools/call", json!({"name": name, "arguments": args}));
        let text = r["result"]["content"][0]["text"].as_str().unwrap().to_string();
        let error = r["result"]["isError"] == true;
        (serde_json::from_str(&text).unwrap_or(Value::String(text)), error)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The staging step file and the arguments for the fixture app: `BASE`, `QUERY` and a secrets file.
fn journey(name: &str) -> (String, Value) {
    let dir = std::env::temp_dir().join(format!("bare-mcp-test-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let secrets = dir.join("secrets");
    std::fs::write(&secrets, format!("EMAIL={EMAIL}\nPASSWORD={PASSWORD}\n")).unwrap();
    let steps = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../tools/drive/staging-journey.steps")).unwrap();
    let args = json!({"vars": {"BASE": common::serve_app(), "QUERY": "Beta"}, "secrets_file": secrets.to_str().unwrap()});
    (steps, args)
}

#[test]
fn tools_list_names_the_four_tools() {
    let mut s = Server::start();
    let r = s.ask("tools/list", json!({}));
    let names: Vec<&str> = r["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["run_steps", "open", "step", "close"]);
}

#[test]
fn run_steps_passes_the_staging_journey_on_the_fixture_app() {
    let mut s = Server::start();
    let (steps, mut args) = journey("run");
    args["steps"] = json!(steps);
    let (rep, error) = s.tool("run_steps", args);
    assert!(!error, "{rep}");
    assert_eq!(rep["ok"], true, "{rep}");
    assert_eq!(rep["steps"].as_array().unwrap().len(), 27);
    // The `shot` steps write PNGs into the out folder of the call.
    let out = rep["out"].as_str().unwrap();
    assert!(std::path::Path::new(out).join("overview.png").is_file(), "{rep}");
}

#[test]
fn a_live_session_runs_the_journey_one_line_at_a_time() {
    let mut s = Server::start();
    let (steps, mut args) = journey("live");
    let mut lines = steps.lines().filter(|l| !l.trim().is_empty() && !l.starts_with('#'));
    assert_eq!(lines.next(), Some("timeout 30000"));
    assert_eq!(lines.next(), Some("open ${BASE}/"));
    args["url"] = json!(format!("{}/", args["vars"]["BASE"].as_str().unwrap()));
    args["timeout_ms"] = json!(30000);
    let (rep, error) = s.tool("open", args);
    assert!(!error && rep["ok"] == true, "{rep}");
    let mut last = 1;
    for line in lines {
        let (rep, error) = s.tool("step", json!({"steps": line}));
        assert!(!error && rep["ok"] == true, "{line}: {rep}");
        // Each call gives only its own step, numbered after the steps before it.
        let got = rep["steps"].as_array().unwrap();
        assert_eq!(got.len(), 1, "{rep}");
        assert_eq!(got[0]["line"], last + 1);
        last += 1;
    }
    let (rep, error) = s.tool("close", json!({}));
    assert!(!error && rep["steps"].as_array().unwrap().is_empty(), "{rep}");
    let (rep, error) = s.tool("step", json!({"steps": "reload"}));
    assert!(error, "{rep}");
}

#[test]
fn a_failed_step_keeps_the_session_open() {
    let mut s = Server::start();
    let (_, mut args) = journey("fail");
    args["url"] = json!(format!("{}/", args["vars"]["BASE"].as_str().unwrap()));
    s.tool("open", args);
    let (rep, error) = s.tool("step", json!({"steps": "timeout 300\nexpect css=\"#nothing-here\" visible\nexpect url~\"/login\""}));
    assert!(!error, "{rep}");
    assert_eq!(rep["ok"], false);
    assert_eq!(rep["failure"]["line"], 3, "{rep}");
    assert_eq!(rep["failure"]["kind"], "not_found", "{rep}");
    // The line after the failure did not run; the next call runs on the same page.
    assert_eq!(rep["steps"].as_array().unwrap().len(), 2);
    let (rep, _) = s.tool("step", json!({"steps": "expect url~\"/login\""}));
    assert_eq!(rep["ok"], true, "{rep}");
}

#[test]
fn bad_calls_are_errors_and_the_server_goes_on() {
    let mut s = Server::start();
    assert!(s.tool("step", json!({"steps": "reload"})).1);
    assert!(s.tool("run_steps", json!({})).1);
    assert!(s.tool("run_steps", json!({"steps": "fly away"})).1);
    assert!(s.tool("nope", json!({})).1);
    assert_eq!(s.ask("nope/method", json!({}))["error"]["code"], -32601);
    writeln!(s.stdin, "not json").unwrap();
    let mut line = String::new();
    s.stdout.read_line(&mut line).unwrap();
    assert!(line.contains("-32700"), "{line}");
    assert_eq!(s.ask("ping", json!({}))["result"], json!({}));
}

#[test]
fn a_run_without_pictures_leaves_no_folder() {
    let mut s = Server::start();
    let (_, mut args) = journey("nopics");
    args["steps"] = json!("open ${BASE}/\nexpect url~\"/login\"");
    let (rep, error) = s.tool("run_steps", args);
    assert!(!error && rep["ok"] == true, "{rep}");
    assert!(rep.get("out").is_none(), "{rep}");
}
