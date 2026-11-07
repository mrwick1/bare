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

/// The peak of the live heap above the heap before the run, for a journey of `opens` open steps (`window`: in a window).
fn peak_of(base: &str, opens: usize, window: bool) -> usize {
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
        window,
        progress: false,
    };
    let before = LIVE.load(Relaxed);
    PEAK.store(before, Relaxed);
    let rep = bare_drive::run::run(&steps, &opt, &mut bare_drive::run::Redactor::default());
    assert!(rep.ok, "the journey failed: {:?}", rep.failure);
    PEAK.load(Relaxed) - before
}

#[test]
fn a_second_open_does_not_hold_two_pages_at_once() {
    let base = serve();
    // The page thread has a large stack in `bare-drive`; the test thread runs the page the same way.
    let run = move || {
        let one = peak_of(&base, 1, false);
        let two = peak_of(&base, 2, false);
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

/// The same in a window (`bare-drive --window`): an `open` replaces the page of the window thread, and the old document ends before the
/// new one loads. A window needs a Wayland compositor: the test runs itself again inside `cage` with the headless backend (it never
/// touches the desktop of a person), and the inner run writes `ok` to the file that the outer run names.
#[test]
fn a_second_window_open_does_not_hold_two_pages_at_once() {
    let name = "a_second_window_open_does_not_hold_two_pages_at_once";
    if let Some(done) = std::env::var_os("BARE_CAGE_DONE") {
        let base = serve();
        // Open once to make the tables that stay for the whole process (the window, the fonts), then measure.
        peak_of(&base, 1, true);
        let one = peak_of(&base, 1, true);
        let two = peak_of(&base, 2, true);
        assert!(two < one + one / 4, "peak with one open {one} B, with two {two} B");
        std::fs::write(done, "ok").unwrap();
        return;
    }
    let done = std::env::temp_dir().join(format!("bare-cage-mem-{}", std::process::id()));
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
