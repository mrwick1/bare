//! Date arithmetic, UTC only (ECMA-262 section 21.4.1). No heap, no clock, no time zone.
//!
//! A time value is an `f64`: whole milliseconds since 1970-01-01T00:00:00Z, within +-8.64e15, or NaN. Every function
//! here is total: bad input gives NaN or `None`, never a panic. The day arithmetic uses `i64` and the proleptic
//! Gregorian calendar, so a leap day follows the rule of section 21.4.1.3 (every 4th year, except 100th, except 400th).
//!
//! Text forms
//! - `parse` reads the Date Time String Format (section 21.4.1.32): `YYYY`, `YYYY-MM`, `YYYY-MM-DD`, each with an
//!   optional `THH:mm[:ss[.s+]]` and then `Z` or `+HH:mm` / `-HH:mm`; years `+YYYYYY` and `-YYYYYY` (`-000000` is invalid).
//!   `24:00:00` is the end of the day. A fraction may have any number of digits: the first three count (as V8 does).
//!   A form with a time and no offset is local time: it is `Parsed::Local`, never a guessed UTC value.
//! - `parse` also reads the `toUTCString` form: `Www, DD Mon YYYY HH:mm:ss GMT`.
//! - A string that has no ASCII digit is NaN. A string with a digit in any other shape is `Parsed::Unrecognized`
//!   (the legacy formats of V8 are not built; see `docs/resources.md`).
//! - A field out of range (month 13, 30 February, 25:00) is NaN: the standard says an illegal value is not a valid
//!   instance of the format. V8 accepts some of these (`2021-02-29`).

pub const MS_PER_DAY: f64 = 86_400_000.0;
/// Largest absolute time value (section 21.4.1.1).
pub const MAX_TIME: f64 = 8.64e15;

/// TimeClip (section 21.4.1.31): NaN outside +-8.64e15 and for non-finite input; else the integer part, with +0 for -0.
pub fn time_clip(t: f64) -> f64 {
    if !t.is_finite() || t.abs() > MAX_TIME { f64::NAN } else { t.trunc() + 0.0 }
}

/// Days since the epoch of the civil date (`m` 1..=12). Hinnant's algorithm, exact in `i64`.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = y - (m <= 2) as i64;
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The civil date `(year, month 1..=12, day 1..=31)` of a day number.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + (m <= 2) as i64, m, d)
}

pub fn is_leap(y: i64) -> bool {
    y % 4 == 0 && (y % 100 != 0 || y % 400 == 0)
}

