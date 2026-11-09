//! Unit tests of the ICU4X wrappers. The Node comparison is `tools/js/intl_num_check.sh`.

use super::*;

fn f(loc: u8) -> NumFmt {
    NumFmt { loc, style: Style::Decimal, currency: *b"USD", display: SYMBOL, accounting: false, min_int: 1, rounding: Rounding::Fraction, fd: (0, 3), sd: (1, 21), grouping: Grouping::Auto, sign: SignDisplay::Auto, mode: HALF_EXPAND, unit: 255, per_unit: 255, unit_display: 0, notation: 0, compact_long: false, strip_zero: false }
}

fn fmt(c: &mut Cache, f: &NumFmt, x: f64) -> String {
    let mut s = String::new();
    c.format(f, num_from_f64(x), Out { s: &mut s, parts: None });
    s
}

#[test]
fn indian_grouping() {
    let mut c = Cache::default();
    assert_eq!(fmt(&mut c, &f(3), 1234567.891), "12,34,567.891");
    assert_eq!(fmt(&mut c, &f(0), 1234567.891), "1,234,567.891");
}

fn with(loc: u8, g: impl FnOnce(&mut NumFmt)) -> NumFmt {
    let mut n = f(loc);
    g(&mut n);
    n
}

#[test]
fn locales_group_and_round() {
    let mut c = Cache::default();
    for loc in 0..5 {
        let n = f(loc);
        let want = if loc == 3 { ["1,23,456", "0.5", "-0", "NaN", "∞"] } else { ["123,456", "0.5", "-0", "NaN", "∞"] };
        let got = [123456.0, 0.5, -0.0, f64::NAN, f64::INFINITY].map(|x| fmt(&mut c, &n, x));
        assert_eq!(got, want, "locale {loc}");
    }
    // half-expand on the shortest digits: 1.005 is 1.01 (a double that is below 1.005 in binary)
    assert_eq!(fmt(&mut c, &with(0, |n| n.fd = (0, 2)), 1.005), "1.01");
    assert_eq!(fmt(&mut c, &with(0, |n| n.fd = (0, 0)), -2.5), "-3");
    assert_eq!(fmt(&mut c, &with(0, |n| n.fd = (0, 0)), 1e21), "1,000,000,000,000,000,000,000");
    assert_eq!(fmt(&mut c, &with(0, |n| n.fd = (2, 2)), 5.0), "5.00");
}

#[test]
fn currency_and_percent() {
    let mut c = Cache::default();
    let cur = |loc: u8, code: &[u8; 3]| with(loc, |n| {
        n.style = Style::Currency;
        n.currency = *code;
        n.fd = (currency_digits(core::str::from_utf8(code).unwrap()), currency_digits(core::str::from_utf8(code).unwrap()));
    });
    assert_eq!(fmt(&mut c, &cur(3, b"INR"), -1234567.5), "-₹12,34,567.50");
    assert_eq!(fmt(&mut c, &cur(0, b"USD"), 1234.5), "$1,234.50");
    assert_eq!(fmt(&mut c, &cur(0, b"JPY"), 1234.5), "¥1,235");
    assert_eq!(fmt(&mut c, &cur(0, b"CHF"), 5.0), "CHF\u{a0}5.00");
    assert_eq!(fmt(&mut c, &with(0, |n| { n.style = Style::Percent; n.fd = (0, 0); }), 0.256), "26%");
}

#[test]
fn parts_have_the_types_of_ecma_402() {
    let mut c = Cache::default();
    let mut s = String::new();
    let mut parts = Vec::new();
    c.format(&with(3, |n| n.fd = (2, 2)), num_from_f64(-1234567.891), Out { s: &mut s, parts: Some(&mut parts) });
    assert_eq!(s, "-12,34,567.89");
    // each part is (type, end offset in bytes)
    assert_eq!(parts, [("minusSign", 1), ("integer", 3), ("group", 4), ("integer", 6), ("group", 7), ("integer", 10), ("decimal", 11), ("fraction", 13)]);
}

#[test]
fn collation_orders_accents_case_and_numbers() {
    let mut c = Cache::default();
    let mut cmp = |key: CollKey, a: &str, b: &str| {
        let k = c.collator(key);
        compare(&k, a, b)
    };
    let d = DEFAULT_COLL;
    assert_eq!(cmp(d, "a", "b"), -1);
    assert_eq!(cmp(d, "a", "A"), -1);
    assert_eq!(cmp(d, "e", "é"), -1);
    assert_eq!(cmp(d, "é", "f"), -1);
    assert_eq!(cmp(d, "a", "a"), 0);
    assert_eq!(cmp(d, "a2", "a10"), 1);
    assert_eq!(cmp(CollKey { numeric: true, ..d }, "a2", "a10"), -1);
    assert_eq!(cmp(CollKey { sensitivity: 0, ..d }, "a", "Á"), 0);
    assert_eq!(cmp(CollKey { sensitivity: 1, ..d }, "a", "A"), 0);
    assert_eq!(cmp(CollKey { sensitivity: 1, ..d }, "a", "á"), -1);
    assert_eq!(cmp(CollKey { ignore_punctuation: true, ..d }, "a-b", "ab"), 0);
    assert_eq!(cmp(CollKey { case_first: 1, ..d }, "a", "A"), 1);
}

#[test]
fn plural_categories() {
    let mut c = Cache::default();
    let n = f(0);
    let cat = |c: &mut Cache, x: f64, ordinal: bool| c.plural(&n, ordinal, num_from_f64(x));
    assert_eq!(cat(&mut c, 1.0, false), "one");
    assert_eq!(cat(&mut c, 1.5, false), "other");
    assert_eq!(cat(&mut c, 22.0, true), "two");
    assert_eq!(cat(&mut c, 13.0, true), "other");
    assert_eq!(cat(&mut c, f64::NAN, false), "other");
}
