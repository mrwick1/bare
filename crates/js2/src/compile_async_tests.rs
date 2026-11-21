// Tests of `async function` and `await` (`compile_async.rs`, `generator.rs`): each case is a program, the global to read and the expected text.
// The jobs run after the script (`drain_microtasks`). Each case runs normally and under constant collection (a tiny trigger).
use crate::heap::Trigger;
use crate::script::load;

const MIRI: bool = cfg!(miri);

fn ev(src: &str, name: &str, gc: bool) -> String {
    let mut vm = load(src).unwrap_or_else(|e| panic!("{e}\n--- source:\n{src}"));
    if gc {
        let k = if MIRI { 4 << 10 } else { 1 << 10 };
        vm.realm.heap.set_trigger(Trigger { min: k, slack: k });
    }
    if let Err(e) = vm.run_main(0) {
        panic!("uncaught {e:?}\n--- source:\n{src}");
    }
    vm.drain_microtasks(&mut |_, _| {}).expect("the jobs run");
    vm.global_text(name)
}

fn check(cases: &[(&str, &str, &str)]) {
    for (src, name, want) in cases {
        assert_eq!(ev(src, name, false), *want, "{src}");
        if !MIRI {
            assert_eq!(ev(src, name, true), *want, "under collection: {src}");
        }
    }
}

#[test]
fn an_async_function_returns_a_promise_and_runs_to_the_first_await_at_once() {
    check(&[
        ("var log = []; async function f() { log.push('a'); await 0; log.push('c'); } f(); log.push('b'); var r = log.join();", "r", "a,b"),
        ("var log = []; async function f() { log.push('a'); await 0; log.push('c'); } f(); log.push('b'); Promise.resolve().then(function () { r = log.join(); }); var r;", "r", "a,b,c"),
        ("async function f() { return 1; } var p = f(); var r = (p instanceof Promise) + ',' + typeof p.then;", "r", "true,function"),
    ]);
}

#[test]
fn the_promise_gets_the_return_value() {
    check(&[
        ("async function f() { return 7; } var r; f().then(function (v) { r = v; });", "r", "7"),
        ("async function f() { await 1; return 8; } var r; f().then(function (v) { r = v; });", "r", "8"),
        ("async function f() {} var r = 'x'; f().then(function (v) { r = String(v); });", "r", "undefined"),
        ("async function f() { return Promise.resolve(9); } var r; f().then(function (v) { r = v; });", "r", "9"),
        ("var f = async () => 4; var r; f().then(function (v) { r = v; });", "r", "4"),
        ("var f = async x => x * 2; var r; f(5).then(function (v) { r = v; });", "r", "10"),
    ]);
}

#[test]
fn await_gives_the_value_or_throws_the_reason() {
    check(&[
        ("async function f() { var a = await 1, b = await Promise.resolve(2); return a + b; } var r; f().then(function (v) { r = v; });", "r", "3"),
        ("async function f() { try { await Promise.reject('no'); } catch (e) { return 'c:' + e; } } var r; f().then(function (v) { r = v; });", "r", "c:no"),
        ("async function f() { await Promise.reject('no'); return 1; } var r; f().then(null, function (e) { r = 'rej:' + e; });", "r", "rej:no"),
        ("async function f() { throw 'early'; } var r; f().then(null, function (e) { r = e; });", "r", "early"),
        ("async function f(a = (function () { throw 'param'; })()) {} var r; f().then(null, function (e) { r = e; });", "r", "param"),
        ("async function f() { await 0; throw new Error('late'); } var r; f().then(null, function (e) { r = e.message; });", "r", "late"),
        ("async function f() { var t = { then(ok) { ok('thenable'); } }; return await t; } var r; f().then(function (v) { r = v; });", "r", "thenable"),
    ]);
}

