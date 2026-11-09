//! Date tests. The corpus (`tools/fixtures/js/date-cases.txt`) is checked against what Node gave (`date-cases.expected`,
//! made with three different `TZ` values: the output is the same; `tools/js/date_check.sh` re-runs Node). The tests below cover
//! what Node cannot show: the pure calendar arithmetic, the clock, gates, deviations from V8, caps and the collector.

use super::*;
use crate::date;
use crate::heap::Heap;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\n', "\\n").replace('\r', "\\r").replace('\t', "\\t")
}

/// The format of `tools/js/eval_oracle.mjs`.
fn show(it: &mut Interp, r: Result<Value, Error>) -> String {
    match r {
        Ok(v @ Value::String(_)) => format!("string:{}", esc(&it.display(v))),
        Ok(v @ Value::Number(_)) => format!("number:{}", it.display(v)),
        Ok(v @ Value::Bool(_)) => format!("boolean:{}", it.display(v)),
        Ok(Value::Undefined) => "undefined".into(),
        Ok(Value::Null) => "null".into(),
        Ok(_) => "object".into(),
        Err(Error::Throw(v)) if matches!(v, Value::Object(_)) => format!("throw:{}:{}", esc(&it.get_str(v, "name")), esc(&it.get_str(v, "message"))),
        Err(Error::Throw(v)) => format!("throw-value:{}", esc(&it.display(v))),
        Err(Error::Unsupported(u)) => format!("unsupported:{}", u.what),
        Err(Error::Limit(k)) => format!("limit:{k:?}"),
        Err(e) => format!("internal-error:{e:?}"),
    }
}

fn out(it: &mut Interp, src: &str) -> String {
    let r = it.run_script(src);
    show(it, r)
}

fn realm() -> Interp<'static> {
    let mut it = Interp::realm(Heap::default(), EvalLimits { max_steps: 50_000_000, ..Default::default() }).unwrap();
    // no String.prototype.repeat yet
    it.run_script("function rep(c, n) { var s = ''; for (var i = 0; i < n; i++) s += c; return s; }").unwrap();
    it
}

fn virt(ms: f64) -> Interp<'static> {
    let mut it = realm();
    it.set_wall_clock(WallClock::Virtual(ms)).unwrap();
    it
}

#[test]
fn corpus_matches_node() {
    big(|| {
        let want: Vec<&str> = include_str!("../../../tools/fixtures/js/date-cases.expected").lines().collect();
        let mut cases: Vec<(&str, Vec<&str>)> = Vec::new();
        for line in include_str!("../../../tools/fixtures/js/date-cases.txt").lines() {
            if let Some(n) = line.strip_prefix("=== ") {
                cases.push((n.trim(), Vec::new()));
            } else if let Some(c) = cases.last_mut() {
                c.1.push(line);
            }
        }
        assert_eq!(cases.len(), want.len());
        let mut bad = Vec::new();
        for ((name, lines), w) in cases.iter().zip(want) {
            let mut it = realm();
            let got = format!("{name}\t{}", out(&mut it, &lines.join("\n")));
            if got != w {
                bad.push(format!("node: {w}\nbare: {got}"));
            }
        }
        assert!(bad.is_empty(), "{} differences:\n{}", bad.len(), bad.join("\n"));
    });
}

// ------------------------------------------------------------------------------------------ pure arithmetic

#[test]
fn every_day_round_trips_through_the_calendar() {
    // Walk 2,000,000 days in order (about 5,470 years around 1970): each day follows the one before, the weekday cycles,
    // and the fields give the same day number again.
    let (mut y, mut m, mut d, mut wd) = (1970 - 2735, 0i64, 1i64, None);
    let start = date::make_day(y as f64, 0.0, 1.0);
    for n in 0..2_000_000i64 {
        let t = (start + n as f64) * date::MS_PER_DAY;
        let f = date::fields(t).unwrap();
        assert_eq!((f.year, f.month, f.day), (y, m, d), "day {n}");
        assert_eq!(date::make_day(f.year as f64, f.month as f64, f.day as f64), start + n as f64);
        if let Some(w) = wd {
            assert_eq!(f.weekday, (w + 1) % 7);
        }
        wd = Some(f.weekday);
        d += 1;
        if d > date::days_in_month(y, m + 1) {
            d = 1;
            m += 1;
            if m == 12 {
                m = 0;
                y += 1;
            }
        }
    }
}

#[test]
fn known_dates_and_weekdays() {
    let f = date::fields(0.0).unwrap();
    assert_eq!((f.year, f.month, f.day, f.weekday), (1970, 0, 1, 4));
    let f = date::fields(951_782_400_000.0).unwrap(); // 2000-02-29, a Tuesday
    assert_eq!((f.year, f.month, f.day, f.weekday), (2000, 1, 29, 2));
    let f = date::fields(-62_198_755_200_000.0).unwrap(); // year -1, 1 January, a Friday
    assert_eq!((f.year, f.month, f.day, f.weekday), (-1, 0, 1, 5));
    let f = date::fields(8.64e15).unwrap();
    assert_eq!((f.year, f.month, f.day, f.weekday), (275_760, 8, 13, 6));
    let f = date::fields(-8.64e15).unwrap();
    assert_eq!((f.year, f.month, f.day, f.weekday), (-271_821, 3, 20, 2));
    assert!(date::fields(f64::NAN).is_none() && date::fields(8.64e15 + 1.0).is_none() && date::fields(f64::INFINITY).is_none());
}

#[test]
fn leap_year_rule() {
    for (y, leap) in [(1900, false), (2000, true), (2100, false), (2400, true), (2023, false), (2024, true), (0, true), (-4, true), (-100, false), (-400, true), (1, false)] {
        assert_eq!(date::is_leap(y), leap, "{y}");
        assert_eq!(date::days_in_month(y, 2), if leap { 29 } else { 28 });
    }
    // 400 years hold 146,097 days
    assert_eq!(date::make_day(2400.0, 0.0, 1.0) - date::make_day(2000.0, 0.0, 1.0), 146_097.0);
}

