//! Time zones for `Intl.DateTimeFormat`: the UTC offset of a zone at an instant.
//!
//! - The rules come from the TZif files of the system (RFC 8536, versions 1 to 4), under `$TZDIR` or `/usr/share/zoneinfo`.
//!   bare carries no zone data. A file is read on the first use of its zone (`eval_intl.rs` keeps the parsed zone).
//! - A file has transitions (an instant and the offset after it) and a footer rule (a POSIX TZ string) for the instants after
//!   the last transition, or for all time when there is no transition. Instants before the first transition use time type 0,
//!   as RFC 8536 says.
//! - Files with leap seconds (`right/`) are refused: JavaScript time has no leap seconds.
//! - `+05:30`, `-0530` and `+05` are fixed offsets (ECMA-262 offset time zones), and the UTC names are one zone.
//!
//! All times here are whole seconds since 1970-01-01T00:00:00Z. Offsets are seconds east of UTC.

use crate::date::days_from_civil;

/// The largest TZif file we read. The biggest file in tzdata 2025 is under 4 KiB.
pub const MAX_FILE: u64 = 64 * 1024;
const MAX_TRANSITIONS: usize = 8192;

#[derive(Debug, PartialEq)]
pub struct Zone {
    /// Transition instants, ascending.
    at: Vec<i64>,
    /// The time type after each transition, an index into `offs`.
    ty: Vec<u8>,
    /// The offset of each time type.
    offs: Vec<i32>,
    /// The rule after the last transition.
    rule: Option<Rule>,
}

/// A POSIX TZ rule: a standard offset, and maybe a daylight offset with its start and end in each year.
#[derive(Debug, PartialEq, Clone, Copy)]
struct Rule {
    std: i32,
    dst: Option<(i32, When, When)>,
}

/// A day of the year and a local time on it (seconds, may be negative or past 24 hours in TZif version 3).
#[derive(Debug, PartialEq, Clone, Copy)]
struct When {
    day: Day,
    time: i32,
}

#[derive(Debug, PartialEq, Clone, Copy)]
enum Day {
    /// `Jn`: day 1 to 365, February 29 is never counted.
    Julian(u16),
    /// `n`: day 0 to 365, February 29 is counted.
    Zero(u16),
    /// `Mm.w.d`: weekday `d` (0 Sunday) of week `w` (1 to 4, 5 the last) of month `m`.
    Month(u8, u8, u8),
}

impl Zone {
    /// A zone with one offset for all time.
    pub fn fixed(off: i32) -> Zone {
        Zone { at: Vec::new(), ty: Vec::new(), offs: vec![off], rule: None }
    }

    /// Parse a TZif file. `None` for a file that is not TZif, is cut short, has leap seconds, or is out of bounds.
    pub fn parse(b: &[u8]) -> Option<Zone> {
        let (v1, ver) = header(b, 0)?;
        let mut p = 44;
        let (cnt, wide) = if ver >= b'2' {
            p += v1.len(4);
            let (v2, _) = header(b, p)?;
            p += 44;
            (v2, true)
        } else {
            (v1, false)
        };
        if cnt.leap != 0 || cnt.types == 0 || cnt.types > 256 || cnt.times > MAX_TRANSITIONS {
            return None;
        }
        let w = if wide { 8 } else { 4 };
        let times = b.get(p..p + cnt.times * w)?;
        let at: Vec<i64> = times.chunks_exact(w).map(|c| if wide { i64::from_be_bytes(c.try_into().unwrap()) } else { i32::from_be_bytes(c.try_into().unwrap()) as i64 }).collect();
        p += cnt.times * w;
        let ty = b.get(p..p + cnt.times)?.to_vec();
        p += cnt.times;
        let infos = b.get(p..p + cnt.types * 6)?;
        let offs: Vec<i32> = infos.chunks_exact(6).map(|c| i32::from_be_bytes([c[0], c[1], c[2], c[3]])).collect();
        if ty.iter().any(|&t| t as usize >= offs.len()) || at.windows(2).any(|x| x[0] >= x[1]) || offs.iter().any(|o| o.abs() > 25 * 3600) {
            return None;
        }
        p += cnt.types * 6 + cnt.len_rest(w);
        let rule = if wide {
            let f = b.get(p..)?;
            let body = f.strip_prefix(b"\n")?;
            let end = body.iter().position(|&c| c == b'\n')?;
            let s = std::str::from_utf8(&body[..end]).ok()?;
            if s.is_empty() { None } else { Some(Rule::parse(s)?) }
        } else {
            None
        };
        Some(Zone { at, ty, offs, rule })
    }

