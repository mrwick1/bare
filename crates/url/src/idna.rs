//! Internationalized domain names: turn a host like "Bücher.de" into "xn--bcher-kva.de".
//!
//! DNS only accepts ASCII. Browsers follow the WHATWG URL Standard, which runs Unicode's
//! UTS #46 algorithm on the host:
//!
//! 1. Map: lowercase, and replace variant forms. Fullwidth "Ｂ" becomes "b".
//! 2. Normalize to NFC (see `unicode.rs`).
//! 3. Split into labels at ".". A label is one part: "bücher" and "de".
//! 4. Check each label: allowed characters, joiner rules, right-to-left rules.
//! 5. Write each non-ASCII label in Punycode with an "xn--" prefix.

use bare_unicode::tables::{BIDI, Bidi, IDNA, Idna, JOINING, Joining, MARKS};
use bare_unicode::{combining_class, find, nfc};
// From the `bare-unicode` crate. In code, "-" in a crate name becomes "_".

/// The WHATWG "domain parser" (not strict): give an ASCII host, or an error.
pub fn domain_to_ascii(domain: &str) -> Result<String, String> {
    let ascii = if domain.is_ascii() {
        domain.to_ascii_lowercase()
        // An ASCII host is only lowercased, even if UTS #46 would reject it.
        // Browsers do this so that old sites keep working.
    } else {
        to_ascii(domain).ok_or_else(|| format!("invalid domain: {domain}"))?
    };
    if ascii.is_empty() {
        return Err("empty host".to_string());
    }
    if let Some(c) = ascii.chars().find(|&c| c <= '\u{1f}' || c == '\u{7f}' || " #%/:<>?@[\\]^|".contains(c)) {
        // The "forbidden domain code points": control characters, and characters that
        // have a meaning in URLs. `{c:?}` prints the character with quotes and escapes.
        return Err(format!("forbidden character in host: {c:?}"));
    }
    Ok(ascii)
}

/// UTS #46 ToASCII, with the settings that browsers use:
/// CheckHyphens, UseSTD3ASCIIRules and VerifyDnsLength off; CheckBidi and CheckJoiners on;
/// nontransitional. Gives `None` on any error.
fn to_ascii(domain: &str) -> Option<String> {
    // Step 1: map each character with the IDNA table.
    let mut mapped = String::new();
    for c in domain.chars() {
        match find(IDNA, c as u32) {
            Some((Idna::Mapped, to)) => mapped.push_str(to),
            Some((Idna::Ignored, _)) => {}
            // Ignored characters, like the soft hyphen, are removed.
            _ => mapped.push(c),
            // Valid and deviation characters stay. Disallowed ones also stay for now.
            // Step 4 finds them and gives an error.
        }
    }

    // Steps 2 and 3: normalize, then split into labels.
    let mut labels: Vec<String> = Vec::new();
    for label in nfc(&mapped).split('.') {
        // Step 4: convert and check each label.
        if let Some(code) = label.strip_prefix("xn--") {
            // Already Punycode. Decode it, so we can check what it really contains.
            if !label.is_ascii() {
                return None;
            }
            let decoded = punycode_decode(code)?;
            if decoded.is_empty() || decoded.is_ascii() || !valid_label(&decoded, true) {
                return None;
                // "xn--" must hide at least one non-ASCII character. Otherwise it is a trick.
            }
            labels.push(decoded);
        } else {
            if !valid_label(label, false) {
                return None;
            }
            labels.push(label.to_string());
        }
    }

    // The right-to-left rules apply only to a domain with right-to-left text in it.
    let rtl_domain = labels.iter().any(|l| l.chars().any(|c| matches!(bidi(c), Bidi::R | Bidi::AL | Bidi::AN)));
    if rtl_domain && !labels.iter().all(|l| l.is_empty() || bidi_ok(l)) {
        return None;
    }

    // Step 5: encode the non-ASCII labels.
    let mut out: Vec<String> = Vec::new();
    for label in labels {
        if label.is_ascii() {
            out.push(label);
        } else {
            out.push(format!("xn--{}", punycode_encode(&label)?));
        }
    }
    Some(out.join("."))
}

