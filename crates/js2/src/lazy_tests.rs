// Tests of the lazy tables (`lazy.rs`): each case is a program that makes its first look at a built-in object with the operation under
// test, the global to read and the expected text. Each runs normally and under constant collection. The differential test compares a
// realm that makes every table entry at the start with a realm that makes them at their first use.
use crate::heap::Trigger;
use crate::script::{load, run};

fn ev(src: &str, name: &str) -> String {
    match run(src) {
        Ok(mut vm) => vm.global_text(name),
        Err(e) => panic!("{e}\n--- source:\n{src}"),
    }
}

fn ev_gc(src: &str, name: &str) -> String {
    let mut vm = load(src).unwrap_or_else(|e| panic!("{e}\n--- source:\n{src}"));
    let k = if cfg!(miri) { 4 << 10 } else { 1 << 10 };
    vm.realm.heap.set_trigger(Trigger { min: k, slack: k });
    if let Err(e) = vm.run_main(0) {
        panic!("uncaught {e:?}\n--- source:\n{src}");
    }
    vm.global_text(name)
}

fn check(cases: &[(&str, &str)]) {
    for (src, want) in cases {
        assert_eq!(ev(src, "r"), *want, "{src}");
        assert_eq!(ev_gc(src, "r"), *want, "under collection: {src}");
    }
}

/// The descriptor of `o[k]` as text.
const DESC: &str = "function d(o, k) { var x = Object.getOwnPropertyDescriptor(o, k); return x ? [typeof x.value, x.writable, x.enumerable, x.configurable].join() : 'none'; }";

#[test]
fn has_own_property_and_in_see_a_name_that_is_not_made() {
    check(&[
        ("var r = Object.hasOwn(Math, 'floor') + ',' + Object.hasOwn(Math, 'nope');", "true,false"),
        ("var r = Math.hasOwnProperty('floor') + ',' + Math.hasOwnProperty('PI');", "true,true"),
        ("var r = ('floor' in Math) + ',' + ('nope' in Math) + ',' + ('push' in []) + ',' + ('toString' in {});", "true,false,true,true"),
        ("var r = Object.hasOwn(Array.prototype, 'push') + ',' + Object.hasOwn(Array, 'isArray') + ',' + Object.hasOwn(JSON, 'parse');", "true,true,true"),
        ("var r = Object.hasOwn(globalThis, 'parseInt') + ',' + Object.hasOwn(Number, 'parseInt') + ',' + Object.hasOwn(Number, 'EPSILON');", "true,true,true"),
        ("var r = Object.hasOwn(Object.prototype, 'hasOwnProperty') + ',' + Object.hasOwn(Function.prototype, 'call') + ',' + Object.hasOwn(String.prototype, 'trim');", "true,true,true"),
    ]);
}

#[test]
fn a_deleted_name_stays_deleted() {
    check(&[
        ("var r = [delete Math.floor, typeof Math.floor, 'floor' in Math, Object.hasOwn(Math, 'floor')].join();", "true,undefined,false,false"),
        ("delete Math.floor; Object.getOwnPropertyNames(Math); var r = typeof Math.floor + ',' + Object.hasOwn(Math, 'floor');", "undefined,false"),
        ("delete Math.floor; Object.freeze(Math); var r = typeof Math.floor + ',' + typeof Math.ceil;", "undefined,function"),
        ("var r = [delete Array.prototype.push, typeof [].push, delete Array.prototype.push].join();", "true,undefined,true"),
        ("var r = [delete Math.PI, delete Number.MAX_VALUE].join();", "false,false"),
        ("var r = [delete globalThis.parseInt, typeof parseInt, typeof Number.parseInt].join();", "true,undefined,function"),
    ]);
}

#[test]
fn set_then_delete_does_not_bring_the_original_back() {
    check(&[
        ("Array.prototype.push = 1; var a = Array.prototype.push; delete Array.prototype.push; var r = a + ',' + typeof [].push;", "1,undefined"),
        ("Math.floor = 7; var r = Math.floor + ',' + Object.keys(Math).length;", "7,0"),
        (&format!("{DESC} Math.floor = 7; var r = d(Math, 'floor');"), "number,true,false,true"),
        (&format!("{DESC} Math.PI = 7; var r = Math.PI + ',' + d(Math, 'PI');"), "3.141592653589793,number,false,false,false"),
        ("Object.prototype.toString = 5; var r = Object.prototype.toString + ',' + typeof [].toString;", "5,function"),
    ]);
}

