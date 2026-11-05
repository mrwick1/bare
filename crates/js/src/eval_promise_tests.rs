//! Promise and microtask tests. The corpus (`tools/fixtures/js/promise-cases.txt`) is checked against what Node
//! gave (`promise-cases.expected`, made by `tools/js/promise_oracle.mjs`; `tools/js/promise_check.sh` re-runs Node).
//! The tests below cover what Node cannot show: limits, gates, roots and GC of promise metadata.

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
        Error::Limit(_) => "limit".into(),
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
    let (cases, expected) = (include_str!("../../../tools/fixtures/js/promise-cases.txt"), include_str!("../../../tools/fixtures/js/promise-cases.expected"));
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
        assert!(list.len() >= 60);
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

/// Run `f` with an interpreter for `src` (script not run yet). Big stack, small step budget.
fn with<R: Send + 'static>(src: &'static str, f: impl FnOnce(&mut Interp) -> R + Send + 'static) -> R {
    big(move || {
        let prog = parse_script(src, Limits::default()).unwrap();
        let mut it = Interp::new(&prog, Heap::default(), EvalLimits { max_steps: 5_000_000, ..Default::default() }).unwrap();
        f(&mut it)
    })
}

fn global_get(it: &mut Interp, name: &str) -> Value {
    let k = it.heap.key_from_str(name).unwrap();
    match it.heap.get(it.global_object(), k).unwrap() {
        Get::Value(v) => v,
        _ => panic!("accessor"),
    }
}

fn out_text(it: &mut Interp) -> String {
    let out = global_get(it, "out");
    it.get_str(out, "length") + ":" + &{
        let k = it.heap.key_from_str("join").unwrap();
        let Get::Value(j) = it.heap.get(match out { Value::Object(o) => o, _ => panic!() }, k).unwrap() else { panic!() };
        let sep = Value::String(it.heap.intern_str("|").unwrap());
        let r = it.call(j, out, &[sep]).unwrap();
        it.display(r)
    }
}

#[test]
fn gates_are_explicit_unsupported_not_silent() {
    let src = "Promise.resolve.call(function () {}, 1)";
    assert_eq!(run_case(src), "unsupported:Promise static method on a constructor other than Promise", "{src}");
    // an async shape that is not supported is found before anything runs
    for src in ["async function f(o) { return (await o)?.x; }", "var f = async () => { for (var k in await 1) {} };"] {
        let prog = parse_script(src, Limits::default()).unwrap();
        assert!(matches!(Interp::new(&prog, Heap::default(), EvalLimits::default()), Err(Error::Unsupported(_))), "{src}");
    }
}

#[test]
fn jobs_run_only_at_the_checkpoint_and_in_fifo_order() {
    with("var out = []; Promise.resolve(1).then(function (v) { out.push('a' + v); Promise.resolve(2).then(function (w) { out.push('c' + w); }); }); Promise.resolve(3).then(function (v) { out.push('b' + v); }); out.push('sync');", |it| {
        it.run().unwrap();
        assert_eq!(it.pending_jobs(), 2);
        assert_eq!(out_text(it), "1:sync");
        assert_eq!(it.drain_microtasks().unwrap(), 3);
        assert_eq!(it.pending_jobs(), 0);
        assert_eq!(out_text(it), "4:sync|a1|b3|c2");
        assert_eq!(it.drain_microtasks().unwrap(), 0);
    });
}

#[test]
fn promise_status_and_unhandled_rejections() {
    with("var a = Promise.reject(1); var b = Promise.reject(2); b.catch(function () {}); var c = Promise.resolve(3).then(function () { throw 4; }); var d = new Promise(function () {});", |it| {
        it.run().unwrap();
        it.drain_microtasks().unwrap();
        let (a, b, c, d) = (global_get(it, "a"), global_get(it, "b"), global_get(it, "c"), global_get(it, "d"));
        assert!(matches!(it.promise_status(a), Some(PromiseStatus::Rejected(Value::Number(n))) if n == 1.0));
        assert!(matches!(it.promise_status(c), Some(PromiseStatus::Rejected(Value::Number(n))) if n == 4.0));
        assert!(matches!(it.promise_status(d), Some(PromiseStatus::Pending)));
        assert!(it.promise_status(Value::Number(1.0)).is_none());
        let un = it.unhandled_rejections();
        assert_eq!(un.len(), 2);
        assert!(matches!((un[0], a), (Value::Object(x), Value::Object(y)) if x == y));
        assert!(matches!((un[1], c), (Value::Object(x), Value::Object(y)) if x == y));
        let _ = b;
    });
}

