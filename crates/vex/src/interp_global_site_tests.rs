use super::*;
use crate::script;

#[test]
fn frame_rebuilds_feedback_with_a_different_site_count() {
    let mut vm = script::load("var x = 42; function read() { return x; }").unwrap();
    let ci = 0;
    vm.run_main(ci).unwrap();
    vm.fbs[ci].get_or_insert_with(Box::default).init(&Code::new(0, 0));
    vm.run_main(ci).unwrap();
    assert_eq!(vm.fb(ci).caches.len(), vm.realm.prog[ci].sites.len());
}

#[test]
fn lazy_nested_globals_refill_after_redefinition() {
    let source = r#"
        globalThis.x = 1;
        var log = '';
        function step() { 'use strict'; x = x + 1; return x; }
        var obj = { valueOf() { return step(); } };
        var a = +obj + ',' + +obj + ',' + +obj;
        Object.defineProperty(globalThis, 'x', {
            get() { log += 'g'; return 10; },
            set(v) { log += 's' + v; },
            configurable: true
        });
        var b = +obj;
        Object.defineProperty(globalThis, 'x', { value: 20, writable: true });
        var c = +obj + ',' + +obj + ',' + +obj;
        var result = a + '/' + b + '/' + log + '/' + c;
    "#;
    let mut vm = script::load(source).unwrap();
    vm.run_main(0).unwrap();
    assert_eq!(vm.global_text("result"), "2,3,4/10/gs11g/21,22,23");
}
