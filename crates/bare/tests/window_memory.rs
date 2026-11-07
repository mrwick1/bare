//! A window shows the old frame until the new page has its first paint. It keeps only the pixels of it, not the old document or
//! the layout of it: the old document and the new one never share the heap. A counting allocator gives the peak of the live heap.
//! The first page has a large frame and few objects, the second has many objects and a small frame. So the peak of the second
//! page shows what the first one left. The tests call the `Page` methods that the window thread runs: `reopen_in` for the `open` of
//! the driver, `navigate` for the address bar and the driver, and a click on a link for a navigation that the page asks for.

use bare::page::Page;
use bare_window::{Event, Mods};
use std::alloc::{GlobalAlloc, Layout, System};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::time::{Duration, Instant};

struct Counting;
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
/// The live heap when the server got the request for `/light`.
static AT_LIGHT: AtomicUsize = AtomicUsize::new(0);

fn grow(n: usize) {
    PEAK.fetch_max(LIVE.fetch_add(n, Relaxed) + n, Relaxed);
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        grow(l.size());
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        LIVE.fetch_sub(l.size(), Relaxed);
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        LIVE.fetch_sub(l.size(), Relaxed);
        grow(n);
        unsafe { System.realloc(p, l, n) }
    }
}

#[global_allocator]
static A: Counting = Counting;

const SIZE: (u32, u32) = (1280, 800);
/// The canvas of the page, in bytes.
const PIXELS: usize = (SIZE.0 * SIZE.1 * 4) as usize;

/// `/`: a link and a wide tree, so the frame (styles and layout) is large. It stays under the node limit of the layout.
fn wide() -> String {
    let rows: String = (0..5000).map(|i| format!("<div class=r><p>row {i}</p></div>")).collect();
    format!("<!doctype html><title>wide</title><a href=/heavy style='display:block;width:200px;height:40px'>go</a>{rows}")
}

/// `/heavy`: about 8 MiB of script objects and a small tree.
const HEAVY: &str = "<!doctype html><title>heavy</title><p id=done>heavy</p><script>window.keep = []; for (var i = 0; i < 60000; i++) keep.push({ n: i, s: 'item ' + i, a: [i, i + 1] });</script>";

/// `/light`: a small page.
const LIGHT: &str = "<!doctype html><title>light</title><a href=/heavy style='display:block;width:200px;height:40px'>go</a>";

/// Serve the three pages on a free loopback port. Gives the origin.
fn serve() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let wide = wide();
    std::thread::spawn(move || {
        for mut s in listener.incoming().flatten() {
            let mut buf = [0u8; 4096];
            let n = s.read(&mut buf).unwrap_or(0);
            let head = String::from_utf8_lossy(&buf[..n]);
            let body = if head.starts_with("GET /heavy") {
                HEAVY
            } else if head.starts_with("GET /light") {
                AT_LIGHT.store(LIVE.load(Relaxed), Relaxed);
                LIGHT
            } else {
                &wide
            };
            let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
        }
    });
    format!("http://127.0.0.1:{port}")
}

/// The counter is global: one test at a time. Run `f` on a thread with a big stack (the VM recurses), as the window thread has.
fn on_big_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn open(url: &str) -> Page {
    let mut p = Page::open(url, SIZE).unwrap();
    p.wait_load(Duration::from_secs(20)).unwrap();
    p.quiet = true;
    p
}

/// The peak of the heap (above the heap before any page) when `go` loads `/heavy` in the page that shows `first`, after a paint of both.
fn heavy_after(first: &'static str, go: fn(&mut Page, &str)) -> usize {
    let base = serve();
    on_big_stack(move || {
        let shown = |base: &str| {
            let mut p = open(&format!("{base}{first}"));
            p.paint();
            assert!(p.errors.is_empty(), "{first} was not laid out: {:?}", p.errors);
            p
        };
        // The first run makes the tables that stay for the whole process.
        drop(shown(&base));
        let before = LIVE.load(Relaxed);
        let mut p = shown(&base);
        PEAK.store(LIVE.load(Relaxed), Relaxed);
        go(&mut p, &base);
        p.paint();
        assert!(p.errors.is_empty(), "{:?}", p.errors);
        PEAK.load(Relaxed) - before
    })
}

/// What the wide page leaves must be what a small page leaves: the pixels of the canvas, which the new page draws over.
fn assert_leaves_nothing(go: fn(&mut Page, &str)) {
    let (light, wide) = (heavy_after("/light", go), heavy_after("/", go));
    assert!(wide < light + (1 << 20), "after a small page the heavy page peaks at {light} B, after the wide page at {wide} B");
}

#[test]
fn a_window_open_does_not_hold_two_pages_at_once() {
    assert_leaves_nothing(|p, base| {
        p.reopen_in(&format!("{base}/heavy"), &bare_net::http::Client::new()).unwrap();
        p.wait_load(Duration::from_secs(20)).unwrap();
    });
}

#[test]
fn a_navigation_does_not_keep_the_frame_of_the_old_document() {
    assert_leaves_nothing(|p, base| {
        p.navigate(Some(&format!("{base}/heavy"))).unwrap();
        p.wait_load(Duration::from_secs(20)).unwrap();
    });
}

#[test]
fn a_link_does_not_keep_the_frame_of_the_old_document() {
    assert_leaves_nothing(|p, _| {
        let m = Mods::default();
        p.handle(Event::PointerMove { x: 10.0, y: 10.0, buttons: 0, mods: m });
        p.handle(Event::PointerButton { x: 10.0, y: 10.0, button: 0, buttons: 1, pressed: true, mods: m });
        p.handle(Event::PointerButton { x: 10.0, y: 10.0, button: 0, buttons: 0, pressed: false, mods: m });
        // The next document downloads on a worker; the old one stays until it is in. Run the loop of the window.
        let end = Instant::now() + Duration::from_secs(20);
        while p.generation() < 2 && Instant::now() < end {
            p.tick();
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(p.generation(), 2, "the link did not load");
    });
}

#[test]
fn the_old_document_and_its_frame_are_gone_when_the_next_one_is_requested() {
    let base = serve();
    on_big_stack(move || {
        let before = LIVE.load(Relaxed);
        let mut p = open(&format!("{base}/"));
        p.paint();
        p.navigate(Some(&format!("{base}/light"))).unwrap();
        p.wait_load(Duration::from_secs(20)).unwrap();
        // What is left: the canvas of the window, the client and its cache, and the session. Not the tree, the styles or the layout.
        let left = AT_LIGHT.load(Relaxed) - before;
        assert!(left < PIXELS + (3 << 19), "{left} B are alive when the next document is requested; the canvas is {PIXELS} B");
    });
}