/// The UTS #46 validity criteria (section 4.1) for one label, with the browser settings.
/// `decoded` is true when the label came from Punycode, so it was not mapped or normalized.
fn valid_label(label: &str, decoded: bool) -> bool {
    let chars: Vec<char> = label.chars().collect();
    let Some(&first) = chars.first() else {
        return true;
        // `let ... else` runs the `else` block if the pattern does not fit.
        // Here: an empty label has nothing to check.
    };
    if decoded && nfc(label) != label {
        return false; // must be NFC
    }
    if label.starts_with("xn--") || label.contains('.') {
        return false; // no "xn--" inside "xn--", and no dot inside a label
    }
    if find(MARKS, first as u32).is_some() {
        return false; // must not start with an accent or other combining mark
    }
    if !chars.iter().all(|&c| matches!(find(IDNA, c as u32), Some((Idna::Valid | Idna::Deviation, _)))) {
        return false; // only valid characters (deviation ones count as valid here)
    }
    joiners_ok(&chars)
}

/// The ContextJ rules (RFC 5892, appendix A) for the invisible joiners U+200C and U+200D.
/// They are allowed only where they change how the text is drawn, so they cannot hide
/// inside a domain name that looks like another one.
fn joiners_ok(chars: &[char]) -> bool {
    // `&[char]` is a borrowed slice: a view into a `Vec` or array. JS: a read-only array.
    for (i, &c) in chars.iter().enumerate() {
        if c != '\u{200C}' && c != '\u{200D}' {
            continue;
        }
        if i > 0 && combining_class(chars[i - 1]) == 9 {
            continue; // after a virama (class 9), for example in Hindi, both joiners are fine
        }
        if c == '\u{200D}' {
            return false; // the zero width joiner needs a virama before it
        }
        // The zero width non-joiner must stand between two letters that would join:
        // (L or D) T* ZWNJ T* (R or D), where T letters are skipped. This is Arabic-style joining.
        let before = chars[..i].iter().rev().map(|&c| joining(c)).find(|&j| j != Some(Joining::T));
        let after = chars[i + 1..].iter().map(|&c| joining(c)).find(|&j| j != Some(Joining::T));
        if !matches!(before, Some(Some(Joining::L | Joining::D))) || !matches!(after, Some(Some(Joining::R | Joining::D))) {
            return false;
        }
    }
    true
}

/// The six bidi rules (RFC 5893, section 2) for one non-empty label.
/// They stop a mix of left-to-right and right-to-left text that could display in a misleading order.
fn bidi_ok(label: &str) -> bool {
    use Bidi::*;
    // Lets us write `R` instead of `Bidi::R` inside this function.
    let classes: Vec<Bidi> = label.chars().map(bidi).collect();
    let last = classes.iter().copied().rev().find(|&b| b != NSM);
    // The last class that is not a combining mark (NSM).
    match classes[0] {
        R | AL => {
            // A right-to-left label (rules 2, 3 and 4).
            classes.iter().all(|b| matches!(b, R | AL | AN | EN | ES | CS | ET | ON | BN | NSM))
                && matches!(last, Some(R | AL | EN | AN))
                && !(classes.contains(&EN) && classes.contains(&AN))
            // European digits (EN) and Arabic digits (AN) must not mix.
        }
        L => {
            // A left-to-right label (rules 5 and 6).
            classes.iter().all(|b| matches!(b, L | EN | ES | CS | ET | ON | BN | NSM)) && matches!(last, Some(L | EN))
        }
        _ => false, // rule 1: the first character must be L, R or AL
    }
}

fn bidi(c: char) -> Bidi {
    find(BIDI, c as u32).unwrap_or(Bidi::L)
}

fn joining(c: char) -> Option<Joining> {
    find(JOINING, c as u32)
    // `None` means "Non_Joining".
}

// Punycode (RFC 3492): write any Unicode label with only a-z, 0-9 and "-".
//
// The idea: first copy the ASCII letters ("bücher" gives "bcher"), then add "-". After that, each
// non-ASCII character is stored as one number that says which character it is and where it goes.
// The numbers are written with the digits a-z (0-25) and 0-9 (26-35), in a variable-length form.
const BASE: u32 = 36;

