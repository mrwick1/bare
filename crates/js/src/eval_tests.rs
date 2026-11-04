//! Evaluator tests. The big corpus (`tools/fixtures/js/eval-cases.txt`) is checked against what Node gave
//! (`eval-cases.expected`, made by `tools/js/eval_oracle.mjs`); `tools/js/eval_check.sh` re-runs Node.
//! The tests below cover what Node cannot show: Unsupported before execution, limits, host calls, roots, GC.

use super::*;
use crate::heap::{Heap, HeapLimits};
use crate::parse::{parse_script, Limits};

/// Tests run on a small stack by default. A JS call needs a few KiB of native stack, so use a big one.
fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\n', "\\n").replace('\r', "\\r").replace('\t', "\\t")
}

fn show(it: &Interp, v: Value) -> String {
    match v {
        Value::String(_) => format!("string:{}", esc(&it.display(v))),
        Value::Number(_) => format!("number:{}", it.display(v)),
        Value::Bool(_) => format!("boolean:{}", it.display(v)),
        Value::Undefined => "undefined".into(),
        Value::Null => "null".into(),
        Value::Symbol(_) => "symbol".into(),
        Value::Object(_) => it.display(v),
    }
}

fn run_case(src: &str) -> String {
    let prog = parse_script(src, Limits::default()).unwrap_or_else(|e| panic!("{src:?}: {e}"));
    let mut it = Interp::new(&prog, Heap::default(), EvalLimits { max_steps: 500_000, ..Default::default() }).expect("setup");
    match it.run() {
        Ok(v) => show(&it, v),
        Err(Error::Throw(v)) if matches!(v, Value::Object(_)) => {
            format!("throw:{}:{}", esc(&it.get_str(v, "name")), esc(&it.get_str(v, "message")))
        }
        Err(Error::Throw(v)) => format!("throw-value:{}", show(&it, v)),
        Err(Error::Limit(_)) => "limit".into(),
        Err(e) => format!("other:{e:?}"),
    }
}