#[test]
fn variables_and_loops_survive_the_waits() {
    check(&[
        ("async function f(n) { var s = 0; for (var i = 0; i < n; i++) s += await i; return s; } var r; f(5).then(function (v) { r = v; });", "r", "10"),
        ("async function f(a, b) { var o = { k: a }; var l = [1, 2]; await 0; o.k += b; l.push(await 3); return o.k + ':' + l.join(); } var r; f('x', 'y').then(function (v) { r = v; });", "r", "xy:1,2,3"),
        ("async function f() { let a = 1; { let a = 2; await 0; a++; var t = a; } return a + ',' + t; } var r; f().then(function (v) { r = v; });", "r", "1,3"),
        ("async function f() { var fs = []; for (let i = 0; i < 3; i++) { await 0; fs.push(() => i); } return fs.map(g => g()).join(); } var r; f().then(function (v) { r = v; });", "r", "0,1,2"),
        ("async function f() { return [await 1, await 2, await 3].join(); } var r; f().then(function (v) { r = v; });", "r", "1,2,3"),
        ("function g(a, b, c) { return a + b + c; } async function f() { return g(1, await 2, 3); } var r; f().then(function (v) { r = v; });", "r", "6"),
    ]);
}

#[test]
fn finally_and_for_of_work_across_a_wait() {
    check(&[
        ("var log = []; async function f() { try { await 0; return 'a'; } finally { log.push('fin'); await 0; log.push('fin2'); } } var r; f().then(function (v) { r = v + log.join(); });", "r", "afin,fin2"),
        ("async function f() { var s = ''; for (var x of [1, 2, 3]) { s += await x; } return s; } var r; f().then(function (v) { r = v; });", "r", "123"),
        ("async function f() { try { try { await Promise.reject('in'); } finally { await 0; } } catch (e) { return 'got ' + e; } } var r; f().then(function (v) { r = v; });", "r", "got in"),
    ]);
}

#[test]
fn methods_and_this_and_arguments() {
    check(&[
        ("var o = { k: 5, async m(x) { await 0; return this.k + x; } }; var r; o.m(2).then(function (v) { r = v; });", "r", "7"),
        ("class C { constructor() { this.k = 1; } async m() { await 0; return this.k; } static async s() { return 's'; } } var r = ''; new C().m().then(function (v) { r += v; }); C.s().then(function (v) { r += v; });", "r", "s1"),
        ("async function f() { await 0; return arguments.length + ':' + arguments[1]; } var r; f(1, 2, 3).then(function (v) { r = v; });", "r", "3:2"),
        ("var o = { k: 3, m() { return (async () => { await 0; return this.k; })(); } }; var r; o.m().then(function (v) { r = v; });", "r", "3"),
        ("async function f(...a) { await 0; return a.length; } var r; f(1, 2).then(function (v) { r = v; });", "r", "2"),
    ]);
}

#[test]
fn the_order_of_the_jobs_is_the_spec_order() {
    check(&[
        // `await p` of a native promise takes one tick; the result promise settles one tick later than a plain `then`.
        (
            "var log = [];
             async function a() { log.push('a1'); await null; log.push('a2'); }
             async function b() { log.push('b1'); await null; log.push('b2'); }
             a(); b();
             Promise.resolve().then(function () { log.push('p1'); }).then(function () { log.push('p2'); });
             log.push('main');
             var r = ''; Promise.resolve().then(function () {}).then(function () {}).then(function () { r = log.join(); });",
            "r",
            "a1,b1,main,a2,b2,p1,p2",
        ),
        (
            "var log = [];
             async function f() { await Promise.resolve(); log.push('f'); }
             f().then(function () { log.push('then'); });
             Promise.resolve().then(function () { log.push('x1'); }).then(function () { log.push('x2'); }).then(function () { log.push('x3'); });
             var r = ''; setTimeout = 0; Promise.resolve().then(function () {}).then(function () {}).then(function () {}).then(function () { r = log.join(); });",
            "r",
            "f,x1,then,x2,x3",
        ),
    ]);
}

#[test]
fn an_async_function_is_no_constructor_and_has_no_prototype() {
    check(&[
        ("async function f() {} var r; try { new f(); r = 'no'; } catch (e) { r = e instanceof TypeError; }", "r", "true"),
        ("async function f() {} var r = f.hasOwnProperty('prototype') + ',' + (f.prototype === undefined);", "r", "false,true"),
        ("async function f() {} var r = Object.prototype.toString.call(f);", "r", "[object AsyncFunction]"),
        ("async function f() {} var r = (Object.getPrototypeOf(f) !== Function.prototype) + ',' + (f instanceof Function);", "r", "true,true"),
    ]);
}

#[test]
fn many_waiting_functions_share_the_queue() {
    check(&[(
        "async function f(i) { await 0; await 0; return i; }
         var ps = []; for (var i = 0; i < 50; i++) ps.push(f(i));
         var r; Promise.all(ps).then(function (a) { r = a.length + ':' + a[49]; });",
        "r",
        "50:49",
    )]);
}

