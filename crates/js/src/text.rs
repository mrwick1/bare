//! UTF-8 encoding and decoding for `TextEncoder` and `TextDecoder`, and the WHATWG label table.
//! Pure functions on slices: no heap, no execution. The callers check every size before they allocate.

use crate::heap::Decoder;

#[path = "text_labels.rs"]
mod labels;

/// What an encoding label names.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Label {
    Utf8,
    /// A real encoding that this engine does not implement (its WHATWG name).
    Other(&'static str),
    /// Not an encoding label: `new TextDecoder` throws a `RangeError`.
    Invalid,
}

/// Look up an encoding label: trim ASCII whitespace, lower-case ASCII, then match.
pub fn label(units: &[u16]) -> Label {
    let ws = |u: &u16| matches!(*u, 9 | 10 | 12 | 13 | 32);
    let start = units.iter().position(|u| !ws(u)).unwrap_or(units.len());
    let end = units.iter().rposition(|u| !ws(u)).map_or(start, |p| p + 1);
    let t = &units[start..end];
    if t.len() > 32 || t.iter().any(|&u| u >= 128) {
        return Label::Invalid; // the longest label is 22 units, and every label is ASCII
    }
    let s: String = t.iter().map(|&u| (u as u8).to_ascii_lowercase() as char).collect();
    match labels::LABELS.binary_search_by(|(l, _)| (*l).cmp(s.as_str())) {
        Ok(i) if labels::LABELS[i].1 == "utf-8" => Label::Utf8,
        Ok(i) => Label::Other(labels::LABELS[i].1),
        Err(_) => Label::Invalid,
    }
}

/// UTF-8 bytes of `units`. A lone surrogate is U+FFFD (3 bytes).
pub fn utf8_len(units: &[u16]) -> usize {
    let mut n = 0;
    let mut i = 0;
    while i < units.len() {
        let u = units[i];
        n += match u {
            0..=0x7F => 1,
            0x80..=0x7FF => 2,
            0xD800..=0xDBFF if units.get(i + 1).is_some_and(|l| (0xDC00..=0xDFFF).contains(l)) => {
                i += 1;
                4
            }
            _ => 3,
        };
        i += 1;
    }
    n
}

/// Encode whole code points of `units` into `out` until the next one does not fit. Returns
/// `(units read, bytes written)`. A lone surrogate becomes U+FFFD.
pub fn encode_into(units: &[u16], out: &mut [u8]) -> (usize, usize) {
    let (mut r, mut w) = (0, 0);
    while r < units.len() {
        let u = units[r];
        let (cp, used) = match u {
            0xD800..=0xDBFF if units.get(r + 1).is_some_and(|l| (0xDC00..=0xDFFF).contains(l)) => {
                (0x10000 + (((u as u32) - 0xD800) << 10) + (units[r + 1] as u32 - 0xDC00), 2)
            }
            0xD800..=0xDFFF => (0xFFFD, 1),
            _ => (u as u32, 1),
        };
        let need = match cp {
            0..=0x7F => 1,
            0x80..=0x7FF => 2,
            0x800..=0xFFFF => 3,
            _ => 4,
        };
        if out.len() - w < need {
            break;
        }
        match need {
            1 => out[w] = cp as u8,
            2 => {
                out[w] = 0xC0 | (cp >> 6) as u8;
                out[w + 1] = 0x80 | (cp & 0x3F) as u8;
            }
            3 => {
                out[w] = 0xE0 | (cp >> 12) as u8;
                out[w + 1] = 0x80 | ((cp >> 6) & 0x3F) as u8;
                out[w + 2] = 0x80 | (cp & 0x3F) as u8;
            }
            _ => {
                out[w] = 0xF0 | (cp >> 18) as u8;
                out[w + 1] = 0x80 | ((cp >> 12) & 0x3F) as u8;
                out[w + 2] = 0x80 | ((cp >> 6) & 0x3F) as u8;
                out[w + 3] = 0x80 | (cp & 0x3F) as u8;
            }
        }
        w += need;
        r += used;
    }
    (r, w)
}

/// A malformed sequence in fatal mode.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Malformed;

