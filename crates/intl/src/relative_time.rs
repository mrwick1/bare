use crate::{icu, intl};
use fixed_decimal::SignDisplay;

#[derive(Clone, Copy)]
pub struct IntlRtf {
    pub loc: u8,
    /// 0 long, 1 short, 2 narrow.
    pub style: u8,
    pub auto: bool,
}


fn rtf_numfmt(loc: u8) -> icu::NumFmt {
    icu::NumFmt {
        loc,
        style: icu::Style::Decimal,
        currency: *b"USD",
        display: icu::SYMBOL,
        accounting: false,
        min_int: 1,
        rounding: icu::Rounding::Fraction,
        fd: (0, 3),
        sd: (1, 21),
        grouping: icu::Grouping::Auto,
        sign: SignDisplay::Auto,
        mode: icu::HALF_EXPAND,
        unit: 255, per_unit: 255, unit_display: 0, notation: 0, compact_long: false, strip_zero: false,
    }
}

/// The text of `format(x, unit)` as parts: (type, text, is a part of the number), the number parts have the unit.
pub fn rtf_parts(cache: &mut icu::Cache, r: IntlRtf, x: f64, unit: usize) -> Vec<(&'static str, String)> {
    let d = &intl::RTF_DATA[intl::RTF_VARIANT[r.loc as usize] as usize][r.style as usize][unit];
    if r.auto {
        // ICU compares the value rounded at two fraction digits
        let k = (x.abs() * 100.0).round();
        let word = if k == 0.0 { 1 } else if k == 100.0 { if x < 0.0 { 0 } else { 2 } } else { 3 };
        if word < 3 && !d.auto[word].is_empty() {
            return vec![("literal", d.auto[word].to_string())];
        }
    }
    let past = x < 0.0 || (x == 0.0 && x.is_sign_negative());
    let f = rtf_numfmt(r.loc);
    let mut nparts = Vec::new();
    let mut s = String::new();
    cache.format(&f, icu::num_from_f64(x.abs()), icu::Out { s: &mut s, parts: Some(&mut nparts) });
    let label = if cache.plural(&f, false, icu::num_from_f64(x.abs())) == "one" { d.one } else { d.other };
    let mut out = Vec::with_capacity(nparts.len() + 2);
    if !past {
        out.push(("literal", "in ".to_string()));
    }
    let mut start = 0;
    for (ty, end) in nparts {
        out.push((ty, s[start..end].to_string()));
        start = end;
    }
    out.push(("literal", if past { format!("{label} ago") } else { label.to_string() }));
    out
}
