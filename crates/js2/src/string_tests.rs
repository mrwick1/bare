// Tests of `string.rs`. Miri runs them in CI, so the counts shrink under it.
use super::*;

fn heap() -> Heap {
    Heap::default()
}

fn s(h: &mut Heap, t: &str) -> Str {
    Str::from_str(h, t).unwrap()
}

/// The units of a string that is flat.
fn units(x: Str) -> Vec<u16> {
    x.chars().expect("flat").units().collect()
}

fn text(h: &mut Heap, x: Str) -> String {
    let f = x.flatten(h).unwrap();
    String::from_utf16(&units(f)).unwrap()
}

const N: usize = if cfg!(miri) { 300 } else { 100_000 };

#[test]
fn a_flat_string_has_the_narrowest_form() {
    let mut h = heap();
    let a = s(&mut h, "abc");
    let b = s(&mut h, "caf\u{e9}");
    let c = s(&mut h, "a\u{20ac}");
    assert!(!a.is_two_byte() && !b.is_two_byte() && c.is_two_byte());
    assert!(matches!(a.chars(), Some(Chars::One(_))) && matches!(c.chars(), Some(Chars::Two(_))));
    // Units that are all below 256 become one byte even when they come as 16-bit units.
    let d = Str::new(&mut h, Chars::Two(&[0x61, 0xFF])).unwrap();
    assert!(!d.is_two_byte());
    assert_eq!(units(d), [0x61, 0xFF]);
    assert_eq!((a.len(), b.len(), c.len()), (3, 4, 2));
    assert_eq!(c.unit_at(1), Some(0x20AC));
    assert_eq!(a.unit_at(3), None);
}

#[test]
fn a_string_cell_is_the_smallest_size_class_that_holds_it() {
    // Header and prefix are 16 bytes: a string of up to 16 bytes of units takes 32, and the empty string takes 16.
    assert_eq!(CellHeader::SIZE, 8);
    assert_eq!(DATA + 16, 32);
    let mut h = heap();
    let before = h.mapped_bytes();
    let all: Vec<Str> = (0..100).map(|_| Str::from_str(&mut h, "").unwrap()).collect();
    assert_eq!(all.iter().map(|x| x.cell()).collect::<std::collections::HashSet<_>>().len(), 100);
    assert!(h.mapped_bytes() - before <= 2 * crate::heap::BLOCK);
}

#[test]
fn a_short_concatenation_is_copied_and_flat() {
    let mut h = heap();
    let (a, b) = (s(&mut h, "ab"), s(&mut h, "cd"));
    let c = Str::concat(&mut h, a, b).unwrap();
    assert!(!c.is_rope());
    assert_eq!(units(c), "abcd".encode_utf16().collect::<Vec<_>>());
    // One part wide: the result is wide, and the one-byte part is widened.
    let w = s(&mut h, "\u{20ac}");
    let d = Str::concat(&mut h, a, w).unwrap();
    let e = Str::concat(&mut h, w, a).unwrap();
    assert!(d.is_two_byte() && e.is_two_byte() && !d.is_rope());
    assert_eq!((text(&mut h, d), text(&mut h, e)), ("ab\u{20ac}".into(), "\u{20ac}ab".into()));
}

#[test]
fn an_empty_part_gives_the_other_string_itself() {
    let mut h = heap();
    let (e, a) = (s(&mut h, ""), s(&mut h, "x"));
    assert_eq!(Str::concat(&mut h, e, a).unwrap(), a);
    assert_eq!(Str::concat(&mut h, a, e).unwrap(), a);
}

