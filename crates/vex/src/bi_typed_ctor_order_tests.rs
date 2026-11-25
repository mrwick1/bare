use crate::script;

#[test]
fn primitive_length_conversion_precedes_prototype_access() {
    let mut vm = script::run(r#"
        var target = function () {}.bind(null);
        var accesses = 0, typeError = false, requiresNew = false;
        Object.defineProperty(target, 'prototype', {get() { accesses++; return Uint8Array.prototype; }});
        try { Reflect.construct(Uint8Array, [Symbol()], target); } catch (e) { typeError = e instanceof TypeError; }
        try { Uint8Array(-1); } catch (e) { requiresNew = e instanceof TypeError; }
        var result = [typeError, requiresNew, accesses, Reflect.construct(Uint8Array, [2], target).length].join();
    "#).unwrap();
    assert_eq!(vm.global_text("result"), "true,true,0,2");
}
