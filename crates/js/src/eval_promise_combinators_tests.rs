//! Tests of `Promise.all`, `allSettled`, `race`, `any` and `AggregateError`. The corpus
//! (`tools/fixtures/js/promise-combinator-cases.txt`) is checked against what Node gave (`.expected`, made by
//! `tools/js/promise_oracle.mjs`). The tests below cover what Node cannot show: gates, caps, charged work, the
//! checkpoint contract, roots, GC of the records, and scripts with colliding node ids.

use super::*;
use crate::heap::Heap;
use crate::parse::{parse_script, Limits};

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\n', "\\n").replace('\r', "\\r").replace('\t', "\\t")
}

fn fail(it: &mut Interp, e: Error) -> String {
    match e {
        Error::Throw(v) if matches!(v, Value::Object(_)) => format!("throw:{}:{}", esc(&it.get_str(v, "name")), esc(&it.get_str(v, "message"))),
        Error::Throw(v) => format!("throw-value:{}", esc(&it.display(v))),
        Error::Unsupported(u) => format!("unsupported:{}", u.what),
        Error::Limit(l) => format!("limit:{l:?}"),
        e => format!("internal-error:{e:?}"),
    }
}

fn run_case(src: &str) -> String {
    let prog = parse_script(src, Limits::default()).unwrap_or_else(|e| panic!("{src:?}: {e}"));
    let mut it = Interp::new(&prog, Heap::default(), EvalLimits { max_steps: 5_000_000, ..Default::default() }).expect("setup");
    let reader = match it.run() {
        Ok(v) => v,
        Err(e) => return fail(&mut it, e),
    };
    let read = |it: &mut Interp| match it.call(reader, Value::Undefined, &[]) {
        Ok(v) => Ok(esc(&it.display(v))),
        Err(e) => Err(fail(it, e)),
    };
    let pre = match read(&mut it) {
        Ok(s) => s,
        Err(s) => return s,
    };
    if let Err(e) = it.drain_microtasks() {
        return fail(&mut it, e);
    }
    match read(&mut it) {
        Ok(post) => format!("{pre} ## {post}"),
        Err(s) => s,
    }
}

#[test]
fn corpus_matches_node() {
    let (cases, expected) = (include_str!("../../../tools/fixtures/js/promise-combinator-cases.txt"), include_str!("../../../tools/fixtures/js/promise-combinator-cases.expected"));
    big(move || {
        let mut list: Vec<(&str, Vec<&str>)> = Vec::new();
        for line in cases.lines() {
            if let Some(n) = line.strip_prefix("=== ") {
                list.push((n.trim(), Vec::new()));
            } else if let Some(c) = list.last_mut() {
                c.1.push(line);
            }
        }
        let want: Vec<&str> = expected.lines().collect();
        assert_eq!(list.len(), want.len());
        assert!(list.len() >= 45);
        let mut bad = Vec::new();
        for ((name, lines), w) in list.iter().zip(want) {
            let got = format!("{name}\t{}", run_case(lines.join("\n").trim_end_matches('\n')));
            if got != w {
                bad.push(format!("node: {w}\nbare: {got}"));
            }
        }
        assert!(bad.is_empty(), "{} differences:\n{}", bad.len(), bad.join("\n"));
    });
}

// ------------------------------------------------------------------ helpers

fn realm(limits: EvalLimits) -> Interp<'static> {
    Interp::realm(Heap::default(), limits).expect("realm")
}

fn out(it: &mut Interp, src: &str) -> String {
    match it.run_script(src) {
        Ok(v) => it.display(v),
        Err(e) => fail(it, e),
    }
}

fn lim() -> EvalLimits {
    EvalLimits { max_steps: 2_000_000, ..Default::default() }
}

