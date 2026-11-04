//! Load test for the style module (`HTMLElement.style` / `CSSStyleDeclaration`). One case at one scale per process.
//!
//!   style_load CASE SCALE      CASE: setget | churn | attrs      SCALE: 1, 10, 100
//!
//! setget: SCALE*2000 operations (`setProperty`, named setters, `removeProperty`, `getPropertyValue`, `!important`) on the
//! style of 50 elements, checked against an ordered-list model in Rust, then the native cascade is run and the used
//! `width` of every element is checked against the model. churn: SCALE*20 rounds of 20 throw-away detached elements that
//! get a style and a `cssText`; the declarations must all be collected. attrs: SCALE*500 outside `setAttribute("style")`
//! calls that the same declaration must see (`length`, `cssText`, `getPropertyValue`), with a checksum. Each case runs in
//! small scripts with a VM collect between them. Prints time, CPU, peak RSS, allocations, and what the realm holds before
//! and after the last collection. `tools/js/style_load.sh` runs the matrix under caps.

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

const P: [&str; 6] = ["color", "width", "height", "display", "margin-top", "--c"];
const V: [&[&str]; 6] = [&["red", "blue", "#fff"], &["1px", "2px", "30px"], &["5px", "10px"], &["block", "none", "inline"], &["1px", "2px"], &["x", "y"]];

/// An ordered block with the replace-in-place rule (no shorthand is used here, so no declaration overlaps another).
#[derive(Default, Clone)]
struct Block(Vec<(String, String, bool)>);

impl Block {
    fn set(&mut self, n: &str, v: &str, imp: bool) {
        match self.0.iter_mut().find(|d| d.0 == n) {
            Some(d) => *d = (n.into(), v.into(), imp),
            None => self.0.push((n.into(), v.into(), imp)),
        }
    }
    fn remove(&mut self, n: &str) -> String {
        match self.0.iter().position(|d| d.0 == n) {
            Some(i) => self.0.remove(i).1,
            None => String::new(),
        }
    }
    fn get(&self, n: &str) -> String {
        self.0.iter().find(|d| d.0 == n).map(|d| d.1.clone()).unwrap_or_default()
    }
    fn text(&self) -> String {
        self.0.iter().map(|d| format!("{}: {}{};", d.0, d.1, if d.2 { " !important" } else { "" })).collect::<Vec<_>>().join(" ")
    }
}

