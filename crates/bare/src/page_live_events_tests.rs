//! A live page frees its events when each turn is short. On a busy machine a turn of `Page::tick` ends after one round, so a
//! collection that waits for a count of rounds in one turn never comes, and a `MessageChannel` loop stopped on the `dom-events` cap.

use crate::browser_session::SessionConfig;
use crate::eng_new::Peek;
use crate::page::Page;
use crate::resources::test_server::{Reply, serve};
use std::time::{Duration, Instant};

const PAGE: &str = "<!DOCTYPE html><title>t</title><script>window.n = 0; const c = new MessageChannel(); c.port1.onmessage = () => { n++; c.port2.postMessage(0) }; c.port2.postMessage(0)</script>";

#[test]
fn a_live_page_with_one_round_per_turn_frees_its_events() {
    let (n, stopped) = std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(|| {
            let server = serve(|_| Reply::new(200, Some("text/html"), PAGE));
            let mut cfg = SessionConfig::new(&server.url("/"));
            // Each turn runs about one round: a round of message tasks takes longer than the slice.
            cfg.page.slice = Duration::from_micros(50);
            let mut p = Page::open_cfg(cfg, (800, 600)).unwrap();
            let _ = p.wait_load(Duration::from_secs(5));
            let end = Instant::now() + Duration::from_secs(120);
            let mut n = 0.0;
            while Instant::now() < end && n < 40_000.0 && p.stopped().is_none() {
                p.tick();
                let b = p.browser().unwrap();
                let v = b.run_script("n").unwrap();
                n = b.it.display(v).parse().unwrap();
            }
            (n, p.stopped())
        })
        .unwrap()
        .join()
        .unwrap();
    assert_eq!(stopped, None, "after {n} tasks");
    assert!(n >= 40_000.0, "the page ran only {n} tasks");
}
