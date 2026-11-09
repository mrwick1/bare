//! English date and time formatting for `Intl.DateTimeFormat` and `Date.prototype.toLocale*String`. Pure functions: the
//! builtins are in `eval_intl.rs`, the zones in `tz.rs`.
//!
//! - Locales: `en`, `en-US`, `en-CA`, `en-IN` and `en-GB`, with the patterns and names of ICU (`intl_data.rs`, made by
//!   `tools/js/intl_gen.mjs` from Node's ICU). Lookup drops subtags from the right (`en-AU` is `en`). A locale with no match
//!   falls back to `en-US`, as ECMA-402 allows when a locale is not available.
//! - A pattern comes from tables, not from a pattern generator: a date pattern for each set of date fields, a time pattern
//!   for each set of time fields and hour cycle, a joiner between them (", " or " at "), and the whole pattern for the 576
//!   pairs that are not date + joiner + time (ICU has a better match for them, like "Fri 3:04 AM").

use crate::date::Fields;
use crate::intl_data::*;

/// The name of locale `i` (the BCP 47 tag that `resolvedOptions` reports).
pub fn locale_name(i: u8) -> &'static str {
    LOCALES[i as usize]
}

/// Option widths. 0 is "absent".
pub const NUMERIC: u8 = 1;
pub const TWO_DIGIT: u8 = 2;
pub const NARROW: u8 = 3;
pub const SHORT: u8 = 4;
pub const LONG: u8 = 5;

/// The fields of a request, after the defaults. Weekday is 0 or NARROW..=LONG, year, day and the time fields 0, NUMERIC or
/// TWO_DIGIT, month any width. Styles: 0 none, 1 full, 2 long, 3 medium, 4 short. `hc`: 0 h11, 1 h12, 2 h23, 3 h24.
#[derive(Clone, Copy, Default, Debug)]
pub struct Request {
    pub weekday: u8,
    pub year: u8,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    pub date_style: u8,
    pub time_style: u8,
    pub hc: u8,
    /// A `timeZoneName` option: the name goes after the fields.
    pub zone: bool,
}

/// What `format` writes besides the fields: the text of the time zone name (a `timeStyle` full or long, or the option
/// `timeZoneName`) and `fractionalSecondDigits` (0 for none).
#[derive(Clone, Copy, Default)]
pub struct Extras<'a> {
    pub zone: &'a str,
    pub fsd: u8,
}

/// A resolved pattern: one pattern, or a date pattern, a joiner and a time pattern.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pattern {
    first: u16,
    second: u16,
    /// 0: `first` only, 1: ", ", 2: " at "
    join: u8,
    /// What goes before the time zone name that follows the pattern: 0 no name, 1 " ", 2 ", ", 3 " at ".
    zone: u8,
}

impl Pattern {
    /// True when a `timeZoneName` option adds a name after the fields: call `zone_tail` after `format`.
    pub fn has_zone(self) -> bool {
        self.zone != 0
    }
}

/// The type of a part, in the order of `PART_NAMES`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    Literal,
    Weekday,
    Year,
    Month,
    Day,
    DayPeriod,
    Hour,
    Minute,
    Second,
    TimeZoneName,
    FractionalSecond,
}

pub const PART_NAMES: [&str; 11] =
    ["literal", "weekday", "year", "month", "day", "dayPeriod", "hour", "minute", "second", "timeZoneName", "fractionalSecond"];

/// What a pattern cannot format yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gate {
    /// A pattern letter that bare cannot format.
    ZoneName,
    /// A year before 1 (ICU writes an era).
    Era,
}

/// The index of locale `tag` (canonical) by lookup, or `None` when nothing matches.
pub fn lookup(tag: &str) -> Option<u8> {
    let mut t = tag;
    loop {
        if let Some(i) = LOCALES.iter().position(|l| l.eq_ignore_ascii_case(t)) {
            return Some(i as u8);
        }
        let cut = t.rfind('-')?;
        t = &t[..cut];
        // a singleton is dropped with the subtag after it (ECMA-402 LookupMatchingLocaleByPrefix)
        if t.len() >= 2 && t.as_bytes()[t.len() - 2] == b'-' {
            t = &t[..t.len() - 2];
        }
    }
}

