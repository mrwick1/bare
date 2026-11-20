use super::*;

fn code(n: u16) -> Code {
    let mut c = Code::new(n, 0);
    c.words = vec![n as u32; 3];
    c
}

fn regs(codes: &Codes) -> Vec<u16> {
    codes.iter().map(|c| c.nregs).collect()
}

#[test]
fn a_reference_survives_push_set_and_a_table_that_grows() {
    let codes = Codes::from(vec![code(1), code(2)]);
    let (a, b) = (&codes[0], &codes[1]);
    // The table of pointers reallocates many times, and the chunks are many; the codes stay where they are.
    for i in 0..300u16 {
        assert_eq!(codes.push_all(vec![code(10 + i)]), 2 + i as usize);
    }
    // A slot is replaced while `a` is read: `a` still shows the old code, the table shows the new one.
    codes.set(0, code(99));
    assert_eq!((a.nregs, a.words.clone()), (1, vec![1; 3]));
    assert_eq!((codes[0].nregs, b.nregs, codes[301].nregs), (99, 2, 309));
    assert_eq!(codes.len(), 302);
    assert!(codes.has_retired());
    assert_eq!(codes.iter().map(|c| c.nregs as u32).sum::<u32>(), 99 + 2 + (0..300).map(|i| 10 + i).sum::<u32>());
}

#[test]
fn a_chunk_is_adopted_without_a_copy_and_its_room_serves_set() {
    let mut v = Vec::with_capacity(10);
    v.extend([code(1), code(2), code(3)]);
    let at = v.as_ptr();
    let codes = Codes::from(v);
    assert!(std::ptr::eq(&codes[0], at), "the buffer is the chunk");
    // Three `set` calls take the room of the chunk (7 slots), the next ones open a new chunk of `ROOM` slots.
    for i in 0..30u16 {
        codes.set(i as usize % 3, code(100 + i));
    }
    assert_eq!(regs(&codes), [127, 128, 129]);
    let after = codes.push_all(vec![code(4), code(5)]);
    assert_eq!((after, regs(&codes)[3..].to_vec()), (3, vec![4, 5]));
}

#[test]
fn retired_slots_are_dropped_and_reused_after_free_retired() {
    let mut codes = Codes::from(vec![code(1)]);
    assert!(!codes.has_retired());
    for i in 0..5 {
        codes.set(0, code(2 + i));
    }
    assert_eq!(codes[0].nregs, 6);
    assert!(codes.has_retired());
    let before = codes.slots.get_mut().chunks.iter().map(|k| k.used).sum::<usize>();
    codes.free_retired();
    assert!(!codes.has_retired());
    // The five slots are free now: five more `set` calls take no new room.
    for i in 0..5 {
        codes.set(0, code(20 + i));
    }
    let after = codes.slots.get_mut().chunks.iter().map(|k| k.used).sum::<usize>();
    assert_eq!((before, after, codes[0].nregs), (after, before, 24));
    // The slots left (retired ones with their old code, free ones with an empty code) are dropped with the chunks; Miri checks it.
}

#[test]
fn an_empty_table_takes_codes() {
    let codes = Codes::default();
    assert!(codes.is_empty() && codes.get(0).is_none());
    assert_eq!(codes.push_all(Vec::new()), 0);
    assert_eq!(codes.push_all(vec![code(7), code(8)]), 0);
    codes.set(1, code(9));
    assert_eq!((regs(&codes), codes.get(2).map(|c| c.nregs)), (vec![7, 9], None));
}

#[test]
#[should_panic(expected = "no code at this index")]
fn set_out_of_range_panics() {
    Codes::default().set(0, code(1));
}
