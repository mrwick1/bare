use super::*;
use crate::atoms::SharedAtoms;
use crate::heap::Trigger;
use crate::script::load;
use std::sync::Arc;

#[test]
fn full_and_short_ranges_on_large_flat_strings_and_ropes() {
    let mut rt = Realm::new(Arc::new(SharedAtoms::default()));
    let left = rt.string(&"a".repeat(256 * 1024));
    let right = rt.string("é😀");
    let rope = Str::concat(&mut rt.heap, Realm::str_of(left).unwrap(), Realm::str_of(right).unwrap()).unwrap();
    for method in [substring, slice, substr] {
        assert_eq!(method(&mut rt, left, &[]).unwrap(), left);
        assert_eq!(method(&mut rt, right, &[]).unwrap(), right);
        assert_eq!(method(&mut rt, rope.value(), &[]).unwrap(), rope.value());
        assert!(rope.chars().is_none());
    }
    let short = substring(&mut rt, left, &[Value::int(131_072), Value::int(131_073)]).unwrap();
    assert!(matches!(Realm::str_of(short).unwrap().chars(), Some(Chars::One([b'a']))));
    let cut = Value::int(256 * 1024);
    let end = Value::int(256 * 1024 + 1);
    let narrow = substring(&mut rt, rope.value(), &[cut, end]).unwrap();
    assert!(matches!(Realm::str_of(narrow).unwrap().chars(), Some(Chars::One([0xe9]))));
    let wide = slice(&mut rt, rope.value(), &[end, Value::int(-1)]).unwrap();
    assert!(matches!(Realm::str_of(wide).unwrap().chars(), Some(Chars::Two([0xd83d]))));
    let trail = substr(&mut rt, rope.value(), &[Value::int(-1), Value::int(1)]).unwrap();
    assert!(matches!(Realm::str_of(trail).unwrap().chars(), Some(Chars::Two([0xde00]))));
    assert_eq!(code_point_at(&mut rt, rope.value(), &[end]).unwrap().as_int(), Some(0x1f600));
    assert_eq!(code_point_at(&mut rt, rope.value(), &[Value::int(256 * 1024 + 2)]).unwrap().as_int(), Some(0xde00));
    assert!(code_point_at(&mut rt, left, &[Value::int(-1)]).unwrap().is_undefined());
    assert_eq!(code_point_at(&mut rt, left, &[Value::number(f64::NAN)]).unwrap().as_int(), Some(97));
}

#[test]
fn range_coercions_run_in_order_and_keep_the_converted_receiver_live() {
    let mut vm = load(r#"
        var log = '';
        var receiver = {toString() { log += 's'; return 'abc' + 'def'.repeat(100); }};
        var start = {valueOf() {
            log += 'a';
            var held = [];
            for (var i = 0; i < 300; i++) held.push({i});
            return 1;
        }};
        var end = {valueOf() { log += 'b'; return 3; }};
        var r = [
            String.prototype.substring.call(receiver, start, end),
            String.prototype.slice.call(receiver, start, end),
            String.prototype.substr.call(receiver, start, end),
            log,
            'abc'.substring(-3, NaN),
            'abc'.substring(2, NaN),
            'abc'.slice(-2, 1),
            'abc'.slice(NaN, -1),
            'abc'.substr(-2, NaN),
            'abc'.substr(NaN, 2)
        ].join('|');
    "#).unwrap();
    vm.realm.heap.set_trigger(Trigger { min: 1024, slack: 1024 });
    vm.run_main(0).unwrap();
    assert!(vm.realm.collections > 0);
    assert_eq!(vm.global_text("r"), "bc|bc|bcd|sabsabsab||ab||ab||ab");
}