#[test]
fn a_long_concatenation_is_a_rope_that_flattens_once() {
    let mut h = heap();
    let a = Str::from_str(&mut h, &"a".repeat(ROPE_MIN - 1)).unwrap();
    let b = s(&mut h, "b");
    let r = Str::concat(&mut h, a, b).unwrap();
    assert!(r.is_rope() && r.len() == ROPE_MIN && r.chars().is_none() && r.unit_at(0).is_none());
    assert_eq!(r.children(), [Some(a), Some(b)]);
    let f = r.flatten(&mut h).unwrap();
    assert!(!f.is_rope() && f != r);
    assert_eq!(text(&mut h, f), format!("{}b", "a".repeat(ROPE_MIN - 1)));
    // The rope now points to the copy, drops its parts, and reads through it.
    assert_eq!(r.children(), [Some(f), None]);
    assert_eq!(r.resolve(), f);
    assert_eq!(r.chars().unwrap().len(), ROPE_MIN);
    assert_eq!(r.unit_at(ROPE_MIN - 1), Some(b'b' as u16));
    assert_eq!(r.flatten(&mut h).unwrap(), f);
    // A concatenation with the flattened rope holds the copy, not the rope.
    let c = Str::concat(&mut h, r, b).unwrap();
    assert_eq!(c.children(), [Some(f), Some(b)]);
}

#[test]
fn a_rope_that_grows_at_the_end_or_at_the_start_flattens_in_order() {
    let mut h = heap();
    let x = s(&mut h, "ab");
    let (mut app, mut pre) = (s(&mut h, ""), s(&mut h, ""));
    let parts: Vec<&str> = (0..N).map(|i| if i % 7 == 0 { "\u{e9}" } else { "ab" }).collect();
    let (want_app, want_pre): (String, String) = (parts.concat(), parts.iter().rev().copied().collect());
    for &t in &parts {
        let y = if t == "ab" { x } else { s(&mut h, t) };
        app = Str::concat(&mut h, app, y).unwrap();
        pre = Str::concat(&mut h, y, pre).unwrap();
    }
    assert!(app.is_rope() && pre.is_rope() && app.len() == want_app.encode_utf16().count());
    let fa = app.flatten(&mut h).unwrap();
    assert_eq!(String::from_utf16(&units(fa)).unwrap(), want_app);
    let fp = pre.flatten(&mut h).unwrap();
    assert_eq!(String::from_utf16(&units(fp)).unwrap(), want_pre);
}

#[test]
fn a_rope_of_ropes_flattens_with_the_right_widths() {
    let mut h = heap();
    // A balanced tree, a shared sub-rope used twice, and one wide leaf.
    let wide = Str::from_str(&mut h, &format!("{}\u{20ac}", "w".repeat(40))).unwrap();
    let narrow = Str::from_str(&mut h, &"n".repeat(40)).unwrap();
    let mut x = Str::concat(&mut h, narrow, wide).unwrap();
    for _ in 0..8 {
        x = Str::concat(&mut h, x, x).unwrap();
    }
    assert!(x.is_two_byte() && x.len() == 81 * 256);
    let want = format!("{}{}\u{20ac}", "n".repeat(40), "w".repeat(40)).repeat(256);
    assert_eq!(text(&mut h, x), want);
    // A rope of narrow parts only flattens to one byte.
    let mut y = narrow;
    for _ in 0..4 {
        y = Str::concat(&mut h, y, narrow).unwrap();
    }
    let f = y.flatten(&mut h).unwrap();
    assert!(!f.is_two_byte() && f.len() == 200);
}

#[test]
fn a_string_that_is_too_long_is_refused() {
    let mut h = heap();
    let mut x = Str::from_str(&mut h, &"a".repeat(ROPE_MIN)).unwrap();
    // Doubling makes one new node each time and does not copy.
    let mut doublings = 0;
    while let Ok(y) = Str::concat(&mut h, x, x) {
        x = y;
        doublings += 1;
    }
    assert!(x.len() <= MAX_LEN && x.len() * 2 > MAX_LEN, "{doublings}");
    assert_eq!(Str::concat(&mut h, x, x), Err(Error::TooLong));
    assert_eq!(Str::new(&mut h, Chars::One(&[])).map(|e| e.len()), Ok(0));
}

