//! Depth limits: the standalone default, the configured caps, the clamps, the counter after a fault, and the paths that
//! must not bypass the guards. Every test runs on a thread with the stack of a real run: 8 MiB in a release build,
//! 64 MiB in a debug build (see `real_stack`). Fresh processes are the way to check them (one test per process).

use super::*;
use crate::heap::{Heap, HeapLimits};
use crate::parse::{parse_script, Limits};

fn real_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    let mib: usize = if cfg!(debug_assertions) { 64 } else { 8 };
    std::thread::Builder::new().stack_size(mib << 20).spawn(f).unwrap().join().unwrap()
}

/// Two mutually recursive functions over a chain of `levels` nodes (2 JS calls per level). `a6`'s `try` cannot catch
/// a limit, so a refusal ends the run.
fn walk(levels: usize) -> String {
    format!(
        "function a5(t) {{ for (t = t.child; t !== null;) a6(t), t = t.sibling; }}
function a6(t) {{ switch (t.tag) {{ case 0: case 1: try {{ a5(t) }} catch (e) {{}} break; default: a5(t) }} }}
var root = {{ tag: 3, child: null, sibling: null }}, tail = root;
for (var i = 0; i < {levels}; i++) {{ var n = {{ tag: 0, child: null, sibling: null }}; tail.child = n; tail = n; }}
a5(root); 'walked'"
    )
}

/// Parse, check and run `src` once. The text names the outcome: a value, a limit, or another error.
fn outcome(src: &str, limits: EvalLimits) -> String {
    let prog = parse_script(src, Limits::default()).unwrap_or_else(|e| panic!("parse: {e}"));
    match Interp::new(&prog, Heap::default(), limits) {
        Err(Error::Limit(k)) => format!("setup limit:{k:?}"),
        Err(e) => panic!("setup: {e:?}"),
        Ok(mut it) => match it.run() {
            Ok(v) => it.display(v),
            Err(Error::Limit(k)) => format!("limit:{k:?}"),
            Err(e) => format!("other:{e:?}"),
        },
    }
}

fn caps(traversal: u32, calls: u32) -> EvalLimits {
    EvalLimits { max_steps: 5_000_000, max_traversal_depth: traversal, max_call_depth: calls, ..Default::default() }
}

#[test]
fn the_standalone_default_walks_forty_levels_and_the_call_cap_bounds_deeper_chains() {
    real_stack(|| {
        assert_eq!(EvalLimits::default().max_traversal_depth, 512);
        assert_eq!(EvalLimits::default().max_call_depth, 100);
        assert_eq!(EvalLimits::default().max_statement_frames, 512);
        let d = EvalLimits { max_steps: 5_000_000, ..Default::default() };
        // The old test pinned an implementation ceiling, not JS semantics: every statement cost a native frame, so 40 levels
        // were refused. Control frames now live on the heap, and 40 levels (81 calls, about 290 native frames) run.
        assert_eq!(outcome(&walk(40), d), "walked");
        // What bounds a deeper chain is the call cap: 60 levels are 121 calls.
        assert_eq!(outcome(&walk(60), d), format!("limit:{:?}", LimitKind::CallDepth));
    });
}

#[test]
fn configured_caps_complete_forty_and_fifty_levels() {
    real_stack(|| {
        assert_eq!(outcome(&walk(40), caps(768, 128)), "walked");
        assert_eq!(outcome(&walk(50), caps(768, 128)), "walked");
    });
}

#[test]
fn lower_configured_caps_refuse_before_they_grow() {
    real_stack(|| {
        // 81 calls for 40 levels: a call cap of 60 refuses the calls
        assert_eq!(outcome(&walk(40), caps(768, 60)), format!("limit:{:?}", LimitKind::CallDepth));
        // The traversal cap bounds the native recursion of ONE call, not of all calls together: the setup walk is the deepest
        // recursion of this script (7 frames refuse, 8 run), and 40 levels of calls add nothing.
        assert_eq!(outcome(&walk(40), caps(7, 128)), format!("setup limit:{:?}", LimitKind::TraversalDepth));
        assert_eq!(outcome(&walk(40), caps(8, 128)), "walked");
        // zero refuses at once: the setup walk is the first descent
        assert_eq!(outcome("1", caps(0, 128)), format!("setup limit:{:?}", LimitKind::TraversalDepth));
        assert_eq!(outcome(&walk(1), caps(768, 0)), format!("limit:{:?}", LimitKind::CallDepth));
    });
}

#[test]
fn requests_above_the_hard_ceilings_are_clamped() {
    real_stack(|| {
        // a left-deep chain of 2000 terms is walked as 2000 levels: the clamp to 768 refuses it, no crash
        let chain = format!("{}1", "1+".repeat(2000));
        assert_eq!(outcome(&chain, caps(u32::MAX, 128)), format!("setup limit:{:?}", LimitKind::TraversalDepth));
        // the call ceiling is 10,000, however large the request: 10,000 nested calls run, the 10,001st is refused as a CallDepth
        let bound = |n: u32| format!("var g = function () {{ return 'done' }}; for (var i = 0; i < {n}; i++) g = g.bind(null); g()");
        assert_eq!(outcome(&bound(9_999), caps(u32::MAX, u32::MAX)), "done");
        assert_eq!(outcome(&bound(10_000), caps(u32::MAX, u32::MAX)), format!("limit:{:?}", LimitKind::CallDepth));
        // `1 + (1 + (1 + (1 + f(n - 1))))` costs about 11 native frames a level, but the frames are counted from the start
        // of each call. 5,000 levels run (the stack grows by itself on the 8 MiB thread), and the ceiling ends 10,001 of them.
        let heavy = |n: u32| format!("function f(n) {{ return n === 0 ? 0 : 1 + (1 + (1 + (1 + f(n - 1)))) }} f({n})");
        let wide = EvalLimits { max_statement_frames: u32::MAX, ..caps(u32::MAX, u32::MAX) };
        assert_eq!(outcome(&heavy(5_000), wide), "20000");
        assert_eq!(outcome(&heavy(10_001), wide), format!("limit:{:?}", LimitKind::CallDepth));
    });
}

