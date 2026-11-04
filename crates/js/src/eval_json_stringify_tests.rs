//! Tests of `JSON.stringify` (`eval_json_stringify.rs`). The Node oracle corpus is
//! `tools/fixtures/js/stringify-cases.txt` (run by `tools/js/stringify_check.sh`). These tests cover what Node cannot
//! show: limits, work budgets, callback state restoration, scripts of different origin, collect between calls.

use super::*;
use crate::heap::{Heap, HeapLimits};

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

fn realm(el: EvalLimits, sl: StringifyLimits, heap: Heap) -> Interp<'static> {
    let mut it = Interp::realm(heap, el).expect("realm");
    it.set_builtin_limits(BuiltinLimits { stringify: sl, ..Default::default() });
    it
}

fn out(it: &mut Interp, src: &str) -> String {
    let r = it.run_script(src);
    show(it, r)
}

fn el() -> EvalLimits {
    EvalLimits { max_steps: 5_000_000, ..Default::default() }
}

/// One script in a fresh realm with the given bounds.
fn run_with(src: &str, el: EvalLimits, sl: StringifyLimits) -> String {
    let src = src.to_string();
    big(move || {
        let mut it = realm(el, sl, Heap::default());
        out(&mut it, &src)
    })
}

fn run(src: &str) -> String {
    run_with(src, el(), StringifyLimits::default())
}

fn lim(k: StringifyLimit) -> String {
    format!("limit:Stringify({k:?})")
}

