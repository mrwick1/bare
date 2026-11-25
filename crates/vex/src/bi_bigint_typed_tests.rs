use crate::script;

fn check(src: &str, expected: &str) {
    let mut vm = script::run(src).unwrap();
    assert_eq!(vm.global_text("result"), expected);
}

#[test]
fn bigint_views_wrap_and_share_the_bytes() {
    check(
        "var a = new BigInt64Array([9223372036854775808n, -1n, 18446744073709551617n]);\n\
         var b = new BigUint64Array(a.buffer);\n\
         var result = a.join() + '|' + b.join() + '|' + typeof a[0];",
        "-9223372036854775808,-1,1|9223372036854775808,18446744073709551615,1|bigint",
    );
}

#[test]
fn bigint_views_convert_and_keep_their_content_type() {
    check(
        "var a = BigUint64Array.of(true, '9007199254740993', -1n);\n\
         var b = a.map(x => x + 1n);\n\
         var rejected = 0;\n\
         try { a[8] = 1; } catch(e) { if (e instanceof TypeError) rejected++; }\n\
         try { a.set(new Int8Array(0)); } catch(e) { if (e instanceof TypeError) rejected++; }\n\
         var result = b.join() + '|' + a.indexOf(1n) + '|' + a.includes(1) + '|' + rejected;",
        "2,9007199254740994,0|0|false|2",
    );
}

#[test]
fn bigint_methods_preserve_order_and_bit_patterns() {
    check(
        "var a = new BigInt64Array([0n, -1n, -9223372036854775808n, 9223372036854775807n]);\n\
         a.sort();\n\
         var before = a.join();\n\
         new BigUint64Array(a.buffer).set(a.subarray(0, 3), 1);\n\
         var d = new DataView(a.buffer);\n\
         d.setBigUint64(0, 18446744073709551615n, false);\n\
         var result = before + '|' + Array.from(a).join() + '|' + d.getBigInt64(0, false);",
        "-9223372036854775808,-1,0,9223372036854775807|-1,-9223372036854775808,-1,0|-1",
    );
}

#[test]
fn bigint_index_reads_work_through_descriptors_and_prototypes() {
    check(
        "var a = new BigUint64Array([18446744073709551615n]);\n\
         var child = Object.create(a);\n\
         var d = Object.getOwnPropertyDescriptor(a, '0');\n\
         var result = [d.value, child[0], child['0'], new Proxy(a, {})[0], '0' in child].join();",
        "18446744073709551615,18446744073709551615,18446744073709551615,18446744073709551615,true",
    );
}
