use crate::script;
use crate::string::Chars;

#[test]
fn array_push_does_not_intern_missing_prototype_indices() {
    let mut vm = script::load("").unwrap();
    bare_js_api::enter(&mut vm.realm, |scope| {
        scope
            .run_script("var a = []; for (var i = 0; i <= 321; i++) a.push(i);")
            .unwrap();
    });
    assert!(vm.realm.atoms.runtime().find(&Chars::One(b"321")).is_none());
}

#[test]
fn array_push_finds_inherited_index_setters() {
    let mut vm = script::load("").unwrap();
    bare_js_api::enter(&mut vm.realm, |scope| {
        let result = scope
            .run_script(
                "var seen = 0;
                 Object.defineProperty(Object.prototype, '0', { set(v) { seen = v; } });
                 var a = []; a.push(42);
                 seen === 42 && a.length === 1 && !Object.hasOwn(a, 0);",
            )
            .unwrap();
        assert_eq!(scope.as_bool(result), Some(true));
    });
}
