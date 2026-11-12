//! The server of the fixture app (`tools/fixtures/drive`): the pages, a login API with one synthetic user, and a data API that needs
//! the token of the login (`serve`). The single-page app (`app.html`, `serve_app`) has the login model of the staging app. Loopback only;
//! one thread for each connection, keep-alive.
#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;

pub const EMAIL: &str = "fixture.user@example.test";
pub const PASSWORD: &str = "fixture pass 7!";
const TOKEN: &str = "fixture-token-1";
/// A second user of the app: a session of this user ends after one data request (the API then answers 401, as for an expired token).
pub const BRIEF_EMAIL: &str = "brief.user@example.test";

/// What a route gives: the method, the target (path and query), the body and the `Authorization` header.
type Route = fn(&str, &str, &str, Option<&str>) -> (u16, &'static str, Vec<u8>);

/// Start the server of the pages on a free port. Gives its origin (`http://127.0.0.1:PORT`).
pub fn serve() -> String {
    serve_with(pages)
}

/// Start the server of the single-page app on a free port. Gives its origin.
pub fn serve_app() -> String {
    serve_with(app)
}

fn serve_with(route: Route) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in listener.incoming().flatten() {
            std::thread::spawn(move || connection(s, route));
        }
    });
    format!("http://127.0.0.1:{port}")
}

fn connection(mut s: TcpStream, route: Route) {
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
        let (status, kind, out) = route(method, target, &body, header("authorization").as_deref());
        let head = format!("HTTP/1.1 {status} X\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nCache-Control: no-store\r\n\r\n", out.len());
        if s.write_all(head.as_bytes()).and_then(|_| s.write_all(&out)).is_err() {
            return;
        }
    }
}

fn pages(method: &str, target: &str, body: &str, auth: Option<&str>) -> (u16, &'static str, Vec<u8>) {
    let path = target.split('?').next().unwrap_or("/");
    match (path, target.split_once("?n=")) {
        ("/buttons", Some((_, n))) => (200, "text/html", buttons(n.parse().unwrap_or(1))),
        ("/forms", _) => (200, "text/html", FORMS.as_bytes().to_vec()),
        ("/upload", _) if method == "POST" => (200, "text/plain", upload_echo(body).into_bytes()),
        _ => reply(method, path, body, auth),
    }
}

/// A page of form controls for the form actions: two selects, a checkbox, a hover menu, a double click, and a file input with a
/// submitted form and a `fetch` with `FormData`.
const FORMS: &str = "<!DOCTYPE html><title>Forms</title>\
<style>#menu { display: none; position: absolute } #hv:hover #menu { display: block }</style>\
<select id=country name=country aria-label=Country><option value=ke>Kenya</option><option value=tz>Tanzania</option></select>\
<select id=fruit name=fruit multiple aria-label=Fruit><option value=a>Apple</option><option value=b>Banana</option><option value=c>Cherry</option></select>\
<p id=events>events:</p>\
<label><input type=checkbox id=agree> Agree</label>\
<div id=hv>Hover target<span id=menu> Menu is open</span></div><p id=entered>cold</p>\
<button id=dbl>Double target</button><p id=dbls>single</p>\
<form method=post action=/upload enctype=multipart/form-data><input type=file name=doc aria-label=Document><button>Send file</button></form>\
<button id=go>Fetch file</button><p id=fetched></p>\
<script>\
var ev = function (e) { document.getElementById('events').textContent += ' ' + e.target.id + ':' + e.type };\
['country', 'fruit'].forEach(function (id) { ['input', 'change'].forEach(function (t) { document.getElementById(id).addEventListener(t, ev) }) });\
document.getElementById('hv').addEventListener('mouseenter', function () { document.getElementById('entered').textContent = 'hot' });\
document.getElementById('dbl').addEventListener('dblclick', function () { document.getElementById('dbls').textContent = 'double' });\
document.getElementById('go').addEventListener('click', function () {\
  var f = document.querySelector('input[type=file]').files[0], fd = new FormData();\
  fd.append('doc', f); fd.append('note', 'hi');\
  fetch('/upload', { method: 'POST', body: fd }).then(function (r) { return r.text() }).then(function (t) { document.getElementById('fetched').textContent = 'fetched ' + t });\
});\
</script>";

/// The reply of `/upload`: the name and the size of each file part of a multipart body.
fn upload_echo(body: &str) -> String {
    let parts: Vec<String> = body
        .split("\r\n--")
        .filter_map(|p| {
            let (head, content) = p.split_once("\r\n\r\n")?;
            let name = head.split("filename=\"").nth(1)?.split('"').next()?;
            Some(format!("{name} {}", content.len()))
        })
        .collect();
    format!("uploaded: {}", parts.join("; "))
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

/// The live tokens of the app and the data requests that each has left.
static SESSIONS: std::sync::Mutex<Vec<(String, u32)>> = std::sync::Mutex::new(Vec::new());

const AGENTS: [&str; 4] = ["Alpha agent", "Beta agent", "Beta helper", "Gamma agent"];

fn app(method: &str, target: &str, body: &str, auth: Option<&str>) -> (u16, &'static str, Vec<u8>) {
    const JSON: &str = "application/json";
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    match (method, path) {
        ("POST", "/api/login") => {
            let uses = [(EMAIL, u32::MAX), (BRIEF_EMAIL, 1)].into_iter().find(|(e, _)| body == format!("{{\"email\":\"{e}\",\"password\":\"{PASSWORD}\"}}"));
            let Some((_, uses)) = uses else { return (401, JSON, b"{\"error\":\"invalid\"}".to_vec()) };
            let mut all = SESSIONS.lock().unwrap();
            let token = format!("app-token-{}", all.len() + 1);
            all.push((token.clone(), uses));
            (200, JSON, format!("{{\"token\":\"{token}\"}}").into_bytes())
        }
        ("GET", "/api/agents") => {
            let mut all = SESSIONS.lock().unwrap();
            let live = all.iter_mut().find(|(t, n)| *n > 0 && auth == Some(&format!("Bearer {t}")));
            let Some((_, left)) = live else { return (401, JSON, b"{\"error\":\"expired\"}".to_vec()) };
            *left -= 1;
            // The query is `q=` and a URL-encoded text; the tests search for plain words.
            let q = query.strip_prefix("q=").unwrap_or("").to_lowercase();
            let list: Vec<String> =
                AGENTS.iter().enumerate().filter(|(_, a)| a.to_lowercase().contains(&q)).map(|(i, a)| format!("{{\"id\":{i},\"name\":\"{a}\"}}")).collect();
            (200, JSON, format!("{{\"agents\":[{}]}}", list.join(",")).into_bytes())
        }
        ("GET", _) if !path.starts_with("/api/") => (200, "text/html; charset=utf-8", page("app")),
        _ => (404, "text/plain", b"not found".to_vec()),
    }
}
