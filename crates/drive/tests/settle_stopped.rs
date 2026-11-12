//! `snap::settle` on a page that stopped while a `fetch` is still open. `tick` then gives no next time, and the loop spun at full CPU
//! for the whole 2 s wait. It now sleeps 5 ms between ticks. Linux only: the CPU time comes from `/proc/thread-self/schedstat`.

mod common;

use bare::page::Page;
use std::time::{Duration, Instant};

fn route(_: &str, target: &str, _: &str, _: Option<&str>) -> (u16, &'static str, Vec<u8>) {
    match target {
        // A slow reply, so the `fetch` stays open while the page settles.
        "/slow" => {
            std::thread::sleep(Duration::from_secs(5));
            (200, "text/plain", b"late".to_vec())
        }
        // The script starts the `fetch`, then stops the page on a gate (the default action of a checkbox).
        _ => (200, "text/html", b"<!DOCTYPE html><title>t</title><input type=checkbox id=c><script>fetch('/slow'); document.getElementById('c').click()</script>".to_vec()),
    }
}

#[cfg(target_os = "linux")]
/// The CPU time of this thread, in nanoseconds.
fn thread_cpu_ns() -> u64 {
    std::fs::read_to_string("/proc/thread-self/schedstat").ok().and_then(|s| s.split(' ').next()?.parse().ok()).unwrap_or(0)
}

#[cfg(target_os = "linux")]
#[test]
fn settle_does_not_spin_on_a_stopped_page_with_an_open_fetch() {
    let origin = common::serve_route(route);
    let (wall, cpu) = std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(move || {
            let mut p = Page::open(&format!("{origin}/"), (800, 600)).unwrap();
            let _ = p.wait_load(Duration::from_secs(5));
            assert!(p.stopped().is_some(), "the page did not stop");
            let (start, cpu) = (Instant::now(), thread_cpu_ns());
            bare_drive::snap::settle(&mut p);
            (start.elapsed(), Duration::from_nanos(thread_cpu_ns() - cpu))
        })
        .unwrap()
        .join()
        .unwrap();
    // The wait limit stays 2 s.
    assert!(wall >= Duration::from_millis(1900) && wall < Duration::from_millis(2500), "settle took {wall:?}");
    assert!(cpu < wall / 4, "settle used {cpu:?} of CPU in {wall:?}");
}