#[test]
fn corpus_matches_node() {
    let (cases, expected) = (include_str!("../../../tools/fixtures/js/eval-cases.txt"), include_str!("../../../tools/fixtures/js/eval-cases.expected"));
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

fn setup<'a>(prog: &'a Program<'a>, limits: EvalLimits) -> Interp<'a> {
    Interp::new(prog, Heap::default(), limits).expect("setup")
}

#[test]
fn unsupported_is_found_before_anything_runs() {
    // Each script would set `ran = 1` first. `Interp::new` must fail, so nothing can run.
    let cases = [
        ("class A {}", "class"),
        ("var a = class {};", "class"),
        ("async function f() {}", "async function"),
        ("var f = async () => 1;", "async function"),
        ("function g() { return await_it; } function h() { class B {} }", "class"),
        ("tag`x`", "tagged template"),
        ("var r = /a(?<=b)/;", "lookbehind"),
        ("var b = 10n;", "BigInt literal"),
        ("var o = { m() { return super.x; } };", "super"),
        ("function f() { return new.target; }", "new.target"),
        ("with ({}) {}", "with statement"),
        ("function f() { try { } finally { class C {} } }", "class"),
    ];
    for (src, what) in cases {
        let prog = parse_script(src, Limits::default()).unwrap_or_else(|e| panic!("{src}: {e}"));
        match Interp::new(&prog, Heap::default(), EvalLimits::default()) {
            Err(Error::Unsupported(u)) => {
                assert_eq!(u.what, what, "{src}");
                assert!(u.span.end > u.span.start, "{src}: span");
            }
            Err(e) => panic!("{src}: {e:?}"),
            Ok(_) => panic!("{src}: accepted"),
        }
    }
    // Generators and modules are rejected by the parser as Unsupported: they never reach the evaluator.
    let e = parse_script("function* g() {}", Limits::default()).unwrap_err();
    assert_eq!(e.kind, crate::parse::ErrorKind::Unsupported);
}

#[test]
fn missing_builtins_are_reference_errors_not_silent() {
    assert_eq!(run_case("typeof Map + typeof WeakMap + typeof Symbol"), "string:undefinedundefinedundefined");
    assert_eq!(run_case("new Map()"), "throw:ReferenceError:Map is not defined");
    assert_eq!(run_case("new String('x')"), "other:Unsupported(Unsupported { what: \"wrapper object (new String/Number/Boolean)\", span: Span { start: 0, end: 0 } })");
    assert_eq!(run_case("Function('return 1')"), "other:Unsupported(Unsupported { what: \"Function constructor (dynamic code)\", span: Span { start: 0, end: 0 } })");
}

fn limit_of(src: &str, limits: EvalLimits, heap: Heap) -> LimitKind {
    let src = src.to_string();
    big(move || {
        let prog = parse_script(&src, Limits::default()).unwrap();
        let mut it = Interp::new(&prog, heap, limits).unwrap();
        match it.run() {
            Err(Error::Limit(k)) => k,
            other => panic!("{src}: {:?}", other.map(|v| it.display(v))),
        }
    })
}

#[test]
fn limits_are_distinct_and_not_catchable() {
    let small = EvalLimits { max_steps: 100_000, ..Default::default() };
    assert_eq!(limit_of("while (true) {}", small, Heap::default()), LimitKind::Steps);
    assert_eq!(limit_of("for (;;) {}", small, Heap::default()), LimitKind::Steps);
    // try/catch/finally cannot swallow a limit
    assert_eq!(limit_of("try { for (;;) {} } catch (e) { 1 } finally { 2 }", small, Heap::default()), LimitKind::Steps);
    assert_eq!(limit_of("function f() { return f(); } try { f(); } catch (e) { 1 }", small, Heap::default()), LimitKind::CallDepth);
    // a bound-function chain is a call depth too, so it cannot overflow the Rust stack
    assert_eq!(limit_of("var f = function() {}; for (var i = 0; i < 500; i++) f = f.bind(null); f()", EvalLimits::default(), Heap::default()), LimitKind::CallDepth);
    let envs = EvalLimits { max_envs: 50, ..Default::default() };
    assert_eq!(limit_of("var keep = []; function mk(n) { return function() { return n; }; } for (var i = 0; i < 1000; i++) keep.push(mk(i));", envs, Heap::default()), LimitKind::Envs);
    let intrinsic_functions = big(|| Interp::realm(Heap::default(), EvalLimits::default()).unwrap().stats().functions);
    let funcs = EvalLimits { max_functions: intrinsic_functions + 100, ..Default::default() };
    assert_eq!(limit_of("var keep = []; for (var i = 0; i < 1000; i++) keep.push(function() {});", funcs, Heap::default()), LimitKind::Functions);
    let heap = Heap::new(HeapLimits { max_bytes: 1 << 20, ..Default::default() }).unwrap();
    assert!(matches!(limit_of("var s = 'xxxxxxxx'; for (;;) s += s;", EvalLimits::default(), heap), LimitKind::Heap(_)));
}

#[test]
fn a_limit_is_not_undefined() {
    // The completion of a limited run is an error, never a value.
    big(|| {
        let prog = parse_script("var i = 0; while (true) i++;", Limits::default()).unwrap();
        let mut it = setup(&prog, EvalLimits { max_steps: 1000, ..Default::default() });
        assert!(matches!(it.run(), Err(Error::Limit(LimitKind::Steps))));
    });
}

fn global_get(it: &mut Interp, name: &str) -> Value {
    let k = it.heap.key_from_str(name).unwrap();
    match it.heap.get(it.global_object(), k).unwrap() {
        Get::Value(v) => v,
        _ => panic!("accessor"),
    }
}

#[test]
fn host_calls_a_callback_and_keeps_state() {
    big(|| {
        let src = "var n = 0; function cb(x) { n += x; return n; } var obj = { m: function(y) { return this.k + y; }, k: 100 };";
        let prog = parse_script(src, Limits::default()).unwrap();
        let mut it = setup(&prog, EvalLimits::default());
        it.run().unwrap();
        let cb = global_get(&mut it, "cb");
        assert!(matches!(it.call(cb, Value::Undefined, &[Value::Number(2.0)]), Ok(Value::Number(n)) if n == 2.0));
        assert!(matches!(it.call(cb, Value::Undefined, &[Value::Number(3.0)]), Ok(Value::Number(n)) if n == 5.0));
        let obj = global_get(&mut it, "obj");
        let km = it.heap.key_from_str("m").unwrap();
        let Get::Value(m) = it.heap.get(match obj { Value::Object(o) => o, _ => unreachable!() }, km).unwrap() else { panic!() };
        assert!(matches!(it.call(m, obj, &[Value::Number(1.0)]), Ok(Value::Number(n)) if n == 101.0));
        // not callable: a TypeError value, not a crash
        assert!(matches!(it.call(Value::Number(1.0), Value::Undefined, &[]), Err(Error::Throw(_))));
        // a callback can throw
        let prog2 = parse_script("function bad() { throw new RangeError('x'); }", Limits::default()).unwrap();
        let mut it2 = setup(&prog2, EvalLimits::default());
        it2.run().unwrap();
        let bad = global_get(&mut it2, "bad");
        let Err(Error::Throw(v)) = it2.call(bad, Value::Undefined, &[]) else { panic!() };
        assert_eq!(it2.get_str(v, "name"), "RangeError");
    });
}

#[test]
fn each_host_call_gets_its_own_step_budget() {
    big(|| {
        let prog = parse_script("function spin(n) { var i = 0; while (i < n) i++; return i; }", Limits::default()).unwrap();
        let mut it = setup(&prog, EvalLimits { max_steps: 10_000, ..Default::default() });
        it.run().unwrap();
        let spin = global_get(&mut it, "spin");
        for _ in 0..5 {
            assert!(matches!(it.call(spin, Value::Undefined, &[Value::Number(500.0)]), Ok(Value::Number(n)) if n == 500.0));
        }
        assert!(matches!(it.call(spin, Value::Undefined, &[Value::Number(1e9)]), Err(Error::Limit(LimitKind::Steps))));
    });
}

#[test]
fn collect_keeps_what_roots_reach_and_frees_the_rest() {
    big(|| {
        // 2000 closures: only the last is kept. Each call env is captured by its closure.
        let src = "var keep; var data = { s: 'text' + 1 };
            function counter() { var c = 0; return function() { c++; return c + data.s; }; }
            for (var i = 0; i < 2000; i++) { keep = counter(); }
            keep(); keep();";
        let prog = parse_script(src, Limits::default()).unwrap();
        let mut it = setup(&prog, EvalLimits::default());
        let intrinsic_functions = it.stats().functions;
        it.run().unwrap();
        let before = (it.stats(), it.heap.stats());
        assert!(before.0.live_envs > 2000, "{:?}", before.0);
        let freed = it.collect(&[]).unwrap();
        let after = (it.stats(), it.heap.stats());
        assert!(freed.objects > 1000, "{freed:?}");
        assert!(after.0.live_envs < 10, "{:?}", after.0);
        assert!(after.0.functions <= intrinsic_functions + 2, "{:?}", after.0);
        assert!(after.1.live_objects < before.1.live_objects / 10);
        // the survivor still has its closure state, its strings and the shared data object
        let keep = global_get(&mut it, "keep");
        let r = it.call(keep, Value::Undefined, &[]).unwrap();
        assert_eq!(it.display(r), "3text1");
        // collecting again, and collecting while holding a result, is fine
        it.collect(&[r]).unwrap();
        assert_eq!(it.display(r), "3text1");
        let r2 = it.call(keep, Value::Undefined, &[]).unwrap();
        assert_eq!(it.display(r2), "4text1");
    });
}

#[test]
fn a_value_not_in_roots_is_stale_after_collect() {
    big(|| {
        let prog = parse_script("var tmp = function() {}; var obj = { f: function() { return 1; } };", Limits::default()).unwrap();
        let mut it = setup(&prog, EvalLimits::default());
        it.run().unwrap();
        let tmp = global_get(&mut it, "tmp");
        // drop the only reference
        let k = it.heap.key_from_str("tmp").unwrap();
        let g = it.global_object();
        it.heap.set(g, k, Value::Undefined).unwrap();
        it.collect(&[]).unwrap();
        // the handle is stale; the evaluator refuses it instead of reading a reused slot
        assert!(matches!(it.call(tmp, Value::Undefined, &[]), Err(Error::Throw(_)) | Err(Error::Heap(_))));
        // the others still work
        let obj = global_get(&mut it, "obj");
        let Value::Object(o) = obj else { panic!() };
        let kf = it.heap.key_from_str("f").unwrap();
        let Get::Value(f) = it.heap.get(o, kf).unwrap() else { panic!() };
        assert!(matches!(it.call(f, obj, &[]), Ok(Value::Number(n)) if n == 1.0));
    });
}

#[test]
fn roots_cover_every_owned_value() {
    big(|| {
        let src = "var a = 'lit'; var o = { k: [1, 'two'] }; function f(p) { var local = 'in env'; return function() { return local + p; }; } var g = f('P').bind(null);";
        let prog = parse_script(src, Limits::default()).unwrap();
        let mut it = setup(&prog, EvalLimits::default());
        it.run().unwrap();
        // collecting with exactly `roots()` (the host way) must not kill anything the evaluator needs
        let roots = it.roots();
        it.heap.collect(&roots).unwrap();
        let g = global_get(&mut it, "g");
        let r = it.call(g, Value::Undefined, &[]).unwrap();
        assert_eq!(it.display(r), "in envP");
    });
}

#[test]
fn run_twice_is_refused() {
    big(|| {
        let prog = parse_script("1", Limits::default()).unwrap();
        let mut it = setup(&prog, EvalLimits::default());
        it.run().unwrap();
        assert!(matches!(it.run(), Err(Error::Unsupported(_))));
    });
}

#[test]
fn environments_of_uncaptured_scopes_are_freed_at_once() {
    big(|| {
        // No closure is created, so no environment may stay: a loop of a million calls and blocks.
        let src = "function f(a) { { let b = a; return b + 1; } } var s = 0; for (let i = 0; i < 100000; i++) { let t = f(i); s += t; } s";
        let prog = parse_script(src, Limits::default()).unwrap();
        let mut it = setup(&prog, EvalLimits { max_envs: 8, ..Default::default() });
        let v = it.run().unwrap();
        assert_eq!(it.display(v), "5000050000");
        assert!(it.stats().live_envs <= 1, "{:?}", it.stats());
    });
}

#[test]
fn strings_and_keys_survive_collect_even_when_only_the_cache_holds_them() {
    big(|| {
        let src = "function f(o) { return o.alpha + o['be' + 'ta'] + 'lit'; } var o = { alpha: 'A', beta: 'B' };";
        let prog = parse_script(src, Limits::default()).unwrap();
        let mut it = setup(&prog, EvalLimits::default());
        it.run().unwrap();
        it.collect(&[]).unwrap();
        let (f, o) = (global_get(&mut it, "f"), global_get(&mut it, "o"));
        let r = it.call(f, Value::Undefined, &[o]).unwrap();
        assert_eq!(it.display(r), "ABlit");
    });
}

/// Every snippet the parser accepts (about 2000) goes through `Interp::new` and `run` with small limits. The
/// outcome can be anything (most are not meant to run); a panic, a hang or a heap fault (a bug) is a failure.
#[test]
fn parser_corpus_never_panics_or_faults() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tools/fixtures/js/");
    let cases = std::fs::read_to_string(format!("{dir}parse-cases.txt")).unwrap();
    big(move || {
        let (mut ran, mut unsupported, mut threw, mut limited) = (0, 0, 0, 0);
        for c in cases.split("\n%%\n") {
            let Ok(prog) = parse_script(c, Limits::default()) else { continue };
            let limits = EvalLimits { max_steps: 20_000, max_call_depth: 50, max_envs: 1000, max_functions: 1000, ..Default::default() };
            match Interp::new(&prog, Heap::default(), limits) {
                Err(Error::Unsupported(_)) => unsupported += 1,
                Err(e) => panic!("{c:?}: setup {e:?}"),
                Ok(mut it) => match it.run() {
                    Ok(_) => ran += 1,
                    Err(Error::Throw(_)) | Err(Error::Unsupported(_)) => threw += 1,
                    Err(Error::Limit(_)) => limited += 1,
                    Err(Error::Heap(e)) => panic!("{c:?}: heap fault {e:?}"),
                },
            }
        }
        eprintln!("corpus: {ran} ran, {threw} threw or hit a runtime Unsupported, {limited} limited, {unsupported} unsupported before running");
        assert!(ran > 100);
    });
}