/// Encode one label: "bücher" becomes "bcher-kva". `None` if a number overflows.
fn punycode_encode(label: &str) -> Option<String> {
    let chars: Vec<u32> = label.chars().map(|c| c as u32).collect();
    // Each character as its Unicode number. "ü" is 252. JS: `[...label].map(c => c.codePointAt(0))`.
    let mut out: String = label.chars().filter(|c| c.is_ascii()).collect();
    let ascii_count = out.len() as u32;
    // `as u32` converts the number type. Rust never converts number types on its own.
    if ascii_count > 0 {
        out.push('-');
    }

    let mut done = ascii_count; // how many characters are already placed
    let mut n = 128; // the character we look for next, from small Unicode numbers to big ones
    let mut delta: u32 = 0; // one running number for "which character" and "where it goes"
    let mut bias = 72; // tunes how many digits each number needs; it adapts as we go

    while (done as usize) < chars.len() {
        let next = *chars.iter().filter(|&&c| c >= n).min()?;
        // The smallest character not yet placed. `&&c` unpacks two layers of borrowing.
        delta = delta.checked_add((next - n).checked_mul(done + 1)?)?;
        // `checked_add` gives `None` on overflow instead of a wrong number. JS numbers never
        // overflow like this. They lose precision instead.
        n = next;
        for &c in &chars {
            if c < n {
                delta = delta.checked_add(1)?;
            }
            if c == n {
                let mut q = delta;
                let mut k = BASE;
                loop {
                    let t = threshold(k, bias);
                    if q < t {
                        break;
                    }
                    out.push(digit(t + (q - t) % (BASE - t)));
                    q = (q - t) / (BASE - t);
                    k += BASE;
                }
                out.push(digit(q));
                // The last digit is below the threshold, so a reader knows the number ends here.
                bias = adapt(delta, done + 1, done == ascii_count);
                delta = 0;
                done += 1;
            }
        }
        delta = delta.checked_add(1)?;
        n += 1;
    }
    Some(out)
}

/// Decode the part after "xn--": "bcher-kva" becomes "bücher". `None` if it is not valid Punycode.
fn punycode_decode(code: &str) -> Option<String> {
    let (ascii, digits) = match code.rfind('-') {
        Some(i) => (&code[..i], &code[i + 1..]),
        None => ("", code),
    };
    // The ASCII part ends at the last "-". JS: `code.lastIndexOf("-")`.
    let mut out: Vec<char> = ascii.chars().collect();
    let mut digits = digits.bytes();
    let (mut n, mut i, mut bias) = (128u32, 0u32, 72);

    while digits.len() > 0 {
        let old_i = i;
        let mut weight = 1u32;
        let mut k = BASE;
        loop {
            let d = match digits.next()? {
                b @ b'a'..=b'z' => b - b'a',
                b @ b'A'..=b'Z' => b - b'A',
                b @ b'0'..=b'9' => b - b'0' + 26,
                _ => return None,
            } as u32;
            // `b @ pattern` names the value that matched. `b'a'..=b'z'` is a range pattern.
            i = i.checked_add(d.checked_mul(weight)?)?;
            let t = threshold(k, bias);
            if d < t {
                break;
            }
            weight = weight.checked_mul(BASE - t)?;
            k += BASE;
        }
        let len = out.len() as u32 + 1;
        bias = adapt(i - old_i, len, old_i == 0);
        n = n.checked_add(i / len)?;
        i %= len;
        if n < 128 {
            return None; // ASCII characters must be in the ASCII part
        }
        out.insert(i as usize, char::from_u32(n)?);
        // `from_u32` gives `None` for numbers that are not characters.
        i += 1;
    }
    Some(out.into_iter().collect())
}

/// The digit threshold for position `k` (RFC 3492, section 6.2).
fn threshold(k: u32, bias: u32) -> u32 {
    if k <= bias {
        1
    } else if k >= bias + 26 {
        26
    } else {
        k - bias
    }
}

/// Tune `bias` after each number, so the next numbers use few digits (RFC 3492, section 6.1).
fn adapt(delta: u32, count: u32, first: bool) -> u32 {
    let mut delta = if first { delta / 700 } else { delta / 2 };
    delta += delta / count;
    let mut k = 0;
    while delta > 455 {
        // 455 is ((36 - 1) * 26) / 2 from the RFC.
        delta /= BASE - 1;
        k += BASE;
    }
    k + (BASE * delta) / (delta + 38)
}

