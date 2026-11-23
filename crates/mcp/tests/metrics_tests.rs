//! One metrics line per tool call, and the `--stats` summary (#965).
#[path = "../../drive/tests/common/mod.rs"]
mod common;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

struct Server { child: Child, input: ChildStdin, output: BufReader<ChildStdout>, id: u64 }
impl Server {
    fn start(metrics: &std::path::Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_bare-mcp")).env("BARE_MCP_METRICS", metrics)
            .stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
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
        reply["result"].clone()
    }
}
impl Drop for Server {
    fn drop(&mut self) { let _ = self.child.kill(); let _ = self.child.wait(); }
}
fn page(_: &str, _: &str, _: &str, _: Option<&str>) -> (u16, &'static str, Vec<u8>) {
    (200, "text/html", b"<!doctype html><body><p>secret page text</p></body>".to_vec())
}
fn dir(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("bare-mcp-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn one_line_per_call_without_page_text_or_query() {
    let d = dir("metrics");
    let file = d.join("m.jsonl");
    let mut s = Server::start(&file);
    let url = format!("{}?token=hunter2", common::serve_route(page));
    let r = s.call("open", json!({"url": url, "timeout_ms": 1000, "snapshot": true}));
    assert_ne!(r["isError"], true, "{r}");
    let r = s.call("act", json!({"op": "click", "loc": "role=button name=\"Nope\""}));
    assert_eq!(r["isError"], true, "{r}");
    let text = std::fs::read_to_string(&file).unwrap();
    let lines: Vec<Value> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(lines.len(), 2, "{text}");
    assert_eq!((lines[0]["tool"].as_str(), lines[0]["ok"].as_bool()), (Some("open"), Some(true)), "{text}");
    assert_eq!((lines[1]["tool"].as_str(), lines[1]["ok"].as_bool()), (Some("act"), Some(false)), "{text}");
    assert!(lines[1]["code"].is_string() && lines[0].get("code").is_none(), "{text}");
    for l in &lines {
        assert!(l["ts"].as_u64().unwrap() > 1_700_000_000 && l["ms"].as_f64().unwrap() >= 0.0 && l["out_chars"].as_u64().unwrap() > 0, "{l}");
    }
    assert!(!text.contains("secret") && !text.contains("hunter2") && !text.contains("127.0.0.1"), "{text}");
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn off_switch_writes_nothing() {
    let d = dir("metrics-off");
    let mut child = Command::new(env!("CARGO_BIN_EXE_bare-mcp")).env("BARE_MCP_METRICS", "off").env("BARE_CACHE_DIR", &d)
        .stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    let mut input = child.stdin.take().unwrap();
    writeln!(input, "{}", json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"nope","arguments":{}}})).unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap()).read_line(&mut line).unwrap();
    drop(input);
    let _ = child.wait();
    assert_eq!(std::fs::read_dir(&d).unwrap().count(), 0);
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn stats_gives_percentiles_and_error_rate_per_tool() {
    let d = dir("metrics-stats");
    let file = d.join("m.jsonl");
    let mut text = String::new();
    for i in 1..=20 {
        text += &format!("{{\"ts\":1,\"tool\":\"snapshot\",\"ms\":{i}.0,\"out_chars\":5,\"ok\":{}}}\n", i != 20);
    }
    std::fs::write(&file, text).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_bare-mcp")).arg("--stats").arg(&file).output().unwrap();
    let out = String::from_utf8(out.stdout).unwrap();
    let row: Vec<&str> = out.lines().nth(1).unwrap().split_whitespace().collect();
    assert_eq!(row, ["snapshot", "20", "10.00", "19.00", "20.00", "5.0%"], "{out}");
    let _ = std::fs::remove_dir_all(d);
}
