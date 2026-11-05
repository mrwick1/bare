//! Generator tests. The corpus (`tools/fixtures/js/generator-cases.txt`) is checked against what Node gave
//! (`generator-cases.expected`, made by `tools/js/eval_oracle.mjs`; `tools/js/generator_check.sh` re-runs Node).
//! The tests below cover what Node cannot show: GC of suspended runs, caps and the Rust stack.

use super::*;
use crate::heap::Heap;
use crate::parse::{parse_script, Limits};

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\n', "\\n").replace('\r', "\\r").replace('\t', "\\t")
}

fn show(it: &mut Interp, r: Result<Value, Error>) -> String {
    match r {
        Ok(Value::String(s)) => format!("string:{}", esc(&it.display(Value::String(s)))),
        Ok(v @ Value::Number(_)) => format!("number:{}", it.display(v)),
        Ok(v @ Value::Bool(_)) => format!("boolean:{}", it.display(v)),
        Ok(Value::Undefined) => "undefined".into(),
        Ok(Value::Null) => "null".into(),
        Ok(v) => it.display(v),
        Err(Error::Throw(v)) if matches!(v, Value::Object(_)) => format!("throw:{}:{}", esc(&it.get_str(v, "name")), esc(&it.get_str(v, "message"))),
        Err(Error::Throw(v)) => format!("throw-value:{}", it.display(v)),
        Err(Error::Limit(l)) => format!("limit:{l:?}"),
        Err(Error::Unsupported(u)) => format!("unsupported:{}", u.what),
        Err(e) => format!("other:{e:?}"),
    }
}

fn realm(limits: EvalLimits) -> Interp<'static> {
    Interp::realm(Heap::default(), limits).expect("realm")
}

fn out(it: &mut Interp, src: &str) -> String {
    let r = it.run_script(src);
    show(it, r)
}

#[test]
fn corpus_matches_node() {
    let (cases, expected) = (include_str!("../../../tools/fixtures/js/generator-cases.txt"), include_str!("../../../tools/fixtures/js/generator-cases.expected"));
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
            let src = lines.join("\n");
            let prog = parse_script(src.trim_end_matches('\n'), Limits::default()).unwrap_or_else(|e| panic!("{name}: {e}"));
            let mut it = Interp::new(&prog, Heap::default(), EvalLimits { max_steps: 500_000, ..Default::default() }).expect("setup");
            let r = it.run();
            let got = format!("{name}\t{}", show(&mut it, r));
            if got != w {
                bad.push(format!("node: {w}\nbare: {got}"));
            }
        }
        assert!(bad.is_empty(), "{} differences:\n{}", bad.len(), bad.join("\n"));
    });
}

#[test]
fn a_suspended_generator_keeps_its_state_across_collects_and_dead_ones_are_freed() {
    big(|| {
        let mut it = realm(EvalLimits::default());
        let src = "
            function* g(tag) {
              var local = { t: tag };
              for (const x of [1, 2, 3]) {
                try { let s = { v: x }; yield local.t + s.v; } finally { local.t += '!'; }
              }
            }
            var keep = g('k');
            for (var i = 0; i < 1000; i++) { var d = g('d'); d.next(); }
            d = null;
            keep.next().value";
        assert_eq!(out(&mut it, src), "string:k1");
        let before = it.async_stats();
        assert_eq!(before.generators, 1001);
        it.collect(&[]).unwrap();
        let st = it.async_stats();
        assert_eq!(st.generators, 1, "only `keep` is left");
        assert!(st.frames < before.frames / 100 && st.slots < before.slots / 100, "{before:?} -> {st:?}");
        assert_eq!(out(&mut it, "[keep.next().value, keep.next().value, keep.next().done].join()"), "string:k!2,k!!3,true");
        it.collect(&[]).unwrap();
        let st = it.async_stats();
        assert_eq!((st.frames, st.slots), (0, 0));
        out(&mut it, "keep = null");
        it.collect(&[]).unwrap();
        assert_eq!(it.async_stats().generators, 0);
        assert_eq!(it.stats().live_envs, { it.collect(&[]).unwrap(); it.stats().live_envs });
    });
}

#[test]
fn deep_yield_star_hits_the_call_depth_ceiling_not_the_rust_stack() {
    big(|| {
        let mut it = realm(EvalLimits { max_call_depth: 200, ..Default::default() });
        let r = it.run_script("function* r(n) { if (n) yield* r(n - 1); else yield 'leaf'; } r(50).next().value");
        assert_eq!(show(&mut it, r), "string:leaf");
        let r = it.run_script("r(100000).next()");
        assert_eq!(show(&mut it, r), "limit:CallDepth");
        // The generator that hit the limit is done, and the realm still works.
        assert_eq!(out(&mut it, "[...r(3)].join()"), "string:leaf");
    });
}

#[test]
fn frames_of_suspended_generators_count_in_the_frame_cap() {
    big(|| {
        let mut it = realm(EvalLimits::default());
        it.set_async_limits(AsyncLimits { max_frames: 50, ..Default::default() });
        let r = it.run_script("function* g() { for (const x of [1]) { try { yield x; } finally {} } } var all = []; for (var i = 0; i < 100; i++) { var x = g(); x.next(); all.push(x); }");
        assert_eq!(show(&mut it, r), "limit:AsyncFrames");
    });
}

#[test]
fn for_of_over_a_generator_makes_no_result_objects() {
    big(|| {
        let mut it = realm(EvalLimits::default());
        out(&mut it, "function* g(n) { for (var i = 0; i < n; i++) yield i; } var s = 0;");
        let a = it.heap.stats().total_object_allocs;
        assert_eq!(out(&mut it, "for (var x of g(1000)) s += x; s"), "number:499500");
        let n = it.heap.stats().total_object_allocs - a;
        assert!(n < 20, "{n} objects for 1000 values");
        // A `next` that is not the intrinsic is called, and its result objects are read.
        out(&mut it, "var GP = Object.getPrototypeOf(g.prototype); var orig = GP.next; var calls = 0; GP.next = function (v) { calls++; return orig.call(this, v); };");
        assert_eq!(out(&mut it, "s = 0; for (var x of g(10)) s += x; s + ',' + calls"), "string:45,11");
    });
}
