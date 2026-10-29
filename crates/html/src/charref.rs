//! Character references: `&amp;`, `&#169;`, `&#xA9;`, `&copy;`.
//!
//! HTML uses "<" and "&" as special characters. To write them as plain text, a page uses a
//! reference: `&lt;` for "<", `&amp;` for "&". A reference can also write any letter by its
//! Unicode number: `&#169;` (decimal) or `&#xA9;` (hex) are both "©".
//!
//! Browsers accept broken references too, and the standard says exactly how. Two rules matter:
//! - A few old names work without the final ";". `&copy 2024` is "© 2024".
//!   Newer names need the ";". `&hearts` is not a reference, but `&hearts;` is "♥".
//! - Inside an attribute value, an old name without ";" is not decoded when the next character
//!   is "=" or a letter or digit. This protects URLs like `href="/a?x=1&copy=2"`.
//!   Without this rule, that link would turn into `?x=1©=2`.
//!
//! Comments compare Rust with JavaScript, because you know JS.

use crate::encoding::windows_1252;
use crate::entities::ENTITIES;

/// The longest name in the table, with its ";" is 33 characters. We never need more than that.
const MAX_NAME: usize = 32;

/// Read a character reference. `input` is the text right after the "&".
/// Gives the text that replaces it, and how many characters of `input` it used.
/// `None` means "this is not a reference": the "&" stays as it is.
/// `in_attribute` is true inside an attribute value, where the stricter rule applies.
pub fn decode(input: &[char], in_attribute: bool) -> Option<(String, usize)> {
    // `&[char]` is a view on a list of letters. We read it. We do not change it.
    // JS: like an array of single-letter strings.
    if input.first() == Some(&'#') {
        number(&input[1..]).map(|(c, used)| (c.to_string(), used + 1))
        // `+ 1` counts the "#".
    } else {
        named(input, in_attribute)
    }
}

/// `&#169;` or `&#xA9;`. `input` is the text after the "#".
fn number(input: &[char]) -> Option<(char, usize)> {
    let hex = matches!(input.first(), Some('x' | 'X'));
    // `matches!` is a short `match` that gives true or false.
    let radix = if hex { 16 } else { 10 };
    let start = usize::from(hex);
    // Hex has one more letter ("x") before the digits. JS: `hex ? 1 : 0`.
    let digits = input[start..].iter().take_while(|c| c.is_digit(radix)).count();
    // How many digits in a row. JS: loop while `isDigit(c)`.
    if digits == 0 {
        return None;
        // "&#;" and "&#x;" are not references.
    }
    let mut value: u32 = 0;
    for c in &input[start..start + digits] {
        value = value.saturating_mul(radix).saturating_add(c.to_digit(radix)?);
        // `saturating_*` stops at the biggest number instead of wrapping around.
        // A page can write a number with 100 digits. This keeps it from overflowing.
    }
    let mut used = start + digits;
    if input.get(used) == Some(&';') {
        used += 1;
        // The ";" is optional for numbers. We take it when it is there.
    }
    Some((fix_number(value), used))
}

/// Some numbers cannot be letters. The standard says what to show instead.
fn fix_number(value: u32) -> char {
    match value {
        0x80..=0x9F => windows_1252(value as u8),
        // These are control codes in Unicode. Old pages meant the windows-1252 letters
        // in this range, like `&#150;` for "–". Browsers follow what the pages meant.
        0 | 0xD800..=0xDFFF | 0x110000.. => '\u{FFFD}',
        // Zero, a surrogate (half of a pair, not a letter), or beyond the Unicode range: "�".
        _ => char::from_u32(value).unwrap_or('\u{FFFD}'),
    }
}

/// `&amp;` or `&copy`. `input` is the text after the "&".
fn named(input: &[char], in_attribute: bool) -> Option<(String, usize)> {
    let run = input.iter().take(MAX_NAME).take_while(|c| c.is_ascii_alphanumeric()).count();
    // The letters and digits in a row: "copy" in "copy 2024", or "amp" in "amp;".
    if run == 0 {
        return None;
    }
    let word: String = input[..run].iter().collect();
    // JS: `input.slice(0, run).join("")`.

    if input.get(run) == Some(&';') {
        if let Some(text) = lookup(&format!("{word};")) {
            return Some((text.to_string(), run + 1));
            // The full name with ";" is in the table. This is the normal case: `&amp;`.
        }
    }

    for len in (1..=run).rev() {
        // Try shorter and shorter starts of the word: "notit" gives "notit", "noti", "noti", "not".
        // `(1..=run).rev()` counts down. JS: `for (let len = run; len >= 1; len--)`.
        if let Some(text) = lookup(&word[..len]) {
            // Without a ";" only the old names are in the table, like "not" or "copy".
            if in_attribute && matches!(input.get(len), Some(c) if *c == '=' || c.is_ascii_alphanumeric()) {
                return None;
                // The attribute rule from the top of this file.
            }
            return Some((text.to_string(), len));
            // `&notit;` gives "¬" and leaves "it;" as plain text. Browsers do the same.
        }
    }
    None
}

