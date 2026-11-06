//! The statement machine (`eval_statements.rs`): the separate `max_statement_frames` cap (lower, zero, clamp, cumulative over
//! nested calls), cleanup on every exit and fault, frame ownership across scripts, and scaling of finite walks. Runs on a
//! thread with the stack of a real run (8 MiB release, 64 MiB debug); fresh processes are the way to measure stack use.

use super::*;
use crate::heap::{Heap, HeapLimits};

fn real_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    let mib: usize = if cfg!(debug_assertions) { 64 } else { 8 };
    std::thread::Builder::new().stack_size(mib << 20).spawn(f).unwrap().join().unwrap()
}

fn lim(frames: u32) -> EvalLimits {
    EvalLimits { max_steps: 5_000_000, max_traversal_depth: 768, max_call_depth: 128, max_statement_frames: frames, ..Default::default() }
}

fn outcome(src: &str, limits: EvalLimits) -> String {
    let mut it = Interp::realm(Heap::default(), limits).unwrap();
    match it.run_script(src) {
        Ok(v) => it.display(v),
        Err(Error::Limit(k)) => format!("limit:{k:?}"),
        Err(e) => format!("other:{e:?}"),
    }
}

const FRAMES: &str = "limit:StatementFrames";

/// `n` nested blocks, each with a `let` (a scope frame), around `1`.
fn blocks(n: usize) -> String {
    format!("{}1{}", "{ let a = 1; ".repeat(n), " }".repeat(n))
}

fn walk(levels: usize) -> String {
    format!(
        "function a5(t) {{ for (t = t.child; t !== null;) a6(t), t = t.sibling; }}
function a6(t) {{ switch (t.tag) {{ case 0: case 1: try {{ a5(t) }} catch (e) {{}} break; default: a5(t) }} }}
var root = {{ tag: 3, child: null, sibling: null }}, tail = root;
for (var i = 0; i < {levels}; i++) {{ var n = {{ tag: 0, child: null, sibling: null }}; tail.child = n; tail = n; }}
a5(root); 'walked'"
    )
}

#[test]
fn the_frame_cap_defaults_and_clamps_in_new_and_realm() {
    assert_eq!(EvalLimits::default().max_statement_frames, 512);
    let prog = crate::parse::parse_script("1", Default::default()).unwrap();
    let it = Interp::new(&prog, Heap::default(), EvalLimits { max_statement_frames: u32::MAX, ..Default::default() }).unwrap();
    assert_eq!(it.limits.max_statement_frames, MAX_STATEMENT_FRAMES);
    let it = Interp::realm(Heap::default(), EvalLimits { max_statement_frames: u32::MAX, ..Default::default() }).unwrap();
    assert_eq!(it.limits.max_statement_frames, 65_536);
    let it = Interp::realm(Heap::default(), EvalLimits { max_statement_frames: 5, ..Default::default() }).unwrap();
    assert_eq!(it.limits.max_statement_frames, 5);
}

#[test]
fn a_lower_cap_refuses_before_the_frame_is_pushed() {
    real_stack(|| {
        // the script body is one list frame, each block one more: 11 frames for 10 blocks
        assert_eq!(outcome(&blocks(10), lim(11)), "1");
        assert_eq!(outcome(&blocks(10), lim(10)), FRAMES);
        assert_eq!(outcome(&blocks(10), lim(1)), FRAMES);
    });
}

#[test]
fn a_zero_cap_refuses_the_first_body_and_setup_still_works() {
    real_stack(|| {
        let prog = crate::parse::parse_script("1", Default::default()).unwrap();
        let mut it = Interp::new(&prog, Heap::default(), lim(0)).unwrap();
        assert!(matches!(it.run(), Err(Error::Limit(LimitKind::StatementFrames))));
        assert_eq!((it.stmt_frames, it.frames, it.depth), (0, 0, 0));
        assert_eq!(outcome("", lim(0)), FRAMES);
        // `if` is a tail step with no frame; a body still needs its list frame
        assert_eq!(outcome("if (1) 2", lim(0)), FRAMES);
        assert_eq!(outcome("if (1) 2", lim(1)), "2");
    });
}

