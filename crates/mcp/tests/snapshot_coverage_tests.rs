//! `snapshot mode=all` holds the visible text of a page (#962). Each fixture in `tools/fixtures/coverage/` has a truth file: Chrome's
//! `document.body.innerText`, made by `tools/coverage/truth.py`. The test gives the word recall (the share of truth words that the
//! snapshot holds) and fails when it falls below the floor in `floors.txt`. Text that the page hides must not show. Offline and headless.

use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
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

/// Serves `page` on loopback for every request.
fn page_server(page: String) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for mut c in listener.incoming().flatten() {
            let (mut head, mut b) = (Vec::new(), [0u8; 1]);
            while !head.ends_with(b"\r\n\r\n") && c.read(&mut b).unwrap_or(0) == 1 {
                head.push(b[0]);
            }
            let _ = write!(c, "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{page}", page.len());
        }
    });
    format!("http://127.0.0.1:{port}/")
}

fn words(s: &str) -> HashMap<String, usize> {
    let mut m = HashMap::new();
    let mut w = String::new();
    for c in s.chars().chain(Some(' ')) {
        if c.is_alphanumeric() || c == '_' {
            w.extend(c.to_lowercase());
        } else if !w.is_empty() {
            *m.entry(std::mem::take(&mut w)).or_insert(0) += 1;
        }
    }
    m
}

/// Share of the truth words (with repeats) that the output holds.
fn recall(truth: &str, out: &str) -> f64 {
    let (t, o) = (words(truth), words(out));
    let total: usize = t.values().sum();
    let hit: usize = t.iter().map(|(w, n)| (*n).min(o.get(w).copied().unwrap_or(0))).sum();
    hit as f64 / total as f64
}

/// The truth words (with the missing count) that the output lacks, for the failure message.
fn missing(truth: &str, out: &str) -> Vec<String> {
    let (t, o) = (words(truth), words(out));
    let mut m: Vec<String> = t.iter().filter(|(w, n)| o.get(*w).copied().unwrap_or(0) < **n).map(|(w, n)| format!("{w} x{n}")).collect();
    m.sort();
    m
}

/// Hidden text that bare still shows. Remove a word when bare stops showing it.
const KNOWN_LEAKS: &[&str] = &[];

#[test]
fn snapshot_all_keeps_the_text_chrome_shows() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/fixtures/coverage");
    let floors = std::fs::read_to_string(dir.join("floors.txt")).unwrap();
    let mut s = Server::start();
    let mut fails = Vec::new();
    for line in floors.lines().filter(|l| !l.trim().is_empty()) {
        let (name, floor) = line.split_once(' ').unwrap();
        let floor: f64 = floor.trim().parse().unwrap();
        let page = std::fs::read_to_string(dir.join(format!("{name}.html"))).unwrap();
        let truth = std::fs::read_to_string(dir.join(format!("{name}.truth.txt"))).unwrap();
        s.tool("open", json!({"session": name, "url": page_server(page.clone()), "snapshot": false, "timeout_ms": 30000}));
        let snap = s.tool("snapshot", json!({"session": name, "mode": "all", "max_chars": 10_000_000}));
        let r = recall(&truth, &snap);
        println!("recall {name} {r:.4} missing {:?}", missing(&truth, &snap));
        if r < floor {
            fails.push(format!("{name}: recall {r:.4} is below the floor {floor}\n{snap}"));
        }
        // A word that starts with HIDDEN in the page marks text that the page hides.
        let lower = snap.to_lowercase();
        for w in page.split(|c: char| !c.is_alphanumeric()).filter(|w| w.starts_with("HIDDEN")) {
            if lower.contains(&w.to_lowercase()) && !KNOWN_LEAKS.contains(&w) {
                fails.push(format!("{name}: hidden text {w} shows in\n{snap}"));
            }
        }
    }
    assert!(fails.is_empty(), "{}", fails.join("\n\n"));
}
