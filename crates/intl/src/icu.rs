//! Numbers, plural rules and collation over ICU4X, for `Intl.NumberFormat`, `Intl.PluralRules`, `Intl.Collator`,
//! `Number.prototype.toLocaleString` and `String.prototype.localeCompare`. Pure functions: the builtins are in
//! `eval_intl_num.rs`.
//!
//! - Locales: the 5 English locales of `intl.rs`. ICU4X loads the digits, the separators and the grouping sizes of the
//!   locale (`en-IN` groups 3 then 2). The sign, the percent sign and the currency patterns are the same in these 5 locales, so
//!   this file writes them.
//! - Rounding is ECMA-402 `FormatNumericToString`, done on the shortest decimal digits of the double, as ICU does (1.005 is
//!   "1.01"). A string argument keeps its exact decimal digits.
//! - Collation: the root collation (the English tailoring is empty).
//! - Data: ICU4X compiled data. The choice and its measurements are in `docs/internal/resources.md`.

use crate::intl_data::LOCALES;
use crate::intl_num_data::{DIGITS, NARROW, SYMBOLS, SYMBOLS_LOCALE};
use crate::packed::pack_table;
use fixed_decimal::{Decimal, Sign, SignDisplay, SignedRoundingMode, UnsignedRoundingMode};
use icu_collator::options::{AlternateHandling, CaseLevel, CollatorOptions, Strength};
use icu_collator::preferences::{CollationCaseFirst, CollationNumericOrdering};
use icu_collator::{Collator, CollatorPreferences};
use icu_decimal::options::GroupingStrategy;
use icu_decimal::DecimalFormatter;
use icu_locale_core::{Locale, locale};
use icu_plurals::{PluralCategory, PluralRules};
use std::rc::Rc;
use writeable::{Part, PartsWrite, Writeable};

// ------------------------------------------------------------ options

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Style {
    Decimal,
    Percent,
    Currency,
}

/// Which digit rule rounds: the fraction digits, the significant digits, or the one with more or fewer digits.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Rounding {
    Fraction,
    Significant,
    MorePrecision,
    LessPrecision,
}

/// The `useGrouping` values. `Never` is `false`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Grouping {
    Auto,
    Always,
    Min2,
    Never,
}

/// `currencyDisplay`: symbol, narrowSymbol, code.
pub const SYMBOL: u8 = 0;
pub const NARROW_SYMBOL: u8 = 1;
pub const CODE: u8 = 2;

/// The resolved options of a number formatter or a plural rules object. Digits: `fd` and `sd` are (minimum, maximum).
#[derive(Clone, Copy, Debug)]
pub struct NumFmt {
    pub unit: u8,
    pub per_unit: u8,
    pub unit_display: u8,
    pub loc: u8,
    pub style: Style,
    pub currency: [u8; 3],
    pub display: u8,
    pub accounting: bool,
    pub min_int: u8,
    pub rounding: Rounding,
    pub fd: (u8, u8),
    pub sd: (u8, u8),
    pub grouping: Grouping,
    pub sign: SignDisplay,
    /// Index of `ROUNDING_MODES`.
    pub mode: u8,
    pub strip_zero: bool,
    pub notation: u8,
    pub compact_long: bool,
}

/// The `roundingMode` values, in the order of ECMA-402 (the default `halfExpand` is index 6).
pub const ROUNDING_MODES: [&str; 9] = ["ceil", "floor", "expand", "trunc", "halfCeil", "halfFloor", "halfExpand", "halfTrunc", "halfEven"];
pub const HALF_EXPAND: u8 = 6;

fn mode_of(i: u8) -> SignedRoundingMode {
    use UnsignedRoundingMode::*;
    match i {
        0 => SignedRoundingMode::Ceil,
        1 => SignedRoundingMode::Floor,
        2 => SignedRoundingMode::Unsigned(Expand),
        3 => SignedRoundingMode::Unsigned(Trunc),
        4 => SignedRoundingMode::HalfCeil,
        5 => SignedRoundingMode::HalfFloor,
        6 => SignedRoundingMode::Unsigned(HalfExpand),
        7 => SignedRoundingMode::Unsigned(HalfTrunc),
        _ => SignedRoundingMode::Unsigned(HalfEven),
    }
}

