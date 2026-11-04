//! Load test for the navigation module. One case at one scale per process.
//!
//!   navigation_load CASE SCALE      CASE: url | iterate | history | storage | route      SCALE: 1, 10, 100
//!
//! iterate: SCALE*1000 rounds that build `URLSearchParams` from a Map, an iterator and a pair iterable and iterate them (for-of with a
//! live append, spread, `Array.from`, an early break). url: SCALE*1000 `URL` objects with live `searchParams` (append, set, sort, get, hash change). history: SCALE*1000
//! `pushState`/`replaceState` of cyclic state graphs, with traversal run by the driver (the history is capped at 50
//! entries). storage: SCALE*1000 writes, reads and removals over 3 origins, with UTF-16 values. route: SCALE*500 rounds of
//! fragment, search and reload navigation, with the driver draining requests and tasks. Each case runs in small scripts,
//! with a VM collect between them, and checks its result against the same computation in Rust. Prints time, CPU, peak
//! RSS, allocations and what the realm holds. `tools/js/navigation_load.sh` runs the matrix under caps.

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
const PAGE: &str = "<body><div id=a></div></body>";

fn digits(n: usize) -> usize {
    n.to_string().len()
}

fn exec(name: &str, scale: usize) {
    let eval = EvalLimits { max_steps: u64::MAX / 2, max_scripts: 1 << 14, max_envs: 1 << 22, max_functions: 1 << 22, ..Default::default() };
    let mut b = Browser::new(tree::parse(PAGE), BrowserLimits::default(), eval, HeapLimits::default()).unwrap_or_else(|e| die(format!("browser: {e:?}")));
    let url0 = if name == "route" { "https://staging.example.test:8443/a/b?x=1" } else { "https://h.test:8443/start" };
    b.set_document_url(url0).unwrap_or_else(|e| die(e));
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
        "url" => {
            let n = 1000 * scale;
            run(&mut b, "function chunk(start, count) { var s = 0;
                  for (var i = start; i < start + count; i++) {
                    var u = new URL('/r/' + i + '?a=' + i + '&b=x y#h' + i, 'https://h.test:8443/base/');
                    u.searchParams.append('c', '' + (i % 7)); u.searchParams.set('a', '' + (i * 2)); u.searchParams.sort();
                    s += u.href.length + Number(u.searchParams.get('a')) + u.searchParams.getAll('c').length;
                    var e = u.searchParams.entries(); e.next();
                    u.hash = ''; s += u.hash.length;
                  }
                  return s; }");
            let mut want: usize = 0;
            let mut got: usize = 0;
            for start in (0..n).step_by(CHUNK) {
                got += run(&mut b, &format!("chunk({start}, {CHUNK})")).parse::<usize>().unwrap_or_else(|_| die("chunk result".into()));
                for i in start..start + CHUNK {
                    let href = format!("https://h.test:8443/r/{i}?a={}&b=x+y&c={}#h{i}", i * 2, i % 7);
                    want += href.len() + i * 2 + 1;
                }
                collect(&mut b);
                let st = b.navigation_stats();
                if st.urls + st.params + st.iterators != 0 {
                    die(format!("wrappers survived a collect: {st:?}"));
                }
            }
            expect("url", got.to_string(), want.to_string());
        }
        "iterate" => {
            // Each round: a Map, an entries iterator and a pair array as init, for-of with a live append, spread, Array.from, an early break.
            let n = 1000 * scale;
            run(&mut b, "function chunk(start, count) { var s = 0;
                  for (var i = start; i < start + count; i++) {
                    var p = new URLSearchParams(new Map([['a' + i, '' + i], ['b', 'x'], ['c' + (i % 5), 'y']]));
                    var q = new URLSearchParams(p.entries()), r = new URLSearchParams([['k', '1'], ['k', '2']].values()), k = 0, m = 0;
                    for (var [name, v] of p) { m += name.length + v.length; if (k++ === 0) p.append('z', '' + (i % 3)); }
                    for (var e of r) { m += e[1].length; break; }
                    s += m + [...q.keys()].length + Array.from(q).length + [...p].length;
                  }
                  return s; }");
            let (mut want, mut got) = (0usize, 0usize);
            for start in (0..n).step_by(CHUNK) {
                got += run(&mut b, &format!("chunk({start}, {CHUNK})")).parse::<usize>().unwrap_or_else(|_| die("chunk result".into()));
                for i in start..start + CHUNK {
                    // pairs of p: a<i>=<i>, b=x, c<i%5>=y, then z=<i%3>
                    want += 2 * digits(i) + 1 + 2 + 3 + 2 + 1 + 3 + 3 + 4;
                }
                collect(&mut b);
                let st = b.navigation_stats();
                if st.params + st.iterators != 0 {
                    die(format!("wrappers survived a collect: {st:?}"));
                }
            }
            expect("iterate", got.to_string(), want.to_string());
        }
        "history" => {
            let n = 1000 * scale;
            run(&mut b, "var pops = 0; window.addEventListener('popstate', function (e) { pops += e.state ? e.state.i + 1 : 0; });
                function chunk(start, count) { var s = 0;
                  for (var i = start; i < start + count; i++) {
                    var st = {i: i, self: null, tags: ['a', 'b', i], nested: {k: i % 5}}; st.self = st;
                    var url = '/p/' + (i % 10) + '?q=' + i;
                    if (i % 3 == 0) history.pushState(st, '', url); else history.replaceState(st, '', url);
                    var h = history.state;
                    s += h.i + h.tags[2] + h.nested.k + (h.self === h ? 1 : 0) + history.length;
                  }
                  return s; }");
            // model: the state id of each entry (None for the first)
            let mut entries: Vec<Option<usize>> = vec![None];
            let mut cur = 0usize;
            let (mut want, mut got, mut want_pops) = (0usize, 0usize, 0usize);
            for start in (0..n).step_by(CHUNK) {
                got += run(&mut b, &format!("chunk({start}, {CHUNK})")).parse::<usize>().unwrap_or_else(|_| die("chunk result".into()));
                for i in start..start + CHUNK {
                    if i % 3 == 0 {
                        entries.truncate(cur + 1);
                        entries.push(Some(i));
                        if entries.len() > 50 {
                            entries.remove(0);
                        }
                        cur = entries.len() - 1;
                    } else {
                        entries[cur] = Some(i);
                    }
                    want += i + i + i % 5 + 1 + entries.len();
                }
                run(&mut b, "history.go(-2); history.go(1); history.go(-100); history.go(1)");
                b.run_navigation_tasks().unwrap_or_else(|e| die(format!("tasks: {e:?}")));
                for d in [-2i64, 1, -100, 1] {
                    let to = cur as i64 + d;
                    if to >= 0 && (to as usize) < entries.len() {
                        cur = to as usize;
                        want_pops += entries[cur].map_or(0, |i| i + 1);
                    }
                }
                collect(&mut b);
            }
            expect("history", got.to_string(), want.to_string());
            expect("history pops", run(&mut b, "pops"), want_pops.to_string());
            let st = b.navigation_stats();
            expect("history length", st.history_len.to_string(), entries.len().to_string());
            expect("history index", st.history_index.to_string(), cur.to_string());
            if st.history_len > 50 {
                die(format!("history grew: {st:?}"));
            }
        }
        "storage" => {
            let n = 1000 * scale;
            let origins = ["https://a.test:8443/", "https://b.test/", "http://c.test:81/"];
            run(&mut b, "function val(i) { return 'v' + i + '\\ud83d\\ude00pppppppp'; }
                function chunk(start, count) { var s = 0;
                  for (var i = start; i < start + count; i++) {
                    var k = 'k' + (i % 64), st = i % 2 ? sessionStorage : localStorage;
                    if (i % 5 == 0) st.removeItem(k); else st.setItem(k, val(i));
                    var v = st.getItem(k); s += (v === null ? 0 : v.length) + st.length + (st.key(0) === null ? 0 : 1);
                  }
                  return s; }");
            // model: per origin and area, (key -> utf16 length) in insertion order
            let mut model: Vec<[Vec<(usize, usize)>; 2]> = vec![Default::default(), Default::default(), Default::default()];
            let (mut want, mut got) = (0usize, 0usize);
            for (c, start) in (0..n).step_by(CHUNK).enumerate() {
                let o = c % 3;
                b.set_document_url(origins[o]).unwrap_or_else(|e| die(e));
                got += run(&mut b, &format!("chunk({start}, {CHUNK})")).parse::<usize>().unwrap_or_else(|_| die("chunk result".into()));
                for i in start..start + CHUNK {
                    let area = &mut model[o][i % 2];
                    let k = i % 64;
                    if i % 5 == 0 {
                        area.retain(|e| e.0 != k);
                    } else {
                        let len = 1 + digits(i) + 2 + 8;
                        match area.iter_mut().find(|e| e.0 == k) {
                            Some(e) => e.1 = len,
                            None => area.push((k, len)),
                        }
                    }
                    let v = area.iter().find(|e| e.0 == k).map_or(0, |e| e.1);
                    want += v + area.len() + usize::from(!area.is_empty());
                }
                collect(&mut b);
            }
            expect("storage", got.to_string(), want.to_string());
            let store = b.storage();
            for (o, o_url) in ["https://a.test:8443", "https://b.test", "http://c.test:81"].iter().enumerate() {
                for (s, area) in model[o].iter().enumerate() {
                    expect("storage items", store.item_count(s == 1, o_url).to_string(), area.len().to_string());
                }
            }
            let units: usize = model.iter().flatten().flatten().map(|e| e.1 + format!("k{}", e.0).len()).sum();
            expect("storage units", (store.total_units(false) + store.total_units(true)).to_string(), units.to_string());
        }
        "route" => {
            let n = 500 * scale;
            run(&mut b, "var pops = 0, hashes = 0; window.addEventListener('popstate', function () { pops++; }); window.addEventListener('hashchange', function () { hashes++; });
                function chunk(start, count) {
                  for (var i = start; i < start + count; i++) {
                    location.hash = 'h' + i;
                    location.assign('/x?i=' + i);
                    if (i % 4 == 0) location.reload();
                    location.search = '?s=' + i;
                    if (i % 7 == 0) { localStorage.setItem('token', 't' + i); sessionStorage.removeItem('token'); }
                  }
                }");
            let (mut want_req_bytes, mut got_req_bytes, mut want_reqs, mut got_reqs) = (0usize, 0usize, 0usize, 0usize);
            for start in (0..n).step_by(10) {
                run(&mut b, &format!("chunk({start}, 10)"));
                b.run_navigation_tasks().unwrap_or_else(|e| die(format!("tasks: {e:?}")));
                while let Some(r) = b.take_navigation_request() {
                    got_reqs += 1;
                    got_req_bytes += r.url.len();
                }
                for i in start..start + 10 {
                    let (base, hash) = ("https://staging.example.test:8443", format!("#h{i}"));
                    want_req_bytes += format!("{base}/x?i={i}").len();
                    want_reqs += 1;
                    if i % 4 == 0 {
                        want_req_bytes += format!("{base}/a/b?x=1{hash}").len();
                        want_reqs += 1;
                    }
                    want_req_bytes += format!("{base}/a/b?s={i}{hash}").len();
                    want_reqs += 1;
                }
                if start % CHUNK == 0 {
                    collect(&mut b);
                }
            }
            expect("route requests", got_reqs.to_string(), want_reqs.to_string());
            expect("route request bytes", got_req_bytes.to_string(), want_req_bytes.to_string());
            expect("route events", run(&mut b, "pops + ',' + hashes"), format!("{n},{n}"));
            expect("route url", b.document_url().unwrap_or_default(), format!("https://staging.example.test:8443/a/b?x=1#h{}", n - 1));
            let st = b.navigation_stats();
            expect("route history", st.history_len.to_string(), "50".to_string());
            expect("route queues", format!("{},{}", st.pending_tasks, st.pending_requests), "0,0".to_string());
            expect("route token", b.storage().get_item(false, "https://staging.example.test:8443", "token").unwrap_or_default(), format!("t{}", (n - 1) / 7 * 7));
        }
        _ => die(format!("unknown case {name}")),
    }
    let run_ms = t.elapsed().as_secs_f64() * 1e3;
    let cpu = cpu_ms() - cpu0;
    let (allocs, abytes, peak_live) = (ALLOCS.load(Relaxed) - a0, BYTES.load(Relaxed) - b0, PEAK.load(Relaxed));
    let st = b.stats();
    let nv = b.navigation_stats();
    let code = b.it.stats();
    println!(
        "{name:8} x{scale:<3} checksum=ok run={run_ms:9.1}ms cpu={cpu}ms peak_rss={:.1}MiB peak_live={:.1}MiB live_after={:.1}MiB allocs={allocs} alloc={:.1}MiB | history={}/{} state_bytes={} urls={} params={} iters={} param_bytes={} wrappers={} events={} gen={} | gc={gc_ms:.1}ms freed_objs={freed_objs} scripts={} source_bytes={}",
        status_kib("VmHWM:") as f64 / 1024.0,
        peak_live as f64 / 1048576.0,
        LIVE.load(Relaxed) as f64 / 1048576.0,
        abytes as f64 / 1048576.0,
        nv.history_index,
        nv.history_len,
        nv.state_bytes,
        nv.urls,
        nv.params,
        nv.iterators,
        nv.param_bytes,
        st.wrappers,
        st.events,
        nv.url_generation,
        code.scripts,
        code.script_source,
    );
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() != 3 {
        die("usage: navigation_load url|iterate|history|storage|route 1|10|100".into());
    }
    let scale: usize = a[2].parse().unwrap_or_else(|_| die("scale".into()));
    // the evaluator recurses: run on a big stack
    let name = a[1].clone();
    std::thread::Builder::new().stack_size(256 << 20).spawn(move || exec(&name, scale)).unwrap().join().unwrap_or_else(|_| die("panic".into()));
}
