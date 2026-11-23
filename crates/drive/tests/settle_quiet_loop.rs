//! `snap::settle` on a page that keeps a quiet loop: an animation frame loop or an interval that changes nothing. The engine's settle
//! rule (`Browser::blocks_settle`) skips such a loop. `settle` waited for any task due within 20 ms, so it ran to its 300 ms limit on
//! each call (bare#959: every `snapshot` of github.com took about 320 ms).

mod common;

use bare::page::Page;
use std::time::{Duration, Instant};

fn route(_: &str, target: &str, _: &str, _: Option<&str>) -> (u16, &'static str, Vec<u8>) {
    let body: &[u8] = match target {
        "/frames" => b"<!DOCTYPE html><title>t</title><p>x</p><script>function f() { requestAnimationFrame(f) } f()</script>",
        _ => b"<!DOCTYPE html><title>t</title><p>x</p><script>setInterval(function () {}, 16)</script>",
    };
    (200, "text/html", body.to_vec())
}

/// The time of a second `settle`, after a first one that gave the loop its verdict.
fn second_settle(path: &'static str) -> Duration {
    let origin = common::serve_route(route);
    std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(move || {
            let mut p = Page::open(&format!("{origin}{path}"), (800, 600)).unwrap();
            let _ = p.wait_load(Duration::from_secs(5));
            bare_drive::snap::settle(&mut p);
            let start = Instant::now();
            bare_drive::snap::settle(&mut p);
            start.elapsed()
        })
        .unwrap()
        .join()
        .unwrap()
}

#[test]
fn settle_skips_a_quiet_animation_frame_loop() {
    let t = second_settle("/frames");
    assert!(t < Duration::from_millis(150), "settle took {t:?}");
}

#[test]
fn settle_skips_a_quiet_interval() {
    let t = second_settle("/interval");
    assert!(t < Duration::from_millis(150), "settle took {t:?}");
}