fn limit(r: Result<impl std::fmt::Debug, Error>) -> LimitKind {
    match r {
        Err(Error::Limit(k)) => k,
        other => panic!("expected a limit, got {other:?}"),
    }
}

#[test]
fn queue_limit_is_distinct_and_checked_before_state_changes() {
    with("var out = []; for (var i = 0; i < 20; i++) Promise.resolve(i).then(function () {});", |it| {
        it.set_promise_limits(PromiseLimits { max_queue: 10, ..Default::default() });
        assert_eq!(limit(it.run()), LimitKind::PromiseQueue);
        assert_eq!(it.pending_jobs(), 10);
    });
    // 11 reactions on one pending promise cannot all be queued: the promise must stay pending.
    with("var res; var p = new Promise(function (r) { res = r; }); for (var i = 0; i < 11; i++) p.then(function () {});", |it| {
        it.set_promise_limits(PromiseLimits { max_queue: 10, ..Default::default() });
        it.run().unwrap();
        let (p, res) = (global_get(it, "p"), global_get(it, "res"));
        assert_eq!(limit(it.call(res, Value::Undefined, &[Value::Number(1.0)])), LimitKind::PromiseQueue);
        assert!(matches!(it.promise_status(p), Some(PromiseStatus::Pending)));
        assert_eq!(it.pending_jobs(), 0);
    });
}

#[test]
fn reaction_and_promise_limits_are_distinct() {
    with("var p = new Promise(function () {}); for (var i = 0; i < 6; i++) p.then(function () {});", |it| {
        it.set_promise_limits(PromiseLimits { max_reactions: 5, ..Default::default() });
        assert_eq!(limit(it.run()), LimitKind::PromiseReactions);
    });
    with("for (var i = 0; i < 20; i++) new Promise(function () {});", |it| {
        it.set_promise_limits(PromiseLimits { max_promises: 10, ..Default::default() });
        assert_eq!(limit(it.run()), LimitKind::Promises);
        // unreachable promises are dropped by collect, so the room comes back
        it.collect(&[]).unwrap();
        assert!(it.promise.map.len() < 3, "{}", it.promise.map.len());
    });
}

#[test]
fn drain_cap_leaves_the_rest_queued() {
    with("var n = 0; for (var i = 0; i < 10; i++) Promise.resolve().then(function () { n++; });", |it| {
        it.set_promise_limits(PromiseLimits { max_jobs_per_drain: 4, ..Default::default() });
        it.run().unwrap();
        assert_eq!(limit(it.drain_microtasks()), LimitKind::DrainJobs);
        assert_eq!(it.pending_jobs(), 6);
        assert_eq!(limit(it.drain_microtasks()), LimitKind::DrainJobs);
        assert_eq!(it.pending_jobs(), 2);
        assert_eq!(it.drain_microtasks().unwrap(), 2);
        let n = global_get(it, "n");
        assert_eq!(it.display(n), "10");
    });
    // a job loop that adds a job each time is stopped by the same cap
    with("function again() { Promise.resolve().then(again); } again();", |it| {
        it.set_promise_limits(PromiseLimits { max_jobs_per_drain: 1000, ..Default::default() });
        it.run().unwrap();
        assert_eq!(limit(it.drain_microtasks()), LimitKind::DrainJobs);
        assert_eq!(it.pending_jobs(), 1);
    });
}

