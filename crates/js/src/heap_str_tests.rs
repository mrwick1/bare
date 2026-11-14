//! Strings of three kinds: atoms (short, interned), flat strings and ropes (long, not interned).

use super::*;

fn long(c: char, n: usize) -> String {
    std::iter::repeat_n(c, n).collect()
}

#[test]
fn short_strings_are_atoms_and_long_strings_are_not() {
    let mut h = Heap::default();
    let a = h.intern_str(&long('a', ATOM_MAX)).unwrap();
    assert!(a.is_atom());
    assert!(a.same(h.intern_str(&long('a', ATOM_MAX)).unwrap()));
    let (b, c) = (h.intern_str(&long('a', ATOM_MAX + 1)).unwrap(), h.intern_str(&long('a', ATOM_MAX + 1)).unwrap());
    assert!(!b.is_atom() && !b.is_rope());
    assert!(!b.same(c), "a long string is a new string each time");
    assert!(h.str_eq(b, c));
    assert!(!h.str_eq(a, b));
    // Two interner entries only for the short strings.
    let n = h.interner.count;
    h.intern_str(&long('b', 1000)).unwrap();
    assert_eq!(h.interner.count, n);
}

#[test]
fn concat_copies_a_short_result_and_makes_a_rope_for_a_long_one() {
    let mut h = Heap::default();
    let (x, y) = (h.intern_str("ab").unwrap(), h.intern_str("cd").unwrap());
    let xy = h.concat(x, y).unwrap();
    assert!(xy.is_atom() && xy.same(h.intern_str("abcd").unwrap()));
    let big = h.intern_str(&long('q', ATOM_MAX)).unwrap();
    let r = h.concat(big, x).unwrap();
    assert!(r.is_rope() && !r.is_atom());
    assert_eq!(h.str_len(r).unwrap(), ATOM_MAX + 2);
    let flat = h.intern_str(&format!("{}ab", long('q', ATOM_MAX))).unwrap();
    assert!(!flat.is_rope() && !flat.same(r));
    assert!(h.str_eq(r, flat) && h.str_eq(flat, r));
    assert_eq!(h.to_string_lossy(r).unwrap(), format!("{}ab", long('q', ATOM_MAX)));
    // An empty side is the other string itself.
    let e = h.intern_str("").unwrap();
    assert!(h.concat(r, e).unwrap().same(r) && h.concat(e, r).unwrap().same(r));
}

#[test]
fn a_rope_reads_like_the_flat_string() {
    let mut h = Heap::default();
    let parts = ["alpha-", "beta-", "gamma-", "delta-", "epsilon-", "zeta-", "eta-"];
    let mut s = h.intern_str("").unwrap();
    let mut want = String::new();
    for p in parts {
        let id = h.intern_str(p).unwrap();
        s = h.concat(s, id).unwrap();
        want += p;
    }
    // A tree on both sides.
    let t = h.concat(s, s).unwrap();
    want = format!("{want}{want}");
    assert!(t.is_rope());
    assert_eq!(h.str_len(t).unwrap(), want.len());
    assert_eq!(h.code_unit_at(t, 7).unwrap(), Some(want.as_bytes()[7] as u16));
    assert_eq!(h.str_units(t).unwrap(), want.encode_utf16().collect::<Vec<_>>());
    // The second read is the same buffer.
    assert!(std::ptr::eq(h.str_units(t).unwrap(), h.str_units(t).unwrap()));
    let sub = h.substring(t, 2, 9).unwrap();
    assert_eq!(h.to_string_lossy(sub).unwrap(), &want[2..9]);
}

#[test]
fn equality_of_strings_of_all_kinds() {
    let mut h = Heap::default();
    let text = long('z', 50);
    let flat = h.intern_str(&text).unwrap();
    let flat2 = h.intern_str(&text).unwrap();
    let half = h.intern_str(&long('z', 25)).unwrap();
    let rope = h.concat(half, half).unwrap();
    let atom = h.atom_of(flat).unwrap();
    assert!(atom.is_atom() && atom.same(h.atom_of(rope).unwrap()) && atom.same(h.atom_of(flat2).unwrap()));
    for (a, b) in [(flat, flat2), (flat, rope), (rope, flat2), (atom, flat), (atom, rope), (rope, atom)] {
        assert!(h.strict_equals(Value::String(a), Value::String(b)));
        assert!(h.same_value(Value::String(a), Value::String(b)));
        assert!(h.same_value_zero(Value::String(a), Value::String(b)));
        assert!(h.loose_equals(Value::String(a), Value::String(b)).unwrap());
    }
    let other = h.intern_str(&long('y', 50)).unwrap();
    let y25 = h.intern_str(&long('y', 25)).unwrap();
    let other_rope = h.concat(half, y25).unwrap();
    for (a, b) in [(flat, other), (rope, other), (rope, other_rope), (atom, other_rope), (flat, half)] {
        assert!(!h.strict_equals(Value::String(a), Value::String(b)));
        assert!(!h.loose_equals(Value::String(a), Value::String(b)).unwrap());
    }
}

