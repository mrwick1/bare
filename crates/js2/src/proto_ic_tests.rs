// Tests of the prototype entries of the feedback records (`interp_rt.rs`, `docs/design-js2-proto-ic.md`): a read of a data property that a
// prototype holds. Each case warms a read site, changes the chain in one way, and reads again. `check` runs each case normally and
// under constant collection, so a stale holder would show as a wrong value or a crash.
use super::*;
use crate::heap::Trigger;

/// Run `src`, then read the global `name` as text.
fn ev(src: &str, name: &str) -> String {
    match run(src) {
        Ok(mut vm) => vm.global_text(name),
        Err(e) => panic!("{e}\n--- source:\n{src}"),
    }
}

/// The same under constant collection (a tiny trigger: nearly every op that allocates collects first).
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

/// Prototype entries that the feedback of a finished script holds.
fn entries(src: &str) -> usize {
    run(src).unwrap().fbs.iter().map(|f| f.protos.len()).sum()
}

#[test]
fn a_method_on_a_prototype_gets_an_entry_at_any_depth() {
    let one = "class A { m() { return 1; } } var o = new A(); var r = 0; for (var i = 0; i < 50; i++) r += o.m();";
    let two = "class A { m() { return 1; } } class B extends A {} var o = new B(); var r = 0; for (var i = 0; i < 50; i++) r += o.m();";
    let three = "class A { m() { return 1; } } class B extends A {} class C extends B {} var o = new C(); var r = 0; for (var i = 0; i < 50; i++) r += o.m();";
    for src in [one, two, three] {
        assert!(entries(src) > 0, "{src}");
    }
    check(&[(one, "50"), (two, "50"), (three, "50")]);
}

#[test]
fn a_value_that_a_prototype_holds_is_read_each_time() {
    check(&[
        ("var P = {v: 1}; var o = Object.create(P); function rd(x) { return x.v; } var r = ''; for (var i = 0; i < 6; i++) { r += rd(o); P.v = i; }", "101234"),
        // The value of a data property in an overflow record (a prototype with many properties).
        ("var P = {}; for (var i = 0; i < 40; i++) P['k' + i] = i; var o = Object.create(P); function rd(x) { return x.k39; } var r = 0; for (var i = 0; i < 5; i++) { r += rd(o); P.k39 = 1; }", "43"),
    ]);
}

#[test]
fn an_own_property_of_the_receiver_hides_the_cached_one() {
    check(&[
        ("class A { m() { return 'A'; } } var o = new A(); var p = new A(); function f(x) { return x.m(); } var r = ''; for (var i = 0; i < 4; i++) r += f(o); o.m = function () { return 'own'; }; r += f(o) + f(p);", "AAAAownA"),
    ]);
}

#[test]
fn a_shadow_on_an_intermediate_object_wins() {
    check(&[
        ("class A { m() { return 'A'; } } class B extends A {} class C extends B {} var o = new C(); function f(x) { return x.m(); } var r = ''; for (var i = 0; i < 4; i++) r += f(o); B.prototype.m = function () { return 'B'; }; r += f(o); delete B.prototype.m; r += f(o);", "AAAABA"),
        // The same with a dictionary-mode intermediate (a middle key was deleted first).
        ("class A { m() { return 'A'; } } class B extends A { x() {} y() {} } class C extends B {} var o = new C(); delete B.prototype.x; function f(x) { return x.m(); } var r = ''; for (var i = 0; i < 4; i++) r += f(o); B.prototype.m = function () { return 'B'; }; r += f(o); delete B.prototype.m; r += f(o);", "AAAABA"),
    ]);
}

#[test]
fn a_change_of_the_holder_is_seen() {
    check(&[
        // The holder gets another value, loses the key, gets it back as an accessor, and as a data property again.
        ("class A { m() { return 1; } } var o = new A(); function f(x) { return typeof x.m; } var r = ''; for (var i = 0; i < 4; i++) r += f(o)[0]; A.prototype.m = 5; r += f(o)[0]; delete A.prototype.m; r += f(o)[0]; Object.defineProperty(A.prototype, 'm', { get() { return 'g'; }, configurable: true }); r += f(o)[0]; Object.defineProperty(A.prototype, 'm', { value: 2, configurable: true }); r += f(o)[0];", "ffffnusn"),
        ("class A { m() { return 1; } } var o = new A(); function f(x) { return x.m; } var r = 0; for (var i = 0; i < 4; i++) f(o); Object.defineProperty(A.prototype, 'm', { value: 7, writable: false }); r = f(o);", "7"),
        // A holder with many properties grows its overflow record after the entry was made.
        ("var P = {a: 1}; var o = Object.create(P); function f(x) { return x.a; } var r = ''; for (var i = 0; i < 4; i++) r += f(o); for (var i = 0; i < 60; i++) P['p' + i] = i; r += f(o); P.a = 2; r += f(o);", "111112"),
    ]);
}

