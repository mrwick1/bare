//! Load test for `HTMLElement.dataset` (`DOMStringMap`). One case at one size per process.
//!
//!   dataset_load CASE OPS ATTRS      CASE: read | write | keys      OPS: 1000, 10000, 100000      ATTRS: 1, 10, 50
//!
//! 20 elements, each with ATTRS attributes `data-k-x0` .. `data-k-x{ATTRS-1}` (the properties `kX0` ..). Each script runs
//! 1000 operations on element `k % 20`.
//! read: `sum += ds['kX' + i].length` with `i = (k * 7) % ATTRS`. The checksum is computed in Rust from the rule alone.
//! write: `ds['kX' + i] = 'w' + (k % 7)`. Rust keeps a model of the 20 attribute lists; at the end the native `Document` must
//! equal the model (the writes went to the attributes, in the first order).
//! keys: `Object.keys(ds)` of element `k % 20`: `sum += keys.length * 10 + keys[0].length`.
//!
//! A collection runs every 10000 operations. Prints time, CPU, peak RSS and allocation counts. `tools/js/dataset_load.sh` runs the matrix under caps.

use bare::browser::*;
use bare_html::tree;
use bare_js::eval::*;
use bare_js::heap::HeapLimits;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::time::Instant;

struct Counting;
static ALLOCS: AtomicUsize = AtomicUsize::new(0);
static BYTES: AtomicUsize = AtomicUsize::new(0);
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(l) };
        if !p.is_null() {
            ALLOCS.fetch_add(1, Relaxed);
            BYTES.fetch_add(l.size(), Relaxed);
            let live = LIVE.fetch_add(l.size(), Relaxed) + l.size();
            PEAK.fetch_max(live, Relaxed);
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        LIVE.fetch_sub(l.size(), Relaxed);
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        let q = unsafe { System.realloc(p, l, new) };
        if !q.is_null() {
            ALLOCS.fetch_add(1, Relaxed);
            if new > l.size() {
                BYTES.fetch_add(new - l.size(), Relaxed);
                let live = LIVE.fetch_add(new - l.size(), Relaxed) + new - l.size();
                PEAK.fetch_max(live, Relaxed);
            } else {
                LIVE.fetch_sub(l.size() - new, Relaxed);
            }
        }
        q
    }
}

#[global_allocator]
static A: Counting = Counting;

fn die(msg: String) -> ! {
    eprintln!("FAIL: {msg}");
    std::process::exit(1)
}

fn status_kib(key: &str) -> u64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    s.lines().find_map(|l| l.strip_prefix(key)).and_then(|v| v.trim().trim_end_matches("kB").trim().parse().ok()).unwrap_or(0)
}

/// CPU time of this process in milliseconds (user + system), from /proc/self/stat (100 ticks per second).
fn cpu_ms() -> u64 {
    let s = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    let rest = s.rsplit_once(')').map_or("", |x| x.1);
    let f: Vec<&str> = rest.split_whitespace().collect();
    let t = |i: usize| f.get(i).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
    (t(11) + t(12)) * 10
}

fn run(b: &mut Browser, src: &str) -> String {
    match b.run_script(src) {
        Ok(v) => b.it.display(v),
        Err(e) => die(format!("script error {e:?} in {src}")),
    }
}

fn expect(what: &str, got: String, want: String) {
    if got != want {
        die(format!("{what}: {got} != {want}"));
    }
}

const PAGE: &str = "<!doctype html><html><head></head><body></body></html>";

