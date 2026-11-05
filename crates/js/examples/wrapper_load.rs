//! Load for primitive wrapper objects: one fresh process runs one kind at N iterations and prints time and peak RSS.
//!
//!   wrapper_load KIND N EXPECTED_CHECKSUM        KIND: empty | primcall | newstring | newnumber | newbool | objectprim | strindex
//!   | strkeys | wrapcall | looseeq | json | forin
//!
//! `primcall` is the control: a method call on a primitive, which must allocate no wrapper. The others make one wrapper in each
//! iteration. Run it under `ulimit -v` (1 GiB) and `timeout` (45 s).

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
        "empty" => "0",
        "primcall" => "(i).toFixed(0).length + 'abc'.length",
        "newstring" => "new String('x' + i).length",
        "newnumber" => "new Number(i).valueOf()",
        "newbool" => "(new Boolean(i % 2).valueOf() ? 1 : 0)",
        "objectprim" => "(typeof Object(i) === 'object' ? 1 : 0)",
        "strindex" => "new String('abcdef')[i % 6].length",
        "strkeys" => "Object.keys(new String('abcdef')).length",
        "wrapcall" => "new Number(i).toFixed(1).length",
        "looseeq" => "(new Number(i) == i ? 1 : 0)",
        "json" => "JSON.stringify(new String('a')).length",
        "forin" => "(function () { var n = 0; for (var k in new String('abc')) n++; return n; })()",
        _ => panic!("kind"),
    };
    let child = std::thread::Builder::new().stack_size(if cfg!(debug_assertions) { 64 << 20 } else { 8 << 20 }).spawn(move || {
        let (_, first) = run(&format!("(function(){{ var i = 0; return {body}; }})()"));
        let (r, hot) = run(&format!("var acc = 0; for (var i = 0; i < {n}; i++) {{ var item = {body}; if (!(item >= 0)) throw 1; acc += item; }} acc"));
        let actual: f64 = r.parse().expect("the engine must return a numeric checksum");
        let expected: f64 = expected.expect("an independent checksum is required").parse().unwrap();
        assert_eq!(actual, expected);
        let (_, base) = run(&format!("var acc = 0; for (var i = 0; i < {n}; i++) {{ acc += 0; }} acc"));
        println!("{kind} n={n} first_script_s={first:.6} hot_s={hot:.4} loop_only_s={base:.4} net_ns_per_iter={:.0} checksum=ok peak_rss_kib={}", (hot - base).max(0.0) / n as f64 * 1e9, rss_kib());
    }).unwrap();
    child.join().unwrap();
}
