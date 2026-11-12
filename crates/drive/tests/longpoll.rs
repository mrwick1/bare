//! A page that keeps an XHR long-poll open (the server holds `/poll` for 20 s) must settle, take a click, and send a later `fetch`
//! without waiting for the poll. The poll leaves the ordered queue after 3 s (`Dom::sync_detached`).

mod common;

use bare::page::Page;
use std::time::{Duration, Instant};

const INDEX: &str = r#"<!doctype html><title>t</title><button id=b onclick="document.getElementById('out').textContent='clicked'">go</button><p id="out"></p><script>
var polls = 0;
function poll() { var x = new XMLHttpRequest(); x.open('GET', '/poll'); x.onload = function () { polls++; poll() }; x.send() }
poll();
function later() { fetch('/data').then(function (r) { return r.text() }).then(function (t) { document.getElementById('out').textContent = 'later ' + t }) }
</script>"#;

fn route(_: &str, target: &str, _: &str, _: Option<&str>) -> (u16, &'static str, Vec<u8>) {
    match target {
        "/poll" => {
            std::thread::sleep(Duration::from_secs(20));
            (200, "text/plain", b"p".to_vec())
        }
        "/data" => (200, "text/plain", b"ok".to_vec()),
        _ => (200, "text/html", INDEX.as_bytes().to_vec()),
    }
}

fn text(p: &mut Page) -> String {
    let b = p.browser().unwrap();
    let v = b.run_script("document.getElementById('out').textContent").unwrap();
    b.it.display(v)
}

/// Tick until `done` or `limit`. Gives the time it took, or `None` at the limit.
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

#[test]
fn a_long_poll_does_not_keep_the_page_from_settling_or_block_later_requests() {
    let origin = common::serve_route(route);
    std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(move || {
            let mut p = Page::open(&format!("{origin}/"), (800, 600)).unwrap();
            p.quiet = true;
            let settled = tick_until(&mut p, Duration::from_secs(6), |p| p.idle().is_ok());
            assert!(settled.is_some_and(|t| t < Duration::from_secs(5)), "the page did not settle in 6 s: {:?}", p.pending().iter().map(|w| w.url.clone()).collect::<Vec<_>>());
            // The poll is open but does not hold the page.
            assert!(p.pending().iter().all(|w| !w.holds));
            // A click works.
            p.browser().unwrap().run_script("document.getElementById('b').click()").unwrap();
            assert!(tick_until(&mut p, Duration::from_secs(2), |p| text(p) == "clicked").is_some(), "the click did not run");
            // A later fetch does not wait for the poll (20 s).
            p.browser().unwrap().run_script("later()").unwrap();
            let t = tick_until(&mut p, Duration::from_secs(5), |p| text(p) == "later ok");
            assert!(t.is_some_and(|t| t < Duration::from_secs(3)), "the later fetch waited for the poll: {t:?}");
            // It settles again with the poll open.
            assert!(tick_until(&mut p, Duration::from_secs(3), |p| p.idle().is_ok()).is_some());
        })
        .unwrap()
        .join()
        .unwrap();
}
