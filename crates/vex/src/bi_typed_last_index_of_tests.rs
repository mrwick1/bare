use crate::script;

#[test]
fn last_index_of_uses_strict_equality_and_observes_resizes() {
    let mut vm = script::run(r#"
        var a = new BigInt64Array([1n, 2n, 1n]);
        var b = new Uint8Array(new ArrayBuffer(3, {maxByteLength: 4}));
        b.set([7, 7, 7]);
        var result = [a.lastIndexOf(1n), a.lastIndexOf(1n, undefined),
                      a.lastIndexOf(1n, -2), a.lastIndexOf(1),
                      b.lastIndexOf(7, {valueOf() { b.buffer.resize(1); return 2; }})].join();
    "#).unwrap();
    assert_eq!(vm.global_text("result"), "2,0,0,-1,0");
}
