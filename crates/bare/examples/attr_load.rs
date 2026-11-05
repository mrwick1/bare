//! Load test for `Element.attributes` (`NamedNodeMap`) and `Attr`. One case at one scale per process.
//!
//!   attr_load CASE SCALE      CASE: react | live | churn      SCALE: 1, 10, 100
//!
//! react: SCALE*500 rounds. Each round builds an element with 16 attributes through five different paths (`setAttribute`,
//! `setAttributeNS`, `className`, `style`, `classList`), then empties it the way React 19 empties `<html>`
//! (`while (map.length) el.removeAttributeNode(map[0])`, or `removeAttribute` of the last entry). The checksum is the sum of
//! the name and value lengths of the removed attributes, in removal order. Rust computes the expected sum from the generating
//! rule alone, not from anything JavaScript returned. After the loop the native document must hold no attribute on the 20
//! kept elements.
//! live: SCALE*1000 operations on 20 attached elements that keep their maps and some `Attr`s: `setAttribute`, `removeAttribute`,
//! `setAttributeNS`, `removeAttributeNS`, `className`, `style`, and reads through `map.length`, `map[i]`, `Attr.ownerElement` and
//! `Attr.value` of Attrs held since earlier operations. Writes also come from Rust straight into the native `Document` between
//! scripts. A model in Rust (ordered list, one id for each attribute lifetime) predicts every read. Each script runs 100
//! operations. At the end the native attribute lists must equal the model.
//! churn: SCALE*20 rounds of 100 throw-away detached elements with maps and Attrs, then a collection. All `Attr` and map
//! objects of the dead elements must be gone from the host tables.
//!
//! Prints time, CPU, peak RSS and allocation counts. `tools/js/attr_load.sh` runs the matrix under caps.

use bare::browser::*;
use bare_html::tree;
use bare_js::eval::*;
use bare_js::heap::HeapLimits;
use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::HashMap;
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
const XLINK: &str = "http://www.w3.org/1999/xlink";

/// The model of one element: attributes in order, each with the id of its lifetime.
#[derive(Default, Clone)]
struct Model {
    attrs: Vec<(u64, String, String)>,
}

