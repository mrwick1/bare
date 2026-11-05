//! Load test for the DOM/JS bridge. One case at one scale per process.
//!
//!   browser_load CASE SCALE      CASE: mutation | event | lookup | svg | svgparse | rel      SCALE: 1, 10, 100
//!
//! mutation: SCALE*1000 elements are made, filled, appended to a list, and old ones removed in blocks.
//! event: SCALE*1000 bubbling events through a 4 level tree with 3 listeners on the path. Half start in JS,
//! half in the Rust test driver. lookup: SCALE*500 rounds of `querySelectorAll`, `getElementById` and `closest`
//! on a 200 item page. svg: SCALE*1000 SVG subtrees made with `createElementNS` and namespaced attributes, appended,
//! cloned (every 10th) and removed in blocks. svgparse: a page of SCALE*500 inline SVG icons that the parser makes, then
//! namespace and attribute reads on the first SCALE*50 (attached wrappers stay alive, so the wrapper cap bounds the reads). Each icon has an SVG title with HTML markup, an SVG style with a character reference and markup, and a CDATA section.
//! rel: SCALE*1000 `link` elements get a `rel` by a string and by an object (ToString), are read and lowercased, removed (every
//! third), cloned (every seventh) and kept as a list of at most 1000. Each case runs in small scripts, with a VM collect between them, and checks its
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

