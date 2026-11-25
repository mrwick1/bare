// Tests of `Error.prototype.stack`, `Error.captureStackTrace` and `Error.stackTraceLimit` (`error_stack.rs`): each case is a program, the
// global to read and the expected text.
use crate::script::run;

fn ev(src: &str, name: &str) -> String {
    match run(src) {
        Ok(mut vm) => vm.global_text(name),
        Err(e) => panic!("{e}\n--- source:\n{src}"),
    }
}

fn check(cases: &[(&str, &str, &str)]) {
    for (src, name, want) in cases {
        assert_eq!(ev(src, name), *want, "{src}");
    }
}

#[test]
fn an_error_has_a_string_stack_that_starts_with_its_text() {
    check(&[
        ("var r = typeof new Error('x').stack;", "r", "string"),
        ("var r = new Error('x').stack.split('\\n')[0];", "r", "Error: x"),
        ("var r = new RangeError('y').stack.split('\\n')[0];", "r", "RangeError: y"),
        ("var r = Error('z').stack.split('\\n')[0];", "r", "Error: z"),
        ("class E extends Error {} var r = new E('w').stack.split('\\n')[0];", "r", "Error: w"),
    ]);
}

#[test]
fn the_frames_name_the_functions_from_the_inside_out() {
    check(&[
        (
            "function inner() { return new Error('e'); } function outer() { return inner(); } var l = outer().stack.split('\\n'); var r = l[1].includes(' inner ') + ',' + l[2].includes(' outer ');",
            "r",
            "true,true",
        ),
        (
            "function f() { return new Error('e'); }\nvar r = f().stack.split('\\n')[1].replace(/^\\s+/, '');",
            "r",
            "at f (<anonymous>:1:23)",
        ),
        (
            "var a = 1;\n\n  var e = new Error('t');\nvar r = e.stack.split('\\n')[1].replace(/^\\s+/, '');",
            "r",
            "at <anonymous>:3:11",
        ),
    ]);
}

#[test]
fn a_thrown_internal_error_has_a_stack() {
    check(&[(
        "function g() { return undefined.x; } var r; try { g(); } catch (e) { r = e.stack.split('\\n')[0] + '|' + e.stack.split('\\n')[1].includes(' g '); }",
        "r",
        "TypeError: Cannot read properties of undefined (reading 'x')|true",
    )]);
}

#[test]
fn the_stack_can_be_assigned_and_is_not_enumerable() {
    check(&[
        ("var e = new Error('x'); e.stack = 'y'; var r = e.stack;", "r", "y"),
        ("var e = new Error('x'); var r = Object.keys(e).length + ',' + Object.getOwnPropertyNames(e).join();", "r", "0,message"),
        ("var e = new Error('x'); e.stack; e.stack = 'q'; var r = e.stack + Object.keys(e).length;", "r", "q0"),
    ]);
}

#[test]
fn capture_stack_trace_and_the_limit() {
    check(&[
        ("var o = {}; Error.captureStackTrace(o); var r = typeof o.stack;", "r", "string"),
        ("var o = {}; Error.captureStackTrace(o); var r = o.stack.split('\\n')[0];", "r", "Error"),
        ("function a() { var o = {}; Error.captureStackTrace(o, a); return o.stack; } function b() { return a(); } var r = b().split('\\n')[1].includes(' b ');", "r", "true"),
        ("var r = Error.stackTraceLimit;", "r", "10"),
        ("Error.stackTraceLimit = 0; var r = new Error('x').stack;", "r", "Error: x"),
        ("function f(n) { return n ? f(n - 1) : new Error('d'); } var r = f(30).stack.split('\\n').length;", "r", "11"),
    ]);
}

#[test]
fn limits_and_a_native_that_calls_back() {
    check(&[
        ("Error.stackTraceLimit = 2; function f(n) { return n ? f(n - 1) : new Error('d'); } var r = f(30).stack.split('\\n').length;", "r", "3"),
        // The error is made inside a callback that a native runs (a nested machine): its frames end at the entry.
        ("var r; [1].forEach(function cb() { r = new Error('c').stack.split('\\n')[1].includes(' cb '); });", "r", "true"),
        // A native that made a plain value before and an error after a callback: no stale frame is read.
        ("var r; try { [1].map(function () { return 1; }).x.y; } catch (e) { r = e.stack.split('\\n')[0]; }", "r", "TypeError: Cannot read properties of undefined (reading 'y')"),
        ("class E extends Error { constructor(m) { super(m); this.name = 'E'; } } function mk() { return new E('q'); } var r = mk().stack.split('\\n').slice(1).some(l => l.includes(' mk '));", "r", "true"),
    ]);
}