fn emit(d: &mut Decoder, cp: u32, out: &mut Vec<u16>) {
    if !d.bom_seen {
        d.bom_seen = true;
        if cp == 0xFEFF && !d.ignore_bom {
            return;
        }
    }
    if cp >= 0x10000 {
        let c = cp - 0x10000;
        out.push(0xD800 + (c >> 10) as u16);
        out.push(0xDC00 + (c & 0x3FF) as u16);
    } else {
        out.push(cp as u16);
    }
}

/// The WHATWG UTF-8 decoder over one chunk. `flush` is true for the last chunk: a sequence that is still open is
/// an error. In replacement mode every error is one U+FFFD (the maximal subpart rule). In fatal mode the first error
/// returns `Malformed`; the output and the registers are then unspecified, and the caller resets them.
/// Output is at most one unit for each input byte plus one for each byte still open from the previous chunk.
pub fn decode_utf8(d: &mut Decoder, input: &[u8], flush: bool, out: &mut Vec<u16>) -> Result<(), Malformed> {
    let mut i = 0;
    while i < input.len() {
        if d.needed == 0 {
            // An ASCII run needs no state.
            let run = input[i..].iter().position(|&b| b >= 0x80).unwrap_or(input.len() - i);
            if run > 0 {
                d.bom_seen = true;
                out.extend(input[i..i + run].iter().map(|&b| b as u16));
                i += run;
                continue;
            }
        }
        let b = input[i];
        i += 1;
        if d.needed == 0 {
            match b {
                0xC2..=0xDF => (d.needed, d.cp) = (1, (b & 0x1F) as u32),
                0xE0..=0xEF => {
                    if b == 0xE0 {
                        d.lower = 0xA0;
                    }
                    if b == 0xED {
                        d.upper = 0x9F;
                    }
                    (d.needed, d.cp) = (2, (b & 0xF) as u32);
                }
                0xF0..=0xF4 => {
                    if b == 0xF0 {
                        d.lower = 0x90;
                    }
                    if b == 0xF4 {
                        d.upper = 0x8F;
                    }
                    (d.needed, d.cp) = (3, (b & 7) as u32);
                }
                _ => {
                    if d.fatal {
                        return Err(Malformed);
                    }
                    emit(d, 0xFFFD, out);
                }
            }
            continue;
        }
        if b < d.lower || b > d.upper {
            // The byte ends the open sequence. Read it again as a new start byte.
            (d.cp, d.needed, d.seen, d.lower, d.upper) = (0, 0, 0, 0x80, 0xBF);
            i -= 1;
            if d.fatal {
                return Err(Malformed);
            }
            emit(d, 0xFFFD, out);
            continue;
        }
        (d.lower, d.upper) = (0x80, 0xBF);
        d.cp = (d.cp << 6) | (b & 0x3F) as u32;
        d.seen += 1;
        if d.seen == d.needed {
            let cp = d.cp;
            (d.cp, d.needed, d.seen) = (0, 0, 0);
            emit(d, cp, out);
        }
    }
    if flush && d.needed != 0 {
        (d.cp, d.needed, d.seen, d.lower, d.upper) = (0, 0, 0, 0x80, 0xBF);
        if d.fatal {
            return Err(Malformed);
        }
        emit(d, 0xFFFD, out);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dec(chunks: &[&[u8]], fatal: bool, ignore_bom: bool) -> Result<String, Malformed> {
        let mut d = Decoder::new(fatal, ignore_bom);
        let mut out = Vec::new();
        for (i, c) in chunks.iter().enumerate() {
            decode_utf8(&mut d, c, i + 1 == chunks.len(), &mut out)?;
        }
        Ok(String::from_utf16_lossy(&out))
    }

    #[test]
    fn labels() {
        let u = |s: &str| s.encode_utf16().collect::<Vec<u16>>();
        assert_eq!(label(&u(" UTF8\n")), Label::Utf8);
        assert_eq!(label(&u("unicode-1-1-utf-8")), Label::Utf8);
        assert_eq!(label(&u("latin1")), Label::Other("windows-1252"));
        assert_eq!(label(&u("utf-16le")), Label::Other("utf-16le"));
        for bad in ["", "foo", "utf 8", "utf-8\u{a0}", "ＵＴＦ-8", "utf-8x"] {
            assert_eq!(label(&u(bad)), Label::Invalid, "{bad:?}");
        }
        assert_eq!(label(&u("\u{212a}")), Label::Invalid); // Kelvin sign does not lower-case to `k`
    }

    #[test]
    fn encode_lengths_and_partial_output() {
        let u = |s: &[u16]| utf8_len(s);
        assert_eq!(u(&[0x61, 0xD800, 0x62]), 5);
        assert_eq!(u(&[0xD83D, 0xDE00]), 4);
        assert_eq!(u(&[0xDE00, 0xD83D]), 6);
        let mut o = [0u8; 4];
        assert_eq!(encode_into(&[0x61, 0xD800, 0x62], &mut o), (2, 4));
        assert_eq!(&o, &[0x61, 0xEF, 0xBF, 0xBD]);
        assert_eq!(encode_into(&[0x20AC, 0x20AC], &mut o), (1, 3));
        assert_eq!(encode_into(&[0xD83D, 0xDE00], &mut o[..3]), (0, 0));
        assert_eq!(encode_into(&[0xD83D, 0xDE00], &mut o), (2, 4));
    }

    /// Every split of every sample gives the same text as one chunk (and as Rust's lossy decoder when valid).
    #[test]
    fn every_split_matches_one_chunk() {
        let samples: [&[u8]; 6] = [
            "a€😀é\u{10FFFF}z".as_bytes(),
            &[0xEF, 0xBB, 0xBF, 0x41, 0xEF, 0xBB, 0xBF],
            &[0xE0, 0x80, 0x80, 0xF0, 0x9F, 0x92, 0x41],
            &[0xF4, 0x90, 0x80, 0x80, 0xED, 0xA0, 0x80, 0xC0, 0xAF],
            &[0xE2, 0x82],
            &[0xF0, 0x9F, 0x98],
        ];
        for s in samples {
            let whole = dec(&[s], false, false).unwrap();
            for i in 0..=s.len() {
                assert_eq!(dec(&[&s[..i], &s[i..]], false, false).unwrap(), whole, "{s:x?} split {i}");
                for j in i..=s.len() {
                    assert_eq!(dec(&[&s[..i], &s[i..j], &s[j..]], false, false).unwrap(), whole);
                }
            }
        }
        assert_eq!(dec(&[&[0xE2, 0x82]], false, false).unwrap(), "\u{fffd}");
        assert_eq!(dec(&[&[0xE0, 0x80, 0x80]], false, false).unwrap(), "\u{fffd}\u{fffd}\u{fffd}");
        assert_eq!(dec(&[&[0xF0, 0x9F, 0x98]], false, false).unwrap(), "\u{fffd}");
        assert_eq!(dec(&[&[0xEF, 0xBB, 0xBF, 0xEF, 0xBB, 0xBF]], false, false).unwrap(), "\u{feff}");
        assert_eq!(dec(&[&[0xEF, 0xBB, 0xBF, 0x41]], false, true).unwrap(), "\u{feff}A");
    }

    #[test]
    fn fatal_rejects_every_malformed_form() {
        for bad in [&[0xFFu8][..], &[0xC0, 0xAF], &[0xE2, 0x28], &[0xED, 0xA0, 0x80], &[0xF4, 0x90, 0x80, 0x80], &[0xE2, 0x82], &[0x80]] {
            assert_eq!(dec(&[bad], true, false), Err(Malformed), "{bad:x?}");
        }
        assert_eq!(dec(&[&[0xE2, 0x82], &[0xAC]], true, false).unwrap(), "€");
    }

    /// Valid text must match the standard library for random-ish bytes through random splits.
    #[test]
    fn matches_std_on_generated_bytes() {
        let mut x = 0x2545F4914F6CDD1Du64;
        for _ in 0..2000 {
            let mut b = Vec::new();
            for _ in 0..(x % 24) {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                b.push(match x % 5 { 0 => (x >> 8) as u8, 1 => 0xE2, 2 => 0x82, 3 => 0xAC, _ => 0x61 });
            }
            let want: String = String::from_utf8_lossy(&b).chars().collect();
            let cut = (x >> 20) as usize % (b.len() + 1);
            let got = dec(&[&b[..cut], &b[cut..]], false, true).unwrap();
            assert_eq!(got, want, "{b:x?} cut {cut}");
        }
    }
}
