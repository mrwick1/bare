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

// ---- the iterator protocol (step G3, `iter.rs`) ----

/// An iterable that counts: `mk(n)` gives `n` values `0..n`, `closed` counts the calls of `return`.
fn c(src: &str) -> String {
    format!("{COUNTER} {src}")
}

const COUNTER: &str = "var closed = 0, steps = 0;
    function mk(n, ret) { return { [Symbol.iterator]() { var i = 0; var o = { next() { steps++; return i < n ? { value: i++, done: false } : { value: 'end', done: true }; } };
        if (ret !== 0) o.return = ret || function () { closed++; return {}; }; return o; } }; }";

#[test]
fn for_of_runs_a_generic_iterator_and_closes_it_on_an_early_exit() {
    check(&[
        (&c("var s = []; for (var v of mk(3)) s.push(v); var r = s.join() + '|' + closed + '|' + steps;"), "r", "0,1,2|0|4"),
        (&c("var s = []; for (var v of mk(5)) { s.push(v); if (v == 1) break; } var r = s.join() + '|' + closed;"), "r", "0,1|1"),
        (&c("function f() { for (var v of mk(5)) { if (v == 2) return v; } } var r = f() + '|' + closed;"), "r", "2|1"),
        (&c("var r; try { for (var v of mk(5)) { throw 'boom'; } } catch (e) { r = e + '|' + closed; }"), "r", "boom|1"),
        // `continue` does not close. A labelled break out of two loops closes both.
        (&c("var s = 0; for (var v of mk(4)) { if (v % 2) continue; s += v; } var r = s + '|' + closed;"), "r", "2|0"),
        (&c("a: for (var x of mk(3)) { for (var y of mk(3)) { break a; } } var r = closed;"), "r", "2"),
        // The original throw wins over a `return()` that throws. A `return()` that throws on a break is the error.
        (&c("var r; try { for (var v of mk(3, function () { throw 'ret'; })) throw 'body'; } catch (e) { r = e; }"), "r", "body"),
        (&c("var r; try { for (var v of mk(3, function () { throw 'ret'; })) break; } catch (e) { r = e; }"), "r", "ret"),
        (&c("var r; try { for (var v of mk(3, function () { return 1; })) break; } catch (e) { r = e instanceof TypeError; }"), "r", "true"),
        // No `return`: nothing to call. A `next` that throws does not close.
        (&c("var s = []; for (var v of mk(3, 0)) { s.push(v); break; } var r = s.join();"), "r", "0"),
        (
            "var closed = 0; var it = { [Symbol.iterator]() { return { next() { throw 'nx'; }, return() { closed++; return {}; } }; } };
             var r; try { for (var v of it) ; } catch (e) { r = e + '|' + closed; }",
            "r",
            "nx|0",
        ),
    ]);
}

#[test]
fn for_of_with_a_value_that_is_no_iterable_is_a_type_error() {
    check(&[
        ("var r = []; for (var x of [undefined, null, 1, true, {}, { [Symbol.iterator]: 1 }, { [Symbol.iterator]() { return 1; } }, { [Symbol.iterator]() { return {}; } }]) { try { for (var v of x) ; r.push('no error'); } catch (e) { r.push(e.constructor.name); } } var r = r.join();", "r", "TypeError,TypeError,TypeError,TypeError,TypeError,TypeError,TypeError,TypeError"),
        ("var it = { [Symbol.iterator]() { return { next() { return 1; } }; } }; var r; try { for (var v of it) ; } catch (e) { r = e.constructor.name; }", "r", "TypeError"),
    ]);
}

