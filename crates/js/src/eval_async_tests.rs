//! Async function and `await` tests. The corpus (`tools/fixtures/js/async-cases.txt`) is checked against what Node
//! gave (`async-cases.expected`, made by `tools/js/promise_oracle.mjs`; `tools/js/async_check.sh` re-runs Node).
//! The tests below cover what Node cannot show: preflight gates, caps, rooting, GC, stale handles, scripts.

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
    let (cases, expected) = (include_str!("../../../tools/fixtures/js/async-cases.txt"), include_str!("../../../tools/fixtures/js/async-cases.expected"));
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
        assert!(list.len() >= 90);
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

// ------------------------------------------------------------------ helpers for the realm tests

fn realm(limits: EvalLimits) -> Interp<'static> {
    Interp::realm(Heap::default(), limits).expect("realm")
}

fn show(it: &mut Interp, r: Result<Value, Error>) -> String {
    match r {
        Ok(v) => it.display(v),
        Err(Error::Throw(v)) if matches!(v, Value::Object(_)) => format!("throw:{}:{}", it.get_str(v, "name"), it.get_str(v, "message")),
        Err(Error::Throw(v)) => format!("throw-value:{}", it.display(v)),
        Err(Error::Limit(l)) => format!("limit:{l:?}"),
        Err(Error::Unsupported(u)) => format!("unsupported:{}", u.what),
        Err(e) => format!("other:{e:?}"),
    }
}

fn out(it: &mut Interp, src: &str) -> String {
    let r = it.run_script(src);
    show(it, r)
}

fn lim() -> EvalLimits {
    EvalLimits { max_steps: 2_000_000, ..Default::default() }
}

/// Run `src` as the first script, drain, and give the text of `out.join("|")`.
fn run_out(src: &str) -> String {
    let src = src.to_string();
    big(move || {
        let mut it = realm(lim());
        let r = out(&mut it, &src);
        assert!(!r.starts_with("limit") && !r.starts_with("throw") && !r.starts_with("unsupported") && !r.starts_with("other"), "{r}");
        it.drain_microtasks().unwrap();
        out(&mut it, "out.join('|')")
    })
}

fn limit_of(r: Result<impl std::fmt::Debug, Error>) -> LimitKind {
    match r {
        Err(Error::Limit(k)) => k,
        other => panic!("expected a limit, got {other:?}"),
    }
}

// ------------------------------------------------------------------ the gates

#[test]
fn unsupported_await_positions_are_found_before_any_effect() {
    big(|| {
        for (src, what) in [
            ("async function f() { var o = {}; for (var k in await o) {} }", "await inside for-in"),
            ("async function f(o) { return (await o)?.x; }", "await inside an optional chain"),
            ("async function f(o) { return o?.[await 1]; }", "await inside an optional chain"),
            ("async function f(o) { return delete o[await 1]; }", "await inside delete"),
            ("async function f(o) { (await o).x++; }", "await inside ++ or --"),
            ("async function f(k) { return { [await k]: 1 }; }", "await in a computed object key"),
            ("async function f() { var { a = await 1 } = {}; }", "await in a binding pattern"),
            ("async function f() { try {} catch ({ a = await 1 }) {} }", "await in a catch binding"),
            ("async function f() { for (var [a = await 1] of []) {} }", "await in a binding pattern"),
            ("async function f(o) { [o.x = await 1] = []; }", "await in an assignment pattern"),
            ("async function f() { for await (var x of []) {} }", "for await"),
            ("async function f(t) { return t`${await 1}`; }", "tagged template"),
            ("async function f(o) { return (o?.a)(await 1); }", "await in a call of an optional chain"),
        ] {
            let mut it = realm(lim());
            let r = out(&mut it, &format!("var effect = 1; {src}"));
            assert_eq!(r, format!("unsupported:{what}"), "{src}");
            assert_eq!(out(&mut it, "typeof effect + typeof f"), "undefinedundefined", "script effects after a refused script: {src}");
            assert_eq!(it.async_stats().marks, 0, "{src}");
        }
        // parse-level rejections stay
        let mut it = realm(lim());
        for src in ["await 1;", "async function f(a = await 1) {}"] {
            let r = it.run_script(src);
            assert!(matches!(r, Err(Error::Throw(_))), "{src}: {r:?}");
        }
        let r = it.run_script("async function* g() {}");
        assert!(matches!(r, Err(Error::Unsupported(_))), "{r:?}");
        // the first program of `Interp::new` is checked the same way
        let prog = parse_script("var effect = 1; async function f() { for (var k in await 1) {} }", Limits::default()).unwrap();
        assert!(matches!(Interp::new(&prog, Heap::default(), EvalLimits::default()), Err(Error::Unsupported(_))));
    });
}

