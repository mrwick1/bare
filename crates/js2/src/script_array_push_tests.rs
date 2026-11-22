use super::tests::check;
use crate::object::F_PROTO;

#[test]
fn stock_array_push_watches_both_prototypes_without_interning_indices() {
    let mut vm = super::run("var a = []; for (var i = 0; i < 322; i++) a.push(i); var r = a.length + ':' + a[321];").unwrap();
    assert_eq!(vm.global_text("r"), "322:321");
    assert!(vm.realm.array_proto.has_flag(F_PROTO));
    assert!(vm.realm.object_proto.has_flag(F_PROTO));
    assert!(vm.realm.atoms.runtime().find(&crate::string::Chars::One(b"320")).is_none());
}

#[test]
fn array_push_falls_back_for_custom_receivers_and_storage() {
    check(&[
        ("var a = []; var r = a.push(1, 2) === 2 && a[0] === 1 && a[1] === 2 && a.push() === 2;", "r", "true"),
        ("var a = [1, 2]; delete a[1]; var r = a.push(3) === 3 && !(1 in a) && a[2] === 3;", "r", "true"),
        ("var a = []; a[100000] = 1; var r = a.push(2) === 100002 && a[100000] === 1 && a[100001] === 2;", "r", "true"),
        ("var a = []; a.length = 4294967295; var r; try { a.push(9); } catch (e) { r = e.name === 'RangeError' && a.length === 4294967295 && a[4294967295] === 9; }", "r", "true"),
        ("var a = Object.freeze([1]); var r; try { a.push(2); } catch (e) { r = e.name === 'TypeError' && a.length === 1 && !(1 in a); }", "r", "true"),
        ("var a = []; Object.defineProperty(a, 'length', {writable: false}); var r; try { a.push(); } catch (e) { r = e.name === 'TypeError' && a.length === 0; }", "r", "true"),
        ("var a = []; Object.defineProperty(a, 'length', {writable: false}); var r; try { a.push(2); } catch (e) { r = e.name === 'TypeError' && a.length === 0 && !(0 in a); }", "r", "true"),
        ("var a = {length: 1}; var r = Array.prototype.push.call(a, 2) === 2 && a.length === 2 && a[1] === 2;", "r", "true"),
        ("function f() { Object.setPrototypeOf(arguments, Array.prototype); arguments.length = 3; return arguments.push(9) === 4 && arguments.length === 4 && arguments[3] === 9; } var r = f(1);", "r", "true"),
        ("var seen; var p = {set 0(v) { seen = this === a && v === 3; }}; var a = []; Object.setPrototypeOf(a, p); var r = Array.prototype.push.call(a, 3) === 1 && seen && !Object.hasOwn(a, 0);", "r", "true"),
    ]);
}

#[test]
fn array_push_respects_host_index_write_rejection() {
    let mut vm = super::load("").unwrap();
    bare_js_api::enter(&mut vm.realm, |scope| {
        let array = scope.run_script("Array.prototype").unwrap();
        let host = scope.new_host_indexed(Some(array), 1).unwrap();
        let global = scope.global();
        scope.set_str(global, "host", host).unwrap();
        let result = scope.run_script("Object.defineProperty(host, 'length', {value: 0, writable: true}); var r; try { host.push(3); } catch (e) { r = e.name === 'TypeError' && host.length === 0; } r;").unwrap();
        assert_eq!(scope.as_bool(result), Some(true));
    });
}

#[test]
fn array_push_rechecks_the_stock_prototypes_after_warmup() {
    check(&[
        ("var a = []; a.push(1); var seen; Object.defineProperty(Object.prototype, '1', {configurable: true, set(v) { seen = this === a && v === 3; }}); var r = a.push(3) === 2 && seen && !Object.hasOwn(a, 1); delete Object.prototype[1];", "r", "true"),
        ("var a = []; a.push(1); Array.prototype[1] = 8; Object.freeze(Array.prototype); var r; try { a.push(3); } catch (e) { r = e.name === 'TypeError' && a.length === 1 && !Object.hasOwn(a, 1) && a[1] === 8; }", "r", "true"),
        ("var a = []; a.push(1); Object.defineProperty(Array.prototype, '1', {value: 8}); var r; try { a.push(3); } catch (e) { r = e.name === 'TypeError' && a.length === 1 && !Object.hasOwn(a, 1); }", "r", "true"),
        ("var a = []; a.push(1); var seen; Object.setPrototypeOf(Array.prototype, {set 1(v) { seen = v; }}); var r = a.push(3) === 2 && seen === 3 && !Object.hasOwn(a, 1);", "r", "true"),
    ]);
}
