// Tests of async generators, `for await` and the async-from-sync iterator (`asyncgen.rs`, `compile_asyncgen.rs`): each case is a program, the
// global to read and the expected text. The jobs run after the script. Each case runs normally and under constant collection.
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
fn an_async_generator_gives_promises_of_results() {
    check(&[
        ("async function* g() { yield 1; yield 2; } var it = g(); var r = typeof it.next().then + ',' + (it.next() instanceof Promise);", "r", "function,true"),
        ("async function* g() { yield 1; yield 2; return 3; } var r = [], it = g(); it.next().then(function (x) { r.push(x.value + ':' + x.done); return it.next(); }).then(function (x) { r.push(x.value + ':' + x.done); return it.next(); }).then(function (x) { r.push(x.value + ':' + x.done); return it.next(); }).then(function (x) { r.push(x.value + ':' + x.done); r = r.join(); });", "r", "1:false,2:false,3:true,undefined:true"),
        ("var log = []; async function* g() { log.push('body'); yield 1; } var it = g(); log.push('called'); it.next(); log.push('next'); var r; Promise.resolve().then(function () { r = log.join(); });", "r", "called,body,next"),
        ("async function* g(a, b) { yield a + b; } var r; g(1, 2).next().then(function (x) { r = x.value; });", "r", "3"),
    ]);
}

#[test]
fn yield_waits_for_its_operand() {
    check(&[
        ("async function* g() { yield Promise.resolve(5); } var r; g().next().then(function (x) { r = x.value; });", "r", "5"),
        ("async function* g() { var x = yield 1; yield x * 2; } var it = g(), r; it.next().then(function () { return it.next(21); }).then(function (x) { r = x.value; });", "r", "42"),
        ("async function* g() { yield Promise.reject('no'); } var r; g().next().then(null, function (e) { r = 'rej:' + e; });", "r", "rej:no"),
        ("async function* g() { try { yield Promise.reject('no'); } catch (e) { yield 'caught ' + e; } } var r; var it = g(); it.next().then(function (x) { r = x.value; });", "r", "caught no"),
    ]);
}

#[test]
fn await_and_return_in_the_body() {
    check(&[
        ("async function* g() { var a = await 1, b = await Promise.resolve(2); yield a + b; } var r; g().next().then(function (x) { r = x.value; });", "r", "3"),
        ("async function* g() { return Promise.resolve(7); } var r; g().next().then(function (x) { r = x.value + ':' + x.done; });", "r", "7:true"),
        ("async function* g() { throw 'boom'; } var r; g().next().then(null, function (e) { r = e; });", "r", "boom"),
        ("async function* g() { await 0; throw 'late'; } var r; g().next().then(null, function (e) { r = e; });", "r", "late"),
        ("async function* g() { return Promise.reject('rr'); } var r; g().next().then(null, function (e) { r = e; });", "r", "rr"),
        ("async function* g() { yield 1; } var it = g(), r = []; it.next().then(function () { return it.next(); }).then(function (x) { r.push(x.done); return it.next(); }).then(function (x) { r.push(x.done); r = r.join(); });", "r", "true,true"),
    ]);
}

#[test]
fn requests_wait_in_a_queue() {
    check(&[
        ("async function* g() { yield 1; yield 2; yield 3; } var it = g(), r = [], ps = [it.next(), it.next(), it.next(), it.next()]; Promise.all(ps).then(function (xs) { r = xs.map(function (x) { return x.value + ':' + x.done; }).join(); }); ", "r", "1:false,2:false,3:false,undefined:true"),
        ("async function* g() { yield await Promise.resolve('a'); yield 'b'; } var it = g(), r; Promise.all([it.next(), it.next(), it.next()]).then(function (xs) { r = xs.map(function (x) { return x.value; }).join(); });", "r", "a,b,"),
        ("var log = []; async function* g() { try { yield 1; yield 2; } finally { log.push('fin'); } } var it = g(); it.next(); var p = it.return('x'); var q = it.next(); var r; Promise.all([p, q]).then(function (xs) { r = log.join() + '|' + xs.map(function (x) { return x.value + ':' + x.done; }).join(); });", "r", "fin|x:true,undefined:true"),
    ]);
}

