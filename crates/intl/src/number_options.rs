//! NumberFormat option validation and defaults.

use crate::icu;
use fixed_decimal::SignDisplay;

pub const NOTATIONS: [&str; 4] = ["standard", "scientific", "engineering", "compact"];
pub const CURRENCY_DISPLAYS: [&str; 4] = ["symbol", "narrowSymbol", "code", "name"];
pub const SIGN_DISPLAYS: [&str; 5] = ["auto", "never", "always", "exceptZero", "negative"];
const ROUNDING_PRIORITIES: [&str; 3] = ["auto", "morePrecision", "lessPrecision"];
const GROUPINGS: [&str; 5] = ["min2", "auto", "always", "true", "false"];
const ROUNDING_INCREMENTS: [u16; 15] = [1, 2, 5, 10, 20, 25, 50, 100, 200, 250, 500, 1000, 2000, 2500, 5000];

/// Operations the engine performs while NumberFormat reads and coerces options.
/// Keep the order here because property access and conversion can run script.
pub trait Options {
    type Value: Copy;
    type Error;

    fn get(&mut self, name: &str) -> Result<Self::Value, Self::Error>;
    fn is_undefined(v: Self::Value) -> bool;
    fn is_true(v: Self::Value) -> bool;
    fn truthy(&mut self, v: Self::Value) -> Result<bool, Self::Error>;
    fn string(&mut self, v: Self::Value) -> Result<String, Self::Error>;
    fn show(&self, v: Self::Value) -> String;
    fn number_option(&mut self, v: Self::Value, name: &str, min: f64, max: f64) -> Result<Option<u32>, Self::Error>;
    fn range_error(&mut self, message: &str) -> Self::Error;
    fn type_error(&mut self, message: &str) -> Self::Error;
    fn unsupported(&mut self, feature: &'static str) -> Self::Error;

    fn string_option(&mut self, name: &str) -> Result<Option<String>, Self::Error> {
        let v = self.get(name)?;
        if Self::is_undefined(v) { Ok(None) } else { self.string(v).map(Some) }
    }

    fn choice(&mut self, who: &str, name: &str, choices: &[&str]) -> Result<Option<usize>, Self::Error> {
        let Some(s) = self.string_option(name)? else { return Ok(None) };
        match choices.iter().position(|v| *v == s) {
            Some(i) => Ok(Some(i)),
            None => Err(self.range_error(&format!("Value {s} out of range for {who} options property {name}"))),
        }
    }

    /// SetNumberFormatDigitOptions. Returns a gate when the options ask for something bare cannot do.
    fn digits(&mut self, who: &str, f: &mut icu::NumFmt, defaults: (u8, u8)) -> Result<Option<&'static str>, Self::Error> {
        let mnid = self.get("minimumIntegerDigits")?;
        f.min_int = self.number_option(mnid, "minimumIntegerDigits", 1.0, 21.0)?.unwrap_or(1) as u8;
        let mnfd = self.get("minimumFractionDigits")?;
        let mxfd = self.get("maximumFractionDigits")?;
        let mnsd = self.get("minimumSignificantDigits")?;
        let mxsd = self.get("maximumSignificantDigits")?;
        let inc = self.get("roundingIncrement")?;
        let inc = self.number_option(inc, "roundingIncrement", 1.0, 5000.0)?.unwrap_or(1);
        if !ROUNDING_INCREMENTS.contains(&(inc as u16)) {
            return Err(self.range_error("roundingIncrement value is out of range."));
        }
        f.mode = self.choice(who, "roundingMode", &icu::ROUNDING_MODES)?.map_or(icu::HALF_EXPAND, |i| i as u8);
        let priority = self.choice(who, "roundingPriority", &ROUNDING_PRIORITIES)?.unwrap_or(0);
        f.strip_zero = self.choice(who, "trailingZeroDisplay", &["auto", "stripIfInteger"])?.unwrap_or(0) == 1;
        let has_sd = !Self::is_undefined(mnsd) || !Self::is_undefined(mxsd);
        let has_fd = !Self::is_undefined(mnfd) || !Self::is_undefined(mxfd);
        let (mut need_sd, mut need_fd) = (true, true);
        if priority == 0 {
            need_sd = has_sd;
            if need_sd {
                need_fd = false;
            }
        }
        f.sd = (1, 21);
        f.fd = defaults;
        if need_sd && has_sd {
            let lo = self.number_option(mnsd, "minimumSignificantDigits", 1.0, 21.0)?.unwrap_or(1);
            let hi = self.number_option(mxsd, "maximumSignificantDigits", lo as f64, 21.0)?.unwrap_or(21);
            f.sd = (lo as u8, hi as u8);
        }
        if need_fd && has_fd {
            let lo = self.number_option(mnfd, "minimumFractionDigits", 0.0, 100.0)?;
            let hi = self.number_option(mxfd, "maximumFractionDigits", 0.0, 100.0)?;
            f.fd = match (lo, hi) {
                (None, Some(h)) => (defaults.0.min(h as u8), h as u8),
                (Some(l), None) => (l as u8, defaults.1.max(l as u8)),
                (Some(l), Some(h)) if l > h => return Err(self.range_error("maximumFractionDigits value is out of range.")),
                (Some(l), Some(h)) => (l as u8, h as u8),
                (None, None) => defaults,
            };
        }
        f.rounding = match priority {
            1 => icu::Rounding::MorePrecision,
            2 => icu::Rounding::LessPrecision,
            _ if has_sd => icu::Rounding::Significant,
            _ => icu::Rounding::Fraction,
        };
        if f.notation == 3 && priority == 0 && !has_fd && !has_sd {
            f.rounding = icu::Rounding::MorePrecision;
            f.fd = (0, 0);
            f.sd = (1, 2);
        }
        Ok((inc != 1).then_some("Intl.NumberFormat option roundingIncrement"))
    }

