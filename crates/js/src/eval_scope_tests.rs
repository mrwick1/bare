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
    // `a` is a parameter (hops 0, index 0). `b` is a var of the function (hops 0, index 1).
    assert_eq!(slots_of("function f(a) { var b = 2; return a + b } f(1); f(1)", "a").iter().map(|&s| hops(s)).collect::<Vec<_>>(), [Some((0, 0))]);
    assert_eq!(slots_of("function f(a) { var b = 2; return a + b } f(1); f(1)", "b").iter().map(|&s| hops(s)).collect::<Vec<_>>(), [Some((0, 1))]);
    // a closure reads a variable two environments up: the arrow's own, a block, then the function
    let s = slots_of("function f(a) { { let c = 1; return (() => a + c)() } } f(1); f(1)", "a");
    assert_eq!(hops(s[0]), Some((2, 0)));
    // a global function is not in any scope: GLOBAL
    assert_eq!(slots_of("function g() {} for (var i = 0; i < 2; i++) g()", "g"), [GLOBAL]);
    // a top-level let is environment 0
    let s = slots_of("let l = 1; function f() { return l } f(); f()", "l");
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
