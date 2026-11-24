//! Time of `Input::Type` into a React page served from a directory over loopback. One run: load the page, then type.
//!
//!   type_load DIR SELECTOR CHARS [virtual]
//!
//! DIR holds `index.html` and the files it links. `tools/js/typing/build.sh` makes one (a production React 19 form with 60
//! inputs and 2 textareas). CHARS is the length of the text that is typed in one `Input::Type` (0: load only, so a run with 0
//! and a run with N differ by the cost of N keys). Prints the load time, the typing time and the value length.
//! Only local files. No external network.

#[path = "../src/test_server.rs"]
mod test_server;

use bare::Input;
use bare::browser_session::{Session, SessionConfig, SessionStop};
use bare::Stop;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::time::Instant;
use test_server::{Reply, serve};

/// Live and peak bytes that the program asked the allocator for (the allocator's own slack is not counted: see the RSS).
struct Counting;
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

fn add(n: usize) {
    let l = LIVE.fetch_add(n, Relaxed) + n;
    PEAK.fetch_max(l, Relaxed);
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        add(l.size());
        unsafe { System.alloc(l) }
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        add(l.size());
        unsafe { System.alloc_zeroed(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        LIVE.fetch_sub(l.size(), Relaxed);
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        LIVE.fetch_sub(l.size(), Relaxed);
        add(new);
        unsafe { System.realloc(p, l, new) }
    }
}

#[global_allocator]
static A: Counting = Counting;

fn status_kib(key: &str) -> u64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    s.lines().find_map(|l| l.strip_prefix(key)).and_then(|v| v.trim().trim_end_matches("kB").trim().parse().ok()).unwrap_or(0)
}

/// Where the memory is (only with `MEM=1`): RSS and peak RSS of the process, live and peak bytes of the allocator.
fn mem(s: &mut Session, what: &str) {
    if std::env::var_os("MEM").is_none() {
        return;
    }
    let mib = |b: usize| b as f64 / 1048576.0;
    eprintln!(
        "mem {what}: rss {:.1} peak {:.1} MiB; alloc live {:.1} peak {:.1} MiB",
        status_kib("VmRSS:") as f64 / 1024.0, status_kib("VmHWM:") as f64 / 1024.0, mib(LIVE.load(Relaxed)), mib(PEAK.load(Relaxed)),
    );
}

fn die(msg: String) -> ! {
    eprintln!("FAIL: {msg}");
    std::process::exit(1)
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 4 {
        die("usage: type_load DIR SELECTOR CHARS [virtual]".into());
    }
    let (dir, sel, n, virt) = (a[1].clone(), a[2].clone(), a[3].parse::<usize>().unwrap_or(0), a.get(4).is_some_and(|v| v == "virtual"));
    std::thread::Builder::new().stack_size(64 << 20).spawn(move || run(&dir, &sel, n, virt)).unwrap().join().unwrap();
}

fn run(dir: &str, sel: &str, n: usize, virt: bool) {
    let root = dir.to_string();
    let server = serve(move |path| {
        let p = if path == "/" { "/index.html" } else { path };
        let Ok(body) = std::fs::read(format!("{root}{}", p.split('?').next().unwrap_or(""))) else { return Reply::new(404, None, "") };
        let ty = if p.ends_with(".js") { "application/javascript" } else if p.ends_with(".css") { "text/css" } else { "text/html; charset=utf-8" };
        Reply::new(200, Some(ty), body)
    });
    let mut cfg = SessionConfig::new(&server.url("/"));
    cfg.virtual_clock = virt;
    cfg.eval.max_steps = u64::MAX / 2;
    let t = Instant::now();
    let mut s = Session::new(cfg);
    let r = s.run();
    let load = t.elapsed();
    mem(&mut s, "after load");
    // `LOGS=1`: the console, the errors of listeners and the failed requests of the page.
    if std::env::var_os("LOGS").is_some()
        && let Some(b) = s.browser()
    {
        for (level, text) in b.take_console() {
            eprintln!("console {level}: {text}");
        }
        for r in b.take_reports() {
            eprintln!("error in {}: {}", r.event_type, r.message);
        }
        for d in b.take_fetch_diagnostics() {
            eprintln!("fetch: {d}");
        }
    }
    if std::env::var_os("MEM").is_some() {
        let b = s.browser().expect("browser");
        let t = Instant::now();
        let _ = b.collect(&[]);
        eprintln!("collect: {:.2} ms", t.elapsed().as_secs_f64() * 1000.0);
        mem(&mut s, "after load + collect");
    }
    if r.stop != SessionStop::Page(Stop::Settled) {
        die(format!("load stopped: {:?}", r.stop));
    }
    let text: String = (0..n).map(|i| (b'a' + (i % 26) as u8) as char).collect();
    let t = Instant::now();
    let r = if n == 0 { None } else { Some(s.resume(&[Input::Type(sel, &text)])) };
    let typed = t.elapsed();
    mem(&mut s, "after typing");
    if let Some(r) = &r
        && r.stop != SessionStop::Page(Stop::Settled) {
        die(format!("typing stopped: {:?}", r.stop));
    }
    if let Some(b) = s.browser() {
        let _ = b.script("localStorage.setItem('counts', JSON.stringify(window.__c || {}))");
    }
    let origin = server.url("");
    let counts = s.storage().get_item(false, &origin, "counts").unwrap_or_default();
    let len = s.browser().and_then(|b| b.query(sel).ok().flatten().and_then(|e| b.input_value(e))).map_or(0, |v| v.len());
    let (tasks, rounds) = r.map_or((0, 0), |r| (r.tasks, r.rounds));
    println!("type_load {sel} x{n}{}: load {:.0} ms, type {:.0} ms ({:.1} ms/key), tasks {tasks} rounds {rounds}, value {len} chars, renders {counts}", if virt { " virtual" } else { "" }, load.as_secs_f64() * 1000.0, typed.as_secs_f64() * 1000.0, typed.as_secs_f64() * 1000.0 / n.max(1) as f64);
    if len != n {
        die(format!("the value has {len} chars, not {n}"));
    }
}
