//! `Intl.ListFormat` and `Intl.DisplayNames` tests. The expected values are what Node 22 (V8, ICU) gives, except where a
//! comment names a deviation.

use super::intl_tests::{big, out, realm};

fn all(cases: &[(&str, &str)]) {
    let cases: Vec<(String, String)> = cases.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
    big(move || {
        let mut it = realm();
        let mut bad = Vec::new();
        for (src, want) in &cases {
            let got = out(&mut it, src);
            if &got != want {
                bad.push(format!("{src}\n  want {want}\n  got  {got}"));
            }
        }
        assert!(bad.is_empty(), "{} differences:\n{}", bad.len(), bad.join("\n"));
    });
}

#[test]
fn list_format_patterns_of_each_type_and_style() {
    let f = |l: &str, t: &str, s: &str| format!("var f = new Intl.ListFormat('{l}', {{ type: '{t}', style: '{s}' }}); [f.format(['a', 'b']), f.format(['a', 'b', 'c']), f.format(['a', 'b', 'c', 'd'])].join('|')");
    let rows = [
        ("en", "conjunction", "long", "a and b|a, b, and c|a, b, c, and d"),
        ("en", "conjunction", "short", "a & b|a, b, & c|a, b, c, & d"),
        ("en", "conjunction", "narrow", "a, b|a, b, c|a, b, c, d"),
        ("en-US", "disjunction", "long", "a or b|a, b, or c|a, b, c, or d"),
        ("en", "disjunction", "narrow", "a or b|a, b, or c|a, b, c, or d"),
        ("en", "unit", "long", "a, b|a, b, c|a, b, c, d"),
        ("en", "unit", "short", "a, b|a, b, c|a, b, c, d"),
        ("en", "unit", "narrow", "a b|a b c|a b c d"),
        ("en-GB", "conjunction", "long", "a and b|a, b and c|a, b, c and d"),
        ("en-GB", "conjunction", "short", "a and b|a, b and c|a, b, c and d"),
        ("en-CA", "disjunction", "short", "a or b|a, b or c|a, b, c or d"),
        ("en-IN", "conjunction", "narrow", "a, b|a, b, and c|a, b, c, and d"),
        ("en-IN", "unit", "narrow", "a b|a b c|a b c d"),
    ];
    let cases: Vec<(String, String)> = rows.iter().map(|(l, t, s, w)| (f(l, t, s), format!("string:{w}"))).collect();
    let refs: Vec<(&str, &str)> = cases.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    all(&refs);
}