pub fn days_in_month(y: i64, m: i64) -> i64 {
    match m {
        2 => 28 + is_leap(y) as i64,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// The calendar fields of a time value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fields {
    pub year: i64,
    /// 0..=11
    pub month: i64,
    /// 1..=31
    pub day: i64,
    pub hour: i64,
    pub minute: i64,
    pub second: i64,
    pub ms: i64,
    /// 0 is Sunday.
    pub weekday: i64,
}

/// The fields of time value `t`. `None` for NaN (and for anything that is not a valid time value).
pub fn fields(t: f64) -> Option<Fields> {
    if !t.is_finite() || t.abs() > MAX_TIME {
        return None;
    }
    // |t| <= 8.64e15 < 2^53, so the integer part is exact.
    let t = t as i64;
    let day = t.div_euclid(86_400_000);
    let tod = t.rem_euclid(86_400_000);
    let (year, m, d) = civil_from_days(day);
    Some(Fields { year, month: m - 1, day: d, hour: tod / 3_600_000, minute: tod / 60_000 % 60, second: tod / 1000 % 60, ms: tod % 1000, weekday: (day + 4).rem_euclid(7) })
}

/// MakeTime (section 21.4.1.27): any non-finite argument gives NaN. Each argument is cut toward zero first.
pub fn make_time(h: f64, m: f64, s: f64, ms: f64) -> f64 {
    if !(h.is_finite() && m.is_finite() && s.is_finite() && ms.is_finite()) {
        return f64::NAN;
    }
    h.trunc() * 3_600_000.0 + m.trunc() * 60_000.0 + s.trunc() * 1000.0 + ms.trunc()
}

/// MakeDay (section 21.4.1.28): the day number of year `y`, month `m` (0 is January, any integer) and day `d`.
/// A year beyond +-1,000,000 or a month beyond +-10,000,000 is NaN, as in V8.
/// This conservative bound can also reject oversized fields that would cancel to a supported date.
pub fn make_day(y: f64, m: f64, d: f64) -> f64 {
    if !(y.is_finite() && m.is_finite() && d.is_finite()) {
        return f64::NAN;
    }
    let (y, m, d) = (y.trunc(), m.trunc(), d.trunc());
    if y.abs() > 1e6 || m.abs() > 1e7 {
        return f64::NAN;
    }
    let ym = y as i64 + (m as i64).div_euclid(12);
    let mn = (m as i64).rem_euclid(12);
    days_from_civil(ym, mn + 1, 1) as f64 + d - 1.0
}

/// MakeDate (section 21.4.1.29).
pub fn make_date(day: f64, time: f64) -> f64 {
    if !(day.is_finite() && time.is_finite()) {
        return f64::NAN;
    }
    let tv = day * MS_PER_DAY + time;
    if tv.is_finite() { tv } else { f64::NAN }
}

/// `TimeWithinDay(t)` for a finite `t`.
pub fn time_within_day(t: f64) -> f64 {
    t.rem_euclid(MS_PER_DAY)
}

/// `Day(t)` for a finite `t`.
pub fn day(t: f64) -> f64 {
    (t / MS_PER_DAY).floor()
}

// ------------------------------------------------------------------------------------------ format

const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// `toISOString` text of a valid time value: `YYYY-MM-DDTHH:mm:ss.sssZ`, or `+YYYYYY` / `-YYYYYY` outside 0..=9999.
pub fn to_iso(t: f64) -> Option<String> {
    let f = fields(t)?;
    let year = if (0..=9999).contains(&f.year) { format!("{:04}", f.year) } else { format!("{}{:06}", if f.year < 0 { '-' } else { '+' }, f.year.abs()) };
    Some(format!("{year}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z", f.month + 1, f.day, f.hour, f.minute, f.second, f.ms))
}

/// `toUTCString` text: `Www, DD Mon YYYY HH:mm:ss GMT`. The year has at least 4 digits and a `-` when negative.
pub fn to_utc_string(t: f64) -> Option<String> {
    let f = fields(t)?;
    let sign = if f.year < 0 { "-" } else { "" };
    Some(format!("{}, {:02} {} {sign}{:04} {:02}:{:02}:{:02} GMT", DAYS[f.weekday as usize], f.day, MONTHS[f.month as usize], f.year.abs(), f.hour, f.minute, f.second))
}

// ------------------------------------------------------------------------------------------ parse

/// The result of reading a date string.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Parsed {
    /// A time value (already clipped; NaN when the text is not a valid date).
    Time(f64),
    /// A valid ISO date-time with no offset: it means local time, and bare has no time zone.
    Local,
    /// Digits in a shape that is not built (a legacy format of V8, `2020-1-1`, a lower-case `t`, `+0100`).
    Unrecognized,
}

struct Cur<'a> {
    s: &'a [u16],
    i: usize,
}

impl Cur<'_> {
    fn peek(&self) -> Option<u16> {
        self.s.get(self.i).copied()
    }
    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(c as u16) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    /// Exactly `n` ASCII digits.
    fn num(&mut self, n: usize) -> Option<i64> {
        let part = self.s.get(self.i..self.i + n)?;
        let mut v = 0i64;
        for &u in part {
            if !(b'0' as u16..=b'9' as u16).contains(&u) {
                return None;
            }
            v = v * 10 + (u - b'0' as u16) as i64;
        }
        self.i += n;
        Some(v)
    }
    fn done(&self) -> bool {
        self.i == self.s.len()
    }
}

/// Read a date string. Never allocates and reads each unit a bounded number of times: linear in the length.
pub fn parse(s: &[u16]) -> Parsed {
    if !s.iter().any(|&u| (b'0' as u16..=b'9' as u16).contains(&u)) {
        return Parsed::Time(f64::NAN);
    }
    parse_iso(s).or_else(|| parse_utc_string(s)).unwrap_or(Parsed::Unrecognized)
}