/// Run a script and drain, then give the text of `expr`.
fn settle(it: &mut Interp, src: &str, expr: &str) -> String {
    let r = out(it, src);
    assert!(!r.starts_with("limit") && !r.starts_with("throw") && !r.starts_with("unsupported"), "{src}: {r}");
    it.drain_microtasks().unwrap();
    out(it, expr)
}

// ------------------------------------------------------------------ gates

#[test]
fn another_this_is_an_explicit_gate_and_a_primitive_this_is_a_type_error() {
    big(|| {
        for f in ["all", "allSettled", "race", "any"] {
            let mut it = realm(lim());
            assert_eq!(out(&mut it, &format!("Promise.{f}.call(function () {{}}, [])")), "unsupported:Promise static method on a constructor other than Promise", "{f}");
            assert_eq!(out(&mut it, &format!("Promise.{f}.call(1, [])")), "throw:TypeError:PromiseResolve called on non-object", "{f}");
            assert_eq!(it.pending_jobs(), 0);
            assert_eq!(it.agg_stats(), AggStats::default());
        }
        let mut it = realm(lim());
        assert!(out(&mut it, "class E extends AggregateError {}; new E([])").starts_with("unsupported:subclassing AggregateError"));
    });
}

// ------------------------------------------------------------------ caps

#[test]
fn caps_are_checked_before_growth_and_end_the_call_with_a_distinct_limit() {
    big(|| {
        // slots of all live records together
        let mut it = realm(lim());
        it.set_agg_limits(AggLimits { max_items: 5, ..Default::default() });
        out(&mut it, "var closed = 0; var never = new Promise(function () {}); var iter = { [Symbol.iterator]: function () { return { next: function () { return { done: false, value: never }; }, return: function () { closed++; return {}; } }; } };");
        assert_eq!(out(&mut it, "Promise.all(iter)"), "limit:Agg(Items)");
        assert_eq!(it.agg_stats().items, 5);
        assert_eq!(out(&mut it, "closed"), "0", "no JavaScript runs after a limit");
        // the call is dead for good, but the interpreter is not: once nothing names the slots, a collect frees them
        out(&mut it, "never = iter = undefined;");
        it.collect(&[]).unwrap();
        assert_eq!(it.agg_stats(), AggStats::default());
        assert_eq!(out(&mut it, "var n2 = new Promise(function () {}); Promise.all([n2, n2]).then(function () {}); 1"), "1");
        assert_eq!(it.agg_stats().items, 2);
        // items of one call (also for race, which has no record)
        for f in ["all", "allSettled", "race", "any"] {
            let mut it = realm(lim());
            it.set_agg_limits(AggLimits { max_items_per_call: 7, ..Default::default() });
            out(&mut it, "var closed = 0; var n = 0; var iter = { [Symbol.iterator]: function () { return { next: function () { n++; return { done: false, value: 1 }; }, return: function () { closed++; return {}; } }; } };");
            assert_eq!(out(&mut it, &format!("Promise.{f}(iter)")), "limit:Agg(ItemsPerCall)", "{f}");
            assert_eq!(out(&mut it, "closed"), "0", "{f}");
            assert_eq!(out(&mut it, "n"), "8", "{f}: next ran once past the cap, nothing grew");
            assert!(it.agg_stats().items <= 7, "{f}");
        }
        // live records
        let mut it = realm(lim());
        it.set_agg_limits(AggLimits { max_records: 3, ..Default::default() });
        assert_eq!(out(&mut it, "var p = new Promise(function () {}); Promise.all([p]); Promise.allSettled([p]); Promise.any([p]); 1"), "1");
        assert_eq!(it.agg_stats().records, 3);
        assert_eq!(out(&mut it, "Promise.all([p])"), "limit:Agg(Records)");
        assert_eq!(it.agg_stats().records, 3);
        // race needs no record
        assert_eq!(out(&mut it, "Promise.race([p]); 2"), "2");
        // a record that nothing names is freed by collect, and the cap is free again
        out(&mut it, "p = undefined;");
        it.collect(&[]).unwrap();
        assert_eq!(it.agg_stats().records, 0);
        assert_eq!(out(&mut it, "Promise.all([p]); 3"), "3");
    });
}

