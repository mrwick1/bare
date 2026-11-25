//! Load test for `indexedDB`: SCALE*1000 records in transactions of 100 puts, a get of each record, a walk of an index cursor
//! and a count. One scale per process.
//!
//!   idb_load SCALE      SCALE: 1, 10, 100
//!
//! The script checks the walk and the count against the record count. Prints time, CPU, peak RSS and allocations.
use bare::browser::*;
use bare::eng_new::Peek;
use bare::js_limits::HeapLimits;
use bare::js_limits::*;
use bare_html::tree;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::time::Instant;

struct Counting;
static ALLOCS: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Relaxed);
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Relaxed);
        unsafe { System.realloc(p, l, new) }
    }
}

#[global_allocator]
static A: Counting = Counting;

fn die(msg: String) -> ! {
    eprintln!("FAIL: {msg}");
    std::process::exit(1)
}

fn run(b: &mut Browser, src: &str) -> String {
    match b.run_script(src) {
        Ok(v) => b.it.display(v),
        Err(e) => die(format!("script error {e:?} in {src}")),
    }
}

/// Run the requests and the transaction ends (a task each) until `done` is true.
fn settle(b: &mut Browser) {
    for _ in 0..10_000 {
        b.drain().unwrap_or_else(|e| die(format!("drain: {e:?}")));
        if run(b, "done") == "true" {
            return;
        }
        b.advance_time(1.0, 1_000).unwrap_or_else(|e| die(format!("time: {e:?}")));
    }
    die("the requests did not finish".into());
}

const SETUP: &str = "var db, done = false, got = 0;
  var r = indexedDB.open('load', 1);
  r.onupgradeneeded = function () { r.result.createObjectStore('s', { keyPath: 'id' }).createIndex('v', 'v') };
  r.onsuccess = function () { db = r.result; done = true };
  function puts(start) {
    done = false;
    var tx = db.transaction('s', 'readwrite'), s = tx.objectStore('s');
    for (var i = start; i < start + 100; i++) s.put({ id: i, v: 'v' + (i % 997), n: i, list: [i, 'x'] });
    tx.oncomplete = function () { done = true };
  }
  function gets(start) {
    done = false;
    var tx = db.transaction('s'), s = tx.objectStore('s');
    for (var i = start; i < start + 100; i++) s.get(i).onsuccess = function (e) { got += e.target.result.n };
    tx.oncomplete = function () { done = true };
  }
  function walk() {
    done = false;
    var tx = db.transaction('s'), seen = 0, sum = 0;
    tx.objectStore('s').index('v').openCursor().onsuccess = function (e) {
      var c = e.target.result;
      if (!c) return;
      seen++; sum += c.value.n;
      c.continue();
    };
    tx.objectStore('s').count().onsuccess = function (e) { walk.count = e.target.result };
    tx.oncomplete = function () { walk.seen = seen; walk.sum = sum; done = true };
  }";

fn exec(scale: usize) {
    let eval = EvalLimits { max_steps: u64::MAX / 2, ..Default::default() };
    let mut b = Browser::new(tree::parse("<body></body>"), BrowserLimits::default(), eval, HeapLimits::default()).unwrap_or_else(|e| die(format!("browser: {e:?}")));
    b.use_virtual_clock_at(1000.0).unwrap_or_else(|e| die(format!("clock: {e:?}")));
    b.set_document_url("https://h.test:8443/start").unwrap_or_else(|e| die(e));
    let (a0, t) = (ALLOCS.load(Relaxed), Instant::now());
    run(&mut b, SETUP);
    settle(&mut b);
    let n = 1000 * scale;
    for start in (0..n).step_by(100) {
        run(&mut b, &format!("puts({start})"));
        settle(&mut b);
    }
    for start in (0..n).step_by(100) {
        run(&mut b, &format!("gets({start})"));
        settle(&mut b);
    }
    run(&mut b, "walk()");
    settle(&mut b);
    let sum = n * (n - 1) / 2;
    let got = run(&mut b, "[got, walk.seen, walk.sum, walk.count].join()");
    if got != format!("{sum},{n},{sum},{n}") {
        die(format!("checksum: {got}"));
    }
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let hwm = s.lines().find_map(|l| l.strip_prefix("VmHWM:")).unwrap_or("").trim().to_string();
    println!("idb x{scale:<3} checksum=ok run={:.1}ms peak_rss={hwm} allocs={}", t.elapsed().as_secs_f64() * 1e3, ALLOCS.load(Relaxed) - a0);
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() != 2 {
        die("usage: idb_load 1|10|100".into());
    }
    let scale: usize = a[1].parse().unwrap_or_else(|_| die("scale".into()));
    std::thread::Builder::new().stack_size(256 << 20).spawn(move || exec(scale)).unwrap().join().unwrap_or_else(|_| die("panic".into()));
}