    /// The offset at instant `t`.
    pub fn offset(&self, t: i64) -> i32 {
        let n = self.at.partition_point(|&a| a <= t);
        // the footer is for instants after the last transition, and for all time in a file without transitions
        if n == self.at.len() && let Some(r) = self.rule && (n == 0 || t > self.at[n - 1]) {
            return r.offset(t);
        }
        if n == 0 { self.offs[0] } else { self.offs[self.ty[n - 1] as usize] }
    }

    /// The instant of wall-clock time `l` (seconds, as if local time were UTC). In an overlap (clocks go back) it is the first
    /// instant. In a gap (clocks go forward) it uses the offset from before the gap, as ECMA-262 says.
    pub fn to_utc(&self, l: i64) -> i64 {
        let (a, b) = (self.offset(l - 86_400) as i64, self.offset(l + 86_400) as i64);
        let ok = |o: i64| self.offset(l - o) as i64 == o;
        match (ok(a), ok(b)) {
            (true, true) => (l - a).min(l - b),
            (false, true) => l - b,
            _ => l - a,
        }
    }

    /// Heap bytes held by the zone.
    pub fn bytes(&self) -> usize {
        self.at.capacity() * 8 + self.ty.capacity() + self.offs.capacity() * 4
    }
}

struct Counts {
    isut: usize,
    isstd: usize,
    leap: usize,
    times: usize,
    types: usize,
    chars: usize,
}

impl Counts {
    /// Bytes of a data block with `w`-byte times.
    fn len(&self, w: usize) -> usize {
        self.times * (w + 1) + self.types * 6 + self.len_rest(w)
    }
    /// Bytes after the time types.
    fn len_rest(&self, w: usize) -> usize {
        self.chars + self.leap * (w + 4) + self.isstd + self.isut
    }
}

fn header(b: &[u8], p: usize) -> Option<(Counts, u8)> {
    let h = b.get(p..p + 44)?;
    if &h[..4] != b"TZif" || !matches!(h[4], 0 | b'2' | b'3' | b'4') {
        return None;
    }
    let n = |i: usize| u32::from_be_bytes(h[20 + 4 * i..24 + 4 * i].try_into().unwrap()) as usize;
    Some((Counts { isut: n(0), isstd: n(1), leap: n(2), times: n(3), types: n(4), chars: n(5) }, h[4]))
}

impl Rule {
    fn parse(s: &str) -> Option<Rule> {
        let b = s.as_bytes();
        let mut p = 0;
        name(b, &mut p)?;
        let std = -hms(b, &mut p, 24)?;
        if p == b.len() {
            return Some(Rule { std, dst: None });
        }
        name(b, &mut p)?;
        let dst = if b.get(p) == Some(&b',') { std + 3600 } else { -hms(b, &mut p, 24)? };
        if b.get(p) != Some(&b',') {
            return None;
        }
        p += 1;
        let start = when(b, &mut p)?;
        if b.get(p) != Some(&b',') {
            return None;
        }
        p += 1;
        let end = when(b, &mut p)?;
        (p == b.len()).then_some(Rule { std, dst: Some((dst, start, end)) })
    }

    fn offset(self, t: i64) -> i32 {
        let Some((dst, start, end)) = self.dst else { return self.std };
        // The year of `t` in standard time. A start or end is a local time: the start in standard time, the end in daylight time.
        let year = crate::date::civil_from_days((t + self.std as i64).div_euclid(86_400)).0;
        let s = start.local(year) - self.std as i64;
        let e = end.local(year) - dst as i64;
        let in_dst = if s < e { s <= t && t < e } else { !(e <= t && t < s) };
        if in_dst { dst } else { self.std }
    }
}

impl When {
    /// Seconds since the epoch of this moment in local time of `year`, as if local time were UTC.
    fn local(self, year: i64) -> i64 {
        let jan1 = days_from_civil(year, 1, 1);
        let day = match self.day {
            Day::Julian(n) => jan1 + n as i64 - 1 + (crate::date::is_leap(year) && n >= 60) as i64,
            Day::Zero(n) => jan1 + n as i64,
            Day::Month(m, w, d) => {
                let first = days_from_civil(year, m as i64, 1);
                let wd_first = (first + 4).rem_euclid(7); // 1970-01-01 was a Thursday
                let mut day = first + (d as i64 - wd_first).rem_euclid(7) + 7 * (w as i64 - 1);
                let last = first + crate::date::days_in_month(year, m as i64) - 1;
                while day > last {
                    day -= 7;
                }
                day
            }
        };
        day * 86_400 + self.time as i64
    }
}