#[test]
fn list_format_api() {
    all(&[
        ("typeof Intl.ListFormat + typeof Intl.DisplayNames", "string:functionfunction"),
        ("[Intl.ListFormat.length, Intl.DisplayNames.length, Intl.ListFormat.prototype.format.length, Intl.DisplayNames.prototype.of.length].join()", "string:0,2,1,1"),
        ("Object.prototype.toString.call(new Intl.ListFormat())", "string:[object Intl.ListFormat]"),
        ("JSON.stringify(new Intl.ListFormat().resolvedOptions())", r#"string:{"locale":"en-US","type":"conjunction","style":"long"}"#),
        ("JSON.stringify(new Intl.ListFormat('en-GB', { type: 'unit', style: 'narrow' }).resolvedOptions())", r#"string:{"locale":"en-GB","type":"unit","style":"narrow"}"#),
        ("JSON.stringify(new Intl.ListFormat().formatToParts(['x', 'y', 'z']))", r#"string:[{"type":"element","value":"x"},{"type":"literal","value":", "},{"type":"element","value":"y"},{"type":"literal","value":", and "},{"type":"element","value":"z"}]"#),
        ("var f = new Intl.ListFormat(); [f.format([]), f.format(['q']), f.format(), f.format(new Set(['a', 'b']))].join('|')", "string:|q||a and b"),
        ("new Intl.ListFormat().format([1])", "throw:TypeError:Iterable yielded 1 which is not a string"),
        ("new Intl.ListFormat('en', { type: 'x' })", "throw:RangeError:Value x out of range for Intl.ListFormat options property type"),
        ("Intl.ListFormat()", "throw:TypeError:Constructor Intl.ListFormat requires 'new'"),
        ("new Intl.ListFormat('en', null)", "throw:TypeError:invalid_argument"),
        ("new Intl.ListFormat('en', 5)", "throw:TypeError:invalid_argument"),
        ("Intl.ListFormat.prototype.format.call({}, [])", "throw:TypeError:Method Intl.ListFormat.prototype.format called on incompatible receiver #<Object>"),
        ("Intl.ListFormat.supportedLocalesOf(['en-GB', 'de']).join()", "string:en-GB"),
        // another locale falls back to English, as the other Intl objects of bare do
        ("new Intl.ListFormat('de').format(['a', 'b'])", "string:a and b"),
    ]);
}

#[test]
fn display_names_of_each_type() {
    let d = |t: &str, extra: &str, code: &str| format!("new Intl.DisplayNames('en', {{ type: '{t}'{extra} }}).of('{code}')");
    let rows = [
        ("region", "", "US", "string:United States"),
        ("region", "", "GB", "string:United Kingdom"),
        ("region", "", "419", "string:Latin America"),
        ("region", "", "001", "string:world"),
        ("region", "", "ZZ", "string:Unknown Region"),
        ("region", ", style: 'short'", "US", "string:US"),
        ("region", ", style: 'narrow'", "GB", "string:UK"),
        ("region", ", style: 'short'", "HK", "string:Hong Kong"),
        ("region", "", "AA", "string:AA"),
        ("region", ", fallback: 'none'", "AA", "undefined"),
        ("region", "", "999", "string:999"),
        // V8 gives the lowercase code back ("in"); the spec makes it upper case first, and bare follows the spec
        ("region", "", "in", "string:India"),
        ("script", "", "Latn", "string:Latin"),
        ("script", "", "hans", "string:Simplified"),
        ("script", "", "Qaaa", "string:Qaaa"),
        ("currency", "", "USD", "string:US Dollar"),
        ("currency", "", "EUR", "string:Euro"),
        ("currency", "", "XYZ", "string:XYZ"),
        ("currency", ", fallback: 'none'", "XYZ", "undefined"),
        ("language", "", "en", "string:English"),
        ("language", "", "EN", "string:English"),
        ("language", "", "fr", "string:French"),
        ("language", "", "en-US", "string:American English"),
        ("language", ", style: 'short'", "en-US", "string:US English"),
        ("language", ", style: 'short'", "en-GB", "string:UK English"),
        ("language", "", "pt-BR", "string:Brazilian Portuguese"),
        ("language", "", "es-419", "string:Latin American Spanish"),
        ("language", "", "zh-Hans", "string:Simplified Chinese"),
        ("language", "", "zh-Hant-TW", "string:Chinese (Traditional, Taiwan)"),
        ("language", "", "en-Latn-US", "string:English (Latin, United States)"),
        ("language", ", style: 'short'", "en-Latn-GB", "string:English (Latin, UK)"),
        ("language", "", "en-XX", "string:English (XX)"),
        ("language", "", "zh-Hans-CN", "string:Chinese (Simplified, China)"),
        ("language", "", "sr-Latn", "string:Serbian (Latin)"),
        ("language", "", "ar-001", "string:Modern Standard Arabic"),
        ("language", "", "en-150", "string:English (Europe)"),
        ("calendar", "", "islamic-civil", "string:Hijri Calendar (tabular, civil epoch)"),
        ("script", "", "Qaaa", "string:Qaaa"),
        ("language", "", "en-ZZ", "string:English (Unknown Region)"),
        ("language", "", "xx", "string:xx"),
        ("language", "", "xx-US", "string:xx (United States)"),
        ("language", ", fallback: 'none'", "xx", "undefined"),
        ("language", ", fallback: 'none'", "en-XX", "undefined"),
        ("language", ", languageDisplay: 'standard'", "en-US", "string:English (United States)"),
        ("language", ", languageDisplay: 'standard'", "zh-Hans", "string:Chinese (Simplified)"),
        ("calendar", "", "gregory", "string:Gregorian Calendar"),
        ("dateTimeField", "", "weekday", "string:day of the week"),
    ];
    let cases: Vec<(String, String)> = rows.iter().map(|(t, e, c, w)| (d(t, e, c), w.to_string())).collect();
    let refs: Vec<(&str, &str)> = cases.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    all(&refs);
}

#[test]
fn display_names_api_and_errors() {
    all(&[
        ("Object.prototype.toString.call(new Intl.DisplayNames('en', { type: 'region' }))", "string:[object Intl.DisplayNames]"),
        ("JSON.stringify(new Intl.DisplayNames(undefined, { type: 'language' }).resolvedOptions())", r#"string:{"locale":"en-US","style":"long","type":"language","fallback":"code","languageDisplay":"dialect"}"#),
        ("JSON.stringify(new Intl.DisplayNames('en-GB', { type: 'region', style: 'short', fallback: 'none' }).resolvedOptions())", r#"string:{"locale":"en-GB","style":"short","type":"region","fallback":"none"}"#),
        ("new Intl.DisplayNames('en')", "throw:TypeError:invalid_argument"),
        ("new Intl.DisplayNames('en', {})", "throw:TypeError:invalid_argument"),
        ("new Intl.DisplayNames('en', 5)", "throw:TypeError:invalid_argument"),
        ("Intl.DisplayNames('en', { type: 'region' })", "throw:TypeError:Constructor Intl.DisplayNames requires 'new'"),
        ("new Intl.DisplayNames('en', { type: 'x' })", "throw:RangeError:Value x out of range for Intl.DisplayNames options property type"),
        ("new Intl.DisplayNames('en', { type: 'region' }).of('xyz')", "throw:RangeError:invalid_argument"),
        ("new Intl.DisplayNames('en', { type: 'region' }).of()", "throw:RangeError:invalid_argument"),
        ("new Intl.DisplayNames('en', { type: 'script' }).of('L')", "throw:RangeError:invalid_argument"),
        ("new Intl.DisplayNames('en', { type: 'currency' }).of('us')", "throw:RangeError:invalid_argument"),
        ("new Intl.DisplayNames('en', { type: 'language' }).of('e')", "throw:RangeError:invalid_argument"),
        ("new Intl.DisplayNames('en', { type: 'language' }).of('en-u-ca-gregory')", "throw:RangeError:invalid_argument"),
        ("Intl.DisplayNames.prototype.of.call({}, 'US')", "throw:TypeError:Method Intl.DisplayNames.prototype.of called on incompatible receiver #<Object>"),
        ("Intl.DisplayNames.supportedLocalesOf('en-IN').join()", "string:en-IN"),
    ]);
}

#[test]
fn list_and_display_names_objects_are_collected_and_capped() {
    big(|| {
        let mut it = realm();
        out(&mut it, "var a = new Intl.ListFormat(), b = new Intl.DisplayNames('en', { type: 'region' }); for (var i = 0; i < 20; i++) { new Intl.ListFormat(); new Intl.DisplayNames('en', { type: 'script' }); } 0;");
        assert_eq!(it.intl_formatters(), 42);
        it.collect(&[]).unwrap();
        assert_eq!(it.intl_formatters(), 2);
        assert_eq!(out(&mut it, "a.format(['x', 'y']) + '|' + b.of('FR')"), "string:x and y|France");
    });
}

#[test]
fn the_formatter_cap_counts_list_formats_and_display_names() {
    use super::*;
    big(|| {
        let mut it = realm();
        it.set_sc_limits(ScLimits { max_formatters: 2, ..Default::default() });
        assert_eq!(out(&mut it, "var a = new Intl.ListFormat(); var b = new Intl.DisplayNames('en', { type: 'region' }); 0"), "number:0");
        assert_eq!(out(&mut it, "new Intl.ListFormat()"), "limit:Sc(Formatters)");
        assert_eq!(out(&mut it, "new Intl.DisplayNames('en', { type: 'region' })"), "limit:Sc(Formatters)");
    });
}
