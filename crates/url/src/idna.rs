//! Internationalized domain names: turn a host like "Bücher.de" into "xn--bcher-kva.de".
//!
//! DNS only understands plain English letters, digits and "-". So browsers rewrite the host:
//!
//! 1. Clean up each character: lowercase it, and turn look-alike forms into the normal one.
//!    The wide "Ｂ" becomes "b".
//! 2. Join letters and accents that belong together ("u" + "¨" becomes "ü").
//! 3. Split into labels at ".". A label is one part: "bücher" and "de".
//! 4. Check each label for characters that are not allowed or that could trick the user.
//! 5. Write each non-English label in a code called Punycode, with "xn--" in front.

use bare_unicode::tables::{BIDI, Bidi, IDNA, Idna, JOINING, Joining, MARKS};
use bare_unicode::{combining_class, find, nfc};
// From the `bare-unicode` crate. In code, "-" in a crate name becomes "_".

/// Turn a host into plain ASCII, or give an error. This is the function other files call.
pub fn domain_to_ascii(domain: &str) -> Result<String, String> {
    let ascii = if domain.is_ascii() {
        domain.to_ascii_lowercase()
        // A plain ASCII host is only lowercased. It skips the checks below.
        // Browsers do this so that old sites keep working.
    } else {
        to_ascii(domain).ok_or_else(|| format!("invalid domain: {domain}"))?
    };
    if ascii.is_empty() {
        return Err("empty host".to_string());
    }
    if let Some(c) = ascii.chars().find(|&c| c <= '\u{1f}' || c == '\u{7f}' || " #%/:<>?@[\\]^|".contains(c)) {
        // Some characters are never allowed in a host: invisible control characters,
        // and characters that mean something else in a URL, like "/" or "?".
        return Err(format!("forbidden character in host: {c:?}"));
    }
    Ok(ascii)
}