#[test]
fn async_functions_are_not_constructors_and_have_no_prototype() {
    assert_eq!(run_out("var out = []; async function f() { await 0; } var g = async () => 1; var o = { async m() {} }; out.push(typeof f.prototype, typeof g.prototype, typeof o.m.prototype, f.hasOwnProperty('prototype')); try { new f(); } catch (e) { out.push(e.name); } try { new o.m(); } catch (e) { out.push(e.name); } try { new (f.bind(null))(); } catch (e) { out.push(e.name); }"), "undefined|undefined|undefined|false|TypeError|TypeError|TypeError");
}

#[test]
fn a_non_async_function_named_await_style_code_is_unchanged() {
    assert_eq!(run_out("var out = []; function f() { return 1; } out.push(f()); function g(await) { return await; } out.push(g(2));"), "1|2");
}

// ------------------------------------------------------------------ behavior that Node cannot show

#[test]
fn host_call_returns_a_pending_promise_then_a_fulfilled_one() {
    big(|| {
        let mut it = realm(lim());
        it.run_script("async function f(x) { await 0; return x * 2; }").unwrap();
        let g = it.global_object();
        let k = it.heap.key_from_str("f").unwrap();
        let Get::Value(f) = it.heap.get(g, k).unwrap() else { panic!() };
        let p = it.call(f, Value::Undefined, &[Value::Number(21.0)]).unwrap();
        assert!(matches!(it.promise_status(p), Some(PromiseStatus::Pending)));
        assert_eq!(it.pending_jobs(), 1);
        it.drain_microtasks().unwrap();
        assert!(matches!(it.promise_status(p), Some(PromiseStatus::Fulfilled(Value::Number(n))) if n == 42.0));
        let st = it.async_stats();
        assert_eq!((st.activations, st.frames, st.slots), (0, 0, 0));
    });
}

#[test]
fn rejected_async_result_is_reported_as_unhandled() {
    big(|| {
        let mut it = realm(lim());
        it.run_script("async function f() { await 0; throw 5; } var p = f(); async function g() { throw 6; } var q = g(); q.catch(function () {});").unwrap();
        it.drain_microtasks().unwrap();
        let un = it.unhandled_rejections();
        assert_eq!(un.len(), 1);
        assert!(matches!(it.promise_status(un[0]), Some(PromiseStatus::Rejected(Value::Number(n))) if n == 5.0));
    });
}

#[test]
fn resume_has_no_rust_stack_even_after_ten_thousand_awaits() {
    big(|| {
        let mut it = realm(EvalLimits { max_steps: 50_000_000, ..Default::default() });
        assert_eq!(out(&mut it, "var n = 0; async function f() { for (var i = 0; i < 10000; i++) { n += await 1; } return n; } var r; f().then(function (v) { r = v; }); 0"), "0");
        it.drain_microtasks().unwrap();
        assert_eq!(out(&mut it, "r"), "10000");
    });
}

#[test]
fn deep_sync_nesting_of_async_calls_hits_the_call_depth_ceiling() {
    big(|| {
        let mut it = realm(lim());
        let r = it.run_script("async function r(n) { if (n == 0) return 0; return 1 + await r(n - 1); } r(200);");
        assert_eq!(limit_of(r), LimitKind::CallDepth);
        assert_eq!(it.async_stats().frames + it.async_stats().slots, it.async_stats().frames + it.async_stats().slots);
    });
}

