//! `BARE_MCP_SNAPSHOT_MODE` sets the default snapshot mode of a whole run (#964). A `mode` in the call still wins.
//! Offline and headless, against a page that a test serves on loopback.

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
    fn start(mode: Option<&str>) -> Server {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_bare-mcp"));
        cmd.env_remove("BARE_MCP_SNAPSHOT_MODE");
        if let Some(m) = mode {
            cmd.env("BARE_MCP_SNAPSHOT_MODE", m);
        }
        let mut child = cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
        let (stdin, stdout) = (child.stdin.take().unwrap(), BufReader::new(child.stdout.take().unwrap()));
        let mut s = Server { child, stdin, stdout, id: 0 };
        s.ask("initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "test", "version": "0"}}));
        writeln!(s.stdin, "{}", json!({"jsonrpc": "2.0", "method": "notifications/initialized"})).unwrap();
        s
    }

    fn ask(&mut self, method: &str, params: Value) -> Value {
        self.id += 1;
        writeln!(self.stdin, "{}", json!({"jsonrpc": "2.0", "id": self.id, "method": method, "params": params})).unwrap();
        let mut line = String::new();
        self.stdout.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    }

    fn tool(&mut self, name: &str, args: Value) -> String {
        let r = self.ask("tools/call", json!({"name": name, "arguments": args}));
        let text = r["result"]["content"][0]["text"].as_str().unwrap().to_string();
        assert!(r["result"]["isError"] != true, "{text}");
        text
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn page_server(page: &'static str) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for mut c in listener.incoming().flatten() {
            let (mut head, mut b) = (Vec::new(), [0u8; 1]);
            while !head.ends_with(b"\r\n\r\n") && c.read(&mut b).unwrap_or(0) == 1 {
                head.push(b[0]);
            }
            let _ = write!(c, "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{page}", page.len());
        }
    });
    format!("http://127.0.0.1:{port}/")
}

const PAGE: &str = "<!doctype html><title>t</title><body><h1>Head</h1><p>Plain words between controls.</p><button>Go</button></body>";

fn snap(mode: Option<&str>, args: Value) -> String {
    let mut s = Server::start(mode);
    s.tool("open", json!({"session": "t", "url": page_server(PAGE), "snapshot": false, "timeout_ms": 30000}));
    s.tool("snapshot", args)
}

#[test]
fn the_default_mode_is_interactive() {
    let out = snap(None, json!({"session": "t"}));
    assert!(out.contains("button \"Go\"") && !out.contains("Plain words"), "{out}");
}

#[test]
fn the_env_sets_the_default_mode_to_all() {
    let out = snap(Some("all"), json!({"session": "t"}));
    assert!(out.contains("Plain words between controls."), "{out}");
}

#[test]
fn a_mode_in_the_call_wins_over_the_env() {
    let out = snap(Some("all"), json!({"session": "t", "mode": "interactive"}));
    assert!(!out.contains("Plain words"), "{out}");
}

#[test]
fn an_unknown_env_value_keeps_interactive() {
    let out = snap(Some("bogus"), json!({"session": "t"}));
    assert!(!out.contains("Plain words"), "{out}");
}
