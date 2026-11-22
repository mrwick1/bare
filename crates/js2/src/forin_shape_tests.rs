use super::{load, tests::check};
use crate::heap::Trigger;
use crate::interp_rt::Realm;
use crate::value::Value;
use bare_js_api::{Args, Attrs, Engine, Error, Host, Lazy, Local, Named, Pending, Scope, enter};
use std::cell::Cell;
use std::rc::Rc;

#[test]
fn opens_only_ordinary_shapes_with_no_inherited_keys() {
    let mut realm = load("").unwrap().realm;
    realm.run_text(r#"
        Object.getOwnPropertyNames(Object.prototype);
        var plain = {b: 0, a: 0};
        var indexed = {5: 0, 2: 0};
        var array = [0];
        var dictionary = Object.freeze({a: 0});
        var inherited = Object.create({p: 0});
        var proxy = new Proxy({a: 0}, {});
    "#).unwrap();
    for (name, shape) in [("plain", true), ("indexed", false), ("array", false), ("dictionary", false), ("inherited", false), ("proxy", false)] {
        let obj = realm.get_named(realm.global.value(), name).unwrap();
        let (_, keys_or_shape) = realm.for_in_open(obj).unwrap();
        assert_eq!(Realm::obj(keys_or_shape).is_none(), shape, "{name}");
    }
}

#[test]
fn key_order_and_non_enumerable_shadowing() {
    check(&[
        (r#"
            var o = {__proto__: null, z: 0, '01': 0, [Symbol()]: 0, '': 0, a: 0};
            Object.defineProperty(o, 'skip', {value: 1});
            var r = [];
            for (var k in o) r.push(k);
            r = r.join(',');
        "#, "r", "z,01,,a"),
        (r#"
            var p = {hidden: 0, z: 0};
            var o = Object.create(p);
            Object.defineProperty(o, 'hidden', {value: 1});
            o.a = 0; o[5] = 0; o[2] = 0;
            var r = '';
            for (var k in o) r += k;
        "#, "r", "25az"),
        (r#"
            var p = Object.create({hidden: 0, z: 0});
            Object.defineProperty(p, 'hidden', {value: 1});
            var o = Object.create(p);
            o.a = 0;
            var r = '';
            for (var k in o) r += k;
        "#, "r", "az"),
    ]);
}

#[test]
fn shape_changes_keep_the_opening_keys() {
    check(&[
        (r#"
            var o = {__proto__: null, a: 0, b: 0, c: 0};
            var r = '';
            for (var k in o) {
                r += k;
                delete o.b;
            }
        "#, "r", "ac"),
        (r#"
            var o = {__proto__: null, a: 0, b: 0};
            var r = '';
            for (var k in o) {
                r += k;
                o.c = 0;
            }
        "#, "r", "ab"),
        (r#"
            var o = {__proto__: null, a: 0, b: 0};
            var r = '';
            for (var k in o) {
                r += k;
                Object.defineProperty(o, 'b', {enumerable: false});
            }
        "#, "r", "a"),
        (r#"
            var o = {__proto__: null, a: 0, b: 0};
            var r = '';
            for (var k in o) {
                r += k;
                if (k === 'a') {
                    Object.setPrototypeOf(o, {b: 1, c: 1});
                    delete o.b;
                }
            }
        "#, "r", "ab"),
        (r#"
            var o = {__proto__: null, a: 0, b: 0, c: 0};
            var r = '';
            for (var a in o) {
                r += a + ':';
                if (a === 'a') {
                    delete o.b;
                    for (var i = 0; i < 200; i++) {
                        var garbage = {['unused' + i]: i};
                    }
                }
                for (var b in o) r += b;
                r += ';';
            }
        "#, "r", "a:ac;c:ac;"),
    ]);
}

#[test]
fn opening_shape_and_atoms_survive_collection_after_a_shape_change() {
    let mut vm = load(r#"
        function* keys() {
            var o = {__proto__: null, first: 0, ['dynamic' + 42]: 0, last: 0};
            for (var k in o) {
                if (k === 'first') {
                    delete o['dynamic' + 42];
                    Object.setPrototypeOf(o, {extra: 1});
                }
                yield k;
            }
        }
        var it = keys();
        var r = it.next().value;
    "#).unwrap();
    vm.realm.heap.set_trigger(Trigger { min: 1024, slack: 1024 });
    vm.run_main(0).unwrap();
    vm.collect_idle();
    vm.realm.run_text(r#"
        for (var i = 0; i < 1000; i++) {
            var garbage = {['unused' + i]: i};
        }
        r += ',' + it.next().value + ',' + it.next().value;
    "#).unwrap();
    assert_eq!(vm.global_text("r"), "first,last,undefined");
}

#[test]
fn proxy_and_host_keys_use_generic_enumeration() {
    check(&[(r#"
        var o = new Proxy({a: 0, b: 0}, {ownKeys() { return ['b', 'a']; }});
        var r = '';
        for (var k in o) r += k;
    "#, "r", "ba")]);

    struct NamedHost;
    impl Host<Realm> for NamedHost {
        fn call<'s>(&self, s: &mut Scope<'s, Realm>, _: u32, _: Local<'s>, _: Args<'s>, _: bool) -> Result<Local<'s>, Error<'s>> {
            Ok(s.undefined())
        }

        fn named_has(&self, _: &Value, _: u32, key: &[u16]) -> Result<bool, Pending> {
            Ok(key == "hostKey".encode_utf16().collect::<Vec<_>>())
        }

        fn named_get(&self, _: &Value, _: u32, _: &[u16]) -> Result<Option<Named<Value>>, Pending> {
            Ok(Some(Named::Text(Vec::new())))
        }

        fn named_keys(&self, _: &Value, _: u32) -> Result<Vec<Vec<u16>>, Pending> {
            Ok(vec!["hostKey".encode_utf16().collect()])
        }
    }

    let mut vm = load("").unwrap();
    vm.realm.set_host(Rc::new(NamedHost));
    enter(&mut vm.realm, |s| {
        let o = s.new_host_named(None, 1, false).unwrap();
        let global = s.global();
        s.set_str(global, "hostObject", o).unwrap();
    });
    vm.realm.run_text("var r = ''; for (var k in hostObject) r += k;").unwrap();
    assert_eq!(vm.global_text("r"), "hostKey");
}

#[test]
fn lazy_prototype_tail_is_rechecked_after_collection() {
    struct LazyHost(Cell<u32>);
    impl Host<Realm> for LazyHost {
        fn call<'s>(&self, s: &mut Scope<'s, Realm>, _: u32, _: Local<'s>, _: Args<'s>, _: bool) -> Result<Local<'s>, Error<'s>> {
            Ok(s.undefined())
        }

        fn lazy_member<'s>(&self, s: &mut Scope<'s, Realm>, object: Local<'s>, what: Lazy<'_>) -> Result<(), Error<'s>> {
            if matches!(what, Lazy::All) {
                let calls = self.0.get() + 1;
                self.0.set(calls);
                if calls == 2 {
                    let v = s.number(1.0);
                    s.define_str(object, "late", v, Attrs::ALL)?;
                }
            }
            Ok(())
        }
    }

    let mut vm = load("").unwrap();
    let host = Rc::new(LazyHost(Cell::new(0)));
    vm.realm.set_host(host.clone());
    enter(&mut vm.realm, |s| {
        let p = s.new_object().unwrap();
        s.set_prototype_of(p, None).unwrap();
        s.set_lazy(p, false).unwrap();
        let o = s.new_object_with(p).unwrap();
        let global = s.global();
        s.set_str(global, "receiver", o).unwrap();
    });
    let o = vm.realm.get_named(vm.realm.global.value(), "receiver").unwrap();
    assert!(vm.realm.for_in_open(o).unwrap().1.as_for_in_shape().is_none());
    assert!(vm.realm.for_in_open(o).unwrap().1.as_for_in_shape().is_some());
    assert_eq!(host.0.get(), 1);

    vm.collect_idle();
    vm.realm.run_text("var r = ''; for (var k in receiver) r += k;").unwrap();
    assert_eq!(vm.global_text("r"), "late");
    assert_eq!(host.0.get(), 2);
}
