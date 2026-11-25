use crate::script;

#[test]
fn to_sorted_uses_a_snapshot_when_the_comparator_mutates_the_source() {
    let mut vm = script::run(r#"
        var a = new BigInt64Array([4n, 2n, 3n]);
        var copy = a.toSorted((x, y) => { a[0] = 99n; return x < y ? -1 : 1; });
        var nums = new Float64Array([10, 2, NaN]).toSorted();
        var result = [copy.join(), a.join(), nums.join()].join('/');
    "#).unwrap();
    assert_eq!(vm.global_text("result"), "2,3,4/99,2,3/2,10,NaN");
}