// The generated currency tables use three ASCII bytes per key.
const fn currency_key(code: &str) -> [u8; 3] {
    let [a, b, c] = code.as_bytes() else { panic!("invalid currency key") };
    [*a, *b, *c]
}

/// The currency codes of the symbol table, sorted.
pub fn currencies() -> impl Iterator<Item = &'static str> {
    SYMBOLS.iter().map(|s| s.0)
}

/// The digits after the point of currency `code` (2 unless the table says otherwise).
pub fn currency_digits(code: &str) -> u8 {
    let Ok(key) = <[u8; 3]>::try_from(code.as_bytes()) else { return 2 };
    let table = pack_table!(DIGITS, 0, ([u8; 3], u8), |i| ([], (currency_key(DIGITS[i].0), DIGITS[i].1)));
    table.search_by(|(a, _)| a.cmp(&key)).map_or(2, |i| table.meta(i).1)
}

/// The text of a currency and whether a no-break space separates it from the digits.
fn currency_text(loc: u8, code: &str, display: u8) -> (&str, bool) {
    if display == CODE {
        return (code, true);
    }
    let Ok(key) = <[u8; 3]>::try_from(code.as_bytes()) else { return (code, true) };
    let narrow = pack_table!(NARROW, 1, ([u8; 3], bool), |i| {
        let (code, text, space) = NARROW[i];
        ([text], (currency_key(code), space))
    });
    if display == NARROW_SYMBOL && let Some(i) = narrow.search_by(|(a, _)| a.cmp(&key)) {
        return (narrow.text(i, 0), narrow.meta(i).1);
    }
    // the narrow symbol is the same in every locale here (the generator checks it)
    let local = pack_table!(SYMBOLS_LOCALE, 1, (u8, [u8; 3], bool), |i| {
        let (locale, code, text, space) = SYMBOLS_LOCALE[i];
        ([text], (locale, currency_key(code), space))
    });
    if display != NARROW_SYMBOL
        && let Some(i) = (0..local.len()).find(|&i| {
            let (locale, currency, _) = local.meta(i);
            locale == loc && currency == key
        }) {
        return (local.text(i, 0), local.meta(i).2);
    }
    let symbols = pack_table!(SYMBOLS, 1, ([u8; 3], bool), |i| {
        let (code, text, space) = SYMBOLS[i];
        ([text], (currency_key(code), space))
    });
    if let Some(i) = symbols.search_by(|(a, _)| a.cmp(&key)) {
        return (symbols.text(i, 0), symbols.meta(i).1);
    }
    (code, true)
}

// ------------------------------------------------------------ numbers

/// A number to format: not a number, an infinity (true when negative), or a finite decimal with its sign.
pub enum Num {
    Nan,
    Inf(bool),
    Fin(Decimal),
}

/// The finite value of a Number: its shortest round-trip digits (ICU reads a double the same way).
pub fn num_from_f64(x: f64) -> Num {
    if x.is_nan() {
        return Num::Nan;
    }
    if x.is_infinite() {
        return Num::Inf(x < 0.0);
    }
    let mut d = if x.abs() < 9007199254740992.0 && x.fract() == 0.0 {
        Decimal::from(x.abs() as i64)
    } else {
        // `number_to_string` is the shortest form, with an exponent for the very large and the very small
        let s = crate::number_to_string(x.abs()).replace("e+", "e");
        Decimal::try_from_str(&s).unwrap_or_default()
    };
    d.set_sign(if x.is_sign_negative() { Sign::Negative } else { Sign::None });
    Num::Fin(d)
}

/// The exact value of a string that is a plain decimal (`-12.50`): ToIntlMathematicalValue keeps all its digits.
pub fn num_from_decimal_str(s: &str) -> Option<Num> {
    let t = s.strip_prefix(['+', '-']).unwrap_or(s);
    let (int, frac) = t.split_once('.').unwrap_or((t, "0"));
    let digits = |p: &str| !p.is_empty() && p.bytes().all(|c| c.is_ascii_digit());
    if !digits(int) || !digits(frac) {
        return None;
    }
    let mut d = Decimal::try_from_str(t).ok()?;
    d.set_sign(if s.starts_with('-') { Sign::Negative } else { Sign::None });
    Some(Num::Fin(d))
}

/// Position of the most significant digit (0 for zero).
fn magnitude(d: &Decimal) -> i16 {
    d.absolute.nonzero_magnitude_start()
}

include!("intl_unit_data.rs");