#[test]
fn destructuring_and_spread_use_the_iterator_protocol() {
    check(&[
        (&c("var [a, b] = mk(5); var r = a + ',' + b + '|' + closed + '|' + steps;"), "r", "0,1|1|2"),
        (&c("var [a, b, c, d] = mk(2); var r = [a, b, c, d].join() + '|' + closed;"), "r", "0,1,,|0"),
        (&c("var [a, ...rest] = mk(4); var r = a + '|' + rest.join() + '|' + closed;"), "r", "0|1,2,3|0"),
        (&c("var [a, , c] = mk(9); var r = a + ',' + c + '|' + closed;"), "r", "0,2|1"),
        (&c("var a, b; [a, b] = mk(3); var r = a + ',' + b;"), "r", "0,1"),
        // A throw while it binds closes the iterator; a `next` that throws does not.
        (&c("var r; try { var [{ x }] = mk(1); var [y = (() => { throw 'd'; })()] = { [Symbol.iterator]() { return { next() { return { value: undefined, done: false }; }, return() { closed++; return {}; } }; } }; } catch (e) { r = e + '|' + closed; }"), "r", "d|2"),
        // A pattern in the head of a `for-of` that throws closes the iterator of the loop.
        ("var n = 0, k = 0, x = { set a(_) { throw 'T'; } }; var it = { [Symbol.iterator]() { return { next() { return { done: false, value: [0] }; }, return() { k++; return {}; } }; } }; function f() { for ([x.a] of it) { n++; } } var r; try { f(); } catch (e) { r = e + '|' + n + '|' + k; }", "r", "T|0|1"),
        (&c("var s = [...mk(3), ...'hi', ...[7, 8], ...mk(0)]; var r = s.join();"), "r", "0,1,2,h,i,7,8"),
        (&c("function f() { return arguments.length; } var r = f(...mk(4), 9) + ',' + Math.max(...mk(5));"), "r", "5,4"),
        (&c("function C() { this.n = arguments.length; } var r = new C(...mk(3)).n;"), "r", "3"),
        ("function* g() { yield 1; yield 2; } var [a, b] = g(); var s = [...g(), ...g()]; var r = a + ',' + b + '|' + s.join();", "r", "1,2|1,2,1,2"),
        ("var r; try { [...1]; } catch (e) { r = e.constructor.name; }", "r", "TypeError"),
        ("var r; try { var [a] = undefined; } catch (e) { r = e.constructor.name; }", "r", "TypeError"),
    ]);
}

#[test]
fn yield_star_delegates_values_and_gives_the_return_value() {
    check(&[
        ("function* i() { yield 1; yield 2; return 'r'; } function* o() { var x = yield* i(); yield x; yield* [3, 4]; yield* 'ab'; return 'e'; } var s = [], it = o(), x; while (!(x = it.next()).done) s.push(x.value); var r = s.join() + '|' + x.value;", "r", "1,2,r,3,4,a,b|e"),
        (&c("function* o() { var x = yield* mk(3); yield x; } var r = [...o()].join();"), "r", "0,1,2,end"),
        // A sent value goes to the inner iterator.
        ("function* i() { var a = yield 1; var b = yield a; return a + b; } function* o() { var x = yield* i(); yield x; } var it = o(); it.next(); var p = it.next('A').value; var q = it.next('B').value; var r = p + q;", "r", "AAB"),
        // `yield*` is an expression with a value; the delegate may yield nothing.
        ("function* e() {} function* o() { var x = yield* e(); yield (x === undefined); } var r = o().next().value;", "r", "true"),
        ("function* i() { yield 1; } function* o() { yield* i(); yield* i(); } var r = [...o()].join();", "r", "1,1"),
        // The registers that o keeps survive each suspension of the delegate.
        ("function* i() { yield 1; yield 2; } function* o() { var a = 10, b = [20], c = 'c'; for (var v of i()) { yield* i(); yield a + b[0]; } return c; } var r = [...o()].join();", "r", "1,2,30,1,2,30"),
    ]);
}

