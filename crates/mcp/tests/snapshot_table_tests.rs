//! `snapshot mode=all` says the text of a table once: a row of plain cells is one line, and a row or a cell that holds other lines
//! does not repeat their text as its name (#960). Offline and headless, against a page that a test serves on loopback.

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

/// The ref of the first line that holds `what`.
fn ref_of(snap: &str, what: &str) -> String {
    let line = snap.lines().find(|l| l.contains(what)).unwrap_or_else(|| panic!("no {what} in\n{snap}"));
    line.trim_start().trim_start_matches('*').split(']').next().unwrap().trim_start_matches('[').to_string()
}

#[test]
fn a_table_row_of_plain_cells_is_one_line() {
    let url = page_server(
        "<!doctype html><title>t</title><body><table><tr><th>#</th><th>Price</th><th>Action</th></tr>\
         <tr><td>1</td><td>$1.99</td><td><a href=/b>Buy one</a></td></tr><tr><td>2</td><td></td><td>Sold</td></tr></table></body>",
    );
    let mut s = Server::start();
    s.tool("open", json!({"session": "t", "url": url, "snapshot": false, "timeout_ms": 30000}));
    let snap = s.tool("snapshot", json!({"session": "t", "mode": "all"}));
    let link = ref_of(&snap, "link \"Buy one\"");
    assert!(snap.contains("row: \"#\" | \"Price\" | \"Action\""), "{snap}");
    assert!(snap.contains(&format!("row: \"1\" | \"$1.99\" | [{link}]\n")), "{snap}");
    // An empty cell keeps its column.
    assert!(snap.contains("row: \"2\" | | \"Sold\""), "{snap}");
    // Each text is said once, and the link keeps its line.
    assert_eq!(snap.matches("$1.99").count(), 1, "{snap}");
    assert_eq!(snap.matches("Buy one").count(), 1, "{snap}");
    assert!(!snap.contains("] cell"), "{snap}");
}

#[test]
fn a_layout_row_does_not_repeat_the_text_of_its_lines() {
    // A layout table as on Hacker News: a row holds a nested table, so it keeps its lines, but not their text as its name.
    let url = page_server(
        "<!doctype html><title>t</title><body><table><tr><td><table><tr><td>478 points by <a href=/u>someone</a></td></tr></table></td></tr>\
         <tr><td aria-label=\"Footer cell\"><table><tr><td>Bottom</td></tr></table></td></tr></table></body>",
    );
    let mut s = Server::start();
    s.tool("open", json!({"session": "t", "url": url, "snapshot": false, "timeout_ms": 30000}));
    let snap = s.tool("snapshot", json!({"session": "t", "mode": "all"}));
    assert_eq!(snap.matches("478 points by").count(), 1, "{snap}");
    assert_eq!(snap.matches("someone").count(), 1, "{snap}");
    // A name that does not come from the text stays.
    assert!(snap.contains("cell \"Footer cell\""), "{snap}");
}
