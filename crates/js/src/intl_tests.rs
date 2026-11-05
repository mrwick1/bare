//! Unit tests of the pure Intl functions. The Node comparison is `tools/js/intl_check.sh`.

use super::*;

fn fields(year: i64, month: i64, day: i64, hour: i64, minute: i64, second: i64, weekday: i64) -> Fields {
    Fields { year, month, day, hour, minute, second, ms: 0, weekday }
}

fn req() -> Request {
    Request::default()
}

fn text(loc: u8, r: &Request, t: &Fields) -> Result<String, Gate> {
    let mut s = String::new();
    format(loc, resolve(loc, r), t, &mut s, None).map(|_| s)
}

#[test]
fn tags_are_canonical_or_refused_like_v8() {
    assert_eq!(canonical_tag("EN-us").unwrap(), "en-US");
    assert_eq!(canonical_tag("en-latn-us").unwrap(), "en-Latn-US");
    assert_eq!(canonical_tag("en-419").unwrap(), "en-419");
    assert_eq!(canonical_tag("en-US-POSIX-1abc").unwrap(), "en-US-posix-1abc");
    for t in ["", "e", "12", "en_US", "en--US", "toolonglanguage", "en-US-abcdefghi", "en-"] {
        assert_eq!(canonical_tag(t), Err(TagError::Malformed), "{t}");
    }
    for t in ["en-US-u", "en-a-bc-a-de", "en-US-x", "en-u-a", "en-1abc-1abc", "en-t-a", "en-z-ab-Z-cd"] {
        assert_eq!(canonical_tag(t), Err(TagError::Invalid), "{t}");
    }
    for t in ["en-u-ca-gregory", "en-x-a-b", "en-a-bc", "en-u-ca", "en-US-x-zz"] {
        assert_eq!(canonical_tag(t), Err(TagError::Extension), "{t}");
    }
}

#[test]
fn lookup_drops_subtags_from_the_right() {
    let name = |t: &str| lookup(t).map(locale_name);
    assert_eq!(name("en-US"), Some("en-US"));
    assert_eq!(name("en-Latn-US"), Some("en"));
    assert_eq!(name("en-AU"), Some("en"));
    assert_eq!(name("en-GB-oxendict"), Some("en-GB"));
    assert_eq!(name("fr"), None);
    assert_eq!(name("fr-CA"), None);
}

#[test]
fn hour12_overrides_hour_cycle_and_the_locale_picks_the_default() {
    let us = lookup("en-US").unwrap();
    let gb = lookup("en-GB").unwrap();
    assert_eq!(hour_cycle(us, None, None), 1);
    assert_eq!(hour_cycle(gb, None, None), 2);
    assert_eq!(hour_cycle(us, Some(false), Some(0)), 2);
    assert_eq!(hour_cycle(gb, Some(true), None), 1);
    assert_eq!(hour_cycle(us, None, Some(3)), 3);
}

#[test]
fn date_and_time_patterns_join_or_use_the_whole_pattern() {
    let us = lookup("en-US").unwrap();
    let t = fields(2024, 6, 4, 15, 6, 9, 4);
    let mut r = req();
    (r.year, r.month, r.day) = (NUMERIC, NUMERIC, NUMERIC);
    assert_eq!(text(us, &r, &t).unwrap(), "7/4/2024");
    (r.hour, r.minute, r.hc) = (NUMERIC, TWO_DIGIT, 1);
    assert_eq!(text(us, &r, &t).unwrap(), "7/4/2024, 3:06 PM");
    let mut r = req();
    (r.month, r.day, r.hour, r.minute, r.second, r.hc) = (LONG, NUMERIC, NUMERIC, NUMERIC, NUMERIC, 1);
    assert_eq!(text(us, &r, &t).unwrap(), "July 4 at 3:06:09 PM");
    let mut r = req();
    (r.weekday, r.hour, r.hc) = (SHORT, NUMERIC, 2);
    assert_eq!(text(us, &r, &t).unwrap(), "Thu, 15");
}

#[test]
fn parts_keep_the_narrow_space_and_strings_do_not() {
    let us = lookup("en-US").unwrap();
    let mut r = req();
    (r.hour, r.minute, r.hc) = (NUMERIC, TWO_DIGIT, 1);
    let p = resolve(us, &r);
    let t = fields(2024, 0, 5, 0, 4, 0, 5);
    let (mut a, mut b, mut parts) = (String::new(), String::new(), Vec::new());
    format(us, p, &t, &mut a, None).unwrap();
    format(us, p, &t, &mut b, Some(&mut parts)).unwrap();
    assert_eq!(a, "12:04 AM");
    assert_eq!(b, "12:04\u{202f}AM");
    let kinds: Vec<Part> = parts.iter().map(|p| p.0).collect();
    assert_eq!(kinds, [Part::Hour, Part::Literal, Part::Minute, Part::Literal, Part::DayPeriod]);
    assert_eq!(parts.last().unwrap().1, b.len());
}

#[test]
fn hour_symbols_cover_the_four_cycles() {
    let us = lookup("en-US").unwrap();
    let t = fields(2024, 0, 5, 0, 4, 0, 5);
    let got: Vec<String> = (0..4)
        .map(|hc| {
            let mut r = req();
            (r.hour, r.minute, r.hc) = (NUMERIC, TWO_DIGIT, hc);
            text(us, &r, &t).unwrap()
        })
        .collect();
    assert_eq!(got, ["0:04 AM", "12:04 AM", "00:04", "24:04"]);
}

#[test]
fn shown_reports_the_fields_of_the_pattern() {
    let us = lookup("en-US").unwrap();
    let mut r = req();
    (r.weekday, r.hour, r.minute, r.hc) = (LONG, TWO_DIGIT, TWO_DIGIT, 1);
    let (f, hc) = shown(resolve(us, &r));
    assert_eq!(f, [("weekday", "long"), ("hour", "2-digit"), ("minute", "2-digit")]);
    assert_eq!(hc, Some(1));
}

#[test]
fn gates_stop_what_bare_cannot_print() {
    let us = lookup("en-US").unwrap();
    let mut r = req();
    r.year = NUMERIC;
    assert_eq!(text(us, &r, &fields(0, 0, 1, 0, 0, 0, 6)), Err(Gate::Era));
    let mut r = req();
    (r.time_style, r.hc) = (1, 1);
    assert_eq!(text(us, &r, &fields(2024, 0, 1, 0, 0, 0, 1)), Err(Gate::ZoneName));
}