#[test]
fn time_clip_make_day_make_time_edges() {
    assert!(date::time_clip(f64::NAN).is_nan() && date::time_clip(f64::INFINITY).is_nan() && date::time_clip(8.64e15 + 1.0).is_nan());
    assert_eq!(date::time_clip(8.64e15), 8.64e15);
    assert_eq!(date::time_clip(-8.64e15), -8.64e15);
    assert_eq!(date::time_clip(1.9), 1.0);
    assert_eq!(date::time_clip(-1.9), -1.0);
    assert!(date::time_clip(-0.0).is_sign_positive());
    assert!(date::make_day(f64::NAN, 0.0, 1.0).is_nan() && date::make_day(2020.0, f64::INFINITY, 1.0).is_nan());
    assert!(date::make_day(1_000_001.0, 0.0, 1.0).is_nan() && date::make_day(0.0, 10_000_001.0, 1.0).is_nan());
    assert!(date::make_time(1.0, f64::NAN, 0.0, 0.0).is_nan());
    assert_eq!(date::make_time(1.9, 1.9, 1.9, 1.9), 3_600_000.0 + 60_000.0 + 1000.0 + 1.0);
    assert!(date::make_date(1e305, 0.0).is_nan());
    // the year of a far month: 12 * 10 months after January 2000 is January 2010
    assert_eq!(date::make_day(2000.0, 120.0, 1.0), date::make_day(2010.0, 0.0, 1.0));
    assert_eq!(date::make_day(2000.0, -1.0, 1.0), date::make_day(1999.0, 11.0, 1.0));
}

#[test]
fn text_forms_round_trip_over_the_whole_range() {
    let mut t = -8.64e15;
    let mut n = 0;
    while t <= 8.64e15 {
        let iso = date::to_iso(t).unwrap();
        assert_eq!(date::parse(&iso.encode_utf16().collect::<Vec<_>>()), date::Parsed::Time(t), "{iso}");
        let utc = date::to_utc_string(t).unwrap();
        let back = match date::parse(&utc.encode_utf16().collect::<Vec<_>>()) {
            date::Parsed::Time(b) => b,
            p => panic!("{utc}: {p:?}"),
        };
        assert_eq!(back, (t / 1000.0).floor() * 1000.0, "{utc}");
        t += 8.64e15 / 4000.0 + 12_345.0;
        n += 1;
    }
    assert!(n > 3900);
}

#[test]
fn parse_classification() {
    let p = |s: &str| date::parse(&s.encode_utf16().collect::<Vec<_>>());
    let nan = |p: date::Parsed| matches!(p, date::Parsed::Time(t) if t.is_nan());
    assert_eq!(p("2020-01-01T00:00:00Z"), date::Parsed::Time(1_577_836_800_000.0));
    assert_eq!(p("2020-01-01T00:00:00"), date::Parsed::Local(1_577_836_800_000.0));
    assert_eq!(p("2020-01-01T00:00"), date::Parsed::Local(1_577_836_800_000.0));
    assert!(nan(p("2020-02-30T00:00")));
    assert_eq!(p("2020-01-01"), date::Parsed::Time(1_577_836_800_000.0));
    // a legacy form is local time without a zone, and UTC-based with one
    assert_eq!(p("Jan 1 2020"), date::Parsed::Local(1_577_836_800_000.0));
    assert_eq!(p("1/2/2020 10:00 GMT+0100"), date::Parsed::Time(1_577_955_600_000.0));
    for s in ["20200101", "2020-01-01T00:00:00.Z", "1/2/2020 xyz", "Jan 1 2020 Mon"] {
        assert!(nan(p(s)), "{s}");
    }
    for s in ["", " ", "abc", "undefined", "Tue", "T", "Z", "NaN"] {
        assert!(nan(p(s)), "{s}");
    }
    // out of range inside a known format is NaN, as the standard says (V8 accepts the 30 February forms)
    for s in ["2020-02-30", "2021-02-29", "2020-06-31", "2020-01-01T24:00:01Z", "2020-01-01T24:01Z", "Sat, 30 Feb 2020 00:00:00 GMT", "Tue, 01 Jan 2020 24:00:00 GMT", "2020-01-01T00:00:00+23:60"] {
        assert!(nan(p(s)), "{s}");
    }
    // a fraction keeps three digits and drops the rest
    assert_eq!(p("1970-01-01T00:00:00.9999999999999999999Z"), date::Parsed::Time(999.0));
    assert_eq!(p("1970-01-01T00:00:00.05Z"), date::Parsed::Time(50.0));
}

// ------------------------------------------------------------------------------------------ the clock

#[test]
fn now_reads_the_virtual_wall_clock_and_new_date_agrees() {
    big(|| {
        let mut it = virt(1_700_000_000_123.0);
        assert_eq!(out(&mut it, "[Date.now(), new Date().getTime(), new Date().toISOString()].join()"), "string:1700000000123,1700000000123,2023-11-14T22:13:20.123Z");
        assert_eq!(it.wall_clock(), WallClock::Virtual(1_700_000_000_123.0));
        assert_eq!(it.wall_now_ms(), 1_700_000_000_123.0);
        // the clock stays where it is, however much script runs
        assert_eq!(out(&mut it, "var s = 0; for (var i = 0; i < 20000; i++) s += i; Date.now()"), "number:1700000000123");
    });
}

#[test]
fn the_virtual_wall_clock_may_move_backward() {
    big(|| {
        let mut it = virt(1000.0);
        assert_eq!(out(&mut it, "Date.now()"), "number:1000");
        it.set_wall_clock(WallClock::Virtual(500.0)).unwrap();
        assert_eq!(out(&mut it, "Date.now()"), "number:500");
        it.set_wall_clock(WallClock::Virtual(-1.0)).unwrap();
        assert_eq!(out(&mut it, "[Date.now(), new Date().toISOString()].join()"), "string:-1,1969-12-31T23:59:59.999Z");
        // a Date that was made earlier keeps its own time value
        let mut it = virt(10.0);
        out(&mut it, "var d = new Date();");
        it.set_wall_clock(WallClock::Virtual(5.0)).unwrap();
        assert_eq!(out(&mut it, "[d.getTime(), Date.now(), Date.now() - d.getTime()].join()"), "string:10,5,-5");
    });
}

#[test]
fn a_bad_virtual_clock_is_refused_and_changes_nothing() {
    let mut it = virt(42.0);
    for bad in [f64::NAN, f64::INFINITY, 1.5, 8.64e15 + 1.0, -8.64e15 - 1.0] {
        assert!(matches!(it.set_wall_clock(WallClock::Virtual(bad)), Err(Error::Unsupported(_))), "{bad}");
        assert_eq!(it.wall_clock(), WallClock::Virtual(42.0));
    }
    assert!(it.set_wall_clock(WallClock::Virtual(8.64e15)).is_ok());
    assert!(it.set_wall_clock(WallClock::Virtual(-8.64e15)).is_ok());
}

