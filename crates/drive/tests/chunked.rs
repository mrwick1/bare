//! A live session parses a new document in slices: the page takes input between them, the downloads of the scripts start while the
//! parse goes on, and the order of the scripts and events is the same as in a one-step parse. A loopback server answers each `/js` file
//! after 300 ms. `/big` is about 2 MB of HTML.

use bare::browser_session::{Session, SessionConfig, SessionStop};
use bare::page::Page;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

const SLOW: Duration = Duration::from_millis(300);

const INDEX: &str = r#"<!doctype html><html><body><a id="big" href="/big" style="display:inline-block">Big</a>
<button id="count">Count</button> <p id="n">0</p>
<script>document.getElementById('count').addEventListener('click', function () {
  var n = document.getElementById('n'); n.textContent = String(Number(n.textContent) + 1)
})</script></body></html>"#;

/// The order of `/big`: each `readyState` change, the blocking and inline scripts in document order, the defer script, `DOMContentLoaded`,
/// the async script and `load`.
const ORDER: &str = "i1,1,2,i2,rs:interactive,3,dcl,a,rs:complete,load";

/// A document of about 2 MB: scripts at the start, in the middle and at the end, with rows of markup between them.
fn big() -> &'static str {
    static BIG: OnceLock<String> = OnceLock::new();
    BIG.get_or_init(|| {
        let rows = |from: usize, n: usize| (from..from + n).map(|i| format!("<div class=\"r\"><span>row {i}</span> <b>x</b></div>\n")).collect::<String>();
        let mut s = String::from(r#"<!doctype html><html><head><title>big</title><script>var log = []
document.addEventListener('readystatechange', function () { log.push('rs:' + document.readyState) })
document.addEventListener('DOMContentLoaded', function () { log.push('dcl') })
addEventListener('load', function () { log.push('load'); document.getElementById('log').textContent = log.join(',') })
log.push('i1')</script><script src="/js?1"></script></head><body><p id="log"></p>"#);
        s += &rows(0, 20_000);
        s += r#"<script src="/js?2"></script><script defer src="/js?3"></script><script async src="/js?a"></script>"#;
        s += &rows(20_000, 20_000);
        s += "<script>log.push('i2')</script><button id=\"b\">B</button></body></html>";
        s
    })
}

/// When the server saw the first request for each path and query.
type Seen = Arc<Mutex<Vec<(String, Instant)>>>;

fn serve() -> (String, Seen) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen: Seen = Arc::default();
    let s2 = seen.clone();
    std::thread::spawn(move || {
        for s in listener.incoming().flatten() {
            let seen = s2.clone();
            std::thread::spawn(move || connection(s, seen));
        }
    });
    (format!("http://127.0.0.1:{port}"), seen)
}

fn connection(mut s: TcpStream, seen: Seen) {
    s.set_nodelay(true).ok();
    let (mut buf, mut piece) = (Vec::new(), [0u8; 4096]);
    loop {
        let end = loop {
            if let Some(at) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break at + 4;
            }
            match s.read(&mut piece) {
                Ok(0) | Err(_) => return,
                Ok(n) => buf.extend_from_slice(&piece[..n]),
            }
        };
        let head = String::from_utf8_lossy(&buf[..end]).to_string();
        buf.drain(..end);
        let path = head.split(' ').nth(1).unwrap_or("/").to_string();
        seen.lock().unwrap().push((path.clone(), Instant::now()));
        let js;
        let (ty, body) = match path.split('?').next().unwrap() {
            "/" => ("text/html", INDEX),
            "/big" => ("text/html", big()),
            "/js" => {
                std::thread::sleep(SLOW);
                js = format!("log.push('{}')", path.split('?').nth(1).unwrap_or(""));
                ("text/javascript", js.as_str())
            }
            _ => ("text/plain", "none"),
        };
        let r = format!("HTTP/1.1 200 OK\r\nContent-Type: {ty}\r\nContent-Length: {}\r\n\r\n", body.len());
        if s.write_all(r.as_bytes()).and_then(|_| s.write_all(body.as_bytes())).is_err() {
            return;
        }
    }
}

fn run_big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn text(p: &mut Page, id: &str) -> String {
    let b = p.browser().unwrap();
    let v = b.run_script(&format!("document.getElementById('{id}') ? document.getElementById('{id}').textContent : ''")).unwrap();
    b.it.display(v)
}

