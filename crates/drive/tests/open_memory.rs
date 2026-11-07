//! An `open` step replaces the page: the old page is gone before the new one loads, so two pages never share the peak. A counting
//! allocator gives the peak of the live heap. The page keeps about 8 MiB of objects alive.

use std::alloc::{GlobalAlloc, Layout, System};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

struct Counting;
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(l) };
        if !p.is_null() {
            PEAK.fetch_max(LIVE.fetch_add(l.size(), Relaxed) + l.size(), Relaxed);
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) };
        LIVE.fetch_sub(l.size(), Relaxed);
    }
}

#[global_allocator]
static A: Counting = Counting;

const PAGE: &str = "<!doctype html><p id=done>big</p><script>window.keep = []; for (var i = 0; i < 60000; i++) keep.push({ n: i, s: 'item ' + i, a: [i, i + 1] });</script>";

/// Serve `PAGE` for each request on a free loopback port. Gives the origin.
fn serve() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for mut s in listener.incoming().flatten() {
            let mut buf = [0u8; 4096];
            let _ = s.read(&mut buf);
            let _ = write!(
                s,
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{PAGE}",
                PAGE.len()
            );
        }
    });
    format!("http://127.0.0.1:{port}")
}

/// The peak of the live heap above the heap before the run, for a journey of `opens` open steps.
fn peak_of(base: &str, opens: usize) -> usize {
    let text = format!(
        "{}expect css=\"#done\" visible\n",
        "open ${BASE}/\n".repeat(opens)
    );
    let vars = std::collections::HashMap::from([("BASE".to_string(), base.to_string())]);
    let steps = bare_drive::steps::parse(&text, &vars).unwrap();
    let opt = bare_drive::run::Options {
        size: (800, 600),
        timeout: std::time::Duration::from_secs(20),
        out: None,
        window: false,
        progress: false,
    };
    let before = LIVE.load(Relaxed);
    PEAK.store(before, Relaxed);
    let rep = bare_drive::run::run(&steps, &opt, &mut bare_drive::run::Redactor::default());
    assert!(rep.ok, "the journey failed");
    PEAK.load(Relaxed) - before
}

#[test]
fn a_second_open_does_not_hold_two_pages_at_once() {
    let base = serve();
    // The page thread has a large stack in `bare-drive`; the test thread runs the page the same way.
    let run = move || {
        let one = peak_of(&base, 1);
        let two = peak_of(&base, 2);
        assert!(
            two < one + one / 4,
            "peak with one open {one} B, with two {two} B"
        );
    };
    std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(run)
        .unwrap()
        .join()
        .unwrap();
}
