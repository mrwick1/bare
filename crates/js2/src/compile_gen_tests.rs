// Tests of `function*` and `yield` (`compile_gen.rs`, `generator.rs`): each case is a program, the global to read and the expected text. Each
// runs normally and under constant collection (a tiny trigger, so a collection runs at almost every allocation, also at each suspension).
use crate::heap::Trigger;
use crate::script::{load, run};

const MIRI: bool = cfg!(miri);

fn ev(src: &str, name: &str) -> String {
    match run(src) {
        Ok(mut vm) => vm.global_text(name),
        Err(e) => panic!("{e}\n--- source:\n{src}"),
    }
}

fn ev_gc(src: &str, name: &str) -> String {
    let mut vm = load(src).unwrap_or_else(|e| panic!("{e}\n--- source:\n{src}"));
    let k = if MIRI { 4 << 10 } else { 1 << 10 };
    vm.realm.heap.set_trigger(Trigger { min: k, slack: k });
    if let Err(e) = vm.run_main(0) {
        panic!("uncaught {e:?}\n--- source:\n{src}");
    }
    vm.global_text(name)
}

fn check(cases: &[(&str, &str, &str)]) {
    for (src, name, want) in cases {
        assert_eq!(ev(src, name), *want, "{src}");
        if !MIRI {
            assert_eq!(ev_gc(src, name), *want, "under collection: {src}");
        }
    }
}

#[test]
fn next_gives_the_yielded_values_and_the_return_value() {
    check(&[
        (
            "function* g(n) { for (var i = 0; i < n; i++) yield i; return 99; }
             var it = g(3), a = it.next(), b = it.next(), c = it.next(), d = it.next(), e = it.next();
             var r = [a.value, a.done, b.value, c.value, d.value, d.done, e.value, e.done].join();",
            "r",
            "0,false,1,2,99,true,,true",
        ),
        ("function* g() { yield; } var it = g(), a = it.next(), b = it.next(); var r = [a.value, a.done, b.value, b.done].join();", "r", ",false,,true"),
        ("function* g() {} var x = g().next(); var r = [x.value, x.done].join();", "r", ",true"),
    ]);
}

#[test]
fn the_sent_value_is_the_value_of_the_yield() {
    check(&[
        ("function* g() { var x = yield 1; var y = yield x + 1; return x + y; } var it = g(); it.next(); it.next(10); var r = it.next(5).value;", "r", "15"),
        ("function* g() { var a = [yield 1, yield 2]; return a; } var it = g(); it.next(); it.next('x'); var r = it.next('y').value.join();", "r", "x,y"),
        ("function f(a, b, c) { return a + b + c; } function* g() { return f(1, yield 2, 3); } var it = g(); it.next(); var r = it.next(10).value;", "r", "14"),
        ("function* g(a) { return a + (yield) + a; } var it = g(1); it.next(); var r = it.next(5).value;", "r", "7"),
        ("function* g() { var o = {}; o.k = yield 1; return o.k; } var it = g(); it.next(); var r = it.next(8).value;", "r", "8"),
    ]);
}

#[test]
fn throw_goes_to_the_handler_at_the_yield() {
    check(&[
        ("function* g() { try { yield 1; } catch (e) { yield 'c' + e; } return 'end'; } var it = g(); it.next(); var a = it.throw('x').value, b = it.next().value; var r = a + b;", "r", "cxend"),
        ("function* g() { yield 1; } var it = g(); it.next(); var r; try { it.throw('boom'); } catch (e) { r = e; }", "r", "boom"),
        // A variable that the handler reads keeps its value: the thrown value goes to a register of the `yield` only.
        ("function* g() { var x = 'kept'; try { x = yield 1; } catch (e) { return x; } } var it = g(); it.next(); var r = it.throw('t').value;", "r", "kept"),
        // `throw()` on a generator that has not started ends it and throws.
        ("function* g() { yield 1; } var it = g(), r; try { it.throw('early'); } catch (e) { r = e + it.next().done; }", "r", "earlytrue"),
    ]);
}

