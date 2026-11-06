//! Load test for `XMLHttpRequest` over a real loopback HTTP server inside this process.
//!
//!   xhr_load N      N: the number of sequential GETs (1, 100, 1000)
//!
//! Each round makes an `XMLHttpRequest`, sends a GET, waits for `load` and drops the object. The script checks that each reply
//! is "hello" with status 200. A collect runs every 100 rounds. At the end the live wrappers, events and listeners must equal the
//! baseline taken after the first read of the globals. Prints time, CPU, peak RSS and those counts.
//! `tools/js/xhr_load.sh` runs the rows. Only throwaway synthetic data. No external network.

use bare::browser::*;
use bare::browser_fetch::*;
use bare_html::tree;
use bare_js::eval::*;
use bare_js::heap::HeapLimits;
use bare_net::http::Client;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Instant;

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

/// A loopback server: every request gets `hello`.
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
    let mut piece = [0u8; 4096];
    loop {
        while !seen.windows(4).any(|w| w == b"\r\n\r\n") {
            match s.read(&mut piece) {
                Ok(0) | Err(_) => return,
                Ok(n) => seen.extend_from_slice(&piece[..n]),
            }
        }
        let end = seen.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
        seen.drain(..end);
        if s.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 5\r\n\r\nhello").is_err() {
            return;
        }
    }
}

fn exec(n: usize) {
    let port = server();
    let eval = EvalLimits { max_steps: u64::MAX / 2, ..Default::default() };
    let mut b = Browser::new(tree::parse("<body></body>"), BrowserLimits::default(), eval, HeapLimits::default()).unwrap_or_else(|e| die(format!("browser: {e:?}")));
    b.configure_fetch(FetchConfig { base_url: Some(format!("http://127.0.0.1:{port}/app")), client: Client::new(), limits: FetchLimits::default() }).unwrap();
    run(&mut b, "var ok = 0, bad = 0; function one() { var x = new XMLHttpRequest(); x.onload = function () { if (x.status === 200 && x.responseText === 'hello') ok++; else bad++; }; x.open('GET', '/t'); x.send(); }");
    b.collect(&[]).unwrap();
    let base = b.stats();
    let cpu0 = cpu_ms();
    let t = Instant::now();
    let mut gc_ms = 0.0;
    for i in 0..n {
        run(&mut b, "one()");
        while b.pending_fetches() > 0 {
            b.run_network_tasks(10).unwrap_or_else(|e| die(format!("network: {e:?}")));
        }
        b.drain().unwrap();
        if i % 100 == 99 {
            let t = Instant::now();
            b.collect(&[]).unwrap();
            gc_ms += t.elapsed().as_secs_f64() * 1e3;
        }
    }
    let run_ms = t.elapsed().as_secs_f64() * 1e3;
    let cpu = cpu_ms() - cpu0;
    let got = run(&mut b, "ok + ',' + bad");
    if got != format!("{n},0") {
        die(format!("replies: {got} != {n},0"));
    }
    b.collect(&[]).unwrap();
    let s = b.stats();
    let live = (s.wrappers, s.events, s.listeners);
    if live != (base.wrappers, base.events, base.listeners) {
        die(format!("live objects {live:?} != baseline {:?}", (base.wrappers, base.events, base.listeners)));
    }
    println!(
        "xhr x{n:<5} replies=ok run={run_ms:9.1}ms cpu={cpu}ms peak_rss={:.1}MiB | wrappers={} events={} listeners={} (baseline equal) | gc={gc_ms:.1}ms",
        status_kib("VmHWM:") as f64 / 1024.0,
        s.wrappers,
        s.events,
        s.listeners,
    );
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() != 2 {
        die("usage: xhr_load N".into());
    }
    let n: usize = a[1].parse().unwrap_or_else(|_| die("N".into()));
    std::thread::Builder::new().stack_size(64 << 20).spawn(move || exec(n)).unwrap().join().unwrap_or_else(|_| die("panic".into()));
}
