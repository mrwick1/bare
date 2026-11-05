//! Load for the numeric builtins: one fresh process runs one kind at N iterations and prints time and peak RSS.
//!
//!   numeric_load KIND N EXPECTED_CHECKSUM        KIND: empty | random | clz32 | log | radix16 | radix36frac | radix2sub
//!
//! The first call (seed read from the OS for `random`) is timed apart from the hot loop. Run it under `ulimit -v`
//! (1 GiB) and `timeout` (45 s).

use bare_js::eval::*;
use bare_js::heap::Heap;
use bare_js::parse::{parse_script, Limits};
use std::time::Instant;

fn rss_kib() -> u64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    s.lines().find_map(|l| l.strip_prefix("VmHWM:")).and_then(|v| v.trim().trim_end_matches(" kB").trim().parse().ok()).unwrap_or(0)
}

fn run(src: &str) -> (String, f64) {
    let prog = parse_script(src, Limits::default()).expect("parse");
    let mut it = Interp::new(&prog, Heap::default(), EvalLimits { max_steps: u64::MAX / 4, ..Default::default() }).expect("setup");
    let t = Instant::now();
    let r = match it.run() {
        Ok(v) => it.display(v),
        Err(e) => format!("error:{e:?}"),
    };
    (r, t.elapsed().as_secs_f64())
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (kind, n) = (a[1].clone(), a[2].parse::<u64>().unwrap());
    let expected = a.get(3).cloned();
    let body = match kind.as_str() {
        "parseint" => "parseInt(i.toString(16),16)",
        "uricomponent" => "encodeURIComponent('é /?#').length",
        "empty" => "0",
        "random" => "Math.random()",
        "clz32" => "Math.clz32(i)",
        "log" => "Math.log(i + 1)",
        "radix16" => "(i * 2654435761).toString(16).length",
        "radix36frac" => "(i / 7 + 0.123).toString(36).length",
        "radix2sub" => "(5e-324 * (i + 1)).toString(2).length",
        _ => panic!("kind"),
    };
    let child = std::thread::Builder::new().stack_size(if cfg!(debug_assertions) { 64 << 20 } else { 8 << 20 }).spawn(move || {
        let (_, first) = run(&format!("(function(){{ var i = 0; return {body}; }})()"));
        let (r, hot) = run(&format!("var acc = 0; for (var i = 0; i < {n}; i++) {{ var item = {body}; if (!(item >= 0) || ({random} && !(item < 1))) throw 1; acc += item; }} acc", random = kind == "random"));
        let actual: f64 = r.parse().expect("the engine must return a numeric checksum");
        if kind == "random" {
            assert!(actual >= 0.0 && actual < n as f64);
        } else {
            let expected: f64 = expected.expect("an independent checksum is required").parse().unwrap();
            if kind == "log" {
                assert!((actual - expected).abs() <= 1e-12 * expected.abs().max(1.0));
            } else {
                assert_eq!(actual, expected);
            }
        }
        let (_, base) = run(&format!("var acc = 0; for (var i = 0; i < {n}; i++) {{ acc += 0; }} acc"));
        println!("{kind} n={n} first_script_s={first:.6} hot_s={hot:.4} loop_only_s={base:.4} net_ns_per_iter={:.0} checksum=ok peak_rss_kib={}", (hot - base).max(0.0) / n as f64 * 1e9, rss_kib());
    }).unwrap();
    child.join().unwrap();
}