/// A zone abbreviation: 3 or more letters, or `<...>` with letters, digits, `+` and `-`.
fn name(b: &[u8], p: &mut usize) -> Option<()> {
    let start = *p;
    if b.get(*p) == Some(&b'<') {
        let end = b[*p..].iter().position(|&c| c == b'>')? + *p;
        if !b[*p + 1..end].iter().all(|c| c.is_ascii_alphanumeric() || *c == b'+' || *c == b'-') {
            return None;
        }
        *p = end + 1;
        return (end - start > 3).then_some(());
    }
    while b.get(*p).is_some_and(|c| c.is_ascii_alphabetic()) {
        *p += 1;
    }
    (*p - start >= 3).then_some(())
}

/// `[+-]hh[:mm[:ss]]` in seconds, with hours up to `max_h`.
fn hms(b: &[u8], p: &mut usize, max_h: i32) -> Option<i32> {
    let neg = match b.get(*p) {
        Some(b'-') => { *p += 1; true }
        Some(b'+') => { *p += 1; false }
        _ => false,
    };
    let mut v = 0;
    for (i, lim) in [max_h, 59, 59].into_iter().enumerate() {
        if i > 0 {
            if b.get(*p) != Some(&b':') {
                break;
            }
            *p += 1;
        }
        let n = num(b, p, 3)?;
        if n > lim {
            return None;
        }
        v += n * [3600, 60, 1][i];
    }
    Some(if neg { -v } else { v })
}

fn num(b: &[u8], p: &mut usize, max_digits: usize) -> Option<i32> {
    let start = *p;
    let mut v = 0i32;
    while let Some(c) = b.get(*p).filter(|c| c.is_ascii_digit()) {
        if *p - start == max_digits {
            return None;
        }
        v = v * 10 + (c - b'0') as i32;
        *p += 1;
    }
    (*p > start).then_some(v)
}

fn when(b: &[u8], p: &mut usize) -> Option<When> {
    let day = match b.get(*p) {
        Some(b'J') => {
            *p += 1;
            let n = num(b, p, 3)?;
            if !(1..=365).contains(&n) { return None; }
            Day::Julian(n as u16)
        }
        Some(b'M') => {
            *p += 1;
            let m = num(b, p, 2)?;
            if b.get(*p) != Some(&b'.') { return None; }
            *p += 1;
            let w = num(b, p, 1)?;
            if b.get(*p) != Some(&b'.') { return None; }
            *p += 1;
            let d = num(b, p, 1)?;
            if !(1..=12).contains(&m) || !(1..=5).contains(&w) || d > 6 { return None; }
            Day::Month(m as u8, w as u8, d as u8)
        }
        _ => {
            let n = num(b, p, 3)?;
            if n > 365 { return None; }
            Day::Zero(n as u16)
        }
    };
    let time = if b.get(*p) == Some(&b'/') {
        *p += 1;
        hms(b, p, 167)?
    } else {
        7200
    };
    Some(When { day, time })
}

/// The offset of an ECMA-262 offset time zone name: `+HH`, `+HHMM` or `+HH:MM` (or `-`), hours to 23. Seconds are refused,
/// as V8 does.
pub fn parse_offset_name(s: &str) -> Option<i32> {
    let b = s.as_bytes();
    let sign = match b.first()? { b'+' => 1, b'-' => -1, _ => return None };
    let d = |i: usize| b.get(i).filter(|c| c.is_ascii_digit()).map(|c| (c - b'0') as i32);
    let h = d(1)? * 10 + d(2)?;
    let m = match b.len() {
        3 => 0,
        5 => d(3)? * 10 + d(4)?,
        6 if b[3] == b':' => d(4)? * 10 + d(5)?,
        _ => return None,
    };
    (h <= 23 && m <= 59).then_some(sign * (h * 3600 + m * 60))
}

/// True for the names that ECMA-402 makes the zone `UTC` (compared without case).
pub fn is_utc_name(s: &str) -> bool {
    const UTC: [&str; 18] = [
        "UTC", "Etc/UTC", "Etc/GMT", "GMT", "Etc/UCT", "UCT", "Etc/Universal", "Universal", "Etc/Zulu", "Zulu", "Etc/GMT0", "GMT0", "Etc/GMT+0",
        "Etc/GMT-0", "GMT+0", "GMT-0", "Etc/Greenwich", "Greenwich",
    ];
    UTC.iter().any(|u| u.eq_ignore_ascii_case(s))
}

/// A zone name that may name a file: ASCII letters, digits, `_`, `-`, `+` and `/` between non-empty parts, no `.`, at most
/// 64 bytes, not under `posix/` or `right/`, and not a tzdata file that is not a zone.
pub fn is_file_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.split('/').all(|p| !p.is_empty() && p.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'+')))
        && !s.starts_with("posix/")
        && !s.starts_with("right/")
        && !matches!(s, "posixrules" | "Factory" | "SECURITY" | "leapseconds")
}

#[cfg(test)]
#[path = "tz_tests.rs"]
mod tests;
