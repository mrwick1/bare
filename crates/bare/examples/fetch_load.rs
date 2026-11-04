//! Load test for `fetch` over a real loopback HTTP server inside this process. One case at one scale per process.
//!
//!   fetch_load CASE SCALE      CASE: post | bytes | objects      SCALE: 1, 10, 100
//!
//! post: SCALE*100 login-style JSON POSTs, 8 chains at a time, with a VM collect between batches. Each reply carries the
//! length and byte sum of the body the server got; the script adds them up and the total is compared with Rust.
//! bytes: SCALE MiB of response bodies (1 MiB each), read through `body.getReader()` in 64 KiB chunks. The script checks
//! the first, middle and last byte of each chunk against the pattern and counts the length.
//! objects: SCALE*1000 rounds of Headers/Request/Response/clone/reader objects, with a collect between batches, ending with
//! zero retained entries. Prints time, CPU, peak RSS, allocations and what the fetch layer holds.
//! `tools/js/fetch_load.sh` runs the matrix under caps. Only throwaway synthetic data. No external network.

use bare::browser::*;
use bare::browser_fetch::*;
use bare_html::tree;
use bare_js::eval::*;
use bare_js::heap::HeapLimits;
use bare_net::http::Client;
use std::alloc::{GlobalAlloc, Layout, System};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
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

const MIB: usize = 1 << 20;

fn pattern(i: usize) -> u8 {
    (i.wrapping_mul(7).wrapping_add(3)) as u8
}

/// A loopback server: `POST /api` answers `{"n":len,"s":sum}`; `GET /bytes` sends 1 MiB of the pattern.
fn server() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::Builder::new().stack_size(1 << 20).spawn(move || {
        for s in l.incoming().flatten() {
            std::thread::Builder::new().stack_size(1 << 20).spawn(move || conn(s)).unwrap();
        }
    }).unwrap();
    port
}

fn conn(mut s: TcpStream) {
    let mut seen: Vec<u8> = Vec::new();
    let mut piece = [0u8; 16384];
    let data: Vec<u8> = (0..MIB).map(pattern).collect();
    loop {
        let end = loop {
            if let Some(at) = seen.windows(4).position(|w| w == b"\r\n\r\n") {
                break at + 4;
            }
            match s.read(&mut piece) {
                Ok(0) | Err(_) => return,
                Ok(n) => seen.extend_from_slice(&piece[..n]),
            }
        };
        let head = String::from_utf8_lossy(&seen[..end]).to_ascii_lowercase();
        seen.drain(..end);
        let len = head.lines().find_map(|l| l.strip_prefix("content-length:")).and_then(|v| v.trim().parse::<usize>().ok()).unwrap_or(0);
        while seen.len() < len {
            match s.read(&mut piece) {
                Ok(0) | Err(_) => return,
                Ok(n) => seen.extend_from_slice(&piece[..n]),
            }
        }
        let body: Vec<u8> = seen.drain(..len).collect();
        let ok = if head.starts_with("get /bytes") {
            let mut r = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", data.len()).into_bytes();
            r.extend_from_slice(&data);
            s.write_all(&r)
        } else {
            let sum: u64 = body.iter().map(|&b| b as u64).sum();
            let json = format!("{{\"n\":{},\"s\":{sum}}}", body.len());
            s.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{json}", json.len()).as_bytes())
        };
        if ok.is_err() {
            return;
        }
    }
}

fn drive(b: &mut Browser) {
    while b.pending_fetches() > 0 {
        b.run_network_tasks(1000).unwrap_or_else(|e| die(format!("network: {e:?}")));
    }
}

