//! Cost rows of `Promise` (plan step P0/P1, `docs/design-js2-async.md` 8 and 9). One case for each process, so
//! `perf stat -e instructions:u` (or `tools/js/instr.sh`) counts it: the cost of one step is the difference of two scales divided by the
//! difference of the scales. The script runs, then the host drains the jobs.
//!   promise_load CASE N          CASE: then | then_hit | thenable | new | microtask | empty | mem | await_prim | await_prom | async_noawait
//!
//! - `then`: N times `p = p.then(f)` on a pending promise, then the root is resolved and the drain runs the N jobs. The step is the
//!   `then` call (derived promise and reaction), the settle, the job and the resolve of the derived promise.
//! - `then_hit`: as `then`, on one pending promise that has `then` as an own property: the read hits the cache of the site (the design
//!   of 9, P0, has no property read: its loop holds `then` in a register), and the N derived promises are not chained.
//! - `thenable`: N times `p = p.then(f)` where `f` returns a promise (a thenable job for each step).
//! - `new`: N times `new Promise(function (res) { res(i) })`, nothing queued.
//! - `microtask`: N times `queueMicrotask(f)`, then the drain.
//! - `get`: N reads of `p.then` (a property that the prototype gives: the interpreter looks it up each time). `loop`: the loop only.
//! - `empty`: the loop only, as the base of the others (`for` with a call of `f`).
//! - `await_prim`: one async function that does `s += await i` N times (a number: one job and no promise kept). `await_prom`: the same with `await p`,
//!   `p` a settled native promise. `async_noawait`: N calls of an async function that returns at once.
//! - `mem`: N pending promises in an array: prints the bytes of live heap that a promise costs (after a collection).
use bare_js2::script::load;

fn main() {
    let mut a = std::env::args().skip(1);
    let (case, n) = (a.next().expect("usage: promise_load CASE N"), a.next().and_then(|s| s.parse::<u32>().ok()).expect("N"));
    // Inside a function, so the variables are registers and the loop costs what a loop of a program costs.
    let body = match case.as_str() {
        "then" => "var rs; (function () { var p = new Promise(function (r) { rs = r; }); var f = function (x) { return x + 1; }; for (var i = 0; i < N; i++) p = p.then(f); })(); rs(0);",
        "then_hit" => "var rs; (function () { var p = new Promise(function (r) { rs = r; }); p.then = Promise.prototype.then; var f = function (x) { return x + 1; }; for (var i = 0; i < N; i++) p.then(f); })(); rs(0);",
        "thenable" => "var rs; (function () { var p = new Promise(function (r) { rs = r; }); var q = Promise.resolve(1); var f = function (x) { return q; }; for (var i = 0; i < N; i++) p = p.then(f); })(); rs(0);",
        "new" => "(function () { for (var i = 0; i < N; i++) new Promise(function (res) { res(i); }); })();",
        "microtask" => "(function () { var f = function () {}; for (var i = 0; i < N; i++) queueMicrotask(f); })();",
        "get" => "(function () { var p = new Promise(function () {}); var g; for (var i = 0; i < N; i++) g = p.then; })();",
        "loop" => "(function () { var p = new Promise(function () {}); var g; for (var i = 0; i < N; i++) g = p; })();",
        "empty" => "(function () { var f = function (x) { return x + 1; }; for (var i = 0; i < N; i++) f(i); })();",
        "await_prim" => "(async function () { var s = 0; for (var i = 0; i < N; i++) s += await i; })();",
        "await_prom" => "(async function () { var p = Promise.resolve(1); var s = 0; for (var i = 0; i < N; i++) s += await p; })();",
        "async_noawait" => "(function () { var f = async function (i) { return i; }; for (var i = 0; i < N; i++) f(i); })();",
        "mem" => "var keep = []; (function () { var f = function () {}; var rs; var root = new Promise(function (r) { rs = r; }); for (var i = 0; i < N; i++) keep.push(root.then(f)); })();",
        _ => panic!("CASE: then | then_hit | thenable | new | microtask | get | loop | empty | mem | await_prim | await_prom | async_noawait"),
    };
    let src = body.replace('N', &n.to_string());
    let mut vm = load(&src).expect("load");
    vm.run_main(0).expect("run");
    let jobs = vm.drain_microtasks(&mut |_, _| panic!("a job threw")).expect("drain");
    if case == "mem" {
        vm.collect_idle();
        println!("live bytes {} for {n} promises with a reaction and a derived promise", vm.realm.last_gc.live_bytes);
    } else {
        println!("jobs {jobs}");
    }
}
