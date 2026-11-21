// Tests of `String.prototype[Symbol.iterator]` and `%StringIteratorPrototype%` (`bi_string_iter.rs`), and of the iterator entry
// points that go through `Symbol.iterator` (`Array.from`, a `String` object, `yield*`, `for await`). Each case is a program, the
// global to read and the expected text. Each runs normally and under constant collection.
use crate::heap::Trigger;
use crate::script::{load, run};

fn ev(src: &str) -> String {
    match run(src) {
        Ok(mut vm) => vm.global_text("r"),
        Err(e) => panic!("{e}\n--- source:\n{src}"),
    }
}

fn ev_gc(src: &str) -> String {
    let mut vm = load(src).unwrap_or_else(|e| panic!("{e}\n--- source:\n{src}"));
    let k = if cfg!(miri) { 4 << 10 } else { 1 << 10 };
    vm.realm.heap.set_trigger(Trigger { min: k, slack: k });
    if let Err(e) = vm.run_main(0) {
        panic!("uncaught {e:?}\n--- source:\n{src}");
    }
    vm.global_text("r")
}

fn check(cases: &[(&str, &str)]) {
    for (src, want) in cases {
        assert_eq!(ev(src), *want, "{src}");
        assert_eq!(ev_gc(src), *want, "under collection: {src}");
    }
}

#[test]
fn the_method_has_the_descriptor_name_and_length() {
    check(&[
        ("var d = Object.getOwnPropertyDescriptor(String.prototype, Symbol.iterator); var r = [typeof d.value, d.writable, d.enumerable, d.configurable].join();", "function,true,false,true"),
        ("var f = String.prototype[Symbol.iterator]; var r = [f.name, f.length, Object.hasOwn(f, 'prototype')].join();", "[Symbol.iterator],0,false"),
        ("var r = Object.getOwnPropertySymbols(String.prototype).map(String).join();", "Symbol(Symbol.iterator)"),
        ("var r; try { new String.prototype[Symbol.iterator](); } catch (e) { r = e instanceof TypeError; }", "true"),
        ("delete String.prototype[Symbol.iterator]; var r = typeof ''[Symbol.iterator] + ',' + (Symbol.iterator in String.prototype);", "undefined,false"),
        ("var f = String.prototype[Symbol.iterator]; var r = f === String.prototype[Symbol.iterator];", "true"),
    ]);
}

#[test]
fn it_walks_code_points_not_code_units() {
    check(&[
        ("var o = []; for (var c of 'a\\u{1F600}b') o.push(c.length); var r = o.join();", "1,2,1"),
        ("var r = [...'a\\u{1F600}b'].length;", "3"),
        ("var r = Array.from('a\\u{1F600}b').length;", "3"),
        ("var r = [...'\\uD83D'].length + ',' + [...'\\uDE00\\uD83D'].length + ',' + [...'\\uD83D\\uD83D\\uDE00'].length;", "1,2,2"),
        ("var r = [...''].length;", "0"),
        ("var [a, b] = 'xy'; var r = a + b;", "xy"),
        ("var r = new Set('abca').size;", "3"),
        ("var r = Array.from('ab', function (c, i) { return c + i; }).join();", "a0,b1"),
    ]);
}

#[test]
fn next_returns_values_then_done_forever() {
    check(&[
        ("var it = 'a\\u{1F600}'[Symbol.iterator](); var a = it.next(), b = it.next(), c = it.next(), d = it.next(); var r = [a.value, a.done, b.value.length, b.done, c.value, c.done, d.value, d.done].join();", "a,false,2,false,,true,,true"),
        ("var it = ''[Symbol.iterator](); var r = it.next().done + ',' + (it[Symbol.iterator]() === it);", "true,true"),
        ("var it = 'ab'[Symbol.iterator](); var r = (it[Symbol.iterator]() === it) + ',' + Object.keys(it).length + ',' + Object.getOwnPropertyNames(it).length;", "true,0,0"),
        ("var it = 12345[Symbol.iterator] === undefined; var f = String.prototype[Symbol.iterator]; var r = [...f.call(12345)].join('-');", "1-2-3-4-5"),
        ("var f = String.prototype[Symbol.iterator]; var r = []; for (var v of [undefined, null]) { try { f.call(v); } catch (e) { r.push(e instanceof TypeError); } } r = r.join();", "true,true"),
        ("var r = [...new String('a\\u{1F600}')].length;", "2"),
        ("var o = { toString: function () { return 'hi'; } }; var r = [...String.prototype[Symbol.iterator].call(o)].join();", "h,i"),
        ("var r; try { String.prototype[Symbol.iterator].call({ toString: function () { throw new RangeError('x'); } }); } catch (e) { r = e instanceof RangeError; }", "true"),
    ]);
}

