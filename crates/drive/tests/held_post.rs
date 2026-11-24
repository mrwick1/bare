//! A live page must not hold later requests behind one that hangs (issue #461). The server holds `/hold` for 20 s. Each request
//! made after it must go out and settle at once, whatever the method and the API: a browser sends them on their own connections.

mod common;

use bare::page::Page;
use std::time::{Duration, Instant};

const INDEX: &str = r#"<!doctype html><title>t</title><p id="out"></p><script>
var got = [];
function xhr(m, u, body) { var x = new XMLHttpRequest(); x.open(m, u); x.onload = function () { got.push(m + ' ' + u + ' ' + x.responseText); document.getElementById('out').textContent = got.sort().join('|') }; x.send(body) }
function f(m, u) { fetch(u, { method: m, body: m == 'POST' ? 'b' : undefined }).then(function (r) { return r.text() }).then(function (t) { got.push('fetch ' + m + ' ' + u + ' ' + t); document.getElementById('out').textContent = got.sort().join('|') }) }
</script>"#;

fn route(_: &str, target: &str, _: &str, _: Option<&str>) -> (u16, &'static str, Vec<u8>) {
    if target.starts_with("/hold") {
        std::thread::sleep(Duration::from_secs(20));
        return (200, "text/plain", b"late".to_vec());
    }
    if target.starts_with("/ok") {
        return (200, "text/plain", b"ok".to_vec());
    }
    (200, "text/html", INDEX.as_bytes().to_vec())
}

fn text(p: &mut Page) -> String {
    let b = p.browser().unwrap();
    let v = b.run_script("document.getElementById('out').textContent").unwrap();
    b.it.display(v)
}

fn tick_until(p: &mut Page, limit: Duration, mut done: impl FnMut(&mut Page) -> bool) -> Option<Duration> {
    let t = Instant::now();
    while t.elapsed() < limit {
        if done(p) {
            return Some(t.elapsed());
        }
        let (ran, next) = p.tick();
        if !ran {
            std::thread::sleep(next.map_or(Duration::from_millis(5), |n| n.saturating_duration_since(Instant::now())).min(Duration::from_millis(20)));
        }
    }
    None
}

/// Open the page, run `hold` (a request that hangs), then `after`, and give how long `after` took to reach `want`.
fn after_a_hang(hold: &'static str, after: &'static str, want: &'static str) -> Option<Duration> {
    let origin = common::serve_route(route);
    std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(move || {
            let mut p = Page::open(&format!("{origin}/"), (800, 600)).unwrap();
            p.quiet = true;
            assert!(tick_until(&mut p, Duration::from_secs(5), |p| p.idle().is_ok()).is_some(), "the page did not settle");
            p.browser().unwrap().run_script(hold).unwrap();
            tick_until(&mut p, Duration::from_millis(300), |_| false);
            p.browser().unwrap().run_script(after).unwrap();
            tick_until(&mut p, Duration::from_secs(6), |p| text(p) == want)
        })
        .unwrap()
        .join()
        .unwrap()
}

#[test]
fn a_post_after_a_hung_xhr_post_goes_out_at_once() {
    let t = after_a_hang("xhr('POST', '/hold', 'x')", "xhr('POST', '/ok', 'y')", "POST /ok ok");
    assert!(t.is_some_and(|t| t < Duration::from_secs(2)), "the POST waited for the hung POST: {t:?}");
}

#[test]
fn requests_after_a_hung_fetch_post_go_out_at_once() {
    let t = after_a_hang("f('POST', '/hold')", "xhr('POST', '/ok', 'y'); f('POST', '/ok2'); f('GET', '/ok3')", "POST /ok ok|fetch GET /ok3 ok|fetch POST /ok2 ok");
    assert!(t.is_some_and(|t| t < Duration::from_secs(2)), "the requests waited for the hung fetch: {t:?}");
}

#[test]
fn a_post_after_a_hung_fetch_get_goes_out_at_once() {
    let t = after_a_hang("f('GET', '/hold')", "xhr('POST', '/ok', 'y')", "POST /ok ok");
    assert!(t.is_some_and(|t| t < Duration::from_secs(2)), "the POST waited for the hung GET: {t:?}");
}
