use crate::script;

fn check(src: &str, expected: &str) {
    let mut vm = script::run(src).unwrap();
    assert_eq!(vm.global_text("result"), expected);
}

#[test]
fn filter_reads_each_value_before_running_the_callback() {
    check(
        r#"
        var a = new Uint8Array([1, 2, 3]), seen = [], ctx = {};
        var b = a.filter(function (v, i, source) {
            seen.push(v + ':' + i + ':' + (this === ctx && source === a));
            if (i === 0) { a[0] = 9; a[1] = 8; }
            return i !== 2;
        }, ctx);
        var result = seen.join() + '|' + b.join();
        "#,
        "1:0:true,8:1:true,3:2:true|1,8",
    );
}

#[test]
fn filter_creates_the_species_after_selection() {
    check(
        r#"
        var a = new BigInt64Array([-1n, 0n, 9007199254740993n]), calls = [];
        a.constructor = { [Symbol.species]: function (n) {
            calls.push(n);
            return new BigUint64Array(n);
        } };
        var b = a.filter(function (v) { calls.push(String(v)); return v !== 0n; });
        var result = calls.join() + '|' + (b instanceof BigUint64Array) + '|' + b.join();
        "#,
        "-1,0,9007199254740993,2|true|18446744073709551615,9007199254740993",
    );
}

#[test]
fn filter_propagates_callback_errors_before_reading_species() {
    check(
        r#"
        var a = new Uint8Array([1]), marker = {}, calls = 0, caught;
        Object.defineProperty(a, 'constructor', { get() { calls++; throw 0; } });
        try { a.filter(function () { throw marker; }); } catch (e) { caught = e; }
        var result = (caught === marker) + '|' + calls;
        "#,
        "true|0",
    );
}