#[test]
fn execution_limits_inside_promise_code_are_not_rejections() {
    // steps: a handler that never ends
    with("Promise.resolve().then(function () { while (true) {} });", |it| {
        it.run().unwrap();
        it.limits.max_steps = 10_000;
        assert_eq!(limit(it.drain_microtasks()), LimitKind::Steps);
        assert!(it.unhandled_rejections().is_empty());
    });
    // call depth
    with("function f() { return f(); } Promise.resolve().then(f); new Promise(function () { f(); });", |it| {
        assert_eq!(limit(it.run()), LimitKind::CallDepth);
    });
    with("function f() { return f(); } Promise.resolve().then(f);", |it| {
        it.run().unwrap();
        assert_eq!(limit(it.drain_microtasks()), LimitKind::CallDepth);
    });
    // a limit inside a thenable's `then` is not a rejection either
    with("Promise.resolve({ then: function () { while (true) {} } });", |it| {
        it.run().unwrap();
        it.limits.max_steps = 10_000;
        assert_eq!(limit(it.drain_microtasks()), LimitKind::Steps);
        assert!(it.unhandled_rejections().is_empty());
    });
}

#[test]
fn long_chains_and_deep_thenable_nesting_do_not_recurse_natively() {
    with("var out = []; var p = Promise.resolve(0); for (var i = 0; i < 20000; i++) p = p.then(function (v) { return v + 1; }); p.then(function (v) { out.push(v); });", |it| {
        it.run().unwrap();
        assert_eq!(it.drain_microtasks().unwrap(), 20001);
        assert_eq!(out_text(it), "1:20000");
    });
    // resolve(p) where p was resolved with another promise, 3000 deep
    with("var out = []; var p = Promise.resolve('end'); for (var i = 0; i < 3000; i++) p = (function (q) { return new Promise(function (r) { r(q); }); })(p); p.then(function (v) { out.push(v); });", |it| {
        it.run().unwrap();
        it.drain_microtasks().unwrap();
        assert_eq!(out_text(it), "1:end");
    });
}

#[test]
fn collect_before_drain_keeps_queued_callbacks_and_their_closures() {
    with("var out = []; (function () { var z = 'zz'; var big = { k: 'kk' }; Promise.resolve(1).then(function (v) { out.push(v + z + big.k); }); })();
          for (var i = 0; i < 300; i++) (function (j) { Promise.resolve(j).then(function () { j = null; }); })(i);", |it| {
        it.run().unwrap();
        it.collect(&[]).unwrap();
        assert_eq!(it.pending_jobs(), 301);
        it.drain_microtasks().unwrap();
        assert_eq!(out_text(it), "1:1zzkk");
    });
}

#[test]
fn promise_reachable_only_through_a_resolver_survives_collect() {
    with("var out = []; var res; (function () { var z = 'zz'; var p = new Promise(function (r) { res = r; }); p.then(function (v) { out.push(v + z); }).then(function (v) { out.push('chained' + v); }); })();", |it| {
        it.run().unwrap();
        let promises_before = it.promise.map.len();
        it.collect(&[]).unwrap();
        assert_eq!(it.promise.map.len(), promises_before, "p, its derived promise and the chain are all reachable");
        let res = global_get(it, "res");
        it.call(res, Value::Undefined, &[Value::Number(5.0)]).unwrap();
        // a second collect with the jobs queued
        it.collect(&[]).unwrap();
        it.drain_microtasks().unwrap();
        assert_eq!(out_text(it), "2:5zz|chainedundefined");
    });
}

#[test]
fn unreferenced_promise_cycles_are_reclaimed() {
    with("var out = [];
          for (var i = 0; i < 500; i++) (function () {
              var a, b, ra;
              a = new Promise(function (r) { ra = r; });
              b = a.then(function () { return b; });   // handler references its own derived promise
              a.then(function () { return a; });       // and its parent
              a.then(function () { return ra; });      // and the resolver
          })();
          var keep = new Promise(function () {}); keep.then(function () {});", |it| {
        let intrinsic_functions = it.stats().functions;
        it.run().unwrap();
        let before = (it.promise.map.len(), it.stats().functions, it.heap.stats().live_objects, it.promise.reactions);
        assert!(before.0 >= 2000 && before.3 >= 1500, "{before:?}");
        it.collect(&[]).unwrap();
        let after = (it.promise.map.len(), it.stats().functions, it.heap.stats().live_objects, it.promise.reactions);
        assert!(after.0 <= 3, "{after:?}");
        assert!(after.1 <= intrinsic_functions + 2, "{after:?}");
        assert!(after.2 < before.2 / 10, "{before:?} {after:?}");
        assert!(after.3 <= 1, "{after:?}");
        assert!(it.promise.done_pairs.len() <= 2, "{}", it.promise.done_pairs.len());
    });
}