#[test]
fn define_property_starts_from_the_made_property() {
    check(&[
        (&format!("{DESC} Object.defineProperty(Math, 'floor', {{ value: 1 }}); var r = Math.floor + ',' + d(Math, 'floor');"), "1,number,true,false,true"),
        (&format!("{DESC} Object.defineProperty(Array.prototype, 'push', {{ enumerable: true }}); var r = d(Array.prototype, 'push') + ',' + Object.keys(Array.prototype).join();"), "function,true,true,true,push"),
        (&format!("{DESC} Object.defineProperty(JSON, 'parse', {{ get: function () {{ return 9; }} }}); var r = JSON.parse;"), "9"),
        ("var r; try { Object.defineProperty(Math, 'E', { value: 1 }); r = 'no'; } catch (e) { r = e.name; }", "TypeError"),
        ("var r = Object.defineProperty(Math, 'E', { value: Math.E }) === Math;", "true"),
    ]);
}

#[test]
fn get_own_property_descriptor_and_names() {
    check(&[
        (&format!("{DESC} var r = d(Math, 'floor') + '|' + d(Math, 'PI') + '|' + d(Array.prototype, 'push') + '|' + d(Number, 'EPSILON') + '|' + d(Math, 'nope');"), "function,true,false,true|number,false,false,false|function,true,false,true|number,false,false,false|none"),
        ("var n = Object.getOwnPropertyNames(Math); var r = n.indexOf('floor') >= 0 && n.indexOf('PI') >= 0 && n.indexOf('abs') >= 0;", "true"),
        ("var n = Object.getOwnPropertyNames(globalThis); var r = ['parseInt', 'parseFloat', 'isNaN', 'isFinite', 'Math', 'JSON'].every(function (k) { return n.indexOf(k) >= 0; });", "true"),
        ("var d = Object.getOwnPropertyDescriptors(Math); var r = d.floor.writable + ',' + d.PI.writable + ',' + d.PI.value;", "true,false,3.141592653589793"),
        ("var n = Reflect.ownKeys(JSON); var r = n.indexOf('parse') >= 0 && n.indexOf('stringify') >= 0;", "true"),
    ]);
}

#[test]
fn enumerable_listings_make_nothing_and_miss_nothing() {
    check(&[
        ("var r = Object.keys(Math).length + ',' + Object.values(JSON).length + ',' + Object.entries(Array.prototype).length;", "0,0,0"),
        ("var k = []; for (var x in Math) k.push(x); var r = k.length;", "0"),
        ("var r = JSON.stringify(Math) + JSON.stringify(Object.assign({}, Math)) + JSON.stringify({ ...JSON });", "{}{}{}"),
    ]);
}

#[test]
fn freeze_seal_and_prevent_extensions_see_every_name() {
    check(&[
        (&format!("{DESC} Object.freeze(Math); var r = d(Math, 'floor') + '|' + d(Math, 'PI') + '|' + Object.isFrozen(Math);"), "function,false,false,false|number,false,false,false|true"),
        (&format!("{DESC} Object.seal(JSON); var r = d(JSON, 'parse') + '|' + Object.isSealed(JSON) + '|' + Object.isFrozen(JSON);"), "function,true,false,false|true|false"),
        (&format!("{DESC} Object.preventExtensions(Array.prototype); var r = d(Array.prototype, 'push') + '|' + typeof [].push + '|' + Object.isExtensible(Array.prototype);"), "function,true,false,true|function|false"),
        ("Object.freeze(Math); Math.floor = 1; var r = typeof Math.floor;", "function"),
        ("'use strict'; Object.preventExtensions(Math); var r; try { Math.zzz = 1; r = 'no'; } catch (e) { r = e.name; } r += Math.abs(-2);", "TypeError2"),
        ("Object.freeze(Object.prototype); var r = typeof ({}).hasOwnProperty + ',' + Object.isFrozen(Object.prototype);", "function,true"),
    ]);
}