pub(crate) fn unit_index(unit: &str) -> Option<u8> {
    let names = pack_table!(UNIT_NAMES, 1, (), |i| ([UNIT_NAMES[i]], ()));
    names.search(unit).map(|i| i as u8)
}

impl NumFmt {
    /// Round `d` (signed) as ECMA-402 FormatNumericToString does, and pad it.
    fn round(&self, d: &mut Decimal) {
        let mode = mode_of(self.mode);
        let by_fraction = |d: &mut Decimal| {
            d.round_with_mode(-(self.fd.1 as i16), mode);
            d.absolute.trim_end();
            d.absolute.pad_end(-(self.fd.0 as i16));
            -(self.fd.1 as i16)
        };
        let by_significant = |d: &mut Decimal| {
            let e = magnitude(d);
            d.round_with_mode(e - self.sd.1 as i16 + 1, mode);
            // A carry (9.96 to 10.0) moves the most significant digit up: the rounded value has `max` digits, not `max + 1`
            let e2 = magnitude(d);
            if e2 > e {
                d.absolute.trim_end();
            }
            d.absolute.trim_end();
            d.absolute.pad_end(e2 - self.sd.0 as i16 + 1);
            e2 - self.sd.1 as i16 + 1
        };
        match self.rounding {
            Rounding::Fraction => {
                by_fraction(d);
            }
            Rounding::Significant => {
                by_significant(d);
            }
            Rounding::MorePrecision | Rounding::LessPrecision => {
                let (mut f, mut s) = (d.clone(), d.clone());
                let (pf, ps) = (by_fraction(&mut f), by_significant(&mut s));
                // the smaller position is the one with more digits
                let take_s = if self.rounding == Rounding::MorePrecision { ps <= pf } else { ps > pf };
                *d = if take_s { s } else { f };
            }
        }
        if self.strip_zero {
            d.absolute.trim_end_if_integer();
        }
        d.absolute.pad_start(self.min_int as i16);
    }

    /// Does the sign display show a sign for a value with sign `neg`, `zero` after rounding?
    fn sign_char(&self, neg: bool, zero: bool) -> Option<(&'static str, &'static str)> {
        const MINUS: Option<(&str, &str)> = Some(("-", "minusSign"));
        const PLUS: Option<(&str, &str)> = Some(("+", "plusSign"));
        match self.sign {
            SignDisplay::Never => None,
            SignDisplay::Always => if neg { MINUS } else { PLUS },
            SignDisplay::ExceptZero => if zero { None } else if neg { MINUS } else { PLUS },
            SignDisplay::Negative => if neg && !zero { MINUS } else { None },
            _ => if neg { MINUS } else { None },
        }
    }
}

/// The text and the parts (type, end offset) of a formatted number. A part is merged into the one before it when the type is
/// the same.
pub struct Out<'a> {
    pub s: &'a mut String,
    pub parts: Option<&'a mut Vec<(&'static str, usize)>>,
}

impl Out<'_> {
    fn push(&mut self, ty: &'static str, text: &str) {
        self.s.push_str(text);
        if let Some(p) = &mut self.parts {
            let end = self.s.len();
            match p.last_mut() {
                Some(last) if last.0 == ty => last.1 = end,
                _ => p.push((ty, end)),
            }
        }
    }
}

/// A sink for the parts of ICU4X. A nested part (a group separator inside the integer) takes the text it covers.
struct Sink<'a, 'b> {
    out: &'a mut Out<'b>,
    stack: [&'static str; 3],
    depth: usize,
}

impl core::fmt::Write for Sink<'_, '_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let ty = if self.depth == 0 { "literal" } else { self.stack[self.depth - 1] };
        self.out.push(ty, s);
        Ok(())
    }
}

impl PartsWrite for Sink<'_, '_> {
    type SubPartsWrite = Self;
    fn with_part(&mut self, part: Part, mut f: impl FnMut(&mut Self) -> core::fmt::Result) -> core::fmt::Result {
        self.stack[self.depth] = part.value;
        self.depth += 1;
        let r = f(self);
        self.depth -= 1;
        r
    }
}

/// ICU4X data for the 5 English locales and the root collation, made by `tools/js/icu_data_gen.sh`.
#[allow(unsafe_code, dead_code, unused_imports, clippy::all)]
mod baked {
    include!("icu_data/mod.rs");
    pub struct Baked;
    impl_data_provider!(Baked);
}
use baked::Baked;