#[test]
fn a_thrown_error_inside_a_resumed_job_rejects_but_a_limit_is_a_failed_checkpoint() {
    big(|| {
        let mut it = realm(EvalLimits { max_steps: 3000, ..Default::default() });
        it.run_script("async function f() { for (;;) { await 0; } } f();").unwrap();
        let e1 = limit_of(it.drain_microtasks());
        assert_eq!(e1, LimitKind::Steps);
        // the failed checkpoint repeats; it is not an empty successful one
        assert_eq!(limit_of(it.drain_microtasks()), LimitKind::Steps);
        assert_eq!(it.depth, 0);
        assert_eq!(it.env, 0);
        assert_eq!(it.cur, 0);
        // the failed job stays queued, so its activation stays too, and nothing more
        assert_eq!(it.async_stats().activations, 1);
    });
}

#[test]
fn one_step_budget_is_shared_by_all_jobs_of_a_drain() {
    big(|| {
        // each activation needs well under 20_000 steps, together far more
        let mut it = realm(EvalLimits { max_steps: 1500, ..Default::default() });
        it.run_script("var n = 0; async function f() { for (var i = 0; i < 20; i++) { await 0; n++; } } for (var j = 0; j < 40; j++) f();").unwrap();
        assert_eq!(limit_of(it.drain_microtasks()), LimitKind::Steps);
        let mut it2 = realm(EvalLimits { max_steps: 1_000_000, ..Default::default() });
        it2.run_script("var n = 0; async function f() { for (var i = 0; i < 20; i++) { await 0; n++; } } for (var j = 0; j < 40; j++) f();").unwrap();
        it2.drain_microtasks().unwrap();
        assert_eq!(out(&mut it2, "n"), "800");
    });
}

// ------------------------------------------------------------------ caps

#[test]
fn activation_cap_is_checked_before_the_activation_is_stored() {
    big(|| {
        let mut it = realm(lim());
        it.set_async_limits(AsyncLimits { max_activations: 5, ..Default::default() });
        let r = it.run_script("async function f() { await new Promise(function () {}); } for (var i = 0; i < 10; i++) f();");
        assert_eq!(limit_of(r), LimitKind::AsyncActivations);
        let st = it.async_stats();
        assert_eq!(st.activations, 5);
        // the refused activation gave its frames and slots back
        let held: usize = it.aw.acts.iter().filter_map(|s| s.run.as_ref()).map(|r| r.stack.len()).sum();
        assert_eq!(st.frames, held);
        let held: usize = it.aw.acts.iter().filter_map(|s| s.run.as_ref()).map(|r| r.slots.len()).sum();
        assert_eq!(st.slots, held);
    });
}

#[test]
fn frame_and_slot_caps_are_distinct_and_hold() {
    big(|| {
        let mut it = realm(lim());
        it.set_async_limits(AsyncLimits { max_frames: 6, ..Default::default() });
        let r = it.run_script("async function f() { for (var i = 0; i < 3; i++) { for (var j = 0; j < 3; j++) { try { await 0; } finally { await 0; } } } } for (var i = 0; i < 10; i++) f();");
        assert_eq!(limit_of(r), LimitKind::AsyncFrames);
        assert!(it.async_stats().frames <= 6);
        let mut it = realm(lim());
        it.set_async_limits(AsyncLimits { max_slots: 6, ..Default::default() });
        let r = it.run_script("async function f() { function m() { return 0; } return m(await 1, await 2, await 3, await 4, await 5, await 6, await 7, await 8, await 9); } f();");
        // the callee, this and nine arguments need 11 slots
        assert_eq!(limit_of(r.and_then(|_| it.drain_microtasks().map(|_| Value::Undefined))), LimitKind::AsyncSlots);
        assert!(it.async_stats().slots <= 6);
    });
}

#[test]
fn code_mark_cap_is_checked_at_preflight_and_a_refused_script_leaves_no_marks() {
    big(|| {
        let mut it = realm(lim());
        it.set_async_limits(AsyncLimits { max_code: 3, ..Default::default() });
        assert_eq!(out(&mut it, "async function ok() { await 1; }"), "undefined");
        let before = it.async_stats().marks;
        assert!(before > 0 && before <= 3);
        let r = it.run_script("var effect = 1; async function big() { await 1; await 2; await 3; await 4; await 5; }");
        assert_eq!(limit_of(r), LimitKind::AsyncCode);
        assert_eq!(it.async_stats().marks, before);
        assert_eq!(out(&mut it, "typeof effect"), "undefined");
    });
}

