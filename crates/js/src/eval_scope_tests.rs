//! Static scope resolution (see `eval_scope.rs`). The Node-checked cases are `scope_*` in `tools/fixtures/js/eval-cases.txt`.
//! These tests cover what Node cannot show: which sites are resolved, a name that a later script declares, and async code.
//! A debug build also checks every resolved lookup against the full search.

use super::*;
use crate::heap::Heap;
use crate::parse::{parse_script, Limits};

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn show(it: &mut Interp, r: Result<Value, Error>) -> String {
    match r {
        Ok(v) => it.display(v),
        Err(Error::Throw(v)) if matches!(v, Value::Object(_)) => format!("throw:{}", it.get_str(v, "name")),
        Err(e) => format!("other:{e:?}"),
    }
}

/// Run `src` and give the slot of each identifier read named `name`, in source order, after the run.
fn slots_of(src: &str, name: &str) -> Vec<u32> {
    let prog = parse_script(src, Limits::default()).unwrap();
    let mut it = Interp::new(&prog, Heap::default(), EvalLimits { max_steps: 100_000, ..Default::default() }).unwrap();
    it.run().unwrap();
    (0..it.prog.stats().exprs)
        .filter(|&i| matches!(it.prog.expr(ExprId::from_index(i)).kind, ExprKind::Ident) && it.names[it.expr_name[i] as usize] == name)
        .map(|i| it.slots.sites[i])
        .collect()
}

/// `(hops, index)` of a resolved slot; `None` for dynamic. A site resolves at its second run.
fn hops(s: u32) -> Option<(u32, u32)> {
    assert_eq!(s & UNRES, 0, "site ran less than twice");
    (s & LOCAL != 0).then_some(((s >> 16) & MAX_HOPS, s & 0xFFFF))
}

#[test]
fn local_outer_and_global_sites_resolve() {
    // `f` has no inner function, so `a` (a parameter) and `b` (a var) are frame slots 0 and 1. Its first call makes no frame (it has
    // no loop and no call), so a site resolves at the second call.
    assert_eq!(slots_of("function f(a) { var b = 2; return a + b } f(1); f(1)", "a"), [FRAME]);
    assert_eq!(slots_of("function f(a) { var b = 2; return a + b } f(1); f(1)", "b"), [FRAME | 1]);
    // `g` has an inner function: its variables are in an environment (hops 0, index 0 and 1)
    assert_eq!(slots_of("function g(a) { var b = 2; function h() {} return a + b } g(1); g(1)", "a").iter().map(|&s| hops(s)).collect::<Vec<_>>(), [Some((0, 0))]);
    assert_eq!(slots_of("function g(a) { var b = 2; function h() {} return a + b } g(1); g(1)", "b").iter().map(|&s| hops(s)).collect::<Vec<_>>(), [Some((0, 1))]);
    // a closure reads a variable one environment up: the arrow has no environment (it is a frame function), then a block, then
    // the function
    let s = slots_of("function f(a) { { let c = 1; return (() => a + c)() } } f(1); f(1)", "a");
    assert_eq!(hops(s[0]), Some((1, 0)));
    // a global function is not in any scope: GLOBAL
    assert_eq!(slots_of("function g() {} for (var i = 0; i < 2; i++) g()", "g"), [GLOBAL]);
    // a top-level let is environment 0, with no hop from a frame function
    let s = slots_of("let l = 1; function f() { return l } f(); f()", "l");
    assert_eq!(hops(s[0]), Some((0, 0)));
    let s = slots_of("let l = 1; function f() { function g() {} return l } f(); f()", "l");
    assert_eq!(hops(s[0]), Some((1, 0)));
}

#[test]
fn dynamic_sites_stay_dynamic() {
    // a class is a barrier: a read that must cross it is dynamic, a read below it is not
    let s = slots_of("var v = 1; class A { m(p) { return v + p } } new A().m(2); new A().m(2)", "v");
    assert_eq!(s, [0]);
    let s = slots_of("var v = 1; class A { m(p) { return v + p } } new A().m(2); new A().m(2)", "p");
    assert_eq!(hops(s[0]), Some((0, 0)));
    // `arguments` is never resolved
    assert_eq!(slots_of("function f() { return arguments.length } f(); f()", "arguments"), [0]);
}

