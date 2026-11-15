//! Tests of the streamed document (`browser_session.rs`: `Feed`, `Parsing`): a live session takes the body of the document in pieces, scans
//! and parses them while the rest is on its way, and runs no script before the tree is complete. A raw socket server sends the document in
//! pieces, so a test controls when each piece leaves.

use crate::browser_session::*;
use bare_js::eval::EvalLimits;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

/// A document that leaves in pieces. `/` is the document; any other path is a script from `files`, or 404.
struct Slow {
    port: u16,
    log: Arc<Mutex<Vec<String>>>,
}

struct Plan {
    pieces: Vec<Vec<u8>>,
    content_type: &'static str,
    /// After this piece, wait (5 s at most) until the server has seen a request for this path.
    hold: Option<(usize, &'static str)>,
    /// A pause after each piece.
    gap: Duration,
    /// The header says this many bytes more than the pieces hold, then the connection closes.
    short_by: usize,
    files: Vec<(&'static str, &'static str)>,
}

impl Plan {
    fn new(pieces: &[&[u8]]) -> Plan {
        Plan { pieces: pieces.iter().map(|p| p.to_vec()).collect(), content_type: "text/html; charset=utf-8", hold: None, gap: Duration::ZERO, short_by: 0, files: Vec::new() }
    }
}

fn slow(plan: Plan) -> Slow {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let log = Arc::new(Mutex::new(Vec::new()));
    let (plan, l) = (Arc::new(plan), log.clone());
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let (Ok(conn), plan, log) = (conn, plan.clone(), l.clone()) else { continue };
            std::thread::spawn(move || answer(conn, &plan, &log));
        }
    });
    Slow { port, log }
}

fn answer(mut c: TcpStream, plan: &Plan, log: &Mutex<Vec<String>>) {
    let _ = c.set_nodelay(true);
    let mut head = Vec::new();
    let mut b = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if c.read(&mut b).unwrap_or(0) == 0 {
            return;
        }
        head.push(b[0]);
    }
    let head = String::from_utf8_lossy(&head).into_owned();
    let path = head.split_whitespace().nth(1).unwrap_or("/").to_string();
    log.lock().unwrap().push(format!("GET {path}"));
    if path != "/" {
        let (status, ct, body) = match plan.files.iter().find(|f| f.0 == path) {
            Some(f) => ("200 OK", "application/javascript", f.1),
            None => ("404 Not Found", "text/plain", "no"),
        };
        let _ = write!(c, "HTTP/1.1 {status}\r\nContent-Type: {ct}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
        return;
    }
    let total: usize = plan.pieces.iter().map(Vec::len).sum::<usize>() + plan.short_by;
    let _ = write!(c, "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {total}\r\nConnection: close\r\n\r\n", plan.content_type);
    for (i, p) in plan.pieces.iter().enumerate() {
        if c.write_all(p).and_then(|_| c.flush()).is_err() {
            return;
        }
        log.lock().unwrap().push(format!("piece {i}"));
        if let Some((at, want)) = plan.hold
            && at == i {
            let end = Instant::now() + Duration::from_secs(5);
            while Instant::now() < end && !log.lock().unwrap().contains(&format!("GET {want}")) {
                std::thread::sleep(Duration::from_millis(2));
            }
        }
        std::thread::sleep(plan.gap);
    }
    log.lock().unwrap().push("end".into());
}

fn cfg(port: u16, live: bool) -> SessionConfig {
    let mut c = SessionConfig::new(&format!("http://127.0.0.1:{port}/"));
    c.virtual_clock = !live;
    c.live = live;
    c.eval = EvalLimits { max_steps: 5_000_000, ..Default::default() };
    c.page.quiet = Duration::from_millis(100);
    c
}

/// Load the document and give the stop of the first turn that ends it badly (or `Settled`). A live session takes turns until the page has loaded.
fn load(mut s: Session) -> (Session, SessionStop) {
    let r = s.run();
    let mut stop = r.stop;
    let end = Instant::now() + Duration::from_secs(10);
    while live_waits(&mut s, &stop) && Instant::now() < end {
        stop = s.resume(&[]).stop;
        std::thread::sleep(Duration::from_millis(1));
    }
    (s, stop)
}

fn live_waits(s: &mut Session, stop: &SessionStop) -> bool {
    matches!(stop, SessionStop::Page(crate::Stop::Settled)) && (s.loading().is_some() || !s.browser().is_some_and(|b| b.loaded()))
}

fn item(s: &Session, port: u16, key: &str) -> Option<String> {
    s.storage().get_item(false, &format!("http://127.0.0.1:{port}"), key)
}

/// 1,100 bytes that the encoding sniff (the first 1,024 bytes) passes at once.
fn pad() -> Vec<u8> {
    format!("<!--{}-->", "x".repeat(1100)).into_bytes()
}

