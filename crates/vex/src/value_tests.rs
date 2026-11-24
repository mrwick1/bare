// Tests of `value.rs`. Miri runs them in CI: the pointer round trip must keep the provenance of the allocation.
use super::*;

#[test]
fn a_value_is_8_bytes() {
    assert_eq!(std::mem::size_of::<Value>(), 8);
}

#[test]
fn doubles_are_stored_as_they_are() {
    for f in [0.0, -0.0, 1.5, -1.5, f64::MAX, f64::MIN, f64::MIN_POSITIVE, 5e-324, f64::INFINITY, f64::NEG_INFINITY] {
        let v = Value::number(f);
        assert_eq!(v.bits(), f.to_bits(), "{f}");
        assert_eq!(v.as_f64().map(f64::to_bits), Some(f.to_bits()), "{f}");
        assert!(v.is_number() && v.as_int().is_none() && v.as_pointer().is_none(), "{f}");
    }
}

#[test]
fn every_nan_that_reaches_the_tag_space_becomes_the_canonical_nan() {
    // The x86 default NaN (sign set, quiet) is below the tag space: it stays as it is.
    let x86 = f64::from_bits(0xFFF8_0000_0000_0000);
    assert_eq!(Value::number(x86).bits(), x86.to_bits());
    for bits in [0xFFF9_0000_0000_0000u64, 0xFFFE_0000_DEAD_BEEF, u64::MAX, Value::NULL.bits(), Value::int(7).bits()] {
        let v = Value::number(f64::from_bits(bits));
        assert_eq!(v.bits(), CANONICAL_NAN, "{bits:#x}");
        assert!(v.as_f64().unwrap().is_nan() && v.as_pointer().is_none() && v.as_int().is_none(), "{bits:#x}");
    }
    for bits in [0x7FF8_0000_0000_0001u64, 0x7FF0_0000_0000_0001] {
        assert!(Value::number(f64::from_bits(bits)).as_f64().unwrap().is_nan());
    }
}

#[test]
fn integers_booleans_null_and_undefined_have_their_own_tags() {
    for i in [0, 1, -1, i32::MAX, i32::MIN] {
        let v = Value::int(i);
        assert_eq!(v.as_int(), Some(i));
        assert_eq!(v.as_f64(), Some(i as f64));
        assert!(v.is_number() && v.as_bool().is_none());
    }
    assert_eq!(Value::bool(true).as_bool(), Some(true));
    assert_eq!(Value::bool(false).as_bool(), Some(false));
    assert!(Value::UNDEFINED.is_undefined() && !Value::UNDEFINED.is_null() && !Value::UNDEFINED.is_number());
    assert!(Value::NULL.is_null() && !Value::NULL.is_undefined() && Value::NULL.as_bool().is_none());
    let all = [Value::UNDEFINED, Value::NULL, Value::TRUE, Value::FALSE, Value::int(0), Value::number(0.0), Value::number(-0.0)];
    for (i, a) in all.iter().enumerate() {
        for b in &all[i + 1..] {
            assert_ne!(a.bits(), b.bits());
        }
    }
}

#[test]
#[allow(unsafe_code)]
fn a_pointer_comes_back_with_its_kind_and_its_provenance() {
    let cells: Vec<Box<u64>> = (0..4).map(|i| Box::new(100 + i)).collect();
    for (i, kind) in [Kind::String, Kind::Symbol, Kind::BigInt, Kind::Object].into_iter().enumerate() {
        let p = NonNull::from(&*cells[i]).cast::<u8>();
        let v = Value::pointer(kind, p);
        assert!(!v.is_number() && v.as_int().is_none() && !v.is_undefined());
        let (k, q) = v.as_pointer().unwrap();
        assert_eq!(k, kind);
        assert_eq!(q, p);
        // Miri checks that the address carries the provenance of the box.
        assert_eq!(unsafe { *q.cast::<u64>().as_ptr() }, 100 + i as u64);
    }
}