#[test]
fn parameter_default_reads_the_outer_name_before_the_body_var() {
    big(|| {
        let src = "var y = 'outer'; function f(a = y) { var y = 'inner'; return a + y } f() + f() + f('p')";
        let prog = parse_script(src, Limits::default()).unwrap();
        let mut it = Interp::new(&prog, Heap::default(), EvalLimits::default()).unwrap();
        let r = it.run();
        assert_eq!(show(&mut it, r), "outerinnerouterinnerpinner");
    });
}

#[test]
fn a_later_script_declares_a_global_let() {
    big(|| {
        let mut it = Interp::realm(Heap::default(), EvalLimits::default()).unwrap();
        for (src, want) in [
            ("function f() { return typeof y === 'undefined' ? 'u' : y } f()", "u"),
            ("let y = 'lex'; f()", "lex"),
            ("y = 'set'; f()", "set"),
            ("var z = 1; function g() { return z } g()", "1"),
        ] {
            let r = it.run_script(src);
            assert_eq!(show(&mut it, r), want, "{src}");
        }
    });
}

#[test]
fn async_code_resumes_with_the_same_scopes() {
    big(|| {
        let src = "var log = [];
        async function f(n) { let a = n; for (let i = 0; i < 2; i++) { let b = a + i; await null; log.push(b + a) } { const c = 100; await null; log.push(c + a) } }
        f(1); f(10); (function () { return log.join() })";
        let prog = parse_script(src, Limits::default()).unwrap();
        let mut it = Interp::new(&prog, Heap::default(), EvalLimits::default()).unwrap();
        let reader = it.run().unwrap();
        it.drain_microtasks().unwrap();
        let r = it.call(reader, Value::Undefined, &[]);
        assert_eq!(show(&mut it, r), "2,20,3,21,101,110");
    });
}

// ---- locals that no closure captures (frame slots; see `eval_scope.rs`)

/// Run `src`, drain the microtasks, then give the text of the global `out`.
fn run_out(src: &str) -> String {
    let src = format!("{src}; (function () {{ return String(out) }})");
    big(move || {
        let prog = parse_script(&src, Limits::default()).unwrap();
        let mut it = Interp::new(&prog, Heap::default(), EvalLimits::default()).unwrap();
        let reader = it.run().unwrap();
        it.drain_microtasks().unwrap();
        let r = it.call(reader, Value::Undefined, &[]);
        show(&mut it, r)
    })
}

/// The result of `src`, or `unsupported:` and what the engine refuses.
fn run_one(src: &str) -> String {
    let src = src.to_string();
    big(move || {
        let prog = parse_script(&src, Limits::default()).unwrap();
        let mut it = match Interp::new(&prog, Heap::default(), EvalLimits::default()) {
            Ok(it) => it,
            Err(Error::Unsupported(u)) => return format!("unsupported:{}", u.what),
            Err(e) => panic!("{e:?}"),
        };
        let r = it.run();
        match r {
            Err(Error::Unsupported(u)) => format!("unsupported:{}", u.what),
            r => show(&mut it, r),
        }
    })
}

#[test]
fn async_function_keeps_its_locals_across_awaits_while_leaf_calls_run() {
    let src = "var out = []; function leaf(k) { var z = k * 3; let w = z + 1; return w }
        async function f(x) { var a = x; let b = a + 1; await 0; var c = leaf(b); await null; out.push(a + ',' + b + ',' + c + ',' + leaf(a)) }
        f(1); f(5); out.push('sync')";
    assert_eq!(run_out(src), "sync,1,2,7,4,5,6,19,16");
}

#[test]
fn async_loop_with_leaf_calls_between_awaits() {
    let src = "var out = ''; function leaf(q) { var r = q + 1; return r }
        async function f() { var a = 1; for (let i = 0; i < 3; i++) { await null; out += a + i + leaf(i) + ',' } }
        f()";
    assert_eq!(run_out(src), "2,4,6,");
}

#[test]
fn generator_resumes_with_locals_between_leaf_calls() {
    let src = "var out = []; function leaf(k) { var q = k * 100; { let t = q + 1; return t } }
        function* g(n) { var a = n; let b = 1; try { for (let i = 0; i < 3; i++) { var t = leaf(i); yield a + b + t; a++; b *= 2 } } finally { out.push('fin' + a) } }
        var it = g(1); out.push(it.next().value); out.push(it.next().value); out.push(it.return('x').value); out.push(it.next().done)";
    assert_eq!(run_out(src), "3,105,fin2,x,true");
}

