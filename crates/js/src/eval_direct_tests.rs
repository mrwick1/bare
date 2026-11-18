//! Direct eval (`eval_scripts.rs`, `Interp::eval_direct`): a call `eval(x)` by that name runs `x` in the scope of the call. Webpack's
//! development builds wrap each module body in such a call inside a factory `(module, exports, __webpack_require__) => { ... }`.

use super::*;
use crate::heap::Heap;

/// The result of `src` in a realm with the global `eval`, the same on the tree walker and on both bytecode tiers, with and without lazy
/// function bodies.
fn run_eval(src: &str) -> String {
    let mut r = Vec::new();
    for exec in [Exec::Tree, Exec::Auto, Exec::Byte] {
        for lazy in [false, true] {
            r.push(run_in(src, exec, lazy));
        }
    }
    assert!(r.iter().all(|x| *x == r[0]), "{src}: tree, auto, byte (each plain, lazy): {r:?}");
    r[0].clone()
}

fn run_in(src: &str, exec: Exec, lazy: bool) -> String {
    let src = if lazy { format!("/*{}*/{src}", " ".repeat(crate::parse_cache::LAZY_MIN_SOURCE)) } else { src.to_string() };
    std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(move || {
            let mut it = Interp::realm(Heap::default(), EvalLimits { exec, ..Default::default() }).unwrap();
            let f = it.eval_function().unwrap();
            let k = it.heap.key_from_str("eval").unwrap();
            it.define_data(it.global_object(), k, Value::Object(f), true, false, true).unwrap();
            match it.run_script(&src) {
                Ok(v) => it.display(v),
                Err(Error::Throw(v)) if matches!(v, Value::Object(_)) => format!("throw:{}", it.get_str(v, "name")),
                Err(e) => format!("other:{e:?}"),
            }
        })
        .unwrap()
        .join()
        .unwrap()
}

#[test]
fn direct_eval_reads_and_writes_the_locals_of_the_caller() {
    assert_eq!(run_eval("function f() { var local = 1; return eval('local') } f()"), "1");
    assert_eq!(run_eval("function f() { var n = 1; eval('n = 2'); return n } f()"), "2");
    assert_eq!(run_eval("function f(a) { let b = 2; { const c = 3; return eval('a + b + c') } } f(1)"), "6");
    assert_eq!(run_eval("function o() { var hid = 7; function i() { return eval('hid') } return i() } o()"), "7");
}

#[test]
fn webpack_module_factory_sees_its_parameters() {
    let src = "var m = { exports: {} };
        var factory = (module, exports, __webpack_require__) => { eval(\"'use strict';\\nObject.defineProperty(exports, '__esModule', { value: true });\\nexports.a = 1;\\nmodule.exports.b = __webpack_require__(2);\\n//# sourceURL=webpack://x\") };
        factory.call(m.exports, m, m.exports, function (x) { return x * 10 });
        [m.exports.__esModule, m.exports.a, m.exports.b].join()";
    assert_eq!(run_eval(src), "true,1,20");
}

#[test]
fn direct_eval_sees_this_and_makes_closures_over_the_caller() {
    assert_eq!(run_eval("var o = { f() { return (() => eval('this'))() } }; o.f() === o"), "true");
    assert_eq!(run_eval("function f() { var n = 5; var g = eval('(function () { return n })'); n = 6; return g() } f()"), "6");
    assert_eq!(run_eval("function f() { var n = 1; var g = eval('function h() { return ++n } h'); g(); g(); return n } f()"), "3");
}

#[test]
fn only_a_call_by_the_name_eval_is_direct() {
    let src = "var x = 'g'; function f() { var x = 'l'; var e = eval; return [(0, eval)('x'), eval('x'), e('x'), globalThis.eval('x')].join() } f()";
    assert_eq!(run_eval(src), "g,l,g,g");
}

#[test]
fn declarations_of_a_direct_eval_stay_in_the_eval() {
    assert_eq!(run_eval("function f() { eval('let y = 1'); return typeof y } f()"), "undefined");
    assert_eq!(run_eval("function f() { var t = eval('var z = 4; function g() { return z } g()'); return t + ',' + typeof z + ',' + typeof g } f()"), "4,undefined,undefined");
    assert_eq!(run_eval("eval('var top = 1'); typeof top"), "undefined");
}

#[test]
fn a_hot_function_with_direct_eval_keeps_its_environment() {
    // Many calls: a function with frame slots or bytecode would hide its locals from the eval.
    assert_eq!(run_eval("function f(a) { return eval('a + 1') } var s = 0; for (var i = 0; i < 300; i++) s += f(i); s"), "45150");
    assert_eq!(run_eval("function o(k) { var hid = k; function i() { return eval('hid') } return i() } var s = 0; for (var j = 0; j < 300; j++) s += o(j); s"), "44850");
    assert_eq!(run_eval("function f(a) { return eval('(function () { return a })')() } var s = 0; for (var i = 0; i < 300; i++) s += f(i); s"), "44850");
}

#[test]
fn indirect_eval_still_runs_in_the_global_scope() {
    assert_eq!(run_eval("function f() { var local = 1; return (0, eval)('typeof local') } f()"), "undefined");
    assert_eq!(run_eval("(0, eval)('var gv = 3'); gv"), "3");
}