#[test]
fn the_system_clock_is_the_default_and_is_a_real_epoch_time() {
    big(|| {
        let mut it = realm();
        assert_eq!(it.wall_clock(), WallClock::System);
        let before = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as f64;
        let Value::Number(n) = it.run_script("Date.now()").unwrap() else { panic!() };
        let after = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as f64;
        assert!(before <= n && n <= after, "{before} {n} {after}");
        // a Unix epoch time in milliseconds is about 1.7e12 now. A process uptime clock would be a few thousand.
        assert!(n > 1.7e12 && n < 4e12);
        it.set_wall_clock(WallClock::Virtual(7.0)).unwrap();
        it.set_wall_clock(WallClock::System).unwrap();
        let Value::Number(m) = it.run_script("Date.now()").unwrap() else { panic!() };
        assert!(m > 1.7e12);
    });
}

#[test]
fn an_out_of_range_system_clock_cannot_make_an_invalid_date_object() {
    // `new Date()` clips the clock, so even a broken clock reads as NaN and never as a wrong number.
    assert!(date::time_clip(9e15).is_nan());
}

// ------------------------------------------------------------------------------------------ gates

/// What node gave under each `TZ` (the expression, then the result in the format of `show`). Node writes a long zone name in
/// parentheses after `GMT+hhmm`. bare writes `GMT+hh:mm` there, so the text from ` (` on is not compared.
fn local_time_cases() -> Vec<(&'static str, &'static [(&'static str, &'static str)])> {
    vec![
        ("UTC", &[
            ("new Date(0).toString()", "string:Thu Jan 01 1970 00:00:00 GMT+0000 (Coordinated Universal Time)"),
            ("new Date(0).toDateString()", "string:Thu Jan 01 1970"),
            ("new Date(0).toTimeString()", "string:00:00:00 GMT+0000 (Coordinated Universal Time)"),
            ("[new Date(1700000000000).getHours(),new Date(1700000000000).getTimezoneOffset(),new Date(1700000000000).getDay(),new Date(1700000000000).getYear(),new Date(1700000000000).getDate(),new Date(1700000000000).getMonth(),new Date(1700000000000).getFullYear(),new Date(1700000000000).getMinutes(),new Date(1700000000000).getSeconds(),new Date(1700000000000).getMilliseconds()].join()", "string:22,0,2,123,14,10,2023,13,20,0"),
            ("[new Date(2020,0,31,10,20,30,400).getTime(),new Date(2020,6,1).getTime(),new Date(99,0).getFullYear(),new Date(2020,0).getTime()].join()", "string:1580466030400,1593561600000,1999,1577836800000"),
            ("[new Date('2020-07-01T12:00:00').getTime(),Date.parse('2020-01-01T00:00'),Date.parse('2020-01-01T00:00:00.5'),Date.parse('2020-13-01T00:00')].join()", "string:1593604800000,1577836800000,1577836800500,NaN"),
            ("[new Date(2020,2,8,2,30).getTime(),new Date(2020,10,1,1,30).getTime(),new Date(2020,2,29,1,30).getTime(),new Date(2020,9,25,1,30).getTime()].join()", "string:1583634600000,1604194200000,1585445400000,1603589400000"),
            ("new Date(2020,2,8,2,30).toString()", "string:Sun Mar 08 2020 02:30:00 GMT+0000 (Coordinated Universal Time)"),
            ("var d=new Date(1700000000000);d.setHours(30);d.getTime()", "number:1700028800000"),
            ("var d=new Date(1700000000000);d.setMonth(5,15);d.getTime()", "number:1686867200000"),
            ("var d=new Date(1700000000000);d.setFullYear(2000);d.getTime()", "number:974240000000"),
            ("var d=new Date(1700000000000);d.setYear(99);d.getTime()", "number:942617600000"),
            ("var d=new Date(NaN);d.setFullYear(2000);d.getTime()", "number:946684800000"),
            ("var d=new Date(1700000000000);d.setDate(0);d.getTime()", "number:1698790400000"),
            ("var d=new Date(1700000000000);d.setMinutes(61,1,1);d.getTime()", "number:1700002861001"),
            ("var d=new Date(1700000000000);d.setSeconds(5);d.getTime()", "number:1699999985000"),
            ("var d=new Date(1700000000000);d.setMilliseconds(5);d.getTime()", "number:1700000000005"),
            ("var d=new Date(1700000000000);[d.setHours(1,2,3,4),d.getTime()].join()", "string:1699923723004,1699923723004"),
            ("var d=new Date(1700000000000);d.setYear(NaN)+','+d.getTime()", "string:NaN,NaN"),
            ("new Date(1e12).toTimeString()", "string:01:46:40 GMT+0000 (Coordinated Universal Time)"),
            ("'' + new Date(0)", "string:Thu Jan 01 1970 00:00:00 GMT+0000 (Coordinated Universal Time)"),
            ("typeof Date()", "string:string"),
            ("new Date(NaN).setHours(1)", "number:NaN"),
            ("new Date(NaN).setYear(2000)", "number:946684800000"),
            ("new Date(1700000000000).toLocaleString('en-US')", "string:11/14/2023, 10:13:20 PM"),
            ("new Date(1700000000000).toLocaleDateString('en-US')", "string:11/14/2023"),
            ("new Date(1700000000000).toLocaleTimeString('en-US')", "string:10:13:20 PM"),
            ("[new Date(2100,6,1).getTimezoneOffset(),new Date(2100,0,1).getTimezoneOffset(),new Date(2030,5,1,12).toString()].join('|')", "string:0|0|Sat Jun 01 2030 12:00:00 GMT+0000 (Coordinated Universal Time)"),
        ]),
        ("Asia/Kolkata", &[
            ("new Date(0).toString()", "string:Thu Jan 01 1970 05:30:00 GMT+0530 (India Standard Time)"),
            ("new Date(0).toDateString()", "string:Thu Jan 01 1970"),
            ("new Date(0).toTimeString()", "string:05:30:00 GMT+0530 (India Standard Time)"),
            ("[new Date(1700000000000).getHours(),new Date(1700000000000).getTimezoneOffset(),new Date(1700000000000).getDay(),new Date(1700000000000).getYear(),new Date(1700000000000).getDate(),new Date(1700000000000).getMonth(),new Date(1700000000000).getFullYear(),new Date(1700000000000).getMinutes(),new Date(1700000000000).getSeconds(),new Date(1700000000000).getMilliseconds()].join()", "string:3,-330,3,123,15,10,2023,43,20,0"),
            ("[new Date(2020,0,31,10,20,30,400).getTime(),new Date(2020,6,1).getTime(),new Date(99,0).getFullYear(),new Date(2020,0).getTime()].join()", "string:1580446230400,1593541800000,1999,1577817000000"),
            ("[new Date('2020-07-01T12:00:00').getTime(),Date.parse('2020-01-01T00:00'),Date.parse('2020-01-01T00:00:00.5'),Date.parse('2020-13-01T00:00')].join()", "string:1593585000000,1577817000000,1577817000500,NaN"),
            ("[new Date(2020,2,8,2,30).getTime(),new Date(2020,10,1,1,30).getTime(),new Date(2020,2,29,1,30).getTime(),new Date(2020,9,25,1,30).getTime()].join()", "string:1583614800000,1604174400000,1585425600000,1603569600000"),
            ("new Date(2020,2,8,2,30).toString()", "string:Sun Mar 08 2020 02:30:00 GMT+0530 (India Standard Time)"),
            ("var d=new Date(1700000000000);d.setHours(30);d.getTime()", "number:1700097200000"),
            ("var d=new Date(1700000000000);d.setMonth(5,15);d.getTime()", "number:1686780800000"),
            ("var d=new Date(1700000000000);d.setFullYear(2000);d.getTime()", "number:974240000000"),
            ("var d=new Date(1700000000000);d.setYear(99);d.getTime()", "number:942617600000"),
            ("var d=new Date(NaN);d.setFullYear(2000);d.getTime()", "number:946665000000"),
            ("var d=new Date(1700000000000);d.setDate(0);d.getTime()", "number:1698704000000"),
            ("var d=new Date(1700000000000);d.setMinutes(61,1,1);d.getTime()", "number:1700001061001"),
            ("var d=new Date(1700000000000);d.setSeconds(5);d.getTime()", "number:1699999985000"),
            ("var d=new Date(1700000000000);d.setMilliseconds(5);d.getTime()", "number:1700000000005"),
            ("var d=new Date(1700000000000);[d.setHours(1,2,3,4),d.getTime()].join()", "string:1699990323004,1699990323004"),
            ("var d=new Date(1700000000000);d.setYear(NaN)+','+d.getTime()", "string:NaN,NaN"),
            ("new Date(1e12).toTimeString()", "string:07:16:40 GMT+0530 (India Standard Time)"),
            ("'' + new Date(0)", "string:Thu Jan 01 1970 05:30:00 GMT+0530 (India Standard Time)"),
            ("typeof Date()", "string:string"),
            ("new Date(NaN).setHours(1)", "number:NaN"),
            ("new Date(NaN).setYear(2000)", "number:946665000000"),
            ("new Date(1700000000000).toLocaleString('en-US')", "string:11/15/2023, 3:43:20 AM"),
            ("new Date(1700000000000).toLocaleDateString('en-US')", "string:11/15/2023"),
            ("new Date(1700000000000).toLocaleTimeString('en-US')", "string:3:43:20 AM"),
            ("[new Date(2100,6,1).getTimezoneOffset(),new Date(2100,0,1).getTimezoneOffset(),new Date(2030,5,1,12).toString()].join('|')", "string:-330|-330|Sat Jun 01 2030 12:00:00 GMT+0530 (India Standard Time)"),
        ]),
        ("America/New_York", &[
            ("new Date(0).toString()", "string:Wed Dec 31 1969 19:00:00 GMT-0500 (Eastern Standard Time)"),
            ("new Date(0).toDateString()", "string:Wed Dec 31 1969"),
            ("new Date(0).toTimeString()", "string:19:00:00 GMT-0500 (Eastern Standard Time)"),
            ("[new Date(1700000000000).getHours(),new Date(1700000000000).getTimezoneOffset(),new Date(1700000000000).getDay(),new Date(1700000000000).getYear(),new Date(1700000000000).getDate(),new Date(1700000000000).getMonth(),new Date(1700000000000).getFullYear(),new Date(1700000000000).getMinutes(),new Date(1700000000000).getSeconds(),new Date(1700000000000).getMilliseconds()].join()", "string:17,300,2,123,14,10,2023,13,20,0"),
            ("[new Date(2020,0,31,10,20,30,400).getTime(),new Date(2020,6,1).getTime(),new Date(99,0).getFullYear(),new Date(2020,0).getTime()].join()", "string:1580484030400,1593576000000,1999,1577854800000"),
            ("[new Date('2020-07-01T12:00:00').getTime(),Date.parse('2020-01-01T00:00'),Date.parse('2020-01-01T00:00:00.5'),Date.parse('2020-13-01T00:00')].join()", "string:1593619200000,1577854800000,1577854800500,NaN"),
            ("[new Date(2020,2,8,2,30).getTime(),new Date(2020,10,1,1,30).getTime(),new Date(2020,2,29,1,30).getTime(),new Date(2020,9,25,1,30).getTime()].join()", "string:1583652600000,1604208600000,1585459800000,1603603800000"),
            ("new Date(2020,2,8,2,30).toString()", "string:Sun Mar 08 2020 03:30:00 GMT-0400 (Eastern Daylight Time)"),
            ("var d=new Date(1700000000000);d.setHours(30);d.getTime()", "number:1700046800000"),
            ("var d=new Date(1700000000000);d.setMonth(5,15);d.getTime()", "number:1686863600000"),
            ("var d=new Date(1700000000000);d.setFullYear(2000);d.getTime()", "number:974240000000"),
            ("var d=new Date(1700000000000);d.setYear(99);d.getTime()", "number:942617600000"),
            ("var d=new Date(NaN);d.setFullYear(2000);d.getTime()", "number:946702800000"),
            ("var d=new Date(1700000000000);d.setDate(0);d.getTime()", "number:1698786800000"),
            ("var d=new Date(1700000000000);d.setMinutes(61,1,1);d.getTime()", "number:1700002861001"),
            ("var d=new Date(1700000000000);d.setSeconds(5);d.getTime()", "number:1699999985000"),
            ("var d=new Date(1700000000000);d.setMilliseconds(5);d.getTime()", "number:1700000000005"),
            ("var d=new Date(1700000000000);[d.setHours(1,2,3,4),d.getTime()].join()", "string:1699941723004,1699941723004"),
            ("var d=new Date(1700000000000);d.setYear(NaN)+','+d.getTime()", "string:NaN,NaN"),
            ("new Date(1e12).toTimeString()", "string:21:46:40 GMT-0400 (Eastern Daylight Time)"),
            ("'' + new Date(0)", "string:Wed Dec 31 1969 19:00:00 GMT-0500 (Eastern Standard Time)"),
            ("typeof Date()", "string:string"),
            ("new Date(NaN).setHours(1)", "number:NaN"),
            ("new Date(NaN).setYear(2000)", "number:946702800000"),
            ("new Date(1700000000000).toLocaleString('en-US')", "string:11/14/2023, 5:13:20 PM"),
            ("new Date(1700000000000).toLocaleDateString('en-US')", "string:11/14/2023"),
            ("new Date(1700000000000).toLocaleTimeString('en-US')", "string:5:13:20 PM"),
            ("[new Date(2100,6,1).getTimezoneOffset(),new Date(2100,0,1).getTimezoneOffset(),new Date(2030,5,1,12).toString()].join('|')", "string:240|300|Sat Jun 01 2030 12:00:00 GMT-0400 (Eastern Daylight Time)"),
        ]),
        ("Europe/London", &[
            ("new Date(0).toString()", "string:Thu Jan 01 1970 01:00:00 GMT+0100 (Greenwich Mean Time)"),
            ("new Date(0).toDateString()", "string:Thu Jan 01 1970"),
            ("new Date(0).toTimeString()", "string:01:00:00 GMT+0100 (Greenwich Mean Time)"),
            ("[new Date(1700000000000).getHours(),new Date(1700000000000).getTimezoneOffset(),new Date(1700000000000).getDay(),new Date(1700000000000).getYear(),new Date(1700000000000).getDate(),new Date(1700000000000).getMonth(),new Date(1700000000000).getFullYear(),new Date(1700000000000).getMinutes(),new Date(1700000000000).getSeconds(),new Date(1700000000000).getMilliseconds()].join()", "string:22,0,2,123,14,10,2023,13,20,0"),
            ("[new Date(2020,0,31,10,20,30,400).getTime(),new Date(2020,6,1).getTime(),new Date(99,0).getFullYear(),new Date(2020,0).getTime()].join()", "string:1580466030400,1593558000000,1999,1577836800000"),
            ("[new Date('2020-07-01T12:00:00').getTime(),Date.parse('2020-01-01T00:00'),Date.parse('2020-01-01T00:00:00.5'),Date.parse('2020-13-01T00:00')].join()", "string:1593601200000,1577836800000,1577836800500,NaN"),
            ("[new Date(2020,2,8,2,30).getTime(),new Date(2020,10,1,1,30).getTime(),new Date(2020,2,29,1,30).getTime(),new Date(2020,9,25,1,30).getTime()].join()", "string:1583634600000,1604194200000,1585445400000,1603585800000"),
            ("new Date(2020,2,8,2,30).toString()", "string:Sun Mar 08 2020 02:30:00 GMT+0000 (Greenwich Mean Time)"),
            ("var d=new Date(1700000000000);d.setHours(30);d.getTime()", "number:1700028800000"),
            ("var d=new Date(1700000000000);d.setMonth(5,15);d.getTime()", "number:1686863600000"),
            ("var d=new Date(1700000000000);d.setFullYear(2000);d.getTime()", "number:974240000000"),
            ("var d=new Date(1700000000000);d.setYear(99);d.getTime()", "number:942617600000"),
            ("var d=new Date(NaN);d.setFullYear(2000);d.getTime()", "number:946684800000"),
            ("var d=new Date(1700000000000);d.setDate(0);d.getTime()", "number:1698790400000"),
            ("var d=new Date(1700000000000);d.setMinutes(61,1,1);d.getTime()", "number:1700002861001"),
            ("var d=new Date(1700000000000);d.setSeconds(5);d.getTime()", "number:1699999985000"),
            ("var d=new Date(1700000000000);d.setMilliseconds(5);d.getTime()", "number:1700000000005"),
            ("var d=new Date(1700000000000);[d.setHours(1,2,3,4),d.getTime()].join()", "string:1699923723004,1699923723004"),
            ("var d=new Date(1700000000000);d.setYear(NaN)+','+d.getTime()", "string:NaN,NaN"),
            ("new Date(1e12).toTimeString()", "string:02:46:40 GMT+0100 (British Summer Time)"),
            ("'' + new Date(0)", "string:Thu Jan 01 1970 01:00:00 GMT+0100 (Greenwich Mean Time)"),
            ("typeof Date()", "string:string"),
            ("new Date(NaN).setHours(1)", "number:NaN"),
            ("new Date(NaN).setYear(2000)", "number:946684800000"),
            ("new Date(1700000000000).toLocaleString('en-US')", "string:11/14/2023, 10:13:20 PM"),
            ("new Date(1700000000000).toLocaleDateString('en-US')", "string:11/14/2023"),
            ("new Date(1700000000000).toLocaleTimeString('en-US')", "string:10:13:20 PM"),
            ("[new Date(2100,6,1).getTimezoneOffset(),new Date(2100,0,1).getTimezoneOffset(),new Date(2030,5,1,12).toString()].join('|')", "string:-60|0|Sat Jun 01 2030 12:00:00 GMT+0100 (British Summer Time)"),
        ]),
        ("Australia/Sydney", &[
            ("new Date(0).toString()", "string:Thu Jan 01 1970 10:00:00 GMT+1000 (Australian Eastern Standard Time)"),
            ("new Date(0).toDateString()", "string:Thu Jan 01 1970"),
            ("new Date(0).toTimeString()", "string:10:00:00 GMT+1000 (Australian Eastern Standard Time)"),
            ("[new Date(1700000000000).getHours(),new Date(1700000000000).getTimezoneOffset(),new Date(1700000000000).getDay(),new Date(1700000000000).getYear(),new Date(1700000000000).getDate(),new Date(1700000000000).getMonth(),new Date(1700000000000).getFullYear(),new Date(1700000000000).getMinutes(),new Date(1700000000000).getSeconds(),new Date(1700000000000).getMilliseconds()].join()", "string:9,-660,3,123,15,10,2023,13,20,0"),
            ("[new Date(2020,0,31,10,20,30,400).getTime(),new Date(2020,6,1).getTime(),new Date(99,0).getFullYear(),new Date(2020,0).getTime()].join()", "string:1580426430400,1593525600000,1999,1577797200000"),
            ("[new Date('2020-07-01T12:00:00').getTime(),Date.parse('2020-01-01T00:00'),Date.parse('2020-01-01T00:00:00.5'),Date.parse('2020-13-01T00:00')].join()", "string:1593568800000,1577797200000,1577797200500,NaN"),
            ("[new Date(2020,2,8,2,30).getTime(),new Date(2020,10,1,1,30).getTime(),new Date(2020,2,29,1,30).getTime(),new Date(2020,9,25,1,30).getTime()].join()", "string:1583595000000,1604154600000,1585405800000,1603549800000"),
            ("new Date(2020,2,8,2,30).toString()", "string:Sun Mar 08 2020 02:30:00 GMT+1100 (Australian Eastern Daylight Time)"),
            ("var d=new Date(1700000000000);d.setHours(30);d.getTime()", "number:1700075600000"),
            ("var d=new Date(1700000000000);d.setMonth(5,15);d.getTime()", "number:1686784400000"),
            ("var d=new Date(1700000000000);d.setFullYear(2000);d.getTime()", "number:974240000000"),
            ("var d=new Date(1700000000000);d.setYear(99);d.getTime()", "number:942617600000"),
            ("var d=new Date(NaN);d.setFullYear(2000);d.getTime()", "number:946645200000"),
            ("var d=new Date(1700000000000);d.setDate(0);d.getTime()", "number:1698704000000"),
            ("var d=new Date(1700000000000);d.setMinutes(61,1,1);d.getTime()", "number:1700002861001"),
            ("var d=new Date(1700000000000);d.setSeconds(5);d.getTime()", "number:1699999985000"),
            ("var d=new Date(1700000000000);d.setMilliseconds(5);d.getTime()", "number:1700000000005"),
            ("var d=new Date(1700000000000);[d.setHours(1,2,3,4),d.getTime()].join()", "string:1699970523004,1699970523004"),
            ("var d=new Date(1700000000000);d.setYear(NaN)+','+d.getTime()", "string:NaN,NaN"),
            ("new Date(1e12).toTimeString()", "string:11:46:40 GMT+1000 (Australian Eastern Standard Time)"),
            ("'' + new Date(0)", "string:Thu Jan 01 1970 10:00:00 GMT+1000 (Australian Eastern Standard Time)"),
            ("typeof Date()", "string:string"),
            ("new Date(NaN).setHours(1)", "number:NaN"),
            ("new Date(NaN).setYear(2000)", "number:946645200000"),
            ("new Date(1700000000000).toLocaleString('en-US')", "string:11/15/2023, 9:13:20 AM"),
            ("new Date(1700000000000).toLocaleDateString('en-US')", "string:11/15/2023"),
            ("new Date(1700000000000).toLocaleTimeString('en-US')", "string:9:13:20 AM"),
            ("[new Date(2100,6,1).getTimezoneOffset(),new Date(2100,0,1).getTimezoneOffset(),new Date(2030,5,1,12).toString()].join('|')", "string:-600|-660|Sat Jun 01 2030 12:00:00 GMT+1000 (Australian Eastern Standard Time)"),
        ]),
    ]
}