#[test]
fn a_new_prototype_anywhere_in_the_chain_is_seen() {
    check(&[
        ("class A { m() { return 'A'; } } class Z { m() { return 'Z'; } } class B extends A {} var o = new B(); function f(x) { return x.m(); } var r = ''; for (var i = 0; i < 4; i++) r += f(o); Object.setPrototypeOf(B.prototype, Z.prototype); r += f(o); B.prototype.__proto__ = A.prototype; r += f(o);", "AAAAZA"),
        ("class A { m() { return 'A'; } } class Z { m() { return 'Z'; } } var o = new A(); var p = new A(); function f(x) { return x.m(); } var r = ''; for (var i = 0; i < 4; i++) r += f(o); Object.setPrototypeOf(o, Z.prototype); r += f(o) + f(p); o.__proto__ = null; r += typeof o.m;", "AAAAZAundefined"),
        // The holder's own prototype is changed, the key stays.
        ("var A = {m: 1}; var B = Object.create(A); var o = Object.create(B); function f(x) { return x.m; } var r = ''; for (var i = 0; i < 4; i++) r += f(o); Object.setPrototypeOf(B, {m: 2}); r += f(o); Object.setPrototypeOf(B, null); r += f(o);", "11112undefined"),
    ]);
}

#[test]
fn an_add_by_a_cached_entry_cannot_skip_the_epoch() {
    // `add` runs on plain objects first, so its site has an add entry for the shape of `{}`. `P` is a plain `{}` too, and a cached read
    // through it made it a prototype. The add on `P` must shadow `Object.prototype.k` for the child.
    check(&[
        ("Object.prototype.k = 0; var P = {}; var child = Object.create(P); function rd(x) { return x.k; } function add(o) { o.k = 7; } var r = ''; for (var i = 0; i < 4; i++) r += rd(child); for (var i = 0; i < 4; i++) add({}); add(P); r += rd(child);", "00007"),
    ]);
}

#[test]
fn a_site_with_several_receivers_keeps_each_entry() {
    check(&[
        ("class A { m() { return 1; } } class B { m() { return 2; } } class C { m() { return 3; } } class D { m() { return 4; } } function f(x) { return x.m(); } var xs = [new A(), new B(), new C(), new D()]; var r = 0; for (var i = 0; i < 40; i++) r += f(xs[i % 4]);", "100"),
        // A fifth receiver shape: the site becomes megamorphic and stays right.
        ("class A { m() { return 1; } } class B { m() { return 2; } } class C { m() { return 3; } } class D { m() { return 4; } } class E { m() { return 5; } } function f(x) { return x.m(); } var xs = [new A(), new B(), new C(), new D(), new E()]; var r = 0; for (var i = 0; i < 50; i++) r += f(xs[i % 5]);", "150"),
        // An own property and a prototype property at one site.
        ("class A { m() { return 1; } } var a = new A(); var b = {m() { return 2; }}; function f(x) { return x.m(); } var r = 0; for (var i = 0; i < 20; i++) r += f(i % 2 ? a : b);", "30"),
    ]);
}

#[test]
fn a_site_that_is_made_stale_over_and_over_goes_megamorphic_and_stays_right() {
    check(&[
        ("var P = {v: 0}; var o = Object.create(P); function rd(x) { return x.v; } var r = 0; for (var i = 0; i < 100; i++) { P['x' + i] = i; P.v = i; r += rd(o); }", "4950"),
        ("var P = {v: 1}; var o = Object.create(P); function rd(x) { return x.v; } var r = 0; for (var i = 0; i < 100; i++) { delete P.v; P.v = 2; r += rd(o); }", "200"),
    ]);
}

#[test]
fn prototypes_that_die_are_not_read_from_the_cache() {
    // One site reads a method of a class made in each turn: the old holders are garbage by the time the next turn warms the site.
    check(&[
        ("function mk(i) { return class { m() { return i; } }; } function f(x) { return x.m(); } var r = 0; for (var i = 0; i < 300; i++) { var C = mk(i); var o = new C(); r += f(o) + f(o) + f(o); }", "134550"),
        ("function mk(i) { var P = {v: i}; return Object.create(Object.create(P)); } function f(x) { return x.v; } var r = 0; for (var i = 0; i < 300; i++) { var o = mk(i); r += f(o) + f(o) + f(o); }", "134550"),
    ]);
}

