//! Native-to-script re-entry is bounded by Rust stack space, not by a count: deep finite recursion works, a runaway one throws a
//! catchable `RangeError` with the message of the register-stack limit.
use super::tests::check;

const MSG: &str = "maximum call stack size exceeded";

/// A function `f(n)` that re-enters script through `via` at each level. `via` uses `n` and `f`, and sets `r`.
fn finite(via: &str, depth: u32) -> String {
    format!("function f(n) {{ if (n == 0) return 0; var r; {via} return r; }} var r = f({depth}) === {depth};")
}

fn runaway(via: &str) -> String {
    format!(
        "var hit; function f(n) {{ try {{ var r; {via} return r; }} catch (e) {{ if (!hit) hit = e; throw e; }} }} try {{ f(0); }} catch (e) {{}} \
         var r = hit instanceof RangeError && hit.message === '{MSG}';"
    )
}

const PROMISE: &str = "new Promise(function (res) { r = f(n - 1) + 1; res(); });";
const REVIVER: &str = "JSON.parse('[1]', function (k, v) { if (k === '0') r = f(n - 1) + 1; return v; });";
const PROXY: &str = "r = new Proxy({}, { get() { return f(n - 1) + 1; } }).x;";
const ITER: &str = "for (var x of { [Symbol.iterator]() { return { next() { r = f(n - 1) + 1; return { done: true }; } }; } });";
const YIELD_STAR: &str = "function* g() { r = f(n - 1) + 1; yield 1; } for (var x of (function* () { yield* g(); })());";

fn run_in_big_thread(src: String) {
    std::thread::Builder::new().stack_size(2 << 20).spawn(move || check(&[(&src, "r", "true")])).unwrap().join().unwrap();
}

#[test]
fn two_thousand_levels_through_a_promise_executor() {
    run_in_big_thread(finite(PROMISE, 2000));
}

#[test]
fn two_thousand_levels_through_a_json_reviver() {
    run_in_big_thread(finite(REVIVER, 2000));
}

#[test]
fn two_thousand_levels_through_proxy_get_traps() {
    run_in_big_thread(finite(PROXY, 2000));
}

#[test]
fn two_thousand_levels_through_a_custom_iterator() {
    run_in_big_thread(finite(ITER, 2000));
}

#[test]
fn deep_recursion_through_yield_star() {
    // Two generators per level, about 440 KB of a debug build: it has room for 868 levels in its budget.
    run_in_big_thread(finite(YIELD_STAR, if cfg!(debug_assertions) { 500 } else { 2000 }));
}

#[test]
fn runaway_promise_executor_ends_with_a_range_error() {
    // The Promise constructor swallows the error of its executor: the promise is rejected with it, and a job reads the reason.
    let src = "var hit, d = 0; function f() { d++; var p = new Promise(function (res) { f(); res(); }); p.catch(function (e) { if (!hit) hit = e; }); } f(); \
               var r = ''; Promise.resolve().then(function () { r = (d > 2000) + ':' + (hit instanceof RangeError) + ':' + hit.message; });";
    std::thread::Builder::new()
        .stack_size(2 << 20)
        .spawn(move || {
            let mut vm = super::run(src).unwrap();
            vm.drain_microtasks(&mut |_, _| {}).unwrap();
            assert_eq!(vm.global_text("r"), format!("true:true:{MSG}"));
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn runaway_json_reviver_ends_with_a_range_error() {
    run_in_big_thread(runaway(&REVIVER.replace("n - 1", "n + 1")));
}

#[test]
fn runaway_proxy_get_trap_ends_with_a_range_error() {
    run_in_big_thread(runaway(&PROXY.replace("n - 1", "n + 1")));
}

#[test]
fn runaway_custom_iterator_ends_with_a_range_error() {
    run_in_big_thread(runaway(&ITER.replace("n - 1", "n + 1")));
}

#[test]
fn runaway_yield_star_ends_with_a_range_error() {
    run_in_big_thread(runaway(&YIELD_STAR.replace("n - 1", "n + 1")));
}