impl Model {
    fn find(&self, q: &str) -> Option<usize> {
        self.attrs.iter().position(|a| a.1 == q)
    }
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
        run(b, "0"); // the interpreter keeps the last script value until the next script
        let t = Instant::now();
        b.collect(&[]).unwrap_or_else(|e| die(format!("collect: {e:?}")));
        gc_ms += t.elapsed().as_secs_f64() * 1e3;
    };
    let mut ops = 0usize;
    let (before, after);
    match name {
        "react" => {
            let rounds = 500 * scale;
            run(&mut b, "var kept=[]; for (var i=0;i<20;i++){ var e=document.createElement('div'); e.id='k'+i; document.body.appendChild(e); kept.push(e) } var e=null; var sum=0;");
            run(&mut b, &format!(
                "function setx(el,i,r){{ el.setAttributeNS('{XLINK}', 'xlink:h' + i, 'x' + ((r + i) % 5)); }}
                 function round2(r) {{
                   var el = (r % 20 < 5) ? kept[r % 20] : document.createElement('section'); el.removeAttribute('id');
                   for (var i = 0; i < 16; i++) {{
                     switch (i % 5) {{
                       case 0: el.setAttribute('data-' + i, 'v' + ((r + i) % 7)); break;
                       case 1: setx(el, i, r); break;
                       case 2: el.setAttribute('t' + i, 'w' + (i % 3)); break;
                       case 3: el.setAttribute('r' + i, ''); break;
                       case 4: el.setAttribute('z' + i, 'q'); break;
                     }}
                   }}
                   var m = el.attributes; if (m.length !== 16) throw new Error('length ' + m.length);
                   if (r % 2) {{ while (m.length) {{ var a = el.removeAttributeNode(m[0]); sum += a.name.length + a.value.length; if (a.ownerElement !== null) throw new Error('attached'); }} }}
                   else {{ while (m.length) {{ var a = m[m.length - 1]; sum += a.name.length + a.value.length; el.removeAttribute(a.name); }} }}
                   if (el.hasAttributes()) throw new Error('left');
                 }}
                 function rounds(s, c) {{ for (var r = s; r < s + c; r++) round2(r); }}"
            ));
            let mut sum = 0u64;
            for r in 0..rounds {
                // Expected sum: attributes in creation order, removed from the front (odd round) or the back (even round).
                let mut list: Vec<(String, String)> = Vec::new();
                for i in 0..16usize {
                    list.push(match i % 5 {
                        0 => (format!("data-{i}"), format!("v{}", (r + i) % 7)),
                        1 => (format!("xlink:h{i}"), format!("x{}", (r + i) % 5)),
                        2 => (format!("t{i}"), format!("w{}", i % 3)),
                        3 => (format!("r{i}"), String::new()),
                        _ => (format!("z{i}"), "q".to_string()),
                    });
                }
                for (n, v) in list {
                    sum += (n.len() + v.len()) as u64;
                }
                ops += 16 + 16;
            }
            for s in (0..rounds).step_by(100) {
                run(&mut b, &format!("rounds({s}, {})", 100.min(rounds - s)));
                if s % 1000 == 900 {
                    collect(&mut b);
                }
            }
            expect("react sum", run(&mut b, "sum"), sum.to_string());
            let doc = b.document();
            for i in 0..20usize {
                let el = b.query(&format!("div:nth-child({})", i + 1)).unwrap_or_else(|e| die(format!("query {e:?}"))).unwrap_or_else(|| die(format!("missing k{i}")));
                let n = doc.borrow().attr_count(el);
                let want = if i < 5 && rounds > i { 0 } else { 1 };
                if n != want {
                    die(format!("native attrs of k{i}: {n} != {want}"));
                }
            }
            before = b.attr_stats().attrs;
            run(&mut b, "kept=null");
            collect(&mut b);
            after = b.attr_stats().attrs;
        }
        "live" => {
            let n = 1000 * scale;
            run(&mut b, &format!("var els=[], maps=[], held=[]; for (var i=0;i<20;i++){{ var e=document.createElement('div'); e.id='e'+i; document.body.appendChild(e); els.push(e); maps.push(e.attributes); held.push(null) }} var e=null; var sum=0;
                var N=['a','b','c','data-x','title','lang','role','hidden'];
                function chunk(s, c) {{ for (var k = s; k < s + c; k++) {{ var i = k % 20, el = els[i], m = maps[i], nm = N[(k * 3 + ((k / 20) | 0)) % 8], op = (k * 7 + ((k / 20) | 0)) % 9;
                  switch (op) {{
                    case 0: el.setAttribute(nm, 'v' + (k % 11)); break;
                    case 1: el.removeAttribute(nm); break;
                    case 2: el.setAttributeNS('{XLINK}', 'xlink:href', 'u' + (k % 13)); break;
                    case 3: el.removeAttributeNS('{XLINK}', 'href'); break;
                    case 4: el.className = 'k' + (k % 5); break;
                    case 5: el.style.color = 'red'; break;
                    case 6: break;
                    case 7: sum += m.length * 7; if (m.length) {{ var at = m[k % m.length]; sum += at.name.length * 31 + at.value.length; }} break;
                    case 8: var h = held[i]; if (h) {{ sum += (h.ownerElement === el ? 1 : 2) * 3 + h.value.length; }} held[i] = m.length ? m[m.length - 1] : null; break;
                  }} }} }}"));
            let mut models = vec![Model { attrs: vec![] }; 20];
            for (i, m) in models.iter_mut().enumerate() {
                m.attrs.push((i as u64 + 1, "id".into(), format!("e{i}")));
            }
            let mut next_id = 100u64;
            let mut removed: HashMap<u64, String> = HashMap::new();
            // The held Attr of each element: its lifetime id.
            let mut held: Vec<Option<u64>> = vec![None; 20];
            let mut sum = 0u64;
            let names = ["a", "b", "c", "data-x", "title", "lang", "role", "hidden"];
            let doc = b.document();
            let els: Vec<_> = (0..20).map(|i| b.query(&format!("#e{i}")).unwrap_or_else(|e| die(format!("query {e:?}"))).unwrap_or_else(|| die("missing".into()))).collect();
            let set = |m: &mut Model, q: &str, v: &str, next_id: &mut u64| match m.find(q) {
                Some(p) => m.attrs[p].2 = v.to_string(),
                None => {
                    m.attrs.push((*next_id, q.to_string(), v.to_string()));
                    *next_id += 1;
                }
            };
            let del = |m: &mut Model, q: &str, removed: &mut HashMap<u64, String>| {
                if let Some(p) = m.find(q) {
                    let a = m.attrs.remove(p);
                    removed.insert(a.0, a.2);
                }
            };
            for s in (0..n).step_by(100) {
                let c = 100.min(n - s);
                run(&mut b, &format!("chunk({s}, {c})"));
                for k in s..s + c {
                    let i = k % 20;
                    let nm = names[(k * 3 + k / 20) % 8];
                    let m = &mut models[i];
                    match (k * 7 + k / 20) % 9 {
                        0 => set(m, nm, &format!("v{}", k % 11), &mut next_id),
                        1 => del(m, nm, &mut removed),
                        2 => set(m, "xlink:href", &format!("u{}", k % 13), &mut next_id),
                        3 => del(m, "xlink:href", &mut removed),
                        4 => set(m, "class", &format!("k{}", k % 5), &mut next_id),
                        5 => set(m, "style", "color: red;", &mut next_id),
                        7 => {
                            sum += (m.attrs.len() * 7) as u64;
                            if !m.attrs.is_empty() {
                                let a = &m.attrs[k % m.attrs.len()];
                                sum += (a.1.len() * 31 + a.2.len()) as u64;
                            }
                        }
                        8 => {
                            if let Some(id) = held[i] {
                                let cur = m.attrs.iter().find(|a| a.0 == id);
                                let (attached, val) = match cur {
                                    Some(a) => (true, a.2.len()),
                                    None => (false, removed[&id].len()),
                                };
                                sum += (if attached { 1 } else { 2 }) * 3 + val as u64;
                            }
                            held[i] = m.attrs.last().map(|a| a.0);
                        }
                        _ => {}
                    }
                    ops += 1;
                }
                // Writes that never pass through JavaScript: the native document changes, the model follows.
                // (The `op 6` slots of this chunk, in order.)
                for k in s..s + c {
                    if (k * 7 + k / 20) % 9 != 6 {
                        continue;
                    }
                    let i = k % 20;
                    let nm = names[(k * 5 + k / 20) % 8];
                    let m = &mut models[i];
                    if k % 2 == 0 {
                        let v = format!("n{}", k % 9);
                        doc.borrow_mut().set_attr(els[i], nm, &v);
                        set(m, nm, &v, &mut next_id);
                    } else {
                        doc.borrow_mut().remove_attr(els[i], nm);
                        del(m, nm, &mut removed);
                    }
                    ops += 1;
                }
                if s % 1000 == 900 {
                    collect(&mut b);
                }
            }
            expect("live sum", run(&mut b, "sum"), sum.to_string());
            // The native lists equal the model, and the map says the same.
            for i in 0..20 {
                let native: Vec<(String, String)> = {
                    let d = doc.borrow();
                    let bare_html::dom::NodeData::Element { attrs, .. } = *d.data(els[i]) else { die("not element".into()) };
                    d.attrs(attrs).iter().map(|a| (d.atom_str(a.name).to_string(), d.text(a.value).to_string())).collect()
                };
                let want: Vec<(String, String)> = models[i].attrs.iter().map(|a| (a.1.clone(), a.2.clone())).collect();
                if native != want {
                    die(format!("native attrs of e{i}: {native:?} != {want:?}"));
                }
            }
            expect("live maps", run(&mut b, "maps.map(function (m) { return Array.prototype.map.call(m, function (a) { return a.name + '=' + a.value; }).join(';') + '#' + m.length; }).join('|')"),
                models.iter().map(|m| m.attrs.iter().map(|a| format!("{}={}", a.1, a.2)).collect::<Vec<_>>().join(";") + "#" + &m.attrs.len().to_string()).collect::<Vec<_>>().join("|"));
            before = b.attr_stats().attrs;
            run(&mut b, "els=maps=held=null");
            collect(&mut b);
            after = b.attr_stats().attrs;
        }
        "churn" => {
            let rounds = 20 * scale;
            run(&mut b, "var sum=0; function round(r) { var keep = []; for (var i = 0; i < 100; i++) { var e = document.createElement('p'); e.setAttribute('a', String(i)); e.setAttribute('b', String(r)); var m = e.attributes; sum += m[0].value.length + m[1].value.length + m.length; if (i % 10 === 0) keep.push(e.removeAttributeNode(m[0])); } return keep.length; }");
            let mut sum = 0u64;
            for r in 0..rounds {
                for i in 0..100usize {
                    sum += (i.to_string().len() + r.to_string().len() + 2) as u64;
                }
                ops += 100;
            }
            for r in 0..rounds {
                expect("churn keep", run(&mut b, &format!("round({r})")), "10".into());
                collect(&mut b);
            }
            expect("churn sum", run(&mut b, "sum"), sum.to_string());
            before = b.attr_stats().attrs;
            collect(&mut b);
            after = b.attr_stats().attrs;
            if after != 0 || b.attr_stats().maps != 0 {
                die(format!("churn: {} Attrs and {} maps survive the collection", after, b.attr_stats().maps));
            }
        }
        _ => die(format!("unknown case {name}")),
    }
    let el = t.elapsed();
    println!(
        "case={name} scale={scale} ops={ops} wall={:.1}ms cpu={}ms gc={:.1}ms peak_rss={}MiB peak_heap={}KiB allocs={} alloc_bytes={}KiB attrs_before_drop={before} attrs_after_drop={after} checksum=ok",
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
        die("usage: attr_load CASE SCALE".into());
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
