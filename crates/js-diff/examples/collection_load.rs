//! The cost rows of `Map`, `Set`, `WeakMap` and `WeakSet` on the old engine (`bare-js`) and on the new one (`bare-js2`): the same program on
//! each, with the result checked between the two. A row that one engine cannot run is reported, not left out.
//!   collection_load ENGINE WORK N      ENGINE: old | new      WORK: map | mapstr | set | setdel | weak | iter | build
//! `map`: N `set` calls of integer keys, then N `get` calls. `mapstr`: the same with string keys (`'k' + i`). `set`: N `add` calls, N
//! `has` calls that hit and N that miss. `setdel`: N `add` calls then `delete` of every key. `weak`: a `WeakMap` of N object keys, set then
//! get. `iter`: a `forEach` over a `Map` of N entries. `build`: N small `Set` objects of 3 members (the cost of making one). Count
//! with `tools/js/instr.sh`, for N and for 0, and divide the difference by N. The line it prints has the result of the program.
use bare_js_diff::{Engine, Out, run};

fn program(work: &str, n: u64) -> Option<String> {
    let body = match work {
        "map" => "var m = new Map(); for (var i = 0; i < N; i++) m.set(i, i * 2); var t = 0; for (var i = 0; i < N; i++) t += m.get(i); var result = t + ',' + m.size;",
        "mapstr" => "var m = new Map(); for (var i = 0; i < N; i++) m.set('k' + i, i); var t = 0; for (var i = 0; i < N; i++) t += m.get('k' + i); var result = t + ',' + m.size;",
        "set" => "var s = new Set(); for (var i = 0; i < N; i++) s.add(i * 3); var h = 0; for (var i = 0; i < N; i++) { if (s.has(i * 3)) h++; if (s.has(i * 3 + 1)) h += 1000; } var result = h + ',' + s.size;",
        "setdel" => "var s = new Set(); for (var i = 0; i < N; i++) s.add(i); for (var i = 0; i < N; i++) s.delete(i); var result = s.size;",
        "weak" => "var wm = new WeakMap(), ks = []; for (var i = 0; i < N; i++) { ks[i] = {}; wm.set(ks[i], i); } var t = 0; for (var i = 0; i < N; i++) t += wm.get(ks[i]); var result = t;",
        "iter" => "var m = new Map(); for (var i = 0; i < N; i++) m.set(i, i); var t = 0; m.forEach(function (v, k) { t += v + k; }); var result = t;",
        "build" => "var last; for (var i = 0; i < N; i++) { var s = new Set(); s.add(i); s.add(i + 1); s.add(i); last = s; } var result = last.size;",
        _ => return None,
    };
    Some(format!("{}\nvar __name = 'result';", body.replace('N', &n.to_string())))
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let (Some(engine), Some(work), Some(n)) = (a.first().and_then(|e| Engine::from_name(e)), a.get(1), a.get(2).and_then(|n| n.parse::<u64>().ok())) else {
        eprintln!("usage: collection_load old|new map|mapstr|set|setdel|weak|iter|build N");
        std::process::exit(2);
    };
    let Some(src) = program(work, n) else {
        eprintln!("unknown work {work}");
        std::process::exit(2);
    };
    // Deep recursion in the engines: the stack of the other runners.
    let r = std::thread::Builder::new().stack_size(256 << 20).spawn(move || run(engine, &src, u64::MAX / 4)).unwrap().join().unwrap();
    match r {
        Out::Done(text) => println!("{work} {n}: {text}"),
        other => {
            println!("FAIL {other:?}");
            std::process::exit(1);
        }
    }
}
