// `Intl.NumberFormat`, `Intl.Collator`, `Intl.PluralRules`, `Intl.getCanonicalLocales`, `Number.prototype.toLocaleString` and
// `String.prototype.localeCompare`. Included by `eval_intl.rs`. The formatting and the collation are in `icu.rs` (ICU4X).
//
// Observed need: the staging dashboard calls `toLocaleString` with `en-IN` and `localeCompare`.
//
// Supported (ECMA-402 sections 10, 15 and 16)
// - `Intl.NumberFormat` (with or without `new`; length 0): `supportedLocalesOf`, `format` (a getter that returns one bound
//   function per formatter), `formatToParts`, `resolvedOptions`. Options in the order of V8 (= ECMA-402 2023): `localeMatcher`,
//   `numberingSystem`, `style` (decimal, percent, currency), `currency`, `currencyDisplay` (symbol, narrowSymbol, code),
//   `currencySign`, `unit`, `unitDisplay`, `notation` (standard, scientific, engineering, compact), the digit options (`minimumIntegerDigits`,
//   `minimumFractionDigits`, `maximumFractionDigits`, `minimumSignificantDigits`, `maximumSignificantDigits`,
//   `roundingIncrement` (1), `roundingMode`, `roundingPriority`, `trailingZeroDisplay`), `compactDisplay`, `useGrouping`,
//   `signDisplay`. A string argument keeps its exact decimal digits.
// - `Intl.Collator`: `supportedLocalesOf`, `compare` (a getter, one bound function per collator), `resolvedOptions`; options
//   `usage` (sort), `localeMatcher`, `collation`, `numeric`, `caseFirst`, `sensitivity`, `ignorePunctuation`.
// - `Intl.PluralRules` (needs `new`): `supportedLocalesOf`, `select`, `resolvedOptions`; options `type` and the digit options.
// - `Intl.getCanonicalLocales`, `Number.prototype.toLocaleString`, `String.prototype.localeCompare`.
// - Locales: the 5 of `intl.rs`. A locale with an extension is a gate (see `intl_locales`).
//
// Gates (Unsupported): `numberingSystem` other than
// `latn`; `roundingIncrement` other than 1; Collator `usage: "search"` and any `collation`; the methods `formatRange`,
// `formatRangeToParts` and `selectRange`.
//
// Known deviations: the locale of an unsupported region (`en-AU`) resolves to `en` (V8 keeps `en-AU`); `Intl` is installed on its first read, so `Object.getOwnPropertyDescriptor(globalThis,
// "Intl")` shows an accessor until then.
//
// Storage: one `IntlNf`, `IntlColl` or `IntlPr` per object, in `intl.nfs`, `intl.colls` and `intl.prs`, capped together with the date
// formatters by `max_formatters` and pruned by `collect`. The ICU4X formatters and the collators are made once and shared
// (`intl.icu`).

use fixed_decimal::SignDisplay;

/// A number formatter: its resolved options and the bound `format` function.
#[derive(Clone)]
struct IntlNf {
    f: icu::NumFmt,
    bound: Option<ObjId>,
}

/// A collator. `loc` is the index of the locale (see `intl::LOCALES`).
#[derive(Clone)]
struct IntlColl {
    key: icu::CollKey,
    loc: u8,
    bound: Option<ObjId>,
}

/// Plural rules. `found` is false when no requested locale matched (the default locale is used).
#[derive(Clone)]
struct IntlPr {
    f: icu::NumFmt,
    ordinal: bool,
    found: bool,
}

const SENSITIVITY: [&str; 4] = ["base", "accent", "case", "variant"];
const CASE_FIRST: [&str; 3] = ["upper", "lower", "false"];
const NOTATIONS: [&str; 4] = ["standard", "scientific", "engineering", "compact"];
const CURRENCY_DISPLAYS: [&str; 4] = ["symbol", "narrowSymbol", "code", "name"];
const SIGN_DISPLAYS: [&str; 5] = ["auto", "never", "always", "exceptZero", "negative"];
const ROUNDING_PRIORITIES: [&str; 3] = ["auto", "morePrecision", "lessPrecision"];
const GROUPINGS: [&str; 5] = ["min2", "auto", "always", "true", "false"];
const ROUNDING_INCREMENTS: [u16; 15] = [1, 2, 5, 10, 20, 25, 50, 100, 200, 250, 500, 1000, 2000, 2500, 5000];
const SANCTIONED_UNITS: [&str; 45] = [
    "acre", "bit", "byte", "celsius", "centimeter", "day", "degree", "fahrenheit", "fluid-ounce", "foot", "gallon", "gigabit", "gigabyte",
    "gram", "hectare", "hour", "inch", "kilobit", "kilobyte", "kilogram", "kilometer", "liter", "megabit", "megabyte", "meter",
    "microsecond", "mile", "mile-scandinavian", "milliliter", "millimeter", "millisecond", "minute", "month", "nanosecond", "ounce",
    "percent", "petabyte", "pound", "second", "stone", "terabit", "terabyte", "week", "yard", "year",
];

