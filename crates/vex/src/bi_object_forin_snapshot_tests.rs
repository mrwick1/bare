use super::{load, tests::check};
use crate::interp_rt::Realm;

#[test]
fn ordinary_snapshot_keeps_internal_indices() {
    let mut realm = load("").unwrap().realm;
    realm.run_text(r#"
        Object.getOwnPropertyNames(Array.prototype);
        Object.getOwnPropertyNames(Object.prototype);
        var source = [1, , 2];
    "#).unwrap();
    let source = realm.get_named(realm.global.value(), "source").unwrap();
    let (_, snapshot) = realm.for_in_open(source).unwrap();
    let keys = Realm::obj(snapshot).unwrap();
    assert_eq!(keys.get_element(0).unwrap().as_int(), Some(0));
    assert_eq!(keys.get_element(1).unwrap().as_int(), Some(2));
}

#[test]
fn dictionary_order_and_inherited_shadows() {
    check(&[(r#"
        Object.getOwnPropertyNames(Object.prototype);
        var proto = {2: 1, shadow: 1, hidden: 1, inherited: 1};
        var o = Object.create(proto);
        o.z = 0;
        o[4294967295] = 0;
        o[4294967294] = 0;
        o[2] = 0;
        o[1] = 0;
        o.shadow = 0;
        o[Symbol()] = 0;
        Object.defineProperty(o, 'hidden', {value: 0});
        Object.preventExtensions(o);
        var r = [];
        for (var k in o) r.push(k);
        r = r.join(',');
    "#, "r", "1,2,4294967294,z,4294967295,shadow,inherited")]);
}

#[test]
fn sparse_array_snapshot_keeps_opening_order_and_skips_deletions() {
    check(&[(r#"
        Object.getOwnPropertyNames(Object.prototype);
        var p = {length: 0, 5: 0, inherited: 0};
        var o = [];
        o[5] = 0;
        o[1000000] = 0;
        o[9] = 0;
        o.tail = 0;
        Object.setPrototypeOf(o, p);
        var r = [];
        for (var k in o) {
            r.push(k);
            if (k === '5') {
                delete o[9];
                o[10] = 0;
            }
        }
        r = r.join(',');
    "#, "r", "5,1000000,tail,inherited"),
    (r#"
        Object.getOwnPropertyNames(Array.prototype);
        Object.getOwnPropertyNames(Object.prototype);
        var p = [];
        p[1] = 0;
        var o = [0, , 0];
        Object.setPrototypeOf(o, p);
        var r = [];
        for (var k in o) r.push(k);
        r = r.join(',');
    "#, "r", "0,2,1"),
    (r#"
        Object.getOwnPropertyNames(Object.prototype);
        var o = [0, 0, 0];
        Object.defineProperty(o, '1', {enumerable: false});
        Object.defineProperty(o, '5', {
            enumerable: true,
            get() { throw new Error('getter ran'); }
        });
        Object.setPrototypeOf(o, {1: 0, 5: 0});
        var r = [];
        for (var k in o) r.push(k);
        r = r.join(',');
    "#, "r", "0,2,5")]);
}

#[test]
fn exotic_sources_and_prototypes_keep_their_key_rules() {
    check(&[
        (r#"
            var p = new Proxy({1: 0, 2: 0, hidden: 0}, {
                ownKeys() { return ['2', 'hidden', '1']; }
            });
            var o = Object.create(p);
            Object.defineProperty(o, 'hidden', {value: 0});
            var r = [];
            for (var k in o) r.push(k);
            r = r.join(',');
        "#, "r", "2,1"),
        (r#"
            var o = new Uint8Array(3);
            o.extra = 0;
            var r = [];
            for (var k in o) r.push(k);
            r = r.join(',');
        "#, "r", "0,1,2,extra"),
        (r#"
            function f(a, b) {
                delete arguments[0];
                var out = [];
                for (var k in arguments) out.push(k);
                return out.join(',');
            }
            var r = f(1, 2);
        "#, "r", "1"),
        (r#"
            var r = [];
            for (var k in 'xy') r.push(k);
            r = r.join(',');
        "#, "r", "0,1"),
    ]);
}
