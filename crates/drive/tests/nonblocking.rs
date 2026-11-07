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
<button id="sheet">Sheet</button> <button id="pre">Preload</button> <a id="scripts" href="/scripts" style="display:inline-block">Scripts</a>
<input id="q"> <p id="out">none</p> <p id="n">0</p> <p id="got">0</p> <p id="css">none</p>
<script>
document.getElementById('load').addEventListener('click', function () {
  fetch('/slow').then(function (r) { return r.text() }).then(function (t) { document.getElementById('out').textContent = t })
})
document.getElementById('count').addEventListener('click', function () {
  var n = document.getElementById('n'); n.textContent = String(Number(n.textContent) + 1)
})
document.getElementById('sheet').addEventListener('click', function () {
  var l = document.createElement('link'); l.rel = 'stylesheet'; l.href = '/slow.css'
  l.onload = function () { document.getElementById('css').textContent = 'loaded' }
  document.head.appendChild(l)
})
document.getElementById('pre').addEventListener('click', function () {
  var l = document.createElement('link'); l.rel = 'preload'; l.as = 'script'; l.href = '/js?p'
  l.onload = function () { document.getElementById('out').textContent = 'preloaded' }
  document.head.appendChild(l)
})
document.getElementById('q').addEventListener('input', function (e) {
  fetch('/slow?q=' + encodeURIComponent(e.target.value)).then(function (r) { return r.text() }).then(function () {
    var g = document.getElementById('got'); g.textContent = String(Number(g.textContent) + 1)
  })
})
</script></body></html>"#;

/// A document with a fast inline script, then slow classic scripts (blocking, defer, async). Each one logs its name.
const SCRIPTS: &str = r#"<!doctype html><html><body><button id="b">B</button> <p id="n">0</p> <p id="log"></p>
<script>var log = []
document.getElementById('b').addEventListener('click', function () { document.getElementById('n').textContent = 'clicked' })
document.addEventListener('DOMContentLoaded', function () { log.push('dcl') })
addEventListener('load', function () {
  log.push('load'); var e = document.getElementById('log'); e.textContent = log.join(','); e.dataset.n = sessionStorage.n = (Number(sessionStorage.n) || 0) + 1
})</script>
<script src="/js?1"></script><script defer src="/js?3"></script><script src="/js?2"></script><script async src="/js?a"></script><script>log.push('i')</script>
</body></html>"#;

/// The order of `SCRIPTS`: the blocking and inline scripts in document order, then the defer script, `DOMContentLoaded`, the async
/// script and `load`.
const ORDER: &str = "1,2,i,3,dcl,a,load";

/// A first document whose inline script adds a slow sheet and a slow preload before `load`, then a slow blocking script.
const EARLY: &str = r#"<!doctype html><html><head><script>var log = []
var l = document.createElement('link'); l.rel = 'stylesheet'; l.href = '/slow.css'; l.onload = function () { log.push('css') }
document.head.appendChild(l)
var p = document.createElement('link'); p.rel = 'preload'; p.as = 'script'; p.href = '/js?p'; p.onload = function () { log.push('pre') }
document.head.appendChild(p)
addEventListener('load', function () { log.push('load'); document.getElementById('log').textContent = log.join(',') })</script>
<script src="/js?1"></script></head><body><p id="css">css</p><p id="log"></p></body></html>"#;

/// Each input event arms a timer of 0 ms. Keys sent in one tick give `i1,i2,i3,t1,t2,t3`; one key in each tick lets the timers run between.
const TYPE: &str = r#"<!doctype html><html><body><input id="q"><p id="log"></p><script>var log = [], q = document.getElementById('q')
q.addEventListener('input', function () {
  var n = q.value.length; log.push('i' + n)
  setTimeout(function () { log.push('t' + n); document.getElementById('log').textContent = log.join(',') }, 0)
})</script></body></html>"#;