#[test]
fn settled_values_and_host_held_promises_are_roots() {
    with("var out = []; var held = Promise.resolve({ tag: 'obj', list: [1, 2, 3] }); var rej = Promise.reject(new RangeError('rr')); rej.catch(function () {}); Promise.resolve({ tag: 'obj2' });", |it| {
        it.run().unwrap();
        it.drain_microtasks().unwrap();
        it.collect(&[]).unwrap();
        let held = global_get(it, "held");
        let Some(PromiseStatus::Fulfilled(v)) = it.promise_status(held) else { panic!() };
        assert_eq!(it.get_str(v, "tag"), "obj");
        let rej = global_get(it, "rej");
        let Some(PromiseStatus::Rejected(e)) = it.promise_status(rej) else { panic!() };
        assert_eq!(it.get_str(e, "message"), "rr");
    });
    // a promise that only the host holds (the script's completion value) needs `extra`
    big(|| {
        let prog = parse_script("Promise.resolve({ tag: 'kept' })", Limits::default()).unwrap();
        let mut it = Interp::new(&prog, Heap::default(), EvalLimits::default()).unwrap();
        let p = it.run().unwrap();
        it.drain_microtasks().unwrap();
        it.collect(&[p]).unwrap();
        let Some(PromiseStatus::Fulfilled(v)) = it.promise_status(p) else { panic!() };
        assert_eq!(it.get_str(v, "tag"), "kept");
    });
}

#[test]
fn recycled_slots_never_confuse_resolver_metadata() {
    // 200 promises die and their slots are free. The surviving resolver pair still settles exactly its own promise, once.
    with("var out = []; var res, rej; var keep = new Promise(function (a, b) { res = a; rej = b; }); keep.then(function (v) { out.push('keep' + v); });
          for (var i = 0; i < 200; i++) new Promise(function (r) { r(i); }).then(function () {});", |it| {
        it.run().unwrap();
        it.drain_microtasks().unwrap();
        it.collect(&[]).unwrap();
        let (res, rej) = (global_get(it, "res"), global_get(it, "rej"));
        it.call(res, Value::Undefined, &[Value::Number(1.0)]).unwrap();
        it.call(rej, Value::Undefined, &[Value::Number(2.0)]).unwrap();
        it.call(res, Value::Undefined, &[Value::Number(3.0)]).unwrap();
        it.drain_microtasks().unwrap();
        assert_eq!(out_text(it), "1:keep1");
        assert_eq!(it.promise.done_pairs.len(), 1);
        it.collect(&[]).unwrap();
        assert_eq!(it.promise.done_pairs.len(), 1, "res and rej are still alive");
    });
}

#[test]
fn settling_fanout_consumes_work_before_changing_state() {
    with("var resolve; var p = new Promise(function(r) { resolve=r; }); for (var i=0; i<100; i++) p.then(function() {});", |it| {
        it.run().unwrap();
        it.limits.max_steps = 10;
        let resolve = global_get(it, "resolve");
        assert_eq!(limit(it.call(resolve, Value::Undefined, &[Value::Number(1.0)])), LimitKind::Steps);
        assert_eq!(it.pending_jobs(), 0);
        let promise = global_get(it, "p");
        assert!(matches!(it.promise_status(promise), Some(PromiseStatus::Pending)));
    });
}

#[test]
fn promise_identity_counters_do_not_wrap() {
    with("Promise.resolve(1)", |it| {
        it.promise.next_seq = u64::MAX;
        assert_eq!(limit(it.run()), LimitKind::Promises);
    });
    with("new Promise(function() {})", |it| {
        it.promise.next_pair = u64::MAX;
        assert_eq!(limit(it.run()), LimitKind::Functions);
    });
}

#[test]
fn a_failed_job_cannot_be_hidden_by_a_later_empty_drain() {
    with("Promise.resolve().then(function() { for (;;) {} });", |it| {
        it.run().unwrap();
        it.limits.max_steps = 100;
        assert_eq!(limit(it.drain_microtasks()), LimitKind::Steps);
        assert_eq!(limit(it.drain_microtasks()), LimitKind::Steps);
    });
}
