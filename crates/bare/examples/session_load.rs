//! Load test for the document session over a loopback HTTP server inside this process. One case at one scale per
//! process, on a virtual clock.
//!
//!   session_load CASE SCALE      CASE: chain | heavy      SCALE: 1, 10, 100 (documents)
//!
//! chain: SCALE documents. Document i counts itself in `localStorage`, queues a timer, a `fetch` and a `load` handler
//! (none may run: the navigation ends the document first) and calls `location.assign` to document i+1.
//! heavy: the same, and each document also builds about 4000 short lived objects and strings first.
//! The check: the counter is SCALE, each document was asked for exactly once, no old callback ran, the last document
//! settled, and the old callbacks' marks are absent. Prints time, CPU, peak RSS and the heap of the last realm.
//! `tools/js/session_load.sh` runs the matrix under caps. Only throwaway synthetic data. No external network.

#[path = "../src/test_server.rs"]
mod test_server;

use bare::Stop;
use bare::browser_session::{Session, SessionConfig, SessionStop};
use std::time::Instant;
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

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (case, scale) = (args.get(1).cloned().unwrap_or_default(), args.get(2).and_then(|s| s.parse::<usize>().ok()).unwrap_or(0));
    if !matches!(case.as_str(), "chain" | "heavy") || scale == 0 {
        die("usage: session_load chain|heavy SCALE".into());
    }
    std::thread::Builder::new().stack_size(8 << 20).spawn(move || run(&case, scale)).unwrap().join().unwrap();
}

fn run(case: &str, scale: usize) {
    let heavy = case == "heavy";
    let server = serve(move |path| {
        let Some(i) = path.strip_prefix("/d").and_then(|n| n.parse::<usize>().ok()) else { return Reply::new(404, None, "") };
        let work = if heavy { "var junk = []; for (var j = 0; j < 4000; j++) junk.push({ j: j, s: 'x' + j }); " } else { "" };
        let next = if i + 1 < scale { format!("location.assign('/d{}')", i + 1) } else { String::new() };
        // The last document has no navigation, so it registers no callback: any mark comes from an old document.
        let old = if i + 1 < scale {
            "setTimeout(function () { localStorage.setItem('old-timer', '1') }, 0);
             fetch('/never').then(function () { localStorage.setItem('old-fetch', '1') });
             addEventListener('load', function () { localStorage.setItem('old-load', '1') });"
        } else {
            ""
        };
        Reply::new(200, Some("text/html; charset=utf-8"), format!("<script>{work}localStorage.setItem('n', String(+(localStorage.getItem('n') || 0) + 1)); {old} {next}</script>"))
    });
    let mut cfg = SessionConfig::new(&server.url("/d0"));
    cfg.virtual_clock = true;
    cfg.limits.max_documents = scale + 1;
    cfg.limits.max_time = std::time::Duration::from_secs(40);
    cfg.eval.max_steps = u64::MAX / 2;
    let (cpu0, t) = (cpu_ms(), Instant::now());
    let mut s = Session::new(cfg);
    let r = s.run();
    let (wall, cpu) = (t.elapsed(), cpu_ms() - cpu0);
    let origin = server.url("");
    let get = |k: &str| s.storage().get_item(false, &origin, k);
    if r.stop != SessionStop::Page(Stop::Settled) || r.pages.len() != scale {
        die(format!("{:?} documents {}", r.stop, r.pages.len()));
    }
    if get("n").as_deref() != Some(&scale.to_string()) || ["old-timer", "old-fetch", "old-load"].iter().any(|k| get(k).is_some()) {
        die(format!("counter {:?}, old callback marks {:?}", get("n"), ["old-timer", "old-fetch", "old-load"].map(get)));
    }
    if server.hits("/never") != 0 || (0..scale).any(|i| server.hits(&format!("/d{i}")) != 1) {
        die("a document was not requested exactly once, or an old fetch started".into());
    }
    let heap = s.browser().map_or(0, |b| b.stats().wrappers);
    println!(
        "session_load {case} x{scale}: docs {} tasks {} rounds {} wall {:.0} ms cpu {cpu} ms peak {} MiB now {} MiB wrappers(last) {heap} OK",
        r.pages.len(),
        r.tasks,
        r.rounds,
        wall.as_secs_f64() * 1000.0,
        status_kib("VmHWM:") / 1024,
        status_kib("VmRSS:") / 1024
    );
}
