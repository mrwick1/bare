// Tests of `Reflect` (`bi_reflect.rs`): each case is a program, the global to read and the expected text. Each runs normally and under
// constant collection.
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

fn check(cases: &[(&str, &str, &str)]) {
    for (src, name, want) in cases {
        assert_eq!(ev(src, name), *want, "{src}");
        assert_eq!(ev_gc(src, name), *want, "under collection: {src}");
    }
}

#[test]
fn the_shape_of_reflect_is_the_standard_one() {
    check(&[
        ("var r = Object.prototype.toString.call(Reflect) + ',' + typeof Reflect + ',' + (Object.getPrototypeOf(Reflect) === Object.prototype);", "r", "[object Reflect],object,true"),
        ("var r = ['apply','construct','defineProperty','deleteProperty','get','getOwnPropertyDescriptor','getPrototypeOf','has','isExtensible','ownKeys','preventExtensions','set','setPrototypeOf'].map(function (n) { return n + Reflect[n].length; }).join();", "r", "apply3,construct2,defineProperty3,deleteProperty2,get2,getOwnPropertyDescriptor2,getPrototypeOf1,has2,isExtensible1,ownKeys1,preventExtensions1,set3,setPrototypeOf2"),
        ("var d = Object.getOwnPropertyDescriptor(Reflect, 'get'); var t = Object.getOwnPropertyDescriptor(Reflect, Symbol.toStringTag); var r = d.writable + ',' + d.enumerable + ',' + d.configurable + ',' + t.writable + ',' + t.configurable;", "r", "true,false,true,false,true"),
        ("var r = ''; try { new Reflect(); } catch (e) { r += e.name; } try { Reflect(); } catch (e) { r += e.name; }", "r", "TypeErrorTypeError"),
    ]);
}

#[test]
fn every_method_needs_an_object_target() {
    check(&[(
        "var r = []; ['defineProperty','deleteProperty','get','getOwnPropertyDescriptor','getPrototypeOf','has','isExtensible','ownKeys','preventExtensions','set','setPrototypeOf'].forEach(function (n) { try { Reflect[n](1, 'a', {}); r.push('no'); } catch (e) { r.push(e.name); } }); var r = r.join();",
        "r",
        "TypeError,TypeError,TypeError,TypeError,TypeError,TypeError,TypeError,TypeError,TypeError,TypeError,TypeError",
    )]);
}

#[test]
fn apply_and_construct() {
    check(&[
        ("function f(a, b) { return this.k + a + b; } var r = Reflect.apply(f, { k: 1 }, [2, 3]) + ',' + Reflect.apply(Math.max, null, { length: 2, 0: 4, 1: 9 }) + ',' + Reflect.apply(function () { return arguments.length; }, null, []);", "r", "6,9,0"),
        ("var r = ''; try { Reflect.apply(1, null, []); } catch (e) { r += e.name; } try { Reflect.apply(function () {}, null); } catch (e) { r += e.name; } try { Reflect.apply(function () {}, null, 1); } catch (e) { r += e.name; }", "r", "TypeErrorTypeErrorTypeError"),
        ("function F(a) { this.a = a; } var o = Reflect.construct(F, [7]); var r = o.a + ',' + (o instanceof F);", "r", "7,true"),
        // The new target gives the prototype.
        ("function F() { this.x = 1; } function G() {} G.prototype = { tag: 'g' }; var o = Reflect.construct(F, [], G); var r = o.tag + ',' + o.x + ',' + (o instanceof G) + ',' + (o instanceof F);", "r", "g,1,true,false"),
        ("class A { constructor() { this.n = new.target === B; } } class B {} var r = Reflect.construct(A, [], B).n;", "r", "true"),
        ("var m = Reflect.construct(Map, [[[1, 2]]]); class M2 extends Map {} var n = Reflect.construct(Map, [], M2); var r = m.get(1) + ',' + (n instanceof M2) + ',' + (Reflect.construct(Array, [3]).length);", "r", "2,true,3"),
        ("var r = ''; try { Reflect.construct(function () {}, [], Math.max); } catch (e) { r += e.name; } try { Reflect.construct(Object); } catch (e) { r += e.name; }", "r", "TypeErrorTypeError"),
    ]);
}