#[test]
fn return_and_throw_requests() {
    check(&[
        ("async function* g() { yield 1; } var r; g().return(5).then(function (x) { r = x.value + ':' + x.done; });", "r", "5:true"),
        ("async function* g() { yield 1; } var r; g().return(Promise.resolve(6)).then(function (x) { r = x.value + ':' + x.done; });", "r", "6:true"),
        ("async function* g() { yield 1; } var r; g().return(Promise.reject('rj')).then(null, function (e) { r = 'rej:' + e; });", "r", "rej:rj"),
        ("async function* g() { yield 1; } var r; g().throw('t').then(null, function (e) { r = 'rej:' + e; });", "r", "rej:t"),
        ("var log = []; async function* g() { try { yield 1; } finally { log.push('fin'); } } var it = g(), r; it.next().then(function () { return it.return(9); }).then(function (x) { r = log.join() + ':' + x.value + ':' + x.done; });", "r", "fin:9:true"),
        ("async function* g() { try { yield 1; } catch (e) { yield 'c' + e; } } var it = g(), r; it.next().then(function () { return it.throw('E'); }).then(function (x) { r = x.value + ':' + x.done; });", "r", "cE:false"),
        ("async function* g() { try { yield 1; } finally { await 0; } } var it = g(), r; it.next().then(function () { return it.return(Promise.resolve('p')); }).then(function (x) { r = x.value; });", "r", "p"),
        ("async function* g() { yield 1; } var r = []; var it = g(); it.next().then(function () { return it.next(); }).then(function () { return it.return(4); }).then(function (x) { r.push(x.value + ':' + x.done); return it.throw('z'); }).then(null, function (e) { r.push(e); r = r.join(); });", "r", "4:true,z"),
    ]);
}

#[test]
fn a_call_on_the_wrong_object_rejects() {
    check(&[
        ("async function* g() {} var r; var p = g().next.call({}); p.then(null, function (e) { r = e instanceof TypeError; });", "r", "true"),
        ("function* s() {} async function* g() {} var r; var p = g().next.call(s()); p.then(null, function (e) { r = e instanceof TypeError; });", "r", "true"),
        ("async function* g() {} var r; try { s = (function* () {})().next.call(g()); } catch (e) { r = e instanceof TypeError; }", "r", "true"),
    ]);
}

#[test]
fn the_objects_around_async_generators() {
    check(&[
        ("async function* g() {} var P = Object.getPrototypeOf(g), r = [typeof g.prototype, Object.getPrototypeOf(g.prototype) === P.prototype, P[Symbol.toStringTag], P.prototype[Symbol.toStringTag], Object.prototype.toString.call(g())].join();", "r", "object,true,AsyncGeneratorFunction,AsyncGenerator,[object AsyncGenerator]"),
        ("async function* g() {} var AI = Object.getPrototypeOf(Object.getPrototypeOf(g.prototype)); var r = [typeof AI[Symbol.asyncIterator], AI[Symbol.asyncIterator].call(5)].join();", "r", "function,5"),
        ("async function* g() {} var it = g(); var r = (it[Symbol.asyncIterator]() === it) + ',' + (typeof it[Symbol.iterator]);", "r", "true,undefined"),
        ("var o = { async *m() { yield 1; } }; class C { static async *s() { yield 2; } async *i() { yield 3; } } var r = []; o.m().next().then(function (x) { r.push(x.value); return C.s().next(); }).then(function (x) { r.push(x.value); return new C().i().next(); }).then(function (x) { r.push(x.value); r = r.join(); });", "r", "1,2,3"),
        ("var f = async function* () { yield 1; }; var r; f().next().then(function (x) { r = x.value; });", "r", "1"),
        ("async function* g() {} var r; try { new g(); } catch (e) { r = e instanceof TypeError; }", "r", "true"),
    ]);
}

