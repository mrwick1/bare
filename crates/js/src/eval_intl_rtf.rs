// `Intl.RelativeTimeFormat` (ECMA-402 section 17). Included by `eval_intl.rs`. The patterns are in `intl_rtf_data.rs` (made by
// `tools/js/intl_rtf_gen.mjs` from Node's ICU), the number part is `NumberFormat` and the plural is `PluralRules` of ICU4X.
//
// Observed need: theverge.com calls `new Intl.RelativeTimeFormat(...)` to show how old a post is.
//
// Supported: `new Intl.RelativeTimeFormat(locales, options)` (needs `new`), `supportedLocalesOf`, `format(value, unit)`,
// `formatToParts`, `resolvedOptions`; `style` long, short and narrow; `numeric` always and auto; the units second to year with
// their plurals (`quarter` too). The locales are the 5 of `intl.rs`: they make three groups of patterns (en and en-US, en-CA,
// en-GB and en-IN).
//
// Rules from ICU (V8): `numeric: "auto"` uses a word ("yesterday", "this year", "now") when the value rounds to -1, 0 or 1 at two
// fraction digits (`1.004` is "tomorrow", `1.005` is too, `1.0051` is "in 1.005 days"). The number part has at most 3 fraction
// digits. A negative value and `-0` are in the past ("0 days ago").
//
// `Intl.RelativeTimeFormat` is an accessor on `Intl` until it is first read (see `intl_loc_lazy` in `eval_intl_locale.rs`).
//
// Gates (Unsupported): a `numberingSystem` other than `latn`. Known deviation: a locale that ICU has and bare has not (`de`)
// resolves to its English parent or to `en-US`, so its text is English.
//
// Storage: one `IntlRtf` in `intl.loc.rtfs`, capped with the other formatters by `max_formatters` and pruned by `collect`.

use bare_intl::relative_time::{IntlRtf, rtf_parts};

const RTF_UNITS: [&str; 8] = ["second", "minute", "hour", "day", "week", "month", "quarter", "year"];
const RTF_STYLES: [&str; 3] = ["long", "short", "narrow"];

impl<'p> Interp<'p> {
    fn this_rtf(&mut self, this: Value, method: &str) -> Res<IntlRtf> {
        if let Value::Object(o) = this
            && let Some(r) = self.intl.loc.rtfs.get(&o)
        {
            return Ok(*r);
        }
        let t = self.intl_show(this);
        Err(self.type_err(&format!("Method Intl.RelativeTimeFormat.prototype.{method} called on incompatible receiver {t}")))
    }

    fn intl_rtf_create(&mut self, locales: Value, options: Value) -> Res<IntlRtf> {
        const RTF: &str = "Intl.RelativeTimeFormat";
        let requested = self.intl_locales(locales)?;
        let opts = self.intl_options(options, RTF)?;
        self.intl_opt_for(RTF, opts, "localeMatcher", &["lookup", "best fit"])?;
        let mut gate = None;
        let mut nu = None;
        if let Some(s) = self.intl_opt_str(opts, "numberingSystem")? {
            self.intl_type_syntax(&s, "numberingSystem")?;
            if !s.eq_ignore_ascii_case("latn") {
                gate = Some("Intl.RelativeTimeFormat option numberingSystem");
            }
            nu = Some(s);
        }
        let loc = self.intl_pick(&requested).unwrap_or(0);
        if requested.iter().any(|t| intl::has_unicode_ext(t)) {
            let kw = Self::intl_keywords(&requested);
            match Self::intl_keyword_used(&kw, "nu", nu.as_deref(), "latn") {
                Ok(true) => self.set_ext(Some("-u-nu-latn".into())),
                Ok(false) => {}
                Err(_) => gate = Some("Intl.RelativeTimeFormat option numberingSystem"),
            }
        }
        let style = self.intl_opt_for(RTF, opts, "style", &RTF_STYLES)?.unwrap_or(0) as u8;
        let auto = self.intl_opt_for(RTF, opts, "numeric", &["always", "auto"])? == Some(1);
        if let Some(g) = gate {
            return Err(unsupported(g, Span::default()));
        }
        Ok(IntlRtf { loc, style, auto })
    }

