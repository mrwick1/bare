use std::cell::Cell;

thread_local! {
    pub(super) static CONVERSIONS: Cell<usize> = const { Cell::new(0) };
}

#[test]
fn the_span_builder_converts_only_non_integer_decimals() {
    let pad = " ".repeat(crate::parse::data_scan::BULK_SOURCE_BYTES);
    let nums = ",11".repeat(550);
    let long = format!("1{}e-1100", "0".repeat(1100));
    let src = format!(
        "{pad}var a = [-999999999,0,999999999,-0,1e0,1.25,2147483648,9007199254740993,{long}{nums}];\n
        var ok = a[0] === -999999999 && a[1] === 0 && a[2] === 999999999 && 1 / a[3] === -Infinity &&
            a[4] === 1 && a[5] === 1.25 && a[6] === 2147483648 && a[7] === 9007199254740992 &&
            a[8] === 1 && a[9] === 11 && a.length === 559;"
    );
    CONVERSIONS.with(|n| n.set(0));
    let mut vm = crate::script::run(&src).unwrap();
    assert_eq!(vm.global_text("ok"), "true");
    assert_eq!(CONVERSIONS.with(Cell::get), 6);
}
