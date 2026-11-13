//! A live page runs for as long as its session: the budgets of one page load (100k tasks, 100k rounds, 60 s) do not end it. Since a
//! live page runs between tool calls (#474), Gmail used them up in about two minutes. The session then ended without a word: no timer,
//! reply or upload ran any more, and each wait said that the page was idle.

mod common;

use bare::page::Page;
use std::time::{Duration, Instant};

/// A chain of message tasks with no clamp (a timer chain would be clamped to 4 ms): each task adds one to `n`.
fn route(_: &str, _: &str, _: &str, _: Option<&str>) -> (u16, &'static str, Vec<u8>) {
    (200, "text/html", b"<!DOCTYPE html><title>t</title><script>window.n = 0; const c = new MessageChannel(); c.port1.onmessage = () => { n++; c.port2.postMessage(0) }; c.port2.postMessage(0)</script>".to_vec())
}

#[test]
fn a_live_page_runs_more_tasks_than_one_page_load_may() {
    let origin = common::serve_route(route);
    let (n, stopped) = std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(move || {
            let mut p = Page::open(&format!("{origin}/"), (800, 600)).unwrap();
            let _ = p.wait_load(Duration::from_secs(5));
            let end = Instant::now() + Duration::from_secs(60);
            let mut n = 0.0;
            while Instant::now() < end && n < 150_000.0 {
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
    assert!(n >= 150_000.0, "the page ran only {n} tasks (stopped: {stopped:?})");
    assert_eq!(stopped, None);
}