fn exec(name: &str, scale: usize) {
    let eval = EvalLimits { max_steps: u64::MAX / 2, max_scripts: 1 << 14, max_envs: 1 << 22, max_functions: 1 << 22, ..Default::default() };
    let mut b = Browser::new(tree::parse(PAGE), BrowserLimits::default(), eval, HeapLimits::default()).unwrap_or_else(|e| die(format!("browser: {e:?}")));
    run(&mut b, "document.body.children.length; document.body.appendChild(document.createElement('hr')).remove(); 0");
    run(&mut b, "0");
    b.collect(&[]).unwrap_or_else(|e| die(format!("collect: {e:?}")));
    let wrappers0 = b.stats().wrappers;
    let (a0, b0, cpu0) = (ALLOCS.load(Relaxed), BYTES.load(Relaxed), cpu_ms());
    PEAK.store(LIVE.load(Relaxed), Relaxed);
    let t = Instant::now();
    let mut gc_ms = 0.0;
    let mut freed_objs = 0;
    let mut collect = |b: &mut Browser| {
        run(b, "0"); // the interpreter keeps the last script value until the next script
        let t = Instant::now();
        freed_objs += b.collect(&[]).unwrap_or_else(|e| die(format!("collect: {e:?}"))).objects;
        gc_ms += t.elapsed().as_secs_f64() * 1e3;
    };
    let mut ops = 0usize;
    let (before, after): (usize, usize);
    match name {
        "setget" => {
            let n = 2000 * scale;
            run(&mut b, "var els=[]; for (var i=0;i<50;i++){ var e=document.createElement('i'); e.id='e'+i; document.body.appendChild(e); els.push(e) } var e=null; var sum=0;
                var P=['color','width','height','display','margin-top','--c'], C=['color','width','height','display','marginTop',null];
                var V=[['red','blue','#fff'],['1px','2px','30px'],['5px','10px'],['block','none','inline'],['1px','2px'],['x','y']];
                function chunk(s,c){ for (var i=s;i<s+c;i++){ var st=els[i%50].style, p=(i*7+((i/50)|0))%6, vs=V[p], v=vs[(i*5)%vs.length];
                  switch (i%5) { case 0: st.setProperty(P[p],v); break; case 1: if (C[p]) st[C[p]]=v; else st.setProperty(P[p],v); break;
                    case 2: sum += st.removeProperty(P[p]).length; break; case 3: sum += st.getPropertyValue(P[p]).length + st.length*100; break;
                    case 4: st.setProperty(P[p],v,'important'); break; } } }");
            let mut model = vec![Block::default(); 50];
            let mut sum = 0usize;
            for s in (0..n).step_by(100) {
                run(&mut b, &format!("chunk({s}, 100)"));
                for i in s..s + 100 {
                    let m = &mut model[i % 50];
                    let p = (i * 7 + i / 50) % 6;
                    let v = V[p][(i * 5) % V[p].len()];
                    match i % 5 {
                        0 | 1 => m.set(P[p], v, false),
                        2 => sum += m.remove(P[p]).len(),
                        3 => sum += m.get(P[p]).len() + m.0.len() * 100,
                        _ => m.set(P[p], v, true),
                    }
                    ops += 1;
                }
                if s % 1000 == 900 {
                    collect(&mut b);
                }
            }
            expect("setget sum", run(&mut b, "sum"), sum.to_string());
            expect("setget text", run(&mut b, "els.map(function(e){return e.style.cssText}).join('|')"), model.iter().map(Block::text).collect::<Vec<_>>().join("|"));
            expect("setget attr", run(&mut b, "els.map(function(e){return e.getAttribute('style') || ''}).join('|')"), model.iter().map(|m| m.text()).collect::<Vec<_>>().join("|"));
            expect("setget length", run(&mut b, "els.map(function(e){return e.style.length}).join()"), model.iter().map(|m| m.0.len().to_string()).collect::<Vec<_>>().join(","));
            // The native cascade sees the same widths.
            let doc = b.document();
            let ids: Vec<_> = (0..50).map(|i| b.query(&format!("#e{i}")).unwrap_or_else(|e| die(format!("query {e:?}"))).unwrap_or_else(|| die("missing".into()))).collect();
            let mut d = doc.borrow_mut();
            let mut rules = bare_css::cascade::RuleSet::new((1000.0, 500.0));
            rules.add(&mut d, bare_css::cascade::UA_CSS, bare_css::cascade::Origin::UserAgent);
            let styles = rules.compute(&d);
            for (i, id) in ids.iter().enumerate() {
                let w = model[i].get("width").trim_end_matches("px").parse::<f32>().unwrap_or(0.0);
                let got = styles.style(*id).map_or(-1.0, |s| s.px(bare_css::values::Property::Width));
                if got != w {
                    die(format!("cascade width of e{i}: {got} != {w}"));
                }
            }
            drop(d);
            before = b.style_stats().objects;
            run(&mut b, "els=null");
            collect(&mut b);
            // attached elements keep their wrappers and styles while the document holds them
            expect("setget attached", b.style_stats().objects.to_string(), "50".into());
            run(&mut b, "var all=document.body.children; for (var i=all.length-1;i>=0;i--) all[i].remove(); all=null");
            collect(&mut b);
            after = b.style_stats().objects;
            expect("setget released", format!("{after},{}", b.stats().wrappers), format!("0,{wrappers0}"));
        }
        "churn" => {
            let n = 20 * scale;
            run(&mut b, "var sum=0; function round(r){ var keep=[]; for (var i=0;i<20;i++){ var e=document.createElement('div'); var s=e.style;
                s.cssText='color: red; width: '+(i+1)+'px'; s.setProperty('height', (r%7+1)+'px'); s.setProperty('--n', ''+(r*20+i)); sum += s.length + s.width.length + s.getPropertyValue('--n').length; if (i%5==0) keep.push(s) } return keep.length }");
            let mut want = 0usize;
            for r in 0..n {
                expect("churn kept", run(&mut b, &format!("round({r})")), "4".into());
                for i in 0..20 {
                    want += 4 + format!("{}px", i + 1).len() + (r * 20 + i).to_string().len();
                }
                ops += 20;
                if r % 10 == 9 {
                    collect(&mut b);
                }
            }
            expect("churn sum", run(&mut b, "sum"), want.to_string());
            run(&mut b, "round(0)"); // 20 more declarations that no collection has seen
            before = b.style_stats().objects;
            collect(&mut b);
            after = b.style_stats().objects;
            expect("churn released", format!("{after},{}", b.stats().wrappers), format!("0,{wrappers0}"));
        }
        "attrs" => {
            let n = 500 * scale;
            run(&mut b, "var a=document.body.appendChild(document.createElement('p')), s=a.style, sum=0;
                function chunk(st,c){ for (var i=st;i<st+c;i++){ a.setAttribute('style', 'color: c'+i+'red; width: '+(i%90+1)+'px; margin-top: '+(i%9)+'px;; bad'); var x=s.cssText; sum += s.length*1000 + s.getPropertyValue('width').length + x.length + s.marginTop.length } }");
            let mut want = 0usize;
            for s in (0..n).step_by(100) {
                run(&mut b, &format!("chunk({s}, 100)"));
                for i in s..s + 100 {
                    // `color: c<i>red` is not a color, so the declaration is dropped; two are left.
                    let text = format!("width: {}px; margin-top: {}px;", i % 90 + 1, i % 9);
                    want += 2 * 1000 + format!("{}px", i % 90 + 1).len() + text.len() + format!("{}px", i % 9).len();
                    ops += 1;
                }
                if s % 1000 == 900 {
                    collect(&mut b);
                }
            }
            expect("attrs sum", run(&mut b, "sum"), want.to_string());
            before = b.style_stats().objects;
            collect(&mut b);
            after = b.style_stats().objects;
            expect("attrs one object", after.to_string(), "1".into());
        }
        _ => die(format!("unknown case {name}")),
    }
    let run_ms = t.elapsed().as_secs_f64() * 1e3;
    let cpu = cpu_ms() - cpu0;
    let (allocs, abytes, peak_live) = (ALLOCS.load(Relaxed) - a0, BYTES.load(Relaxed) - b0, PEAK.load(Relaxed));
    let st = b.stats();
    let code = b.it.stats();
    println!(
        "{name:6} x{scale:<3} checksum=ok run={run_ms:9.1}ms cpu={cpu}ms peak_rss={:.1}MiB peak_live={:.1}MiB live_after={:.1}MiB allocs={allocs} alloc={:.1}MiB | ops={ops} style_objs_before_gc={before} after_gc={after} wrappers={} nodes={} text_bytes={} attr_slots={} | gc={gc_ms:.1}ms freed_objs={freed_objs} scripts={} source_bytes={}",
        status_kib("VmHWM:") as f64 / 1024.0,
        peak_live as f64 / 1048576.0,
        LIVE.load(Relaxed) as f64 / 1048576.0,
        abytes as f64 / 1048576.0,
        st.wrappers,
        st.nodes,
        st.text_bytes,
        st.attr_slots,
        code.scripts,
        code.script_source,
    );
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() != 3 {
        die("usage: style_load setget|churn|attrs 1|10|100".into());
    }
    let scale: usize = a[2].parse().unwrap_or_else(|_| die("scale".into()));
    let name = a[1].clone();
    // the evaluator recurses: run on a big stack (release: at least 8 MiB)
    std::thread::Builder::new().stack_size(8 << 20).spawn(move || exec(&name, scale)).unwrap().join().unwrap_or_else(|_| die("panic".into()));
}
