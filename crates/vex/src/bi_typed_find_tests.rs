use crate::script;

#[test]
fn find_methods_select_the_first_match_in_their_direction() {
    let mut vm = script::run(r#"
        var a = new BigInt64Array([2n, 3n, 4n, 5n]);
        var even = value => value % 2n === 0n;
        var miss = () => false;
        var result = [a.find(even), a.findIndex(even), a.findLast(even), a.findLastIndex(even),
                      a.find(miss) === undefined, a.findIndex(miss),
                      a.findLast(miss) === undefined, a.findLastIndex(miss)].join();
    "#).unwrap();
    assert_eq!(vm.global_text("result"), "2,0,4,2,true,-1,true,-1");
}

#[test]
fn find_last_reads_mutations_and_returns_the_value_seen_by_the_callback() {
    let mut vm = script::run(r#"
        var a = new Uint8Array([1, 2, 3]);
        var context = {};
        var seen = [];
        var match = a.findLast(function (value, index, array) {
            if (this === context && array === a) seen.push(index);
            if (index === 2) array[1] = 9;
            if (value === 9) array[index] = 0;
            return value === 9;
        }, context);
        var result = [match, seen.join('/'), a[1]].join();
    "#).unwrap();
    assert_eq!(vm.global_text("result"), "9,2/1,0");
}