#[test]
fn basics_without_node() {
    assert_eq!(run("JSON.stringify({a:[1,'x\\u0000',null],b:undefined})"), r#"{"a":[1,"x\u0000",null]}"#);
    assert_eq!(run("typeof JSON.stringify(undefined)"), "undefined");
    assert_eq!(run("JSON.stringify(-0) + JSON.stringify(NaN)"), "0null");
    assert_eq!(run("JSON.stringify('\\ud800')"), r#""\ud800""#);
    assert_eq!(run("var a = {}; a.a = a; try { JSON.stringify(a) } catch (e) { e.name }"), "TypeError");
}

#[test]
fn each_bound_is_a_distinct_limit_and_not_catchable() {
    let base = StringifyLimits::default();
    let wrap = |body: &str| format!("try {{ {body} }} catch (e) {{ 'caught' }}");
    let cases: Vec<(String, StringifyLimits, StringifyLimit)> = vec![
        (wrap("JSON.stringify(new Array(51).join('ab'))"), StringifyLimits { max_output_units: 100, ..base }, StringifyLimit::Output),
        (wrap("JSON.stringify({ k: 'x' }, null, '          ')"), StringifyLimits { max_output_units: 8, ..base }, StringifyLimit::Output),
        (wrap("var a = []; for (var i = 0; i < 50; i++) a = [a]; JSON.stringify(a)"), StringifyLimits { max_depth: 10, ..base }, StringifyLimit::Depth),
        (wrap("var a = []; for (var i = 0; i < 50; i++) a.push(i); JSON.stringify(a)"), StringifyLimits { max_nodes: 20, ..base }, StringifyLimit::Nodes),
        (wrap("var o = {}; for (var i = 0; i < 50; i++) o['k' + i] = i; JSON.stringify(o)"), StringifyLimits { max_keys: 10, ..base }, StringifyLimit::Keys),
        (wrap("var a = []; a.length = 4000000000; JSON.stringify(a)"), base, StringifyLimit::Keys),
        (wrap("var r = []; r.length = 4000000000; JSON.stringify({}, r)"), base, StringifyLimit::Keys),
        (wrap("var r = []; for (var i = 0; i < 50; i++) r.push('k' + i); JSON.stringify({}, r)"), StringifyLimits { max_keys: 10, ..base }, StringifyLimit::Keys),
    ];
    for (src, sl, want) in cases {
        assert_eq!(run_with(&src, el(), sl), lim(want), "{src}");
    }
    // the same limit inside a callback is still a limit
    let src = "try { JSON.stringify({ a: { toJSON: function () { return JSON.stringify([[[[[1]]]]]); } } }) } catch (e) { 'caught' }";
    assert_eq!(run_with(src, el(), StringifyLimits { max_depth: 3, ..base }), lim(StringifyLimit::Depth));
}

#[test]
fn work_is_charged_to_the_step_budget() {
    let build = "var a = []; for (var i = 0; i < 2000; i++) a.push(i); ";
    let loop_steps = big(move || {
        let mut it = realm(el(), StringifyLimits::default(), Heap::default());
        out(&mut it, build);
        it.stats().steps
    });
    // the building loop plus slack fits, the stringify of 2000 elements (two or more steps each) does not
    let tight = EvalLimits { max_steps: loop_steps + 1000, ..Default::default() };
    assert_eq!(run_with(&format!("{build} a.length"), tight, StringifyLimits::default()), "2000");
    assert_eq!(run_with(&format!("{build} JSON.stringify(a)"), tight, StringifyLimits::default()), "limit:Steps");
    // long string escaping is charged too
    let s = "var s = ''; for (var i = 0; i < 1000; i++) s += 'abcdefghijklmnopqrstuvwxyz0123456789'; ";
    let used = {
        let mut it = realm(el(), StringifyLimits::default(), Heap::default());
        out(&mut it, s);
        let before = it.stats().steps;
        out(&mut it, "JSON.stringify(s).length");
        it.stats().steps - before
    };
    assert!(used >= 36_000 / 32, "steps {used}");
}

#[test]
fn heap_limits_surface_as_limits() {
    let src = "var s = 'abcdefgh'; for (var i = 0; i < 6; i++) s += s; try { JSON.stringify(s) } catch (e) { 'caught' }";
    big(move || {
        let heap = Heap::new(HeapLimits { max_string_units: 400, ..Default::default() }).unwrap();
        let mut it = realm(el(), StringifyLimits::default(), heap);
        let r = out(&mut it, src);
        assert!(r.starts_with("limit:Heap"), "{r}");
    });
}

#[test]
fn recursion_through_callbacks_keeps_the_call_depth_guard_and_restores_state() {
    big(|| {
        let mut it = realm(el(), StringifyLimits::default(), Heap::default());
        let src = "function mk() { return { toJSON: function () { return JSON.stringify(mk()); } }; } JSON.stringify(mk())";
        assert_eq!(out(&mut it, src), "limit:CallDepth");
        // depth, frames and the current script are restored: another script runs and succeeds
        assert_eq!(out(&mut it, "JSON.stringify({ a: [1] })"), r#"{"a":[1]}"#);
        // a replacer that throws, caught by JS, leaves nothing behind
        assert_eq!(out(&mut it, "try { JSON.stringify({ a: 1 }, function () { throw new Error('x') }) } catch (e) { e.message }"), "x");
        assert_eq!(out(&mut it, "JSON.stringify([1, { b: 2 }])"), r#"[1,{"b":2}]"#);
        // a limit in a callback of the middle of an object, then the realm is usable again
        assert_eq!(out(&mut it, "JSON.stringify({ a: 1, b: { toJSON: function () { return JSON.stringify(mk()); } } })"), "limit:CallDepth");
        assert_eq!(out(&mut it, "JSON.stringify('ok')"), r#""ok""#);
    });
}

#[test]
fn deep_data_does_not_use_the_evaluator_frame_guard() {
    // 1000 nested arrays: more than the 512 tree-walk frames, fewer than max_depth (1024). Iterative, so it works.
    let src = "var a = []; for (var i = 0; i < 1000; i++) a = [a]; JSON.stringify(a).length";
    assert_eq!(run(src), "2002");
    let over = "var a = []; for (var i = 0; i < 1100; i++) a = [a]; JSON.stringify(a)";
    assert_eq!(run(over), lim(StringifyLimit::Depth));
}

#[test]
fn callbacks_from_two_scripts_run_their_own_code() {
    big(|| {
        let mut it = realm(el(), StringifyLimits::default(), Heap::default());
        // script A: colliding node ids with B on purpose (same shape of code)
        out(&mut it, "var A = { toJSON: function (k) { return 'A' + k + [1, 2].length; } }; function repA(k, v) { return typeof v === 'number' ? v + 100 : v; } var tag = 'a';");
        let r = out(&mut it, "var B = { toJSON: function (k) { return 'B' + k + [1, 2, 3].length; } }; var tag2 = 'b'; JSON.stringify({ x: A, y: B, z: [3] }, repA)");
        assert_eq!(r, r#"{"x":"Ax2","y":"By3","z":[103]}"#);
        // the replacer of B called from a stringify started in A's closure
        out(&mut it, "function viaA(v, r) { return JSON.stringify(v, r); }");
        let r = out(&mut it, "viaA({ n: 1 }, function (k, v) { return typeof v === 'number' ? v * 7 + [0, 0].length : v; })");
        assert_eq!(r, r#"{"n":9}"#);
        // a regexp literal in a later script after the previous scripts: key by script, not by node id
        assert_eq!(out(&mut it, "JSON.stringify([/a/g.test('a'), /b/.source])"), r#"[true,"b"]"#);
    });
}

#[test]
fn nothing_survives_between_calls_and_collect_frees_the_scratch() {
    big(|| {
        let mut it = realm(el(), StringifyLimits::default(), Heap::default());
        out(&mut it, "var keep = { a: [1, { b: 'x' }] }; var s = JSON.stringify(keep, null, 1);");
        for _ in 0..3 {
            out(&mut it, "for (var i = 0; i < 50; i++) JSON.stringify({ i: i, o: { p: [i] } });");
        }
        let g = it.global_object();
        let k = it.heap.key_from_str("s").unwrap();
        let s = match it.heap.get(g, k).unwrap() {
            Get::Value(v) => v,
            _ => unreachable!(),
        };
        let before = it.heap.stats().live_objects;
        it.collect(&[s]).unwrap();
        let after = it.heap.stats().live_objects;
        assert!(after < before, "wrapper holders and temporaries are collectable: {before} -> {after}");
        // after the collect the realm still serializes, and the kept string is intact
        assert_eq!(out(&mut it, "JSON.stringify(keep) + s.length"), r#"{"a":[1,{"b":"x"}]}39"#);
        // a failed call leaves no root behind either
        assert_eq!(out(&mut it, "var c = {}; c.c = c; try { JSON.stringify(c) } catch (e) { 'ok' }"), "ok");
        it.collect(&[]).unwrap();
        assert_eq!(out(&mut it, "JSON.stringify({ z: [] })"), r#"{"z":[]}"#);
    });
}

#[test]
fn objects_created_during_a_call_are_not_collected_by_the_call() {
    // The evaluator never collects during a call, so the holder object, key strings and toJSON results stay valid.
    assert_eq!(run("JSON.stringify([{ toJSON: function () { return { n: 1 }; } }, { toJSON: function (k) { return { k: k }; } }])"), r#"[{"n":1},{"k":"1"}]"#);
}

#[test]
fn replacer_property_list_is_read_once_and_deduplicated() {
    assert_eq!(run("var n = 0; var r = ['a', 'b', 'a']; Object.defineProperty(r, 1, { get: function () { n++; r.length = 1; return 'c'; } }); JSON.stringify({ a: 1, b: 2, c: 3 }, r) + n"), r#"{"a":1,"c":3}1"#);
}
