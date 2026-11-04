//! Load test for the page loader and driver over a loopback HTTP server inside this process. One case at one scale per
//! process, on a virtual clock (nothing sleeps).
//!
//!   loader_load CASE SCALE      CASE: blocking | dynamic | mixed      SCALE: 1, 10, 100
//!
//! blocking: SCALE*20 external classic scripts (all different URLs) and SCALE*20 inline ones. Each adds its index to a sum
//! and to a rolling hash. dynamic: SCALE*50 scripts, each added by the `load` handler of the one before (one pending at a time).
//! mixed: SCALE*20 async scripts, each starts a `fetch` and a timer and a Promise chain; a replies counts its own bytes.
//! Every case compares sum, hash and counters with the same computation in Rust, then collects and checks that nothing
//! is left queued. Prints time, CPU, peak RSS and what the realm holds. `tools/js/loader_load.sh` runs the matrix under caps.
//! Only throwaway synthetic data. No external network.

#[path = "../src/test_server.rs"]
mod test_server;

use bare::browser::*;
use bare::browser_fetch::{FetchConfig, FetchLimits};
use bare::{PageConfig, PageLimits, Stop};
use bare_html::tree;
use bare_js::eval::*;
use bare_js::heap::HeapLimits;
use bare_net::http::Client;
use std::time::{Duration, Instant};
use test_server::{Reply, serve};

fn die(msg: String) -> ! {
    eprintln!("FAIL: {msg}");
    std::process::exit(1)
}

fn status_kib(key: &str) -> u64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    s.lines().find_map(|l| l.strip_prefix(key)).and_then(|v| v.trim().trim_end_matches("kB").trim().parse().ok()).unwrap_or(0)
}

fn cpu_ms() -> u64 {
    let s = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    let rest = s.rsplit_once(')').map_or("", |x| x.1);
    let f: Vec<&str> = rest.split_whitespace().collect();
    let t = |i: usize| f.get(i).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
    (t(11) + t(12)) * 10
}