impl<'p> Interp<'p> {
    /// Number and collator objects the evaluator tracks (until `collect` drops the dead ones).
    pub fn intl_tracked_num(&self) -> usize {
        self.intl.nfs.len() + self.intl.colls.len() + self.intl.prs.len()
    }

    fn intl_formatters(&self) -> usize {
        self.intl.dtfs.len() + self.intl.nfs.len() + self.intl.colls.len() + self.intl.prs.len() + self.intl.segs.len() + self.intl.loc.locales.len() + self.intl.loc.rtfs.len()
    }

    // ------------------------------------------------------------ option helpers

    /// CoerceOptionsToObject: `None` for undefined; null is the TypeError of V8, which names the caller.
    fn intl_options(&mut self, options: Value, who: &str) -> Res<Option<Value>> {
        match options {
            Value::Undefined => Ok(None),
            Value::Null => Err(self.type_err(&format!("{who} called on null or undefined"))),
            v => Ok(Some(v)),
        }
    }

    /// `Get(options, name)`.
    fn intl_get(&mut self, opts: Option<Value>, name: &str) -> Res<Value> {
        let Some(o) = opts else { return Ok(Value::Undefined) };
        let k = self.intl_key(name)?;
        self.get_prop_nat(o, k)
    }

    /// GetOption for a boolean: `None` when absent.
    fn intl_opt_bool(&mut self, opts: Option<Value>, name: &str) -> Res<Option<bool>> {
        let v = self.intl_get(opts, name)?;
        if matches!(v, Value::Undefined) { Ok(None) } else { Ok(Some(self.truthy(v)?)) }
    }

    /// DefaultNumberOption: `None` for undefined (the caller has the fallback), else the integer in `min..=max` or a RangeError.
    fn intl_number(&mut self, v: Value, name: &str, min: f64, max: f64) -> Res<Option<u32>> {
        if matches!(v, Value::Undefined) {
            return Ok(None);
        }
        let n = self.coerce_number(v)?;
        if n.is_nan() || n < min || n > max {
            return Err(self.range_error(&format!("{name} value is out of range.")));
        }
        Ok(Some(n.floor() as u32))
    }

    /// The `numberingSystem`, `calendar` and `collation` types: 3 to 8 letters and digits, joined by `-`.
    fn intl_type_syntax(&mut self, s: &str, name: &str) -> Res<()> {
        if s.split('-').all(|p| (3..=8).contains(&p.len()) && p.bytes().all(|c| c.is_ascii_alphanumeric())) {
            Ok(())
        } else {
            Err(self.range_error(&format!("Invalid {name} : {s}")))
        }
    }

    /// The first requested locale that bare has: its index, or `None`.
    fn intl_pick(&self, requested: &[String]) -> Option<u8> {
        requested.iter().find_map(|t| intl::lookup(t))
    }

    // ------------------------------------------------------------ NumberFormat

