use super::{CANON_MOVES, canon, canon_class};

/// `{canon(c) : c in ranges}` one code unit at a time.
fn brute(ranges: &[(u32, u32)]) -> Vec<(u32, u32)> {
    let mut bits = vec![false; 65536];
    for &(lo, hi) in ranges {
        for c in lo..=hi {
            bits[canon(c as u16) as usize] = true;
        }
    }
    let mut out: Vec<(u32, u32)> = Vec::new();
    for (c, _) in bits.iter().enumerate().filter(|b| *b.1) {
        match out.last_mut() {
            Some(r) if r.1 + 1 == c as u32 => r.1 = c as u32,
            _ => out.push((c as u32, c as u32)),
        }
    }
    out
}

#[test]
fn the_fold_of_a_class_is_the_canonical_form_of_each_member() {
    let cases: &[&[(u32, u32)]] = &[
        &[(0x61, 0x7A)],
        &[(0x30, 0x39), (0x41, 0x5A), (0x5F, 0x5F), (0x61, 0x7A)],
        &[(0, 0xFFFF)],
        &[(0, 0x40), (0x5B, 0xFFFF)],
        &[(0xC0, 0x24F)],
        &[(0x3B1, 0x3C9), (0x430, 0x44F)],
        &[(0xDF, 0xDF), (0x17F, 0x17F), (0x212A, 0x212A), (0x1E9E, 0x1E9E)],
        &[(0x3F, 0x40), (0x7F, 0x80), (0xFFC0, 0xFFFF)],
        &[],
    ];
    for &r in cases {
        assert_eq!(canon_class(r), brute(r), "{r:x?}");
    }
    // Ranges that start and end at each bit of a word.
    for lo in (0u32..200).step_by(7) {
        let r = [(lo, lo + 63), (lo + 100, lo + 101)];
        assert_eq!(canon_class(&r), brute(&r), "{r:x?}");
    }
}

#[test]
fn the_canon_table_matches_canon() {
    // After a Rust update with new Unicode data, generate `regexp_canon_table.rs` again from this list.
    let want: Vec<(u16, u16)> = (0..=0xFFFFu16).filter_map(|c| Some((c, canon(c))).filter(|m| m.1 != c)).collect();
    assert_eq!(CANON_MOVES, &want[..]);
}
