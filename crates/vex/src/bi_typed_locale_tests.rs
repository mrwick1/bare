use crate::script;

#[test]
fn to_locale_string_uses_internal_length_and_reads_current_values() {
    let mut vm = script::run(r#"
        var a = new Uint8Array(new ArrayBuffer(3, {maxByteLength: 4}));
        a.set([4, 5, 6]);
        Object.defineProperty(a, 'length', {get() { throw 1; }});
        Number.prototype.toLocaleString = function (locales, options) {
            a.buffer.resize(1);
            return '[' + [this, locales, options.marker, arguments.length].join(':') + ']';
        };
        BigInt.prototype.toLocaleString = function () { return '{' + this + '}'; };
        var result = a.toLocaleString('en', {marker: 3}, 'ignored') + '/' +
                     new BigInt64Array([2n]).toLocaleString();
    "#).unwrap();
    assert_eq!(vm.global_text("result"), "[4:en:3:2],,/{2}");
}