/// Why a language tag is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TagError {
    /// A subtag that is not 1 to 8 letters or digits, or a bad language subtag: V8's first check.
    Malformed,
    /// Any other tag that is not well formed (a duplicate variant or singleton, an empty extension).
    Invalid,
    /// An extension or private use (`-u-hc-h23`, `-x-...`): bare does not read them yet.
    Extension,
}

/// CanonicalizeUnicodeLocaleId for the tags bare reads: `language[-script][-region](-variant)*`, with the case of each part
/// made canonical. A tag with an extension is refused (`TagError::Extension`) after its syntax is checked.
pub fn canonical_tag(s: &str) -> Result<String, TagError> {
    let parts: Vec<&str> = s.split('-').collect();
    let alpha = |p: &str| p.bytes().all(|c| c.is_ascii_alphabetic());
    let alnum = |p: &str| !p.is_empty() && p.bytes().all(|c| c.is_ascii_alphanumeric());
    let lang = parts[0];
    if !(alpha(lang) && matches!(lang.len(), 2 | 3 | 5..=8)) || parts.iter().any(|p| !alnum(p) || p.len() > 8) {
        return Err(TagError::Malformed);
    }
    let mut out = lang.to_ascii_lowercase();
    let mut i = 1;
    if parts.get(i).is_some_and(|p| p.len() == 4 && alpha(p)) {
        let p = parts[i];
        out.push('-');
        out.push(p.as_bytes()[0].to_ascii_uppercase() as char);
        out.push_str(&p[1..].to_ascii_lowercase());
        i += 1;
    }
    if parts.get(i).is_some_and(|p| (p.len() == 2 && alpha(p)) || (p.len() == 3 && p.bytes().all(|c| c.is_ascii_digit()))) {
        out.push('-');
        out.push_str(&parts[i].to_ascii_uppercase());
        i += 1;
    }
    let mut variants: Vec<String> = Vec::new();
    while let Some(p) = parts.get(i).filter(|p| alnum(p) && ((5..=8).contains(&p.len()) || (p.len() == 4 && p.as_bytes()[0].is_ascii_digit()))) {
        let v = p.to_ascii_lowercase();
        if variants.contains(&v) {
            return Err(TagError::Invalid);
        }
        variants.push(v);
        i += 1;
    }
    for v in &variants {
        out.push('-');
        out.push_str(v);
    }
    if i == parts.len() {
        return Ok(out);
    }
    // the rest must be extensions or private use: a singleton, then subtags of 2 to 8 (1 to 8 after `x`, which takes the rest)
    let mut j = i;
    let mut seen = 0u128;
    while j < parts.len() {
        let s = parts[j];
        if s.len() != 1 || !alnum(s) {
            return Err(TagError::Malformed);
        }
        let bit = 1u128 << (s.as_bytes()[0].to_ascii_lowercase() - b'0');
        if seen & bit != 0 {
            return Err(TagError::Invalid);
        }
        seen |= bit;
        let x = s.eq_ignore_ascii_case("x");
        j += 1;
        let start = j;
        while j < parts.len() && (x || parts[j].len() != 1) {
            let p = parts[j];
            if !alnum(p) || p.len() > 8 || (!x && p.len() < 2) {
                return Err(TagError::Invalid);
            }
            j += 1;
        }
        if j == start {
            return Err(TagError::Invalid);
        }
    }
    Err(TagError::Extension)
}

/// The resolved hour cycle for a locale: `hour12` (None, true, false) and `hourCycle` (None or 0..=3).
pub fn hour_cycle(loc: u8, hour12: Option<bool>, hc: Option<u8>) -> u8 {
    let a = match hour12 { None => 0, Some(true) => 1, Some(false) => 2 };
    HOUR_CYCLE[loc as usize][a * 5 + hc.map_or(0, |h| h as usize + 1)]
}