/// Do the 5 steps from the top of this file, the way browsers do them.
/// Gives `None` on any error.
fn to_ascii(domain: &str) -> Option<String> {
    // Step 1: clean up each character. A big table from Unicode says what to do with each one.
    let mut mapped = String::new();
    for c in domain.chars() {
        match find(IDNA, c as u32) {
            Some((Idna::Mapped, to)) => mapped.push_str(to),
            Some((Idna::Ignored, _)) => {}
            // Ignored characters, like the soft hyphen, are removed.
            _ => mapped.push(c),
            // All other characters stay as they are for now.
            // Step 4 finds the ones that are not allowed and gives an error.
        }
    }

    // Steps 2 and 3: join letters and accents, then split into labels.
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

    // Languages like Arabic and Hebrew are written right to left. Extra rules apply
    // only when the domain has such text in it.
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

/// Check one label. True means it is allowed.
/// `decoded` is true when the label came from "xn--" text, so steps 1 and 2 did not clean it up.
fn valid_label(label: &str, decoded: bool) -> bool {
    let chars: Vec<char> = label.chars().collect();
    let Some(&first) = chars.first() else {
        return true;
        // `let ... else` runs the `else` block if the pattern does not fit.
        // Here: an empty label has nothing to check.
    };
    if decoded && nfc(label) != label {
        return false; // letters and accents must already be joined
    }
    if label.starts_with("xn--") || label.contains('.') {
        return false; // no "xn--" inside "xn--", and no dot inside a label
    }
    if find(MARKS, first as u32).is_some() {
        return false; // must not start with an accent or other combining mark
    }
    if !chars.iter().all(|&c| matches!(find(IDNA, c as u32), Some((Idna::Valid | Idna::Deviation, _)))) {
        return false; // every character must be one the table allows
    }
    joiners_ok(&chars)
}

/// Rules for two invisible characters, called joiners.
/// They are allowed only where they change how the text looks, for example in Hindi or Arabic.
/// Anywhere else they are invisible, so a fake "google.com" could hide one.
fn joiners_ok(chars: &[char]) -> bool {
    // `&[char]` is a list of characters that we only read.
    for (i, &c) in chars.iter().enumerate() {
        if c != '\u{200C}' && c != '\u{200D}' {
            continue;
        }
        if i > 0 && combining_class(chars[i - 1]) == 9 {
            continue; // after a special Hindi-style mark, both joiners are fine
        }
        if c == '\u{200D}' {
            return false; // this joiner is only fine after that mark
        }
        // The other joiner must stand between two Arabic-style letters that would
        // connect to each other. Small marks between them are skipped.
        let before = chars[..i].iter().rev().map(|&c| joining(c)).find(|&j| j != Some(Joining::T));
        let after = chars[i + 1..].iter().map(|&c| joining(c)).find(|&j| j != Some(Joining::T));
        if !matches!(before, Some(Some(Joining::L | Joining::D))) || !matches!(after, Some(Some(Joining::R | Joining::D))) {
            return false;
        }
    }
    true
}

/// Rules for labels with right-to-left text (Arabic, Hebrew).
/// They stop a mix of directions that could show the name in a misleading order.
fn bidi_ok(label: &str) -> bool {
    use Bidi::*;
    // Lets us write `R` instead of `Bidi::R` inside this function.
    let classes: Vec<Bidi> = label.chars().map(bidi).collect();
    let last = classes.iter().copied().rev().find(|&b| b != NSM);
    // The last character's direction, skipping accents.
    match classes[0] {
        R | AL => {
            // A right-to-left label: only right-to-left letters, digits and some symbols.
            classes.iter().all(|b| matches!(b, R | AL | AN | EN | ES | CS | ET | ON | BN | NSM))
                && matches!(last, Some(R | AL | EN | AN))
                && !(classes.contains(&EN) && classes.contains(&AN))
            // Western digits and Arabic digits must not mix.
        }
        L => {
            // A left-to-right label: no right-to-left letters.
            classes.iter().all(|b| matches!(b, L | EN | ES | CS | ET | ON | BN | NSM)) && matches!(last, Some(L | EN))
        }
        _ => false, // the first character must be a letter
    }
}

fn bidi(c: char) -> Bidi {
    find(BIDI, c as u32).unwrap_or(Bidi::L)
}

fn joining(c: char) -> Option<Joining> {
    find(JOINING, c as u32)
    // `None` means the letter never connects to its neighbors.
}

// Punycode: write any label with only a-z, 0-9 and "-".
//
// The idea: first copy the plain letters ("bücher" gives "bcher"), then add "-". After that,
// each other character becomes a number that says which character it is and where it goes.
// The numbers are written with a-z and 0-9 as digits: "bcher-kva".
// You do not need to follow the math below. It is a fixed recipe from the Punycode standard.
const BASE: u32 = 36;

/// Encode one label: "bücher" becomes "bcher-kva". `None` if a number overflows.
fn punycode_encode(label: &str) -> Option<String> {
    let chars: Vec<u32> = label.chars().map(|c| c as u32).collect();
    // Each character as its Unicode number. "ü" is 252.
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
        // `checked_add` adds, but gives `None` if the number gets too big to store.
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
                // The last digit is small, so the reader knows the number ends here.
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

/// Part of the Punycode recipe: how small a digit must be to end a number.
fn threshold(k: u32, bias: u32) -> u32 {
    if k <= bias {
        1
    } else if k >= bias + 26 {
        26
    } else {
        k - bias
    }
}

/// Part of the Punycode recipe: tune `bias` so the next numbers use few digits.
fn adapt(delta: u32, count: u32, first: bool) -> u32 {
    let mut delta = if first { delta / 700 } else { delta / 2 };
    delta += delta / count;
    let mut k = 0;
    while delta > 455 {
        // 455 is a fixed value from the Punycode standard.
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
        // The expected values come from Python, to check against another tool.
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
        // The Japanese full stop "。" counts as a dot, and the wide "ＪＰ" becomes "jp".
        assert_eq!(domain_to_ascii("u\u{308}.com").unwrap(), "xn--tda.com");
        // "u" + a separate "¨" becomes "ü" before encoding.
        assert_eq!(domain_to_ascii("☕.us").unwrap(), "xn--53h.us");
        assert!(domain_to_ascii("a⒈com").is_err());
        // "⒈" would turn into "1." and change the domain's structure, so it is disallowed.
        assert_eq!(to_ascii("xn--u-ccb.com"), None);
        // The full checks reject it: it decodes to "u" + a separate "¨", which is not joined.
        assert_eq!(domain_to_ascii("xn--u-ccb.com").unwrap(), "xn--u-ccb.com");
        // But browsers only lowercase a plain ASCII host. They skip the checks for old sites.
        assert!(domain_to_ascii("a\u{200D}b.com").is_err());
        // An invisible joiner in the wrong place could fake a domain.
        assert!(domain_to_ascii("exa mple.com").is_err());
        // A space is never allowed in a host.
    }

    /// Run the official test file from Unicode: thousands of hosts with the right answers.
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
            // These error codes are for checks that browsers turn off. The test file says
            // to ignore them for browser settings.
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