    /// GetBooleanOrStringNumberFormatOption for `useGrouping`.
    fn grouping(&mut self, who: &str, compact: bool) -> Result<icu::Grouping, Self::Error> {
        let default = if compact { icu::Grouping::Min2 } else { icu::Grouping::Auto };
        let v = self.get("useGrouping")?;
        if Self::is_undefined(v) {
            return Ok(default);
        }
        if Self::is_true(v) {
            return Ok(icu::Grouping::Always);
        }
        if !self.truthy(v)? {
            return Ok(icu::Grouping::Never);
        }
        let shown = self.show(v);
        let s = self.string(v)?;
        match GROUPINGS.iter().position(|g| *g == s) {
            Some(0) => Ok(icu::Grouping::Min2),
            Some(1) => Ok(icu::Grouping::Auto),
            Some(2) => Ok(icu::Grouping::Always),
            Some(_) => Ok(default),
            None => Err(self.range_error(&format!("Value {shown} out of range for {who} options property useGrouping"))),
        }
    }
}

/// Resolve number options after the engine has selected the locale.
pub fn initialize<A: Options>(
    options: &mut A,
    who: &str,
    loc: u8,
    mut gate: Option<&'static str>,
) -> Result<icu::NumFmt, A::Error> {
    let mut f = icu::NumFmt {
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
    };
    // SetNumberFormatUnitOptions
    let style = options.choice(who, "style", &["decimal", "percent", "currency", "unit"])?.unwrap_or(0);
    match options.string_option("currency")? {
        Some(c) if c.len() == 3 && c.bytes().all(|b| b.is_ascii_alphabetic()) => f.currency.copy_from_slice(c.to_ascii_uppercase().as_bytes()),
        Some(c) => return Err(options.range_error(&format!("Invalid currency code : {c}"))),
        None if style == 2 => return Err(options.type_error("Currency code is required with currency style.")),
        None => {}
    }
    let display = options.choice(who, "currencyDisplay", &CURRENCY_DISPLAYS)?.unwrap_or(0);
    f.accounting = options.choice(who, "currencySign", &["standard", "accounting"])?.unwrap_or(0) == 1;
    let unit = options.string_option("unit")?;
    match &unit {
        Some(u) if !u.split("-per-").all(|p| icu::UNIT_NAMES.contains(&p)) || u.matches("-per-").count() > 1 => {
            return Err(options.range_error(&format!("Invalid unit argument for Intl.NumberFormat() '{u}'")));
        }
        None if style == 3 => return Err(options.type_error("Invalid unit argument for Intl.NumberFormat() ''")),
        _ => {}
    }
    f.unit_display = options.choice(who, "unitDisplay", &["short", "narrow", "long"])?.unwrap_or(0) as u8;
    f.style = [icu::Style::Decimal, icu::Style::Percent, icu::Style::Currency, icu::Style::Decimal][style];
    if style == 3 {
        let mut parts = unit.as_deref().expect("required unit").split("-per-");
        let numerator = parts.next();
        f.unit = icu::UNIT_NAMES.iter().position(|u| Some(*u) == numerator).expect("validated unit") as u8;
        f.per_unit = parts.next().and_then(|p| icu::UNIT_NAMES.iter().position(|u| *u == p)).map_or(255, |i| i as u8);
    }
    if style == 2 {
        f.display = display as u8;
    }
    let cd = icu::currency_digits(core::str::from_utf8(&f.currency).unwrap_or(""));
    let defaults = match style {
        2 => (cd, cd),
        1 => (0, 0),
        _ => (0, 3),
    };
    let notation = options.choice(who, "notation", &NOTATIONS)?.unwrap_or(0);
    f.notation = notation as u8;
    gate = gate.or(options.digits(who, &mut f, if notation == 3 { (0, 0) } else { defaults })?);
    f.compact_long = options.choice(who, "compactDisplay", &["short", "long"])?.unwrap_or(0) == 1;
    f.grouping = options.grouping(who, notation == 3)?;
    f.sign = match options.choice(who, "signDisplay", &SIGN_DISPLAYS)?.unwrap_or(0) {
        1 => SignDisplay::Never,
        2 => SignDisplay::Always,
        3 => SignDisplay::ExceptZero,
        4 => SignDisplay::Negative,
        _ => SignDisplay::Auto,
    };
    match gate {
        Some(g) => Err(options.unsupported(g)),
        None => Ok(f),
    }
}
