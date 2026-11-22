use crate::interp_rt::Realm;
use crate::atoms::SharedAtoms;
use crate::heap::BLOCK;
use crate::value::Value;
use std::sync::Arc;

#[test]
fn short_integer_concat_allocates_only_the_result() {
    let mut realm = Realm::new(Arc::new(SharedAtoms::default()));
    let prefix = realm.string("x");
    let before = realm.heap.grown_bytes();
    let mut result = Value::UNDEFINED;
    for i in 0..32_000 {
        result = realm.add_slow(prefix, Value::int(i)).unwrap();
    }
    assert_eq!(realm.text(Realm::str_of(result).unwrap()), "x31999");
    assert!(realm.heap.grown_bytes() - before <= 32_000 * 32 + BLOCK);
}

#[test]
fn integer_concat_preserves_order_and_text() {
    let mut realm = Realm::new(Arc::new(SharedAtoms::default()));
    for text in [
        "",
        "x",
        "é",
        "文",
        "a long string that requires the rope path",
    ] {
        let s = realm.string(text);
        let n = Value::int(i32::MIN);
        let left = realm.add_slow(s, n).unwrap();
        let right = realm.add_slow(n, s).unwrap();
        let left = realm.flat(Realm::str_of(left).unwrap());
        let right = realm.flat(Realm::str_of(right).unwrap());
        assert_eq!(realm.text(left), format!("{text}{}", i32::MIN));
        assert_eq!(realm.text(right), format!("{}{text}", i32::MIN));
    }
}