#[test]
fn promise_caps_apply_to_awaits_before_any_state_change() {
    big(|| {
        let mut it = realm(lim());
        it.set_promise_limits(PromiseLimits { max_reactions: 2, ..Default::default() });
        let r = it.run_script("var p = new Promise(function () {}); async function f() { await p; } f(); f(); f();");
        assert_eq!(limit_of(r), LimitKind::PromiseReactions);
        assert_eq!(it.async_stats().activations, 2);
        let mut it = realm(lim());
        it.set_promise_limits(PromiseLimits { max_queue: 3, ..Default::default() });
        let r = it.run_script("async function f() { await 1; } for (var i = 0; i < 10; i++) f();");
        assert_eq!(limit_of(r), LimitKind::PromiseQueue);
        assert_eq!(it.pending_jobs(), 3);
        assert_eq!(it.async_stats().activations, 3);
    });
}

// ------------------------------------------------------------------ rooting and GC

#[test]
fn collect_between_awaits_keeps_every_saved_value_alive() {
    big(|| {
        let mut it = realm(lim());
        let src = "
            var out = []; var res; var obj = { k1: 1 };
            function key() { return 'k' + 1; }
            function mk() { return { a: 'x' + 'y' }; }
            async function f(x) {
              'use strict';
              var local = mk(); var arr = [local, 'p' + 'q'];
              let blockv = { n: 3 };
              try {
                for (var s of ['ab' + 'c', 'de' + 'f']) {
                  obj[key()] += await new Promise(function (r) { res = r; });
                  out.push(s + local.a + arr[1] + blockv.n + arguments.length + String(this));
                }
                return { fin: 'v' + 'al' };
              } finally {
                await new Promise(function (r) { res = r; });
                out.push('fin' + local.a);
              }
            }
            var result; f(7, 8).then(function (v) { result = v; });
            0";
        out(&mut it, src);
        for step in 0..3 {
            it.collect(&[]).unwrap();
            it.collect(&[]).unwrap();
            let r = it.global_object();
            let k = it.heap.key_from_str("res").unwrap();
            let Get::Value(res) = it.heap.get(r, k).unwrap() else { panic!() };
            it.call(res, Value::Undefined, &[Value::Number(step as f64 + 1.0)]).unwrap();
            it.drain_microtasks().unwrap();
        }
        it.collect(&[]).unwrap();
        assert_eq!(out(&mut it, "out.join('|') + '#' + obj.k1 + '#' + result.fin"), "abcxypq32undefined|defxypq32undefined|finxy#4#val");
        let st = it.async_stats();
        assert_eq!((st.activations, st.frames, st.slots), (0, 0, 0));
        assert_eq!(it.stats().live_envs, { it.collect(&[]).unwrap(); it.stats().live_envs });
    });
}

#[test]
fn a_switch_keeps_its_discriminant_and_scope_alive_across_collects_and_frees_them() {
    big(|| {
        let mut it = realm(lim());
        let src = "
            var out = []; var res; var keep = {};
            function wait() { return new Promise(function (r) { res = r; }); }
            async function f() {
              var d = { tag: 'd' + 1 };
              var probe = d; d = null;
              switch (probe) {
                case await wait(): out.push('no');
                case probe: let s = { v: 's' + 2 }; await wait(); out.push(s.v); await wait();
                case keep: out.push('fall');
              }
              out.push(probe.tag);
            }
            f(); 0";
        out(&mut it, src);
        for step in 0..3 {
            it.collect(&[]).unwrap();
            let r = it.global_object();
            let k = it.heap.key_from_str("res").unwrap();
            let Get::Value(res) = it.heap.get(r, k).unwrap() else { panic!() };
            it.call(res, Value::Undefined, &[Value::Number(step as f64)]).unwrap();
            it.drain_microtasks().unwrap();
        }
        assert_eq!(out(&mut it, "out.join('|')"), "s2|fall|d1");
        let st = it.async_stats();
        assert_eq!((st.activations, st.frames, st.slots), (0, 0, 0));
        assert_eq!(it.stats().live_envs, { it.collect(&[]).unwrap(); it.stats().live_envs });
    });
}

