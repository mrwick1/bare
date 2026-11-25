use crate::script;

fn check(src: &str, expected: &str) {
    let mut vm = script::run(src).unwrap();
    assert_eq!(vm.global_text("result"), expected);
}

#[test]
fn copy_within_moves_overlapping_bytes_without_converting_elements() {
    check(
        r#"
        var buffer = new ArrayBuffer(32);
        var bytes = new BigUint64Array(buffer);
        var floats = new Float64Array(buffer, 8);
        bytes.set([1n, 9221120237041090561n, 77n, 99n]);
        var same = floats.copyWithin(1, 0, 2) === floats;
        var result = same + '|' + bytes.join();
        floats.copyWithin(0, 1);
        result += '|' + bytes.join();
        "#,
        "true|1,9221120237041090561,9221120237041090561,77|1,9221120237041090561,77,77",
    );
}

#[test]
fn copy_within_rechecks_the_view_after_converting_arguments() {
    check(
        r#"
        var buffer = new ArrayBuffer(5, { maxByteLength: 8 });
        var tracking = new Uint8Array(buffer);
        var fixed = new Uint8Array(buffer, 0, 5);
        tracking.set([11, 22, 33, 44, 55]);
        tracking.copyWithin({ valueOf() { buffer.resize(3); return 1; } }, 0);
        var result = tracking.join();
        buffer.resize(5);
        var threw = false;
        try { fixed.copyWithin(1, { valueOf() { buffer.resize(3); return 0; } }); }
        catch (e) { threw = e instanceof TypeError; }
        result += '|' + threw;
        buffer.resize(5);
        var same = fixed.copyWithin({ valueOf() { buffer.resize(3); return 5; } }, 0) === fixed;
        result += '|' + same;
        "#,
        "11,11,22|true|true",
    );
}
