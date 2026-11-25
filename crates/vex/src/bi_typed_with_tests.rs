use crate::script;

#[test]
fn with_uses_the_original_length_and_checks_the_current_bounds() {
    let mut vm = script::run(r#"
        var a = new Float64Array(new ArrayBuffer(24, {maxByteLength: 32}));
        a.set([1, 2, 3]);
        var copy = a.with(-3, {valueOf() { a.buffer.resize(8); return 9; }});
        var error = false;
        try { a.with(1, 3); } catch (e) { error = e instanceof RangeError; }
        var result = [copy[0], isNaN(copy[1]), isNaN(copy[2]), a[0], error].join();
    "#).unwrap();
    assert_eq!(vm.global_text("result"), "9,true,true,1,true");
}
