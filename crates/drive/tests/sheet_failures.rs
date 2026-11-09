//! A style sheet that fails, does not apply or is off must not stop the page, and a sheet that is on its way must not fill the console.
//! A loopback server gives the pages. `slow.css` answers after 300 ms; the other sheets answer at once.

use bare_drive::run::{Options, Redactor, run};
use bare_drive::steps::parse;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const BODY: &str = "<body><p id=p>hi</p></body></html>";

fn page(head: &str) -> String {
    format!("<!doctype html><html><head>{head}</head>{BODY}")
}

/// The reply for `path`: type and body. `None`: a 404.
fn route(path: &str) -> Option<(&'static str, String)> {
    let html = |h: &str| Some(("text/html", page(h)));
    match path {
        "/slow.css" => {
            std::thread::sleep(Duration::from_millis(300));
            Some(("text/css", "p { color: red }".into()))
        }
        "/imp.css" => Some(("text/css", "@import url(/missing.css); p { color: red }".into())),
        "/plain.css" => Some(("text/plain", "p { color: red }".into())),
        "/slow" => html(r#"<link rel=stylesheet href="/slow.css">"#),
        "/e404" => html(r#"<link rel=stylesheet href="/nope.css">"#),
        "/unreach" => html(r#"<link rel=stylesheet href="http://127.0.0.1:1/x.css">"#),
        "/import" => html(r#"<link rel=stylesheet href="/imp.css">"#),
        "/mime" => html(r#"<link rel=stylesheet href="/plain.css">"#),
        "/print" => html(r#"<link rel=stylesheet media=print href="/print.css">"#),
        "/alt" => html(r#"<link rel="alternate stylesheet" href="/alt.css" title=x>"#),
        "/off" => html(r#"<link rel=stylesheet href="/off.css" disabled>"#),
        _ => None,
    }
}

fn serve(seen: Arc<Mutex<Vec<String>>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in listener.incoming().flatten() {
            let seen = seen.clone();
            std::thread::spawn(move || connection(s, seen));
        }
    });
    format!("http://127.0.0.1:{port}")
}

fn connection(mut s: TcpStream, seen: Arc<Mutex<Vec<String>>>) {
    let (mut got, mut piece) = (Vec::new(), [0u8; 4096]);
    while !got.windows(4).any(|w| w == b"\r\n\r\n") {
        match s.read(&mut piece) {
            Ok(0) | Err(_) => return,
            Ok(n) => got.extend_from_slice(&piece[..n]),
        }
    }
    let head = String::from_utf8_lossy(&got).to_string();
    let path = head.split(' ').nth(1).unwrap_or("/").to_string();
    seen.lock().unwrap().push(path.clone());
    let (status, ty, body) = match route(&path) {
        Some((ty, body)) => (200, ty, body),
        None => (404, "text/plain", "no".to_string()),
    };
    let r = format!("HTTP/1.1 {status} X\r\nContent-Type: {ty}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
    let _ = s.write_all(r.as_bytes());
}

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

/// Open `path`, expect `#p` visible. Gives the console lines that name a render and the paths that the server saw.
fn visit(path: &'static str) -> (Vec<String>, Vec<String>) {
    big(move || {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let origin = serve(seen.clone());
        let src = format!("open ${{ORIGIN}}{path}\nexpect css=\"#p\" visible\nexpect text=\"hi\" visible\n");
        let steps = parse(&src, &HashMap::from([("ORIGIN".to_string(), origin)])).unwrap();
        let opt = Options { size: (800, 600), timeout: Duration::from_secs(5), out: None, window: false, progress: false };
        let rep = run(&steps, &opt, &mut Redactor::default());
        assert!(rep.ok, "{path}: {:?}", rep.failure.as_ref().map(|f| &f.1.message));
        let renders = rep.logs.iter().filter(|(_, l)| l.text.starts_with("render:")).map(|(_, l)| l.text.clone()).collect();
        let seen = seen.lock().unwrap().clone();
        (renders, seen)
    })
}

#[test]
fn a_sheet_that_is_on_its_way_adds_no_console_line() {
    let (renders, _) = visit("/slow");
    assert!(renders.is_empty(), "{} lines: {:?}", renders.len(), renders.first());
}

#[test]
fn a_sheet_that_fails_does_not_stop_the_page_or_fill_the_console() {
    for path in ["/e404", "/unreach", "/import", "/mime"] {
        let (renders, _) = visit(path);
        assert!(renders.is_empty(), "{path}: {} lines: {:?}", renders.len(), renders.first());
    }
}

#[test]
fn a_print_sheet_an_alternate_sheet_and_a_disabled_link_are_not_fetched() {
    for (path, css) in [("/print", "/print.css"), ("/alt", "/alt.css"), ("/off", "/off.css")] {
        let (renders, seen) = visit(path);
        assert!(renders.is_empty(), "{path}: {:?}", renders.first());
        assert!(!seen.iter().any(|p| p == css), "{path}: {css} was fetched: {seen:?}");
    }
}
