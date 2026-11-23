//! `snapshot` spends its tokens on what an agent uses: a plain table row has no ref and its cells have no quotes, a link below the fold
//! has no `(offscreen)` state, and a `clickable` with no name and no lines below it does not show. Offline and headless, against a page
//! that a test serves on loopback.

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

fn snap_of(page: &'static str) -> String {
    let url = page_server(page);
    let mut s = Server::start();
    s.tool("open", json!({"session": "t", "url": url, "snapshot": false, "timeout_ms": 30000}));
    s.tool("snapshot", json!({"session": "t", "mode": "all"}))
}

#[test]
fn a_plain_row_has_no_ref_and_its_cells_have_no_quotes() {
    let snap = snap_of(
        "<!doctype html><title>t</title><body><table><tr><th>#</th><th>Price</th></tr><tr><td>1</td><td><a href=/b>Buy</a></td></tr>\
         <tr><td>a | b</td><td></td></tr></table></body>",
    );
    let lines: Vec<&str> = snap.lines().map(str::trim).collect();
    assert!(lines.contains(&"row: # | Price"), "{snap}");
    assert!(lines.iter().any(|l| l.starts_with("row: 1 | [e") && l.ends_with(']')), "{snap}");
    // A cell whose text could read as a cell border or a ref keeps its quotes; an empty cell keeps its column.
    assert!(lines.contains(&"row: \"a | b\" |"), "{snap}");
}

#[test]
fn a_link_below_the_fold_is_not_marked_offscreen() {
    let snap = snap_of("<!doctype html><title>t</title><body><div style='height:3000px'></div><a href=/far style='display:block'>Far link</a><button>Far button</button></body>");
    let link = snap.lines().find(|l| l.contains("Far link")).unwrap();
    let button = snap.lines().find(|l| l.contains("Far button")).unwrap();
    assert!(!link.contains("offscreen") && button.contains("(offscreen)"), "{snap}");
}

#[test]
fn an_empty_clickable_with_nothing_below_it_does_not_show() {
    let snap = snap_of(
        "<!doctype html><title>t</title><style>.c{cursor:pointer;width:20px;height:20px}</style><body><div class=c></div><div class=c>Go</div></body>",
    );
    // The second keeps its line: its text follows on the line below it.
    assert_eq!(snap.matches("] clickable").count(), 1, "{snap}");
    assert!(snap.contains("\"Go\""), "{snap}");
}