#[test]
fn return_runs_the_finally_blocks() {
    check(&[
        ("var log = ''; function* g() { try { yield 1; yield 2; } finally { log += 'f'; } } var it = g(); it.next(); var x = it.return(7); var r = x.value + ',' + x.done + log;", "r", "7,truef"),
        // A `finally` with a `yield` suspends the return.
        (
            "function* g() { try { yield 1; } finally { yield 'fin'; } } var it = g(); it.next();
             var a = it.return(5), b = it.next(); var r = [a.value, a.done, b.value, b.done].join();",
            "r",
            "fin,false,5,true",
        ),
        // `return()` before the first `next()` ends the generator and runs nothing.
        ("var log = ''; function* g() { try { log += 'b'; yield 1; } finally { log += 'f'; } } var it = g(); var x = it.return(3); var r = x.value + log + it.next().done;", "r", "3true"),
        // A `return` in the body runs the `finally` blocks too.
        ("var log = ''; function* g() { try { return 4; } finally { log += 'f'; } } var x = g().next(); var r = x.value + log;", "r", "4f"),
        // `return()` out of nested `try` statements runs each `finally`, the inner one first.
        ("var log = ''; function* g() { try { try { yield 1; } finally { log += 'a'; } } finally { log += 'b'; } } var it = g(); it.next(); it.return(); var r = log;", "r", "ab"),
        // A `finally` that returns replaces the value.
        ("function* g() { try { yield 1; } finally { return 'over'; } } var it = g(); it.next(); var r = it.return('v').value;", "r", "over"),
    ]);
}

#[test]
fn a_generator_that_is_done_stays_done() {
    check(&[
        ("function* g() { yield 1; } var it = g(); it.next(); it.next(); var x = it.next(); var y = it.return(5); var r = [x.value, x.done, y.value, y.done].join();", "r", ",true,5,true"),
        ("function* g() { throw 'bad'; } var it = g(), r = ''; try { it.next(); } catch (e) { r += e; } r += it.next().done;", "r", "badtrue"),
        ("var it; function* g() { it.next(); } it = g(); var r; try { it.next(); } catch (e) { r = e instanceof TypeError; }", "r", "true"),
    ]);
}

#[test]
fn for_of_runs_a_generator_and_closes_it_on_exit() {
    check(&[
        ("function* g() { yield 1; yield 2; yield 3; } var s = 0; for (var x of g()) s += x; var r = s;", "r", "6"),
        ("var log = ''; function* g() { try { yield 1; yield 2; } finally { log += 'f'; } } for (var x of g()) { log += x; break; } var r = log;", "r", "1f"),
        ("var log = ''; function* g() { try { yield 1; yield 2; } finally { log += 'f'; } } try { for (var x of g()) { throw 'e'; } } catch (e) { log += e; } var r = log;", "r", "fe"),
        ("var log = ''; function* g() { try { yield 1; yield 2; } finally { log += 'f'; } } function h() { for (var x of g()) { return x; } } var r = h() + log;", "r", "1f"),
        // The `return()` of a generator that a `for-of` runs inside the generator: the inner generator is closed.
        (
            "var log = ''; function* inner() { try { yield 1; yield 2; } finally { log += 'i'; } }
             function* outer() { try { for (var x of inner()) yield x; } finally { log += 'o'; } }
             var it = outer(); it.next(); it.return(); var r = log;",
            "r",
            "io",
        ),
    ]);
}

#[test]
fn the_parameters_are_bound_at_the_call() {
    check(&[
        ("var log = ''; function* g(a = (log += 'd')) { log += 'b'; yield a; } var it = g(); var r1 = log; it.next(); var r = r1 + log;", "r", "ddb"),
        ("function* g(a) { yield a; yield arguments.length; } var it = g(5, 6, 7); var r = it.next().value + ',' + it.next().value;", "r", "5,3"),
        ("function* g() { yield this.v; } var o = { v: 'ov', g: g }; var r = o.g().next().value;", "r", "ov"),
        ("function* g(a) { var f = () => a; yield f(); a = 2; yield f(); } var it = g(1); var r = it.next().value + ',' + it.next().value;", "r", "1,2"),
    ]);
}

