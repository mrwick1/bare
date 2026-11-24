use crate::script::run;

fn check(source: &str, expected: &str) {
    let mut vm = run(source).unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(vm.global_text("r"), expected);
}

#[test]
fn collation_uses_the_requested_options_and_caches_the_bound_compare() {
    check(
        r#"
        var coll = new Intl.Collator('en', {numeric: true, sensitivity: 'base'});
        var compare = coll.compare;
        var r = [compare === coll.compare, compare.name, compare.length,
                 compare('a2', 'A10'), 'é'.localeCompare('e', 'en', {sensitivity: 'base'})].join('|');
        "#,
        "true||2|-1|0",
    );
}

#[test]
fn date_time_format_uses_the_selected_zone_and_reports_options() {
    check(
        r#"
        var f = Intl.DateTimeFormat('en-GB', {timeZone: 'Asia/Kolkata',
            year: 'numeric', month: '2-digit', day: '2-digit'});
        var format = f.format;
        var o = f.resolvedOptions();
        var r = [format === f.format, format(0), o.locale, o.timeZone, o.month].join('|');
        "#,
        "true|01/01/1970|en-GB|Asia/Kolkata|2-digit",
    );
}

#[test]
fn display_names_uses_the_requested_type_and_fallback() {
    check(
        r#"
        var f = new Intl.DisplayNames('en', {type: 'region', fallback: 'none'});
        var r = [f.of('GB'), f.of('US'), typeof f.of('XZ')].join('|');
        "#,
        "United Kingdom|United States|undefined",
    );
}

#[test]
fn number_format_reads_options_in_order_and_formats_units() {
    check(
        r#"
        var reads = [];
        var f = new Intl.NumberFormat('en', {
            get style() { reads.push('style'); return 'unit'; },
            get unit() { reads.push('unit'); return 'megabyte'; },
            get maximumFractionDigits() { reads.push('digits'); return 1; },
            get useGrouping() { reads.push('grouping'); return false; }
        });
        var n = f.format;
        var supported = Intl.NumberFormat.supportedLocalesOf(['en-GB', 'fr', 'EN-gb', 'en-AU']);
        var r = [reads.join(), n === f.format, n(1024.25), supported.join()].join('|');
        "#,
        "style,unit,digits,grouping|true|1024.3 MB|en-GB,en-AU",
    );
}

#[test]
fn locale_segmenter_and_relative_time_use_the_shared_data() {
    check(
        r#"
        var l = new Intl.Locale('EN-latn-us', {numeric: true});
        var s = new Intl.Segmenter('en', {granularity: 'word'});
        var f = new Intl.RelativeTimeFormat('en', {numeric: 'auto'});
        var r = [l.toString(), l.baseName, l.language, l.numeric,
                 s.resolvedOptions().granularity, f.format(-1, 'days')].join('|');
        "#,
        "en-Latn-US-u-kn|en-Latn-US|en|true|word|yesterday",
    );
}

#[test]
fn frozen_instances_and_detached_format_functions_survive_collection() {
    let mut vm = run(r#"
        var f = Object.freeze(new Intl.NumberFormat('en'));
        var detached = f.format;
        var r = detached === f.format && !Reflect.ownKeys(f).length;
        f = null;
        new Intl.Collator('en');
    "#).unwrap();
    assert_eq!(vm.global_text("r"), "true");
    vm.realm.collect(&mut [], |_| {});
    assert_eq!(vm.realm.intl.as_ref().unwrap().formats.len(), 1);
    bare_js_api::enter(&mut vm.realm, |scope| {
        let value = scope.run_script("detached(1234)").unwrap();
        assert_eq!(scope.as_str(value).unwrap().to_string_lossy(), "1,234");
    });
}