#[test]
fn the_cap_is_clamped_to_the_ceiling_whatever_is_asked() {
    real_stack(|| {
        // one body list + seven blocks = 8 frames a call. The ceiling is 65,536 frames: 8,000 calls are 64,000 frames, 8,200 are 65,600
        let body = |n: u32| format!("function f(n) {{ {{ {{ {{ {{ {{ {{ {{ return n === 0 ? 0 : 1 + f(n - 1) }} }} }} }} }} }} }} }} f({n})");
        let wide = EvalLimits { max_call_depth: 10_000, ..lim(u32::MAX) };
        assert_eq!(outcome(&body(8_000), wide), "8000");
        assert_eq!(outcome(&body(8_200), wide), FRAMES);
        // the default of 512 is below the clamp: 60 calls are 480 frames, 70 are 560
        let d = EvalLimits { max_steps: 5_000_000, max_traversal_depth: 768, max_call_depth: 128, ..Default::default() };
        assert_eq!(outcome(&body(60), d), "60");
        assert_eq!(outcome(&body(70), d), FRAMES);
    });
}

#[test]
fn the_count_is_cumulative_over_nested_calls_and_returns_with_each_call() {
    real_stack(|| {
        // each call holds 4 frames (body list + 3 blocks) on top of the script list. One call alone needs 5; the three
        // nested calls of f(2) need 13, though no single call holds more than 4.
        let f = "function f(n) { { { { return n === 0 ? 'x' : f(n - 1) } } } }";
        assert_eq!(outcome(&format!("{f} f(0)"), lim(5)), "x");
        assert_eq!(outcome(&format!("{f} f(0)"), lim(4)), FRAMES);
        assert_eq!(outcome(&format!("{f} f(2)"), lim(13)), "x");
        assert_eq!(outcome(&format!("{f} f(2)"), lim(12)), FRAMES);
        // the same calls one after another never add up
        assert_eq!(outcome(&format!("{f} f(0); f(0); f(0); f(0); f(0); 'ok'"), lim(5)), "ok");
    });
}

#[test]
fn a_callback_from_a_native_counts_too() {
    real_stack(|| {
        // forEach calls a script function: its frames sit on top of the caller's
        let src = "function g() { { { return 1 } } } [1].forEach(function () { { g() } }); 'ok'";
        // script 1 + callback body 1 + block 1 + g body 1 + 2 blocks = 6
        assert_eq!(outcome(src, lim(6)), "ok");
        assert_eq!(outcome(src, lim(5)), FRAMES);
    });
}

/// Run `src` in a fresh realm, then check that nothing is left behind and that the realm still works.
fn run_clean(src: &str, limits: EvalLimits, heap: Heap) -> Result<String, Error> {
    let mut it = Interp::realm(heap, limits).unwrap();
    let base = it.stats().live_envs;
    let r = it.run_script(src).map(|v| it.display(v));
    assert_eq!((it.stmt_frames, it.frames, it.depth), (0, 0, 0), "counters after {src}");
    assert!(it.pending_labels.is_empty(), "labels after {src}");
    assert_eq!(it.env, 0, "env after {src}");
    assert!(!it.strict, "strictness after {src}");
    assert_eq!(it.stats().live_envs, base, "live envs after {src}");
    // the realm is usable again
    it.step_limit = u64::MAX;
    it.limits.max_steps = u64::MAX / 2;
    it.limits.max_envs = 1 << 20;
    assert_eq!(it.run_script("1 + 1").map(|v| it.display(v)).unwrap(), "2", "reuse after {src}");
    r
}

#[test]
fn every_exit_restores_env_labels_strictness_and_the_frame_count() {
    real_stack(|| {
        let l = lim(768);
        let ok = |src: &str| run_clean(src, l, Heap::default()).unwrap_or_else(|e| panic!("{src}: {e:?}"));
        // normal completion and the abrupt kinds
        assert_eq!(ok("a: { let a = 1; for (let i = 0; i < 3; i++) { switch (i) { case 0: let z = 1; break; default: { let w = 2; } } } }; 1"), "1");
        assert_eq!(ok("function f() { for (let i = 0;;) { try { { let q = 1; return q; } } finally { { let r = 2; } } } } f()"), "1");
        assert_eq!(ok("o: for (let i = 0; i < 3; i++) { for (let j = 0; j < 3; j++) { { let k = 1; continue o; } } } 2"), "2");
        assert_eq!(ok("b: { let x = 1; { let y = 2; break b; } } 3"), "3");
        assert_eq!(ok("do { let d = 1; { continue; } } while (false); 4"), "4");
        // a JS throw: caught, rethrown through a finally, and uncaught
        assert_eq!(ok("var r; try { for (let i = 0;;) { switch (1) { case 1: { let q = 1; throw 'x'; } } } } catch (e) { { let c = 1; r = e; } } finally { { let f = 1; } } r"), "x");
        assert_eq!(ok("var r; try { try { { let q = 1; throw 1 } } finally { { let f = 1; } } } catch (e) { r = e } r"), "1");
        assert!(matches!(run_clean("a: for (let i = 0;;) { switch (1) { case 1: { let q = 1; throw 'u'; } } }", l, Heap::default()), Err(Error::Throw(_))));
        // a strict callee that throws through sloppy frames
        assert_eq!(ok("function s() { 'use strict'; { let q = 1; throw 'e'; } } var r; try { { let z = 1; s(); } } catch (e) { r = e } finally { { let w = 1; } } r"), "e");
        // a catch whose binding fails and a catch parameter
        assert_eq!(ok("var r; try { try { throw 1 } catch (e) { nope } finally { { let f = 1; } } } catch (x) { r = x.name } r"), "ReferenceError");
    });
}