#[test]
fn for_await_over_async_generators_and_sync_iterables() {
    check(&[
        ("async function* g() { yield 1; yield 2; yield 3; } var r = []; (async function () { for await (var x of g()) r.push(x); r = r.join(); })();", "r", "1,2,3"),
        ("var r = []; (async function () { for await (var x of [1, Promise.resolve(2), 3]) r.push(x); r = r.join(); })();", "r", "1,2,3"),
        ("function* s() { yield 1; yield Promise.resolve(2); } var r = []; (async function () { for await (let x of s()) r.push(x); r = r.join(); })();", "r", "1,2"),
        ("var r = []; (async function () { try { for await (var x of [Promise.reject('bad')]) r.push(x); } catch (e) { r.push('c:' + e); } r = r.join(); })();", "r", "c:bad"),
        ("var r; (async function () { try { for await (var x of 5) ; } catch (e) { r = e instanceof TypeError; } })();", "r", "true"),
        ("var r = []; var it = { [Symbol.asyncIterator]() { var i = 0; return { next() { return Promise.resolve({ value: i, done: i++ >= 2 }); } }; } }; (async function () { for await (var x of it) r.push(x); r = r.join(); })();", "r", "0,1"),
        ("var r = []; (async function () { for await (var [a, b] of [[1, 2], [3, 4]]) r.push(a + b); r = r.join(); })();", "r", "3,7"),
        ("async function* g() { yield [1]; } var r = []; (async function () { for await (const { length } of g()) r.push(length); r = r.join(); })();", "r", "1"),
        ("var r = []; (async function () { var i = 0; for await (var x of [1, 2, 3]) { if (x == 2) continue; r.push(x); } r = r.join(); })();", "r", "1,3"),
        ("var r = []; (async function () { var n = 0; l: for await (var x of [1, 2]) for await (var y of [10, 20]) { if (y == 20) continue l; r.push(x + y); } r = r.join(); })();", "r", "11,12"),
    ]);
}

#[test]
fn leaving_a_for_await_loop_closes_the_iterator() {
    check(&[
        ("var log = []; var it = { [Symbol.asyncIterator]() { return { next() { return Promise.resolve({ value: 1, done: false }); }, return() { log.push('ret'); return Promise.resolve({}); } }; } }; var r; (async function () { for await (var x of it) { break; } log.push('after'); r = log.join(); })();", "r", "ret,after"),
        ("var log = []; var it = { [Symbol.asyncIterator]() { return { next() { return Promise.resolve({ value: 1, done: false }); }, return() { log.push('ret'); return { then(ok) { log.push('then'); ok({}); } }; } }; } }; var out; (async function () { for await (var x of it) { break; } log.push('after'); out = log.join(); })();", "out", "ret,then,after"),
        ("var log = []; var it = { [Symbol.asyncIterator]() { return { next() { return Promise.resolve({ value: 1, done: false }); }, return() { log.push('ret'); return Promise.resolve({}); } }; } }; var r; (async function () { try { for await (var x of it) { throw 'e'; } } catch (e) { log.push('c:' + e); } r = log.join(); })();", "r", "ret,c:e"),
        ("var log = []; var it = { [Symbol.asyncIterator]() { return { next() { return Promise.resolve({ value: 1, done: false }); }, return() { log.push('ret'); return Promise.reject('closing'); } }; } }; var r; (async function () { try { for await (var x of it) { throw 'e'; } } catch (e) { log.push('c:' + e); } r = log.join(); })();", "r", "ret,c:e"),
        ("var log = []; var it = { [Symbol.asyncIterator]() { return { next() { return Promise.resolve({ value: 1, done: false }); }, return() { log.push('ret'); return Promise.reject('closing'); } }; } }; var r; (async function () { try { for await (var x of it) { break; } } catch (e) { log.push('c:' + e); } r = log.join(); })();", "r", "ret,c:closing"),
        ("var log = []; var it = { [Symbol.asyncIterator]() { return { next() { return Promise.resolve({ value: 1, done: false }); }, return() { log.push('ret'); return 1; } }; } }; var r; (async function () { try { for await (var x of it) { break; } } catch (e) { log.push(e instanceof TypeError); } r = log.join(); })();", "r", "ret,true"),
        ("var log = []; var it = { [Symbol.asyncIterator]() { return { next() { return Promise.resolve({ value: 1, done: false }); }, return() { log.push('ret'); return Promise.resolve({}); } }; } }; var r; (async function f() { for await (var x of it) { return 'v'; } })().then(function (v) { r = log.join() + ':' + v; });", "r", "ret:v"),
        ("var log = []; async function* g() { try { yield 1; yield 2; } finally { log.push('fin'); } } var r; (async function () { for await (var x of g()) { break; } r = log.join(); })();", "r", "fin"),
        ("var log = []; function* s() { try { yield 1; yield 2; } finally { log.push('fin'); } } var r; (async function () { for await (var x of s()) { break; } r = log.join(); })();", "r", "fin"),
    ]);
}

