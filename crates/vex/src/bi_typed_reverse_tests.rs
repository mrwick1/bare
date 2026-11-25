use crate::script;

#[test]
fn reverse_preserves_bits_and_stays_within_the_view() {
    let mut vm = script::run(r#"
        var raw = new Uint32Array([9, 0x7fa12345, 0x80000000, 8]);
        var a = new Float32Array(raw.buffer, 4, 2);
        var copy = a.toReversed();
        var result = [new Uint32Array(copy.buffer).join(), a.reverse() === a, raw.join()].join('/');
    "#).unwrap();
    assert_eq!(vm.global_text("result"), "2147483648,2141266757/true/9,2147483648,2141266757,8");
}
