use super::parse;
use crate::script::{load, run};
use crate::value::Value;

fn check(source: &str, expected: &str) {
    let mut vm = run(source).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(vm.global_text("r"), expected);
}

#[test]
fn repeated_keys_do_not_allocate_temporary_heap_strings() {
    let mut vm = load("").unwrap();
    let rt = &mut vm.realm;
    let key = "k".repeat(64 * 1024);
    let source = rt.string(&format!("{{{}}}", vec![format!(r#""{key}":0"#); 64].join(",")));
    let before = rt.heap.grown_bytes();
    parse(rt, Value::UNDEFINED, &[source]).unwrap();
    assert!(rt.heap.grown_bytes() - before < 1024 * 1024);
}

#[test]
fn escaped_keys_preserve_utf16_and_duplicate_order() {
    check(
        r#"
        var o = JSON.parse('{"\\ud800":"\\udfff","ab":1,"middle":2,"a\\u0062":3,"Ω":"Ω"}');
        var keys = Object.keys(o);
        var r = [keys[0].charCodeAt(0), o[keys[0]].charCodeAt(0), keys.slice(1), o.ab, o.Ω].join('|');
        "#,
        "55296|57343|ab,middle,Ω|3|Ω",
    );
    check(
        r#"
        var o = JSON.parse('{"\\ud800":1,"' + String.fromCharCode(0xd800) + '":2}');
        var r = [Object.keys(o).length, o[String.fromCharCode(0xd800)]].join('|');
        "#,
        "1|2",
    );
}

#[test]
fn index_bounds_and_proto_are_own_data_properties() {
    check(
        r#"
        var o = JSON.parse('{"01":1,"4294967295":2,"-0":3,"4294967294":4,"0":5,"\\u0030":6,"__proto__":{"x":7}}');
        var d = Object.getOwnPropertyDescriptor(o, '__proto__');
        var r = [Object.keys(o), o[0], o[1], o[4294967294], o[4294967295], d.value.x,
                 d.writable, d.enumerable, d.configurable, Object.getPrototypeOf(o) === Object.prototype].join('|');
        "#,
        "0,4294967294,01,4294967295,-0,__proto__|6||4|2|7|true|true|true|true",
    );
}

#[test]
fn reviver_visits_children_then_parents_in_key_order() {
    check(
        r#"
        var order = [];
        var o = JSON.parse('{"z":[{"a":1}],"2":2,"a":3,"z":[{"a":4}]}', function(k, v) {
            order.push(k + ':' + (typeof v === 'object' ? 'object' : v));
            return v;
        });
        var r = order.join('|');
        "#,
        "2:2|a:4|0:object|z:object|a:3|:object",
    );
}

#[test]
fn nesting_limit_is_unchanged() {
    stacker::grow(8 * 1024 * 1024, || {
        check(
            r#"
            var s = '['.repeat(1000) + '0' + ']'.repeat(1000);
            var v = JSON.parse(s);
            var depth = 0;
            while (Array.isArray(v)) { v = v[0]; depth++; }
            var r = depth + ':' + v;
            try { JSON.parse('[' + s + ']'); } catch (e) { r += ':' + e.name; }
            "#,
            "1000:0:RangeError",
        );
    });
}