fn exec(name: &str, scale: usize) {
    let port = server();
    let eval = EvalLimits { max_steps: u64::MAX / 2, max_scripts: 1 << 14, max_envs: 1 << 22, max_functions: 1 << 22, ..Default::default() };
    let mut b = Browser::new(tree::parse("<body></body>"), BrowserLimits::default(), eval, HeapLimits::default()).unwrap_or_else(|e| die(format!("browser: {e:?}")));
    b.configure_fetch(FetchConfig { base_url: Some(format!("http://127.0.0.1:{port}/app")), client: Client::new(), limits: FetchLimits::default() }).unwrap();
    let (a0, b0, cpu0) = (ALLOCS.load(Relaxed), BYTES.load(Relaxed), cpu_ms());
    PEAK.store(LIVE.load(Relaxed), Relaxed);
    let t = Instant::now();
    let mut gc_ms = 0.0;
    let mut collect = |b: &mut Browser| {
        let t = Instant::now();
        b.collect(&[]).unwrap_or_else(|e| die(format!("collect: {e:?}")));
        gc_ms += t.elapsed().as_secs_f64() * 1e3;
    };
    let mut max_pending = 0;
    let mut max_retained = 0;
    match name {
        "post" => {
            let n = 100 * scale;
            run(&mut b, "var total = 0, count = 0;
                function go(i, end) { if (i >= end) return; var body = JSON.stringify({u: 'user' + i, p: 'x' + i});
                  fetch('/api/auth/login', {method: 'POST', headers: {'Content-Type': 'application/json'}, body: body})
                    .then(function (r) { return r.json(); }).then(function (j) { total += j.n * 1000 + j.s % 997; count++; go(i + 8, end); }); }");
            let (mut want, mut chunks) = (0u64, 0);
            for start in (0..n).step_by(80) {
                let end = (start + 80).min(n);
                run(&mut b, &format!("for (var k = 0; k < 8; k++) go({start} + k, {end});"));
                max_pending = max_pending.max(b.pending_fetches());
                drive(&mut b);
                for i in start..end {
                    let body = format!("{{\"u\":\"user{i}\",\"p\":\"x{i}\"}}");
                    want += body.len() as u64 * 1000 + body.bytes().map(|x| x as u64).sum::<u64>() % 997;
                }
                chunks += 1;
                max_retained = max_retained.max(b.fetch_stats().retained_bytes);
                collect(&mut b);
            }
            expect("post", run(&mut b, "count + ',' + total"), format!("{n},{want}"));
            let _ = chunks;
        }
        "bytes" => {
            run(&mut b, "var bad = 0, len = 0, done = 0;
                function want(o) { return (o * 7 + 3) & 255; }
                function one() { return fetch('/bytes').then(function (r) { var rd = r.body.getReader(), o = 0;
                  function pump() { return rd.read().then(function (x) { if (x.done) { len += o; done++; return; }
                    var v = x.value, n = v.length;
                    if (v[0] !== want(o) || v[n - 1] !== want(o + n - 1) || v[n >> 1] !== want(o + (n >> 1))) bad++;
                    o += n; return pump(); }); }
                  return pump(); }); }");
            for _ in 0..scale {
                run(&mut b, "one()");
                drive(&mut b);
                max_retained = max_retained.max(b.fetch_stats().retained_bytes);
                collect(&mut b);
            }
            expect("bytes", run(&mut b, "bad + ',' + len + ',' + done"), format!("0,{},{scale}", scale * MIB));
        }
        "objects" => {
            let n = 1000 * scale;
            run(&mut b, "var acc = 0;
                function round(i) { var r = new Response('v' + i, {status: 200, headers: {'X-I': '' + i}}); var c = r.clone(); var q = new Request('/x/' + i, {method: 'POST', body: 'b'}).clone();
                  var rd = c.body.getReader(); acc += r.headers.get('x-i').length + q.method.length; r.text().then(function (t) { acc += t.length; }); }");
            let mut want = 0usize;
            for start in (0..n).step_by(100) {
                run(&mut b, &format!("for (var i = {start}; i < {}; i++) round(i);", start + 100));
                b.drain().unwrap_or_else(|e| die(format!("drain: {e:?}")));
                for i in start..start + 100 {
                    want += i.to_string().len() + 4 + 1 + i.to_string().len();
                }
                max_retained = max_retained.max(b.fetch_stats().objects);
                collect(&mut b);
            }
            expect("objects", run(&mut b, "acc"), want.to_string());
            expect("objects pruned", format!("{}", b.fetch_stats().objects), "0".into());
        }
        "iterate" => {
            // Headers from a Map and from another Headers' iterator, for-of with a live append, spread, keys/values, an early break.
            let n = 1000 * scale;
            run(&mut b, "var acc = 0;
                function round(i) { var h = new Headers(new Map([['X-A' + (i % 5), '' + i], ['b', 'x'], ['B', 'y']]));
                  var g = new Headers(h.entries()), k = 0;
                  for (var [name, v] of h) { acc += name.length + v.length; if (k++ === 0) h.append('z', '' + (i % 3)); }
                  for (var e of g) { acc += e[0].length; break; }
                  acc += [...g.keys()].length + [...g.values()].join('').length + [...h].length; }");
            let mut want = 0usize;
            for start in (0..n).step_by(100) {
                run(&mut b, &format!("for (var i = {start}; i < {}; i++) round(i);", start + 100));
                for i in start..start + 100 {
                    // h sorted: b="x, y" (5), x-a<k>=<i> (4+d), then the appended z=<i%3> (2); the break (1), g's keys (2), values (4+d), spread of h (3)
                    want += 21 + 2 * i.to_string().len();
                }
                max_retained = max_retained.max(b.fetch_stats().objects);
                collect(&mut b);
            }
            expect("iterate", run(&mut b, "acc"), want.to_string());
            expect("iterate pruned", format!("{}", b.fetch_stats().objects), "0".into());
        }
        _ => die(format!("unknown case {name}")),
    }
    let run_ms = t.elapsed().as_secs_f64() * 1e3;
    let cpu = cpu_ms() - cpu0;
    let (allocs, abytes, peak_live) = (ALLOCS.load(Relaxed) - a0, BYTES.load(Relaxed) - b0, PEAK.load(Relaxed));
    let st = b.fetch_stats();
    println!(
        "{name:8} x{scale:<3} checksum=ok run={run_ms:9.1}ms cpu={cpu}ms peak_rss={:.1}MiB peak_live={:.1}MiB live_after={:.1}MiB allocs={allocs} alloc={:.1}MiB | started={} pending_max={max_pending} max_retained_or_objects={max_retained} final_objects={} final_retained={} final_queued={} | gc={gc_ms:.1}ms",
        status_kib("VmHWM:") as f64 / 1024.0,
        peak_live as f64 / 1048576.0,
        LIVE.load(Relaxed) as f64 / 1048576.0,
        abytes as f64 / 1048576.0,
        st.started,
        st.objects,
        st.retained_bytes,
        st.queued_bytes,
    );
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() != 3 {
        die("usage: fetch_load post|bytes|objects|iterate 1|10|100".into());
    }
    let scale: usize = a[2].parse().unwrap_or_else(|_| die("scale".into()));
    let name = a[1].clone();
    std::thread::Builder::new().stack_size(64 << 20).spawn(move || exec(&name, scale)).unwrap().join().unwrap_or_else(|_| die("panic".into()));
}