#[test]
fn local_time_matches_node_in_each_zone() {
    big(|| {
        let cut = |s: &str| s.split(" (").next().unwrap().to_string();
        let mut bad = Vec::new();
        for (zone, cases) in local_time_cases() {
            let mut it = realm();
            it.set_default_time_zone(zone).unwrap();
            for (src, want) in cases {
                let got = out(&mut it, src);
                if cut(&got) != cut(want) {
                    bad.push(format!("{zone}: {src}
node: {want}
bare: {got}"));
                }
            }
        }
        assert!(bad.is_empty(), "{} differences:
{}", bad.len(), bad.join("
"));
    });
}

#[test]
fn local_time_zone_names_and_offsets_in_text() {
    big(|| {
        let mut it = realm();
        it.set_default_time_zone("UTC").unwrap();
        assert_eq!(out(&mut it, "new Date(0).toString()"), "string:Thu Jan 01 1970 00:00:00 GMT+0000 (Coordinated Universal Time)");
        let mut it = realm();
        it.set_default_time_zone("Asia/Kolkata").unwrap();
        assert_eq!(out(&mut it, "new Date(0).toString()"), "string:Thu Jan 01 1970 05:30:00 GMT+0530 (India Standard Time)");
        assert_eq!(out(&mut it, "new Date(0).toTimeString()"), "string:05:30:00 GMT+0530 (India Standard Time)");
        let mut it = realm();
        it.set_default_time_zone("America/New_York").unwrap();
        assert_eq!(out(&mut it, "new Date(1e12).toString()"), "string:Sat Sep 08 2001 21:46:40 GMT-0400 (Eastern Daylight Time)");
    });
}

#[test]
fn local_time_zone_file_is_read_on_the_first_local_call_only() {
    big(|| {
        let mut it = realm();
        // a zone that cannot be read would fail the realm if anything loaded it before the first local call
        it.run_script("Date.UTC(2020, 0); new Date(5).toISOString(); new Date(5).getUTCHours(); new Date(NaN).getHours(); new Date(NaN).toString();").unwrap();
        assert!(it.intl.default_zone.is_none());
        assert_eq!(out(&mut it, "new Date(0).getTimezoneOffset() === new Date(0).getTimezoneOffset()"), "boolean:true");
        assert!(it.intl.default_zone.is_some());
    });
}

#[test]
fn local_time_methods_on_an_invalid_date() {
    big(|| {
        let mut it = realm();
        assert_eq!(
            out(&mut it, "var d = new Date(NaN); [d.getFullYear(), d.getDay(), d.getHours(), d.getTimezoneOffset(), d.getYear(), d.toString(), d.toDateString(), d.toTimeString(), d.toLocaleString(), '' + d, String(d), d.setHours(1), d.setDate(1)].join('|')"),
            "string:NaN|NaN|NaN|NaN|NaN|Invalid Date|Invalid Date|Invalid Date|Invalid Date|Invalid Date|Invalid Date|NaN|NaN"
        );
        // the branded receiver is checked first
        assert_eq!(out(&mut it, "try { Date.prototype.getFullYear.call({}); } catch (e) { e.name }"), "string:TypeError");
        assert_eq!(out(&mut it, "typeof Date.prototype.getFullYear + typeof Date.prototype.toString + Date.prototype.setMonth.length + Date.prototype.setHours.length"), "string:functionfunction24");
    });
}

#[test]
fn local_time_arithmetic_stays_usable() {
    big(|| {
        let mut it = realm();
        assert_eq!(out(&mut it, "var d = new Date(86400000); [d - 1, +d, d > 5, d.valueOf(), d.getTime()].join()"), "string:86399999,86400000,true,86400000,86400000");
    });
}

#[test]
fn legacy_string_formats_parse_as_in_node_and_bad_text_is_nan() {
    big(|| {
        let mut it = realm();
        it.set_default_time_zone("UTC").unwrap();
        // Values below come from Node 22.18 with TZ=UTC.
        for (src, want) in [
            // the dates that ubuntu.com reads from /blog/latest-news
            ("Date.parse('6 November 2026')", "1793923200000"),
            ("new Date('15 October 2026').getDate()", "15"),
            ("Date.parse('Jan 1 2020')", "1577836800000"),
            ("Date.parse('1/2/2020')", "1577923200000"),
            ("Date.parse('2020/01/02')", "1577923200000"),
            ("Date.parse('2020-1-1')", "1577836800000"),
            ("Date.parse('2020-01-01 10:30:15')", "1577874615000"),
            ("Date.parse('Mon Nov 09 2026 12:00:00 GMT+0530 (India Standard Time)')", "1794205800000"),
            ("Date.parse('November 9, 2026')", "1794182400000"),
            ("Date.parse('Nov 9, 2026 3:04 PM')", "1794236640000"),
            ("Date.parse('2020-01-01T00:00:00+0100')", "1577833200000"),
            ("Date.parse('Jan 1')", "978307200000"),
            ("Date.parse('Feb 30 2020')", "1583020800000"),
            ("Date.parse('Jan 1 2020 Mon')", "NaN"),
            ("Date.parse('13/1/2020')", "NaN"),
            ("Date.parse('Jan 1 2020 25:00')", "NaN"),
            ("Date.parse('1/2/2020 xyz')", "NaN"),
            ("Date.parse('20200101')", "NaN"),
            ("Date.parse('Jan')", "NaN"),
            ("Date.parse('garbage')", "NaN"),
            ("Date.parse('')", "NaN"),
        ] {
            assert_eq!(out(&mut it, src), format!("number:{want}"), "{src}");
        }
    });
}

#[test]
fn subclass_and_reflect_construct_take_the_new_target_prototype() {
    big(|| {
        let mut it = realm();
        assert_eq!(out(&mut it, "class D extends Date {} var d = new D(0); [d instanceof D, d.getTime(), Object.prototype.toString.call(d)].join()"), "string:true,0,[object Date]");
        let mut it = realm();
        assert_eq!(out(&mut it, "function F() {} F.prototype = Object.create(Date.prototype); var d = Reflect.construct(Date, [5], F); [d instanceof F, d.getTime()].join()"), "string:true,5");
        // the same new.target is a plain construction
        let mut it = realm();
        assert_eq!(out(&mut it, "Reflect.construct(Date, [5], Date).getTime()"), "number:5");
        assert_eq!(out(&mut it, "new (Date.bind(null, 7))().getTime()"), "number:7");
        // methods are not constructors
        assert_eq!(out(&mut it, "try { new Date.now(); } catch (e) { e.name }"), "string:TypeError");
        assert_eq!(out(&mut it, "try { new Date.prototype.getTime(); } catch (e) { e.name }"), "string:TypeError");
    });
}

// ------------------------------------------------------------------------------------------ deviations and fine points

#[test]
fn illegal_field_values_are_nan_where_v8_rolls_over() {
    big(|| {
        let mut it = realm();
        assert_eq!(out(&mut it, "['2021-02-29', '2020-04-31', '1900-02-29', 'Tue, 30 Feb 2020 00:00:00 GMT'].map(Date.parse).join()"), "string:NaN,NaN,NaN,NaN");
    });
}

#[test]
fn setter_arguments_are_all_coerced_even_for_an_invalid_date() {
    big(|| {
        // V8 coerces only the first one; the standard (section 21.4.4.24 and siblings) coerces each one that is present.
        let mut it = realm();
        assert_eq!(
            out(&mut it, "var n = 0; var o = { valueOf: function () { n++; return 1; } }; var d = new Date(NaN); d.setUTCHours(o, o, o, o); d.setUTCMinutes(o, o, o); d.setUTCSeconds(o, o); d.setUTCMilliseconds(o); d.setUTCDate(o); d.setUTCMonth(o, o); [n, d.getTime()].join()"),
            "string:13,NaN"
        );
        // setUTCFullYear starts from +0
        assert_eq!(out(&mut it, "new Date(NaN).setUTCFullYear(1970)"), "number:0");
    });
}

#[test]
fn setters_use_the_time_value_read_before_coercion() {
    big(|| {
        // The standard reads [[DateValue]] before ToNumber (section 21.4.4.24 step 3), so a valueOf that changes the date does
        // not change the base of the result. V8 reads it after and gives 9005.
        let mut it = realm();
        assert_eq!(out(&mut it, "var d = new Date(1000); var o = { valueOf: function () { d.setTime(5); return 9; } }; [d.setUTCSeconds(o), d.getTime()].join()"), "string:9000,9000");
    });
}

#[test]
fn symbol_and_object_arguments_follow_to_number_rules() {
    big(|| {
        let mut it = realm();
        assert_eq!(out(&mut it, "try { new Date(Symbol()); } catch (e) { e.name }"), "string:TypeError");
        assert_eq!(out(&mut it, "try { new Date(0).setTime(Symbol()); } catch (e) { e.name }"), "string:TypeError");
        assert_eq!(out(&mut it, "new Date(function () {}).getTime()"), "number:NaN");
        // a thrown valueOf comes out of the constructor
        assert_eq!(out(&mut it, "try { new Date({ valueOf: function () { throw new RangeError('r'); }, toString: null }); } catch (e) { e.name }"), "string:RangeError");
    });
}

#[test]
fn json_stringify_uses_to_json_of_the_date() {
    big(|| {
        let mut it = realm();
        assert_eq!(out(&mut it, "JSON.stringify({ a: new Date(1e12), b: [new Date(NaN)] })"), "string:{\"a\":\"2001-09-09T01:46:40.000Z\",\"b\":[null]}");
        // a replacer sees the string, because toJSON ran first
        assert_eq!(out(&mut it, "var seen; JSON.stringify({ d: new Date(0) }, function (k, v) { if (k === 'd') seen = typeof v; return v; }); seen"), "string:string");
    });
}

// ------------------------------------------------------------------------------------------ caps

#[test]
fn date_objects_are_capped_before_the_object_is_made() {
    big(|| {
        let mut it = realm();
        it.set_sc_limits(ScLimits { max_dates: 5, ..Default::default() });
        assert_eq!(out(&mut it, "var a = []; for (var i = 0; i < 5; i++) a.push(new Date(i)); a.length"), "number:5");
        let objects = it.heap.stats().live_objects;
        assert_eq!(out(&mut it, "try { new Date(5); } catch (e) { 'caught' }"), "limit:Sc(Dates)");
        assert_eq!(it.heap.stats().live_objects, objects, "the object is not made past the cap");
        // collecting frees the budget
        out(&mut it, "a = null;");
        it.collect(&[]).unwrap();
        assert_eq!(it.date_tracked(), 0);
        assert_eq!(out(&mut it, "new Date(1).getTime()"), "number:1");
    });
}

#[test]
fn a_date_string_is_capped_and_charged_before_it_is_read() {
    big(|| {
        let mut it = realm();
        it.set_sc_limits(ScLimits { max_date_string: 40, ..Default::default() });
        assert_eq!(out(&mut it, "Date.parse('2020-01-01T00:00:00.000000000000000000Z'.slice(0, 40))"), "number:1577836800000");
        assert_eq!(out(&mut it, "Date.parse('2020-01-01T00:00:00.' + rep('0', 30) + 'Z')"), "limit:Sc(DateString)");
        assert_eq!(out(&mut it, "new Date(rep('x', 41))"), "limit:Sc(DateString)");
        // 128 units by default, and the length is charged as steps
        let mut it = realm();
        assert_eq!(out(&mut it, "Date.parse('2020-01-01T00:00:00.' + rep('1', 107) + 'Z')"), "number:1577836800111");
        assert_eq!(out(&mut it, "Date.parse('2020-01-01T00:00:00.' + rep('1', 108) + 'Z')"), "limit:Sc(DateString)");
        let before = it.steps;
        out(&mut it, "Date.parse('2020-01-01T00:00:00.' + rep('1', 100) + 'Z')");
        assert!(it.steps - before >= 120, "steps {}", it.steps - before);
    });
}

#[test]
fn a_hostile_loop_of_date_work_ends_in_the_step_limit() {
    big(|| {
        let mut it = Interp::realm(Heap::default(), EvalLimits { max_steps: 200_000, ..Default::default() }).unwrap();
        assert_eq!(out(&mut it, "var s = 0; for (;;) { s += Date.parse('2020-01-01T00:00:00.123Z') + new Date(s).getUTCDay(); }"), "limit:Steps");
    });
}

// ------------------------------------------------------------------------------------------ collector

#[test]
fn unreachable_dates_are_pruned_and_reachable_ones_keep_their_time() {
    big(|| {
        let mut it = realm();
        out(&mut it, "var keep = new Date(12345); var obj = { d: new Date(777) }; for (var i = 0; i < 1000; i++) new Date(i); 0;");
        assert_eq!(it.date_tracked(), 1002);
        it.collect(&[]).unwrap();
        assert_eq!(it.date_tracked(), 2);
        assert_eq!(out(&mut it, "[keep.getTime(), obj.d.getTime(), Object.prototype.toString.call(keep)].join()"), "string:12345,777,[object Date]");
        out(&mut it, "obj = null;");
        it.collect(&[]).unwrap();
        assert_eq!(it.date_tracked(), 1);
        // a second collection changes nothing
        it.collect(&[]).unwrap();
        assert_eq!(it.date_tracked(), 1);
    });
}

#[test]
fn a_stale_handle_never_finds_the_date_of_a_reused_slot() {
    big(|| {
        let mut it = realm();
        let Value::Object(old) = it.run_script("var tmp = new Date(111); tmp").unwrap() else { panic!() };
        assert_eq!(it.date.times.get(&old), Some(&111.0));
        out(&mut it, "tmp = 0;");
        it.collect(&[]).unwrap();
        assert!(!it.date.times.contains_key(&old), "pruned");
        // fill freed slots with plain objects and with dates, so the old slot index is used again
        out(&mut it, "var a = []; for (var i = 0; i < 300; i++) { a.push(i % 2 ? {} : new Date(i)); }");
        assert!(!it.date.times.contains_key(&old), "an old handle has an old generation");
        assert_eq!(out(&mut it, "a.filter(function (x) { return Object.prototype.toString.call(x) === '[object Date]'; }).length"), "number:150");
        assert_eq!(out(&mut it, "a.every(function (x, i) { return i % 2 ? !(x instanceof Date) : x.getTime() === i; })"), "boolean:true");
    });
}

#[test]
fn a_deleted_date_global_does_not_lose_the_builtin() {
    big(|| {
        let mut it = realm();
        out(&mut it, "var D = Date; var P = Date.prototype; delete globalThis.Date;");
        it.collect(&[]).unwrap();
        it.collect(&[]).unwrap();
        assert_eq!(out(&mut it, "typeof Date"), "string:undefined");
        assert_eq!(out(&mut it, "var d = new D(5); [d.getTime(), d instanceof D, Object.getPrototypeOf(d) === P, typeof P.toISOString, D.now() > 0].join()"), "string:5,true,true,function,true");
        // even when nothing at all holds the constructor
        let mut it = realm();
        out(&mut it, "delete globalThis.Date;");
        it.collect(&[]).unwrap();
        assert!(it.heap.get_prototype_of(it.date.proto).is_ok(), "the prototype object is alive");
        assert!(it.heap.get_own_property(it.date.ctor, it.k_prototype).is_ok(), "the constructor is alive");
    });
}

#[test]
fn dates_work_across_scripts_with_promises_and_collections_between() {
    big(|| {
        let mut it = realm();
        out(&mut it, "var d = new Date(100); var log = []; var p = Promise.resolve(d).then(function (x) { log.push('then:' + x.getTime()); return x.getTime() + 1; });");
        it.collect(&[]).unwrap();
        out(&mut it, "var q = p.then(function (v) { log.push('q:' + v); return new Date(v); }); function later() { return d.getUTCFullYear() + ':' + q; }");
        it.collect(&[]).unwrap();
        it.drain_microtasks().unwrap();
        it.collect(&[]).unwrap();
        assert_eq!(out(&mut it, "log.join()"), "string:then:100,q:101");
        // `q` is a promise: its Date result is held by the promise state until read
        out(&mut it, "var got; q.then(function (x) { got = x.getTime() + ':' + (x instanceof Date); });");
        it.drain_microtasks().unwrap();
        assert_eq!(out(&mut it, "got"), "string:101:true");
        assert_eq!(out(&mut it, "later().indexOf('1970:') === 0"), "boolean:true");
    });
}

#[test]
fn a_date_in_a_pending_promise_reaction_survives_collection() {
    big(|| {
        let mut it = realm();
        out(&mut it, "var res; var resolve; var p = new Promise(function (r) { resolve = r; }); p.then(function (v) { res = v.getTime(); });");
        out(&mut it, "resolve(new Date(4242));");
        it.collect(&[]).unwrap();
        it.drain_microtasks().unwrap();
        assert_eq!(out(&mut it, "res"), "number:4242");
    });
}

#[test]
fn a_date_held_by_a_map_key_weak_map_and_closure_survives() {
    big(|| {
        let mut it = realm();
        out(&mut it, "var m = new Map(), w = new WeakMap(); var k = new Date(1); m.set(k, new Date(2)); w.set(k, new Date(3)); var f = (function () { var c = new Date(4); return function () { return c.getTime(); }; })(); k = null;");
        it.collect(&[]).unwrap();
        // the Map keeps its key; the WeakMap entry stays because the Map keeps the key reachable
        assert_eq!(out(&mut it, "var r = []; m.forEach(function (v, key) { r.push(v.getTime(), key.getTime(), w.get(key).getTime()); }); r.push(f()); r.join()"), "string:2,1,3,4");
        assert_eq!(it.date_tracked(), 4);
    });
}
