//! Load test for the style sheet loading of the page driver (`src/browser_loader/sheets.rs`) over a loopback HTTP server
//! inside this process. One case at one scale per process, on a virtual clock (nothing sleeps).
//!
//!   sheets_load CASE SCALE      CASE: linked | imports | dynamic      SCALE: 1, 10, 100
//!
//! linked: SCALE*8 `<link>` sheets (all different URLs) and as many `<p>`. A script reads `direction` of every `<p>` before it ends.
//! Sheet `i` sets `#e{i}` rtl if `i % 2 == 0`, and sheet `i` with `i % 4 == 1` sets `#e{i-1}` ltr (a later sheet wins).
//! imports: SCALE*3 sheets; each imports one shared sheet in a layer (fetched once) that sets class `x` rtl. A sheet with an
//! even number also sets its own `<p>` ltr, with no layer, so it beats the layer: `p` of odd `k` is rtl, of even `k` is ltr.
//! dynamic: SCALE*5 links, each added by a timer after the sheet before it was read. Each timer reads the last sheet's effect.
//! Every case compares the reads with the same model in Rust, and checks the server counts (one fetch for each URL). Prints
//! time, CPU and peak RSS. Only throwaway synthetic data. No external network.

#[path = "../src/test_server.rs"]
mod test_server;

