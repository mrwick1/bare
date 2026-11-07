//! The scripts of an app download once per driver run: a reload and a later `open` take a fresh copy from the HTTP cache, as Chrome
//! does in one tab. A script that is stale is asked about, and a 304 answers it. A window (`--window`) does the same as the headless
//! run. A loopback server counts the requests for each path.

use bare_drive::run::{Options, Redactor, run};
use bare_drive::steps::parse;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::sync::Arc;
use std::time::Duration;

const INDEX: &str = r#"<!doctype html><html><body><p id="n">0</p><p id="s">0</p><script src="/app.js"></script><script src="/stale.js"></script></body></html>"#;
const APP: &str = "document.getElementById('n').textContent = 'ran'";
const STALE: &str = "document.getElementById('s').textContent = 'ran'";

/// What the server saw: connections, downloads of the immutable script, and for the stale script the full downloads and the 304s.
#[derive(Default)]
struct Counts {
    conns: AtomicUsize,
    app: AtomicUsize,
    stale_full: AtomicUsize,
    stale_304: AtomicUsize,
}

fn serve(counts: Arc<Counts>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in listener.incoming().flatten() {
            counts.conns.fetch_add(1, SeqCst);
            let counts = counts.clone();
            std::thread::spawn(move || connection(s, &counts));
        }
    });
    format!("http://127.0.0.1:{port}")
}

fn connection(mut s: TcpStream, counts: &Counts) {
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
        let r = if head.starts_with("GET /app.js ") {
            counts.app.fetch_add(1, SeqCst);
            format!("HTTP/1.1 200 OK\r\nContent-Type: text/javascript\r\nCache-Control: public, max-age=31536000, immutable\r\nContent-Length: {}\r\n\r\n{APP}", APP.len())
        } else if head.starts_with("GET /stale.js ") && head.to_ascii_lowercase().contains("if-none-match: \"v1\"\r\n") {
            counts.stale_304.fetch_add(1, SeqCst);
            "HTTP/1.1 304 Not Modified\r\nETag: \"v1\"\r\nCache-Control: max-age=0\r\n\r\n".to_string()
        } else if head.starts_with("GET /stale.js ") {
            counts.stale_full.fetch_add(1, SeqCst);
            format!("HTTP/1.1 200 OK\r\nContent-Type: text/javascript\r\nCache-Control: max-age=0\r\nETag: \"v1\"\r\nContent-Length: {}\r\n\r\n{STALE}", STALE.len())
        } else {
            format!("HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\r\n{INDEX}", INDEX.len())
        };
        if s.write_all(r.as_bytes()).is_err() {
            return;
        }
    }
}

/// `open`, `reload`, `open` of the page, with a check after each that both scripts ran.
fn journey(origin: String, window: bool) -> bare_drive::run::Report {
    std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(move || {
            let ran = "expect js \"document.getElementById('n').textContent === 'ran' && document.getElementById('s').textContent === 'ran'\"\n";
            let src = format!("open ${{ORIGIN}}/\n{ran}reload\n{ran}open ${{ORIGIN}}/\n{ran}");
            let steps = parse(&src, &HashMap::from([("ORIGIN".to_string(), origin)])).unwrap();
            let opt = Options { size: (800, 600), timeout: Duration::from_secs(5), out: None, window, progress: false };
            run(&steps, &opt, &mut Redactor::default())
        })
        .unwrap()
        .join()
        .unwrap()
}

fn check(counts: &Counts, rep: &bare_drive::run::Report) {
    assert!(rep.ok, "{:?}", rep.failure.as_ref().map(|f| &f.1.message));
    assert_eq!(counts.app.load(SeqCst), 1, "the immutable script was downloaded once");
    // The stale script is asked about at the reload and at the second `open`: one download, two 304s.
    assert_eq!((counts.stale_full.load(SeqCst), counts.stale_304.load(SeqCst)), (1, 2), "full downloads and 304s of the stale script");
    // The second `open` reuses the connections of the first: no new connection for each page.
    assert!(counts.conns.load(SeqCst) <= 2, "connections: {}", counts.conns.load(SeqCst));
}

#[test]
fn a_reload_and_a_second_open_take_the_script_from_the_cache() {
    let counts = Arc::new(Counts::default());
    let rep = journey(serve(counts.clone()), false);
    check(&counts, &rep);
}

/// A navigation of the page of a window (`bare --window`: a link, the address bar) keeps the client of its session: the scripts come from
/// the cache, the stale one is asked about, and the connections stay.
#[test]
fn a_navigation_in_a_window_page_keeps_the_connections_and_the_cache() {
    let counts = Arc::new(Counts::default());
    let origin = serve(counts.clone());
    std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(move || {
            let mut page = bare::page::Page::open(&format!("{origin}/"), (800, 600)).unwrap();
            page.navigate(Some(&format!("{origin}/"))).unwrap();
            page.navigate(None).unwrap();
        })
        .unwrap()
        .join()
        .unwrap();
    assert_eq!(counts.app.load(SeqCst), 1);
    assert_eq!((counts.stale_full.load(SeqCst), counts.stale_304.load(SeqCst)), (1, 2));
    assert!(counts.conns.load(SeqCst) <= 2, "connections: {}", counts.conns.load(SeqCst));
}

/// The same journey in a window. A window needs a Wayland compositor: the test runs itself again inside `cage` with the headless
/// backend (it never touches the desktop of a person), and the inner run writes `ok` to the file that the outer run names.
#[test]
fn a_window_open_keeps_the_connections_and_the_cache() {
    let name = "a_window_open_keeps_the_connections_and_the_cache";
    if let Some(done) = std::env::var_os("BARE_CAGE_DONE") {
        let counts = Arc::new(Counts::default());
        let rep = journey(serve(counts.clone()), true);
        check(&counts, &rep);
        std::fs::write(done, "ok").unwrap();
        return;
    }
    let done = std::env::temp_dir().join(format!("bare-cage-{}", std::process::id()));
    let _ = std::fs::remove_file(&done);
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").unwrap_or_else(|| std::env::temp_dir().into_os_string());
    let status = std::process::Command::new("cage")
        .arg("--")
        .arg(std::env::current_exe().unwrap())
        .args(["--exact", name, "--nocapture"])
        .env_remove("WAYLAND_DISPLAY")
        .env("XDG_RUNTIME_DIR", runtime)
        .env("WLR_BACKENDS", "headless")
        .env("WLR_LIBINPUT_NO_DEVICES", "1")
        .env("WLR_RENDERER", "pixman")
        .env("BARE_CAGE_DONE", &done)
        .status();
    let Ok(status) = status else {
        eprintln!("cage is not installed: the window test did not run");
        return;
    };
    let result = std::fs::read_to_string(&done).ok();
    let _ = std::fs::remove_file(&done);
    assert!(result.as_deref() == Some("ok"), "the run in cage failed ({status})");
}
