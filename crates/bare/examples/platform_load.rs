//! Load test for the platform module (`matchMedia`, `classList`, `getElementsBy*`). One case at one scale per process.
//!
//!   platform_load CASE SCALE      CASE: media | class | query      SCALE: 1, 10, 100
//!
//! media: SCALE*1000 viewport changes over 8 live media lists (7 `addEventListener` listeners and one `onchange`), with
//! throw-away lists made and collected on the way. class: SCALE*2000 `classList` operations (add, remove, toggle with
//! and without force, replace, forEach, contains) on 50 elements, checked against an ordered-set model in Rust and
//! against the native selector matcher. query: SCALE*20 rounds of `getElementsByClassName` / `getElementsByTagName` over
//! 1000 elements while classes change. Each case runs in small scripts, with a VM collect between them, and checks its
//! result against the same computation in Rust. Prints time, CPU, peak RSS, allocations and what the realm holds.
//! `tools/js/platform_load.sh` runs the matrix under caps.

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

#[derive(Default, Clone)]
struct Set(Vec<String>);

impl Set {
    fn has(&self, t: &str) -> bool {
        self.0.iter().any(|x| x == t)
    }
    fn add(&mut self, t: &str) {
        if !self.has(t) {
            self.0.push(t.to_string());
        }
    }
    fn remove(&mut self, t: &str) {
        self.0.retain(|x| x != t);
    }
    fn toggle(&mut self, t: &str, force: Option<bool>) -> bool {
        if self.has(t) {
            if force == Some(true) {
                return true;
            }
            self.remove(t);
            return false;
        }
        if force == Some(false) {
            return false;
        }
        self.0.push(t.to_string());
        true
    }
    fn replace(&mut self, old: &str, new: &str) -> bool {
        if !self.has(old) {
            return false;
        }
        let mut out = Vec::new();
        let mut placed = false;
        for x in self.0.drain(..) {
            if x == old || x == new {
                if !placed {
                    out.push(new.to_string());
                    placed = true;
                }
            } else {
                out.push(x);
            }
        }
        self.0 = out;
        true
    }
}