#[test]
fn methods_and_expressions() {
    check(&[
        ("var o = { *g() { yield 1; yield 2; } }; var it = o.g(); var r = it.next().value + it.next().value;", "r", "3"),
        ("class C { *g() { yield 'a'; } static *s() { yield 'b'; } } var r = new C().g().next().value + C.s().next().value;", "r", "ab"),
        ("var f = function* () { yield 7; }; var r = f().next().value;", "r", "7"),
        ("var f = function* named() { yield typeof named; }; var r = f().next().value;", "r", "function"),
        ("function* g() { yield 1; } var r; try { new g(); } catch (e) { r = e instanceof TypeError; }", "r", "true"),
        ("function* g() { yield 1; } var it = g(); var r = (Object.getPrototypeOf(it) === g.prototype) + ',' + (!g.prototype.hasOwnProperty('constructor'));", "r", "true,true"),
        ("function* g() { yield new.target; } var r = String(g().next().value);", "r", "undefined"),
    ]);
}

#[test]
fn many_live_values_survive_suspensions() {
    check(&[
        (
            "function* g(a, b) { var c = a + b, d = [a, b, c], e = { c: c }; var s = ''; for (var i = 0; i < 3; i++) { var x = yield i; s += x + d.length + e.c; } return s; }
             var it = g(1, 2); it.next(); it.next('a'); it.next('b'); var r = it.next('c').value;",
            "r",
            "a33b33c33",
        ),
        // A closure over the variables of a generator that suspends.
        ("function* g() { var n = 0; var inc = () => ++n; while (true) { yield inc(); } } var it = g(); it.next(); it.next(); var r = it.next().value;", "r", "3"),
        ("function* g() { let i = 0; for (let j = 0; j < 3; j++) { yield () => j; } } var fs = []; for (var f of g()) fs.push(f); var r = fs.map(f => f()).join();", "r", "0,1,2"),
    ]);
}

#[test]
fn a_generator_that_nobody_reaches_is_freed() {
    // 20,000 suspended generators, each dropped: the heap does not keep their frames.
    let src = "function* g(i) { var a = [i]; yield a; yield a; } var s = 0; for (var i = 0; i < 20000; i++) { var it = g(i); s += it.next().value[0]; } var r = s;";
    assert_eq!(ev(src, "r"), "199990000");
}

#[test]
fn a_generator_function_has_the_prototypes_of_the_spec() {
    check(&[
        (
            "function* g() {} var GF = Object.getPrototypeOf(g), GP = GF.prototype;
             var r = [Object.getPrototypeOf(GF) === Function.prototype, Object.getPrototypeOf(g.prototype) === GP, Object.getOwnPropertyNames(g).join(),
                      Object.prototype.toString.call(g()), Object.prototype.toString.call(g), GP.constructor === GF, GF.hasOwnProperty('prototype')].join('|');",
            "r",
            "true|true|length,name,prototype|[object Generator]|[object GeneratorFunction]|true|true",
        ),
        ("function* g() {} g.prototype = null; var GP = Object.getPrototypeOf(g).prototype; var r = Object.getPrototypeOf(g()) === GP;", "r", "true"),
        ("function f() {} function* g() {} var r = (Object.getPrototypeOf(f) === Function.prototype) + ',' + (Object.getPrototypeOf(g) !== Function.prototype);", "r", "true,true"),
    ]);
}

#[test]
fn the_temporaries_of_a_yield_are_not_roots_after_a_resume() {
    // The operand of `yield` is a temporary that a resume does not bring back. A `return()` runs a `finally` that allocates: a collection
    // there must not read the stale register.
    check(&[(
        "var n = 0; function* g() { try { yield [1, 2, 3].map(x => ({ x })); } finally { for (var i = 0; i < 200; i++) n += ({ i }).i; } }
         var it = g(); it.next(); var x = it.return(5); var r = n + '|' + x.value;",
        "r",
        "19900|5",
    )]);
}
