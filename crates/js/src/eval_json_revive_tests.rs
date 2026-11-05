//! Tests of `JSON.parse` with a callable reviver (`eval_json_revive.rs`). The Node oracle corpus is
//! `tools/fixtures/js/revive-cases.txt` (run by `tools/js/revive_check.sh`). These tests cover what Node cannot
//! show: limits, work budgets, call depth, collect between calls, sticky failure in promise jobs, cyclic mutation.

use super::*;
use crate::heap::{Heap, HeapLimits};
use crate::json::LimitKind as JL;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn show(it: &mut Interp, r: Result<Value, Error>) -> String {
    match r {
        Ok(v) => it.display(v),
        Err(Error::Throw(v)) if matches!(v, Value::Object(_)) => format!("throw:{}:{}", it.get_str(v, "name"), it.get_str(v, "message")),
        Err(Error::Limit(l)) => format!("limit:{l:?}"),
        Err(e) => format!("other:{e:?}"),
    }
}

fn realm(el: EvalLimits, bl: BuiltinLimits, heap: Heap) -> Interp<'static> {
    let mut it = Interp::realm(heap, el).expect("realm");
    it.set_builtin_limits(bl);
    it
}

fn out(it: &mut Interp, src: &str) -> String {
    let r = it.run_script(src);
    show(it, r)
}

fn el() -> EvalLimits {
    EvalLimits { max_steps: 5_000_000, ..Default::default() }
}

fn bl(f: impl FnOnce(&mut BuiltinLimits)) -> BuiltinLimits {
    let mut b = BuiltinLimits::default();
    f(&mut b);
    b
}

fn run_with(src: &str, el: EvalLimits, bl: BuiltinLimits) -> String {
    let src = src.to_string();
    big(move || {
        let mut it = realm(el, bl, Heap::default());
        out(&mut it, &src)
    })
}

fn run(src: &str) -> String {
    run_with(src, el(), BuiltinLimits::default())
}

fn lim(k: JL) -> String {
    format!("limit:Json({k:?})")
}