/// A fault in a closure-free program: no environment stays behind, and neither `catch` nor `finally` ran.
fn fault(src: &str, limits: EvalLimits, heap: Heap) -> Error {
    let mut it = Interp::realm(heap, limits).unwrap();
    let base = it.stats().live_envs;
    let full = format!("var fin = 0, cat = 0; try {{ {src} }} catch (e) {{ cat = 1 }} finally {{ fin = 1 }}");
    let e = it.run_script(&full).expect_err("a fault");
    assert_eq!((it.stmt_frames, it.frames, it.depth, it.env), (0, 0, 0, 0), "{src}");
    assert!(it.pending_labels.is_empty(), "{src}");
    assert_eq!(it.stats().live_envs, base, "live envs after {src}");
    it.collect(&[]).unwrap();
    it.limits.max_steps = 1 << 30;
    it.limits.max_envs = 1 << 20;
    assert_eq!(it.run_script("fin + ',' + cat").map(|v| it.display(v)).unwrap(), "0,0", "catch or finally ran after {src}");
    e
}

#[test]
fn resource_faults_clean_up_and_skip_catch_and_finally() {
    real_stack(|| {
        let nest = "a: for (let i = 0;;) { switch (1) { case 1: { let q = 1; for (;;) { { let z = 1; } } } } }";
        let steps = EvalLimits { max_steps: 3000, ..lim(768) };
        assert!(matches!(fault(nest, steps, Heap::default()), Error::Limit(LimitKind::Steps)));
        assert!(matches!(fault(nest, lim(6), Heap::default()), Error::Limit(LimitKind::StatementFrames)));
        assert!(matches!(fault(&blocks(20), EvalLimits { max_envs: 6, ..lim(768) }, Heap::default()), Error::Limit(LimitKind::Envs)));
        // Unsupported at run time (dynamic code), inside a scope and a try
        assert!(matches!(fault("{ let a = 1; try { { let b = 2; Function('x'); } } finally { } }", lim(768), Heap::default()), Error::Unsupported(_)));
        // a heap fault (object cap)
        let intrinsic = Interp::realm(Heap::default(), lim(768)).unwrap().heap.stats().live_objects;
        let heap = Heap::new(HeapLimits { max_objects: intrinsic + 20, ..Default::default() }).unwrap();
        let r = fault("var keep = []; for (let i = 0;; i++) { { let o = {}; keep.push(o); } }", lim(768), heap);
        assert!(matches!(r, Error::Limit(LimitKind::Heap(_))), "{r:?}");
    });
}

#[test]
fn resource_faults_do_not_run_iterator_close_but_a_throw_does() {
    real_stack(|| {
        let it = "var closed = 0; var it = { [Symbol.iterator]() { return { next() { return { value: 1, done: false }; }, return() { closed++; return {}; } }; } };";
        let run = |body: &str, limits: EvalLimits| {
            let mut r = Interp::realm(Heap::default(), limits).unwrap();
            let e = r.run_script(&format!("{it} {body}"));
            assert_eq!((r.stmt_frames, r.frames, r.depth, r.env), (0, 0, 0, 0), "{body}");
            assert!(r.pending_labels.is_empty());
            r.limits.max_steps = 1 << 30;
            let closed = r.run_script("closed").map(|v| r.display(v)).unwrap();
            (e, closed)
        };
        let (e, closed) = run("a: for (let i of it) { switch (1) { case 1: { let q = 1; for (;;) { { let z = 1; } } } } }", EvalLimits { max_steps: 3000, ..lim(768) });
        assert!(matches!(e, Err(Error::Limit(LimitKind::Steps))));
        assert_eq!(closed, "0");
        let (e, closed) = run("for (let i of it) { { let a = 1; { let b = 2; { let c = 3; } } } }", lim(4));
        assert!(matches!(e, Err(Error::Limit(LimitKind::StatementFrames))));
        assert_eq!(closed, "0");
        // an ordinary throw closes the iterator once
        let (e, closed) = run("for (let i of it) { { let a = 1; throw 'x'; } }", lim(768));
        assert!(matches!(e, Err(Error::Throw(_))));
        assert_eq!(closed, "1");
    });
}