/// The pattern of a request. The request has at least one field or style.
pub fn resolve(loc: u8, r: &Request) -> Pattern {
    let l = loc as usize;
    if r.date_style != 0 || r.time_style != 0 {
        return Pattern { first: STYLE[l][r.hc as usize][r.date_style as usize * 5 + r.time_style as usize], second: 0, join: 0, zone: 0 };
    }
    let wd = if r.weekday == 0 { 0 } else { r.weekday - NARROW + 1 } as usize;
    let d = wd * 54 + r.year as usize * 18 + r.month as usize * 3 + r.day as usize;
    let t = if r.hour | r.minute | r.second == 0 { 0 } else { r.hc as usize * 27 + r.hour as usize * 9 + r.minute as usize * 3 + r.second as usize };
    // the name follows the time, or follows a date with the joiner of date and time
    let zone = |z: u8| if r.zone { z } else { 0 };
    match (d, t) {
        (_, 0) => Pattern { first: DATE[l][d], second: 0, join: 0, zone: zone(JOIN[l][d] + 2) },
        (0, _) => Pattern { first: TIME[l][t], second: 0, join: 0, zone: zone(1) },
        _ => {
            let key = (l as u32) << 16 | (d as u32) << 7 | t as u32;
            match EXCEPT.binary_search_by_key(&key, |e| e.0) {
                Ok(i) => Pattern { first: EXCEPT[i].1, second: 0, join: 0, zone: zone(1) },
                Err(_) => Pattern { first: DATE[l][d], second: TIME[l][t], join: JOIN[l][d] + 1, zone: zone(1) },
            }
        }
    }
}

fn pool(i: u16) -> &'static str {
    &POOL[AT[i as usize] as usize..AT[i as usize + 1] as usize]
}

/// The pieces of a pattern in order: `Ok((letter, width))` for a field, `Err(text)` for a literal.
fn pieces(p: Pattern) -> impl Iterator<Item = Result<(u8, u8), &'static str>> {
    let join = [None, Some(", "), Some(" at ")][p.join as usize];
    let first = Tokens(pool(p.first));
    let rest = join.map(|j| std::iter::once(Err(j)).chain(Tokens(pool(p.second)))).into_iter().flatten();
    first.chain(rest)
}