#[test]
fn two_generators_and_a_leaf_function_do_not_share_slots() {
    let src = "var out = []; function leaf(a, b) { var s = a + b; return s }
        function* g(id) { var v = id; for (let i = 0; i < 3; i++) { yield leaf(v, i); v += 10 } }
        var a = g(1), b = g(2); for (var k = 0; k < 3; k++) { out.push(a.next().value, b.next().value) }";
    assert_eq!(run_out(src), "1,2,12,13,23,24");
}

#[test]
fn eval_and_with_cannot_see_or_hide_the_locals_of_a_function() {
    // The engine has no `eval`, `with` or `Function` constructor, so no code can reach a local by name.
    assert_eq!(run_one("function f() { var local = 1; return eval('local') } f()"), "throw:ReferenceError");
    assert_eq!(run_one("function f() { var local = 1; with ({ local: 2 }) { return local } } f()"), "unsupported:with statement");
    assert_eq!(run_one("function f() { var local = 1; return new Function('return local')() } f()"), "unsupported:Function constructor (dynamic code)");
}

#[test]
fn sloppy_arguments_is_a_copy_and_does_not_alias_the_parameters() {
    // Node aliases them (a mapped arguments object). This engine does not, so `arguments` and a parameter are separate
    // bindings, and the function that reads `arguments` keeps its environment.
    assert_eq!(run_one("function f(a) { arguments[0] = 9; return a } f(1)"), "1");
    assert_eq!(run_one("function f(a) { a = 5; return arguments[0] } f(1)"), "1");
    assert_eq!(run_one("function f(a, b) { var n = arguments.length; var s = a + b; return n + ',' + s + ',' + arguments[1] } f(1, 2)"), "2,3,2");
}

#[test]
fn locals_of_a_leaf_function_leave_no_environment_behind() {
    big(|| {
        let src = "function leaf(a, b) { var c = a + b; { let d = c * 2; { const e = d + 1; return e } } } var s = 0; for (var i = 0; i < 50; i++) s += leaf(i, 1); s";
        let prog = parse_script(src, Limits::default()).unwrap();
        let mut it = Interp::new(&prog, Heap::default(), EvalLimits::default()).unwrap();
        let r = it.run();
        assert_eq!(show(&mut it, r), "2600");
        assert_eq!(it.stats().live_envs, 1);
        // nothing of a frame stays after the calls (the frame store is empty between calls)
        assert!(it.envs[FRAME_ENV as usize].bindings.is_empty());
    });
}

#[test]
fn a_throw_inside_nested_leaf_calls_drops_their_frames() {
    big(|| {
        let src = "function a(n) { var x = n; if (n == 0) throw 'boom'; { let y = x; a(y - 1) } } var r; try { a(5) } catch (e) { r = e } r";
        let prog = parse_script(src, Limits::default()).unwrap();
        let mut it = Interp::new(&prog, Heap::default(), EvalLimits::default()).unwrap();
        let r = it.run();
        assert_eq!(show(&mut it, r), "boom");
        assert!(it.envs[FRAME_ENV as usize].bindings.is_empty());
    });
}

#[test]
fn a_limit_inside_a_leaf_call_drops_its_frame() {
    big(|| {
        let src = "function deep(n) { var x = n; return deep(n + 1) + x } deep(0)";
        let prog = parse_script(src, Limits::default()).unwrap();
        let mut it = Interp::new(&prog, Heap::default(), EvalLimits { max_call_depth: 50, ..Default::default() }).unwrap();
        assert!(matches!(it.run(), Err(Error::Limit(_))));
        assert!(it.envs[FRAME_ENV as usize].bindings.is_empty());
    });
}

/// How many functions of `src` ran in frame slots (after the script ran), and how many functions it has.
fn frame_count(src: &str) -> (usize, usize) {
    let src = src.to_string();
    big(move || {
        let prog = parse_script(&src, Limits::default()).unwrap();
        let mut it = Interp::new(&prog, Heap::default(), EvalLimits::default()).unwrap();
        it.run().unwrap();
        it.drain_microtasks().unwrap();
        let n = it.prog.stats().funcs;
        ((0..n).filter(|&i| it.scopes[i].fr == FR_ON).count(), n)
    })
}

