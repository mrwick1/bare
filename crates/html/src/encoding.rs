//! Bytes to text.
//!
//! A server sends bytes. The same byte can mean different letters in different encodings.
//! For example, byte 0xE9 is "é" in windows-1252, but it is a broken byte in UTF-8.
//! So before we read any HTML, we must find out which encoding the page uses.
//!
//! The HTML standard says where to look, in this order:
//! 1. A byte order mark (BOM) at the start of the bytes.
//! 2. The `charset` in the `Content-Type` header, like `text/html; charset=utf-8`.
//! 3. A `<meta charset=...>` tag in the first 1024 bytes of the page.
//! 4. A guess. Real browsers use a detector. We say UTF-8 if the bytes are valid UTF-8,
//!    and windows-1252 if not.
//!
//! The header comes before the meta tag because the server knows more than the page
//! author: the author may not control how the server sends the page.
//! The scan stops at 1024 bytes so that the browser can decide quickly, before it has the whole page.
//!
//! We support two encodings: UTF-8 and windows-1252. Windows-1252 is also what browsers use
//! for the labels "latin1" and "iso-8859-1": real pages that say "iso-8859-1" use the
//! extra letters of windows-1252, like the curly quotes. Others, like UTF-16, are not done yet.
//!
//! Comments compare Rust with JavaScript, because you know JS.

/// The encodings we can read.
#[derive(Debug, PartialEq, Clone, Copy)]
// `Debug` lets us print it and `PartialEq` lets us compare with `==`. `Copy` means that
// passing it around copies it, like a number. JS: like a string constant `"utf-8"`.
enum Encoding {
    // JS: like a TypeScript union type: `"utf-8" | "windows-1252"`.
    Utf8,
    Windows1252,
}

/// Turn the bytes of a page into text. `content_type` is the value of the `Content-Type` header.
pub fn decode(bytes: &[u8], content_type: Option<&str>) -> String {
    // `Option<&str>` is a text that may be missing. JS: `string | undefined`.
    let (encoding, bytes) = if let Some(rest) = bytes.strip_prefix(b"\xEF\xBB\xBF") {
        // Step 1: the three bytes EF BB BF at the start are a BOM. They say "this is UTF-8".
        // They are not part of the page, so we drop them. `rest` is the bytes after them.
        (Encoding::Utf8, rest)
    } else {
        let found = content_type.and_then(charset_from_header).or_else(|| charset_from_meta(bytes));
        // Step 2, then step 3. `and_then` runs only if there is a header.
        // `or_else` runs only if the first part found nothing. JS: `a ?? b`.
        let encoding = found.unwrap_or(if std::str::from_utf8(bytes).is_ok() { Encoding::Utf8 } else { Encoding::Windows1252 });
        // Step 4. `unwrap_or` gives the value, or this guess if there is none.
        (encoding, bytes)
    };

    let text = match encoding {
        Encoding::Utf8 => String::from_utf8_lossy(bytes).into_owned(),
        // Bad bytes become "�" instead of failing.
        Encoding::Windows1252 => bytes.iter().map(|&b| windows_1252(b)).collect(),
        // JS: `[...bytes].map(windows1252).join("")`. Each byte is one letter.
    };
    normalize_newlines(&text)
}

/// The HTML standard treats "\r\n" and "\r" as "\n". Do this once, so later code sees only "\n".
fn normalize_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
    // The order matters. If we changed "\r" first, "\r\n" would become two newlines.
}

/// Find `charset=...` in a header like `text/html; charset=UTF-8`.
fn charset_from_header(value: &str) -> Option<Encoding> {
    read_charset(value)
}

/// Look for `<meta charset=...>` or `<meta http-equiv=... content="...; charset=...">`
/// in the first 1024 bytes. This is a simple version of the scan in the standard.
fn charset_from_meta(bytes: &[u8]) -> Option<Encoding> {
    let start = &bytes[..bytes.len().min(1024)];
    // JS: `bytes.slice(0, 1024)`. `min` stops us from going past the end of a short page.
    let text = String::from_utf8_lossy(start).to_ascii_lowercase();
    // We only need ASCII letters here. Bad bytes become "�", and that is fine.
    let mut rest = text.as_str();
    while let Some(i) = rest.find("<meta") {
        // `while let` repeats while the pattern fits. Here: while there is another "<meta".
        rest = &rest[i + 5..];
        // Move past "<meta".
        let tag = &rest[..rest.find('>').unwrap_or(rest.len())];
        // The text of this tag, up to ">" or the end of the scan.
        if let Some(encoding) = read_charset(tag) {
            return Some(encoding);
        }
    }
    None
}

/// Find `charset` followed by `=` and a name. Give the encoding that the name stands for.
fn read_charset(text: &str) -> Option<Encoding> {
    let lower = text.to_ascii_lowercase();
    let after = lower[lower.find("charset")? + 7..].trim_start();
    // The text after "charset". The `?` returns `None` from this function if there is no "charset".
    let after = after.strip_prefix('=')?.trim_start();
    // It must have an "=" next, like `charset = utf-8`. Spaces are allowed.
    let name = after.trim_start_matches(['"', '\'']);
    // A name may be in quotes.
    let end = name.find(|c: char| c == '"' || c == '\'' || c == ';' || c == '>' || c == '/' || c.is_whitespace()).unwrap_or(name.len());
    label(&name[..end])
}