#[test]
fn assign_spread_and_json_copy_only_the_enumerable_names() {
    check(&[
        ("var t = Object.assign({}, Math, JSON, Array.prototype); var r = Object.getOwnPropertyNames(t).length;", "0"),
        (&format!("{DESC} Object.assign(Math, {{ floor: 1, extra: 2 }}); var r = Math.floor + ',' + d(Math, 'floor') + ',' + d(Math, 'extra');"), "1,number,true,false,true,number,true,true,true"),
        ("var t = { ...Number }; var r = Object.keys(t).length;", "0"),
        ("var r = JSON.stringify([Math, JSON, parseInt]);", "[{},{},null]"),
    ]);
}

#[test]
fn identity_and_function_properties() {
    check(&[
        ("var r = (Number.parseInt === parseInt) + ',' + (Number.parseFloat === parseFloat) + ',' + (Number.parseInt === Number.parseInt);", "true,true,true"),
        ("var r = (Array.prototype.push === Array.prototype.push) + ',' + ([].push === Array.prototype.push) + ',' + (Math.floor === Math.floor);", "true,true,true"),
        ("var f = Math.floor; var r = f.length + ',' + f.name + ',' + Object.hasOwn(f, 'length') + ',' + Object.getOwnPropertyNames(f).join();", "1,floor,true,length,name"),
        ("var r = Array.prototype.push.name + Array.prototype.push.length + String.prototype.padStart.length + JSON.stringify.length + Object.defineProperty.length;", "push1133"),
        ("var r = Function.prototype.toString.call(Math.floor).indexOf('native code') > 0;", "true"),
        ("var r = Object.getPrototypeOf(Math.floor) === Function.prototype && Math.floor.hasOwnProperty('name');", "true"),
    ]);
}

#[test]
fn a_name_of_a_nearer_object_hides_one_of_a_farther_object() {
    check(&[
        ("var a = Object.prototype.toString; var b = [].toString; var r = (a === b) + ',' + (b === Array.prototype.toString);", "false,true"),
        ("var a = Object.prototype.toString; var r = (Array.prototype.toString === a) + ',' + (Object.hasOwn(Array.prototype, 'toString'));", "false,true"),
        ("var r = []; for (var i = 0; i < 4; i++) { r.push([].toString === Object.prototype.toString); } var r = r.join();", "false,false,false,false"),
        ("var a = ({}).toString; var b = Number.prototype.toString; var c = (5).toString; var r = (c === b) + ',' + (a === b) + ',' + (5).toString(2);", "true,false,101"),
        ("var r = ''; for (var i = 0; i < 3; i++) { r += ('x'.toString === String.prototype.toString) + ',' + ('x'.valueOf === String.prototype.valueOf) + ';'; }", "true,true;true,true;true,true;"),
        ("var r = (function () { return typeof this.hasOwnProperty; }).call(Object.create(Array.prototype));", "function"),
    ]);
}

#[test]
fn a_prototype_setter_and_a_read_only_name_are_found_through_the_chain() {
    check(&[
        ("var o = Object.create(Math); o.PI = 3; var r = o.PI + ',' + Object.hasOwn(o, 'PI');", "3.141592653589793,false"),
        ("'use strict'; var o = Object.create(Math); var r; try { o.PI = 3; r = 'no'; } catch (e) { r = e.name; }", "TypeError"),
        ("var o = Object.create(Math); o.floor = 5; var r = o.floor + ',' + Object.hasOwn(o, 'floor') + ',' + typeof Math.floor;", "5,true,function"),
        ("var o = Object.create(Array.prototype); o.push = 5; var r = o.push + ',' + typeof Array.prototype.push;", "5,function"),
        ("var r = Reflect.set(Object.create(Math), 'E', 1) + ',' + Reflect.set(Object.create(Math), 'abs', 1);", "false,true"),
    ]);
}

#[test]
fn super_and_instanceof_and_classes_reach_a_table_name() {
    check(&[
        ("class A extends Array { go() { return super.push(1, 2); } } var a = new A(); var r = a.go() + ',' + a.length + ',' + (a instanceof Array);", "2,2,true"),
        ("class B extends Number { f() { return super.toFixed(1); } } var r = new B(3).f();", "3.0"),
        ("var o = { __proto__: Object.prototype, m() { return super.hasOwnProperty('m'); } }; var r = o.m();", "true"),
        ("class C { static f() { return super.hasOwnProperty('f'); } } var r = C.f();", "true"),
        ("var r = ({}) instanceof Object && Object.prototype.isPrototypeOf({}) && Function.prototype.call.call(Math.abs, null, -3) === 3;", "true"),
    ]);
}