#[test]
fn the_other_caps_still_apply_to_combinators() {
    big(|| {
        // function objects: two element functions per item for allSettled
        let mut it = realm(EvalLimits { max_functions: 400, ..lim() });
        let r = out(&mut it, "var a = []; for (var i = 0; i < 1000; i++) a.push(i); Promise.allSettled(a)");
        assert_eq!(r, "limit:Functions");
        // promises: `PromiseResolve` makes one per non-promise item
        let mut it = realm(lim());
        it.set_promise_limits(PromiseLimits { max_promises: 50, ..Default::default() });
        assert_eq!(out(&mut it, "var a = []; for (var i = 0; i < 1000; i++) a.push(i); Promise.race(a)"), "limit:Promises");
        // steps are charged per item
        let mut it = realm(lim());
        out(&mut it, "var a = []; for (var i = 0; i < 1000; i++) a.push(i);");
        let s0 = it.stats().steps;
        out(&mut it, "Promise.all(a)");
        let per_item = (it.stats().steps - s0) / 1000;
        assert!(per_item >= 4, "per item {per_item}");
        it.limits.max_steps = 1000 * 4;
        assert_eq!(out(&mut it, "Promise.all(a)"), "limit:Steps");
        assert_eq!(out(&mut it, "Promise.allSettled(a)"), "limit:Steps");
        // heap bytes
        let mut it = Interp::realm(Heap::new(crate::heap::HeapLimits { max_objects: 600, ..Default::default() }).unwrap(), lim()).unwrap();
        let r = out(&mut it, "var a = []; for (var i = 0; i < 100; i++) a.push(i); Promise.allSettled(a); Promise.allSettled(a); Promise.allSettled(a); 1");
        assert!(r.starts_with("limit:Heap"), "{r}");
    });
}

// ------------------------------------------------------------------ the checkpoint contract