/// `String.prototype.normalize` to NFKC (`compose`) or NFKD, with the baked data.
pub fn normalize_compat(s: &str, compose: bool) -> String {
    if compose {
        icu_normalizer::ComposingNormalizer::try_new_nfkc_unstable(&Baked).expect("baked NFKC data").as_borrowed().normalize(s).into_owned()
    } else {
        icu_normalizer::DecomposingNormalizer::try_new_nfkd_unstable(&Baked).expect("baked NFKD data").as_borrowed().normalize(s).into_owned()
    }
}

/// Formatters that the evaluator makes once and keeps: a decimal formatter for each locale and grouping, and the plural rules.
#[derive(Default)]
pub struct Cache {
    dec: [[Option<DecimalFormatter>; 4]; LOCALES.len()],
    plurals: [Option<PluralRules>; 2],
    colls: Vec<(CollKey, Rc<Collator>)>,
}

fn icu_locale(loc: u8) -> &'static Locale {
    // This order matches intl_data::LOCALES.
    static ICU_LOCALES: [Locale; LOCALES.len()] = [
        locale!("en-US"), locale!("en"), locale!("en-CA"), locale!("en-IN"), locale!("en-GB"),
    ];
    &ICU_LOCALES[loc as usize]
}

fn strategy(g: Grouping) -> GroupingStrategy {
    match g {
        Grouping::Auto => GroupingStrategy::Auto,
        Grouping::Always => GroupingStrategy::Always,
        Grouping::Min2 => GroupingStrategy::Min2,
        Grouping::Never => GroupingStrategy::Never,
    }
}

impl Cache {
    fn decimal(&mut self, loc: u8, g: Grouping) -> &DecimalFormatter {
        self.dec[loc as usize][g as usize].get_or_insert_with(|| {
            // the baked data has the 5 locales of `LOCALES`; a failure here is a build error
            DecimalFormatter::try_new_unstable(&Baked, icu_locale(loc).into(), strategy(g).into()).expect("baked decimal data")
        })
    }

