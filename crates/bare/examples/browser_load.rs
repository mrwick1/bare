//! Load test for the DOM/JS bridge. One case at one scale per process.
//!
//!   browser_load CASE SCALE      CASE: mutation | event | lookup      SCALE: 1, 10, 100
//!
//! mutation: SCALE*1000 elements are made, filled, appended to a list, and old ones removed in blocks.
//! event: SCALE*1000 bubbling events through a 4 level tree with 3 listeners on the path. Half start in JS,
//! half in the Rust test driver. lookup: SCALE*500 rounds of `querySelectorAll`, `getElementById` and `closest`
//! on a 200 item page. Each case runs in small scripts, with a VM collect between them, and checks its
//! result against the same computation in Rust. Prints time, CPU, peak RSS, allocations and what the realm holds.
//! `tools/js/browser_load.sh` runs the matrix under caps.

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

fn page(name: &str) -> String {
    match name {
        "lookup" => {
            let items: String = (0..200).map(|i| format!("<li id=i{i} class='k{}'><b>x</b></li>", i % 10)).collect();
            format!("<body><ul id=l>{items}</ul></body>")
        }
        "event" => "<body><div id=r><div id=m><div id=n><span id=t>x</span></div></div></div></body>".into(),
        _ => "<body><ul id=l></ul></body>".into(),
    }
}

fn exec(name: &str, scale: usize) {
    let eval = EvalLimits { max_steps: u64::MAX / 2, max_scripts: 1 << 14, max_envs: 1 << 22, max_functions: 1 << 22, ..Default::default() };
    let doc = tree::parse(&page(name));
    let mut b = Browser::new(doc, BrowserLimits::default(), eval, HeapLimits::default()).unwrap_or_else(|e| die(format!("browser: {e:?}")));
    let (a0, b0, cpu0) = (ALLOCS.load(Relaxed), BYTES.load(Relaxed), cpu_ms());
    PEAK.store(LIVE.load(Relaxed), Relaxed);
    let t = Instant::now();
    let mut gc_ms = 0.0;
    let mut freed_objs = 0;
    let mut collect = |b: &mut Browser| {
        let t = Instant::now();
        freed_objs += b.collect(&[]).unwrap_or_else(|e| die(format!("collect: {e:?}"))).objects;
        gc_ms += t.elapsed().as_secs_f64() * 1e3;
    };
    match name {
        "mutation" => {
            let n = 1000 * scale;
            run(&mut b, "var ul = document.getElementById('l');
                function chunk(start, count) {
                  for (var i = start; i < start + count; i++) {
                    var li = document.createElement('li');
                    li.textContent = 'n' + i;
                    li.setAttribute('data-i', '' + i);
                    ul.appendChild(li);
                    if (ul.childElementCount > 1000) for (var k = 0; k < 500; k++) ul.removeChild(ul.firstChild);
                  }
                }");
            let mut live: Vec<usize> = Vec::new();
            for start in (0..n).step_by(CHUNK) {
                run(&mut b, &format!("chunk({start}, {CHUNK})"));
                for i in start..start + CHUNK {
                    live.push(i);
                    if live.len() > 1000 {
                        live.drain(..500);
                    }
                }
                collect(&mut b);
            }
            let sum: usize = live.iter().sum();
            expect("mutation", run(&mut b, "var s = 0; for (var e = ul.firstElementChild; e; e = e.nextElementSibling) s += Number(e.getAttribute('data-i')); ul.childElementCount + ',' + s"), format!("{},{sum}", live.len()));
            expect("mutation text", run(&mut b, "ul.lastChild.textContent"), format!("n{}", n - 1));
        }
        "event" => {
            let n = 1000 * scale;
            run(&mut b, "var hits = 0; var t = document.getElementById('t');
                ['r', 'm', 'n'].forEach(function (id) { document.getElementById(id).addEventListener('x', function (e) { hits += e.eventPhase; }); });
                function chunk(count) { for (var i = 0; i < count; i++) t.dispatchEvent(new Event('x', { bubbles: true })); }");
            let target = b.query("#t").unwrap().unwrap();
            let mut done = 0;
            while done < n {
                run(&mut b, &format!("chunk({})", CHUNK / 2));
                for _ in 0..CHUNK / 2 {
                    let r = b.dispatch(target, "x", true, false).unwrap_or_else(|e| die(format!("dispatch {e:?}")));
                    if !r.not_canceled || !r.reports.is_empty() {
                        die("dispatch result".into());
                    }
                }
                done += CHUNK;
                collect(&mut b);
            }
            // each event: three bubble-phase listeners (phase 3 each, target is the span with no listener)
            expect("event", run(&mut b, "hits"), (done * 9).to_string());
        }
        "lookup" => {
            let n = 500 * scale;
            run(&mut b, "var sum = 0;
                function chunk(start, count) {
                  for (var i = start; i < start + count; i++) {
                    sum += document.querySelectorAll('li.k3').length;
                    var li = document.getElementById('i' + (i % 200));
                    sum += li.id.length + li.querySelector('b').closest('ul').id.length;
                  }
                }");
            let mut want = 0;
            for start in (0..n).step_by(CHUNK) {
                run(&mut b, &format!("chunk({start}, {CHUNK})"));
                for i in start..start + CHUNK {
                    want += 20 + format!("i{}", i % 200).len() + 1;
                }
                collect(&mut b);
            }
            expect("lookup", run(&mut b, "sum"), want.to_string());
        }
        _ => die(format!("unknown case {name}")),
    }
    let run_ms = t.elapsed().as_secs_f64() * 1e3;
    let cpu = cpu_ms() - cpu0;
    let (allocs, abytes, peak_live) = (ALLOCS.load(Relaxed) - a0, BYTES.load(Relaxed) - b0, PEAK.load(Relaxed));
    let st = b.stats();
    println!(
        "{name:8} x{scale:<3} checksum=ok run={run_ms:9.1}ms cpu={cpu}ms peak_rss={:.1}MiB peak_live={:.1}MiB live_after={:.1}MiB allocs={allocs} alloc={:.1}MiB | wrappers={} listeners={} events={} nodes={} text={:.2}MiB | gc={gc_ms:.1}ms freed_objs={freed_objs}",
        status_kib("VmHWM:") as f64 / 1024.0,
        peak_live as f64 / 1048576.0,
        LIVE.load(Relaxed) as f64 / 1048576.0,
        abytes as f64 / 1048576.0,
        st.wrappers,
        st.listeners,
        st.events,
        st.nodes,
        st.text_bytes as f64 / 1048576.0,
    );
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() != 3 {
        die("usage: browser_load mutation|event|lookup 1|10|100".into());
    }
    let scale: usize = a[2].parse().unwrap_or_else(|_| die("scale".into()));
    // the evaluator recurses: run on a big stack
    let name = a[1].clone();
    std::thread::Builder::new().stack_size(256 << 20).spawn(move || exec(&name, scale)).unwrap().join().unwrap_or_else(|_| die("panic".into()));
}