#[test]
fn the_prototype_is_an_iterator_prototype_with_next_and_a_tag() {
    check(&[
        ("var p = Object.getPrototypeOf(''[Symbol.iterator]()); var r = [Object.hasOwn(p, 'next'), p[Symbol.toStringTag], Object.prototype.toString.call(''[Symbol.iterator]())].join();", "true,String Iterator,[object String Iterator]"),
        ("var p = Object.getPrototypeOf(''[Symbol.iterator]()); var d = Object.getOwnPropertyDescriptor(p, Symbol.toStringTag); var r = [d.writable, d.enumerable, d.configurable].join();", "false,false,true"),
        ("var p = Object.getPrototypeOf(''[Symbol.iterator]()); var d = Object.getOwnPropertyDescriptor(p, 'next'); var r = [d.writable, d.enumerable, d.configurable, p.next.length, p.next.name].join();", "true,false,true,0,next"),
        ("var s = Object.getPrototypeOf(''[Symbol.iterator]()); var a = Object.getPrototypeOf(Object.getPrototypeOf([][Symbol.iterator]())); var r = Object.getPrototypeOf(s) === a;", "true"),
        ("var g = Object.getPrototypeOf(Object.getPrototypeOf((function* () {}).prototype)); var s = Object.getPrototypeOf(''[Symbol.iterator]()); var r = Object.getPrototypeOf(s) === g && g !== Object.prototype;", "true"),
        ("var p = Object.getPrototypeOf(''[Symbol.iterator]()); var r = (p === Object.getPrototypeOf('x'[Symbol.iterator]())) + ',' + (p.next === Object.getPrototypeOf(''[Symbol.iterator]()).next);", "true,true"),
        ("var it = ''[Symbol.iterator](); var o = Object.create(it); var r; try { o.next(); } catch (e) { r = e instanceof TypeError; }", "true"),
        ("var p = Object.getPrototypeOf(''[Symbol.iterator]()); var r = []; for (var v of [undefined, null, {}, 1, 'a', [], function () {}]) { try { p.next.call(v); r.push('no'); } catch (e) { r.push(e instanceof TypeError); } } r = r.join();", "true,true,true,true,true,true,true"),
        ("var p = Object.getPrototypeOf(''[Symbol.iterator]()); var r; try { new p.next(); } catch (e) { r = e instanceof TypeError; }", "true"),
    ]);
}

#[test]
fn a_script_can_change_the_method_and_the_iterator() {
    check(&[
        ("String.prototype[Symbol.iterator] = function () { return [1, 2][Symbol.iterator](); }; var r = [...new String('abc')].join();", "1,2"),
        ("var o = { s: 'ab' }; o[Symbol.iterator] = String.prototype[Symbol.iterator]; var r = [...o].length;", "15"),
        ("var it = 'ab'[Symbol.iterator](); it.x = 1; var a = it.next().value; var r = a + it.next().value + it.x;", "ab1"),
        ("var it = 'ab'[Symbol.iterator](); var p = Object.getPrototypeOf(it); var n = p.next; p.next = function () { return { done: true }; }; var r = [...{ [Symbol.iterator]() { return it; } }].length; p.next = n;", "0"),
    ]);
}

#[test]
fn the_other_entry_points_use_symbol_iterator() {
    check(&[
        ("var r = Array.from(new Set([1, 2, 3])).join();", "1,2,3"),
        ("var r = Array.from(new Map([[1, 2]])).join();", "1,2"),
        ("var r = Array.from(new Map([[1, 2]]).keys()).join();", "1"),
        ("var r = Array.from({ length: 2, 0: 'a', 1: 'b' }).join();", "a,b"),
        ("var r = Array.from((function* () { yield 1; yield 2; })(), function (x) { return x * 2; }).join();", "2,4"),
        ("var r = Array.from({ [Symbol.iterator]: function* () { yield 7; } }).join();", "7"),
        ("var n = 0; var it = { [Symbol.iterator]() { return this; }, next() { return { value: n++, done: n > 5 }; }, return() { n = 100; return {}; } }; try { Array.from(it, function (x) { if (x == 2) throw 1; return x; }); } catch (e) {} var r = n;", "100"),
        ("var r = Array.from([1, 2, 3], function (x) { return x + 1; }).join();", "2,3,4"),
        ("var r = Array.from([3, 4].entries()).join('|');", "0,3|1,4"),
        ("var r; try { Array.from({ [Symbol.iterator]: 5 }); } catch (e) { r = e instanceof TypeError; }", "true"),
        ("var r = Array.from({ [Symbol.iterator]: null, length: 1, 0: 'z' }).join();", "z"),
        ("var r = [...[5, 6].keys()].join() + '|' + [...new Set([1]).entries()].join();", "0,1|1,1"),
        ("function* g() { yield* 'a\\u{1F600}'; } var r = [...g()].map(function (c) { return c.length; }).join();", "1,2"),
        ("var r = Object.prototype.toString.call([][Symbol.iterator]());", "[object Array Iterator]"),
        ("var r = Object.hasOwn(Object.getPrototypeOf([][Symbol.iterator]()), Symbol.iterator);", "false"),
        ("var r = [][Symbol.iterator]()[Symbol.iterator] === Object.getPrototypeOf(Object.getPrototypeOf([][Symbol.iterator]()))[Symbol.iterator];", "true"),
    ]);
}