    /// The number formatter of the patterns: decimal, up to 3 fraction digits, grouping.
    fn call_intl_rtf(&mut self, f: IntlFn, this: Value, args: &[Value], is_new: bool) -> Res<Value> {
        let (a0, a1) = (arg(args, 0), arg(args, 1));
        match f {
            IntlFn::RtfCtor => {
                let nt = self.cls.native_nt.take();
                if !is_new {
                    return Err(self.type_err("Constructor Intl.RelativeTimeFormat requires 'new'"));
                }
                if self.intl_formatters() >= self.sc.limits.max_formatters {
                    return Err(Error::Limit(LimitKind::Sc(ScLimit::Formatters)));
                }
                let proto = self.nt_proto(nt, self.intl.loc.rtf_proto)?;
                self.intl.in_ctor = true;
                let r = self.intl_rtf_create(a0, a1).inspect_err(|_| self.intl_ctor_failed())?;
                let o = h!(self, self.heap.new_object(Some(proto)));
                self.intl.loc.rtfs.insert(o, r);
                self.intl_keep_ext(o);
                Ok(Value::Object(o))
            }
            IntlFn::RtfFormat(to_parts) => {
                let method = if to_parts { "formatToParts" } else { "format" };
                let r = self.this_rtf(this, method)?;
                let x = self.coerce_number(a0)?;
                let u = self.coerce_string(a1)?;
                let u = h!(self, self.heap.to_string_lossy(u));
                if !x.is_finite() {
                    return Err(self.range_error(&format!("Value need to be finite number for Intl.RelativeTimeFormat.prototype.{method}()")));
                }
                let Some(unit) = RTF_UNITS.iter().position(|n| u == *n || u.strip_suffix('s') == Some(*n)) else {
                    return Err(self.range_error(&format!("Invalid unit argument for Intl.RelativeTimeFormat.prototype.{method}() '{u}'")));
                };
                let parts = rtf_parts(&mut self.intl.icu, r, x, unit);
                if !to_parts {
                    let s: String = parts.iter().map(|p| p.1.as_str()).collect();
                    return self.str_value(&s);
                }
                let (kt, kv, ku) = (h!(self, self.heap.key_from_str("type")), h!(self, self.heap.key_from_str("value")), h!(self, self.heap.key_from_str("unit")));
                let mut items = Vec::with_capacity(parts.len());
                for (ty, text) in parts {
                    let o = h!(self, self.heap.new_object(Some(self.object_proto)));
                    let (t, v) = (self.str_value(ty)?, self.str_value(&text)?);
                    self.def(o, kt, t, true, true, true)?;
                    self.def(o, kv, v, true, true, true)?;
                    if ty != "literal" {
                        let un = self.str_value(RTF_UNITS[unit])?;
                        self.def(o, ku, un, true, true, true)?;
                    }
                    items.push(Value::Object(o));
                }
                self.new_array_from(&items)
            }
            IntlFn::RtfResolved => {
                let r = self.this_rtf(this, "resolvedOptions")?;
                let props: Vec<(&str, Value)> = vec![
                    ("locale", self.intl_locale_value(this, r.loc)?),
                    ("style", self.str_value(RTF_STYLES[r.style as usize])?),
                    ("numeric", self.str_value(if r.auto { "auto" } else { "always" })?),
                    ("numberingSystem", self.str_value("latn")?),
                ];
                let o = h!(self, self.heap.new_object(Some(self.object_proto)));
                self.intl_fill(o, props)?;
                Ok(Value::Object(o))
            }
            _ => unreachable!("handled in call_intl_num"),
        }
    }
}