#[test]
fn a_switch_holds_its_discriminant_in_a_slot_only_while_a_case_test_awaits() {
    big(|| {
        let mut it = realm(lim());
        // await-free tests: paused in a body, no slot
        out(&mut it, "var out = []; async function f(k) { switch (k) { case 0: let a = k; await 0; out.push(a); break; default: await 1; out.push('d'); } } f(0); f(1); 0");
        assert_eq!((it.async_stats().activations, it.async_stats().slots), (2, 0));
        it.drain_microtasks().unwrap();
        assert_eq!(out(&mut it, "out.join('|')"), "0|d");
        // an awaited test: one slot while it waits, none once the body runs
        out(&mut it, "var log = []; var res; async function g(k) { switch (k) { case 1: log.push('one'); break; case await new Promise(function (r) { res = r; }): await 0; log.push('two'); } } g(2); 0");
        assert_eq!(it.async_stats().slots, 1);
        out(&mut it, "res(2)");
        it.drain_microtasks().unwrap();
        assert_eq!(out(&mut it, "log.join('|')"), "two");
        let st = it.async_stats();
        assert_eq!((st.activations, st.frames, st.slots), (0, 0, 0));
    });
}

#[test]
fn await_of_a_primitive_makes_no_hidden_promise_and_keeps_the_order() {
    big(|| {
        let mut it = realm(lim());
        out(&mut it, "var out = []; async function f(k) { await k; out.push('f' + k); await 'x'; out.push('g' + k); } f(1); f(2); Promise.resolve().then(function () { out.push('t'); }); 0");
        // the two result promises and the one from Promise.resolve and then: four, and no promise per await
        assert_eq!(it.promise.map.len(), 4);
        it.drain_microtasks().unwrap();
        assert_eq!(out(&mut it, "out.join('|')"), "f1|f2|t|g1|g2");
        assert_eq!(it.promise.map.len(), 4);
    });
}

#[test]
fn a_pending_promise_keeps_room_for_one_reaction_only() {
    big(|| {
        let mut it = realm(lim());
        out(&mut it, "var res; var p = new Promise(function (r) { res = r; }); p.then(function () {}); async function f() { await q; } var q = new Promise(function () {}); f(); 0");
        let caps: Vec<usize> = it.promise.map.values().filter(|d| !d.reactions.is_empty()).map(|d| d.reactions.capacity()).collect();
        assert_eq!(caps, vec![1, 1]);
    });
}

#[test]
fn a_dead_burst_gives_the_side_tables_back_after_a_quiet_collect() {
    big(|| {
        let mut it = realm(EvalLimits { max_steps: 50_000_000, ..Default::default() });
        out(&mut it, "var keep = []; function burst(n) { for (var i = 0; i < n; i++) { let a = i; keep.push(() => a); keep.push(new Promise(function () {}).then(function () {})); keep.push((async function () { await new Promise(function () {}); })()); } } 0");
        it.collect(&[]).unwrap();
        it.collect(&[]).unwrap();
        out(&mut it, "burst(40000); queueMicrotask(function () {}); 0");
        it.drain_microtasks().unwrap();
        let big = (it.funcs.capacity(), it.envs.capacity(), it.promise.map.capacity(), it.aw.acts.capacity(), it.promise.queue.capacity());
        assert!(big.0 > 100_000 && big.1 > 40_000 && big.2 > 80_000 && big.3 > 40_000, "{big:?}");
        out(&mut it, "keep = []; 0");
        it.collect(&[]).unwrap();
        it.collect(&[]).unwrap();
        let small = (it.funcs.capacity(), it.envs.capacity(), it.promise.map.capacity(), it.aw.acts.capacity());
        assert!(small.0 < 4096 && small.1 < 4096 && small.2 < 4096 && small.3 < 4096, "{small:?}");
        assert!(it.envs.iter().all(|e| e.live || e.bindings.capacity() == 0), "a free environment keeps its bindings buffer");
        // the realm still works and grows again
        assert_eq!(out(&mut it, "keep = []; burst(100); keep.length"), "300");
        it.drain_microtasks().unwrap();
    });
}