#[test]
fn sparse_native_loops_consume_steps() {
    big(|| {
        for source in ["Array(1000).forEach(function() {});", "Array(1000).join('');"] {
            let prog = parse_script(source, Limits::default()).unwrap();
            let mut it = Interp::new(&prog, Heap::default(), EvalLimits { max_steps: 100, ..Default::default() }).unwrap();
            assert!(matches!(it.run(), Err(Error::Limit(LimitKind::Steps))), "native loop escaped budget: {source}");
        }
    });
}

#[test]
fn preflight_cannot_recurse_through_unbounded_flat_trees() {
    big(|| {
        let source = "1+".repeat(1000) + "1";
        let prog = parse_script(&source, Limits::default()).unwrap();
        assert!(matches!(Interp::new(&prog, Heap::default(), EvalLimits::default()), Err(Error::Limit(_))));
    });
}

#[test]
fn generic_array_methods_preserve_property_rules() {
    big(|| {
        assert_eq!(run_case("var a={length:1}; Object.defineProperty(a,'0',{value:7,configurable:false}); var result; try { Array.prototype.pop.call(a); } catch(e) { result=e.name; } result;"), "string:TypeError");
        assert_eq!(run_case("var a={length:4294967295}; Array.prototype.push.call(a,7); a[4294967295] + ':' + a.length;"), "string:7:4294967296");
    });
}

#[test]
fn string_builders_stop_at_the_heap_string_cap() {
    big(|| {
        let prog = parse_script("Array(1000).join('xxxxxxxx');", Limits::default()).unwrap();
        let heap = Heap::new(HeapLimits { max_string_units: 64, ..Default::default() }).unwrap();
        let mut it = Interp::new(&prog, heap, EvalLimits { max_steps: 100, ..Default::default() }).unwrap();
        assert!(matches!(it.run(), Err(Error::Limit(LimitKind::Heap(Limit::StringLength)))));
    });
}

#[test]
fn call_depth_cannot_be_unbounded_and_zero_envs_are_rejected() {
    big(|| {
        let prog = parse_script("function f() { return f(); } f();", Limits::default()).unwrap();
        let mut it = Interp::new(&prog, Heap::default(), EvalLimits { max_call_depth: u32::MAX, ..Default::default() }).unwrap();
        assert!(matches!(it.run(), Err(Error::Limit(LimitKind::CallDepth | LimitKind::TraversalDepth))));
        assert!(matches!(Interp::new(&prog, Heap::default(), EvalLimits { max_envs: 0, ..Default::default() }), Err(Error::Limit(LimitKind::Envs))));
    });
}