#[test]
fn the_traversal_cap_is_the_configured_value_not_the_old_512() {
    real_stack(|| {
        // 600 terms: more than 512 frames, fewer than 768
        let chain = format!("{}1", "1+".repeat(600));
        assert_eq!(outcome(&chain, caps(512, 128)), format!("setup limit:{:?}", LimitKind::TraversalDepth));
        assert_eq!(outcome(&chain, caps(768, 128)), "601");
    });
}

#[test]
fn the_counter_is_back_to_zero_after_each_fault() {
    real_stack(|| {
        let mut it = Interp::realm(Heap::default(), caps(100, 128)).unwrap();
        // a traversal fault in the middle of a run (a cyclic array joins itself without a call): the next run starts from zero
        assert!(matches!(it.run_script("var a = []; a[0] = a; a.flat(Infinity)"), Err(Error::Limit(LimitKind::TraversalDepth))));
        assert_eq!(it.run_script("1+1").map(|v| it.display(v)).unwrap(), "2");
        assert_eq!(it.run_script(&walk(40)).map(|v| it.display(v)).unwrap(), "walked");
        // a Throw from depth 30
        let mut it = Interp::realm(Heap::default(), caps(768, 128)).unwrap();
        assert!(matches!(it.run_script("function f(n) { if (n === 0) throw 'x'; return f(n - 1) } f(30)"), Err(Error::Throw(_))));
        assert_eq!(it.run_script(&walk(40)).map(|v| it.display(v)).unwrap(), "walked");
        // an Unsupported construct is found before anything runs
        assert!(matches!(it.run_script("function* g() { with (Math) {} }"), Err(Error::Unsupported(_))));
        assert_eq!(it.run_script(&walk(40)).map(|v| it.display(v)).unwrap(), "walked");
    });
}

#[test]
fn the_counter_is_back_to_zero_after_a_step_or_heap_fault() {
    real_stack(|| {
        // a step fault in the middle of the walk, then a walk that fits
        let mut it = Interp::realm(Heap::default(), EvalLimits { max_steps: 600, ..caps(768, 128) }).unwrap();
        assert!(matches!(it.run_script(&walk(40)), Err(Error::Limit(LimitKind::Steps))));
        assert_eq!(it.run_script(&walk(3)).map(|v| it.display(v)).unwrap(), "walked");
        // a heap fault about 40 levels deep (each level allocates one object); then collect and a 100-level recursion
        let intrinsic = Interp::realm(Heap::default(), caps(768, 128)).unwrap().heap.stats().live_objects;
        let heap = HeapLimits { max_objects: intrinsic + 40, ..Default::default() };
        let mut it = Interp::realm(Heap::new(heap).unwrap(), caps(768, 128)).unwrap();
        assert!(matches!(it.run_script("function g(n) { var o = {}; return n === 0 ? 0 : 1 + g(n - 1) } g(80)"), Err(Error::Limit(LimitKind::Heap(_)))));
        it.collect(&[]).unwrap();
        assert_eq!(it.run_script("function f(n) { return n === 0 ? 0 : 1 + f(n - 1) } f(100)").map(|v| it.display(v)).unwrap(), "100");
    });
}

#[test]
fn ordinary_callbacks_and_constructors_hit_the_same_guards() {
    real_stack(|| {
        // the map callback and the native call both count
        let via_map = "function g(n) { return n === 0 ? 0 : [n].map(function (x) { return g(x - 1) })[0] + 1 } g(300)";
        assert_eq!(outcome(via_map, caps(768, 128)), format!("limit:{:?}", LimitKind::CallDepth));
        // valueOf calls itself through ToPrimitive
        assert_eq!(outcome("var o = { valueOf: function () { return +o } }; +o", caps(768, 128)), format!("limit:{:?}", LimitKind::CallDepth));
        // a class constructor, and a derived one through super
        let cls = "class A { constructor(n) { this.c = n ? new A(n - 1) : null } } new A(300)";
        assert_eq!(outcome(cls, caps(768, 128)), format!("limit:{:?}", LimitKind::CallDepth));
        let derived = "class A { constructor(n) { this.c = n ? new B(n - 1) : null } } class B extends A { constructor(n) { super(n) } } new B(300)";
        assert_eq!(outcome(derived, caps(768, 128)), format!("limit:{:?}", LimitKind::CallDepth));
    });
}

#[test]
fn a_react_shaped_walk_of_fifty_nine_levels_completes_under_the_session_caps() {
    real_stack(|| {
        // 118 JS calls. Control frames of `for`, `switch`, `try` and blocks live in the statement machine, not on the native stack.
        assert_eq!(outcome(&walk(59), caps(768, 128)), "walked");
    });
}

#[test]
fn a_react_shaped_walk_of_seventy_five_levels_completes_under_a_call_cap_of_256() {
    real_stack(|| {
        // 151 JS calls. The saved login page needs 158 calls and 608 native frames (measured): 128 calls refused it.
        assert_eq!(outcome(&walk(75), EvalLimits { max_statement_frames: 768, ..caps(768, 128) }), format!("limit:{:?}", LimitKind::CallDepth));
        assert_eq!(outcome(&walk(75), EvalLimits { max_statement_frames: 768, ..caps(768, 256) }), "walked");
    });
}
