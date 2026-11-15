//! `Page::wait_load` ends at its time limit also when every tick has work. A split document that waits for a slow script has a timer that
//! is due, so each tick "ran" and the loop `continue`d: it checked the limit only after a tick that did nothing, and so it ran until the
//! script came (for ever, if it never came).

mod common;

use bare::page::Page;
use std::time::{Duration, Instant};

fn route(_: &str, target: &str, _: &str, _: Option<&str>) -> (u16, &'static str, Vec<u8>) {
    match target {
        // The script that holds the document.
        "/slow.js" => {
            std::thread::sleep(Duration::from_secs(3));
            (200, "application/javascript", b"1".to_vec())
        }
        _ => (200, "text/html", b"<!DOCTYPE html><title>t</title><script>setTimeout(function () {}, 0)</script><script src=/slow.js></script>".to_vec()),
    }
}

#[test]
fn wait_load_stops_at_its_limit_while_ticks_have_work() {
    let origin = common::serve_route(route);
    let (r, took) = std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(move || {
            let mut p = Page::open(&format!("{origin}/"), (800, 600)).unwrap();
            let start = Instant::now();
            let r = p.wait_load(Duration::from_millis(300));
            (r, start.elapsed())
        })
        .unwrap()
        .join()
        .unwrap();
    assert!(r.is_err(), "the page loaded: {r:?} after {took:?}");
    assert!(took < Duration::from_millis(1500), "wait_load took {took:?} for a limit of 300 ms");
}