#[test]
fn reflect_and_the_global_functions() {
    check(&[
        ("var r = Reflect.has(Math, 'floor') + ',' + Reflect.deleteProperty(Math, 'floor') + ',' + Reflect.has(Math, 'floor') + ',' + (Reflect.getOwnPropertyDescriptor(Math, 'abs').writable);", "true,true,false,true"),
        ("var r = Reflect.defineProperty(Math, 'abs', { value: 1 }) + ',' + Math.abs;", "true,1"),
        ("var r = parseInt('12px') + ',' + parseFloat('1.5e1') + ',' + isNaN('x') + ',' + isFinite('1');", "12,15,true,true"),
        ("var r = typeof globalThis.parseInt + ',' + ('isNaN' in globalThis) + ',' + (globalThis.hasOwnProperty('isFinite'));", "function,true,true"),
        ("var r = typeof parseInt; parseInt = 3; r += ',' + globalThis.parseInt + ',' + typeof Number.parseInt;", "function,3,function"),
        ("var r = typeof isNaN + typeof undefinedName;", "functionundefined"),
        ("function parseFloat() { return 'mine'; } var r = parseFloat();", "mine"),
        ("var parseInt = 5; var r = parseInt;", "5"),
    ]);
}

#[test]
fn an_alias_is_the_function_that_it_names() {
    check(&[
        ("var r = (Date.prototype.toGMTString === Date.prototype.toUTCString) + ',' + Date.prototype.toGMTString.name;", "true,toUTCString"),
        ("var f = Date.prototype.toUTCString; delete Date.prototype.toUTCString; var r = (Date.prototype.toGMTString === f) + ',' + typeof Date.prototype.toUTCString;", "true,undefined"),
        ("var r = typeof Date.prototype.toGMTString + (Date.prototype.toGMTString === Date.prototype.toUTCString);", "functiontrue"),
        ("var f = parseInt; delete globalThis.parseInt; var r = (Number.parseInt === f) + ',' + typeof parseInt;", "true,undefined"),
        ("parseFloat = 1; var r = (Number.parseFloat !== 1) + ',' + Number.parseFloat('2.5');", "true,2.5"),
        ("var r = new Date(0).toGMTString() + '|' + new Date(0).getTime() + '|' + Date.UTC(1970, 0, 1);", "Thu, 01 Jan 1970 00:00:00 GMT|0|0"),
    ]);
}

#[test]
fn collections_and_promise_methods_are_made_at_their_first_use() {
    check(&[
        ("var m = new Map([[1, 2]]); var r = m.get(1) + ',' + m.has(1) + ',' + m.size + ',' + m.keys().next().value + ',' + m.delete(1) + ',' + Object.hasOwn(Map.prototype, 'forEach');", "2,true,1,1,true,true"),
        ("var s = new Set([1, 2]); var r = s.has(2) + ',' + s.entries().next().value.length + ',' + (Set.prototype.keys === Set.prototype.values) + ',' + (Set.prototype[Symbol.iterator] === Set.prototype.values);", "true,2,true,true"),
        ("var w = new WeakMap(); var k = {}; w.set(k, 3); var r = w.get(k) + ',' + w.has(k) + ',' + (new WeakSet().has(k));", "3,true,false"),
        ("var r = typeof Promise.resolve + typeof Promise.all + typeof Promise.prototype.catch + typeof Promise.prototype.finally + Promise.withResolvers.length + Object.hasOwn(Promise, 'race');", "functionfunctionfunctionfunction0true"),
        ("var r = typeof queueMicrotask + ',' + ('queueMicrotask' in globalThis) + ',' + Object.hasOwn(globalThis, 'queueMicrotask');", "function,true,true"),
    ]);
}