#[test]
fn a_limit_deep_in_nested_calls_gives_every_count_back() {
    real_stack(|| {
        let mut it = Interp::realm(Heap::default(), lim(60)).unwrap();
        let base = it.stats().live_envs;
        let r = it.run_script("function f(n) { try { { let k = n; for (;;) { return f(n + 1); } } } finally { } } f(0)");
        assert!(matches!(r, Err(Error::Limit(LimitKind::StatementFrames))), "{r:?}");
        assert_eq!((it.stmt_frames, it.frames, it.depth, it.env), (0, 0, 0, 0));
        assert_eq!(it.stats().live_envs, base);
        // a JS throw from the bottom of nested calls passes every frame and every finally, in order
        let mut it = Interp::realm(Heap::default(), lim(768)).unwrap();
        let src = "var o = []; function f(n) { try { if (n === 0) throw 'bottom'; { f(n - 1); } } finally { o.push(n); } } try { f(9) } catch (e) { o.push(e); } o.join()";
        assert_eq!(it.run_script(src).map(|v| it.display(v)).unwrap(), "0,1,2,3,4,5,6,7,8,9,bottom");
        assert_eq!((it.stmt_frames, it.frames, it.depth, it.env), (0, 0, 0, 0));
    });
}

#[test]
fn frames_keep_their_script_across_cross_script_callbacks_and_colliding_ids() {
    real_stack(|| {
        // Two scripts reuse the same statement ids for different statements. A's loop frames stay live while B's callbacks run.
        let a = r#"function walkA(xs, cb) { var o = []; for (var i = 0; i < xs.length; i++) { switch (xs[i] % 3) { case 0: o.push(cb(xs[i])); break; default: try { o.push("d" + xs[i]); } finally { o.push("f"); } } } return o.join(); }
class KA { m(xs, cb) { return walkA(xs, cb) + "|" + (this.tag || ""); } }"#;
        let b = r#"var kb = new KA(); kb.tag = "t";
function cbB(n) { for (var j = 0; j < 2; j++) { if (j === 1) { try { throw n; } catch (e) { return "c" + e; } } } }
var res = kb.m([3, 4, 6], cbB) + ";" + walkA([1, 3], function (n) { return walkA([n], function (m) { return "in" + m; }); });
res"#;
        let mut it = Interp::realm(Heap::default(), lim(768)).unwrap();
        it.run_script(a).unwrap();
        // expected value computed in Node with the same two scripts in one context
        assert_eq!(it.run_script(b).map(|v| it.display(v)).unwrap(), "c3,d4,f,c6|t;d1,f,in3");
        assert_eq!((it.stmt_frames, it.frames, it.depth, it.env, it.cur), (0, 0, 0, 0, 0));
        // a limit raised inside B's callback while A's frames are live, then the realm keeps working
        let mut it = Interp::realm(Heap::default(), lim(768)).unwrap();
        it.run_script(a).unwrap();
        let r = it.run_script("walkA([3], function () { for (;;) { { let z = 1; } } })");
        assert!(matches!(r, Err(Error::Limit(LimitKind::Steps))), "{r:?}");
        assert_eq!((it.stmt_frames, it.frames, it.depth, it.env, it.cur), (0, 0, 0, 0, 0));
        assert_eq!(it.run_script("walkA([1, 3], function () {})").map(|v| it.display(v)).unwrap(), "d1,f,");
    });
}

#[test]
fn finite_walks_scale_in_width_without_growing_the_frame_need() {
    real_stack(|| {
        // 59 levels, 1x 10x 100x siblings at the top: the frame need is the depth, not the work
        let wide = |w: usize| format!("{} for (var s = 0; s < {w}; s++) {{ var sib = {{ tag: 1, child: null, sibling: root.child }}; root.child = sib; }} a5(root); 'walked'", walk(59).replace("a5(root); 'walked'", ""));
        let mut need = None;
        for cap in 1..768u32 {
            if outcome(&walk(59), lim(cap)) == "walked" {
                need = Some(cap);
                break;
            }
        }
        let need = need.expect("59 levels fit in 768 frames");
        assert!(need < 512, "need {need}");
        for w in [1usize, 10, 100] {
            assert_eq!(outcome(&wide(w), lim(768)), "walked", "width {w}");
        }
        assert_eq!(outcome(&walk(59), lim(need - 1)), FRAMES);
        // loops: 1x 10x 100x iterations in nested loops use the same frames
        for n in [1u32, 10, 100] {
            let src = format!("var c = 0; for (let i = 0; i < {n}; i++) {{ for (let j = 0; j < {n}; j++) {{ switch (j % 2) {{ case 0: {{ let k = 1; c += k; }} break; default: try {{ c++ }} finally {{ }} }} }} }} c");
            // script list, for, body, for, body, switch, case body, try, try block: 9 at the deepest
            assert_eq!(outcome(&src, lim(9)), (n * n).to_string(), "n {n}");
            assert_eq!(outcome(&src, lim(5)), FRAMES);
        }
    });
}

