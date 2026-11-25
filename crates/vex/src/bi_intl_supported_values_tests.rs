use crate::script::run;

fn check(source: &str, expected: &str) {
    let mut vm = run(source).unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(vm.global_text("r"), expected);
}

#[test]
fn lists_supported_values_sorted_and_unique() {
    check(
        r#"
        var f = Intl.supportedValuesOf, z = f('timeZone');
        var sorted = a => a.every((v, i) => i == 0 || a[i - 1] < v);
        var r = [f.length, f('calendar').join(), f('numberingSystem').join(), f('collation').length,
                 f('currency').includes('USD'), sorted(f('currency')), f('unit').includes('kilometer'), sorted(f('unit')),
                 z.includes('Asia/Kolkata'), z.includes('UTC'), sorted(z), f('timeZone') !== z].join('|');
        "#,
        "1|gregory|latn|0|true|true|true|true|true|true|true|true",
    );
}

#[test]
fn rejects_an_unknown_key() {
    check(
        r#"
        var e = [];
        for (var k of ['timezone', undefined, 'x']) try { Intl.supportedValuesOf(k); } catch (x) { e.push(x instanceof RangeError); }
        try { Intl.supportedValuesOf(Symbol()); } catch (x) { e.push(x instanceof TypeError); }
        var r = e.join();
        "#,
        "true,true,true,true",
    );
}