    /// Format `n` as `f` says. With `parts`, the parts of ECMA-402 `formatToParts` are recorded too.
    pub fn format(&mut self, f: &NumFmt, n: Num, mut out: Out) {
        let code = core::str::from_utf8(&f.currency).unwrap_or("USD");
        let (neg, text) = match n {
            Num::Nan => (false, None),
            Num::Inf(neg) => (neg, None),
            Num::Fin(ref d) => (d.sign() == Sign::Negative, Some(d)),
        };
        // the digits, rounded
        let mut exponent = 0i16;
        let rounded = text.map(|d| {
            let mut d = d.clone();
            if f.style == Style::Percent {
                d.multiply_pow10(2);
            }
            if f.notation != 0 && !d.absolute.is_zero() {
                let e = magnitude(&d);
                exponent = match f.notation {
                    1 => e,
                    2 => e.div_euclid(3) * 3,
                    _ => (e.div_euclid(3) * 3).clamp(0, 12),
                };
                d.multiply_pow10(-exponent);
            }
            f.round(&mut d);
            let step = if f.notation == 1 { 1 } else { 3 };
            if f.notation != 0 && magnitude(&d) >= step && (f.notation != 3 || exponent < 12) {
                d.multiply_pow10(-step);
                exponent += step;
                f.round(&mut d);
            }
            d
        });
        let singular = (f.unit != 255 || (f.style == Style::Currency && f.display == 3))
            && (f.notation != 3 || exponent == 0)
            && rounded.as_ref().is_some_and(|d| *d.absolute.magnitude_range().start() == 0 && magnitude(d) == 0 && d.absolute.digit_at(0) == 1);
        let zero = rounded.as_ref().is_some_and(|d| d.absolute.is_zero());
        let sign = f.sign_char(neg, zero || matches!(n, Num::Nan));
        let paren = f.style == Style::Currency && f.accounting && neg && sign.is_some_and(|s| s.1 == "minusSign");
        // sign, or the open parenthesis of an accounting negative
        if paren {
            out.push("literal", "(");
        } else if let Some((t, ty)) = sign {
            out.push(ty, t);
        }
        if f.style == Style::Currency && f.display != 3 {
            let (sym, space) = currency_text(f.loc, code, f.display);
            out.push("currency", sym);
            // currency spacing applies before a digit only
            if space && matches!(n, Num::Fin(_)) {
                out.push("literal", "\u{a0}");
            }
        }
        match (&n, rounded) {
            (Num::Nan, _) => out.push("nan", "NaN"),
            (Num::Inf(_), _) => out.push("infinity", "\u{221e}"),
            (Num::Fin(_), Some(mut d)) => {
                d.set_sign(Sign::None);
                let fmt = self.decimal(f.loc, f.grouping).format(&d);
                if out.parts.is_some() {
                    let mut sink = Sink { out: &mut out, stack: [""; 3], depth: 0 };
                    let _ = fmt.write_to_parts(&mut sink);
                } else {
                    let _ = fmt.write_to(out.s);
                }
            }
            _ => {}
        }
        if text.is_some() && f.notation != 0 {
            if f.notation == 3 {
                if exponent >= 3 {
                    let i = (exponent / 3 - 1) as usize;
                    if f.compact_long { out.push("literal", " "); }
                    out.push("compact", if f.compact_long { ["thousand", "million", "billion", "trillion"][i] } else { ["K", "M", "B", "T"][i] });
                }
            } else {
                out.push("exponentSeparator", "E");
                if exponent < 0 { out.push("exponentMinusSign", "-"); }
                out.push("exponentInteger", &exponent.abs().to_string());
            }
        }
        if f.style == Style::Percent {
            out.push("percentSign", "%");
        }
        if f.style == Style::Currency && f.display == 3 {
            let names = pack_table!(CURRENCY_NAMES, 3, (), |i| {
                let (code, one, other) = CURRENCY_NAMES[i];
                ([code, one, other], ())
            });
            let name = names.search(code).map_or(code, |i| names.text(i, if singular { 1 } else { 2 }));
            out.push("literal", " ");
            out.push("currency", name);
        }
        if f.unit != 255 {
            let suffixes = pack_table!(UNIT_SUFFIXES, 6, (), |i| {
                let [(a, b), (c, d), (e, f)] = UNIT_SUFFIXES[i];
                ([a, b, c, d, e, f], ())
            });
            let display = f.unit_display as usize;
            let suffix = suffixes.text(f.unit as usize, 2 * display + usize::from(!singular));
            let unit = if let Some(t) = suffix.strip_prefix(' ') { out.push("literal", " "); t } else { suffix };
            let mut label = unit.to_string();
            if f.per_unit != 255 {
                label.push_str(if f.unit_display == 2 { " per " } else { "/" });
                let denominators = pack_table!(UNIT_DENOMINATORS, 3, (), |i| (UNIT_DENOMINATORS[i], ()));
                label.push_str(denominators.text(f.per_unit as usize, display));
            }
            out.push("unit", &label);
        }
        if paren {
            out.push("literal", ")");
        }
    }

    fn plural_rules_for<const ORDINAL: bool>(&mut self) -> &PluralRules {
        self.plurals[ORDINAL as usize].get_or_insert_with(|| {
            let prefs = icu_locale(0).into();
            if ORDINAL {
                PluralRules::try_new_ordinal_unstable(&Baked, prefs)
            } else {
                PluralRules::try_new_cardinal_unstable(&Baked, prefs)
            }.expect("baked plural data")
        })
    }

    pub fn plural_rules(&mut self, ordinal: bool) -> &PluralRules {
        if ordinal { self.plural_rules_for::<true>() } else { self.plural_rules_for::<false>() }
    }