#[test]
fn the_hash_depends_on_the_units_and_not_on_the_form_or_the_rope() {
    let mut h = heap();
    let a = s(&mut h, "hello");
    assert_eq!(a.cached_hash(), 0);
    let ha = a.hash(&mut h).unwrap();
    assert_ne!(ha, 0);
    assert_eq!(a.cached_hash(), ha);
    assert_eq!(Chars::Two(&[104, 101, 108, 108, 111]).hash(), ha);
    assert_eq!(Chars::One(b"hello").hash(), ha);
    assert_ne!(Chars::One(b"hellp").hash(), ha);
    let (p, q) = (s(&mut h, &"x".repeat(ROPE_MIN)), s(&mut h, "y"));
    let r = Str::concat(&mut h, p, q).unwrap();
    assert_eq!(r.cached_hash(), 0);
    let flat = format!("{}y", "x".repeat(ROPE_MIN));
    assert_eq!(r.hash(&mut h).unwrap(), Chars::One(flat.as_bytes()).hash());
    // The hash of a rope is the hash of its copy.
    assert_eq!(r.resolve().cached_hash(), r.hash(&mut h).unwrap());
}

#[test]
fn equality_compares_units_across_ropes_and_forms() {
    let mut h = heap();
    let (p, q) = (s(&mut h, &"x".repeat(ROPE_MIN)), s(&mut h, "y"));
    let r = Str::concat(&mut h, p, q).unwrap();
    let flat = Str::from_str(&mut h, &format!("{}y", "x".repeat(ROPE_MIN))).unwrap();
    let other = Str::from_str(&mut h, &format!("{}z", "x".repeat(ROPE_MIN))).unwrap();
    assert!(Str::equals(&mut h, r, flat).unwrap());
    assert!(!Str::equals(&mut h, r, other).unwrap());
    assert!(Str::equals(&mut h, r, r).unwrap());
    let (w1, w2) = (s(&mut h, "\u{20ac}"), s(&mut h, "\u{20ad}"));
    let a = s(&mut h, "a");
    assert!(!Str::equals(&mut h, w1, a).unwrap() && !Str::equals(&mut h, w1, w2).unwrap());
    let w3 = s(&mut h, "\u{20ac}");
    assert!(Str::equals(&mut h, w1, w3).unwrap());
}

#[test]
fn a_string_above_the_small_limit_is_one_inline_cell() {
    let mut h = heap();
    let big = Str::from_str(&mut h, &"q".repeat(20_000)).unwrap();
    assert_eq!((big.len(), big.unit_at(19_999), big.is_rope()), (20_000, Some(b'q' as u16), false));
    let wide = Str::new(&mut h, Chars::Two(&vec![0x20AC; 10_000])).unwrap();
    assert_eq!((wide.len(), wide.unit_at(9_999)), (10_000, Some(0x20AC)));
    let r = Str::concat(&mut h, big, wide).unwrap();
    let f = r.flatten(&mut h).unwrap();
    assert_eq!((f.len(), f.unit_at(0), f.unit_at(29_999)), (30_000, Some(b'q' as u16), Some(0x20AC)));
}

#[test]
fn a_value_holds_the_cell_and_gives_the_string_back() {
    let mut h = heap();
    let a = s(&mut h, "v");
    let v = a.value();
    assert_eq!(v.as_pointer(), Some((Kind::String, a.cell())));
    // SAFETY: `a` is live.
    assert_eq!(unsafe { Str::from_value(v) }, Some(a));
    // SAFETY: not a pointer.
    assert_eq!(unsafe { Str::from_value(Value::int(1)) }, None);
}

#[test]
fn a_collection_keeps_a_string_that_the_roots_reach_through_its_children() {
    let mut h = heap();
    let (a, b) = (Str::from_str(&mut h, &"a".repeat(40)).unwrap(), s(&mut h, "b"));
    let dead = s(&mut h, "dead");
    let r = Str::concat(&mut h, a, b).unwrap();
    h.start_mark();
    let mut work = vec![r];
    while let Some(x) = work.pop() {
        // SAFETY: a live cell of `h`.
        if unsafe { h.mark(x.cell()) } {
            work.extend(x.children().into_iter().flatten());
        }
    }
    // SAFETY: a cell of `h`.
    assert!(unsafe { h.is_marked(a.cell()) && h.is_marked(b.cell()) && !h.is_marked(dead.cell()) });
    h.finish_mark();
    assert_eq!(text(&mut h, r), format!("{}b", "a".repeat(40)));
}
