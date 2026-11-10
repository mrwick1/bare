//! The tag logic of `Intl.Locale` (ECMA-402 section 14): the parts of a canonical tag, the options that replace them, and
//! `maximize` and `minimize`. Pure functions on strings: the builtins are in `eval_intl_locale.rs`. The canonical form comes
//! from `intl::canonical_tag` (CanonicalizeUnicodeLocaleId).
//!
//! Likely subtags (UTS 35 "Likely Subtags") are a table of 40 languages, not the whole CLDR table (about 1,900 rows, 60 KB or
//! more): bare has data for English only, so a page asks `maximize` for a locale it shows, mostly a common one. A language that
//! the table lacks stays as it is.

use crate::intl::{self, TagError};

/// The most likely script and region of a language, in the order of CLDR (a script or region alone, as in `und-Cyrl`, takes
/// the first row that has it).
const LIKELY: [(&str, &str, &str); 40] = [
    ("en", "Latn", "US"), ("es", "Latn", "ES"), ("fr", "Latn", "FR"), ("de", "Latn", "DE"), ("it", "Latn", "IT"), ("pt", "Latn", "BR"),
    ("nl", "Latn", "NL"), ("ru", "Cyrl", "RU"), ("uk", "Cyrl", "UA"), ("sr", "Cyrl", "RS"), ("pl", "Latn", "PL"), ("tr", "Latn", "TR"),
    ("ar", "Arab", "EG"), ("fa", "Arab", "IR"), ("ur", "Arab", "PK"), ("he", "Hebr", "IL"), ("hi", "Deva", "IN"), ("mr", "Deva", "IN"),
    ("bn", "Beng", "BD"), ("ta", "Taml", "IN"), ("te", "Telu", "IN"), ("ja", "Jpan", "JP"), ("ko", "Kore", "KR"), ("zh", "Hans", "CN"),
    ("th", "Thai", "TH"), ("vi", "Latn", "VN"), ("id", "Latn", "ID"), ("ms", "Latn", "MY"), ("sv", "Latn", "SE"), ("da", "Latn", "DK"),
    ("nb", "Latn", "NO"), ("no", "Latn", "NO"), ("fi", "Latn", "FI"), ("cs", "Latn", "CZ"), ("el", "Grek", "GR"), ("hu", "Latn", "HU"),
    ("ro", "Latn", "RO"), ("ca", "Latn", "ES"), ("sw", "Latn", "TZ"), ("bg", "Cyrl", "BG"),
];
/// A script that sets the region, or a region that sets the script, where the rule "the language's own script or region" is wrong.
const SCRIPT_REGION: [(&str, &str, &str); 2] = [("zh", "Hant", "TW"), ("sr", "Latn", "RS")];
const REGION_SCRIPT: [(&str, &str, &str); 3] = [("zh", "TW", "Hant"), ("zh", "HK", "Hant"), ("zh", "MO", "Hant")];

/// The pieces of a canonical tag: language, script, region, the variants (with their `-`) and the extensions (with their `-`).
pub struct Split<'a> {
    pub lang: &'a str,
    pub script: &'a str,
    pub region: &'a str,
    pub variants: &'a str,
    pub ext: &'a str,
}

pub fn split(tag: &str) -> Split<'_> {
    let ext_at = tag.match_indices('-').find(|(i, _)| tag.as_bytes().get(i + 2) == Some(&b'-') || tag.len() == i + 2).map_or(tag.len(), |(i, _)| i);
    let (base, ext) = tag.split_at(ext_at);
    let mut parts = base.splitn(2, '-');
    let lang = parts.next().unwrap_or("");
    let mut rest = parts.next().unwrap_or("");
    let mut take = |len: usize, digits: bool| {
        let p = rest.split('-').next().unwrap_or("");
        let ok = p.len() == len && if digits { p.bytes().all(|b| b.is_ascii_digit()) } else { p.bytes().all(|b| b.is_ascii_alphabetic()) };
        if ok {
            rest = rest[len..].strip_prefix('-').unwrap_or("");
        }
        if ok { p } else { "" }
    };
    let script = take(4, false);
    let region = match take(2, false) {
        "" => take(3, true),
        r => r,
    };
    let variants = if rest.is_empty() { "" } else { &base[base.len() - rest.len() - 1..] };
    Split { lang, script, region, variants, ext }
}

/// The `-u-` extension of a canonical tag as (attributes, keywords): a keyword is (key, type), the type "" for `true`.
fn unicode_ext(ext: &str) -> (Vec<&str>, Vec<(&str, String)>) {
    let mut attrs = Vec::new();
    let mut kws: Vec<(&str, String)> = Vec::new();
    let mut in_u = false;
    for p in ext.split('-').skip(1) {
        if p.len() == 1 {
            in_u = p == "u";
        } else if in_u && p.len() == 2 {
            kws.push((p, String::new()));
        } else if in_u && let Some(k) = kws.last_mut() {
            k.1.push_str(if k.1.is_empty() { "" } else { "-" });
            k.1.push_str(p);
        } else if in_u {
            attrs.push(p);
        }
    }
    (attrs, kws)
}