#[test]
fn the_host_reads_a_name_that_is_not_made() {
    let mut vm = run("var x = 1;").expect("runs");
    let g = vm.realm.global;
    let m = vm.realm.get_named(g.value(), "Math").expect("Math");
    let f = vm.realm.get_named(m, "floor").expect("floor");
    assert!(vm.realm.native_id(f).is_some());
    let a = vm.realm.get_named(g.value(), "Array").expect("Array");
    assert!(vm.realm.get_named(a, "isArray").is_ok_and(|v| vm.realm.native_id(v).is_some()));
    let p = vm.realm.get_named(a, "prototype").expect("prototype");
    assert!(vm.realm.get_named(p, "push").is_ok_and(|v| vm.realm.native_id(v).is_some()));
    assert!(vm.realm.get_named(p, "nope").expect("a read").is_undefined());
}

/// The own keys, with their attributes and the `length` and `name` of a function, of every built-in object that has a table.
const DUMP: &str = "
var paths = ['globalThis', 'Object', 'Object.prototype', 'Function', 'Function.prototype', 'Array', 'Array.prototype', 'String', 'String.prototype',
  'Number', 'Number.prototype', 'Boolean', 'Boolean.prototype', 'Math', 'JSON', 'Error', 'Error.prototype', 'Symbol', 'Symbol.prototype', 'Promise',
  'Promise.prototype', 'Map', 'Map.prototype', 'Set', 'Set.prototype', 'WeakMap', 'WeakSet', 'Date', 'Date.prototype', 'Reflect', 'RegExp', 'RegExp.prototype'];
var r = paths.map(function (p) {
  var o; try { o = p.split('.').reduce(function (a, k) { return a[k]; }, globalThis); } catch (e) { return p + ': -'; }
  if (o === undefined) return p + ': -';
  var names = Object.getOwnPropertyNames(o).sort();
  var syms = Object.getOwnPropertySymbols(o).map(String).sort();
  return p + ': ' + names.concat(syms).map(function (n) {
    var d = Object.getOwnPropertyDescriptor(o, n);
    if (!d) return n + '(none)';
    if (n === 'prototype' || n === 'constructor') return n;
    if (d.get || d.set) return n + '(accessor,' + d.enumerable + ',' + d.configurable + ')';
    var v = d.value;
    var s = typeof v === 'function' ? 'fn' + v.length + v.name : typeof v === 'number' ? 'n' + String(v) : typeof v;
    return n + '(' + s + ',' + d.writable + ',' + d.enumerable + ',' + d.configurable + ')';
  }).join(' ');
}).join('\\n');";

fn dump(eager: bool) -> String {
    let mut vm = load(DUMP).expect("loads");
    if eager {
        vm.realm.lazy_everything();
    }
    vm.run_main(0).expect("runs");
    vm.global_text("r")
}

#[test]
fn a_lazy_realm_and_an_eager_realm_have_the_same_properties() {
    let (eager, lazy) = (dump(true), dump(false));
    assert!(eager.len() > 2000, "the dump is real: {eager}");
    for (a, b) in eager.lines().zip(lazy.lines()) {
        assert_eq!(a, b);
    }
    assert_eq!(eager.lines().count(), lazy.lines().count());
}

#[test]
fn listing_before_and_after_the_first_use_is_one_set() {
    // The first listing makes everything; a use before it makes one name first. Both give the same set.
    let a = ev("var r = Object.getOwnPropertyNames(Math).sort().join();", "r");
    let b = ev("Math.floor(1.5); Math.PI; var r = Object.getOwnPropertyNames(Math).sort().join();", "r");
    let c = ev("Object.getOwnPropertyNames(Math); var r = Object.getOwnPropertyNames(Math).sort().join();", "r");
    assert!(a.contains("floor") && a.contains("PI"));
    assert_eq!(a, b);
    assert_eq!(a, c);
}

#[test]
fn every_table_hash_is_the_hash_of_its_name() {
    let vm = run("var x = 1;").expect("runs");
    let mut seen = 0;
    for t in &vm.realm.ltab.tabs {
        for (i, e) in t.ents.iter().enumerate() {
            assert_eq!(e.hash, crate::string::Chars::One(e.name.as_bytes()).hash(), "{}", e.name);
            assert!(!t.ents[..i].iter().any(|o| o.name == e.name), "a name twice in one table: {}", e.name);
            seen += 1;
        }
    }
    assert!(seen > 0);
}