#[test]
fn a_collection_drops_the_entries() {
    let mut vm = load("class A { m() { return 1; } } var o = new A(); var r = 0; for (var i = 0; i < 20; i++) r += o.m();").unwrap();
    vm.run_main(0).unwrap();
    assert!(vm.fbs.iter().map(|f| f.protos.len()).sum::<usize>() > 0);
    let before = vm.realm.shapes.proto_epoch();
    let Vm { realm, fbs, .. } = &mut vm;
    realm.collect(fbs, |_| {});
    assert!(vm.realm.shapes.proto_epoch() > before);
}

#[test]
fn strings_numbers_and_booleans_read_their_prototypes() {
    check(&[
        ("function f(s) { return s.slice(1, 3); } var r = ''; for (var i = 0; i < 5; i++) r += f('abcd'); String.prototype.slice = function () { return 'S'; }; r += f('abcd');", "bcbcbcbcbcS"),
        ("function f(n) { return n.toString(); } var r = ''; for (var i = 0; i < 4; i++) r += f(5) + ','; Number.prototype.toString = function () { return 'N'; }; r += f(2);", "5,5,5,5,N"),
        ("Object.prototype.zz = 'z'; function f(x) { return x.zz; } var r = ''; for (var i = 0; i < 4; i++) r += f('a') + f(1) + f(true); delete Object.prototype.zz; r += typeof f('a');", "zzzzzzzzzzzzundefined"),
        // `length` of a string is not a prototype property.
        ("function f(s) { return s.length; } var r = ''; for (var i = 0; i < 4; i++) r += f('abc') + ',' + f('de') + ','; ", "3,2,3,2,3,2,3,2,"),
        // A string that has the method both as own wrapper property and ... a wrapper object does not use the entry of the primitive.
        ("function f(s) { return s.slice(0, 1); } var r = ''; for (var i = 0; i < 4; i++) r += f('xy') + f(new String('zw'));", "xzxzxzxz"),
    ]);
}

#[test]
fn a_getter_on_a_prototype_is_called_each_time() {
    check(&[
        ("var n = 0; class A { get c() { return ++n; } } var o = new A(); var r = ''; for (var i = 0; i < 5; i++) r += o.c;", "12345"),
    ]);
}

#[test]
fn functions_arrays_and_wrappers_keep_their_own_properties() {
    check(&[
        ("function g(a, b) {} function f(x) { return x.length + ':' + x.name; } var r = ''; for (var i = 0; i < 4; i++) r += f(g) + ','; r += f(Math.max);", "2:g,2:g,2:g,2:g,2:max"),
        ("function f(x) { return typeof x.call + typeof x.apply + typeof x.prototype; } function g() {} var r = ''; for (var i = 0; i < 4; i++) r += f(g) + ','; r += f(function () {});", "functionfunctionobject,functionfunctionobject,functionfunctionobject,functionfunctionobject,functionfunctionobject"),
        ("function f(x) { return x.length; } var r = ''; for (var i = 0; i < 4; i++) r += f([1, 2, 3]) + ',' + f([]) + ',' + f(new String('ab')) + ';'; ", "3,0,2;3,0,2;3,0,2;3,0,2;"),
        ("function f(x) { return x.push; } var a = []; var r = ''; for (var i = 0; i < 4; i++) r += typeof f(a); a.push = 1; r += typeof f(a); delete a.push; r += typeof f(a);", "functionfunctionfunctionfunctionnumberfunction"),
        ("function f(x) { return x.constructor === Array; } var r = ''; for (var i = 0; i < 4; i++) r += f([]); Array.prototype.constructor = Object; r += f([]);", "truetruetruetruefalse"),
    ]);
}

#[test]
fn a_class_with_static_inheritance_and_super_reads_the_right_holder() {
    check(&[
        ("class A { static s() { return 'sA'; } } class B extends A {} function f() { return B.s(); } var r = ''; for (var i = 0; i < 4; i++) r += f(); B.s = function () { return 'sB'; }; r += f(); delete B.s; r += f();", "sAsAsAsAsBsA"),
        ("class A { m() { return 1; } } class B extends A { m() { return super.m() + 1; } } var o = new B(); var r = 0; for (var i = 0; i < 5; i++) r += o.m();", "10"),
    ]);
}
