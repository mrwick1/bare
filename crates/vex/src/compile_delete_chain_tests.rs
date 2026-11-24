use crate::script;

fn run(source: &str) -> String {
    let mut vm = script::load(source).unwrap();
    vm.run_main(0).unwrap();
    vm.global_text("result")
}

#[test]
fn delete_of_an_optional_chain_deletes_or_short_circuits() {
    let source = r#"
        var o = { a: { b: 1, c: 2 }, k: 'c' };
        var n = null, calls = 0;
        function key() { calls++; return 'b'; }
        var r1 = delete o?.a.b;
        var r2 = delete o.a?.[o.k];
        var r3 = delete n?.x;
        var r4 = delete n?.[key()];
        var r5 = delete n?.x.y.z;
        var r6 = delete undefined?.x;
        var frozen = Object.freeze({ q: 1 });
        var r7 = delete frozen?.q;
        var result = [r1, r2, r3, r4, r5, r6, r7, 'b' in o.a, 'c' in o.a, calls].join();
    "#;
    assert_eq!(run(source), "true,true,true,true,true,true,false,false,false,0");
}

#[test]
fn strict_delete_of_an_optional_chain_throws_on_a_frozen_property() {
    let source = r#"
        'use strict';
        var f = Object.freeze({ q: 1 }), result;
        try { delete f?.q; result = 'no'; } catch (e) { result = e.constructor.name; }
    "#;
    assert_eq!(run(source), "TypeError");
}