/// A red page, served after 500 ms.
const RED: &str = "<!doctype html><html><body style='background: rgb(255, 0, 0)'><p id=red>red</p></body></html>";

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
        let js;
        let (ty, body) = match path.split('?').next().unwrap() {
            "/" => ("text/html", INDEX),
            "/scripts" => ("text/html", SCRIPTS),
            "/early" => ("text/html", EARLY),
            "/type" => ("text/html", TYPE),
            "/slow.css" => {
                std::thread::sleep(SLOW);
                ("text/css", "#css { color: rgb(255, 0, 0) }")
            }
            "/js" => {
                std::thread::sleep(SLOW);
                js = format!("log.push('{}')", path.split('?').nth(1).unwrap_or(""));
                ("text/javascript", js.as_str())
            }
            "/red" => {
                std::thread::sleep(SLOW);
                ("text/html", RED)
            }
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
        p.wait_load(std::time::Duration::from_secs(10)).unwrap();
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
        p.wait_load(std::time::Duration::from_secs(10)).unwrap();
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
        p.wait_load(std::time::Duration::from_secs(10)).unwrap();
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

fn color(p: &mut Page, id: &str) -> String {
    let b = p.browser().unwrap();
    let v = b.run_script(&format!("getComputedStyle(document.getElementById('{id}')).color")).unwrap();
    b.it.display(v)
}

#[test]
fn a_style_sheet_added_after_load_does_not_block_the_turn() {
    let origin = serve();
    big(move || {
        let mut p = Page::open(&format!("{origin}/"), (800, 600)).unwrap();
        p.wait_load(std::time::Duration::from_secs(10)).unwrap();
        p.quiet = true;
        let (sheet, count) = (one(&mut p, "#sheet"), one(&mut p, "#count"));
        let before = color(&mut p, "css");
        let t = Instant::now();
        p.click(sheet).unwrap();
        let click = t.elapsed();
        assert!(click < Duration::from_millis(50), "the click took {click:?}");
        // The page takes input while the sheet is on its way, and shows the page without it.
        p.click(count).unwrap();
        assert_eq!(text(&mut p, "n"), "1");
        assert_eq!(text(&mut p, "css"), "none");
        assert_eq!(color(&mut p, "css"), before);
        until(&mut p, |p| text(p, "css") == "loaded");
        assert!(t.elapsed() >= SLOW);
        assert_eq!(color(&mut p, "css"), "rgb(255, 0, 0)");
        assert_eq!(p.tick().1, None);
    });
}

#[test]
fn a_new_document_runs_its_slow_scripts_across_ticks_and_takes_input_between_them() {
    let origin = serve();
    big(move || {
        let mut p = Page::open(&format!("{origin}/"), (800, 600)).unwrap();
        p.wait_load(std::time::Duration::from_secs(10)).unwrap();
        p.quiet = true;
        let (go, doc) = (one(&mut p, "#scripts"), p.generation());
        p.click(go).unwrap();
        until(&mut p, |p| p.generation() != doc);
        // The new document is up and its first inline script ran; the slow scripts are on their way.
        let t = Instant::now();
        let b = one(&mut p, "#b");
        p.click(b).unwrap();
        assert!(t.elapsed() < Duration::from_millis(50), "the click took {:?}", t.elapsed());
        assert_eq!(text(&mut p, "n"), "clicked");
        assert_eq!(text(&mut p, "log"), "", "the scripts were done before the click");
        // No tick blocks on a script download.
        let mut worst = Duration::ZERO;
        until(&mut p, |p| {
            let t = Instant::now();
            let done = text(p, "log") == ORDER;
            worst = worst.max(t.elapsed());
            done
        });
        let t = Instant::now();
        while p.tick().0 {}
        worst = worst.max(t.elapsed());
        assert!(worst < Duration::from_millis(50), "a tick took {worst:?}");
        assert_eq!(text(&mut p, "log"), ORDER);
    });
}

#[test]
fn a_headless_run_keeps_the_script_order() {
    let origin = serve();
    big(move || {
        let mut s = bare::browser_session::Session::new(bare::browser_session::SessionConfig::new(&format!("{origin}/scripts")));
        let r = s.run();
        assert!(matches!(r.stop, bare::browser_session::SessionStop::Page(bare::Stop::Settled)), "{:?}", r.stop);
        let b = s.browser().unwrap();
        let v = b.run_script("document.getElementById('log').textContent").unwrap();
        assert_eq!(b.it.display(v), ORDER);
    });
}

#[test]
fn a_preload_added_after_load_does_not_block_the_turn() {
    let origin = serve();
    big(move || {
        let mut p = Page::open(&format!("{origin}/"), (800, 600)).unwrap();
        p.wait_load(std::time::Duration::from_secs(10)).unwrap();
        p.quiet = true;
        let pre = one(&mut p, "#pre");
        let t = Instant::now();
        p.click(pre).unwrap();
        let click = t.elapsed();
        assert!(click < Duration::from_millis(50), "the click took {click:?}");
        assert_eq!(text(&mut p, "out"), "none");
        until(&mut p, |p| text(p, "out") == "preloaded");
        assert!(t.elapsed() >= SLOW);
        assert_eq!(p.tick().1, None);
    });
}

#[test]
fn a_sheet_and_a_preload_added_before_load_download_with_the_scripts() {
    let origin = serve();
    big(move || {
        let t = Instant::now();
        let mut p = Page::open(&format!("{origin}/early"), (800, 600)).unwrap();
        p.wait_load(std::time::Duration::from_secs(10)).unwrap();
        let took = t.elapsed();
        p.quiet = true;
        // The sheet, the preload and the script are on their way at once: one wait of 500 ms, not three.
        assert!(took < Duration::from_millis(900), "the open took {took:?}");
        let log = text(&mut p, "log");
        assert!(log.ends_with("load") && log.contains("css") && log.contains("pre") && log.contains('1'), "{log}");
        assert_eq!(color(&mut p, "css"), "rgb(255, 0, 0)");
    });
}

/// The steps that load a document, the slow scripts of `/scripts` and a check after each. The `n` of the log counts the loads of the session.
fn loads(origin: String, window: bool) -> bare_drive::run::Report {
    let done = |n: u32| format!("expect js \"document.getElementById('log').textContent === '{ORDER}' && document.getElementById('log').dataset.n === '{n}'\"\n");
    let src = format!("open ${{ORIGIN}}/scripts\n{}goto ${{ORIGIN}}/scripts\n{}reload\n{}", done(1), done(2), done(3));
    let steps = parse(&src, &HashMap::from([("ORIGIN".to_string(), origin)])).unwrap();
    let opt = Options { size: (800, 600), timeout: Duration::from_secs(5), out: None, window, progress: false };
    run(&steps, &opt, &mut Redactor::default())
}

/// `open`, `goto` and `reload` return at once. The download, the parse and the slow scripts run in the ticks of the next step, and its
/// `expect` passes once the document is in. In a window, the first `open` also makes the window, so only `goto` and `reload` are timed.
fn check_loads(rep: &bare_drive::run::Report, window: bool) {
    assert!(rep.ok, "{:?}", rep.failure.as_ref().map(|f| &f.1.message));
    let ms: Vec<f64> = rep.steps.iter().map(|s| s.ms).collect();
    let timed: &[usize] = if window { &[2, 4] } else { &[0, 2, 4] };
    for &i in timed {
        assert!(ms[i] < 50.0, "step {} took {:.1} ms: {ms:?}", rep.steps[i].text, ms[i]);
    }
    for i in [1, 3, 5] {
        assert!(ms[i] >= 400.0, "the expect waited for the scripts: {ms:?}");
    }
}

#[test]
fn a_driver_open_goto_and_reload_return_at_once() {
    let origin = serve();
    let rep = big(move || loads(origin, false));
    check_loads(&rep, false);
}

/// A window needs a Wayland compositor: the test runs itself again inside `cage` with the headless backend, and the inner run writes `ok`
/// to the file that the outer run names (as in `window_wait.rs`).
#[test]
fn a_window_goto_and_reload_return_at_once() {
    let name = "a_window_goto_and_reload_return_at_once";
    if let Some(done) = std::env::var_os("BARE_CAGE_DONE") {
        let origin = serve();
        let rep = big(move || loads(origin, true));
        check_loads(&rep, true);
        std::fs::write(done, "ok").unwrap();
        return;
    }
    let done = std::env::temp_dir().join(format!("bare-cage-loads-{}", std::process::id()));
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

/// The window paints the old frame until the next document is in: after an address bar navigation (`navigate`) and after a window
/// `open` (`reopen_in`). Both return at once.
#[test]
fn the_window_keeps_the_old_frame_until_the_next_document_is_in() {
    const W: u32 = 200;
    const H: u32 = 100;
    let origin = serve();
    big(move || {
        let (red, white) = (0xFFFF_0000u32, 0xFFFF_FFFFu32);
        let px = |p: &mut Page| p.capture().pixels[(10 * W + 10) as usize];
        let mut p = Page::open(&format!("{origin}/red"), (W, H)).unwrap();
        p.wait_load(std::time::Duration::from_secs(10)).unwrap();
        p.quiet = true;
        until(&mut p, |p| p.browser().is_some());
        assert_eq!(px(&mut p), red);
        let t = Instant::now();
        p.navigate(Some(&format!("{origin}/next"))).unwrap();
        assert!(t.elapsed() < Duration::from_millis(50), "the navigation took {:?}", t.elapsed());
        assert_eq!(px(&mut p), red, "the old frame went before the next document was in");
        until(&mut p, |p| p.url().is_some_and(|u| u.ends_with("/next")));
        assert_eq!(px(&mut p), white);
        let t = Instant::now();
        p.reopen_in(&format!("{origin}/red"), &bare_net::http::Client::new()).unwrap();
        assert!(t.elapsed() < Duration::from_millis(50), "the open took {:?}", t.elapsed());
        assert_eq!(px(&mut p), white, "the old frame went before the next document was in");
        until(&mut p, |p| p.browser().is_some());
        assert_eq!(px(&mut p), red);
    });
}

/// The driver types one key in each tick, as a person does: the tasks of the page run between two keys.
#[test]
fn a_driver_type_step_sends_one_key_in_each_tick() {
    let origin = serve();
    let rep = big(move || {
        let src = "open ${ORIGIN}/type\ntype css=\"#q\" \"abc\"\nexpect js \"document.getElementById('log').textContent === 'i1,t1,i2,t2,i3,t3'\"\n";
        let steps = parse(src, &HashMap::from([("ORIGIN".to_string(), origin)])).unwrap();
        let opt = Options { size: (800, 600), timeout: Duration::from_secs(5), out: None, window: false, progress: false };
        run(&steps, &opt, &mut Redactor::default())
    });
    assert!(rep.ok, "{:?}", rep.failure.as_ref().map(|f| &f.1.message));
    assert!(rep.steps[1].ticks.n >= 3, "{:?}", rep.steps[1].ticks);
}