#[test]
fn basics_without_node() {
    assert_eq!(run("JSON.stringify(JSON.parse('{\"a\":[1,2],\"b\":3}', function (k, v) { return typeof v === 'number' ? v * 2 : v; }))"), r#"{"a":[2,4],"b":6}"#);
    assert_eq!(run("var l = []; JSON.parse('{\"a\":[1],\"b\":2}', function (k, v) { l.push(k); return v; }); l.join()"), "0,a,b,");
    assert_eq!(run("typeof JSON.parse('1', function () {})"), "undefined");
}

#[test]
fn the_reviver_gets_two_arguments_and_the_context_gate_is_not_faked() {
    // ES2022 has no third `context` argument; a fabricated one would be wrong. Absent on purpose.
    assert_eq!(run("var n = []; JSON.parse('[1]', function () { n.push(arguments.length); }); n.join()"), "2,2");
}

#[test]
fn caps_are_distinct_limits_and_not_catchable() {
    let wrap = |body: &str| format!("try {{ {body} }} catch (e) {{ 'caught' }}");
    let id = "function (k, v) { return v; }";
    // exactly at the caps passes: three values, two open containers
    let body = |extra: &str| wrap(&format!("JSON.parse('[1,2,3]', function (k, v) {{ {extra} return v; }}).length"));
    assert_eq!(run_with(&body(""), el(), bl(|b| b.json.max_nodes = 4)), "3");
    assert_eq!(run_with(&wrap(&format!("JSON.parse('[[1]]', {id}).length")), el(), bl(|b| b.json.max_depth = 2)), "1");
    // one value more than max_nodes: a distinct limit, `catch` does not see it
    assert_eq!(run_with(&body(""), el(), bl(|b| b.json.max_nodes = 3)), lim(JL::Nodes));
    assert_eq!(run_with(&wrap(&format!("JSON.parse('[[1]]', {id})")), el(), bl(|b| b.json.max_depth = 1)), lim(JL::Depth));
    // a callback builds a deep chain that the walk then reaches
    let src = wrap("JSON.parse('{\"a\":0,\"b\":{}}', function (k, v) { if (k === 'a') { var d = {}, t = d; for (var i = 0; i < 20; i++) { t.x = {}; t = t.x; } this.b = d; } return v; })");
    assert_eq!(run_with(&src, el(), bl(|b| b.json.max_depth = 8)), lim(JL::Depth));
    // a huge array made by a callback is refused before any visit
    let src = wrap("JSON.parse('[0,[]]', function (k, v) { if (k === '0') this[1].length = 4000000000; return v; })");
    assert_eq!(run_with(&src, el(), BuiltinLimits::default()), lim(JL::Nodes));
    // an object with many keys is refused before its key list is filtered
    let src = wrap("JSON.parse('{\"a\":0,\"b\":{}}', function (k, v) { if (k === 'a') for (var i = 0; i < 100; i++) this.b['k' + i] = i; return v; })");
    assert_eq!(run_with(&src, el(), bl(|b| b.json.max_nodes = 50)), lim(JL::Nodes));
}

#[test]
fn work_is_charged_to_the_step_budget() {
    let build = "var s = '[' + new Array(2000).join('1,') + '1]'; ";
    let base = big(move || {
        let mut it = realm(el(), BuiltinLimits::default(), Heap::default());
        out(&mut it, build);
        out(&mut it, "JSON.parse(s)");
        it.stats().steps
    });
    let tight = EvalLimits { max_steps: base + 1000, ..Default::default() };
    assert_eq!(run_with(&format!("{build} JSON.parse(s).length"), tight, BuiltinLimits::default()), "2000");
    // each visit costs steps, so a trivial reviver over 2000 elements runs out
    assert_eq!(run_with(&format!("{build} JSON.parse(s, function (k, v) {{ return v; }})"), tight, BuiltinLimits::default()), "limit:Steps");
    // key materialization is charged: a mutation-built object with many keys, no reviver call per key needed
    let src = "JSON.parse('{\"a\":0,\"b\":{}}', function (k, v) { if (k === 'a') for (var i = 0; i < 3000; i++) this.b['k' + i] = i; return v; })";
    assert_eq!(run_with(src, EvalLimits { max_steps: 12_000, ..Default::default() }, BuiltinLimits::default()), "limit:Steps");
}

#[test]
fn heap_limits_surface_as_limits() {
    big(|| {
        let heap = Heap::new(HeapLimits { max_objects: 3000, ..Default::default() }).unwrap();
        let mut it = realm(el(), BuiltinLimits::default(), heap);
        let r = out(&mut it, "try { JSON.parse('[0]', function (k, v) { for (var i = 0; i < 5000; i++) ({}); return v; }) } catch (e) { 'caught' }");
        assert!(r.starts_with("limit:Heap"), "{r}");
    });
}

#[test]
fn a_limit_in_a_callback_runs_no_catch_or_finally_and_leaves_the_realm_usable() {
    big(|| {
        let mut it = realm(el(), BuiltinLimits::default(), Heap::default());
        let src = "var log = []; function rec() { return JSON.parse('[1]', function (k, v) { return rec(); }); } try { rec(); } catch (e) { log.push('catch'); } finally { log.push('finally'); } log.join()";
        assert_eq!(out(&mut it, src), "limit:CallDepth");
        // neither the outer catch/finally nor the reviver frames ran: `log` is empty
        assert_eq!(out(&mut it, "log.length"), "0");
        assert_eq!(out(&mut it, "JSON.stringify(JSON.parse('{\"a\":[1]}', function (k, v) { return v; }))"), r#"{"a":[1]}"#);
        // inside the reviver: its own finally must not run after the fault
        let src = "var ran = 0; try { JSON.parse('[1,2]', function (k, v) { try { (function f() { f(); })(); } finally { ran = 1; } return v; }); } catch (e) { ran = 2; }";
        assert!(out(&mut it, src).starts_with("limit:"));
        assert_eq!(out(&mut it, "ran"), "0");
    });
}

#[test]
fn deep_data_is_iterative_and_collect_between_calls_is_safe() {
    // 1000 nested arrays exceed the 512 tree-walk frames but not max_depth; the walk is iterative
    assert_eq!(run("var s = '', i; for (i = 0; i < 1000; i++) s += '['; for (i = 0; i < 1000; i++) s += ']'; var n = 0; JSON.parse(s, function (k, v) { n++; return v; }); n"), "1000");
    big(|| {
        let mut it = realm(el(), BuiltinLimits::default(), Heap::default());
        out(&mut it, "var keep = JSON.parse('{\"a\":[1,{\"b\":\"x\"}]}', function (k, v) { return v; });");
        for _ in 0..3 {
            out(&mut it, "for (var i = 0; i < 50; i++) JSON.parse('{\"i\":[' + i + ',{\"p\":1}]}', function (k, v) { return v; });");
        }
        let before = it.heap.stats().live_objects;
        it.collect(&[]).unwrap();
        let after = it.heap.stats().live_objects;
        assert!(after < before, "temporaries are collectable: {before} -> {after}");
        assert_eq!(out(&mut it, "JSON.stringify(keep)"), r#"{"a":[1,{"b":"x"}]}"#);
        assert_eq!(out(&mut it, "JSON.stringify(JSON.parse('[1,{\"c\":2}]', function (k, v) { return v; }))"), r#"[1,{"c":2}]"#);
    });
}

#[test]
fn a_limit_inside_a_promise_job_is_sticky() {
    big(|| {
        let mut it = realm(el(), bl(|b| b.json.max_nodes = 2), Heap::default());
        it.run_script("var done = 0; Promise.resolve().then(function () { JSON.parse('[1,2]', function (k, v) { return v; }); done = 1; });").unwrap();
        let first = it.drain_microtasks();
        assert!(matches!(first, Err(Error::Limit(LimitKind::Json(JL::Nodes)))), "{first:?}");
        // a later drain repeats the failure instead of reporting an empty checkpoint
        assert!(matches!(it.drain_microtasks(), Err(Error::Limit(LimitKind::Json(JL::Nodes)))));
        assert_eq!(out(&mut it, "done"), "0");
    });
    // a JS throw in a reviver inside a job only rejects that job's promise
    big(|| {
        let mut it = realm(el(), BuiltinLimits::default(), Heap::default());
        it.run_script("var r = ''; Promise.resolve().then(function () { JSON.parse('[1]', function () { throw new Error('x'); }); }).catch(function (e) { r = e.message; });").unwrap();
        it.drain_microtasks().unwrap();
        assert_eq!(out(&mut it, "r"), "x");
    });
}

#[test]
fn cyclic_values_made_by_callbacks_are_bounded() {
    // a callback links a later sibling back to an ancestor; the walk would never end: depth ends it
    let src = "try { JSON.parse('{\"a\":{\"b\":1,\"c\":2}}', function (k, v) { if (k === 'b') this.c = this; return v; }); 'done' } catch (e) { 'caught' }";
    assert_eq!(run_with(src, el(), bl(|b| b.json.max_depth = 16)), lim(JL::Depth));
    // a shallow cycle re-entered again and again (the outer array is walked anew each time): visits end it
    let src = "try { JSON.parse('[[0],[0]]', function (k, v) { if (k === '0' && Array.isArray(v)) this[1] = this; return v; }); 'done' } catch (e) { 'caught' }";
    assert_eq!(run_with(src, el(), bl(|b| b.json.max_nodes = 200)), lim(JL::Nodes));
    // with the default caps the step budget or a json cap still ends it, never a hang
    let r = run_with(src, el(), BuiltinLimits::default());
    assert!(r == lim(JL::Nodes) || r == lim(JL::Depth) || r == "limit:Steps", "{r}");
    // a cycle that the walk does not reach again is legal
    assert_eq!(run("var r = JSON.parse('{\"a\":{\"b\":1}}', function (k, v) { return k === 'b' ? this : v; }); r.a.b === r.a"), "true");
}

#[test]
fn call_depth_guard_applies_to_reentrant_parse() {
    big(|| {
        let mut it = realm(el(), BuiltinLimits::default(), Heap::default());
        let src = "function mk() { return JSON.parse('[1]', function (k, v) { return mk(); }); } mk()";
        assert_eq!(out(&mut it, src), "limit:CallDepth");
        assert_eq!(out(&mut it, "JSON.stringify(JSON.parse('[1,[2]]', function (k, v) { return v; }))"), "[1,[2]]");
    });
}

#[test]
fn callbacks_from_two_scripts_run_their_own_code() {
    big(|| {
        let mut it = realm(el(), BuiltinLimits::default(), Heap::default());
        out(&mut it, "function revA(k, v) { return typeof v === 'number' ? v + [1, 2].length : v; } var tag = 'a';");
        let r = out(&mut it, "function revB(k, v) { return typeof v === 'number' ? v * [1, 2, 3].length : v; } JSON.stringify([JSON.parse('[1,[2]]', revA), JSON.parse('[1,[2]]', revB)])");
        assert_eq!(r, "[[3,[4]],[3,[6]]]");
    });
}
