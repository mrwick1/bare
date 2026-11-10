//! `Intl.Locale` and `Intl.RelativeTimeFormat` tests. The corpus (`tools/fixtures/js/intl-locale-cases.txt`) is checked against
//! what Node gave (`intl-locale-cases.expected`; `tools/js/intl_locale_check.sh` re-runs Node). The tests below cover what Node
//! cannot show: the lazy install, the cap, GC of the objects and the limits of bare.

use super::intl_tests::{big, out, realm};
use super::*;

/// Every case of the corpus gives what Node gave.
#[test]
fn corpus_matches_node() {
    let (cases, expected) = (include_str!("../../../tools/fixtures/js/intl-locale-cases.txt"), include_str!("../../../tools/fixtures/js/intl-locale-cases.expected"));
    big(move || {
        let mut list: Vec<(&str, Vec<&str>)> = Vec::new();
        for line in cases.lines() {
            if let Some(n) = line.strip_prefix("=== ") {
                list.push((n.trim(), Vec::new()));
            } else if let Some(c) = list.last_mut() {
                c.1.push(line);
            }
        }
        let want: Vec<&str> = expected.lines().collect();
        assert_eq!(list.len(), want.len());
        let mut bad = Vec::new();
        for ((name, lines), w) in list.iter().zip(want) {
            let mut it = realm();
            let got = format!("{name}\t{}", out(&mut it, &lines.join("\n")));
            if got != w {
                bad.push(format!("node: {w}\nbare: {got}"));
            }
        }
        assert!(bad.is_empty(), "{} differences:\n{}", bad.len(), bad.join("\n"));
    });
}

#[test]
fn both_constructors_exist_after_the_first_read_of_intl() {
    big(|| {
        let mut it = realm();
        assert_eq!(out(&mut it, "typeof Intl.Locale + typeof Intl.RelativeTimeFormat"), "string:functionfunction");
    });
}

#[test]
fn the_two_constructors_are_made_by_their_first_read_not_by_the_read_of_intl() {
    big(|| {
        let mut it = realm();
        assert_eq!(out(&mut it, "var d = Object.getOwnPropertyDescriptor(Intl, 'Locale'); [typeof d.get, 'Locale' in Intl, 'RelativeTimeFormat' in Intl, typeof Intl.DateTimeFormat].join()"), "string:function,true,true,function");
        assert_eq!(out(&mut it, "var c = Intl.Locale; var e = Object.getOwnPropertyDescriptor(Intl, 'Locale'); [c === Intl.Locale, typeof e.value, e.writable, e.enumerable, e.configurable].join()"), "string:true,function,true,false,true");
        // an assignment before the first read replaces the accessor, as it does for `Intl` itself
        assert_eq!(out(&mut it, "Intl.RelativeTimeFormat = 5; Intl.RelativeTimeFormat"), "number:5");
    });
}

#[test]
fn locales_are_collected_when_dead() {
    big(|| {
        let mut it = realm();
        out(&mut it, "var keep = new Intl.Locale('en-GB'); for (var i = 0; i < 50; i++) new Intl.Locale('en'); 0;");
        assert_eq!(it.intl_formatters(), 51);
        it.collect(&[]).unwrap();
        assert_eq!(it.intl_formatters(), 1);
        assert_eq!(out(&mut it, "keep.region"), "string:GB");
        out(&mut it, "keep = null;");
        it.collect(&[]).unwrap();
        assert_eq!(it.intl_formatters(), 0);
    });
}

#[test]
fn relative_time_formats_are_collected_when_dead() {
    big(|| {
        let mut it = realm();
        out(&mut it, "var keep = new Intl.RelativeTimeFormat('en'); for (var i = 0; i < 50; i++) new Intl.RelativeTimeFormat('en'); 0;");
        assert_eq!(it.intl_formatters(), 51);
        it.collect(&[]).unwrap();
        assert_eq!(it.intl_formatters(), 1);
        assert_eq!(out(&mut it, "keep.format(1, 'day')"), "string:in 1 day");
    });
}

#[test]
fn the_formatter_cap_counts_both() {
    big(|| {
        let mut it = realm();
        it.set_sc_limits(ScLimits { max_formatters: 2, ..Default::default() });
        assert_eq!(out(&mut it, "var a = new Intl.Locale('en'); var b = new Intl.RelativeTimeFormat('en'); 0"), "number:0");
        assert_eq!(out(&mut it, "new Intl.Locale('en')"), "limit:Sc(Formatters)");
        assert_eq!(out(&mut it, "new Intl.RelativeTimeFormat('en')"), "limit:Sc(Formatters)");
    });
}

#[test]
fn a_tag_with_a_language_bare_has_no_data_for_still_works_as_a_locale() {
    big(|| {
        let mut it = realm();
        assert_eq!(out(&mut it, "var l = new Intl.Locale('fr-CA'); [l.language, l.region, l.maximize().toString(), l.minimize().toString()].join()"), "string:fr,CA,fr-Latn-CA,fr-CA");
        // a language that the likely-subtags subset lacks is returned as it is (V8 knows more)
        assert_eq!(out(&mut it, "new Intl.Locale('xx').maximize().toString()"), "string:xx");
    });
}