/// Find a name in the generated table. The table is sorted, so we can binary search.
fn lookup(name: &str) -> Option<&'static str> {
    ENTITIES.binary_search_by_key(&name, |&(n, _)| n).ok().map(|i| ENTITIES[i].1)
    // Binary search looks at the middle, then throws away the half that cannot match.
    // For 2231 names it needs about 11 steps, not 2231.
}

#[cfg(test)]
// Only compiled for `cargo test`.
mod tests {
    use super::*;

    fn chars(text: &str) -> Vec<char> {
        text.chars().collect()
        // A helper: tests write text, the code reads a list of letters.
    }

    /// Decode the text after an "&". Gives the replacement and the number of characters used.
    fn data(text: &str) -> Option<(String, usize)> {
        decode(&chars(text), false)
    }

    fn attribute(text: &str) -> Option<(String, usize)> {
        decode(&chars(text), true)
    }

    fn some(text: &str, used: usize) -> Option<(String, usize)> {
        Some((text.to_string(), used))
    }

    #[test]
    fn table_is_complete_and_sorted() {
        assert_eq!(ENTITIES.len(), 2231);
        assert!(ENTITIES.windows(2).all(|w| w[0].0 < w[1].0));
        // Binary search only works on a sorted list.
    }

    #[test]
    fn reads_names_with_semicolon() {
        assert_eq!(data("amp; rest"), some("&", 4));
        assert_eq!(data("hearts;"), some("♥", 7));
        assert_eq!(data("NotEqualTilde;"), some("≂̸", 14));
        // This one is two letters: "≂" and a combining slash.
    }

    #[test]
    fn old_names_work_without_semicolon() {
        assert_eq!(data("copy 2024"), some("©", 4));
        assert_eq!(data("ampx"), some("&", 3));
        // "&ampx" is "&x". The reference is only "&amp".
    }

    #[test]
    fn new_names_need_the_semicolon() {
        assert_eq!(data("hearts"), None);
        assert_eq!(data("bogus;"), None);
    }

    #[test]
    fn uses_the_longest_old_name() {
        assert_eq!(data("notit;"), some("¬", 3));
        // "&notit;" is "¬it;". The "it;" stays as text.
        assert_eq!(data("notin;"), some("∉", 6));
        // "&notin;" is a full name, so it wins over "&not".
    }

    #[test]
    fn attributes_are_stricter() {
        assert_eq!(attribute("copy=2"), None);
        assert_eq!(attribute("copyx"), None);
        assert_eq!(attribute("copy 2"), some("©", 4));
        assert_eq!(attribute("copy;"), some("©", 5));
        // With the ";" there is no doubt, so it is decoded.
        assert_eq!(attribute("notit;"), None);
    }

    #[test]
    fn reads_numbers() {
        assert_eq!(data("#169;"), some("©", 5));
        assert_eq!(data("#xA9;"), some("©", 5));
        assert_eq!(data("#XA9"), some("©", 4));
        // The ";" is optional.
        assert_eq!(data("#65x"), some("A", 3));
    }

    #[test]
    fn fixes_odd_numbers() {
        assert_eq!(data("#0;"), some("\u{FFFD}", 3));
        assert_eq!(data("#xD800;"), some("\u{FFFD}", 7));
        assert_eq!(data("#1114112;"), some("\u{FFFD}", 9));
        assert_eq!(data("#99999999999999999999;"), some("\u{FFFD}", 22));
        // A number with too many digits must not crash the program.
        assert_eq!(data("#150;"), some("–", 5));
        assert_eq!(data("#x80;"), some("€", 5));
        // The old pages meant the windows-1252 letters.
    }

    #[test]
    fn not_a_reference() {
        assert_eq!(data(""), None);
        assert_eq!(data(" amp;"), None);
        assert_eq!(data("#;"), None);
        assert_eq!(data("#x;"), None);
        assert_eq!(data("#xyz"), None);
    }
}
