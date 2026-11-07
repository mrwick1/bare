//! Input does not wait for the network: an action returns after its listeners, its microtasks and the tasks that are due, and the
//! replies, the timers and a navigation go on in `Page::tick`. A loopback server answers `/slow` after 500 ms.

use bare::control::Handle;
use bare::page::Page;
use bare_drive::run::{Options, Redactor, run};
use bare_drive::steps::parse;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

const SLOW: Duration = Duration::from_millis(500);

const INDEX: &str = r#"<!doctype html><html><body>
<button id="load">Load</button> <button id="count">Count</button> <a id="go" href="/next" style="display:inline-block">Next</a>
<input id="q"> <p id="out">none</p> <p id="n">0</p> <p id="got">0</p>
<script>
document.getElementById('load').addEventListener('click', function () {
  fetch('/slow').then(function (r) { return r.text() }).then(function (t) { document.getElementById('out').textContent = t })
})
document.getElementById('count').addEventListener('click', function () {
  var n = document.getElementById('n'); n.textContent = String(Number(n.textContent) + 1)
})
document.getElementById('q').addEventListener('input', function (e) {
  fetch('/slow?q=' + encodeURIComponent(e.target.value)).then(function (r) { return r.text() }).then(function () {
    var g = document.getElementById('got'); g.textContent = String(Number(g.textContent) + 1)
  })
})
</script></body></html>"#;

const NEXT: &str = "<!doctype html><html><body><p id=done>next page</p></body></html>";

fn serve() -> String {
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
        let path = head.split(' ').nth(1).unwrap_or("/").to_string();
        let (ty, body) = match path.split('?').next().unwrap() {
            "/" => ("text/html", INDEX),
            "/next" => {
                std::thread::sleep(SLOW);
                ("text/html", NEXT)
            }
            _ => {
                std::thread::sleep(SLOW);
                ("text/plain", "fetched text")
            }
        };
        let r = format!("HTTP/1.1 200 OK\r\nContent-Type: {ty}\r\nContent-Length: {}\r\n\r\n{body}", body.len());
        if s.write_all(r.as_bytes()).is_err() {
            return;
        }
    }
}

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn one(p: &mut Page, sel: &str) -> Handle {
    let l = p.query_all(sel).unwrap();
    assert_eq!(l.len(), 1, "{sel}");
    l[0]
}

fn text(p: &mut Page, id: &str) -> String {
    let b = p.browser().unwrap();
    let v = b.run_script(&format!("document.getElementById('{id}') ? document.getElementById('{id}').textContent : ''")).unwrap();
    b.it.display(v)
}

/// Tick until `done` holds or 5 s pass. The page must never be idle while it waits: a reply in flight is queued work.
fn until(p: &mut Page, mut done: impl FnMut(&mut Page) -> bool) -> Duration {
    let t = Instant::now();
    while !done(p) {
        assert!(t.elapsed() < Duration::from_secs(5), "time limit");
        let (ran, next) = p.tick();
        if !ran {
            let next = next.expect("a reply in flight is queued work: the page is not idle");
            std::thread::sleep(next.saturating_duration_since(Instant::now()));
        }
    }
    t.elapsed()
}

#[test]
fn a_click_that_starts_a_slow_fetch_returns_at_once() {
    let origin = serve();
    big(move || {
        let mut p = Page::open(&format!("{origin}/"), (800, 600)).unwrap();
        p.quiet = true;
        let (load, count) = (one(&mut p, "#load"), one(&mut p, "#count"));
        let t = Instant::now();
        p.click(load).unwrap();
        let click = t.elapsed();
        assert!(click < Duration::from_millis(50), "the click took {click:?}");
        // A second input is handled while the reply is on its way.
        p.click(count).unwrap();
        assert_eq!(text(&mut p, "n"), "1");
        assert_eq!(text(&mut p, "out"), "none", "the reply came before the second click");
        until(&mut p, |p| text(p, "out") == "fetched text");
        assert!(t.elapsed() >= SLOW);
        // Nothing is queued now: the page is idle.
        assert_eq!(p.tick().1, None);
    });
}

#[test]
fn typing_does_not_wait_for_the_fetch_of_each_key() {
    let origin = serve();
    big(move || {
        let mut p = Page::open(&format!("{origin}/"), (800, 600)).unwrap();
        p.quiet = true;
        let q = one(&mut p, "#q");
        let t = Instant::now();
        p.type_text(q, "abcdefghijklmnopqrst").unwrap();
        let typed = t.elapsed();
        assert!(typed < SLOW, "typing 20 characters took {typed:?}");
        assert_eq!(p.value(q).as_deref(), Some("abcdefghijklmnopqrst"));
        until(&mut p, |p| text(p, "got") == "20");
    });
}

#[test]
fn a_link_loads_the_next_document_in_the_background() {
    let origin = serve();
    big(move || {
        let mut p = Page::open(&format!("{origin}/"), (800, 600)).unwrap();
        p.quiet = true;
        let (go, doc) = (one(&mut p, "#go"), p.generation());
        let t = Instant::now();
        p.click(go).unwrap();
        let click = t.elapsed();
        assert!(click < Duration::from_millis(50), "the click took {click:?}");
        // The old document stays until the new one arrives.
        assert_eq!(p.generation(), doc);
        until(&mut p, |p| text(p, "done") == "next page");
        assert!(p.url().unwrap().ends_with("/next"));
    });
}

#[test]
fn a_driver_click_step_returns_and_the_expect_waits_for_the_reply() {
    let origin = serve();
    let rep = big(move || {
        let src = "open ${ORIGIN}/\nclick css=\"#load\"\nclick css=\"#count\"\nexpect js \"document.getElementById('n').textContent === '1'\"\nexpect text=\"fetched text\" visible\n";
        let steps = parse(src, &HashMap::from([("ORIGIN".to_string(), origin)])).unwrap();
        let opt = Options { size: (800, 600), timeout: Duration::from_secs(5), out: None, window: false, progress: false };
        run(&steps, &opt, &mut Redactor::default())
    });
    assert!(rep.ok, "{:?}", rep.failure.as_ref().map(|f| &f.1.message));
    let ms: Vec<f64> = rep.steps.iter().map(|s| s.ms).collect();
    assert!(ms[1] < 50.0 && ms[2] < 50.0 && ms[3] < 50.0, "step times {ms:?}");
    assert!(ms[4] >= 400.0, "the expect waited for the reply: {ms:?}");
}