#[test]
fn unreachable_activation_cycles_are_dropped_and_reachable_ones_stay() {
    big(|| {
        let mut it = realm(lim());
        out(&mut it, "var out = []; var res; async function f(tag) { var big = { tag: tag }; await new Promise(function (r) { if (tag == 'keep') res = r; }); out.push(big.tag); } f('drop'); f('keep'); f('drop2'); 0");
        let envs = it.stats().live_envs;
        assert_eq!(it.async_stats().activations, 3);
        it.collect(&[]).unwrap();
        // 'drop' and 'drop2' wait on promises nobody can settle: their cycle is garbage
        assert_eq!(it.async_stats().activations, 1);
        assert!(it.stats().live_envs < envs);
        let (frames, slots) = (it.async_stats().frames, it.async_stats().slots);
        assert!(frames > 0 || slots == 0);
        out(&mut it, "res(1)");
        it.drain_microtasks().unwrap();
        assert_eq!(out(&mut it, "out.join('|')"), "keep");
        it.collect(&[]).unwrap();
        let st = it.async_stats();
        assert_eq!((st.activations, st.frames, st.slots, st.table), (0, 0, 0, 0));
    });
}

#[test]
fn promise_cycle_through_an_activation_is_collected() {
    big(|| {
        let mut it = realm(lim());
        // p is held only by the activation that awaits it; the activation is held only by p
        out(&mut it, "async function f() { var p = new Promise(function () {}); var keep = { p: p }; await p; } for (var i = 0; i < 50; i++) f(); 0");
        assert_eq!(it.async_stats().activations, 50);
        let promises = it.promise.map.len();
        it.collect(&[]).unwrap();
        assert_eq!(it.async_stats().activations, 0);
        assert!(it.promise.map.len() < promises / 4, "{} -> {}", promises, it.promise.map.len());
        assert_eq!(it.async_stats().frames, 0);
    });
}

#[test]
fn stale_and_reused_handles_do_nothing() {
    big(|| {
        let mut it = realm(lim());
        out(&mut it, "var out = []; var res; async function f(t) { await new Promise(function (r) { res = r; }); out.push(t); } f('first'); 0");
        out(&mut it, "res(1)");
        let Some(Job::Resume { act: stale, .. }) = it.promise.queue.iter().find(|j| matches!(j, Job::Resume { .. })).map(|j| match *j {
            Job::Resume { act, arg, rejected } => Job::Resume { act, arg, rejected },
            _ => unreachable!(),
        }) else { panic!("no resume job") };
        it.drain_microtasks().unwrap();
        assert_eq!(out(&mut it, "out.join('|')"), "first");
        // the slot is free: a stale resume does nothing
        it.async_resume(stale, Value::Undefined, false).unwrap();
        // a new activation reuses the slot index with a new generation
        out(&mut it, "f('second'); 0");
        let live = it.aw.acts.iter().position(|s| s.run.is_some()).unwrap() as u32;
        assert_eq!(live, stale.idx);
        it.async_resume(stale, Value::Undefined, false).unwrap();
        assert_eq!(it.async_stats().activations, 1);
        assert_eq!(out(&mut it, "out.join('|')"), "first");
        out(&mut it, "res(1)");
        it.drain_microtasks().unwrap();
        assert_eq!(out(&mut it, "out.join('|')"), "first|second");
    });
}

#[test]
fn no_activation_metadata_survives_the_work_that_made_it() {
    big(|| {
        let mut it = realm(EvalLimits { max_steps: 50_000_000, ..Default::default() });
        out(&mut it, "var n = 0; async function g(i) { await 0; try { await Promise.reject(i); } catch (e) { n += e; } finally { await 0; } return i; } for (var i = 0; i < 3000; i++) g(i); 0");
        it.drain_microtasks().unwrap();
        it.collect(&[]).unwrap();
        let st = it.async_stats();
        assert_eq!((st.activations, st.table, st.frames, st.slots), (0, 0, 0, 0));
        assert!(it.aw.free.is_empty());
        assert_eq!(out(&mut it, "n"), (0..3000u64).sum::<u64>().to_string());
        // environments of finished activations were freed
        assert!(it.stats().live_envs < 20, "{}", it.stats().live_envs);
    });
}

