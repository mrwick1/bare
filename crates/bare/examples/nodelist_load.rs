//! Load test for `Document.getElementsByName` and the live `NodeList`. One case at one scale per process.
//!
//!   nodelist_load CASE SCALE      CASE: live | churn | scan      SCALE: 1, 10, 100
//!
//! live: SCALE*1000 operations on 24 elements in two attached containers, with five lists held for the whole run (names `a`,
//! `b`, `c`, the empty string, and one that nothing has). Operations: set or remove `name` through script, move an element to
//! any place of either container, detach it, and the same three through the native `Document` between scripts. Reads of every
//! list are mixed into the script. A Rust model (the two containers as ordered lists, the `name` of each element) predicts
//! every read; it does not use anything that JavaScript returned. At the end the native tree must equal the model, and a
//! collection with the lists held must change none of the answers.
//! churn: SCALE*20 rounds of 100 lists with different names, read in part, dropped, then a collection. The host tables must
//! hold no list after the last collection.
//! scan: a document of 2000 elements; SCALE*20 changes of one `name`, each followed by 50 reads of an unchanged list (the
//! document is scanned once for each change, never for a read). The model counts the members.
//!
//! Prints time, CPU, peak RSS and allocation counts. `tools/js/nodelist_load.sh` runs the matrix under caps.

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

const PAGE: &str = "<!doctype html><html><head></head><body><div id=c0></div><div id=c1></div></body></html>";
const NAMES: [&str; 4] = ["a", "b", "c", ""];

/// A small deterministic generator (LCG), so a failure repeats.
struct Rng(u64);
impl Rng {
    fn next(&mut self, n: usize) -> usize {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 33) as usize) % n
    }
}

/// The model: the elements of the two containers in order, and the `name` of every element (`None`: no attribute).
struct Model {
    cont: [Vec<usize>; 2],
    name: Vec<Option<usize>>,
}

impl Model {
    fn remove(&mut self, j: usize) {
        for c in self.cont.iter_mut() {
            c.retain(|&x| x != j);
        }
    }
    fn place(&mut self, j: usize, c: usize, pos: usize) {
        self.remove(j);
        let at = pos.min(self.cont[c].len());
        self.cont[c].insert(at, j);
    }
    /// The ids of the members of list `k` (`k == 4` is the list nothing matches), in tree order.
    fn list(&self, k: usize) -> String {
        if k == 4 {
            return String::new();
        }
        self.cont.iter().flatten().filter(|&&j| self.name[j] == Some(k)).map(|j| format!("e{j}")).collect::<Vec<_>>().join(",")
    }
}

const IDS: &str = "function ids(l) { var r = []; for (var i = 0; i < l.length; i++) r.push(l[i].id); return r.join(); }";

fn find(doc: &bare_html::dom::Document, id: &str) -> bare_html::dom::NodeId {
    let mut st = vec![doc.root()];
    while let Some(n) = st.pop() {
        if doc.attr(n, bare_html::dom::Atom::ID) == Some(id) {
            return n;
        }
        st.extend(doc.children(n));
    }
    die(format!("no #{id}"))
}