fn click(p: &mut Page, sel: &str) -> Duration {
    let l = p.query_all(sel).unwrap();
    assert_eq!(l.len(), 1, "{sel}");
    let t = Instant::now();
    p.click(l[0]).unwrap();
    t.elapsed()
}

/// One tick, timed. Sleeps until the next call when the tick ran nothing.
fn tick(p: &mut Page, worst: &mut Duration) -> bool {
    let t = Instant::now();
    let (ran, next) = p.tick();
    *worst = (*worst).max(t.elapsed());
    if !ran {
        let next = next.expect("a document or a reply in flight is queued work: the page is not idle");
        std::thread::sleep(next.saturating_duration_since(Instant::now()));
    }
    ran
}

#[test]
fn a_big_document_parses_across_ticks_and_takes_input_during_the_parse() {
    let (origin, seen) = serve();
    let (worst, parse_ticks, clicked, log, js2_before_up) = run_big(move || {
        let mut p = Page::open(&format!("{origin}/"), (800, 600)).unwrap();
        p.wait_load(std::time::Duration::from_secs(10)).unwrap();
        p.quiet = true;
        let doc = p.generation();
        click(&mut p, "#big");
        let (t, mut worst, mut parse_ticks, mut clicked) = (Instant::now(), Duration::ZERO, 0, None);
        // The document downloads, then each tick parses a slice of it. The old document is on screen and takes input meanwhile.
        while p.generation() == doc {
            assert!(t.elapsed() < Duration::from_secs(10), "time limit");
            if tick(&mut p, &mut worst) && p.generation() == doc {
                parse_ticks += 1;
                if clicked.is_none() {
                    clicked = Some((click(&mut p, "#count"), text(&mut p, "n")));
                }
            }
        }
        let up = Instant::now();
        let js2_before_up = seen.lock().unwrap().iter().any(|(path, at)| path == "/js?2" && *at < up);
        while text(&mut p, "log").is_empty() {
            assert!(t.elapsed() < Duration::from_secs(10), "time limit");
            tick(&mut p, &mut worst);
        }
        (worst, parse_ticks, clicked, text(&mut p, "log"), js2_before_up)
    });
    eprintln!("parse ticks {parse_ticks}, longest tick {worst:?}, click {clicked:?}");
    assert!(parse_ticks >= 2, "the parse took {parse_ticks} ticks");
    let (took, n) = clicked.expect("a click during the parse");
    assert!(took < Duration::from_millis(50), "the click took {took:?}");
    assert_eq!(n, "1", "the click was handled");
    assert!(worst < Duration::from_millis(50), "a tick took {worst:?}");
    assert!(js2_before_up, "the download of a script in the middle of the document started during the parse");
    assert_eq!(log, ORDER);
}

#[test]
fn a_chunked_parse_gives_the_order_and_the_dom_of_a_one_step_parse() {
    let (origin, _) = serve();
    let o2 = origin.clone();
    let (live_log, live_dom) = run_big(move || {
        let mut p = Page::open(&format!("{o2}/"), (800, 600)).unwrap();
        p.wait_load(std::time::Duration::from_secs(10)).unwrap();
        p.quiet = true;
        click(&mut p, "#big");
        let (t, mut worst) = (Instant::now(), Duration::ZERO);
        while !p.url().unwrap().ends_with("/big") || text(&mut p, "log").is_empty() {
            assert!(t.elapsed() < Duration::from_secs(10), "time limit");
            tick(&mut p, &mut worst);
        }
        let dom = p.browser().unwrap().document().borrow().dump();
        (text(&mut p, "log"), dom)
    });
    let (head_log, head_dom) = run_big(move || {
        let mut s = Session::new(SessionConfig::new(&format!("{origin}/big")));
        let r = s.run();
        assert!(matches!(r.stop, SessionStop::Page(bare::Stop::Settled)), "{:?}", r.stop);
        let b = s.browser().unwrap();
        let v = b.run_script("document.getElementById('log').textContent").unwrap();
        (b.it.display(v), b.document().borrow().dump())
    });
    assert_eq!(head_log, ORDER);
    assert_eq!(live_log, head_log);
    assert!(live_dom == head_dom, "the DOM of the chunked parse differs from the one-step parse");
}