/// The encoding for a name like "utf-8" or "latin1". Names that we do not know give `None`.
fn label(name: &str) -> Option<Encoding> {
    match name {
        // JS: like a `switch`. `|` joins several names into one case.
        "utf-8" | "utf8" | "unicode-1-1-utf-8" => Some(Encoding::Utf8),
        "windows-1252" | "cp1252" | "x-cp1252" | "latin1" | "iso-8859-1" | "iso8859-1" | "iso_8859-1" | "l1" | "ascii" | "us-ascii" => {
            Some(Encoding::Windows1252)
        }
        _ => None,
        // We do not know this name. The caller goes on to the next step.
    }
}

/// The letter for one byte in windows-1252.
fn windows_1252(byte: u8) -> char {
    // Bytes 0x80 to 0x9F are the odd ones. Latin-1 puts control codes there.
    // windows-1252 puts letters and punctuation there, like "€" and the curly quotes.
    // Every other byte has the same number as its Unicode letter.
    const HIGH: [char; 32] = [
        '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8D}', 'Ž', '\u{8F}', '\u{90}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™',
        'š', '›', 'œ', '\u{9D}', 'ž', 'Ÿ',
    ];
    if (0x80..0xA0).contains(&byte) { HIGH[(byte - 0x80) as usize] } else { byte as char }
    // `byte as char` turns the number into the letter with that Unicode number.
}

#[cfg(test)]
// Only compiled for `cargo test`.
mod tests {
    use super::*;
    // Import everything from this file, including the private functions.

    #[test]
    fn reads_utf8_by_default() {
        assert_eq!(decode("héllo".as_bytes(), None), "héllo");
        // Valid UTF-8 and no hints: it is UTF-8.
    }

    #[test]
    fn guesses_windows_1252_for_bad_utf8() {
        assert_eq!(decode(b"caf\xE9", None), "café");
        // 0xE9 alone is not valid UTF-8, so we guess windows-1252.
    }

    #[test]
    fn drops_the_bom() {
        assert_eq!(decode(b"\xEF\xBB\xBFhi", None), "hi");
    }

    #[test]
    fn bom_wins_over_the_header() {
        assert_eq!(decode(b"\xEF\xBB\xBFcaf\xC3\xA9", Some("text/html; charset=iso-8859-1")), "café");
        // Step 1 beats step 2. Read as windows-1252, this would give "cafÃ©".
    }

    #[test]
    fn header_wins_over_meta() {
        let page = b"<meta charset=utf-8>caf\xE9";
        assert_eq!(decode(page, Some("text/html; charset=ISO-8859-1")), "<meta charset=utf-8>café");
        // The header says windows-1252, the page says UTF-8. The header wins.
        assert_eq!(decode(page, None), "<meta charset=utf-8>caf\u{FFFD}");
        // With no header, the meta tag says UTF-8. The byte 0xE9 is bad in UTF-8, so it becomes "�".
    }

    #[test]
    fn reads_the_meta_tag() {
        assert_eq!(decode(b"<meta charset=\"windows-1252\">\x93hi\x94", None), "<meta charset=\"windows-1252\">\u{201C}hi\u{201D}");
        // The meta tag says windows-1252, so 0x93 and 0x94 are curly quotes.
        let old_style = b"<META HTTP-EQUIV=\"Content-Type\" CONTENT=\"text/html; charset=iso-8859-1\">\xE9";
        assert!(decode(old_style, None).ends_with('é'));
        // The old way to write it, with capital letters.
    }

    #[test]
    fn ignores_the_meta_tag_after_1024_bytes() {
        let mut page = vec![b' '; 1100];
        page.extend_from_slice(b"<meta charset=windows-1252>\xC3\xA9");
        assert!(decode(&page, None).ends_with('é'));
        // The tag is too late, so it is ignored. The bytes are valid UTF-8, so it is UTF-8.
    }

    #[test]
    fn unknown_names_are_skipped() {
        assert_eq!(decode(b"caf\xE9", Some("text/html; charset=klingon")), "café");
        // We do not know "klingon", so we go on to guess.
    }

    #[test]
    fn reads_header_forms() {
        assert_eq!(charset_from_header("text/html;charset=utf-8"), Some(Encoding::Utf8));
        assert_eq!(charset_from_header("text/html; charset = \"UTF-8\""), Some(Encoding::Utf8));
        assert_eq!(charset_from_header("text/html; charset=latin1; x=y"), Some(Encoding::Windows1252));
        assert_eq!(charset_from_header("text/html"), None);
    }

    #[test]
    fn joins_newlines() {
        assert_eq!(decode(b"a\r\nb\rc\n", None), "a\nb\nc\n");
    }

    #[test]
    fn windows_1252_special_bytes() {
        assert_eq!(windows_1252(0x80), '€');
        assert_eq!(windows_1252(0x41), 'A');
        assert_eq!(windows_1252(0xFF), 'ÿ');
    }
}