fn exec(name: &str, scale: usize) {
    let eval = EvalLimits { max_steps: u64::MAX / 2, max_scripts: 1 << 14, max_envs: 1 << 22, max_functions: 1 << 22, ..Default::default() };
    let mut b = Browser::new(tree::parse(PAGE), BrowserLimits::default(), eval, HeapLimits::default()).unwrap_or_else(|e| die(format!("browser: {e:?}")));
    run(&mut b, "document.body.appendChild(document.createElement('hr')).remove(); 0");
    b.collect(&[]).unwrap_or_else(|e| die(format!("collect: {e:?}")));
    let (a0, b0, cpu0) = (ALLOCS.load(Relaxed), BYTES.load(Relaxed), cpu_ms());
    PEAK.store(LIVE.load(Relaxed), Relaxed);
    let t = Instant::now();
    let mut gc_ms = 0.0;
    let mut collect = |b: &mut Browser| {
        run(b, "0");
        let t = Instant::now();
        b.collect(&[]).unwrap_or_else(|e| die(format!("collect: {e:?}")));
        gc_ms += t.elapsed().as_secs_f64() * 1e3;
    };
    let mut ops = 0usize;
    let (before, after);
    match name {
        "live" => {
            const N: usize = 24;
            run(&mut b, &format!("{IDS} var els = [], cont = [document.getElementById('c0'), document.getElementById('c1')]; for (var i = 0; i < {N}; i++) {{ var e = document.createElement('i'); e.id = 'e' + i; els.push(e); cont[i % 2].appendChild(e); }} var NM = ['a', 'b', 'c', '']; var Ls = [document.getElementsByName('a'), document.getElementsByName('b'), document.getElementsByName('c'), document.getElementsByName(''), document.getElementsByName('nothing')]; var out; e = null;"));
            let mut m = Model { cont: [(0..N).filter(|i| i % 2 == 0).collect(), (0..N).filter(|i| i % 2 == 1).collect()], name: vec![None; N] };
            let doc = b.document();
            let nodes: Vec<_> = (0..N).map(|i| find(&doc.borrow(), &format!("e{i}"))).collect();
            let conts = [find(&doc.borrow(), "c0"), find(&doc.borrow(), "c1")];
            let mut rng = Rng(42);
            let total = 1000 * scale;
            let mut done = 0;
            while done < total {
                let batch = 50.min(total - done);
                let mut js = String::from("out = [];");
                let mut want: Vec<String> = Vec::new();
                for _ in 0..batch {
                    let j = rng.next(N);
                    match rng.next(9) {
                        0 | 1 => {
                            let n = rng.next(4);
                            js += &format!("els[{j}].setAttribute('name', NM[{n}]);");
                            m.name[j] = Some(n);
                        }
                        2 => {
                            js += &format!("els[{j}].removeAttribute('name');");
                            m.name[j] = None;
                        }
                        3 | 4 => {
                            let (c, pos) = (rng.next(2), rng.next(14));
                            js += &format!("els[{j}].remove(); cont[{c}].insertBefore(els[{j}], cont[{c}].children[{pos}] || null);");
                            m.place(j, c, pos);
                        }
                        5 => {
                            js += &format!("els[{j}].remove();");
                            m.remove(j);
                        }
                        _ => {
                            let k = rng.next(5);
                            js += &format!("out.push(ids(Ls[{k}]) + '#' + Ls[{k}].length);");
                            want.push(format!("{}#{}", m.list(k), if k == 4 { 0 } else { m.list(k).split(',').filter(|s| !s.is_empty()).count() }));
                        }
                    }
                }
                js += "out.join(';')";
                expect("live reads", run(&mut b, &js), want.join(";"));
                ops += batch;
                done += batch;
                // Between scripts, Rust changes the native document directly, and the model follows.
                for _ in 0..3 {
                    let j = rng.next(N);
                    match rng.next(3) {
                        0 => {
                            let n = rng.next(4);
                            doc.borrow_mut().set_attr(nodes[j], "name", NAMES[n]);
                            m.name[j] = Some(n);
                        }
                        1 => {
                            let (c, pos) = (rng.next(2), rng.next(14));
                            m.place(j, c, pos);
                            let mut d = doc.borrow_mut();
                            d.detach(nodes[j]);
                            let at = m.cont[c].iter().position(|&x| x == j).unwrap();
                            let next = m.cont[c].get(at + 1).map(|&x| nodes[x]);
                            d.move_to(nodes[j], conts[c], next);
                        }
                        _ => {
                            doc.borrow_mut().detach(nodes[j]);
                            m.remove(j);
                        }
                    }
                    ops += 1;
                }
                if (done / 50) % 20 == 0 {
                    collect(&mut b);
                }
            }
            // The native tree equals the model, and every list equals the model after the last native change.
            let d = doc.borrow();
            for c in 0..2 {
                let native: Vec<String> = d.children(conts[c]).map(|n| d.attr(n, bare_html::dom::Atom::ID).unwrap_or("?").to_string()).collect();
                let want: Vec<String> = m.cont[c].iter().map(|j| format!("e{j}")).collect();
                if native != want {
                    die(format!("native container {c}: {native:?} != {want:?}"));
                }
            }
            for j in 0..N {
                let have = d.find_atom("name").and_then(|a| d.attr(nodes[j], a)).map(str::to_string);
                if have != m.name[j].map(|n| NAMES[n].to_string()) {
                    die(format!("native name of e{j}: {have:?}"));
                }
            }
            drop(d);
            let expect_all = (0..5).map(|k| format!("{}#{}", m.list(k), if k == 4 { 0 } else { m.list(k).split(',').filter(|s| !s.is_empty()).count() })).collect::<Vec<_>>().join(";");
            let read_all = "var o2 = []; for (var k = 0; k < 5; k++) o2.push(ids(Ls[k]) + '#' + Ls[k].length); o2.join(';')";
            expect("live final", run(&mut b, read_all), expect_all.clone());
            collect(&mut b);
            expect("live after gc", run(&mut b, read_all), expect_all);
            before = b.nodelist_stats().lists;
            run(&mut b, "Ls = null");
            collect(&mut b);
            after = b.nodelist_stats().lists;
            if after != 0 {
                die(format!("live: {after} lists survive the collection"));
            }
        }
        "churn" => {
            let rounds = 20 * scale;
            run(&mut b, "var e0 = document.createElement('i'); e0.setAttribute('name', 'r0'); document.body.appendChild(e0); var sum = 0; function round(r) { var keep = []; for (var i = 0; i < 100; i++) { var l = document.getElementsByName('r' + (i % 7)); sum += l.length; if (i % 10 === 0) keep.push(l); } return keep.length; }");
            // 100 lists a round; the `i % 7 == 0` ones match the one element: i = 0,7,...,98 -> 15 lists, each of length 1.
            let per_round = (0..100).filter(|i| i % 7 == 0).count() as u64;
            for r in 0..rounds {
                expect("churn keep", run(&mut b, &format!("round({r})")), "10".into());
                collect(&mut b);
                ops += 100;
            }
            expect("churn sum", run(&mut b, "sum"), (per_round * rounds as u64).to_string());
            before = b.nodelist_stats().lists;
            collect(&mut b);
            after = b.nodelist_stats().lists;
            if after != 0 || b.nodelist_stats().cached_ids != 0 || b.nodelist_stats().name_bytes != 0 {
                die(format!("churn: {:?} survive the collection", b.nodelist_stats()));
            }
        }
        "scan" => {
            const N: usize = 2000;
            run(&mut b, &format!("var els = []; for (var i = 0; i < {N}; i++) {{ var e = document.createElement('i'); e.setAttribute('name', i % 5 === 0 ? 'hit' : 'miss'); document.body.appendChild(e); els.push(e); }} var L = document.getElementsByName('hit'); e = null; L.length"));
            let mut hits: Vec<bool> = (0..N).map(|i| i % 5 == 0).collect();
            let mut rng = Rng(7);
            for r in 0..20 * scale {
                let j = rng.next(N);
                hits[j] = !hits[j];
                let count = hits.iter().filter(|&&h| h).count();
                let first = hits.iter().position(|&h| h).map_or("none".to_string(), |i| i.to_string());
                run(&mut b, &format!("els[{j}].setAttribute('name', '{}')", if hits[j] { "hit" } else { "miss" }));
                // 50 reads of an unchanged list: the first rebuilds, the others answer from the cache
                let got = run(&mut b, "var s = 0; for (var q = 0; q < 50; q++) s += L.length + (L.length ? els.indexOf(L[0]) : -1); s");
                let f: i64 = if first == "none" { -1 } else { first.parse().unwrap() };
                expect("scan reads", got, (50 * (count as i64 + f)).to_string());
                ops += 50;
                if r % 200 == 199 {
                    collect(&mut b);
                }
            }
            before = b.nodelist_stats().lists;
            run(&mut b, "L = null");
            collect(&mut b);
            after = b.nodelist_stats().lists;
            if after != 0 {
                die(format!("scan: {after} lists survive the collection"));
            }
        }
        _ => die(format!("unknown case {name}")),
    }
    let el = t.elapsed();
    println!(
        "case={name} scale={scale} ops={ops} wall={:.1}ms cpu={}ms gc={:.1}ms peak_rss={}MiB peak_heap={}KiB allocs={} alloc_bytes={}KiB lists_before_drop={before} lists_after_drop={after} checksum=ok",
        el.as_secs_f64() * 1e3,
        cpu_ms() - cpu0,
        gc_ms,
        status_kib("VmHWM:") / 1024,
        PEAK.load(Relaxed) / 1024,
        ALLOCS.load(Relaxed) - a0,
        (BYTES.load(Relaxed) - b0) / 1024,
    );
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() != 3 {
        die("usage: nodelist_load CASE SCALE".into());
    }
    let scale: usize = a[2].parse().unwrap_or_else(|_| die("bad scale".into()));
    let case = a[1].clone();
    // Debug builds need a large stack for the interpreter, release builds 8 MiB.
    let stack = if cfg!(debug_assertions) { 64 << 20 } else { 8 << 20 };
    let h = std::thread::Builder::new().stack_size(stack).spawn(move || exec(&case, scale)).unwrap();
    if h.join().is_err() {
        std::process::exit(1);
    }
}