    /// The plural category of `n` (formatted by `f`: the digits decide `1` against `1.0`), as its ECMA-402 name.
    pub fn plural(&mut self, f: &NumFmt, ordinal: bool, n: Num) -> &'static str {
        if ordinal { self.plural_for::<true>(f, n) } else { self.plural_for::<false>(f, n) }
    }

    // RelativeTimeFormat uses cardinal rules only. Keep ordinal data out of
    // binaries that do not expose a PluralRules constructor.
    pub(crate) fn plural_for<const ORDINAL: bool>(&mut self, f: &NumFmt, n: Num) -> &'static str {
        let Num::Fin(mut d) = n else { return "other" };
        d.set_sign(Sign::None);
        f.round(&mut d);
        category_name(self.plural_rules_for::<ORDINAL>().category_for(&d))
    }

    /// The categories of the rules, in the order of V8 (alphabetic, `other` last).
    pub fn plural_categories(&mut self, ordinal: bool) -> Vec<&'static str> {
        let mut v: Vec<&'static str> = self.plural_rules(ordinal).categories().map(category_name).collect();
        v.sort_by_key(|c| (*c == "other", *c));
        v
    }

    /// A collator, made once for each set of options.
    pub fn collator(&mut self, k: CollKey) -> Rc<Collator> {
        if let Some((_, c)) = self.colls.iter().find(|(key, _)| *key == k) {
            return c.clone();
        }
        let mut prefs = CollatorPreferences::default();
        prefs.case_first = Some(match k.case_first {
            1 => CollationCaseFirst::Upper,
            2 => CollationCaseFirst::Lower,
            _ => CollationCaseFirst::False,
        });
        prefs.numeric_ordering = Some(if k.numeric { CollationNumericOrdering::True } else { CollationNumericOrdering::False });
        let mut o = CollatorOptions::default();
        let (strength, case_level) = match k.sensitivity {
            0 => (Strength::Primary, CaseLevel::Off),
            1 => (Strength::Secondary, CaseLevel::Off),
            2 => (Strength::Primary, CaseLevel::On),
            _ => (Strength::Tertiary, CaseLevel::Off),
        };
        (o.strength, o.case_level) = (Some(strength), Some(case_level));
        if k.ignore_punctuation {
            o.alternate_handling = Some(AlternateHandling::Shifted);
        }
        let c = Rc::new(Collator::try_new_unstable(&Baked, prefs, o).expect("baked collation data"));
        // a program that varies the options keeps at most 16 collators
        if self.colls.len() >= 16 {
            self.colls.remove(0);
        }
        self.colls.push((k, c.clone()));
        c
    }
}

fn category_name(c: PluralCategory) -> &'static str {
    match c {
        PluralCategory::Zero => "zero",
        PluralCategory::One => "one",
        PluralCategory::Two => "two",
        PluralCategory::Few => "few",
        PluralCategory::Many => "many",
        PluralCategory::Other => "other",
    }
}

// ------------------------------------------------------------ collation

/// The options of a collator. `sensitivity`: 0 base, 1 accent, 2 case, 3 variant. `case_first`: 0 false, 1 upper, 2 lower.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CollKey {
    pub sensitivity: u8,
    pub case_first: u8,
    pub numeric: bool,
    pub ignore_punctuation: bool,
}

pub const DEFAULT_COLL: CollKey = CollKey { sensitivity: 3, case_first: 0, numeric: false, ignore_punctuation: false };

/// -1, 0 or 1 like `String.prototype.localeCompare`.
pub fn compare(c: &Collator, a: &str, b: &str) -> i32 {
    c.as_borrowed().compare(a, b) as i32
}

// ------------------------------------------------------------ segmentation

/// The boundaries of `u` for `Intl.Segmenter`: `granularity` 0 grapheme, 1 word, 2 sentence. The result starts with 0 and ends
/// with `u.len()`, so segment `i` is `b[i]..b[i + 1]`. For the word granularity the second list tells whether segment `i` is
/// word-like. Rules only: no dictionary or LSTM data, so Chinese, Japanese, Thai, Lao, Khmer and Burmese words are cut by the rules.
pub fn segment_breaks(granularity: u8, u: &[u16]) -> (Vec<u32>, Vec<bool>) {
    let mut b: Vec<u32> = vec![0];
    let mut wl: Vec<bool> = Vec::new();
    match granularity {
        0 => b.extend(icu_segmenter::GraphemeClusterSegmenter::new().segment_utf16(u).map(|i| i as u32)),
        1 => {
            for (i, t) in icu_segmenter::WordSegmenter::new_for_non_complex_scripts(Default::default()).segment_utf16(u).iter_with_word_type() {
                if i != 0 {
                    b.push(i as u32);
                    wl.push(t.is_word_like());
                }
            }
        }
        _ => b.extend(icu_segmenter::SentenceSegmenter::new(Default::default()).segment_utf16(u).map(|i| i as u32)),
    }
    b.dedup();
    if b.last() != Some(&(u.len() as u32)) {
        b.push(u.len() as u32);
    }
    wl.resize(b.len() - 1, false);
    (b, wl)
}

#[cfg(test)]
#[path = "icu_packed_locales_tests.rs"]
mod packed_locales_tests;
