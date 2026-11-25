use crate::script;

#[test]
fn reduce_right_reads_mutated_elements_in_reverse_order() {
    let mut vm = script::run(r#"
        var a = new Int8Array([1, 2, 3]);
        var result = a.reduceRight(function (acc, element, index, array) {
            if (index === 2 && array === a) a[0] = 9;
            return acc + ':' + index + '=' + element;
        }, 'start');
    "#).unwrap();
    assert_eq!(vm.global_text("result"), "start:2=3:1=2:0=9");
}

#[test]
fn reduce_right_uses_the_last_bigint_when_no_initial_value_is_given() {
    let mut vm = script::run(r#"
        var a = new BigInt64Array([2n, 3n, 10n]);
        var empty = new BigInt64Array(0);
        var error;
        try { empty.reduceRight((acc, element) => acc + element); }
        catch (e) { error = e instanceof TypeError; }
        var result = [a.reduceRight((acc, element) => acc - element),
                      empty.reduceRight(() => 0n, undefined) === undefined, error].join();
    "#).unwrap();
    assert_eq!(vm.global_text("result"), "5,true,true");
}