#[test]
fn yield_star_passes_throw_and_return_to_the_iterator() {
    check(&[
        // `throw()` goes in; the inner generator catches it and goes on.
        ("function* i() { try { yield 1; } catch (e) { yield 'caught ' + e; } yield 3; } function* o() { yield* i(); } var it = o(); it.next(); var a = it.throw('x').value, b = it.next().value; var r = a + '|' + b;", "r", "caught x|3"),
        // An inner that does not catch it passes the throw on to the outer, which catches it.
        ("function* i() { yield 1; } function* o() { try { yield* i(); } catch (e) { yield 'outer ' + e; } } var it = o(); it.next(); var r = it.throw('x').value;", "r", "outer x"),
        // `return()` goes in: the inner `finally` runs, then the outer `finally`.
        ("var log = []; function* i() { try { yield 1; } finally { log.push('inner'); } } function* o() { try { yield* i(); } finally { log.push('outer'); } } var it = o(); it.next(); var x = it.return('v'); var r = log.join() + '|' + x.value + '|' + x.done;", "r", "inner,outer|v|true"),
        // An iterator without `return` ends the delegation with the value.
        (&c("function* o() { yield* mk(5, 0); } var it = o(); it.next(); var x = it.return('v'); var r = x.value + '|' + x.done;"), "r", "v|true"),
        // With `return`, it is called with the value.
        ("var got; function* o() { yield* { [Symbol.iterator]() { return { next() { return { value: 1, done: false }; }, return(v) { got = v; return { value: 'rv', done: true }; } }; } }; } var it = o(); it.next(); var x = it.return('v'); var r = got + '|' + x.value + '|' + x.done;", "r", "v|rv|true"),
        // No `throw` method: the iterator is closed and the outer gets a TypeError.
        ("var closed = 0; function* o() { yield* { [Symbol.iterator]() { return { next() { return { value: 1, done: false }; }, return() { closed++; return {}; } }; } }; } var it = o(), r; it.next(); try { it.throw('x'); } catch (e) { r = e.constructor.name + '|' + closed; }", "r", "TypeError|1"),
        // An array has no throw: a TypeError.
        ("function* o() { yield* [1, 2]; } var it = o(), r; it.next(); try { it.throw('x'); } catch (e) { r = e.constructor.name; }", "r", "TypeError"),
        // A `yield*` over a value that is no iterable.
        ("function* o() { yield* 5; } var r; try { o().next(); } catch (e) { r = e.constructor.name; }", "r", "TypeError"),
    ]);
}

#[test]
fn generators_inherit_from_the_iterator_prototype() {
    check(&[
        (
            "function* g() {} var GP = Object.getPrototypeOf(Object.getPrototypeOf(g())), IP = Object.getPrototypeOf(GP.constructor.prototype === undefined ? GP : GP);
             var it = g(); var r = [typeof IP[Symbol.iterator], it[Symbol.iterator]() === it, Object.getPrototypeOf(IP) === Object.prototype].join();",
            "r",
            "function,true,true",
        ),
        ("function* g() { yield 1; yield 2; } var o = { [Symbol.iterator]: g }; var r = [...o].join();", "r", "1,2"),
        ("function* g() { yield 1; yield 2; } var o = { [Symbol.iterator]() { return g(); } }; var s = 0; for (var v of o) s += v; var r = s;", "r", "3"),
    ]);
}

#[test]
fn the_temporaries_of_a_yield_are_not_roots_after_a_resume() {
    // The operand of `yield` (and the iterable of `yield*`) is a temporary that a resume does not bring back. A `return()` runs a `finally`
    // that allocates: a collection there must not read the stale register.
    check(&[
        (
            "var n = 0; function* g() { try { yield [1, 2, 3].map(x => ({ x })); } finally { for (var i = 0; i < 200; i++) n += ({ i }).i; } }
             var it = g(); it.next(); var x = it.return(5); var r = n + '|' + x.value;",
            "r",
            "19900|5",
        ),
        (
            "var n = 0; function* i() { yield 1; } function* g() { try { yield* [{ a: 1 }, { a: 2 }].map(x => x.a); } finally { for (var i = 0; i < 200; i++) n += ({ i }).i; } }
             var it = g(); it.next(); var x = it.return(5); var r = n + '|' + x.value;",
            "r",
            "19900|5",
        ),
    ]);
}