#[test]
fn yield_star_in_an_async_generator() {
    check(&[
        ("async function* a() { yield 1; yield 2; return 'ra'; } async function* g() { var v = yield* a(); yield v; } var r = []; (async function () { for await (var x of g()) r.push(x); r = r.join(); })();", "r", "1,2,ra"),
        ("async function* g() { yield* [1, Promise.resolve(2)]; yield* [3]; } var r = []; (async function () { for await (var x of g()) r.push(x); r = r.join(); })();", "r", "1,2,3"),
        ("function* s() { yield 1; return 'rs'; } async function* g() { var v = yield* s(); yield v; } var r = []; (async function () { for await (var x of g()) r.push(x); r = r.join(); })();", "r", "1,rs"),
        ("async function* a() { try { yield 1; yield 2; } finally { log.push('afin'); } } var log = []; async function* g() { yield* a(); } var it = g(), r; it.next().then(function () { return it.return('stop'); }).then(function (x) { r = log.join() + ':' + x.value + ':' + x.done; });", "r", "afin:stop:true"),
        ("async function* a() { try { yield 1; } catch (e) { yield 'in:' + e; } } async function* g() { yield* a(); } var it = g(), r; it.next().then(function () { return it.throw('T'); }).then(function (x) { r = x.value; });", "r", "in:T"),
        ("var it0 = { [Symbol.asyncIterator]() { return { next() { return { value: 1, done: false }; } }; } }; async function* g() { yield* it0; } var r; var it = g(); it.next().then(function () { return it.throw('x'); }).then(null, function (e) { r = e instanceof TypeError; });", "r", "true"),
        ("var log = []; var it0 = { [Symbol.asyncIterator]() { return { next() { return { value: 1, done: false }; }, return() { log.push('ret'); return {}; } }; } }; async function* g() { yield* it0; } var r; var it = g(); it.next().then(function () { return it.throw('x'); }).then(null, function (e) { r = log.join() + ':' + (e instanceof TypeError); });", "r", "ret:true"),
        ("var it0 = { [Symbol.asyncIterator]() { return { next() { return { value: 1, done: false }; } }; } }; async function* g() { yield* it0; } var r; var it = g(); it.next().then(function () { return it.return(Promise.resolve('pv')); }).then(function (x) { r = x.value + ':' + x.done; });", "r", "pv:true"),
        ("async function* g() { var r = yield* (async function* () { return 5; })(); yield r; } var out; g().next().then(function (x) { out = x.value; });", "out", "5"),
    ]);
}

#[test]
fn an_async_from_sync_iterator_closes_the_sync_iterator() {
    check(&[
        ("var log = []; var it0 = { [Symbol.iterator]() { return { next() { return { value: Promise.reject('rv'), done: false }; }, return() { log.push('ret'); return {}; } }; } }; var r; (async function () { try { for await (var x of it0) ; } catch (e) { log.push('c:' + e); } r = log.join(); })();", "r", "ret,c:rv"),
        ("var it0 = { [Symbol.iterator]() { return { next() { return { value: 1, done: true }; } }; } }; var r = []; (async function () { for await (var x of it0) r.push(x); r.push('end'); r = r.join(); })();", "r", "end"),
        ("var args = []; var it0 = { [Symbol.iterator]() { return { next() { args.push(arguments.length); return { value: 1, done: args.length > 1 }; } }; } }; var r; (async function () { for await (var x of it0) ; r = args.join(); })();", "r", "0,0"),
    ]);
}

#[test]
fn the_async_function_constructors_build_functions_that_run() {
    check(&[
        (
            "var A = Object.getPrototypeOf(async function () {}).constructor; var f = A('x', 'return await x + 1;'); var r = ''; \
             f(1).then(v => r = f.name + v);",
            "r",
            "anonymous2",
        ),
        (
            "var AG = Object.getPrototypeOf(async function* () {}).constructor; var f = AG('yield 1; yield 2;'); var r = []; \
             (async () => { for await (var v of f()) r.push(v); r = r.join(); })();",
            "r",
            "1,2",
        ),
    ]);
}