#[test]
fn a_limit_in_an_element_job_is_sticky_and_no_javascript_runs_after_it() {
    big(|| {
        // steps: the element jobs run out of budget
        let mut it = realm(lim());
        out(&mut it, "var ran = 0; var ps = []; for (var i = 0; i < 40; i++) ps.push(Promise.resolve(i)); Promise.all(ps).then(function () { ran++; }); Promise.resolve().then(function () { ran += 100; });");
        it.limits.max_steps = 60;
        let first = it.drain_microtasks();
        let queued = it.pending_jobs();
        assert!(matches!(first, Err(Error::Limit(LimitKind::Steps))), "{first:?}");
        for _ in 0..3 {
            assert!(matches!(it.drain_microtasks(), Err(Error::Limit(LimitKind::Steps))));
            assert_eq!(it.pending_jobs(), queued, "a failed checkpoint runs nothing");
        }
        assert_eq!(out(&mut it, "ran"), "0");
        // queue room: settling the result promise needs room for its reactions
        let mut it = realm(lim());
        it.set_promise_limits(PromiseLimits { max_queue: 3, ..Default::default() });
        out(&mut it, "var ran = 0; var a = new Promise(function () {}); var res; var p = new Promise(function (r) { res = r; }); var all = Promise.all([p]);
                      for (var i = 0; i < 4; i++) all.then(function () { ran++; }); res(1);");
        let first = it.drain_microtasks();
        assert!(matches!(first, Err(Error::Limit(LimitKind::PromiseQueue))), "{first:?}");
        assert!(matches!(it.drain_microtasks(), Err(Error::Limit(LimitKind::PromiseQueue))));
        assert_eq!(out(&mut it, "ran"), "0");
    });
}

#[test]
fn a_limit_or_unsupported_in_resolve_or_next_closes_nothing_and_rejects_nothing() {
    big(|| {
        for f in ["all", "allSettled", "race", "any"] {
            // `resolve` runs into the step budget
            let mut it = realm(EvalLimits { max_steps: 20_000, ..lim() });
            out(&mut it, "var closed = 0, handled = 0; var iter = { [Symbol.iterator]: function () { return { next: function () { return { done: false, value: 1 }; }, return: function () { closed++; return {}; } }; } };
                          var orig = Promise.resolve; Promise.resolve = function () { while (true) {} };");
            let r = out(&mut it, &format!("var p = Promise.{f}(iter); p.catch(function () {{ handled++; }}); 1"));
            assert_eq!(r, "limit:Steps", "{f}");
            assert_eq!(out(&mut it, "closed + ',' + handled"), "0,0", "{f}");
            assert!(matches!(it.drain_microtasks(), Ok(0)), "{f}: nothing was queued");
            // `next` of the iterator reaches a gate
            let mut it = realm(lim());
            out(&mut it, "var closed = 0, handled = 0; var iter = { [Symbol.iterator]: function () { return { next: function () { return Promise.resolve.call(function () {}, 1); }, return: function () { closed++; return {}; } }; } };");
            let r = out(&mut it, &format!("Promise.{f}(iter).catch(function () {{ handled++; }}); 1"));
            assert_eq!(r, "unsupported:Promise static method on a constructor other than Promise", "{f}");
            assert_eq!(out(&mut it, "closed + ',' + handled"), "0,0", "{f}");
        }
    });
}

// ------------------------------------------------------------------ collect

#[test]
fn collect_while_combinators_are_suspended_keeps_every_value_and_result_alive() {
    big(|| {
        let mut it = realm(lim());
        out(&mut it, "var keep = []; var res = [];
            var a = new Promise(function (r) { keep.push(r); }), b = new Promise(function (r, j) { keep.push(r); keep.push(j); }), c = new Promise(function (r, j) { keep.push(j); });
            Promise.resolve(1).then(function () { return 0; });
            Promise.all([a, Promise.resolve({ k: 'x' }), b]).then(function (v) { res.push('all:' + v[0] + v[1].k + v[2]); });
            Promise.allSettled([a, Promise.reject({ k: 'y' }), c]).then(function (v) { res.push('as:' + v[0].value + v[1].reason.k + v[2].reason); });
            Promise.race([a, c]).then(function (v) { res.push('race:' + v); });
            Promise.any([c, Promise.reject({ k: 'z' }), a]).then(function (v) { res.push('any:' + v); });
            Promise.any([c, Promise.reject({ k: 'z' })]).catch(function (e) { res.push('anyerr:' + e.errors[0] + e.errors[1].k); });
            0");
        // the jobs for the settled inputs are still queued: collect with them suspended, then again after they ran
        assert!(it.pending_jobs() > 0);
        let n1 = it.collect(&[]).unwrap();
        it.drain_microtasks().unwrap();
        let n2 = it.collect(&[]).unwrap();
        let _ = (n1, n2);
        assert_eq!(out(&mut it, "res.length"), "0");
        let live = it.agg_stats();
        assert_eq!(live.records, 4, "four live records; race has none");
        out(&mut it, "keep[0]('A'); keep[1]('B'); keep[3]('C');");
        it.collect(&[]).unwrap();
        it.drain_microtasks().unwrap();
        assert_eq!(out(&mut it, "res.join()"), "race:A,any:A,all:AxB,as:AyC,anyerr:Cz");
        it.collect(&[]).unwrap();
        assert_eq!(it.agg_stats().records, 0, "every record is done and unnamed");
    });
}

#[test]
fn unreachable_cycles_through_records_inputs_and_results_are_pruned() {
    big(|| {
        let mut it = realm(lim());
        out(&mut it, "var p = new Promise(function () {}); var q = Promise.all([p, 1]); p.cycle = q; q.cycle = p;
                      var r = Promise.allSettled([q, p]); r.me = [r, q]; var s = Promise.any([r]); s.me = s;");
        let busy = it.stats().functions;
        let objs = it.heap.stats().live_objects;
        assert_eq!(it.agg_stats().records, 3);
        it.drain_microtasks().unwrap();
        out(&mut it, "p = q = r = s = undefined;");
        it.collect(&[]).unwrap();
        assert_eq!(it.agg_stats(), AggStats::default());
        assert!(it.stats().functions < busy);
        assert!(it.heap.stats().live_objects < objs);
        assert!(it.promise.map.len() <= 1, "no promise of the cycle is left: {}", it.promise.map.len());
        // the same loop many times: nothing accumulates
        for _ in 0..5 {
            out(&mut it, "for (var i = 0; i < 50; i++) { var x = new Promise(function () {}); x.all = Promise.all([x, x]); x.race = Promise.race([x]); x.settled = Promise.allSettled([x]); }");
            it.collect(&[]).unwrap();
        }
        out(&mut it, "x = undefined;");
        it.collect(&[]).unwrap();
        assert_eq!(it.agg_stats(), AggStats::default());
        assert!(it.stats().functions < busy);
    });
}

#[test]
fn a_kept_element_function_keeps_its_record_and_cannot_touch_a_later_one() {
    big(|| {
        let mut it = realm(lim());
        out(&mut it, "var saved = []; var fake = Promise.resolve(0); fake.then = function (ok, err) { saved.push(ok, err); }; var orig = Promise.resolve; Promise.resolve = function () { return fake; };
                      var res = []; Promise.all([1]).then(function (v) { res.push('first:' + v.length + v[0]); }, function (e) { res.push('rej' + e); }); Promise.resolve = orig;");
        it.collect(&[]).unwrap();
        assert_eq!(it.agg_stats().records, 1);
        out(&mut it, "saved[0]('v');");
        it.drain_microtasks().unwrap();
        // finished, but the saved function still names it, so its once flag survives collect
        it.collect(&[]).unwrap();
        assert_eq!(it.agg_stats().records, 1);
        out(&mut it, "saved[0]('again'); saved[1]('late');");
        it.drain_microtasks().unwrap();
        assert_eq!(out(&mut it, "res.join()"), "first:1v");
        // a new record never reuses the key of the old one
        out(&mut it, "var hold = []; var fake2 = Promise.resolve(0); fake2.then = function (ok, err) { hold.push(ok, err); }; Promise.resolve = function () { return fake2; };
                      Promise.all([1, 2]).then(function (v) { res.push('second:' + v.join()); }); Promise.resolve = orig;");
        out(&mut it, "saved[0]('X'); saved[1]('Y'); hold[0]('p'); hold[2]('q');");
        it.drain_microtasks().unwrap();
        assert_eq!(out(&mut it, "res.join()"), "first:1v,second:p,q");
        out(&mut it, "saved = undefined;");
        it.collect(&[]).unwrap();
        out(&mut it, "hold = undefined; fake2 = undefined;");
        it.collect(&[]).unwrap();
        assert_eq!(it.agg_stats(), AggStats::default());
    });
}

// ------------------------------------------------------------------ scripts with colliding node ids

const SCRIPT_A: &str = "var res = []; function okA(v) { res.push('A' + v.length); return 'a'; } function failA(e) { res.push('Ae' + e.errors.length); }
    var itA = { [Symbol.iterator]: function () { var i = 0; return { next: function () { i++; return { done: i > 2, value: 'a' + i }; } }; } };";
const SCRIPT_B: &str = "function okB(v) { res.push('B' + v.length); return 'b'; } function failB(e) { res.push('Be' + e.errors.length); }
    var itB = { [Symbol.iterator]: function () { var i = 0; return { next: function () { i++; return { done: i > 3, value: Promise.resolve('b' + i) }; } }; } };";

#[test]
fn callbacks_iterators_and_getters_of_several_scripts_with_colliding_ids_work_together() {
    big(|| {
        let mut it = realm(lim());
        assert_eq!(out(&mut it, SCRIPT_A), "undefined");
        assert_eq!(out(&mut it, SCRIPT_B), "undefined");
        // iterators of A and B, callbacks of A and B, a rejecting thenable of a third script, a collect between
        assert_eq!(out(&mut it, "var c = { then: function (ok, bad) { bad('c'); } };
            Promise.all(itA).then(okA); Promise.all(itB).then(okB); Promise.allSettled(itB).then(okA);
            Promise.any([c, c]).catch(failB); Promise.any(itA).then(function (v) { res.push('any' + v); }); Promise.race(itB).then(okB);
            Promise.any({ [Symbol.iterator]: function () { return { next: function () { return { done: true }; } }; } }).catch(failA); 0"), "0");
        it.collect(&[]).unwrap();
        it.drain_microtasks().unwrap();
        it.collect(&[]).unwrap();
        let got = out(&mut it, "res.slice().sort().join()");
        assert_eq!(got, "A2,A3,Ae0,B2,B3,Be2,anya1", "{got}");
        assert_eq!(it.cur, 0);
        // the host calls a combinator and the result's callbacks, after other scripts ran
        let f = it.run_script("(function (x) { return Promise.all([x, 'z']); })").unwrap();
        let p = it.call(f, Value::Undefined, &[Value::Number(1.0)]).unwrap();
        it.drain_microtasks().unwrap();
        assert!(matches!(it.promise_status(p), Some(PromiseStatus::Fulfilled(_))));
        assert_eq!(it.cur, 0);
    });
}

// ------------------------------------------------------------------ the intrinsics do not come from globals

#[test]
fn replaced_or_deleted_globals_do_not_change_what_combinators_use() {
    big(|| {
        let mut it = realm(lim());
        out(&mut it, "var P = Promise, AE = AggregateError, E = Error, A = Array, O = Object, S = Symbol, res = [];");
        for name in ["Array", "Object", "Symbol", "AggregateError", "Error", "Promise"] {
            let k = it.heap.key_from_str(name).unwrap();
            let g = it.global_object();
            it.heap.delete(g, k).unwrap();
        }
        assert_eq!(out(&mut it, "typeof Promise + typeof AggregateError + typeof Array"), "undefinedundefinedundefined");
        out(&mut it, "P.any([P.reject(1), P.reject(2)]).catch(function (e) { res.push(e instanceof AE, e instanceof E, e.errors.join(), O.getPrototypeOf(e) === AE.prototype); });
                      P.all([1, P.resolve(2)]).then(function (v) { res.push(v.join(), A.isArray(v)); });
                      P.allSettled([3]).then(function (v) { res.push(v[0].status, O.getPrototypeOf(v[0]) === O.prototype); });
                      P.race([4]).then(function (v) { res.push(v); });");
        it.drain_microtasks().unwrap();
        assert_eq!(out(&mut it, "res.join('|')"), "true|true|1,2|true|1,2|true|fulfilled|true|4");
    });
}

// ------------------------------------------------------------------ load-style checks that are cheap enough for a unit test

#[test]
fn a_thousand_items_keep_order_and_free_everything() {
    big(|| {
        let mut it = realm(lim());
        let r = settle(&mut it, "var ps = []; for (var i = 0; i < 1000; i++) ps.push(i % 2 ? Promise.resolve(i) : mk(i)); function mk(k) { return { then: function (ok) { ok(k); } }; } var sum = 0, n = 0; var order = true;
            Promise.all(ps).then(function (v) { n = v.length; for (var k = 0; k < v.length; k++) { if (v[k] !== k) order = false; sum += v[k]; } });", "n + ':' + order + ':' + sum");
        assert_eq!(r, "1000:true:499500", "{r}");
        it.collect(&[]).unwrap();
        assert_eq!(it.agg_stats(), AggStats::default());
    });
}
