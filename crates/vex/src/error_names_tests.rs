// TypeError messages name the callee and the property, as V8 does. The name is made on the throw path only (`error_names.rs`).
use crate::script;

fn msg(body: &str) -> String {
    let src = format!("var result; try {{ {body} }} catch (e) {{ result = e.name + ': ' + e.message; }}");
    let mut vm = script::load(&src).unwrap();
    vm.run_main(0).unwrap();
    vm.global_text("result")
}

#[test]
fn call_of_a_property() {
    assert_eq!(msg("var o = {}; o.f();"), "TypeError: o.f is not a function");
    assert_eq!(msg("var o = {a: {}}; o.a.b();"), "TypeError: o.a.b is not a function");
    assert_eq!(msg("var o = {}, k = 'x'; o[k]();"), "TypeError: o[k] is not a function");
    assert_eq!(msg("var o = {}; o['x']();"), "TypeError: o.x is not a function");
}

#[test]
fn call_of_a_name() {
    assert_eq!(msg("var f = 1; f();"), "TypeError: f is not a function");
    assert_eq!(msg("var o = {f: 1}; o.f(1, 2);"), "TypeError: o.f is not a function");
    assert_eq!(msg("var f = 1; f(...[1]);"), "TypeError: f is not a function");
}

#[test]
fn call_of_a_property_in_a_function() {
    assert_eq!(msg("function g(o) { return o.run(); } g({});"), "TypeError: (intermediate value).run is not a function");
    assert_eq!(msg("function g() { return this.run(); } g.call({});"), "TypeError: this.run is not a function");
}

#[test]
fn read_of_undefined_and_null() {
    assert_eq!(msg("var u; u.length;"), "TypeError: Cannot read properties of undefined (reading 'length')");
    assert_eq!(msg("var n = null; n.x;"), "TypeError: Cannot read properties of null (reading 'x')");
    assert_eq!(msg("var u; u[0];"), "TypeError: Cannot read properties of undefined (reading '0')");
    assert_eq!(msg("var u, k = 'key'; u[k];"), "TypeError: Cannot read properties of undefined (reading 'key')");
    assert_eq!(msg("var o = {}; o.a.b;"), "TypeError: Cannot read properties of undefined (reading 'b')");
}

#[test]
fn write_to_undefined_and_null() {
    assert_eq!(msg("var u; u.a = 1;"), "TypeError: Cannot set properties of undefined (setting 'a')");
    assert_eq!(msg("var n = null; n.a = 1;"), "TypeError: Cannot set properties of null (setting 'a')");
    assert_eq!(msg("var n = null, k = 'z'; n[k] = 1;"), "TypeError: Cannot set properties of null (setting 'z')");
}

#[test]
fn new_of_a_non_constructor() {
    assert_eq!(msg("var X = 1; new X();"), "TypeError: X is not a constructor");
    assert_eq!(msg("var o = {}; new o.K();"), "TypeError: o.K is not a constructor");
    assert_eq!(msg("var A = () => 1; new A();"), "TypeError: A is not a constructor");
    assert_eq!(msg("var X = 1; new X(...[]);"), "TypeError: X is not a constructor");
}

#[test]
fn iteration_of_a_non_iterable() {
    assert_eq!(msg("var o = {}; for (var x of o) {}"), "TypeError: o is not iterable");
    assert_eq!(msg("var o = {p: 1}; for (var x of o.p) {}"), "TypeError: o.p is not iterable");
}
