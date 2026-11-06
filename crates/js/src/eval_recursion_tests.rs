//! Deep recursion of the React commit shape under the browser session limits. The live staging app (React 19) stopped
//! with `Limit(TraversalDepth)` at a fiber tree 96 levels deep: `commitMutationEffectsOnFiber` and
//! `recursivelyTraverseMutationEffects` call each other once per level (2 JS calls a level), with a `switch`, an `if`, a
//! `for` loop and a `try`. Real apps are deeper. Every test runs on a thread with the stack of a real run (8 MiB in a
//! release build, 64 MiB in a debug build), as `main.rs` does.

use super::*;
use crate::heap::Heap;
use crate::parse::{parse_script, Limits};

fn on_stack<T: Send + 'static>(bytes: usize, f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(bytes).spawn(f).unwrap().join().unwrap()
}

fn real_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    on_stack(if cfg!(debug_assertions) { 64 << 20 } else { 8 << 20 }, f)
}

const COMMIT: &str = "
var MASK = 3;
function commitMutationEffectsOnFiber(fiber, root) {
  switch (fiber.tag) {
    case 0: case 11:
      recursivelyTraverseMutationEffects(root, fiber);
      root.count = root.count + 1;
      break;
    case 5:
      try { recursivelyTraverseMutationEffects(root, fiber); } catch (e) { root.err = e; }
      root.count = root.count + 1;
      break;
    default:
      recursivelyTraverseMutationEffects(root, fiber);
  }
}
function recursivelyTraverseMutationEffects(root, parentFiber) {
  if (parentFiber.subtreeFlags & MASK) {
    var child = parentFiber.child;
    for (; child !== null; child = child.sibling) commitMutationEffectsOnFiber(child, root);
  }
}
";

/// A chain of `levels` fibers; each has one leaf sibling. The tags cycle through the cases of the `switch`.
fn tree(levels: usize) -> String {
    format!(
        "{COMMIT}
var tags = [0, 5, 3, 11], root = {{ tag: 3, subtreeFlags: 3, child: null, sibling: null, count: 0 }}, tail = root;
for (var i = 0; i < {levels}; i++) {{
  var n = {{ tag: tags[i % 4], subtreeFlags: 3, child: null, sibling: null }};
  n.sibling = {{ tag: 0, subtreeFlags: 0, child: null, sibling: null }};
  tail.child = n; tail = n;
}}
commitMutationEffectsOnFiber(root, root); root.count"
    )
}

/// A fiber whose child is itself: the same recursion, with no end.
fn endless() -> String {
    format!("{COMMIT} var f = {{ tag: 3, subtreeFlags: 3, child: null, sibling: null }}; f.child = f; var root = {{ count: 0 }}; commitMutationEffectsOnFiber(f, root); 'done'")
}

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

#[test]
fn a_commit_walk_of_150_levels_runs_under_the_session_limits() {
    real_stack(|| {
        // 150 levels = 300 JS calls on the chain, 150 leaf calls more. Tags 0, 5 and 11 count, tag 3 does not: 113 of the 150
        // chain fibers, and every leaf: 263.
        assert_eq!(outcome(&tree(150), EvalLimits::session()), "263");
    });
}

#[test]
fn a_commit_walk_of_2000_levels_runs_under_the_session_limits() {
    real_stack(|| {
        // 4,000 JS calls on the chain: 1,500 counted chain fibers and 2,000 leaves
        assert_eq!(outcome(&tree(2000), EvalLimits::session()), "3500");
    });
}

#[test]
fn endless_commit_recursion_ends_as_a_limit_not_a_crash() {
    real_stack(|| {
        let r = outcome(&endless(), EvalLimits::session());
        assert!(r == "limit:CallDepth" || r == "limit:TraversalDepth" || r == "limit:StatementFrames", "{r}");
    });
}

#[test]
fn a_million_levels_end_as_a_limit_not_a_crash() {
    real_stack(|| {
        // the limits asked for are far above the ceilings: the clamps hold
        let huge = EvalLimits { max_traversal_depth: u32::MAX, max_call_depth: u32::MAX, max_statement_frames: u32::MAX, max_steps: u64::MAX, ..Default::default() };
        let r = outcome(&tree(1_000_000), huge);
        assert!(r.starts_with("limit:"), "{r}");
        let r = outcome(&endless(), huge);
        assert!(r.starts_with("limit:"), "{r}");
    });
}

/// One call with the deepest native recursion that `max_traversal_depth` allows and no nested call: a left-deep chain of 760
/// terms and a chain of 760 member reads.
fn one_call_at_the_cap() -> String {
    format!("var a = {{}}; a.a = a; function f() {{ return {}1 }} function g() {{ return a{} === a }} f() + (g() ? 1 : 0)", "1+".repeat(760), ".a".repeat(760))
}

#[test]
fn the_stack_grows_on_a_thread_that_is_too_small() {
    // A 256 KiB thread cannot hold 2,000 levels (4,000 calls), one call at the cap, or the setup walk of the script. The
    // evaluator moves a call, the walk and the top level to a new stack segment when less than `STACK_RED` is left.
    // A crash here is a stack overflow.
    on_stack(256 << 10, || {
        assert_eq!(outcome(&one_call_at_the_cap(), EvalLimits::session()), "762");
        assert_eq!(outcome(&tree(2000), EvalLimits::session()), "3500");
        let r = outcome(&endless(), EvalLimits::session());
        assert_eq!(r, "limit:CallDepth");
    });
}

#[test]
fn nested_calls_through_natives_end_at_the_call_ceiling_without_a_crash() {
    real_stack(|| {
        let wide = EvalLimits { max_steps: u64::MAX, max_statement_frames: u32::MAX, ..EvalLimits::session() };
        for src in [
            "function f(n) { return [n].map(f) } f(1)",
            "function f() { return Reflect.apply(f, null, []) } f()",
            "var o = { get g() { return this.g } }; o.g",
            "var o = { valueOf: function () { return +o } }; +o",
            "function F() { return new F() } new F()",
        ] {
            assert_eq!(outcome(src, wide), "limit:CallDepth", "{src}");
        }
    });
}