fn page(name: &str, scale: usize) -> String {
    match name {
        "svgparse" => {
            let icons: String = (0..500 * scale).map(|i| format!("<svg viewBox='0 0 24 24' xmlns='http://www.w3.org/2000/svg'><g><path d='M{i}' xlink:href='#g{i}'/><foreignObject><div>x</div></foreignObject><title><b>t</b></title><style>&amp;<g/></style><text><![CDATA[<c>]]></text></g></svg>")).collect();
            format!("<body><p id=l>{icons}</p></body>")
        }
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
    let tp = Instant::now();
    let doc = tree::parse(&page(name, scale));
    let parse_ms = tp.elapsed().as_secs_f64() * 1e3;
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
        "svg" => {
            let n = 1000 * scale;
            run(&mut b, "var host = document.getElementById('l'), SVG = 'http://www.w3.org/2000/svg', XL = 'http://www.w3.org/1999/xlink', XMLNS = 'http://www.w3.org/2000/xmlns/', H = 'http://www.w3.org/1999/xhtml', score = 0;
                function make(i) {
                  var s = document.createElementNS(SVG, 'svg');
                  s.setAttribute('viewBox', '0 0 24 24'); s.setAttributeNS(XMLNS, 'xmlns:xlink', XL);
                  var g = document.createElementNS(SVG, 'g'), p = document.createElementNS(SVG, 'path'), fo = document.createElementNS(SVG, 'foreignObject'), d = document.createElement('div');
                  p.setAttributeNS(null, 'data-i', '' + i); p.setAttributeNS(XL, 'xlink:href', '#g' + i);
                  fo.appendChild(d); g.appendChild(p); g.appendChild(fo); s.appendChild(g);
                  return s;
                }
                function chunk(start, count) {
                  for (var i = start; i < start + count; i++) {
                    var s = make(i);
                    host.appendChild(s);
                    var c = i % 10 === 0 ? s.cloneNode(true) : s;
                    var g = c.firstElementChild, p = g.firstElementChild, fo = g.lastElementChild;
                    score += Number(p.getAttributeNS(null, 'data-i')) + p.getAttributeNS(XL, 'href').length + (c.namespaceURI === SVG) + (fo.namespaceURI === SVG) + (fo.firstElementChild.namespaceURI === H) + fo.tagName.length;
                    if (host.childElementCount > 1000) for (var k = 0; k < 500; k++) host.removeChild(host.firstChild);
                  }
                }");
            let mut want = 0;
            for start in (0..n).step_by(CHUNK) {
                run(&mut b, &format!("chunk({start}, {CHUNK})"));
                for i in start..start + CHUNK {
                    // data-i + length of "#g<i>" + three true checks + the length of "foreignObject"
                    want += i + (2 + i.to_string().len()) + 3 + 13;
                }
                collect(&mut b);
            }
            expect("svg", run(&mut b, "score"), want.to_string());
        }
        "svgparse" => {
            // The page holds 500*scale icons. The script reads the first 50*scale: wrappers of attached nodes live until the node
            // leaves the tree (up to 11 for each icon), so more would end at the wrapper cap (65,536) by design.
            let n = 50 * scale;
            run(&mut b, "var SVG = 'http://www.w3.org/2000/svg', XL = 'http://www.w3.org/1999/xlink', H = 'http://www.w3.org/1999/xhtml', score = 0, host = document.getElementById('l'), cur = host.firstElementChild;
                function chunk(start, count) {
                  for (var i = 0; i < count; i++) {
                    var p = cur.firstElementChild.firstElementChild, fo = p.nextElementSibling;
                    score += Number(p.getAttribute('d').slice(1)) + p.getAttributeNS(XL, 'href').length + (p.namespaceURI === SVG) + (fo.localName === 'foreignObject') + (fo.firstElementChild.namespaceURI === H) + cur.getAttribute('viewBox').length;
                    var t = fo.nextElementSibling, st = t.nextElementSibling, tx = st.nextElementSibling;
                    score += (t.firstElementChild.namespaceURI === H) + (st.firstChild.data === '&') + (st.firstElementChild.namespaceURI === SVG) + tx.firstChild.data.length;
                    cur = cur.nextElementSibling;
                  }
                }");
            expect("svgparse count", run(&mut b, "host.childElementCount"), (500 * scale).to_string());
            let mut want = 0;
            for start in (0..n).step_by(CHUNK) {
                run(&mut b, &format!("chunk({start}, {CHUNK})"));
                for i in start..start + CHUNK {
                    // d number + length of "#g<i>" + three true checks + the length of "0 0 24 24" (the svg is `cur`)
                    // + three true checks (HTML `b` in the SVG title, the decoded `&` in the SVG style, the SVG `g` in it) + the length of "<c>" (CDATA)
                    want += i + (2 + i.to_string().len()) + 2 + 1 + 9 + 3 + 3;
                }
                collect(&mut b);
            }
            expect("svgparse", run(&mut b, "score"), want.to_string());
        }
        "rel" => {
            // `link.rel` is a reflected string: set by a plain string and by an object (ToString), read lowercased,
            // removed, cloned, and kept as the list of links is trimmed to 1000.
            let n = 1000 * scale;
            run(&mut b, "var ul = document.getElementById('l'), score = 0;
                function chunk(start, count) {
                  for (var i = start; i < start + count; i++) {
                    var e = document.createElement('link');
                    e.rel = 'R' + i + ' X';
                    score += e.rel.length + e.rel.toLowerCase().length;
                    e.rel = { toString: function () { return 'T' + i; } };
                    if (i % 3 === 0) e.removeAttribute('rel');
                    score += e.rel.length;
                    if (i % 7 === 0) score += e.cloneNode(false).rel.length;
                    ul.appendChild(e);
                    if (ul.childElementCount > 1000) for (var k = 0; k < 500; k++) ul.removeChild(ul.firstChild);
                  }
                }");
            let (mut want, mut count) = (0, 0);
            let mut last = String::new();
            for start in (0..n).step_by(CHUNK) {
                run(&mut b, &format!("chunk({start}, {CHUNK})"));
                for i in start..start + CHUNK {
                    // the lowercase copy has the same length; the value after the removal (or the clone) is `v`
                    let v = if i % 3 == 0 { 0 } else { format!("T{i}").len() };
                    want += 2 * format!("R{i} X").len() + v + if i % 7 == 0 { v } else { 0 };
                    count += 1;
                    if count > 1000 {
                        count -= 500;
                    }
                    last = if i % 3 == 0 { String::new() } else { format!("T{i}") };
                }
                collect(&mut b);
            }
            expect("rel", run(&mut b, "score"), want.to_string());
            expect("rel count", run(&mut b, "ul.childElementCount"), count.to_string());
            expect("rel last", run(&mut b, "ul.lastElementChild.rel"), last);
        }
        _ => die(format!("unknown case {name}")),
    }
    let run_ms = t.elapsed().as_secs_f64() * 1e3;
    let cpu = cpu_ms() - cpu0;
    let (allocs, abytes, peak_live) = (ALLOCS.load(Relaxed) - a0, BYTES.load(Relaxed) - b0, PEAK.load(Relaxed));
    let st = b.stats();
    let code = b.it.stats();
    println!(
        "{name:8} x{scale:<3} checksum=ok parse={parse_ms:.1}ms run={run_ms:9.1}ms cpu={cpu}ms peak_rss={:.1}MiB peak_live={:.1}MiB live_after={:.1}MiB allocs={allocs} alloc={:.1}MiB | wrappers={} listeners={} events={} nodes={} text={:.2}MiB | gc={gc_ms:.1}ms freed_objs={freed_objs} scripts={} source_bytes={} code_nodes={}",
        status_kib("VmHWM:") as f64 / 1024.0,
        peak_live as f64 / 1048576.0,
        LIVE.load(Relaxed) as f64 / 1048576.0,
        abytes as f64 / 1048576.0,
        st.wrappers,
        st.listeners,
        st.events,
        st.nodes,
        st.text_bytes as f64 / 1048576.0,
        code.scripts,
        code.script_source,
        code.script_nodes,
    );
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() != 3 {
        die("usage: browser_load mutation|event|lookup|svg|svgparse|rel 1|10|100".into());
    }
    let scale: usize = a[2].parse().unwrap_or_else(|_| die("scale".into()));
    // the evaluator recurses: run on a big stack
    let name = a[1].clone();
    // BARE_LOAD_STACK_MIB sets the VM stack (default 256).
    let stack_mib: usize = std::env::var("BARE_LOAD_STACK_MIB").ok().and_then(|v| v.parse().ok()).unwrap_or(256);
    std::thread::Builder::new().stack_size(stack_mib << 20).spawn(move || exec(&name, scale)).unwrap().join().unwrap_or_else(|_| die("panic".into()));
}