#[test]
fn deep_expressions_and_native_recursion_still_stop_at_their_own_limits_inside_frames() {
    real_stack(|| {
        // a left-deep chain and a deep call nest, each under many statement frames: the traversal guard answers
        let chain = format!("{{ {{ {{ {}1 }} }} }}", "1+".repeat(2000));
        assert_eq!(outcome(&chain, lim(768)), "limit:TraversalDepth");
        let nest = format!("function f(x) {{ return x }} {{ {{ {}0{} }} }}", "f(".repeat(300), ")".repeat(300));
        assert_eq!(outcome(&nest, lim(768)), "limit:TraversalDepth");
        // native recursion through callbacks, coercions, getters and constructors, in bodies full of statements
        let cb = "function g(n) { try { { return [n].map(function (x) { { return g(x + 1); } })[0]; } } finally { } } g(0)";
        assert_eq!(outcome(cb, lim(768)), "limit:CallDepth");
        let coerce = "var o = { valueOf: function () { { try { return +o } finally { } } } }; +o";
        assert_eq!(outcome(coerce, lim(768)), "limit:CallDepth");
        let getter = "var o = { get p() { { for (;;) { return this.p; } } } }; o.p";
        assert_eq!(outcome(getter, lim(768)), "limit:CallDepth");
        let ctor = "class A { constructor(n) { { this.c = n >= 0 ? new A(n + 1) : null; } } } new A(0)";
        assert_eq!(outcome(ctor, lim(768)), "limit:CallDepth");
        // the frame cap answers first when the statement frames are the scarce resource
        assert_eq!(outcome(cb, lim(20)), FRAMES);
    });
}

#[test]
fn a_realm_survives_a_frame_fault_and_runs_the_next_script() {
    real_stack(|| {
        let mut it = Interp::realm(Heap::default(), lim(30)).unwrap();
        assert_eq!(it.run_script(&walk(3)).map(|v| it.display(v)).unwrap(), "walked");
        assert!(matches!(it.run_script(&walk(30)), Err(Error::Limit(LimitKind::StatementFrames))));
        assert_eq!((it.stmt_frames, it.frames, it.depth, it.env), (0, 0, 0, 0));
        assert_eq!(it.run_script("var z = 0; { { z = 7 } } z").map(|v| it.display(v)).unwrap(), "7");
    });
}


#[test]
fn terminating_a_do_while_does_not_charge_an_extra_iteration() {
    real_stack(|| {
        fn steps(src: &str) -> u64 {
            let prog = crate::parse::parse_script(src, Default::default()).unwrap();
            let mut it = Interp::new(&prog, Heap::default(), lim(768)).unwrap();
            it.begin();
            let before = it.steps;
            assert!(it.exec(prog.list(prog.body).next().unwrap()).is_ok());
            it.steps - before
        }
        // Each loop enters once. The false literal costs one expression step; break costs one statement step.
        assert_eq!(steps("do {} while (false)"), steps("do { break } while (true)"));
    });
}


#[test]
fn a_statement_frame_fault_poisoned_job_is_not_replayed() {
    real_stack(|| {
        let mut it = Interp::realm(Heap::default(), lim(768)).unwrap();
        let src = format!("var runs=0, late=0, fin=0, cat=0; Promise.resolve().then(function() {{ runs++; try {{ {}1{} }} catch(e) {{ cat++ }} finally {{ fin++ }} }}); Promise.resolve().then(function() {{ late++ }});", "{ ".repeat(12), " }".repeat(12));
        it.run_script(&src).unwrap();
        it.limits.max_statement_frames = 8;
        assert!(matches!(it.drain_microtasks(), Err(Error::Limit(LimitKind::StatementFrames))));
        assert_eq!((it.stmt_frames, it.frames, it.depth), (0, 0, 0));
        it.limits.max_statement_frames = 768;
        assert!(matches!(it.drain_microtasks(), Err(Error::Limit(LimitKind::StatementFrames))));
        assert_eq!(it.run_script("[runs,late,cat,fin].join()").map(|v| it.display(v)).unwrap(), "1,0,0,0");
        it.collect(&[]).unwrap();
    });
}
