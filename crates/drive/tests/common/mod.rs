//! The server of the fixture app (`tools/fixtures/drive`): the pages, a login API with one synthetic user, and a data API that needs
//! the token of the login. Loopback only; one thread for each connection, keep-alive.
#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;

pub const EMAIL: &str = "fixture.user@example.test";
pub const PASSWORD: &str = "fixture pass 7!";
const TOKEN: &str = "fixture-token-1";

/// Start the server on a free port. Gives its origin (`http://127.0.0.1:PORT`).
pub fn serve() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in listener.incoming().flatten() {
            std::thread::spawn(move || connection(s));
        }
    });
    format!("http://127.0.0.1:{port}")
}

fn connection(mut s: TcpStream) {
    s.set_nodelay(true).ok();
    let (mut seen, mut piece) = (Vec::new(), [0u8; 4096]);
    loop {
        let end = loop {
            if let Some(at) = seen.windows(4).position(|w| w == b"\r\n\r\n") {
                break at + 4;
            }
            match s.read(&mut piece) {
                Ok(0) | Err(_) => return,
                Ok(n) => seen.extend_from_slice(&piece[..n]),
            }
        };
        let head = String::from_utf8_lossy(&seen[..end]).to_string();
        seen.drain(..end);
        let header = |name: &str| head.lines().find_map(|l| l.split_once(':').filter(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.trim().to_string()));
        let want = header("content-length").and_then(|v| v.parse::<usize>().ok()).unwrap_or(0);
        while seen.len() < want {
            match s.read(&mut piece) {
                Ok(0) | Err(_) => return,
                Ok(n) => seen.extend_from_slice(&piece[..n]),
            }
        }
        let body = String::from_utf8_lossy(&seen[..want]).to_string();
        seen.drain(..want);
        let mut words = head.split_whitespace();
        let (method, target) = (words.next().unwrap_or(""), words.next().unwrap_or("/"));
        let path = target.split('?').next().unwrap_or("/");
        let (status, kind, out) = match (path, target.split_once("?n=")) {
            ("/buttons", Some((_, n))) => (200, "text/html", buttons(n.parse().unwrap_or(1))),
            _ => reply(method, path, &body, header("authorization").as_deref()),
        };
        let head = format!("HTTP/1.1 {status} X\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nCache-Control: no-store\r\n\r\n", out.len());
        if s.write_all(head.as_bytes()).and_then(|_| s.write_all(&out)).is_err() {
            return;
        }
    }
}

/// A page of `n` buttons (`#b0`..), each named "Button K", and a text that a timer adds after 2 s (`#late`). For the cost example.
fn buttons(n: usize) -> Vec<u8> {
    let list: String = (0..n).map(|i| format!("<button id=b{i}>Button {i}</button>\n")).collect();
    format!("<!DOCTYPE html><title>Buttons</title><p id=late></p>{list}<script>var c = 0; document.addEventListener('click', function () {{ c++ }}); setTimeout(function () {{ document.getElementById('late').textContent = 'late' }}, 2000);</script>").into_bytes()
}

fn page(name: &str) -> Vec<u8> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tools/fixtures/drive");
    std::fs::read(dir.join(format!("{name}.html"))).unwrap()
}

fn reply(method: &str, path: &str, body: &str, auth: Option<&str>) -> (u16, &'static str, Vec<u8>) {
    const HTML: &str = "text/html; charset=utf-8";
    const JSON: &str = "application/json";
    match (method, path) {
        ("GET", "/" | "/login") => (200, HTML, page("login")),
        ("GET", "/overview") => (200, HTML, page("overview")),
        ("GET", "/agents") => (200, HTML, page("agents")),
        ("GET", "/kinds") => (200, HTML, page("kinds")),
        ("POST", "/api/login") => {
            let want = format!("{{\"email\":\"{EMAIL}\",\"password\":\"{PASSWORD}\"}}");
            if body == want { (200, JSON, format!("{{\"token\":\"{TOKEN}\"}}").into_bytes()) } else { (401, JSON, b"{\"error\":\"no\"}".to_vec()) }
        }
        ("GET", "/api/stats") if auth == Some(&format!("Bearer {TOKEN}")) => (200, JSON, b"{\"items\":[\"Calls: 12\",\"Agents: 3\",\"Minutes: 40\"]}".to_vec()),
        ("GET", "/api/stats") => (401, JSON, b"{}".to_vec()),
        _ => (404, "text/plain", b"not found".to_vec()),
    }
}
