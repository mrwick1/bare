use crate::script::run;

fn check(source: &str, expected: &str) {
    let mut vm = run(source).unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(vm.global_text("r"), expected);
}

#[test]
fn cardinal_and_ordinal_categories_follow_the_rules() {
    check(
        r#"
        var c = new Intl.PluralRules('en'), o = new Intl.PluralRules('en', {type: 'ordinal'});
        var r = [[0, 1, 2, 1.5].map(n => c.select(n)).join(), new Intl.PluralRules('en', {minimumFractionDigits: 1}).select(1),
                 [1, 2, 3, 4, 11, 21, 22].map(n => o.select(n)).join(), c.select('1'), c.select(NaN)].join('|');
        "#,
        "other,one,other,other|other|one,two,few,other,other,one,two|one|other",
    );
}

#[test]
fn resolved_options_and_categories_match_v8() {
    check(
        r#"
        var p = new Intl.PluralRules('en', {type: 'ordinal'});
        var r = [JSON.stringify(p.resolvedOptions()),
                 JSON.stringify(new Intl.PluralRules('en', {maximumSignificantDigits: 3}).resolvedOptions()),
                 JSON.stringify(new Intl.PluralRules('en', {minimumFractionDigits: 1}).resolvedOptions().minimumFractionDigits)].join('|');
        "#,
        concat!(
            r#"{"locale":"en","type":"ordinal","minimumIntegerDigits":1,"minimumFractionDigits":0,"maximumFractionDigits":3,"pluralCategories":["few","one","two","other"],"roundingIncrement":1,"roundingMode":"halfExpand","roundingPriority":"auto","trailingZeroDisplay":"auto"}|"#,
            r#"{"locale":"en","type":"cardinal","minimumIntegerDigits":1,"minimumSignificantDigits":1,"maximumSignificantDigits":3,"pluralCategories":["one","other"],"roundingIncrement":1,"roundingMode":"halfExpand","roundingPriority":"auto","trailingZeroDisplay":"auto"}|1"#,
        ),
    );
}

#[test]
fn constructor_shape_and_errors() {
    check(
        r#"
        var P = Intl.PluralRules, e = [];
        try { P('en'); } catch (x) { e.push(x instanceof TypeError); }
        try { new P('en', {type: 'x'}); } catch (x) { e.push(x instanceof RangeError); }
        try { P.prototype.select.call({}, 1); } catch (x) { e.push(x instanceof TypeError); }
        var r = [e.join(), Object.prototype.toString.call(new P), P.length, P.prototype.select.length,
                 P.supportedLocalesOf(['en', 'xx']).join(), new P('en') instanceof P,
                 Object.getOwnPropertyNames(P.prototype).sort().join()].join('|');
        "#,
        "true,true,true|[object Intl.PluralRules]|0|1|en|true|constructor,resolvedOptions,select",
    );
}