#[test]
fn await_in_every_kind_of_expression_and_statement() {
    check(&[
        ("async function f() { return (await 1) + (await 2) * (await 3); } var r; f().then(function (v) { r = v; });", "r", "7"),
        ("async function f(a) { return a && await 'x' || await 'y'; } var r = ''; f(1).then(function (v) { r += v; }); f(0).then(function (v) { r += v; });", "r", "xy"),
        ("async function f(a) { return a ? await 1 : await 2; } var r = ''; f(1).then(function (v) { r += v; }); f(0).then(function (v) { r += v; });", "r", "12"),
        ("async function f() { return `a${await 1}b${await 2}c`; } var r; f().then(function (v) { r = v; });", "r", "a1b2c"),
        ("async function f() { return await (await Promise.resolve(Promise.resolve(5))); } var r; f().then(function (v) { r = v; });", "r", "5"),
        ("async function f() { var o = { a: await 1, b: [await 2, await 3], c: { d: await 4 } }; return JSON.stringify(o); } var r; f().then(function (v) { r = v; });", "r", "{\"a\":1,\"b\":[2,3],\"c\":{\"d\":4}}"),
        ("async function f() { var { a, b: [c] } = await { a: 1, b: [2] }; var [d, e = await 9] = await [3]; return [a, c, d, e].join(); } var r; f().then(function (v) { r = v; });", "r", "1,2,3,9"),
        ("async function f(x) { switch (await x) { case 1: return 'one'; case await 2: return 'two'; default: return 'other'; } } var r = ''; f(1).then(function (v) { r += v; }); f(2).then(function (v) { r += v; }); f(3).then(function (v) { r += v; });", "r", "onetwoother"),
        ("async function f() { var n = 0; do { n += await 1; } while (n < await 3); var s = ''; for (var k in { a: 1, b: 2 }) s += k + await 0; return n + s; } var r; f().then(function (v) { r = v; });", "r", "3a0b0"),
        ("async function f() { var s = ''; outer: for (var i = 0; i < 3; i++) { for (var j = 0; j < 3; j++) { if (j == 1) continue outer; if (i == 2) break outer; s += await (i + '' + j); } } return s; } var r; f().then(function (v) { r = v; });", "r", "0010"),
        ("async function f() { var a = 1; a += await 2; a **= await 2; var o = { k: 1 }; o.k += await 5; o['k'] *= await 2; return a + ',' + o.k; } var r; f().then(function (v) { r = v; });", "r", "9,12"),
        ("async function f() { var g = async () => (await 1) + 1; return await g() + await g(); } var r; f().then(function (v) { r = v; });", "r", "4"),
        ("var o = { async m() { return (() => super.toString === Object.prototype.toString)(); } }; var r; o.m().then(function (v) { r = v; });", "r", "true"),
        ("class A { v() { return 'A'; } } class B extends A { async v() { await 0; return super.v() + 'B'; } } var r; new B().v().then(function (x) { r = x; });", "r", "AB"),
        ("class A { constructor() { this.k = 'a'; } } class B extends A { async m() { await 0; return (() => this.k)(); } } var r; new B().m().then(function (x) { r = x; });", "r", "a"),
    ]);
}

#[test]
fn a_rejection_that_nobody_reads_does_not_stop_the_other_jobs() {
    check(&[
        ("async function f() { await 0; throw 1; } f(); var r = 'x'; Promise.resolve().then(function () { r = 'ok'; });", "r", "ok"),
        ("async function f() { try { await Promise.reject(1); } catch (e) { return e; } finally { await 0; } } var r; f().then(function (v) { r = v; });", "r", "1"),
        ("async function g() { throw new TypeError('t'); } async function f() { try { await g(); } catch (e) { return e.name; } } var r; f().then(function (v) { r = v; });", "r", "TypeError"),
    ]);
}

#[test]
fn an_async_function_in_a_block_is_not_hoisted_to_the_function() {
    check(&[
        ("var r; try { { async function x() {} } x; r = 'seen'; } catch (e) { r = e.name; }", "r", "ReferenceError"),
        ("var r; { function* g() {} } r = typeof g;", "r", "undefined"),
        ("var r; { function f() {} } r = typeof f;", "r", "function"),
    ]);
}