#[test]
fn get_and_set_use_the_receiver() {
    check(&[
        ("var o = { get a() { return this.v; }, v: 1, b: 2 }; var r = Reflect.get(o, 'a') + ',' + Reflect.get(o, 'a', { v: 9 }) + ',' + Reflect.get(o, 'b') + ',' + Reflect.get(o, 'zz') + ',' + Reflect.get([5, 6], 1) + ',' + Reflect.get(new String('ab'), 1) + Reflect.get(new String('ab'), 'length');", "r", "1,9,2,undefined,6,b2"),
        ("var p = { inherited: 3 }; var o = Object.create(p); var r = Reflect.get(o, 'inherited') + ',' + (Reflect.get(o, 'toString') === Object.prototype.toString);", "r", "3,true"),
        ("var o = { v: 1, set s(x) { this.v = x; } }; var t = {}; var a = Reflect.set(o, 's', 5); var b = Reflect.set(o, 's', 8, t); var r = a + ',' + b + ',' + o.v + ',' + t.v;", "r", "true,true,5,8"),
        ("var o = {}; var r = Reflect.set(o, 'a', 1) + ',' + o.a + ',' + JSON.stringify(Object.getOwnPropertyDescriptor(o, 'a'));", "r", "true,1,{\"value\":1,\"writable\":true,\"enumerable\":true,\"configurable\":true}"),
        // False, not an exception.
        ("var o = Object.freeze({ a: 1 }); var g = { get a() { return 1; } }; var ne = Object.preventExtensions({}); var r = [Reflect.set(o, 'a', 2), Reflect.set(o, 'b', 2), Reflect.set(g, 'a', 2), Reflect.set(ne, 'x', 1), Reflect.set({}, 'a', 1, 1), Reflect.set({}, 'a', 1, Object.freeze({ a: 0 })), Reflect.set({}, 'a', 1, { get a() { return 0; } })].join();", "r", "false,false,false,false,false,false,false"),
        // A value that is not writable on the prototype blocks the write on the receiver.
        ("var p = Object.defineProperty({}, 'a', { value: 1, writable: false }); var o = Object.create(p); var r = Reflect.set(o, 'a', 2) + ',' + o.hasOwnProperty('a');", "r", "false,false"),
        ("var a = [1, 2, 3]; var r = Reflect.set(a, 'length', 1) + ',' + a.join() + ',' + Reflect.set(a, 5, 'x') + ',' + a.length;", "r", "true,1,true,6"),
        ("var o = { a: 1 }; var recv = Object.defineProperty({}, 'a', { value: 0, writable: true, configurable: true, enumerable: false }); Reflect.set(o, 'a', 5, recv); var r = o.a + ',' + recv.a + ',' + Object.keys(recv).length;", "r", "1,5,0"),
    ]);
}

#[test]
fn descriptors_and_definition() {
    check(&[
        ("var o = {}; var a = Reflect.defineProperty(o, 'x', { value: 1 }); var b = Reflect.defineProperty(o, 'x', { value: 2 }); var c = Reflect.defineProperty(o, 'x', { value: 1 }); var r = a + ',' + b + ',' + c + ',' + o.x;", "r", "true,false,true,1"),
        ("var o = {}; Reflect.defineProperty(o, 'g', { get: function () { return 4; }, configurable: true }); var d = Reflect.getOwnPropertyDescriptor(o, 'g'); var r = o.g + ',' + typeof d.get + ',' + d.set + ',' + d.enumerable + ',' + d.configurable;", "r", "4,function,undefined,false,true"),
        // The descriptor object is read with [[Get]], so its own accessors run, and the prototype is looked up.
        ("var log = []; var d = { get value() { log.push('v'); return 3; }, get enumerable() { log.push('e'); return true; } }; var o = {}; Reflect.defineProperty(o, 'k', d); var r = log.join() + ',' + o.k + ',' + Object.keys(o);", "r", "e,v,3,k"),
        ("var o = {}; Reflect.defineProperty(o, 'k', Object.create({ value: 8, enumerable: true })); var r = o.k + ',' + Object.keys(o);", "r", "8,k"),
        ("var r = ''; try { Reflect.defineProperty({}, 'a', 1); } catch (e) { r += e.name; } try { Reflect.defineProperty({}, 'a', { get: 1 }); } catch (e) { r += e.name; } try { Reflect.defineProperty({}, 'a', { get: function () {}, value: 1 }); } catch (e) { r += e.name; }", "r", "TypeErrorTypeErrorTypeError"),
        ("var o = Object.preventExtensions({}); var r = Reflect.defineProperty(o, 'a', { value: 1 }) + ',' + Object.keys(o).length;", "r", "false,0"),
        ("var a = [1, 2]; var r = Reflect.defineProperty(a, 'length', { value: 1 }) + ',' + a.length + ',' + Reflect.getOwnPropertyDescriptor(a, 'length').writable + ',' + Reflect.getOwnPropertyDescriptor(a, 0).value + ',' + Reflect.getOwnPropertyDescriptor(a, 9);", "r", "true,1,true,1,undefined"),
        ("function f(a, b) {} var d = Reflect.getOwnPropertyDescriptor(f, 'length'); var r = d.value + ',' + d.writable + ',' + d.configurable + ',' + Reflect.getOwnPropertyDescriptor(f, 'name').value;", "r", "2,false,true,f"),
        ("var s = Symbol('q'); var o = { [s]: 1 }; var r = Reflect.getOwnPropertyDescriptor(o, s).value;", "r", "1"),
        ("var o = { a: 1 }; var r = Reflect.deleteProperty(o, 'a') + ',' + ('a' in o) + ',' + Reflect.deleteProperty(o, 'nope') + ',' + Reflect.deleteProperty(Object.freeze({ b: 1 }), 'b') + ',' + Reflect.deleteProperty([1], 'length') + ',' + Reflect.deleteProperty(Object.defineProperty({}, 'c', { value: 1 }), 'c');", "r", "true,false,true,false,false,false"),
    ]);
}

