//! Load test for `Promise.all`, `allSettled`, `race` and `any` on js2 (plan step P3): the same programs as `crates/js/examples/promise_all_load.rs`,
//! so `tools/js/instr.sh` gives the counts of both engines for the same work. One case at one scale for each process.
//!
//!   promise_all_load CASE SCALE    CASE: all | settled | race | any | anyerr | many | thenable    SCALE: 1, 10, 100
//!   also the cases of `crates/js/examples/promise_load.rs`: chain | fanout | pthenable | finally | reject | ctor | closures
//!
//! One scale step is 1000 items. The script runs, the host drains the jobs, and the sum that the script made is checked against the same
//! computation in Rust. A wrong sum exits with status 1.
use bare_vex::interp_rt::Realm;
use bare_vex::script::load;

fn die(msg: String) -> ! {
    eprintln!("FAIL: {msg}");
    std::process::exit(1)
}

/// (source, expected checksum): `out[0]` holds the checksum.
fn program(case: &str, n: usize) -> (String, f64) {
    let nf = n as f64;
    let arr = format!("var a = []; for (var i = 0; i < {n}; i++) a.push(i % 3 == 0 ? i : i % 3 == 1 ? Promise.resolve(i) : mk(i)); function mk(k) {{ return {{ then: function (ok) {{ ok(k); }} }}; }}");
    let tri = nf * (nf - 1.0) / 2.0;
    match case {
        "all" => (format!("var out = [0]; {arr} Promise.all(a).then(function (v) {{ for (var k = 0; k < v.length; k++) out[0] += v[k]; }});"), tri),
        "settled" => (format!("var out = [0]; {arr} Promise.allSettled(a).then(function (v) {{ for (var k = 0; k < v.length; k++) out[0] += v[k].value + (v[k].status == 'fulfilled' ? 0 : 1e9); }});"), tri),
        "race" => (format!("var out = [0]; {arr} Promise.race(a).then(function (v) {{ out[0] = v + 1; }});"), 1.0),
        "any" => (format!("var out = [0]; {arr} Promise.any(a).then(function (v) {{ out[0] = v + 1; }});"), 1.0),
        "anyerr" => (format!("var out = [0]; var a = []; for (var i = 0; i < {n}; i++) a.push(i % 2 ? Promise.reject(i) : {{ then: function (ok, no) {{ no(0); no(1); }} }}); Promise.any(a).catch(function (e) {{ out[0] = e.errors.length + (e instanceof AggregateError ? 1 : 0); }});"), nf + 1.0),
        "many" => (format!("var out = [0]; for (var j = 0; j < {n} / 10; j++) Promise.all([j, Promise.resolve(1), {{ then: function (ok) {{ ok(2); }} }}, Promise.reject(7).catch(function () {{ return 3; }}), 4, 5, 6, 7, 8, 9]).then(function (v) {{ out[0] += v[0] + v[9]; }});"), (0..n / 10).map(|j| (j + 9) as f64).sum()),
        "thenable" => (format!("var out = [0]; var a = []; for (var i = 0; i < {n}; i++) a.push({{ then: function (ok, no) {{ ok(1); ok(2); no(3); }} }}); Promise.all(a).then(function (v) {{ out[0] = v.length; }});"), nf),
        "chain" => (format!("var out = [0]; var p = Promise.resolve(0); for (var i = 0; i < {n}; i++) p = p.then(function (v) {{ return v + 1; }}); p.then(function (v) {{ out[0] = v; }});"), nf),
        "fanout" => (format!("var out = [0]; var res; var p = new Promise(function (r) {{ res = r; }}); for (var i = 0; i < {n}; i++) p.then(function (v) {{ out[0] += v; }}); res(2);"), 2.0 * nf),
        "pthenable" => (format!("var out = [0]; for (var i = 0; i < {n}; i++) Promise.resolve({{ then: function (a) {{ a(1); }} }}).then(function (v) {{ out[0] += v; }});"), nf),
        "finally" => (format!("var out = [0]; var p = Promise.resolve(1); for (var i = 0; i < {n}; i++) p = p.finally(function () {{ out[0]++; }}); p.then(function (v) {{ out[0] += v; }});"), nf + 1.0),
        "reject" => (format!("var out = [0]; for (var i = 0; i < {n}; i++) Promise.reject(1).then(function () {{ out[0] += 1000; }}).catch(function (e) {{ out[0] += e; }});"), nf),
        "ctor" => (format!("var out = [0]; for (var i = 0; i < {n}; i++) new Promise(function (r, j) {{ if (i % 2) r(i); else j(i); }}).then(function (v) {{ out[0] += v; }}, function (e) {{ out[0] -= e; }});"), (0..n).map(|i| if i % 2 == 1 { i as f64 } else { -(i as f64) }).sum()),
        "closures" => (format!("var out = [0]; for (var i = 0; i < {n}; i++) (function (k) {{ var big = {{ k: k }}; Promise.resolve().then(function () {{ out[0] += big.k; }}); }})(i);"), (0..n).map(|i| i as f64).sum()),
        _ => die(format!("unknown case {case}")),
    }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() != 3 {
        die("usage: promise_all_load CASE SCALE".into());
    }
    let scale: usize = a[2].parse().unwrap_or_else(|_| die("scale".into()));
    let (src, want) = program(&a[1], 1000 * scale);
    let mut vm = load(&src).unwrap_or_else(|e| die(format!("load {e}")));
    vm.run_main(0).unwrap_or_else(|e| die(format!("run {e:?}")));
    let jobs = vm.drain_microtasks(&mut |_, _| die("a job threw".into())).unwrap_or_else(|s| die(format!("drain {s:?}")));
    let out = Realm::obj(vm.global("out")).and_then(|o| o.get_element(0)).and_then(|v| v.as_f64()).unwrap_or_else(|| die("no sum".into()));
    if out != want {
        die(format!("{} x{scale}: checksum {out} != {want}", a[1]));
    }
    println!("{} x{scale} jobs {jobs}", a[1]);
}