#[test]
fn a_script_in_the_first_piece_is_requested_before_the_last_piece_is_sent() {
    big(|| {
        let mut first = pad();
        first.extend_from_slice(b"<head><script src=/a.js></script></head><body>");
        let mut plan = Plan::new(&[&first, b"<p id=x>x</p>", b"</body>"]);
        plan.hold = Some((0, "/a.js"));
        plan.files = vec![("/a.js", "localStorage.setItem('a', 'ran')")];
        let srv = slow(plan);
        let (s, stop) = load(Session::new(cfg(srv.port, true)));
        assert_eq!(stop, SessionStop::Page(crate::Stop::Settled));
        let log = srv.log.lock().unwrap().clone();
        let at = |what: &str| log.iter().position(|l| l == what).unwrap_or_else(|| panic!("{what} not in {log:?}"));
        assert!(at("GET /a.js") < at("piece 1"), "the script was requested after the document went on: {log:?}");
        assert_eq!(item(&s, srv.port, "a").as_deref(), Some("ran"));
    });
}

/// The same page, one time in one piece (not live), one time in pieces that the server spaces out (live). Same result.
const PAGE: &str = "<script>var L=[document.readyState];document.addEventListener('DOMContentLoaded',function(){L.push('dcl:'+document.readyState)});window.addEventListener('load',function(){L.push('load:'+document.readyState);localStorage.setItem('log',L.join())});document.write('<b id=w>w</b>')</script>\
<script src=/a.js></script><p id=t>caf\u{e9} \u{1F600} caf\u{e9}</p><script defer src=/d.js></script><script>L.push('inline:'+document.getElementById('t').textContent+':'+(document.getElementById('w')!==null))</script>";

fn page_plan(cut: &[usize], content_type: &'static str) -> Plan {
    let mut bytes = pad();
    bytes.extend_from_slice(PAGE.as_bytes());
    let (mut pieces, mut from) = (Vec::new(), 0);
    for &c in cut {
        pieces.push(&bytes[from..c]);
        from = c;
    }
    pieces.push(&bytes[from..]);
    let mut plan = Plan::new(&pieces);
    plan.content_type = content_type;
    plan.files = vec![("/a.js", "L.push('a')"), ("/d.js", "L.push('d')")];
    plan
}

fn log_of(live: bool, cut: &[usize]) -> String {
    let mut plan = page_plan(cut, "text/html; charset=utf-8");
    plan.gap = Duration::from_millis(if live { 5 } else { 0 });
    let srv = slow(plan);
    let mut c = cfg(srv.port, live);
    c.page.slice = Duration::from_millis(8);
    let (s, stop) = load(Session::new(c));
    assert_eq!(stop, SessionStop::Page(crate::Stop::Settled));
    item(&s, srv.port, "log").unwrap_or_else(|| "no log".into())
}

#[test]
fn a_document_in_pieces_runs_its_scripts_in_the_order_of_one_piece() {
    big(|| {
        let whole = log_of(false, &[]);
        assert!(whole.starts_with("loading,"), "{whole}");
        // Cuts inside a tag, inside a script, and inside the two letters of a UTF-8 text (the bytes of an `e` with an accent and of an emoji).
        let at = |needle: &str| pad().len() + PAGE.find(needle).unwrap();
        let cuts = [at("<script src=/a.js") + 9, at("caf\u{e9}") + 4, at("\u{1F600}") + 2, at("defer") + 2, at("<script>L.push") + 10];
        assert_eq!(log_of(true, &cuts), whole);
        assert_eq!(log_of(true, &[1300, 1450]), whole);
    });
}

#[test]
fn a_page_in_windows_1252_that_is_cut_in_pieces_gives_the_text_of_one_piece() {
    big(|| {
        let plan = |pieces: &[&[u8]]| {
            let mut p = Plan::new(pieces);
            p.content_type = "text/html";
            p
        };
        let tail: &[u8] = b"\xE9</p><script>localStorage.setItem('t', document.getElementById('t').textContent)</script>";
        // The meta tag is in the first 1,024 bytes only when the padding is short, so the padding comes after it.
        let mut early = b"<meta charset=windows-1252>".to_vec();
        early.extend_from_slice(&pad());
        early.extend_from_slice(b"<p id=t>caf");
        let srv = slow(plan(&[&early, tail]));
        let (s, stop) = load(Session::new(cfg(srv.port, true)));
        assert_eq!(stop, SessionStop::Page(crate::Stop::Settled));
        assert_eq!(item(&s, srv.port, "t").as_deref(), Some("caf\u{e9}"));
    });
}

#[test]
fn a_document_over_the_limit_or_cut_short_fails_to_load() {
    big(|| {
        let mut first = pad();
        first.extend_from_slice(b"<p>a</p>");
        let srv = slow(Plan::new(&[&first, &first, &first]));
        let mut c = cfg(srv.port, true);
        c.limits.max_document_bytes = 2000;
        let (_, stop) = load(Session::new(c));
        assert!(matches!(stop, SessionStop::LoadFailed(_)), "{stop:?}");
        let mut plan = Plan::new(&[&first]);
        plan.short_by = 50;
        let srv = slow(plan);
        let (_, stop) = load(Session::new(cfg(srv.port, true)));
        assert!(matches!(stop, SessionStop::LoadFailed(_)), "{stop:?}");
    });
}
