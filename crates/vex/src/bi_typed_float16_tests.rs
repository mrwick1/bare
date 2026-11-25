use super::{decode_f16, encode_f16};
use crate::script;

#[test]
fn all_non_nan_binary16_values_round_trip() {
    for bits in 0..=u16::MAX {
        let decoded = decode_f16(bits);
        if bits & 0x7fff > 0x7c00 {
            assert!(decoded.is_nan());
        } else {
            assert_eq!(encode_f16(decoded), bits);
        }
    }
}

#[test]
fn float16_rounds_directly_and_dataview_respects_byte_order() {
    let mut vm = script::run(r#"
        var halfway = 1 + 2 ** -11;
        var above = halfway + 2 ** -52;
        var a = new Float16Array([halfway, above, -(2 ** -25)]);
        var d = new DataView(a.buffer);
        var bits = new Uint8Array(a.buffer);
        var first = [a[0], a[1], 1 / a[2], bits[2], bits[3], Math.f16round(above)].join();
        d.setFloat16(0, 1.5);
        var result = first + '/' + [bits[0], bits[1], d.getFloat16(0), a.BYTES_PER_ELEMENT].join();
    "#).unwrap();
    assert_eq!(vm.global_text("result"), "1,1.0009765625,-Infinity,1,60,1.0009765625/62,0,1.5,2");
}
