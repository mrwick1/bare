//! Load test for `MutationObserver` and `requestAnimationFrame`. One case at one scale per process.
//!
//!   observer_load CASE SCALE      CASE: mo | raf      SCALE: 1, 10, 100
//!
//! mo: one observer watches a list (childList, attributes, subtree). SCALE*1000 elements are made and appended, and
//! old ones are removed in blocks. Each gets a `class` after it is attached. The queue is delivered after each block.
//! The added, removed and attribute records are checked against the same count in Rust.
//! raf: a loop of SCALE*500 frames. Each frame sets a style on 20 elements and queues the next frame. The virtual
//! clock moves frame by frame. The count of callbacks and the sum of the frame numbers are checked.
//! Prints time, CPU, peak RSS and allocations. `tools/js/observer_load.sh` runs the matrix under caps.

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

const CHUNK: usize = 100;

fn exec(name: &str, scale: usize) {
    let eval = EvalLimits { max_steps: u64::MAX / 2, max_scripts: 1 << 14, max_envs: 1 << 22, max_functions: 1 << 22, ..Default::default() };
    let doc = tree::parse("<!doctype html><body><ul id=l></ul><div id=s></div></body>");
    let mut b = Browser::new(doc, BrowserLimits::default(), eval, HeapLimits::default()).unwrap_or_else(|e| die(format!("browser: {e:?}")));
    b.use_virtual_clock_at(1000.0).unwrap_or_else(|e| die(format!("clock {e:?}")));
    let (a0, b0, cpu0) = (ALLOCS.load(Relaxed), BYTES.load(Relaxed), cpu_ms());
    PEAK.store(LIVE.load(Relaxed), Relaxed);
    let t = Instant::now();
    match name {
        "mo" => {
            let n = 1000 * scale;
            run(&mut b, "var ul = document.getElementById('l'), added = 0, removed = 0, attrs = 0;
                new MutationObserver(function (rs) { for (var r of rs) { if (r.type == 'attributes') attrs++; else { added += r.addedNodes.length; removed += r.removedNodes.length; } } })
                  .observe(ul, { childList: true, attributes: true, subtree: true });
                function chunk(start, count) {
                  for (var i = start; i < start + count; i++) {
                    var li = document.createElement('li');
                    li.setAttribute('data-i', '' + i);
                    ul.appendChild(li);
                    li.className = 'k' + (i % 7);
                    if (ul.childElementCount > 1000) for (var k = 0; k < 500; k++) ul.removeChild(ul.firstChild);
                  }
                }");
            let (mut live, mut removed) = (0usize, 0usize);
            for start in (0..n).step_by(CHUNK) {
                run(&mut b, &format!("chunk({start}, {CHUNK})"));
                for _ in 0..CHUNK {
                    live += 1;
                    if live > 1000 {
                        live -= 500;
                        removed += 500;
                    }
                }
                b.advance_time(0.0, 1 << 20).unwrap_or_else(|e| die(format!("advance {e:?}")));
                b.collect(&[]).unwrap_or_else(|e| die(format!("collect: {e:?}")));
            }
            expect("added", run(&mut b, "added"), n.to_string());
            expect("removed", run(&mut b, "removed"), removed.to_string());
            expect("attrs", run(&mut b, "attrs"), n.to_string());
        }
        "raf" => {
            let n = 500 * scale;
            run(&mut b, "var s = document.getElementById('s'), els = [], calls = 0, sum = 0;
                for (var i = 0; i < 20; i++) els.push(s.appendChild(document.createElement('i')));
                function f() { calls++; sum += calls; for (var e of els) e.style.width = (calls % 50) + 'px'; if (calls < %N%) requestAnimationFrame(f) }
                requestAnimationFrame(f);".replace("%N%", &n.to_string()).as_str());
            let mut done = 0;
            while done < n {
                let step = CHUNK.min(n - done);
                b.advance_time(step as f64 * 1000.0 / 60.0 + 1.0, 1 << 20).unwrap_or_else(|e| die(format!("advance {e:?}")));
                done += step;
                b.collect(&[]).unwrap_or_else(|e| die(format!("collect: {e:?}")));
            }
            expect("calls", run(&mut b, "calls"), n.to_string());
            expect("sum", run(&mut b, "sum"), (n * (n + 1) / 2).to_string());
        }
        _ => die(format!("unknown case {name}")),
    }
    let run_ms = t.elapsed().as_secs_f64() * 1e3;
    let cpu = cpu_ms() - cpu0;
    let (allocs, abytes, peak_live) = (ALLOCS.load(Relaxed) - a0, BYTES.load(Relaxed) - b0, PEAK.load(Relaxed));
    let st = b.stats();
    println!(
        "{name:4} x{scale:<3} checksum=ok run={run_ms:9.1}ms cpu={cpu}ms peak_rss={:.1}MiB peak_live={:.1}MiB live_after={:.1}MiB allocs={allocs} alloc={:.1}MiB | wrappers={} nodes={} text={:.2}MiB",
        status_kib("VmHWM:") as f64 / 1024.0,
        peak_live as f64 / 1048576.0,
        LIVE.load(Relaxed) as f64 / 1048576.0,
        abytes as f64 / 1048576.0,
        st.wrappers,
        st.nodes,
        st.text_bytes as f64 / 1048576.0,
    );
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() != 3 {
        die("usage: observer_load mo|raf 1|10|100".into());
    }
    let scale: usize = a[2].parse().unwrap_or_else(|_| die("scale".into()));
    let name = a[1].clone();
    let stack_mib: usize = std::env::var("BARE_LOAD_STACK_MIB").ok().and_then(|v| v.parse().ok()).unwrap_or(256);
    std::thread::Builder::new().stack_size(stack_mib << 20).spawn(move || exec(&name, scale)).unwrap().join().unwrap_or_else(|_| die("panic".into()));
}
