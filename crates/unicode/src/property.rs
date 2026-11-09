//! The sets of code points of RegExp `\p{...}`: General_Category, Script, Script_Extensions and the binary properties.
//!
//! The data is in `props.rs` (made by `tools/gen_unicode_props.py`). Names and ranges are sorted, so a lookup is a binary search.
//! The answer is a list of sorted `(first, last)` ranges, made when a regular expression is compiled, never when it runs.

use crate::props::*;

const MAX: u32 = 0x10FFFF;

/// The code points that have the property `name`, or `name=value`. `None` when the property or the value is not known
/// (the engine then throws a `SyntaxError`). The names are case-sensitive, as in ECMA-262.
///
/// - `property_ranges("Lu", None)`, `("General_Category", Some("Uppercase_Letter"))`, `("gc", Some("Lu"))`
/// - `("Script", Some("Greek"))`, `("sc", Some("Grek"))`, `("Script_Extensions", Some("Latin"))`, `("scx", Some("Latn"))`
/// - `("Alphabetic", None)`, `("Alpha", None)`, `("ASCII", None)`, `("Any", None)`, `("Assigned", None)`
pub fn property_ranges(name: &str, value: Option<&str>) -> Option<Vec<(u32, u32)>> {
    match value {
        Some(v) => match name {
            "General_Category" | "gc" => general_category(v),
            "Script" | "sc" => Some(script(index(SCRIPT_NAMES, v)?)),
            "Script_Extensions" | "scx" => Some(script_extensions(index(SCRIPT_NAMES, v)?)),
            _ => None,
        },
        None => match name {
            "Any" => Some(vec![(0, MAX)]),
            "ASCII" => Some(vec![(0, 0x7F)]),
            "Assigned" => general_category("Cn").map(|cn| complement(&cn)),
            _ => general_category(name).or_else(|| Some(BINARY_RANGES[index(BINARY_NAMES, name)? as usize].to_vec())),
        },
    }
}

/// The value of `name` in a table of sorted names.
fn index<T: Copy>(table: &[(&str, T)], name: &str) -> Option<T> {
    table.binary_search_by(|&(n, _)| n.cmp(name)).ok().map(|i| table[i].1)
}

fn general_category(name: &str) -> Option<Vec<(u32, u32)>> {
    let mask = index(GC_NAMES, name)?;
    let mut out: Vec<(u32, u32)> = Vec::new();
    for &(first, last, code) in GC {
        if mask >> code & 1 == 1 {
            push(&mut out, first, last);
        }
    }
    Some(out)
}

fn script(id: u8) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    for &(first, last, s) in SCRIPT {
        if s == id {
            push(&mut out, first, last);
        }
    }
    out
}

/// The code points with `id` in their Script_Extensions: the ones that list it, and the ones that list nothing and have it as Script.
fn script_extensions(id: u8) -> Vec<(u32, u32)> {
    let mut listed = Vec::new();
    let mut any = Vec::new();
    for &(first, last, set) in SCX {
        push(&mut any, first, last);
        if SCX_SETS[set as usize].contains(&id) {
            push(&mut listed, first, last);
        }
    }
    let mut out = intersect(&script(id), &complement(&any));
    out.extend(listed);
    out.sort_unstable();
    let mut merged: Vec<(u32, u32)> = Vec::new();
    for (a, b) in out {
        push(&mut merged, a, b);
    }
    merged
}

/// Add a range at the end of a sorted list, joined to the last one when they touch.
fn push(out: &mut Vec<(u32, u32)>, first: u32, last: u32) {
    match out.last_mut() {
        Some(l) if l.1 + 1 >= first => l.1 = l.1.max(last),
        _ => out.push((first, last)),
    }
}

/// The code points that are not in the sorted ranges `r`.
pub fn complement(r: &[(u32, u32)]) -> Vec<(u32, u32)> {
    let mut out = Vec::with_capacity(r.len() + 1);
    let mut next = 0u32;
    for &(a, b) in r {
        if a > next {
            out.push((next, a - 1));
        }
        next = b + 1;
    }
    if next <= MAX {
        out.push((next, MAX));
    }
    out
}