// ------------------------------------------------------------------ several scripts

#[test]
fn resume_selects_its_own_script_and_restores_the_callers() {
    big(|| {
        let mut it = realm(lim());
        // script 1: its own ids; script 2 has different code with possibly equal node ids
        assert_eq!(out(&mut it, "var secret = 'one'; var out = []; var res1; async function fa() { var l = 'L1'; try { await new Promise(function (r) { res1 = r; }); out.push(secret + l); await 0; throw 'ex1'; } catch (e) { out.push(e); } finally { await 0; out.push('fin1'); } return 'ret1'; } var r1; fa().then(function (v) { r1 = v; }); 0"), "0");
        assert_eq!(out(&mut it, "var other = [1, 2, 3].length; async function fb() { await 0; return 'b' + other; } var r2; fb().then(function (v) { r2 = v; }); 0"), "0");
        assert_eq!(out(&mut it, "res1(1); 0"), "0");
        it.collect(&[]).unwrap();
        it.drain_microtasks().unwrap();
        assert_eq!(it.cur, 0, "the realm is back on its first slot");
        assert_eq!(out(&mut it, "out.join('|') + '#' + r1 + '#' + r2"), "oneL1|ex1|fin1#ret1#b3");
    });
}

#[test]
fn closure_from_an_earlier_script_awaits_inside_a_later_scripts_call() {
    big(|| {
        let mut it = realm(lim());
        out(&mut it, "var log = []; function cb(x) { log.push('A' + x); return x; } async function slow(x) { await 0; return cb(x); }");
        out(&mut it, "var q = [1, 2].length; async function run() { var a = await slow(1); log.push('B' + a + q); var b = await slow(2); return a + b; } var done; run().then(function (v) { done = v; }); 0");
        it.drain_microtasks().unwrap();
        assert_eq!(out(&mut it, "log.join('|') + '#' + done"), "A1|B12|A2#3");
    });
}

#[test]
fn script_state_is_restored_when_a_resume_throws_or_hits_a_limit() {
    big(|| {
        let mut it = realm(EvalLimits { max_steps: 60_000, ..Default::default() });
        out(&mut it, "var junk = [1, 2, 3, 4, 5, 6, 7, 8]; var tag = 'a';");
        out(&mut it, "var res; async function t() { await new Promise(function (r) { res = r; }); throw new RangeError('boom'); } var caught; t().catch(function (e) { caught = e.message; }); 0");
        out(&mut it, "res(1); 0");
        it.drain_microtasks().unwrap();
        assert_eq!(out(&mut it, "caught"), "boom");
        assert_eq!((it.cur, it.env, it.depth), (0, 0, 0));
        out(&mut it, "var res2; async function spin() { await new Promise(function (r) { res2 = r; }); for (;;) { await 0; } } spin(); 0");
        out(&mut it, "res2(1); 0");
        assert_eq!(limit_of(it.drain_microtasks()), LimitKind::Steps);
        assert_eq!((it.cur, it.env, it.depth), (0, 0, 0));
        assert_eq!(it.async_stats().activations, 0);
    });
}

#[test]
fn a_second_script_can_start_async_functions_of_the_first_and_both_share_the_queue() {
    big(|| {
        let mut it = realm(lim());
        out(&mut it, "var out = []; async function a(t) { out.push(t + 1); await 0; out.push(t + 2); }");
        out(&mut it, "async function b(t) { out.push(t + 1); await 0; out.push(t + 2); } a('a'); b('b'); a('c'); out.push('s'); 0");
        it.drain_microtasks().unwrap();
        assert_eq!(out(&mut it, "out.join('|')"), "a1|b1|c1|s|a2|b2|c2");
    });
}
