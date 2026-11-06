//! Intl.NumberFormat, Intl.Collator, Intl.PluralRules, `toLocaleString` and `localeCompare` tests. What Node can show is in
//! `tools/fixtures/js/intl-num-cases.txt` (`tools/js/intl_num_check.sh`). The tests below cover what Node cannot show: the lazy
//! `Intl`, GC of the objects, the cap and the gates.

use super::intl_tests::{big, out, realm};
use super::*;

#[test]
fn intl_is_made_on_first_read_and_stays_one_object() {
    big(|| {
        let mut it = realm();
        let before = it.heap.stats().live_objects;
        // the install makes no Intl object: a getter stands in for it
        assert!(before < 4000);
        assert_eq!(out(&mut it, "typeof Intl"), "string:object");
        assert_eq!(out(&mut it, "Intl === globalThis.Intl && Intl.NumberFormat === Intl.NumberFormat"), "boolean:true");
        // a write replaces it
        assert_eq!(out(&mut it, "Intl = 5; Intl"), "number:5");
        let mut it = realm();
        assert_eq!(out(&mut it, "Intl = 7; Intl"), "number:7");
        let mut it = realm();
        assert_eq!(out(&mut it, "(1234.5).toLocaleString('en-IN')"), "string:1,234.5");
        assert_eq!(out(&mut it, "typeof Intl.Collator"), "string:function");
    });
}

#[test]
fn indian_grouping_through_to_locale_string() {
    big(|| {
        let mut it = realm();
        assert_eq!(out(&mut it, "(1234567.89).toLocaleString('en-IN')"), "string:12,34,567.89");
        assert_eq!(out(&mut it, "['b', 'a', 'é', 'e', 'z'].sort(function (x, y) { return x.localeCompare(y); }).join()"), "string:a,b,e,é,z");
    });
}

#[test]
fn number_formatters_collators_and_rules_are_collected_when_dead() {
    big(|| {
        let mut it = realm();
        out(&mut it, "var nf = new Intl.NumberFormat('en-IN'), f = nf.format, c = new Intl.Collator('en'), cmp = c.compare, p = new Intl.PluralRules('en');");
        out(&mut it, "for (var i = 0; i < 40; i++) { new Intl.NumberFormat('en'); new Intl.Collator('en'); new Intl.PluralRules('en'); } 0;");
        assert_eq!(it.intl_tracked_num(), 123);
        it.collect(&[]).unwrap();
        assert_eq!(it.intl_tracked_num(), 3);
        // the bound functions keep their objects and give the same function after a collect
        out(&mut it, "nf = null; c = null; p = null;");
        it.collect(&[]).unwrap();
        assert_eq!(it.intl_tracked_num(), 2);
        assert_eq!(out(&mut it, "f(1234567.5) + '|' + cmp('a', 'B')"), "string:12,34,567.5|-1");
        out(&mut it, "f = null; cmp = null;");
        it.collect(&[]).unwrap();
        assert_eq!(it.intl_tracked_num(), 0);
    });
}

#[test]
fn objects_of_all_kinds_share_the_cap_and_are_not_made_past_it() {
    big(|| {
        let mut it = realm();
        it.set_sc_limits(ScLimits { max_formatters: 3, ..Default::default() });
        assert_eq!(out(&mut it, "var a = [new Intl.NumberFormat(), new Intl.Collator(), new Intl.PluralRules()]; a.length"), "number:3");
        let objects = it.heap.stats().live_objects;
        for src in ["new Intl.NumberFormat()", "new Intl.Collator()", "new Intl.PluralRules()", "new Intl.DateTimeFormat()"] {
            assert_eq!(out(&mut it, src), "limit:Sc(Formatters)", "{src}");
        }
        assert_eq!(it.heap.stats().live_objects, objects, "no object is made past the cap");
        // toLocaleString and localeCompare make no tracked object
        assert_eq!(out(&mut it, "(1234.5).toLocaleString('en-US') + '|' + 'a'.localeCompare('b', 'en', { numeric: true })"), "string:1,234.5|-1");
        assert_eq!(it.intl_tracked_num(), 3);
    });
}

