use crate::script;

#[test]
fn predicates_short_circuit_and_read_current_elements() {
    let mut vm = script::run(r#"
        var numbers = new Uint8Array([1, 2, 3]);
        var context = {};
        var seen = [];
        var every = numbers.every(function (value, index, array) {
            if (this === context && array === numbers) seen.push(value);
            if (index === 0) array[1] = 0;
            return value;
        }, context);
        var big = new BigInt64Array([0n, 2n, 3n]);
        var some = big.some(function (value) {
            seen.push(String(value));
            return value;
        });
        var empty = new Uint8Array(0);
        var unused = () => { throw 1; };
        var result = [every, some, seen.join('/'), empty.every(unused), empty.some(unused)].join();
    "#).unwrap();
    assert_eq!(vm.global_text("result"), "false,true,1/0/0/2,true,false");
}