fn exec(name: &str, ops: usize, attrs: usize) {
    let eval = EvalLimits { max_steps: u64::MAX / 2, max_scripts: 1 << 14, max_envs: 1 << 22, max_functions: 1 << 22, ..Default::default() };
    let mut b = Browser::new(tree::parse(PAGE), BrowserLimits::default(), eval, HeapLimits::default()).unwrap_or_else(|e| die(format!("browser: {e:?}")));
    run(&mut b, &format!(
        "var els = []; for (var i = 0; i < 20; i++) {{ var e = document.createElement('div'); e.id = 'e' + i; for (var j = 0; j < {attrs}; j++) e.setAttribute('data-k-x' + j, 'v' + j); document.body.appendChild(e); els.push(e); }} var e = null; var sum = 0;
         function read(s, c) {{ for (var k = s; k < s + c; k++) sum += els[k % 20].dataset['kX' + ((k * 7) % {attrs})].length; }}
         function write(s, c) {{ for (var k = s; k < s + c; k++) els[k % 20].dataset['kX' + ((k * 7) % {attrs})] = 'w' + (k % 7); }}
         function keys(s, c) {{ for (var k = s; k < s + c; k++) {{ var ks = Object.keys(els[k % 20].dataset); sum += ks.length * 10 + ks[0].length; }} }}"
    ));
    b.collect(&[]).unwrap_or_else(|e| die(format!("collect: {e:?}")));
    let (a0, b0, cpu0) = (ALLOCS.load(Relaxed), BYTES.load(Relaxed), cpu_ms());
    PEAK.store(LIVE.load(Relaxed), Relaxed);
    let t = Instant::now();
    let mut model = vec![(0..attrs).map(|j| format!("v{j}")).collect::<Vec<_>>(); 20];
    let mut sum = 0u64;
    for k in 0..ops {
        let i = (k * 7) % attrs;
        match name {
            "read" => sum += model[k % 20][i].len() as u64,
            "write" => model[k % 20][i] = format!("w{}", k % 7),
            "keys" => sum += (attrs * 10 + 3) as u64,
            _ => die(format!("unknown case {name}")),
        }
    }
    let mut gc_ms = 0.0;
    for s in (0..ops).step_by(1000) {
        run(&mut b, &format!("{name}({s}, {})", 1000.min(ops - s)));
        if s % 10_000 == 9_000 {
            run(&mut b, "0"); // the interpreter keeps the last script value until the next script
            let t = Instant::now();
            b.collect(&[]).unwrap_or_else(|e| die(format!("collect: {e:?}")));
            gc_ms += t.elapsed().as_secs_f64() * 1e3;
        }
    }
    expect("sum", run(&mut b, "sum"), sum.to_string());
    let doc = b.document();
    for i in 0..20usize {
        let el = b.query(&format!("#e{i}")).unwrap_or_else(|e| die(format!("query {e:?}"))).unwrap_or_else(|| die(format!("missing e{i}")));
        let d = doc.borrow();
        let bare_html::dom::NodeData::Element { attrs: list, .. } = *d.data(el) else { die("not element".into()) };
        let native: Vec<(String, String)> = d.attrs(list).iter().map(|a| (d.atom_str(a.name).to_string(), d.text(a.value).to_string())).collect();
        let mut want = vec![("id".to_string(), format!("e{i}"))];
        want.extend(model[i].iter().enumerate().map(|(j, v)| (format!("data-k-x{j}"), v.clone())));
        if native != want {
            die(format!("native attributes of e{i}: {native:?} != {want:?}"));
        }
    }
    let el = t.elapsed();
    println!(
        "case={name} ops={ops} attrs={attrs} wall={:.1}ms cpu={}ms gc={:.1}ms peak_rss={}MiB peak_heap={}KiB allocs={} alloc_bytes={}KiB maps={} checksum=ok",
        el.as_secs_f64() * 1e3,
        cpu_ms() - cpu0,
        gc_ms,
        status_kib("VmHWM:") / 1024,
        PEAK.load(Relaxed) / 1024,
        ALLOCS.load(Relaxed) - a0,
        (BYTES.load(Relaxed) - b0) / 1024,
        b.dataset_stats().objects,
    );
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() != 4 {
        die("usage: dataset_load CASE OPS ATTRS".into());
    }
    let ops: usize = a[2].parse().unwrap_or_else(|_| die("bad ops".into()));
    let attrs: usize = a[3].parse().unwrap_or_else(|_| die("bad attrs".into()));
    let case = a[1].clone();
    // Debug builds need a large stack for the interpreter, release builds 8 MiB.
    let stack = if cfg!(debug_assertions) { 64 << 20 } else { 8 << 20 };
    let h = std::thread::Builder::new().stack_size(stack).spawn(move || exec(&case, ops, attrs)).unwrap();
    if h.join().is_err() {
        std::process::exit(1);
    }
}