use bare::browser::*;
use bare::browser_computed::ComputedLimits;
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
    let (n_link, n_imp, n_dyn) = (8 * scale, 3 * scale, 5 * scale);
    let read = "function dirs(prefix, n) { var s = ''; for (var i = 0; i < n; i++) s += getComputedStyle(document.getElementById(prefix + i)).direction === 'rtl' ? 'r' : 'l'; return s }; function one(id) { return getComputedStyle(document.getElementById(id)).direction === 'rtl' ? 'r' : 'l' }";
    let (html, want_urls, want, kind): (String, usize, String, u8) = match name {
        "linked" => {
            let mut h = String::from("<head>");
            for i in 0..n_link {
                h += &format!("<link rel=stylesheet href='/s{i}.css'>");
            }
            h += "</head><body>";
            for i in 0..n_link {
                h += &format!("<p id=e{i}></p>");
            }
            h += &format!("<script>{read}; var out = dirs('e', {n_link})</script>");
            let want: String = (0..n_link).map(|i| if i % 4 == 2 { 'r' } else { 'l' }).collect();
            (h, n_link, want, 0)
        }
        "imports" => {
            let mut h = String::from("<head>");
            for k in 0..n_imp {
                h += &format!("<link rel=stylesheet href='/i{k}.css'>");
            }
            h += "</head><body>";
            for k in 0..n_imp {
                h += &format!("<p class=x id=q{k}></p>");
            }
            h += &format!("<script>{read}; var out = dirs('q', {n_imp})</script>");
            let want: String = (0..n_imp).map(|k| if k % 2 == 1 { 'r' } else { 'l' }).collect();
            (h, n_imp + 1, want, 1)
        }
        "dynamic" => {
            let mut h = String::from("<body>");
            for k in 0..n_dyn {
                h += &format!("<p id=d{k}></p>");
            }
            h += &format!(
                "<script>{read}; var out = '', k = 0; function step() {{ if (k > 0) out += one('d' + (k - 1)); if (k >= {n_dyn}) return; var l = document.createElement('link'); l.rel = 'stylesheet'; l.setAttribute('href', '/d' + k + '.css'); document.head.appendChild(l); k++; setTimeout(step, 20) }} step()</script>"
            );
            // The last read is after the last link: one more step with no link. The first has nothing to read.
            let want: String = (0..n_dyn).map(|_| 'r').collect();
            (h, n_dyn, want, 2)
        }
        _ => die(format!("unknown case {name}")),
    };
    let server = serve(move |path| {
        let num = |p: &str| path.trim_start_matches(p).trim_end_matches(".css").parse::<usize>().ok();
        match kind {
            0 => match num("/s") {
                Some(i) => {
                    let mut t = String::new();
                    if i % 2 == 0 {
                        t += &format!("#e{i} {{ direction: rtl }}");
                    }
                    if i % 4 == 1 {
                        t += &format!("#e{} {{ direction: ltr }}", i - 1);
                    }
                    Reply::css(t)
                }
                None => Reply::new(404, None, ""),
            },
            1 if path == "/common.css" => Reply::css(".x { direction: rtl }"),
            1 => match num("/i") {
                Some(k) => Reply::css(format!("@import url(common.css) layer(c{}); {}", k % 4, if k % 2 == 0 { format!("#q{k} {{ direction: ltr }}") } else { String::new() })),
                None => Reply::new(404, None, ""),
            },
            _ => match num("/d") {
                Some(k) => Reply::css(format!("#d{k} {{ direction: rtl }}")),
                None => Reply::new(404, None, ""),
            },
        }
    });
    let eval = EvalLimits { max_steps: u64::MAX / 2, ..Default::default() };
    let mut b = Browser::new(tree::parse(&html), BrowserLimits::default(), eval, HeapLimits::default()).unwrap_or_else(|e| die(format!("browser: {e:?}")));
    b.use_virtual_clock();
    // The default caps of the computed module (256 sheets) are lower than the 800 sheets of the biggest case.
    b.set_computed_limits(ComputedLimits { max_external: 2048, max_sources: 2048, ..ComputedLimits::default() });
    let url = server.url("/page.html");
    let (cpu0, t) = (cpu_ms(), Instant::now());
    let limits = PageLimits {
        max_tasks: 10_000_000,
        max_rounds: 10_000_000,
        max_time: Duration::from_secs(40),
        quiet: Duration::from_millis(50),
        resources: bare::resources::Limits { max_fetches: 4096, max_references: 1 << 16, ..Default::default() },
        ..Default::default()
    };
    let r = b.run_page(PageConfig { url, client: Client::new(), limits, fetch: None });
    if r.stop != Stop::Settled || r.script_throws != 0 || r.css.failed != 0 {
        die(format!("{r:?}"));
    }
    let got = {
        let v = b.run_script("out").unwrap_or_else(|e| die(format!("read: {e:?}")));
        b.it.display(v)
    };
    if got != want {
        die(format!("reads differ: got {got:.80} want {want:.80}"));
    }
    if r.css.fetches != want_urls || r.css.loaded != want_urls || r.fetches != want_urls || server.total() != want_urls {
        die(format!("one fetch for each URL: wanted {want_urls}: css={:?} fetches={} server={}", r.css, r.fetches, server.total()));
    }
    let run_ms = t.elapsed().as_secs_f64() * 1e3;
    let c = b.computed_stats();
    println!(
        "{name:8} x{scale:<3} reads=ok run={run_ms:9.1}ms cpu={}ms peak_rss={:.1}MiB | urls={want_urls} css={:?} tasks={} rounds={} | computed: sheets={} bytes={} builds={} computes={}",
        cpu_ms() - cpu0,
        status_kib("VmHWM:") as f64 / 1024.0,
        r.css,
        r.tasks,
        r.rounds,
        c.external_sheets,
        c.external_bytes,
        c.builds,
        c.computes,
    );
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        die("usage: sheets_load CASE SCALE".into());
    }
    let (name, scale) = (args[1].clone(), args[2].parse::<usize>().unwrap_or_else(|_| die("bad scale".into())));
    // 8 MiB is the stated floor in release, a debug build needs 64 MiB
    let stack = if cfg!(debug_assertions) { 64 << 20 } else { 8 << 20 };
    let t = std::thread::Builder::new().stack_size(stack).spawn(move || exec(&name, scale)).unwrap();
    t.join().unwrap_or_else(|_| die("panic".into()));
}