#[test]
fn prototypes_keys_and_extensibility() {
    check(&[
        ("var p = {}; var o = Object.create(p); var r = (Reflect.getPrototypeOf(o) === p) + ',' + Reflect.getPrototypeOf(Object.create(null)) + ',' + (Reflect.getPrototypeOf([]) === Array.prototype);", "r", "true,null,true"),
        ("var o = {}; var p = { z: 1 }; var r = Reflect.setPrototypeOf(o, p) + ',' + o.z + ',' + Reflect.setPrototypeOf(o, null) + ',' + o.z;", "r", "true,1,true,undefined"),
        ("var a = {}; var b = Object.create(a); var r = Reflect.setPrototypeOf(a, b) + ',' + Reflect.setPrototypeOf(Object.preventExtensions({}), {}) + ',' + Reflect.setPrototypeOf(Object.preventExtensions({}), Object.prototype) + ',' + Reflect.setPrototypeOf(Object.prototype, Object.create(null));", "r", "false,false,true,false"),
        ("var r = ''; try { Reflect.setPrototypeOf({}, 1); } catch (e) { r += e.name; } try { Reflect.setPrototypeOf({}); } catch (e) { r += e.name; }", "r", "TypeErrorTypeError"),
        ("var s = Symbol('s'); var o = { b: 1, 2: 'x', a: 2, 1: 'y', [s]: 3 }; Object.defineProperty(o, 'hid', { value: 1, enumerable: false }); var k = Reflect.ownKeys(o); var r = k.length + ',' + k.slice(0, 5).map(String).join('|') + ',' + (k[5] === s);", "r", "6,1|2|b|a|hid,true"),
        ("var r = Reflect.ownKeys([7, 8]).join() + ',' + Reflect.ownKeys(function (a) {}).slice(0, 2).join() + ',' + Array.isArray(Reflect.ownKeys({}));", "r", "0,1,length,length,name,true"),
        ("var o = { a: 1 }; var r = Reflect.isExtensible(o) + ',' + Reflect.preventExtensions(o) + ',' + Reflect.isExtensible(o) + ',' + Reflect.preventExtensions(o) + ',' + Reflect.has(o, 'a') + ',' + Reflect.has(o, 'toString') + ',' + Reflect.has(o, 'q') + ',' + Reflect.has([1], 0) + ',' + Reflect.has([1], 1);", "r", "true,true,false,true,true,true,false,true,false"),
        ("var s = Symbol(); var o = {}; o[s] = 1; var r = Reflect.has(o, s) + ',' + Reflect.has(o, 'x') + ',' + Reflect.has({ 5: 1 }, 5) + ',' + Reflect.has({ 5: 1 }, '5');", "r", "true,false,true,true"),
    ]);
}

#[test]
fn it_works_through_a_loop_under_collection() {
    check(&[(
        "var t = 0; for (var i = 0; i < 300; i++) { var o = Reflect.construct(Object, []); Reflect.set(o, 'k' + i, i); Reflect.defineProperty(o, 'h', { value: [i] }); t += Reflect.get(o, 'k' + i) + Reflect.apply(function (a) { return a[0]; }, null, [Reflect.get(o, 'h')]) + Reflect.ownKeys(o).length; } var r = t;",
        "r",
        "90300",
    )]);
}