fn intersect(a: &[(u32, u32)], b: &[(u32, u32)]) -> Vec<(u32, u32)> {
    let (mut i, mut j, mut out) = (0, 0, Vec::new());
    while i < a.len() && j < b.len() {
        let (lo, hi) = (a[i].0.max(b[j].0), a[i].1.min(b[j].1));
        if lo <= hi {
            out.push((lo, hi));
        }
        if a[i].1 < b[j].1 {
            i += 1;
        } else {
            j += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn has(r: &[(u32, u32)], c: impl Into<u32>) -> bool {
        let c = c.into();
        r.binary_search_by(|&(a, b)| if b < c { std::cmp::Ordering::Less } else if a > c { std::cmp::Ordering::Greater } else { std::cmp::Ordering::Equal }).is_ok()
    }

    fn get(name: &str, value: Option<&str>) -> Vec<(u32, u32)> {
        property_ranges(name, value).unwrap_or_else(|| panic!("{name}={value:?}"))
    }

    #[test]
    fn general_categories_by_short_and_long_name() {
        for name in ["Lu", "Uppercase_Letter"] {
            let r = get(name, None);
            assert!(has(&r, 'A') && has(&r, 'Ω') && has(&r, 'Я') && !has(&r, 'a') && !has(&r, '1'), "{name}");
        }
        assert_eq!(get("Lu", None), get("gc", Some("Lu")));
        assert_eq!(get("Lu", None), get("General_Category", Some("Uppercase_Letter")));
        let l = get("L", None);
        assert_eq!(l, get("Letter", None));
        assert!(has(&l, 'a') && has(&l, 'A') && has(&l, '字') && has(&l, 'ǅ') && has(&l, 'ʰ') && !has(&l, '1') && !has(&l, ' '));
        let n = get("N", None);
        assert!(has(&n, '7') && has(&n, '٣') && has(&n, 'Ⅷ') && has(&n, '½') && !has(&n, 'a'));
        assert!(has(&get("Nd", None), '٣') && !has(&get("Nd", None), 'Ⅷ') && has(&get("Nl", None), 'Ⅷ') && has(&get("No", None), '½'));
        assert!(has(&get("P", None), '!') && has(&get("Punctuation", None), '。') && !has(&get("P", None), '+'));
        assert!(has(&get("S", None), '+') && has(&get("Symbol", None), '€') && has(&get("Sc", None), '$'));
        assert!(has(&get("Z", None), ' ') && has(&get("Zs", None), '\u{a0}') && has(&get("Separator", None), '\u{2028}'));
        assert!(has(&get("C", None), '\u{0}') && has(&get("Other", None), '\u{ad}') && has(&get("Cc", None), '\n') && has(&get("Cn", None), '\u{378}'));
        assert!(has(&get("LC", None), 'a') && has(&get("Cased_Letter", None), 'A') && !has(&get("LC", None), 'ʰ'));
        assert!(has(&get("Lm", None), 'ʰ') && has(&get("Lo", None), '字') && has(&get("Lt", None), 'ǅ') && has(&get("Ll", None), 'a'));
        assert!(has(&get("M", None), '\u{301}') && has(&get("Mark", None), '\u{301}') && has(&get("Cs", None), 0xd800u32) && has(&get("Co", None), '\u{e000}'));
        assert!(has(&get("Unassigned", None), '\u{378}'));
    }

    #[test]
    fn scripts() {
        for (name, ch) in [("Latin", 'a'), ("Latn", 'é'), ("Greek", 'α'), ("Cyrillic", 'ж'), ("Han", '字'), ("Hiragana", 'あ'), ("Katakana", 'カ'), ("Arabic", 'ع'), ("Hebrew", 'א'), ("Devanagari", 'अ'), ("Thai", 'ก'), ("Hangul", '한')] {
            let r = get("Script", Some(name));
            assert!(has(&r, ch), "{name}");
            assert!(!has(&r, '1'), "{name}");
            assert_eq!(r, get("sc", Some(name)));
        }
        assert_eq!(get("sc", Some("Grek")), get("sc", Some("Greek")));
        assert!(!has(&get("sc", Some("Latin")), 'α') && !has(&get("sc", Some("Greek")), 'a'));
        // Common letters like `1` and the space are in Common, not in a script.
        assert!(has(&get("sc", Some("Common")), '1') && has(&get("sc", Some("Zyyy")), ' '));
        assert!(has(&get("sc", Some("Inherited")), '\u{301}') && has(&get("sc", Some("Qaai")), '\u{301}'));
    }

    #[test]
    fn script_extensions_add_the_listed_scripts() {
        // U+30FC is Common in Script, and Hiragana and Katakana list it in Script_Extensions.
        assert!(!has(&get("sc", Some("Hiragana")), '\u{30fc}') && has(&get("sc", Some("Common")), '\u{30fc}'));
        assert!(has(&get("scx", Some("Hiragana")), '\u{30fc}') && has(&get("scx", Some("Katakana")), '\u{30fc}'));
        assert!(has(&get("Script_Extensions", Some("Hira")), '\u{30fc}') && !has(&get("scx", Some("Latin")), '\u{30fc}'));
        // A code point that lists nothing has its Script.
        assert!(has(&get("scx", Some("Latin")), 'a') && has(&get("scx", Some("Greek")), 'α'));
        // U+0640 lists Arabic, Mandaic...: it is not in the Script of Arabic.
        assert!(!has(&get("sc", Some("Arabic")), '\u{640}') && has(&get("scx", Some("Arabic")), '\u{640}') && !has(&get("scx", Some("Greek")), '\u{640}'));
    }

    #[test]
    fn binary_properties() {
        assert!(has(&get("Alphabetic", None), 'a') && has(&get("Alpha", None), '字') && !has(&get("Alphabetic", None), '1'));
        assert!(has(&get("White_Space", None), ' ') && has(&get("space", None), '\u{3000}') && has(&get("WSpace", None), '\n') && !has(&get("White_Space", None), 'a'));
        assert!(has(&get("Uppercase", None), 'A') && has(&get("Upper", None), 'Ω') && !has(&get("Uppercase", None), 'a'));
        assert!(has(&get("Lowercase", None), 'a') && has(&get("Lower", None), 'ß') && !has(&get("Lowercase", None), 'A'));
        assert_eq!(get("ASCII", None), vec![(0, 0x7f)]);
        assert_eq!(get("Any", None), vec![(0, 0x10ffff)]);
        let assigned = get("Assigned", None);
        assert!(has(&assigned, 'a') && !has(&assigned, '\u{378}') && !has(&assigned, '\u{10ffff}'));
        assert!(has(&get("Emoji", None), '😀') && has(&get("Emoji", None), '#') && !has(&get("Emoji", None), 'a'));
        assert!(has(&get("ID_Start", None), 'a') && has(&get("IDS", None), '字') && !has(&get("ID_Start", None), '1'));
        assert!(has(&get("ID_Continue", None), '1') && has(&get("IDC", None), '_') && !has(&get("ID_Continue", None), '-'));
        assert!(has(&get("ASCII_Hex_Digit", None), 'f') && has(&get("AHex", None), '0') && !has(&get("ASCII_Hex_Digit", None), 'g'));
        assert!(has(&get("Bidi_Mirrored", None), '(') && has(&get("Changes_When_NFKC_Casefolded", None), 'A'));
    }

    #[test]
    fn unknown_names_are_not_found() {
        for (name, value) in [
            ("Foo", None), ("lu", None), ("Letterr", None), ("Script", None), ("General_Category", None), ("Latin", None), ("Script", Some("Foo")), ("Script", Some("latin")),
            ("gc", Some("Foo")), ("Foo", Some("Latin")), ("Alphabetic", Some("Yes")), ("Script_Extensions", Some("Foo")), ("Other_Alphabetic", None), ("ASCII", Some("Y")),
            ("", None), ("sc", Some("")), ("InCB", None), ("Block", Some("Basic_Latin")),
        ] {
            assert_eq!(property_ranges(name, value), None, "{name}={value:?}");
        }
    }

    #[test]
    fn the_sets_are_sorted_and_do_not_touch() {
        for (n, v) in [("L", None), ("Assigned", None), ("sc", Some("Han")), ("scx", Some("Arab")), ("Alphabetic", None), ("Emoji", None), ("Cn", None), ("Any", None)] {
            let r = get(n, v);
            assert!(r.windows(2).all(|w| w[0].1 + 1 < w[1].0), "{n}");
            assert!(r.iter().all(|&(a, b)| a <= b && b <= MAX));
        }
    }

    #[test]
    fn every_code_point_has_exactly_one_general_category() {
        let mut total = 0u32;
        for c in ["Lu", "Ll", "Lt", "Lm", "Lo", "Mn", "Mc", "Me", "Nd", "Nl", "No", "Pc", "Pd", "Ps", "Pe", "Pi", "Pf", "Po", "Sm", "Sc", "Sk", "So", "Zs", "Zl", "Zp", "Cc", "Cf", "Cs", "Co", "Cn"] {
            total += get(c, None).iter().map(|&(a, b)| b - a + 1).sum::<u32>();
        }
        assert_eq!(total, MAX + 1);
    }
}