fn exec(name: &str, scale: usize) {
    let eval = EvalLimits { max_steps: u64::MAX / 2, max_scripts: 1 << 14, max_envs: 1 << 22, max_functions: 1 << 22, ..Default::default() };
    let mut b = Browser::new(tree::parse(PAGE), BrowserLimits::default(), eval, HeapLimits::default()).unwrap_or_else(|e| die(format!("browser: {e:?}")));
    b.set_document_url("https://staging.example.test/login").unwrap_or_else(|e| die(e));
    // Warm up the wrappers every case needs (`document.body` makes the html and body wrappers), then take the baseline.
    run(&mut b, "document.getElementsByTagName('*').length; document.body.children.length; document.body.appendChild(document.createElement('hr')).remove(); 0");
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
    let mut events = 0usize;
    match name {
        "media" => {
            let n = 1000 * scale;
            run(&mut b, "var c=[0,0,0,0,0,0,0,0], last=[], ms=[];
                for (var k=0;k<8;k++) (function(k){ var m=matchMedia('(max-width: '+((k+1)*100)+'px)'); ms.push(m); last.push(m.matches);
                  if (k==7) m.onchange=function(e){ c[k]++; last[k]=e.matches }; else m.addEventListener('change', function(e){ c[k]++; last[k]=e.matches }); })(k);
                function junk(){ for (var j=0;j<5;j++) matchMedia('(min-width: '+(j*50)+'px)'); }");
            let mut cur: Vec<bool> = (0..8).map(|k| 1280 <= (k + 1) * 100).collect();
            let mut want = [0usize; 8];
            let mut got_fired = 0usize;
            for i in 0..n {
                let w = (i * 37 % 1000) + 1;
                got_fired += b.set_viewport(w as f32, 800.0).unwrap_or_else(|e| die(format!("viewport: {e:?}")));
                for k in 0..8 {
                    let now = w <= (k + 1) * 100;
                    if now != cur[k] {
                        cur[k] = now;
                        want[k] += 1;
                        events += 1;
                    }
                }
                if i % 100 == 99 {
                    run(&mut b, "junk()");
                    collect(&mut b);
                }
            }
            expect("media fired", got_fired.to_string(), events.to_string());
            expect("media counts", run(&mut b, "c.join()"), want.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(","));
            let lasts = cur.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(",");
            expect("media last", run(&mut b, "last.join()"), lasts.clone());
            expect("media matches", run(&mut b, "ms.map(function(m){return m.matches}).join()"), lasts);
            collect(&mut b);
            let p = b.platform_stats();
            expect("media retained", format!("{},{}", p.media_lists, p.rooted_media_lists), "8,8".into());
            run(&mut b, "ms=null; last=null; c=null;");
            let p = b.platform_stats();
            expect("media still rooted by listeners", format!("{},{}", p.media_lists, p.rooted_media_lists), "8,8".into());
        }
        "class" => {
            let n = 2000 * scale;
            run(&mut b, "var els=[]; for (var i=0;i<50;i++){ var e=document.createElement('i'); e.id='e'+i; e.className='base'; document.body.appendChild(e); els.push(e) } var e=null; var sum=0;
                function chunk(s,c){ for (var i=s;i<s+c;i++){ var cl=els[i%50].classList; var t='t'+((i*7)%13), u='t'+((i*5+3)%13);
                  switch (i%6) { case 0: cl.add(t,u); break; case 1: cl.remove(t); break; case 2: sum += cl.toggle(t)?1:0; break; case 3: sum += cl.replace(t,u)?1:0; break;
                    case 4: sum += cl.toggle(u, i%4==0)?1:0; break; case 5: var k=0; cl.forEach(function(){ k++ }); sum += k + cl.length*1000 + (cl.contains(t)?1:0); break; } } }");
            let mut model: Vec<Set> = (0..50).map(|_| Set(vec!["base".into()])).collect();
            let mut sum = 0usize;
            for s in (0..n).step_by(100) {
                run(&mut b, &format!("chunk({s}, 100)"));
                for i in s..s + 100 {
                    let m = &mut model[i % 50];
                    let (t, u) = (format!("t{}", (i * 7) % 13), format!("t{}", (i * 5 + 3) % 13));
                    match i % 6 {
                        0 => {
                            m.add(&t);
                            m.add(&u);
                        }
                        1 => m.remove(&t),
                        2 => sum += usize::from(m.toggle(&t, None)),
                        3 => sum += usize::from(m.replace(&t, &u)),
                        4 => sum += usize::from(m.toggle(&u, Some(i % 4 == 0))),
                        _ => sum += m.0.len() + m.0.len() * 1000 + usize::from(m.has(&t)),
                    }
                }
                if s % 1000 == 900 {
                    collect(&mut b);
                }
            }
            expect("class sum", run(&mut b, "sum"), sum.to_string());
            let want: Vec<String> = model.iter().map(|m| m.0.join(" ")).collect();
            expect("class names", run(&mut b, "els.map(function(e){return e.className}).join('|')"), want.join("|"));
            expect("class length", run(&mut b, "els.map(function(e){return e.classList.length}).join()"), model.iter().map(|m| m.0.len().to_string()).collect::<Vec<_>>().join(","));
            // The native selector matcher agrees (cache invalidation) for every token.
            for t in 0..13 {
                let tok = format!("t{t}");
                let first = model.iter().position(|m| m.has(&tok));
                let id = b.query(&format!(".{tok}")).unwrap_or_else(|e| die(format!("query: {e:?}"))).map(|n| b.document().borrow().attr(n, bare_html::dom::Atom::ID).unwrap_or("").to_string());
                expect("class selector", id.unwrap_or_default(), first.map(|i| format!("e{i}")).unwrap_or_default());
                let count = model.iter().filter(|m| m.has(&tok)).count();
                expect("class by name", run(&mut b, &format!("document.getElementsByClassName('{tok}').length")), count.to_string());
            }
            events = n;
            collect(&mut b);
            expect("class lists", b.platform_stats().token_lists.to_string(), "50".into());
            // Attached elements keep their wrappers and lists; detached and dropped ones are released.
            run(&mut b, "els=null");
            collect(&mut b);
            expect("class attached", format!("{},{}", b.platform_stats().token_lists, b.stats().wrappers), format!("50,{}", wrappers0 + 50));
            run(&mut b, "var all=document.body.children; for (var i=all.length-1;i>=0;i--) all[i].remove(); all=null");
            collect(&mut b);
            let p = b.platform_stats();
            expect("class released", format!("{},{},{}", p.token_lists, p.token_iters, b.stats().wrappers), format!("0,0,{wrappers0}"));
        }
        "query" => {
            let n = 20 * scale;
            run(&mut b, "var els=[]; for (var i=0;i<1000;i++){ var e=document.createElement(i%3==0?'p':'span'); e.className='k'+(i%10)+' all'; document.body.appendChild(e); els.push(e) } var e=null;
                function toggle(x,k){ return els[x].classList.toggle('k'+k) ? 1 : 0 }");
            let mut flags: Vec<u16> = (0..1000).map(|i| 1u16 << (i % 10)).collect();
            let (mut got, mut want) = (0usize, 0usize);
            for r in 0..n {
                let (x, k) = ((r * 13) % 1000, r % 10);
                flags[x] ^= 1 << k;
                got += run(&mut b, &format!("toggle({x},{k}) + document.getElementsByClassName('k{k} all').length + document.getElementsByTagName('{}').length + els[{x}].getElementsByTagName('*').length", ["p", "span", "*"][r % 3])).parse::<usize>().unwrap_or(0);
                let tag = match r % 3 {
                    0 => (0..1000).filter(|i| i % 3 == 0).count(),
                    1 => (0..1000).filter(|i| i % 3 != 0).count(),
                    _ => 1003,
                };
                want += usize::from(flags[x] & (1 << k) != 0) + flags.iter().filter(|f| *f & (1 << k) != 0).count() + tag;
                events += 1;
                if r % 10 == 9 {
                    collect(&mut b);
                }
            }
            expect("query total", got.to_string(), want.to_string());
            collect(&mut b);
            run(&mut b, "var all=document.body.children; for (var i=all.length-1;i>=0;i--) all[i].remove(); els=null; all=null");
            collect(&mut b);
            expect("query released", format!("{},{}", b.platform_stats().token_lists, b.stats().wrappers), format!("0,{wrappers0}"));
        }
        _ => die(format!("unknown case {name}")),
    }
    let run_ms = t.elapsed().as_secs_f64() * 1e3;
    let cpu = cpu_ms() - cpu0;
    let (allocs, abytes, peak_live) = (ALLOCS.load(Relaxed) - a0, BYTES.load(Relaxed) - b0, PEAK.load(Relaxed));
    let st = b.stats();
    let p = b.platform_stats();
    let code = b.it.stats();
    println!(
        "{name:6} x{scale:<3} checksum=ok run={run_ms:9.1}ms cpu={cpu}ms peak_rss={:.1}MiB peak_live={:.1}MiB live_after={:.1}MiB allocs={allocs} alloc={:.1}MiB | ops={events} media={}/{} token_lists={} iters={} wrappers={} listeners={} nodes={} text_bytes={} | gc={gc_ms:.1}ms freed_objs={freed_objs} scripts={} source_bytes={}",
        status_kib("VmHWM:") as f64 / 1024.0,
        peak_live as f64 / 1048576.0,
        LIVE.load(Relaxed) as f64 / 1048576.0,
        abytes as f64 / 1048576.0,
        p.media_lists,
        p.rooted_media_lists,
        p.token_lists,
        p.token_iters,
        st.wrappers,
        st.listeners,
        st.nodes,
        st.text_bytes,
        code.scripts,
        code.script_source,
    );
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() != 3 {
        die("usage: platform_load media|class|query 1|10|100".into());
    }
    let scale: usize = a[2].parse().unwrap_or_else(|_| die("scale".into()));
    let name = a[1].clone();
    // the evaluator recurses: run on a big stack (release: at least 8 MiB)
    std::thread::Builder::new().stack_size(64 << 20).spawn(move || exec(&name, scale)).unwrap().join().unwrap_or_else(|_| die("panic".into()));
}