    /// SetNumberFormatDigitOptions. Returns a gate when the options ask for something bare cannot do.
    fn intl_digits(&mut self, who: &str, opts: Option<Value>, f: &mut icu::NumFmt, defaults: (u8, u8)) -> Res<Option<&'static str>> {
        let mnid = self.intl_get(opts, "minimumIntegerDigits")?;
        f.min_int = self.intl_number(mnid, "minimumIntegerDigits", 1.0, 21.0)?.unwrap_or(1) as u8;
        let mnfd = self.intl_get(opts, "minimumFractionDigits")?;
        let mxfd = self.intl_get(opts, "maximumFractionDigits")?;
        let mnsd = self.intl_get(opts, "minimumSignificantDigits")?;
        let mxsd = self.intl_get(opts, "maximumSignificantDigits")?;
        let inc = self.intl_get(opts, "roundingIncrement")?;
        let inc = self.intl_number(inc, "roundingIncrement", 1.0, 5000.0)?.unwrap_or(1);
        if !ROUNDING_INCREMENTS.contains(&(inc as u16)) {
            return Err(self.range_error("roundingIncrement value is out of range."));
        }
        f.mode = self.intl_opt_for(who, opts, "roundingMode", &icu::ROUNDING_MODES)?.map_or(icu::HALF_EXPAND, |i| i as u8);
        let priority = self.intl_opt_for(who, opts, "roundingPriority", &ROUNDING_PRIORITIES)?.unwrap_or(0);
        f.strip_zero = self.intl_opt_for(who, opts, "trailingZeroDisplay", &["auto", "stripIfInteger"])?.unwrap_or(0) == 1;
        let has_sd = !matches!(mnsd, Value::Undefined) || !matches!(mxsd, Value::Undefined);
        let has_fd = !matches!(mnfd, Value::Undefined) || !matches!(mxfd, Value::Undefined);
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
            let lo = self.intl_number(mnsd, "minimumSignificantDigits", 1.0, 21.0)?.unwrap_or(1);
            let hi = self.intl_number(mxsd, "maximumSignificantDigits", lo as f64, 21.0)?.unwrap_or(21);
            f.sd = (lo as u8, hi as u8);
        }
        if need_fd && has_fd {
            let lo = self.intl_number(mnfd, "minimumFractionDigits", 0.0, 100.0)?;
            let hi = self.intl_number(mxfd, "maximumFractionDigits", 0.0, 100.0)?;
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

    /// InitializeNumberFormat (`who` is the caller named by the TypeError for a null `options`).
    /// The `-u-` keyword `nu` of the requested locales. `latn` goes in `resolvedOptions().locale` (`pending_ext`); another
    /// numbering system that ICU has sets the gate, as the option does.
    #[cold]
    #[inline(never)]
    fn intl_nf_keywords(&mut self, requested: &[String], nu: Option<&str>, gate: &mut Option<&'static str>) {
        let kw = Self::intl_keywords(requested);
        match Self::intl_keyword_used(&kw, "nu", nu, "latn") {
            Ok(true) => self.set_ext(Some("-u-nu-latn".into())),
            Ok(false) => {}
            Err(_) => *gate = Some("Intl.NumberFormat option numberingSystem"),
        }
    }

    fn intl_nf_create(&mut self, locales: Value, options: Value, who: &str) -> Res<icu::NumFmt> {
        let requested = self.intl_locales(locales)?;
        let opts = self.intl_options(options, who)?;
        self.intl_opt_for(who, opts, "localeMatcher", &["lookup", "best fit"])?;
        let mut gate = None;
        let mut nu = None;
        if let Some(s) = self.intl_opt_str(opts, "numberingSystem")? {
            self.intl_type_syntax(&s, "numberingSystem")?;
            if !s.eq_ignore_ascii_case("latn") {
                gate = Some("Intl.NumberFormat option numberingSystem");
            }
            nu = Some(s);
        }
        let loc = self.intl_pick(&requested).unwrap_or(0);
        if requested.iter().any(|t| intl::has_unicode_ext(t)) {
            self.intl_nf_keywords(&requested, nu.as_deref(), &mut gate);
        }
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
        let style = self.intl_opt_for(who, opts, "style", &["decimal", "percent", "currency", "unit"])?.unwrap_or(0);
        match self.intl_opt_str(opts, "currency")? {
            Some(c) if c.len() == 3 && c.bytes().all(|b| b.is_ascii_alphabetic()) => f.currency.copy_from_slice(c.to_ascii_uppercase().as_bytes()),
            Some(c) => return Err(self.range_error(&format!("Invalid currency code : {c}"))),
            None if style == 2 => return Err(self.type_err("Currency code is required with currency style.")),
            None => {}
        }
        let display = self.intl_opt_for(who, opts, "currencyDisplay", &CURRENCY_DISPLAYS)?.unwrap_or(0);
        f.accounting = self.intl_opt_for(who, opts, "currencySign", &["standard", "accounting"])?.unwrap_or(0) == 1;
        let unit = self.intl_opt_str(opts, "unit")?;
        match &unit {
            Some(u) if !u.split("-per-").all(|p| SANCTIONED_UNITS.contains(&p)) || u.matches("-per-").count() > 1 => {
                return Err(self.range_error(&format!("Invalid unit argument for Intl.NumberFormat() '{u}'")));
            }
            None if style == 3 => return Err(self.type_err("Invalid unit argument for Intl.NumberFormat() ''")),
            _ => {}
        }
        f.unit_display = self.intl_opt_for(who, opts, "unitDisplay", &["short", "narrow", "long"])?.unwrap_or(0) as u8;
        f.style = [icu::Style::Decimal, icu::Style::Percent, icu::Style::Currency, icu::Style::Decimal][style];
        if style == 3 {
            let mut parts = unit.as_deref().unwrap_or("meter").split("-per-");
            let numerator = parts.next();
            f.unit = SANCTIONED_UNITS.iter().position(|u| Some(*u) == numerator).unwrap_or(24) as u8;
            f.per_unit = parts.next().and_then(|p| SANCTIONED_UNITS.iter().position(|u| *u == p)).map_or(255, |i| i as u8);
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
        let notation = self.intl_opt_for(who, opts, "notation", &NOTATIONS)?.unwrap_or(0);
        f.notation = notation as u8;
        gate = gate.or(self.intl_digits(who, opts, &mut f, if notation == 3 { (0, 0) } else { defaults })?);
        f.compact_long = self.intl_opt_for(who, opts, "compactDisplay", &["short", "long"])?.unwrap_or(0) == 1;
        f.grouping = self.intl_grouping(who, opts, notation == 3)?;
        f.sign = match self.intl_opt_for(who, opts, "signDisplay", &SIGN_DISPLAYS)?.unwrap_or(0) {
            1 => SignDisplay::Never,
            2 => SignDisplay::Always,
            3 => SignDisplay::ExceptZero,
            4 => SignDisplay::Negative,
            _ => SignDisplay::Auto,
        };
        match gate {
            Some(g) => Err(unsupported(g, Span::default())),
            None => Ok(f),
        }
    }

    /// GetBooleanOrStringNumberFormatOption for `useGrouping`.
    fn intl_grouping(&mut self, who: &str, opts: Option<Value>, compact: bool) -> Res<icu::Grouping> {
        let default = if compact { icu::Grouping::Min2 } else { icu::Grouping::Auto };
        let v = self.intl_get(opts, "useGrouping")?;
        if matches!(v, Value::Undefined) {
            return Ok(default);
        }
        if matches!(v, Value::Bool(true)) {
            return Ok(icu::Grouping::Always);
        }
        if !self.truthy(v)? {
            return Ok(icu::Grouping::Never);
        }
        let shown = self.intl_show(v);
        let s = self.coerce_string(v)?;
        let s = h!(self, self.heap.to_string_lossy(s));
        match GROUPINGS.iter().position(|g| *g == s) {
            Some(0) => Ok(icu::Grouping::Min2),
            Some(1) => Ok(icu::Grouping::Auto),
            Some(2) => Ok(icu::Grouping::Always),
            Some(_) => Ok(default),
            None => Err(self.range_error(&format!("Value {shown} out of range for {who} options property useGrouping"))),
        }
    }

    /// A value as V8 shows it in a receiver or option error: `#<Object>` for an object.
    fn intl_show(&self, v: Value) -> String {
        match v {
            Value::Object(o) if self.funcs.contains_key(&o) => "function".into(),
            Value::Object(o) if self.heap.is_array(o).unwrap_or(false) => "[object Array]".into(),
            Value::Object(o) => {
                let i = &self.intl;
                let name = if o == self.object_proto {
                    "Object"
                } else if o == i.proto || i.dtfs.contains_key(&o) {
                    "DateTimeFormat"
                } else if o == i.nf_proto || i.nfs.contains_key(&o) {
                    "NumberFormat"
                } else if o == i.coll_proto || i.colls.contains_key(&o) {
                    "Collator"
                } else if o == i.pr_proto || i.prs.contains_key(&o) {
                    "PluralRules"
                } else {
                    "Object"
                };
                format!("#<{name}>")
            }
            _ => self.display(v),
        }
    }

    fn this_intl_nf(&mut self, this: Value, method: &str, unwrap: bool) -> Res<IntlNf> {
        if let Value::Object(o) = this
            && let Some(n) = self.intl.nfs.get(&o)
        {
            return Ok(n.clone());
        }
        // V8 names UnwrapNumberFormat and an undefined receiver for an object: its legacy unwrap step runs first
        if unwrap && matches!(this, Value::Object(_)) {
            return Err(self.type_err("Method UnwrapNumberFormat called on incompatible receiver undefined"));
        }
        let t = self.intl_show(this);
        Err(self.type_err(&format!("Method {method} called on incompatible receiver {t}")))
    }

    /// ToIntlMathematicalValue: ToPrimitive, then a string keeps its exact digits, else ToNumber.
    fn intl_num_arg(&mut self, v: Value) -> Res<icu::Num> {
        if let Value::Number(x) = v {
            return Ok(icu::num_from_f64(x));
        }
        let p = self.coerce_primitive(v, Hint::Number)?;
        if let Value::String(s) = p {
            let t = h!(self, self.heap.to_string_lossy(s));
            let t = t.trim_matches(|c: char| u16::try_from(c as u32).is_ok_and(is_js_space));
            if let Some(n) = icu::num_from_decimal_str(t) {
                return Ok(n);
            }
        }
        if let Value::Symbol(_) = p {
            return Err(self.type_err("Cannot convert a Symbol value to a number"));
        }
        if p.is_bigint() {
            return self.intl_big_num(p);
        }
        Ok(icu::num_from_f64(h!(self, self.heap.to_number(p))))
    }

    /// The text of `x` formatted by `f`, with its parts when `parts` is given.
    fn intl_num_format(&mut self, f: &icu::NumFmt, x: icu::Num, parts: Option<&mut Vec<(&'static str, usize)>>) -> String {
        let mut s = String::new();
        self.intl.icu.format(f, x, icu::Out { s: &mut s, parts });
        s
    }

    // ------------------------------------------------------------ Collator

    /// The `-u-` keywords `kn`, `kf` and `co` of the requested locales. `kn` and `kf` set `numeric` and `caseFirst` when no option
    /// did, and stay in `resolvedOptions().locale` (`pending_ext`), as in V8. A collation that bare lacks (`emoji`, `eor`, which
    /// `en` has) is `Unsupported`, as the option is; the other values of `co` are not used for `en`.
    #[cold]
    #[inline(never)]
    fn intl_coll_keywords(&mut self, requested: &[String], numeric: &mut Option<bool>, case_first: &mut Option<u8>) -> Res<()> {
        let kw = Self::intl_keywords(requested);
        if kw.iter().any(|k| k.0 == "co" && (k.1 == "emoji" || k.1 == "eor")) {
            return Err(unsupported("Intl.Collator option collation", Span::default()));
        }
        let mut ext = String::new();
        for (k, v) in &kw {
            match (k.as_str(), v.as_str()) {
                ("kf", "upper" | "lower" | "false") => {
                    ext.push_str(&format!("-kf-{v}"));
                    case_first.get_or_insert(match v.as_str() { "upper" => 1, "lower" => 2, _ => 0 });
                }
                // V8 reads a `kn` with any type but `false` as true
                ("kn", v) => {
                    ext.push_str(if v == "false" { "-kn-false" } else { "-kn" });
                    numeric.get_or_insert(v != "false");
                }
                _ => {}
            }
        }
        if !ext.is_empty() {
            self.set_ext(Some(format!("-u{ext}").into()));
        }
        Ok(())
    }

    fn intl_coll_create(&mut self, locales: Value, options: Value, who: &'static str) -> Res<IntlColl> {
        let requested = self.intl_locales(locales)?;
        let opts = self.intl_options(options, who)?;
        if self.intl_opt_for(who, opts, "usage", &["sort", "search"])? == Some(1) {
            return Err(unsupported("Intl.Collator option usage search", Span::default()));
        }
        self.intl_opt_for(who, opts, "localeMatcher", &["lookup", "best fit"])?;
        if let Some(c) = self.intl_opt_str(opts, "collation")? {
            self.intl_type_syntax(&c, "collation")?;
            return Err(unsupported("Intl.Collator option collation", Span::default()));
        }
        let mut numeric = self.intl_opt_bool(opts, "numeric")?;
        let mut case_first = self.intl_opt_for(who, opts, "caseFirst", &CASE_FIRST)?.map(|i| [1, 2, 0][i] as u8);
        let loc = self.intl_pick(&requested).unwrap_or(0);
        if requested.iter().any(|t| intl::has_unicode_ext(t)) {
            self.intl_coll_keywords(&requested, &mut numeric, &mut case_first)?;
        }
        let (numeric, case_first) = (numeric.unwrap_or(false), case_first.unwrap_or(0));
        let sensitivity = self.intl_opt_for(who, opts, "sensitivity", &SENSITIVITY)?.unwrap_or(3) as u8;
        let ignore_punctuation = self.intl_opt_bool(opts, "ignorePunctuation")?.unwrap_or(false);
        Ok(IntlColl { key: icu::CollKey { sensitivity, case_first, numeric, ignore_punctuation }, loc, bound: None })
    }

    /// The locale of `resolvedOptions` of a collator: `en-US` or `en`, and the kept extension.
    fn intl_coll_locale(&mut self, this: Value, loc: u8) -> Res<Value> {
        let base = if loc == 0 { "en-US" } else { "en" };
        if let Value::Object(o) = this
            && let Some(e) = self.intl.exts.get(&o)
        {
            let s = format!("{base}{e}");
            return self.str_value(&s);
        }
        self.str_value(base)
    }

    fn this_intl_coll(&mut self, this: Value, method: &str) -> Res<IntlColl> {
        if let Value::Object(o) = this
            && let Some(c) = self.intl.colls.get(&o)
        {
            return Ok(c.clone());
        }
        let t = self.intl_show(this);
        Err(self.type_err(&format!("Method {method} called on incompatible receiver {t}")))
    }

    // ------------------------------------------------------------ PluralRules

    fn this_intl_pr(&mut self, this: Value, method: &str) -> Res<IntlPr> {
        if let Value::Object(o) = this
            && let Some(p) = self.intl.prs.get(&o)
        {
            return Ok(p.clone());
        }
        let t = self.intl_show(this);
        Err(self.type_err(&format!("Method {method} called on incompatible receiver {t}")))
    }

    // ------------------------------------------------------------ objects

    /// A bound function of `target` for `this`, with `length` and an empty name (what `format` and `compare` return).
    fn intl_bound(&mut self, target: ObjId, this: Value, len: u32) -> Res<ObjId> {
        let b = self.new_func_object(Callee::Bound { target, this, args: Vec::new() })?;
        self.def(b, self.k_length, Value::Number(len as f64), false, false, true)?;
        let empty = h!(self, self.heap.intern_str(""));
        self.def(b, self.k_name, Value::String(empty), false, false, true)?;
        Ok(b)
    }

    /// Define the own data properties of `o` in order, each writable, enumerable and configurable.
    fn intl_fill(&mut self, o: ObjId, props: Vec<(&str, Value)>) -> Res<()> {
        for (n, v) in props {
            let k = h!(self, self.heap.key_from_str(n));
            self.def(o, k, v, true, true, true)?;
        }
        Ok(())
    }

    /// The digit and sign properties of `resolvedOptions`, in the order of V8.
    fn intl_resolved_digits(&mut self, f: &icu::NumFmt, props: &mut Vec<(&'static str, Value)>) {
        props.push(("minimumIntegerDigits", Value::Number(f.min_int as f64)));
        if f.rounding != icu::Rounding::Significant {
            props.push(("minimumFractionDigits", Value::Number(f.fd.0 as f64)));
            props.push(("maximumFractionDigits", Value::Number(f.fd.1 as f64)));
        }
        if f.rounding != icu::Rounding::Fraction {
            props.push(("minimumSignificantDigits", Value::Number(f.sd.0 as f64)));
            props.push(("maximumSignificantDigits", Value::Number(f.sd.1 as f64)));
        }
    }

    fn intl_resolved_rounding(&mut self, f: &icu::NumFmt, props: &mut Vec<(&'static str, Value)>) -> Res<()> {
        let priority = match f.rounding {
            icu::Rounding::MorePrecision => "morePrecision",
            icu::Rounding::LessPrecision => "lessPrecision",
            _ => "auto",
        };
        props.push(("roundingIncrement", Value::Number(1.0)));
        props.push(("roundingMode", self.str_value(icu::ROUNDING_MODES[f.mode as usize])?));
        props.push(("roundingPriority", self.str_value(priority)?));
        props.push(("trailingZeroDisplay", self.str_value(if f.strip_zero { "stripIfInteger" } else { "auto" })?));
        Ok(())
    }

    // ------------------------------------------------------------ natives

    fn call_intl_num(&mut self, f: IntlFn, this: Value, args: &[Value], is_new: bool) -> Res<Value> {
        let (a0, a1) = (arg(args, 0), arg(args, 1));
        match f {
            IntlFn::NfCtor | IntlFn::CollCtor | IntlFn::PrCtor => {
                let nt = self.cls.native_nt.take();
                if f == IntlFn::PrCtor && !is_new {
                    return Err(self.type_err("Constructor Intl.PluralRules requires 'new'"));
                }
                if self.intl_formatters() >= self.sc.limits.max_formatters {
                    return Err(Error::Limit(LimitKind::Sc(ScLimit::Formatters)));
                }
                match f {
                    IntlFn::NfCtor => {
                        let proto = self.nt_proto(nt, self.intl.nf_proto)?;
                        self.intl.in_ctor = true;
                        let nf = self.intl_nf_create(a0, a1, "Intl.NumberFormat").inspect_err(|_| self.intl_ctor_failed())?;
                        let o = h!(self, self.heap.new_object(Some(proto)));
                        self.intl.nfs.insert(o, IntlNf { f: nf, bound: None });
                        self.intl_keep_ext(o);
                        Ok(Value::Object(o))
                    }
                    IntlFn::CollCtor => {
                        let proto = self.nt_proto(nt, self.intl.coll_proto)?;
                        self.intl.in_ctor = true;
                        let c = self.intl_coll_create(a0, a1, "Intl.Collator").inspect_err(|_| self.intl_ctor_failed())?;
                        let o = h!(self, self.heap.new_object(Some(proto)));
                        self.intl.colls.insert(o, c);
                        self.intl_keep_ext(o);
                        Ok(Value::Object(o))
                    }
                    _ => {
                        let proto = self.nt_proto(nt, self.intl.pr_proto)?;
                        let p = self.intl_pr_create(a0, a1)?;
                        let o = h!(self, self.heap.new_object(Some(proto)));
                        self.intl.prs.insert(o, p);
                        Ok(Value::Object(o))
                    }
                }
            }
            IntlFn::Supported(kind) => {
                let who = ["Intl.NumberFormat", "Intl.Collator", "Intl.PluralRules", "Intl.Segmenter", "Intl.RelativeTimeFormat"][kind as usize];
                let tags = self.intl_locales(a0)?;
                if !matches!(a1, Value::Undefined) {
                    let o = self.intl_options(a1, who)?;
                    self.intl_opt_for(who, o, "localeMatcher", &["lookup", "best fit"])?;
                }
                let mut out = Vec::new();
                for t in tags.iter().filter(|t| intl::lookup(t).is_some()) {
                    out.push(self.str_value(t)?);
                }
                self.new_array_from(&out)
            }
            IntlFn::NfFormatGet => {
                let n = self.this_intl_nf(this, "get Intl.NumberFormat.prototype.format", true)?;
                if let Some(b) = n.bound {
                    return Ok(Value::Object(b));
                }
                let b = self.intl_bound(self.intl.nf_format, this, 1)?;
                if let Value::Object(o) = this
                    && let Some(n) = self.intl.nfs.get_mut(&o)
                {
                    n.bound = Some(b);
                }
                Ok(Value::Object(b))
            }
            IntlFn::NfFormat => {
                let n = self.this_intl_nf(this, "Intl.NumberFormat.prototype.format", false)?;
                let x = self.intl_num_arg(a0)?;
                let s = self.intl_num_format(&n.f, x, None);
                self.str_value(&s)
            }
            IntlFn::NfToParts => {
                let n = self.this_intl_nf(this, "Intl.NumberFormat.prototype.formatToParts", false)?;
                let x = self.intl_num_arg(a0)?;
                let mut parts = Vec::new();
                let s = self.intl_num_format(&n.f, x, Some(&mut parts));
                let (kt, kv) = (h!(self, self.heap.key_from_str("type")), h!(self, self.heap.key_from_str("value")));
                let mut items = Vec::with_capacity(parts.len());
                let mut start = 0;
                for (ty, end) in parts {
                    let o = h!(self, self.heap.new_object(Some(self.object_proto)));
                    let t = self.str_value(ty)?;
                    let v = self.str_value(&s[start..end])?;
                    self.def(o, kt, t, true, true, true)?;
                    self.def(o, kv, v, true, true, true)?;
                    items.push(Value::Object(o));
                    start = end;
                }
                self.new_array_from(&items)
            }
            IntlFn::NfResolved => {
                let n = self.this_intl_nf(this, "Intl.NumberFormat.prototype.resolvedOptions", true)?;
                let f = n.f;
                let mut props: Vec<(&str, Value)> = vec![("locale", self.intl_locale_value(this, f.loc)?), ("numberingSystem", self.str_value("latn")?)];
                props.push(("style", self.str_value(if f.unit != 255 { "unit" } else { ["decimal", "percent", "currency"][f.style as usize] })?));
                if f.style == icu::Style::Currency {
                    props.push(("currency", self.str_value(core::str::from_utf8(&f.currency).unwrap_or("USD"))?));
                    props.push(("currencyDisplay", self.str_value(CURRENCY_DISPLAYS[f.display as usize])?));
                    props.push(("currencySign", self.str_value(if f.accounting { "accounting" } else { "standard" })?));
                }
                if f.unit != 255 {
                    let mut unit = icu::UNIT_NAMES[f.unit as usize].to_string();
                    if f.per_unit != 255 { unit.push_str("-per-"); unit.push_str(icu::UNIT_NAMES[f.per_unit as usize]); }
                    props.push(("unit", self.str_value(&unit)?));
                    props.push(("unitDisplay", self.str_value(["short", "narrow", "long"][f.unit_display as usize])?));
                }
                self.intl_resolved_digits(&f, &mut props);
                props.push((
                    "useGrouping",
                    match f.grouping {
                        icu::Grouping::Never => Value::Bool(false),
                        g => self.str_value(["auto", "always", "min2"][g as usize])?,
                    },
                ));
                props.push(("notation", self.str_value(NOTATIONS[f.notation as usize])?));
                if f.notation == 3 { props.push(("compactDisplay", self.str_value(if f.compact_long { "long" } else { "short" })?)); }
                props.push(("signDisplay", self.str_value(SIGN_DISPLAYS[f.sign as usize])?));
                self.intl_resolved_rounding(&f, &mut props)?;
                let o = h!(self, self.heap.new_object(Some(self.object_proto)));
                self.intl_fill(o, props)?;
                Ok(Value::Object(o))
            }
            IntlFn::NfRange(kind) => Err(unsupported(["Intl.NumberFormat.prototype.formatRange", "Intl.NumberFormat.prototype.formatRangeToParts", "Intl.PluralRules.prototype.selectRange"][kind as usize], Span::default())),
            IntlFn::CollCompareGet => {
                let c = self.this_intl_coll(this, "get Intl.Collator.prototype.compare")?;
                if let Some(b) = c.bound {
                    return Ok(Value::Object(b));
                }
                let b = self.intl_bound(self.intl.coll_compare, this, 2)?;
                if let Value::Object(o) = this
                    && let Some(c) = self.intl.colls.get_mut(&o)
                {
                    c.bound = Some(b);
                }
                Ok(Value::Object(b))
            }
            IntlFn::CollCompare => {
                let c = self.this_intl_coll(this, "Intl.Collator.prototype.compare")?;
                let (x, y) = (self.coerce_string(a0)?, self.coerce_string(a1)?);
                self.intl_compare(c.key, x, y)
            }
            IntlFn::CollResolved => {
                let c = self.this_intl_coll(this, "Intl.Collator.prototype.resolvedOptions")?;
                let props: Vec<(&str, Value)> = vec![
                    ("locale", self.intl_coll_locale(this, c.loc)?),
                    ("usage", self.str_value("sort")?),
                    ("sensitivity", self.str_value(SENSITIVITY[c.key.sensitivity as usize])?),
                    ("ignorePunctuation", Value::Bool(c.key.ignore_punctuation)),
                    ("collation", self.str_value("default")?),
                    ("numeric", Value::Bool(c.key.numeric)),
                    ("caseFirst", self.str_value(CASE_FIRST[[2, 0, 1][c.key.case_first as usize]])?),
                ];
                let o = h!(self, self.heap.new_object(Some(self.object_proto)));
                self.intl_fill(o, props)?;
                Ok(Value::Object(o))
            }
            IntlFn::PrSelect => {
                let p = self.this_intl_pr(this, "Intl.PluralRules.prototype.select")?;
                let x = self.coerce_number(a0)?;
                let c = self.intl.icu.plural(&p.f, p.ordinal, icu::num_from_f64(x));
                self.str_value(c)
            }
            IntlFn::PrResolved => {
                let p = self.this_intl_pr(this, "Intl.PluralRules.prototype.resolvedOptions")?;
                // V8 omits the fraction digits whenever significant digits count
                let mut f = p.f;
                let rounding = f.rounding;
                if rounding != icu::Rounding::Fraction {
                    f.rounding = icu::Rounding::Significant;
                }
                let mut props: Vec<(&str, Value)> = vec![
                    ("locale", self.str_value(if p.found { "en" } else { "en-US" })?),
                    ("type", self.str_value(if p.ordinal { "ordinal" } else { "cardinal" })?),
                ];
                self.intl_resolved_digits(&f, &mut props);
                f.rounding = rounding;
                let mut cats = Vec::new();
                for c in self.intl.icu.plural_categories(p.ordinal) {
                    cats.push(self.str_value(c)?);
                }
                props.push(("pluralCategories", self.new_array_from(&cats)?));
                self.intl_resolved_rounding(&f, &mut props)?;
                let o = h!(self, self.heap.new_object(Some(self.object_proto)));
                self.intl_fill(o, props)?;
                Ok(Value::Object(o))
            }
            IntlFn::GetCanonical => {
                let tags = self.intl_locales(a0)?;
                let mut out = Vec::with_capacity(tags.len());
                for t in &tags {
                    out.push(self.str_value(t)?);
                }
                self.new_array_from(&out)
            }
            IntlFn::NumLocale => {
                let Some(x) = self.this_number_value(this) else { return Err(self.type_err("Number.prototype.toLocaleString requires that 'this' be a Number")) };
                let f = self.intl_nf_create(a0, a1, "Number.prototype.toLocaleString")?;
                let s = self.intl_num_format(&f, icu::num_from_f64(x), None);
                self.str_value(&s)
            }
            IntlFn::StrLocaleCompare => {
                let x = self.this_str(this, "localeCompare")?;
                let y = self.coerce_string(a0)?;
                let key = if matches!(a1, Value::Undefined) && matches!(arg(args, 2), Value::Undefined) {
                    icu::DEFAULT_COLL
                } else {
                    self.intl_coll_create(a1, arg(args, 2), "String.prototype.localeCompare")?.key
                };
                self.intl_compare(key, x, y)
            }
            IntlFn::SegCtor | IntlFn::SegResolved | IntlFn::SegSegment | IntlFn::SegsContaining | IntlFn::SegsIter | IntlFn::SegIterNext => self.call_intl_seg(f, this, args, is_new),
            IntlFn::LocCtor | IntlFn::LocToString | IntlFn::LocMaximize(_) | IntlFn::LocGet(_) | IntlFn::RtfCtor | IntlFn::RtfFormat(_) | IntlFn::RtfResolved | IntlFn::LazyLoc(_) => self.call_intl_locale(f, this, args, is_new),
            _ => unreachable!("handled in call_intl"),
        }
    }

    /// Compare two strings with the collator of `key`: -1, 0 or 1.
    fn intl_compare(&mut self, key: icu::CollKey, x: StrId, y: StrId) -> Res<Value> {
        let (xs, ys) = (h!(self, self.heap.to_string_lossy(x)), h!(self, self.heap.to_string_lossy(y)));
        self.spend_units(xs.len() + ys.len())?;
        let c = self.intl.icu.collator(key);
        Ok(Value::Number(icu::compare(&c, &xs, &ys) as f64))
    }

    fn intl_pr_create(&mut self, locales: Value, options: Value) -> Res<IntlPr> {
        const PR: &str = "Intl.PluralRules";
        let requested = self.intl_locales(locales)?;
        let opts = self.intl_options(options, PR)?;
        self.intl_opt_for(PR, opts, "localeMatcher", &["lookup", "best fit"])?;
        let ordinal = self.intl_opt_for(PR, opts, "type", &["cardinal", "ordinal"])? == Some(1);
        let mut f = icu::NumFmt {
            loc: 0,
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
        if let Some(g) = self.intl_digits(PR, opts, &mut f, (0, 3))? {
            return Err(unsupported(g, Span::default()));
        }
        let found = self.intl_pick(&requested).is_some();
        Ok(IntlPr { f, ordinal, found })
    }
}