fn parse_iso(s: &[u16]) -> Option<Parsed> {
    let mut c = Cur { s, i: 0 };
    let (year, neg_zero) = match c.peek() {
        Some(u) if u == b'+' as u16 || u == b'-' as u16 => {
            c.i += 1;
            let y = c.num(6)?;
            if u == b'-' as u16 { (-y, y == 0) } else { (y, false) }
        }
        _ => (c.num(4)?, false),
    };
    let (mut month, mut day) = (1, 1);
    if c.eat('-') {
        month = c.num(2)?;
        if c.eat('-') {
            day = c.num(2)?;
        }
    }
    let (mut h, mut mi, mut sec, mut ms) = (0, 0, 0, 0);
    let mut has_time = false;
    let mut offset: Option<i64> = None;
    if c.eat('T') {
        has_time = true;
        h = c.num(2)?;
        if !c.eat(':') {
            return None;
        }
        mi = c.num(2)?;
        if c.eat(':') {
            sec = c.num(2)?;
            if c.eat('.') {
                let start = c.i;
                while matches!(c.peek(), Some(u) if (b'0' as u16..=b'9' as u16).contains(&u)) {
                    if c.i - start < 3 {
                        ms = ms * 10 + (c.s[c.i] - b'0' as u16) as i64;
                    }
                    c.i += 1;
                }
                match c.i - start {
                    0 => return None,
                    n @ 1..=2 => ms *= if n == 1 { 100 } else { 10 },
                    _ => {}
                }
            }
        }
        if c.eat('Z') {
            offset = Some(0);
        } else if let Some(u) = c.peek().filter(|&u| u == b'+' as u16 || u == b'-' as u16) {
            c.i += 1;
            let (oh, om) = (c.num(2)?, if c.eat(':') { c.num(2)? } else { return None });
            offset = Some(if u == b'-' as u16 { -(oh * 60 + om) } else { oh * 60 + om });
            if oh > 23 || om > 59 {
                return Some(Parsed::Time(f64::NAN));
            }
        }
    }
    if !c.done() {
        return None;
    }
    if has_time && offset.is_none() {
        return Some(Parsed::Local);
    }
    let end_of_day = h == 24 && mi == 0 && sec == 0 && ms == 0;
    let valid = !neg_zero && (1..=12).contains(&month) && (1..=days_in_month(year, month)).contains(&day) && (h < 24 || end_of_day) && mi < 60 && sec < 60;
    if !valid {
        return Some(Parsed::Time(f64::NAN));
    }
    let t = make_date(make_day(year as f64, (month - 1) as f64, day as f64), make_time(h as f64, mi as f64, sec as f64, ms as f64));
    Some(Parsed::Time(time_clip(t - offset.unwrap_or(0) as f64 * 60_000.0)))
}

/// `Www, DD Mon YYYY HH:mm:ss GMT`. The weekday name is read and not checked, as V8 does.
fn parse_utc_string(s: &[u16]) -> Option<Parsed> {
    let mut c = Cur { s, i: 0 };
    let word = |c: &mut Cur, names: &[&str]| -> Option<usize> {
        let p = c.s.get(c.i..c.i + 3)?;
        let k = names.iter().position(|n| n.bytes().zip(p).all(|(a, &b)| a as u16 == b))?;
        c.i += 3;
        Some(k)
    };
    word(&mut c, &DAYS)?;
    if !(c.eat(',') && c.eat(' ')) {
        return None;
    }
    let day = c.num(2)?;
    if !c.eat(' ') {
        return None;
    }
    let month = word(&mut c, &MONTHS)? as i64 + 1;
    if !c.eat(' ') {
        return None;
    }
    let neg = c.eat('-');
    let start = c.i;
    let mut year = 0i64;
    while matches!(c.peek(), Some(u) if (b'0' as u16..=b'9' as u16).contains(&u)) && c.i - start < 7 {
        year = year * 10 + (c.s[c.i] - b'0' as u16) as i64;
        c.i += 1;
    }
    if c.i - start < 4 || !c.eat(' ') {
        return None;
    }
    let year = if neg { -year } else { year };
    let (h, mi) = (c.num(2)?, if c.eat(':') { c.num(2)? } else { return None });
    let sec = if c.eat(':') { c.num(2)? } else { return None };
    if !(c.eat(' ') && c.eat('G') && c.eat('M') && c.eat('T') && c.done()) {
        return None;
    }
    if !(1..=days_in_month(year, month)).contains(&day) || h > 23 || mi > 59 || sec > 59 {
        return Some(Parsed::Time(f64::NAN));
    }
    Some(Parsed::Time(time_clip(make_date(make_day(year as f64, (month - 1) as f64, day as f64), make_time(h as f64, mi as f64, sec as f64, 0.0)))))
}
