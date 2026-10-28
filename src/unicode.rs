//! Unicode helpers: table lookups and NFC normalization.
//!
//! The same text can be stored in more than one way. "é" can be one code point (U+00E9),
//! or "e" plus a combining accent (U+0065 U+0301). Both look the same on screen.
//! NFC ("Normalization Form C") picks one form: the composed one, where possible.
//! After NFC, two strings that look the same also compare equal.
//! JS has this built in: `str.normalize("NFC")`. We write it by hand (UAX #15).

use crate::unicode_tables::{COMBINING_CLASS, COMPOSE, DECOMPOSE};
use std::cmp::Ordering;

/// Find the value for `cp` in a sorted table of `(first, last, value)` ranges.
pub fn find<T: Copy>(table: &[(u32, u32, T)], cp: u32) -> Option<T> {
    // `<T: Copy>` makes the function generic: it works for any value type that can be copied.
    // TypeScript: `function find<T>(table: [number, number, T][], cp: number): T | undefined`.
    table
        .binary_search_by(|&(first, last, _)| {
            // Binary search: look at the middle range, then throw away the half that cannot match.
            // This needs about 14 steps for 10 000 ranges. A loop over all of them needs 10 000.
            if last < cp {
                Ordering::Less // this range is before `cp`
            } else if first > cp {
                Ordering::Greater // this range is after `cp`
            } else {
                Ordering::Equal // `cp` is inside this range
            }
        })
        .ok()
        // `Ok(index)` if found. `.ok()` turns it into `Some(index)` or `None`.
        .map(|i| table[i].2)
    // `.2` is the third item of the tuple. JS: `table[i][2]`.
}

/// The canonical combining class of `c`. 0 means a "starter", like a base letter.
/// Accents have other numbers. They decide the order of accents on one letter.
pub fn combining_class(c: char) -> u8 {
    find(COMBINING_CLASS, c as u32).unwrap_or(0)
}

// Korean Hangul syllables are composed by a formula, not by a table (Unicode, section 3.12).
// Each syllable is a leading consonant (L), a vowel (V) and an optional trailing consonant (T).
const S_BASE: u32 = 0xAC00; // first syllable
const L_BASE: u32 = 0x1100;
const V_BASE: u32 = 0x1161;
const T_BASE: u32 = 0x11A7; // one below the first T, because "no T" counts as 0
const L_COUNT: u32 = 19;
const V_COUNT: u32 = 21;
const T_COUNT: u32 = 28;
const S_COUNT: u32 = L_COUNT * V_COUNT * T_COUNT;
// `const` is a value fixed at compile time. JS: a top-level `const` that holds a literal.

/// Normalize `text` to NFC.
pub fn nfc(text: &str) -> String {
    // Step 1: decompose. Split every character into its base and accents: "é" becomes "e" + "´".
    let mut chars: Vec<char> = Vec::new();
    for c in text.chars() {
        decompose(c, &mut chars);
    }

    // Step 2: sort the accents after each base by combining class.
    // "a" + dot below (220) + hat (230) and "a" + hat + dot below then become the same.
    // This is an insertion sort. It never moves a starter (class 0), and it keeps equal classes in order.
    for i in 1..chars.len() {
        // JS: `for (let i = 1; i < chars.length; i++)`.
        let mut j = i;
        while j > 0 && combining_class(chars[j]) != 0 && combining_class(chars[j - 1]) > combining_class(chars[j]) {
            chars.swap(j - 1, j);
            j -= 1;
        }
    }

    // Step 3: compose. Join a base with a following accent when a composed character exists.
    let mut out: Vec<char> = Vec::new();
    let mut starter: Option<usize> = None;
    // Position in `out` of the last base character. `None` until we see one.
    for c in chars {
        let class = combining_class(c);
        if let Some(s) = starter {
            let last = out.len() - 1;
            let blocked = last != s && combining_class(out[last]) >= class;
            // An accent cannot join the base if another accent of the same or higher class
            // stands between them. ("Blocked" in UAX #15.)
            if !blocked {
                if let Some(both) = compose(out[s], c) {
                    out[s] = both;
                    // Replace the base with the composed character, and drop `c`.
                    continue;
                }
            }
        }
        if class == 0 {
            starter = Some(out.len());
        }
        out.push(c);
    }
    out.into_iter().collect()
    // Turn the `Vec<char>` back into a `String`. JS: `chars.join("")`.
}

/// Add the full canonical decomposition of `c` to `out`.
fn decompose(c: char, out: &mut Vec<char>) {
    let cp = c as u32;
    if (S_BASE..S_BASE + S_COUNT).contains(&cp) {
        // A Hangul syllable: compute its parts.
        let s = cp - S_BASE;
        let parts = [L_BASE + s / (V_COUNT * T_COUNT), V_BASE + (s % (V_COUNT * T_COUNT)) / T_COUNT, T_BASE + s % T_COUNT];
        for (i, part) in parts.into_iter().enumerate() {
            if i < 2 || part != T_BASE {
                // The T part is optional. `T_BASE` itself means "no T".
                out.extend(char::from_u32(part));
                // `char::from_u32` gives an `Option`. `extend` adds it if it is `Some`.
            }
        }
    } else if let Ok(i) = DECOMPOSE.binary_search_by_key(&cp, |&(key, _)| key) {
        out.extend(DECOMPOSE[i].1.chars());
    } else {
        out.push(c);
        // No decomposition: keep the character.
    }
}

/// The composed character for `a` followed by `b`, if one exists.
fn compose(a: char, b: char) -> Option<char> {
    let (a, b) = (a as u32, b as u32);
    if (L_BASE..L_BASE + L_COUNT).contains(&a) && (V_BASE..V_BASE + V_COUNT).contains(&b) {
        // Hangul L + V gives an LV syllable.
        return char::from_u32(S_BASE + ((a - L_BASE) * V_COUNT + (b - V_BASE)) * T_COUNT);
    }
    if (S_BASE..S_BASE + S_COUNT).contains(&a) && (a - S_BASE) % T_COUNT == 0 && (T_BASE + 1..T_BASE + T_COUNT).contains(&b) {
        // Hangul LV + T gives an LVT syllable.
        return char::from_u32(a + (b - T_BASE));
    }
    let i = COMPOSE.binary_search_by_key(&(a, b), |&(first, second, _)| (first, second)).ok()?;
    // `?` on an `Option`: if it is `None`, return `None` now.
    char::from_u32(COMPOSE[i].2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composes_accents() {
        assert_eq!(nfc("e\u{301}"), "é");
        // "e" + combining acute accent becomes one character.
        assert_eq!(nfc("é"), "é");
        // Already NFC: no change.
    }

    #[test]
    fn orders_accents() {
        assert_eq!(nfc("\u{1E0B}\u{323}"), "\u{1E0D}\u{307}");
        // "ḋ" + dot below becomes "ḍ" + dot above. The dot below sorts first, so it joins
        // the "d". This example is from UAX #15.
    }

    #[test]
    fn composes_hangul() {
        assert_eq!(nfc("\u{1100}\u{1161}\u{11A8}"), "각");
        // L + V + T jamo become one syllable.
        assert_eq!(nfc("각"), "각");
    }

    #[test]
    fn keeps_excluded_characters_apart() {
        assert_eq!(nfc("\u{958}"), "\u{915}\u{93C}");
        // DEVANAGARI LETTER QA is on Unicode's composition exclusion list.
        // NFC decomposes it and never composes it again.
    }
}