/// One Punycode digit: 0-25 are "a"-"z", 26-35 are "0"-"9".
fn digit(d: u32) -> char {
    if d < 26 { (b'a' + d as u8) as char } else { (b'0' + (d - 26) as u8) as char }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn punycode_labels() {
        // Expected values come from Python's built-in "punycode" codec.
        let cases = [("bücher", "bcher-kva"), ("münchen", "mnchen-3ya"), ("中文", "fiq228c"), ("faß", "fa-hia"), ("ñ", "ida")];
        for (label, code) in cases {
            assert_eq!(punycode_encode(label).as_deref(), Some(code), "encode {label}");
            assert_eq!(punycode_decode(code).as_deref(), Some(label), "decode {code}");
            // `as_deref()` turns `Option<String>` into `Option<&str>`, so it compares with `Some(code)`.
        }
    }

    #[test]
    fn domains() {
        assert_eq!(domain_to_ascii("BÜCHER.de").unwrap(), "xn--bcher-kva.de");
        assert_eq!(domain_to_ascii("日本語。ＪＰ").unwrap(), "xn--wgv71a119e.jp");
        // The ideographic full stop "。" is a dot, and fullwidth "ＪＰ" becomes "jp".
        assert_eq!(domain_to_ascii("u\u{308}.com").unwrap(), "xn--tda.com");
        // "u" + combining umlaut becomes "ü" (NFC) before encoding.
        assert_eq!(domain_to_ascii("☕.us").unwrap(), "xn--53h.us");
        assert!(domain_to_ascii("a⒈com").is_err());
        // "⒈" would turn into "1." and change the domain's structure, so it is disallowed.
        assert_eq!(to_ascii("xn--u-ccb.com"), None);
        // UTS #46 rejects it: it decodes to "u" + combining umlaut, which is not NFC.
        assert_eq!(domain_to_ascii("xn--u-ccb.com").unwrap(), "xn--u-ccb.com");
        // But browsers only lowercase an ASCII host. They skip UTS #46 for old sites.
        assert!(domain_to_ascii("a\u{200D}b.com").is_err());
        // A zero width joiner with no virama before it: invisible, so it could fake a domain.
        assert!(domain_to_ascii("exa mple.com").is_err());
        // A space is a forbidden domain code point.
    }

    /// Unicode's official UTS #46 conformance tests, with the browser settings.
    #[test]
    fn idna_conformance() {
        let mut tested = 0;
        for line in include_str!("../../../data/unicode/IdnaTestV2.txt").lines() {
            // `include_str!` puts the file's text into the program at compile time.
            let line = line.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let f: Vec<&str> = line.split(';').map(str::trim).collect();
            let Some(source) = unescape(f[0]) else {
                continue; // contains a lone surrogate, which a Rust string cannot hold
            };
            let to_unicode = if f[1].is_empty() { source.clone() } else { unescape(f[1]).unwrap() };
            let to_ascii_n = if f[3].is_empty() { to_unicode } else { unescape(f[3]).unwrap() };
            let status = if f[4].is_empty() { f[2] } else { f[4] };
            let has_error = status
                .trim_matches(['[', ']'])
                .split(", ")
                .any(|code| !matches!(code, "" | "V2" | "V3" | "U1" | "A4_1" | "A4_2" | "X4_2"));
            // Browsers turn off CheckHyphens (V2, V3), UseSTD3ASCIIRules (U1) and
            // VerifyDnsLength (A4_1, A4_2; X4_2 is the same check in ToUnicode). The test file
            // header says to ignore those codes for these settings.
            let expected = if has_error { None } else { Some(to_ascii_n) };
            assert_eq!(to_ascii(&source), expected, "source {:?}", f[0]);
            tested += 1;
        }
        assert!(tested > 6000, "only {tested} lines tested");
    }

    /// Read the test file's escapes: `\uXXXX`, `\x{XXXX}`, and `""` for the empty string.
    fn unescape(text: &str) -> Option<String> {
        if text == "\"\"" {
            return Some(String::new());
        }
        let mut out = String::new();
        let mut rest = text;
        while let Some(i) = rest.find('\\') {
            out.push_str(&rest[..i]);
            let (hex, after) = if let Some(r) = rest[i..].strip_prefix("\\x{") {
                r.split_once('}')?
            } else {
                (&rest[i + 2..i + 6], &rest[i + 6..])
            };
            out.push(char::from_u32(u32::from_str_radix(hex, 16).ok()?)?);
            rest = after;
        }
        out.push_str(rest);
        Some(out)
    }
}