/// The type of the keyword `key` of a canonical tag, `None` when it is absent ("" for `true`).
pub fn keyword(tag: &str, key: &str) -> Option<String> {
    unicode_ext(split(tag).ext).1.into_iter().find(|k| k.0 == key).map(|k| k.1)
}

/// The tag up to its first extension (`language-script-region-variants`).
pub fn base_name(tag: &str) -> &str {
    let s = split(tag);
    &tag[..tag.len() - s.ext.len()]
}

/// A tag with other language, script and region (`""` keeps one) and other `-u-` keywords: (key, type) with "" for `true`. The
/// result is canonical. `Err` when a replaced part is not well formed or the tag is not.
pub fn apply(tag: &str, lang: &str, script: &str, region: &str, kws: &[(&str, String)]) -> Result<String, TagError> {
    let s = split(tag);
    let alpha = |p: &str| p.bytes().all(|b| b.is_ascii_alphabetic());
    if !(lang.is_empty() || alpha(lang) && matches!(lang.len(), 2 | 3 | 5..=8)) || !(script.is_empty() || alpha(script) && script.len() == 4) || !(region.is_empty() || alpha(region) && region.len() == 2 || region.len() == 3 && region.bytes().all(|b| b.is_ascii_digit())) {
        return Err(TagError::Malformed);
    }
    let pick = |new: &'_ str, old: &'_ str| if new.is_empty() { old.to_string() } else { new.to_string() };
    let mut out = pick(lang, s.lang);
    for p in [pick(script, s.script), pick(region, s.region)] {
        if !p.is_empty() {
            out.push('-');
            out.push_str(&p);
        }
    }
    out.push_str(s.variants);
    // the extensions, with the `-u-` keywords replaced
    let (attrs, mut old) = unicode_ext(s.ext);
    if kws.is_empty() {
        out.push_str(s.ext);
    } else {
        for (k, v) in kws {
            old.retain(|o| o.0 != *k);
            old.push((k, v.clone()));
        }
        // the other extensions stay; the `-u-` one is written again
        let mut other = String::new();
        let mut skip = false;
        for p in s.ext.split('-').skip(1) {
            if p.len() == 1 {
                skip = p == "u";
            }
            if !skip {
                other.push('-');
                other.push_str(p);
            }
        }
        out.push_str(&other);
        out.push_str("-u");
        for a in attrs {
            out.push('-');
            out.push_str(a);
        }
        for (k, v) in &old {
            out.push('-');
            out.push_str(k);
            if !v.is_empty() {
                out.push('-');
                out.push_str(v);
            }
        }
    }
    intl::canonical_tag(&out)
}

/// AddLikelySubtags with the table above.
pub fn maximize(tag: &str) -> String {
    let s = split(tag);
    let row = if s.lang == "und" {
        LIKELY.iter().find(|r| (s.script.is_empty() || r.1 == s.script) && (s.region.is_empty() || r.2 == s.region) && !(s.script.is_empty() && s.region.is_empty() && r.0 != "en"))
    } else {
        LIKELY.iter().find(|r| r.0 == s.lang)
    };
    let Some(&(l, rs, rr)) = row else { return tag.to_string() };
    let lang = if s.lang == "und" { l } else { s.lang };
    // script from the region, or region from the script, where the table's own pair is wrong
    let script = match (s.script, s.region) {
        ("", r) if !r.is_empty() => REGION_SCRIPT.iter().find(|x| x.0 == lang && x.1 == r).map_or(rs, |x| x.2),
        ("", _) => rs,
        (sc, _) => sc,
    };
    let region = match (s.region, s.script) {
        ("", sc) if !sc.is_empty() && sc != rs => SCRIPT_REGION.iter().find(|x| x.0 == lang && x.1 == sc).map_or("", |x| x.2),
        ("", _) => rr,
        (r, _) => r,
    };
    apply(tag, lang, script, region, &[]).unwrap_or_else(|_| tag.to_string())
}

/// RemoveLikelySubtags: the shortest of `language`, `language-region` and `language-script` that maximizes to the same tag.
pub fn minimize(tag: &str) -> String {
    let max = maximize(tag);
    let s = split(&max);
    for (script, region) in [("", ""), ("", s.region), (s.script, "")] {
        let mut t = s.lang.to_string();
        for p in [script, region] {
            if !p.is_empty() {
                t.push('-');
                t.push_str(p);
            }
        }
        if base_name(&maximize(&t)) == base_name(&max) {
            // the variants and the extensions stay
            return format!("{t}{}{}", s.variants, s.ext);
        }
    }
    max
}
