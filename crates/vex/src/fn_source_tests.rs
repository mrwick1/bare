// `Function.prototype.toString` of a script function gives its source text (ECMA-262 "Function.prototype.toString", step 2:
// the [[SourceText]] of the function). WhatsApp Web reads it: its Emscripten `cwrap` helper runs `f.toString().match(re).slice(1)`.
// Each case runs normally and under constant collection (`script.rs`, `tests::check`).
use super::tests::check;

#[test]
fn script_functions_give_their_source_text() {
    check(&[
        ("function B(a, b) { return a + b; } var r = B.toString();", "r", "function B(a, b) { return a + b; }"),
        ("var r = (function () {}) + '';", "r", "function () {}"),
        ("var f = function g(x) {\n  return x;\n}; var r = '' + f;", "r", "function g(x) {\n  return x;\n}"),
        ("var r = String((a, b) => a * b);", "r", "(a, b) => a * b"),
        ("var r = String(x => x);", "r", "x => x"),
        ("var r = String(async function h() { await 1; });", "r", "async function h() { await 1; }"),
        ("var r = String(function* gen() { yield 1; });", "r", "function* gen() { yield 1; }"),
        // A function that never ran (its code is still a stub) and one inside it.
        ("function outer() { function inner(q) { return q; } return inner; } var r = String(outer) + '|' + outer().toString();", "r",
         "function outer() { function inner(q) { return q; } return inner; }|function inner(q) { return q; }"),
        // The Emscripten pattern.
        ("var d = /^function\\s*[a-zA-Z$_0-9]*\\s*\\(([a-zA-Z$_0-9,]*)\\)\\s*{\\s*([^]*?)[\\s;]*(?:return\\s*(.*?)[;\\s]*)?}$/;\
          var t = (function(x,y){var s=1;return x+y}).toString().match(d).slice(1); var r = t.join('#');", "r", "x,y#var s=1#x+y"),
    ]);
}

#[test]
fn natives_and_bound_functions_keep_the_native_text() {
    check(&[
        ("var t = Function.prototype.toString; var r = [t.call(Object), t.call(Math.max), t.call((function B() {}).bind(null))].join('|');", "r",
         "function Object() { [native code] }|function max() { [native code] }|function () { [native code] }"),
    ]);
}