#[test]
fn a_key_is_an_atom() {
    let mut h = Heap::default();
    let text = long('k', 100);
    let a = h.intern_str(&text).unwrap();
    let half = h.intern_str(&long('k', 50)).unwrap();
    let rope = h.concat(half, half).unwrap();
    let (ka, kr) = (h.key_from_string(a).unwrap(), h.key_from_string(rope).unwrap());
    assert_eq!(ka, kr);
    let o = h.new_object(None).unwrap();
    h.set(o, ka, Value::Number(1.0)).unwrap();
    assert!(matches!(h.get(o, kr), Ok(Get::Value(Value::Number(n))) if n == 1.0));
    // A key made by hand from a string that is not an atom is refused.
    assert!(matches!(h.get(o, PropertyKey::String(a)), Err(HeapError::InvalidKey)));
    assert!(matches!(h.to_property_key(Value::String(rope)), Ok(k) if k == ka));
    // The key of a long digit string is not an index.
    let digits = h.intern_str(&long('1', 40)).unwrap();
    assert!(matches!(h.key_from_string(digits), Ok(PropertyKey::String(_))));
}

#[test]
fn a_rope_survives_collect_and_keeps_its_parts_until_it_is_flat() {
    let mut h = Heap::default();
    let mut s = h.intern_str("").unwrap();
    for i in 0..100 {
        let part = h.intern_str(&format!("part-{i:04}-{}", long('x', 30))).unwrap();
        s = h.concat(s, part).unwrap();
    }
    let want = h.to_string_lossy(s).unwrap();
    // Build it again, so that the first read is after the collect.
    let mut t = h.intern_str("").unwrap();
    for i in 0..100 {
        let part = h.intern_str(&format!("part-{i:04}-{}", long('x', 30))).unwrap();
        t = h.concat(t, part).unwrap();
    }
    let before = h.stats();
    let c = h.collect(&[Value::String(t)]).unwrap();
    assert!(c.strings > 0, "the first rope and its parts are dead");
    assert!(matches!(h.str_len(s), Err(HeapError::Stale)));
    assert_eq!(h.to_string_lossy(t).unwrap(), want, "the parts of a rope that is not flat are alive");
    // `t` is flat now. A collect frees every node under it and keeps the one root rope and its units.
    let kept = h.collect(&[Value::String(t)]).unwrap();
    assert_eq!(kept.strings, 98 + 100, "{kept:?}"); // the 98 inner nodes and the 100 parts
    assert_eq!(h.to_string_lossy(t).unwrap(), want);
    assert_eq!(h.stats().live_strings, 2, "the rope `t` and the permanent `length`: {before:?}");
    let k = h.collect(&[]).unwrap();
    assert_eq!(k.strings, 1);
    assert_eq!(h.stats().bytes, h.used_bytes());
}

#[test]
fn bytes_return_to_the_start_after_a_collect() {
    let mut h = Heap::default();
    let start = h.stats().bytes;
    let mut s = h.intern_str("").unwrap();
    for _ in 0..1000 {
        let p = h.intern_str(&long('p', 40)).unwrap();
        s = h.concat(s, p).unwrap();
    }
    assert_eq!(h.str_len(s).unwrap(), 40_000);
    let mid = h.stats().bytes;
    assert!(mid > start + 40_000 * 2, "the flat units are counted: {mid}");
    h.collect(&[]).unwrap();
    assert_eq!(h.stats().bytes, start);
}

#[test]
fn a_deep_rope_does_not_use_the_native_stack() {
    let mut h = Heap::default();
    let part = h.intern_str("abcd").unwrap();
    let n = 1_000_000;
    // From the left: `s += x`.
    let mut s = h.intern_str("").unwrap();
    for _ in 0..n {
        s = h.concat(s, part).unwrap();
    }
    assert_eq!(h.str_len(s).unwrap(), 4 * n);
    assert_eq!(h.code_unit_at(s, 4 * n - 1).unwrap(), Some(b'd' as u16));
    // From the right: `s = x + s`.
    let mut t = h.intern_str("").unwrap();
    for _ in 0..n {
        t = h.concat(part, t).unwrap();
    }
    assert_eq!(h.code_unit_at(t, 0).unwrap(), Some(b'a' as u16));
    assert!(h.str_eq(s, t));
    // A balanced tree.
    let mut level = vec![part; 1 << 12];
    while level.len() > 1 {
        level = level.chunks(2).map(|p| h.concat(p[0], p[1]).unwrap()).collect();
    }
    assert_eq!(h.str_len(level[0]).unwrap(), 4 << 12);
    assert!(h.str_units(level[0]).unwrap().chunks(4).all(|c| c == [97, 98, 99, 100]));
    // A collect with a deep rope that is not flat marks it without recursion.
    let mut u = h.intern_str("").unwrap();
    for _ in 0..n {
        u = h.concat(u, part).unwrap();
    }
    h.collect(&[Value::String(u)]).unwrap();
    assert_eq!(h.str_len(u).unwrap(), 4 * n);
    assert_eq!(h.code_unit_at(u, 5).unwrap(), Some(b'b' as u16));
}

#[test]
fn the_limits_hold_for_ropes() {
    let mut h = Heap::new(HeapLimits { max_string_units: 100, ..HeapLimits::default() }).unwrap();
    let a = h.intern_str(&long('a', 60)).unwrap();
    assert!(matches!(h.concat(a, a), Err(HeapError::Limit(Limit::StringLength))));
    let mut h = Heap::new(HeapLimits { max_bytes: 200_000, ..HeapLimits::default() }).unwrap();
    let a = h.intern_str(&long('a', 40)).unwrap();
    let mut s = a;
    let mut hit = false;
    for _ in 0..100_000 {
        match h.concat(s, a) {
            Ok(n) => s = n,
            Err(HeapError::Limit(Limit::Bytes)) => {
                hit = true;
                break;
            }
            Err(e) => panic!("{e:?}"),
        }
    }
    assert!(hit, "nodes are charged");
}