#[test]
fn which_functions_keep_their_variables_in_frame_slots() {
    // (script, functions in frame slots, functions). A function without a loop or a call gets its frame at its second call, so each
    // one is called twice.
    let yes = |src: &str| assert_eq!(frame_count(src), (1, 1), "{src}");
    let no = |src: &str| assert_eq!(frame_count(src), (0, 1), "{src}");
    yes("function f(a, b) { var c = a + b; { let d = c; return d } } f(1, 2); f(1, 2)");
    yes("var f = (x) => { var y = x; return y }; f(1); f(1)");
    yes("var f = (x) => x + 1; f(1); f(1)");
    yes("var f = function (x) { var y = x; return y }; f(1); f(1)");
    assert_eq!(frame_count("var o = { m(p) { var q = p; return q }, get g() { var r = 1; return r } }; o.m(1); o.m(1); o.g; o.g"), (2, 2));
    yes("function f() { 'use strict'; var a = 1; return a } f(); f()");
    yes("function f() { try { throw 1 } catch (e) { var r = e } return r } f(); f()");
    // a function with an inner function or class keeps its environment (the inner one here is a frame function, called or not)
    assert_eq!(frame_count("function f(a) { function g() { return 1 } return a + g() } f(1); f(1)"), (1, 2));
    assert_eq!(frame_count("function f(a) { return () => a } f(1)(); f(1)()"), (1, 2));
    no("function f() { var t = this; return t } f(); f()");
    no("function f(a) { var n = arguments.length; return n + a } f(1); f(1)");
    no("function f({ a }) { var b = a; return b } f({ a: 1 }); f({ a: 1 })");
    no("function f(a = 1) { var b = a; return b } f(); f()");
    no("function f(...r) { var b = r; return b } f(); f()");
    no("function f(a, a) { var b = a; return b } f(1, 2); f(1, 2)");
    no("function f() { var a = 1; delete a; return a } f(); f()");
    no("var f = function self() { var a = 1; return a }; f(); f()");
    no("function* f() { var a = 1; yield a } f().next(); f().next()");
    no("async function f() { var a = 1; return a } f(); f()");
    no("class K { m(x) { var y = x; return y } } new K().m(1); new K().m(1)");
    no("function f() { return new.target } f(); f()");
    // an arrow function may use `this`: it reads the one of its closure
    yes("var f = (x) => { var y = this; return y }; f(1); f(1)");
    // more than 255 names keep the environment
    let many: String = (0..260).map(|i| format!("var v{i} = {i};")).collect();
    no(&format!("function f() {{ {many} return v0 }} f(); f()"));
    let few: String = (0..250).map(|i| format!("var v{i} = {i};")).collect();
    yes(&format!("function f() {{ {few} return v0 }} f(); f()"));
}

#[test]
fn a_function_gets_its_frame_at_the_first_call_with_a_loop_or_a_call_and_at_the_second_without() {
    // no loop, no call: the second call
    assert_eq!(frame_count("function f(a) { return a + 1 } f(1)"), (0, 1));
    assert_eq!(frame_count("function f(a) { return a + 1 } f(1); f(1)"), (1, 1));
    // a call, a `new`, a tagged template or a loop: the first call
    assert_eq!(frame_count("function f(a) { return Math.abs(a) } f(1)"), (1, 1));
    assert_eq!(frame_count("function f(a) { return new Array(a) } f(1)"), (1, 1));
    assert_eq!(frame_count("function f(a) { return String.raw`x${a}` } f(1)"), (1, 1));
    assert_eq!(frame_count("function f(a) { var s = 0; for (var i = 0; i < a; i++) s += i; return s } f(3)"), (1, 1));
    assert_eq!(frame_count("function f(a) { var s = 0; while (s < a) s++; return s } f(3)"), (1, 1));
    assert_eq!(frame_count("function f(a) { for (var k in a) return k } f({ x: 1 })"), (1, 1));
}

#[test]
fn a_call_that_comes_back_into_a_function_during_its_first_call_still_works() {
    // `f` has no loop and no call, so its first call has no frame. The getter calls `f` again before that call ended: the second call
    // must keep an environment, and the call after both gets the frame.
    let src = "var o = { get p() { return f(0) } }; function f(a) { var b = a + 1; { let c = b * 2; return a ? o.p + c : c } } \
               [f(1), f(1), f(2), f(0)].join()";
    assert_eq!(run_one(src), "6,6,8,2");
}