struct Tokens(&'static str);

impl Iterator for Tokens {
    type Item = Result<(u8, u8), &'static str>;
    fn next(&mut self) -> Option<Self::Item> {
        let s = self.0;
        if s.is_empty() {
            return None;
        }
        if let Some(rest) = s.strip_prefix('{') {
            let b = rest.as_bytes();
            self.0 = &rest[3..];
            return Some(Ok((b[0], b[1])));
        }
        let end = s.find(|c| c == '{').unwrap_or(s.len());
        self.0 = &s[end..];
        Some(Err(&s[..end]))
    }
}

/// The fields that a pattern shows, with their option values, for `resolvedOptions`: (name, value) in ECMA-402 order, and
/// the hour cycle (0..=3) when the pattern has an hour.
pub fn shown(p: Pattern) -> (Vec<(&'static str, &'static str)>, Option<u8>) {
    let mut f: [Option<&'static str>; 7] = [None; 7];
    let mut hc = None;
    for x in pieces(p) {
        let Ok((c, w)) = x else { continue };
        let v = match w {
            b'1' => "numeric",
            b'2' => "2-digit",
            b'n' => "narrow",
            b's' => "short",
            _ => "long",
        };
        let i = match c {
            b'w' => 0,
            b'y' => 1,
            b'M' => 2,
            b'd' => 3,
            b'h' | b'K' | b'H' | b'k' => {
                hc = Some(match c { b'K' => 0, b'h' => 1, b'H' => 2, _ => 3 });
                4
            }
            b'm' => 5,
            b's' => 6,
            _ => continue,
        };
        f[i] = Some(v);
    }
    let names = ["weekday", "year", "month", "day", "hour", "minute", "second"];
    (names.iter().zip(f).filter_map(|(n, v)| v.map(|v| (*n, v))).collect(), hc)
}

/// Format local fields `t` with pattern `p` of locale `loc` into `out`. `parts` gets the type and the end of each part
/// (adjacent literals are one part).
pub fn format(loc: u8, p: Pattern, t: &Fields, ex: &Extras, out: &mut String, mut parts: Option<&mut Vec<(Part, usize)>>) -> Result<(), Gate> {
    let names = &NAMES[LOCALE_NAMES[loc as usize] as usize];
    for x in pieces(p) {
        let kind = match x {
            Err(lit) => {
                // V8 prints U+202F (narrow no-break space) as U+0020 in a string, but keeps it in parts.
                match parts {
                    None => lit.chars().for_each(|c| out.push(if c == '\u{202f}' { ' ' } else { c })),
                    Some(_) => out.push_str(lit),
                }
                if let Some(ps) = parts.as_deref_mut() {
                    match ps.last_mut() {
                        Some((Part::Literal, end)) => *end = out.len(),
                        _ => ps.push((Part::Literal, out.len())),
                    }
                }
                continue;
            }
            Ok((c, w)) => {
                let text = |base: usize| match w { b'n' => base, b's' => base + 1, _ => base + 2 };
                let num = |out: &mut String, v: i64| {
                    if w == b'2' && v < 10 {
                        out.push('0');
                    }
                    out.push_str(&v.to_string());
                };
                match c {
                    b'w' => {
                        out.push_str(names[36 + (text(0) * 7) + t.weekday as usize]);
                        Part::Weekday
                    }
                    b'y' => {
                        if t.year < 1 {
                            return Err(Gate::Era);
                        }
                        num(out, if w == b'2' { t.year % 100 } else { t.year });
                        Part::Year
                    }
                    b'M' => {
                        if w == b'1' || w == b'2' { num(out, t.month + 1) } else { out.push_str(names[text(0) * 12 + t.month as usize]) }
                        Part::Month
                    }
                    b'd' => {
                        num(out, t.day);
                        Part::Day
                    }
                    b'h' | b'K' | b'H' | b'k' => {
                        let h = t.hour;
                        num(out, match c { b'h' => (h + 11) % 12 + 1, b'K' => h % 12, b'H' => h, _ => (h + 23) % 24 + 1 });
                        Part::Hour
                    }
                    b'm' => {
                        num(out, t.minute);
                        Part::Minute
                    }
                    b's' => {
                        num(out, t.second);
                        if ex.fsd == 0 {
                            Part::Second
                        } else {
                            fraction(ex.fsd, t.ms, out, parts.as_deref_mut());
                            Part::FractionalSecond
                        }
                    }
                    b'z' => {
                        out.push_str(ex.zone);
                        Part::TimeZoneName
                    }
                    b'a' => {
                        out.push_str(names[57 + (t.hour >= 12) as usize]);
                        Part::DayPeriod
                    }
                    _ => return Err(Gate::ZoneName),
                }
            }
        };
        if let Some(ps) = parts.as_deref_mut() {
            ps.push((kind, out.len()));
        }
    }
    Ok(())
}

/// ".ddd" after the seconds: the part of the seconds, the point, then the digits (the caller pushes their part).
#[cold]
#[inline(never)]
fn fraction(digits: u8, ms: i64, out: &mut String, parts: Option<&mut Vec<(Part, usize)>>) {
    if let Some(ps) = parts {
        ps.extend([(Part::Second, out.len()), (Part::Literal, out.len() + 1)]);
    }
    out.push('.');
    out.push_str(&format!("{ms:03}")[..digits as usize]);
}

/// The name that a `timeZoneName` option adds after the fields.
/// `format` does not call it: the caller does when `p.zone` is not 0 (`Pattern::has_zone`).
pub fn zone_tail(p: Pattern, zone: &str, out: &mut String, parts: Option<&mut Vec<(Part, usize)>>) {
    out.push_str(["", " ", ", ", " at "][p.zone as usize]);
    let mut parts = parts;
    if let Some(ps) = parts.as_deref_mut() {
        match ps.last_mut() {
            Some((Part::Literal, end)) => *end = out.len(),
            _ => ps.push((Part::Literal, out.len())),
        }
    }
    out.push_str(zone);
    if let Some(ps) = parts {
        ps.push((Part::TimeZoneName, out.len()));
    }
}

#[cfg(test)]
#[path = "intl_tests.rs"]
mod tests;