#[test]
fn gates_refuse_what_bare_cannot_format() {
    big(|| {
        let mut it = realm();
        for (src, what) in [
            ("new Intl.NumberFormat('en', { style: 'unit', unit: 'meter' })", "Intl.NumberFormat style unit"),
            ("new Intl.NumberFormat('en', { notation: 'compact' })", "Intl.NumberFormat option notation (only standard)"),
            ("new Intl.NumberFormat('en', { notation: 'scientific' })", "Intl.NumberFormat option notation (only standard)"),
            ("new Intl.NumberFormat('en', { style: 'currency', currency: 'USD', currencyDisplay: 'name' })", "Intl.NumberFormat option currencyDisplay name"),
            ("new Intl.NumberFormat('en', { numberingSystem: 'arab' })", "Intl.NumberFormat option numberingSystem"),
            ("new Intl.NumberFormat('en', { roundingIncrement: 5, maximumFractionDigits: 2, minimumFractionDigits: 2 })", "Intl.NumberFormat option roundingIncrement"),
            ("new Intl.NumberFormat('en').formatRange(1, 2)", "Intl.NumberFormat.prototype.formatRange"),
            ("new Intl.NumberFormat('en-u-nu-arab')", "Intl: a locale with an extension (-u-, -t-, -x-)"),
            ("(1).toLocaleString('en', { notation: 'compact' })", "Intl.NumberFormat option notation (only standard)"),
            ("new Intl.Collator('en', { usage: 'search' })", "Intl.Collator option usage search"),
            ("new Intl.Collator('en', { collation: 'phonebk' })", "Intl.Collator option collation"),
            ("'a'.localeCompare('b', 'en', { collation: 'phonebk' })", "Intl.Collator option collation"),
            ("new Intl.PluralRules('en', { roundingIncrement: 5 })", "Intl.NumberFormat option roundingIncrement"),
        ] {
            let r = out(&mut it, src);
            assert!(r.starts_with("unsupported:") && r.contains(what), "{src}: {r}");
        }
    });
}

#[test]
fn plural_rules_need_new_and_follow_the_english_rules() {
    big(|| {
        let mut it = realm();
        assert_eq!(out(&mut it, "try { Intl.PluralRules('en') } catch (e) { e.message }"), "string:Constructor Intl.PluralRules requires 'new'");
        assert_eq!(out(&mut it, "var p = new Intl.PluralRules('en', { type: 'ordinal' }); [1, 2, 3, 4, 11, 21, 22].map(function (n) { return p.select(n); }).join()"), "string:one,two,few,other,other,one,two");
        assert_eq!(out(&mut it, "var q = new Intl.PluralRules('en'); [0, 1, 1.5, 2].map(function (n) { return q.select(n); }).join()"), "string:other,one,other,other");
    });
}

#[test]
fn a_loop_of_number_calls_does_not_grow_the_heap() {
    big(|| {
        let mut it = realm();
        out(&mut it, "var s = 0; for (var i = 0; i < 300; i++) { s += (i * 1234.5).toLocaleString('en-IN').length + 'a'.localeCompare('b'); } s");
        it.collect(&[]).unwrap();
        let a = it.heap.stats().live_objects;
        out(&mut it, "for (var i = 0; i < 3000; i++) { s += (i * 1234.5).toLocaleString('en-IN').length + 'a'.localeCompare('b'); } s");
        it.collect(&[]).unwrap();
        assert_eq!(it.heap.stats().live_objects, a);
        assert_eq!(it.intl_tracked_num(), 0);
    });
}

#[test]
fn every_option_name_has_its_index() {
    for (i, n) in OPTION_NAMES.iter().enumerate() {
        assert_eq!(option_index(n), Some(i), "{n}");
    }
    assert_eq!(option_index("nope"), None);
}

#[test]
fn segmenter_objects_are_collected_and_capped() {
    big(|| {
        let mut it = realm();
        out(&mut it, "var g = new Intl.Segmenter().segment('abc'), i = g[Symbol.iterator](); i.next();");
        out(&mut it, "for (var k = 0; k < 20; k++) { new Intl.Segmenter().segment('x' + k)[Symbol.iterator](); } 0;");
        assert!(it.intl.segs.len() >= 21 && it.intl.segments.len() >= 21 && it.intl.seg_iters.len() >= 21);
        it.collect(&[]).unwrap();
        assert_eq!((it.intl.segs.len(), it.intl.segments.len(), it.intl.seg_iters.len()), (0, 1, 1));
        assert_eq!(out(&mut it, "i.next().value.segment + g.containing(2).segment"), "string:bc");
        out(&mut it, "g = null; i = null;");
        it.collect(&[]).unwrap();
        assert_eq!((it.intl.segs.len(), it.intl.segments.len(), it.intl.seg_iters.len()), (0, 0, 0));
        let mut it = realm();
        it.set_sc_limits(ScLimits { max_iterators: 3, ..Default::default() });
        assert_eq!(out(&mut it, "var g = new Intl.Segmenter().segment('abc'); g[Symbol.iterator](); g[Symbol.iterator](); 1"), "number:1");
        assert_eq!(out(&mut it, "g[Symbol.iterator]()"), "limit:Sc(Iterators)");
        let mut it = realm();
        it.set_sc_limits(ScLimits { max_formatters: 1, ..Default::default() });
        assert_eq!(out(&mut it, "new Intl.Segmenter(); new Intl.Segmenter()"), "limit:Sc(Formatters)");
    });
}

#[test]
fn segmenter_input_is_charged_and_long_strings_work() {
    big(|| {
        let mut it = realm();
        assert_eq!(out(&mut it, "var s = new Intl.Segmenter('en', { granularity: 'word' }).segment('ab '.repeat(5000)); [...s].length"), "number:10000");
        assert_eq!(out(&mut it, "s.containing(14999).segment"), "string: ");
        assert_eq!(out(&mut it, "s.containing(15000)"), "undefined");
    });
}