fn exec(name: &str, scale: usize) {
    let (n_ext, n_dyn, n_mix) = (20 * scale, 50 * scale, 20 * scale);
    let body = |i: usize| format!("sum += {i}; h = (h * 31 + {i}) % 1000003;");
    let (html, kind) = match name {
        "blocking" => {
            let mut h = String::from("<script>var sum = 0, h = 0, calls = 0</script>");
            for i in 0..n_ext {
                h += &format!("<script src='/b{i}.js'></script><script>{}</script>", body(i + n_ext));
            }
            (h, 0)
        }
        "dynamic" => (
            format!("<script>var sum = 0, h = 0, calls = 0; function next(k) {{ if (k >= {n_dyn}) return; var s = document.createElement('script'); s.src = '/d' + k + '.js'; s.onload = function () {{ s.remove(); next(k + 1) }}; document.head.appendChild(s) }} next(0)</script>"),
            1,
        ),
        "mixed" => {
            let mut h = String::from("<script>var sum = 0, h = 0, calls = 0, bytes = 0, timers = 0, pr = 0</script>");
            for i in 0..n_mix {
                h += &format!("<script async src='/m{i}.js'></script>");
            }
            (h, 2)
        }
        _ => die(format!("unknown case {name}")),
    };
    let server = serve(move |path| {
        let num = |p: &str| path.trim_start_matches(p).trim_end_matches(".js").parse::<usize>().ok();
        match kind {
            0 => num("/b").map_or_else(|| Reply::new(404, None, ""), |i| Reply::js(format!("sum += {i}; h = (h * 31 + {i}) % 1000003; calls++"))),
            1 => num("/d").map_or_else(|| Reply::new(404, None, ""), |i| Reply::js(format!("sum += {i}; h = (h * 31 + {i}) % 1000003; calls++"))),
            _ if path == "/data" => Reply::new(200, Some("text/plain"), "0123456789"),
            _ => num("/m").map_or_else(
                || Reply::new(404, None, ""),
                |i| Reply::js(format!("calls++; fetch('/data').then(function (r) {{ return r.text() }}).then(function (t) {{ bytes += t.length; pr++ }}); setTimeout(function () {{ timers++; sum += {i}; h = (h * 31 + {i}) % 1000003 }}, {});", i % 7)),
            ),
        }
    });
    let eval = EvalLimits { max_steps: u64::MAX / 2, max_scripts: 1 << 14, max_envs: 1 << 22, max_functions: 1 << 22, ..Default::default() };
    let mut b = Browser::new(tree::parse(&html), BrowserLimits::default(), eval, HeapLimits::default()).unwrap_or_else(|e| die(format!("browser: {e:?}")));
    b.use_virtual_clock();
    let url = server.url("/page.html");
    b.configure_fetch(FetchConfig { base_url: Some(url.clone()), client: Client::new(), limits: FetchLimits::default() }).unwrap_or_else(|e| die(e));
    let (cpu0, t) = (cpu_ms(), Instant::now());
    let limits = PageLimits { max_tasks: 10_000_000, max_rounds: 10_000_000, max_time: Duration::from_secs(40), quiet: Duration::from_millis(50), resources: bare::resources::Limits { max_fetches: 1 << 16, max_references: 1 << 16, ..Default::default() }, ..Default::default() };
    let r = b.run_page(PageConfig { url, client: Client::new(), limits });
    if r.stop != Stop::Settled || r.load_errors != 0 || r.script_throws != 0 {
        die(format!("{r:?}"));
    }
    // Expected values, computed in Rust.
    let chain = |h: u64, v: usize| (h * 31 + v as u64) % 1000003;
    let (mut sum, mut h, mut calls) = (0u64, 0u64, 0usize);
    match name {
        "blocking" => {
            for i in 0..n_ext {
                for v in [i, i + n_ext] {
                    sum += v as u64;
                    h = chain(h, v);
                }
                calls += 1;
            }
        }
        "dynamic" => {
            for i in 0..n_dyn {
                sum += i as u64;
                h = chain(h, i);
                calls += 1;
            }
        }
        _ => {
            // timers with delay i % 7 fire by (delay, order); scripts ran in document order, so ties keep index order
            let mut order: Vec<usize> = (0..n_mix).collect();
            order.sort_by_key(|&i| (i % 7, i));
            for i in order {
                sum += i as u64;
                h = chain(h, i);
            }
            calls = n_mix;
        }
    }
    let got = {
        let v = b.run_script("[sum, h, calls].join()").unwrap_or_else(|e| die(format!("read: {e:?}")));
        b.it.display(v)
    };
    let want = format!("{sum},{h},{calls}");
    if got != want {
        die(format!("checksum {got} != {want}"));
    }
    if name == "mixed" {
        let v = b.run_script("[bytes, timers, pr].join()").unwrap();
        let want = format!("{},{n_mix},{n_mix}", 10 * n_mix);
        if b.it.display(v) != want {
            die(format!("mixed counters {} != {want}", b.it.display(v)));
        }
    }
    b.collect(&[]).unwrap_or_else(|e| die(format!("collect: {e:?}")));
    let (s, f, st) = (b.sched_stats(), b.fetch_stats(), b.stats());
    if (s.timers, s.tasks, f.pending, st.events) != (0, 0, 0, 0) {
        die(format!("not empty: {s:?} {f:?} events={}", st.events));
    }
    let run_ms = t.elapsed().as_secs_f64() * 1e3;
    println!(
        "{name:9} x{scale:<3} checksum=ok run={run_ms:9.1}ms cpu={}ms peak_rss={:.1}MiB | scripts={} fetches={} tasks={} rounds={} bytes={} | after: wrappers={} listeners={} events={} fetch_objs={} retained={}",
        cpu_ms() - cpu0,
        status_kib("VmHWM:") as f64 / 1024.0,
        r.scripts_run,
        r.fetches,
        r.tasks,
        r.rounds,
        r.bytes,
        st.wrappers,
        st.listeners,
        st.events,
        f.objects,
        f.retained_bytes,
    );
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        die("usage: loader_load CASE SCALE".into());
    }
    let (name, scale) = (args[1].clone(), args[2].parse::<usize>().unwrap_or_else(|_| die("bad scale".into())));
    // 8 MiB is the stated floor in release, a debug build needs 64 MiB
    let stack = if cfg!(debug_assertions) { 64 << 20 } else { 8 << 20 };
    let t = std::thread::Builder::new().stack_size(stack).spawn(move || exec(&name, scale)).unwrap();
    t.join().unwrap_or_else(|_| die("panic".into()));
}
