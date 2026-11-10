// `Intl.Locale` (ECMA-402 section 14). Included by `eval_intl.rs`. The tag logic is in `intl_locale.rs`; the canonical form is
// `intl::canonical_tag`.
//
// Observed need: wise.com calls `new Intl.Locale(tag.replace("_", "-")).baseName` and `new Intl.Locale(tag).language`, `.region`.
//
// Supported: `new Intl.Locale(tag, options)` (a string, a `Locale` or an object with `toString`); the options `language`,
// `script`, `region`, `calendar`, `collation`, `hourCycle`, `caseFirst`, `numeric` and `numberingSystem` (they replace the
// subtags and the `-u-` keywords of the tag); the getters `baseName`, `language`, `script`, `region`, `calendar`, `caseFirst`,
// `collation`, `hourCycle`, `numeric` and `numberingSystem`; `toString`, `maximize` and `minimize`.
//
// Known deviations: `maximize` and `minimize` use a table of 40 languages (`intl_locale.rs`), so a language that is not in it
// stays as it is. A tag is canonical by `intl::canonical_tag`: the alias rules of CLDR (`sgn-US` is `ase`, `en-posix` is
// `en-u-va-posix`) are not applied. `getCalendars`, `getCollations`, `getHourCycles`, `getNumberingSystems`, `getTextInfo`,
// `getTimeZones` and `getWeekInfo` (and the older getters of the same names) are absent.
//
// Storage: the canonical tag of each object in `intl.loc.locales`, capped with the other formatters by `max_formatters` and
// pruned by `collect`.

/// The state of `Intl.Locale` and `Intl.RelativeTimeFormat`. The objects are made on the first read of `Intl`.
struct IntlLoc {
    locales: FxMap<ObjId, Box<str>>,
    rtfs: FxMap<ObjId, IntlRtf>,
    loc_proto: ObjId,
    rtf_proto: ObjId,
    loc_ctor: ObjId,
    rtf_ctor: ObjId,
}

impl IntlLoc {
    fn new(o: ObjId) -> IntlLoc {
        IntlLoc { locales: FxMap::default(), rtfs: FxMap::default(), loc_proto: o, rtf_proto: o, loc_ctor: o, rtf_ctor: o }
    }
}

/// The getters of the prototype, in the order of `IntlFn::LocGet`.
const LOCALE_GETTERS: [&str; 10] = ["baseName", "language", "script", "region", "calendar", "caseFirst", "collation", "hourCycle", "numeric", "numberingSystem"];
/// The `-u-` key of each getter from `calendar` on (`LOCALE_GETTERS[4..]`).
const LOCALE_KEYS: [&str; 6] = ["ca", "kf", "co", "hc", "kn", "nu"];
const BAD_TAG: &str = "Incorrect locale information provided";

impl<'p> Interp<'p> {
    /// Make the constructors of `Locale` and `RelativeTimeFormat` on `intl`.
    fn intl_loc_install(&mut self, intl: ObjId) -> Res<()> {
        let (rtf_ctor, rtf_proto) = self.intl_ctor(intl, "RelativeTimeFormat", IntlFn::RtfCtor, IntlFn::Supported(4), "Intl.RelativeTimeFormat")?;
        self.method(rtf_proto, "resolvedOptions", Nat::Intl(IntlFn::RtfResolved), 0)?;
        self.method(rtf_proto, "format", Nat::Intl(IntlFn::RtfFormat(false)), 2)?;
        self.method(rtf_proto, "formatToParts", Nat::Intl(IntlFn::RtfFormat(true)), 2)?;
        let proto = h!(self, self.heap.new_object(Some(self.object_proto)));
        let ctor = self.native(Nat::Intl(IntlFn::LocCtor), "Locale", 1)?;
        self.def(ctor, self.k_prototype, Value::Object(proto), false, false, false)?;
        self.def(proto, self.k_constructor, Value::Object(ctor), true, false, true)?;
        let k = h!(self, self.heap.key_from_str("Locale"));
        self.def(intl, k, Value::Object(ctor), true, false, true)?;
        self.sc_tag(proto, "Intl.Locale")?;
        self.method(proto, "toString", Nat::Intl(IntlFn::LocToString), 0)?;
        self.method(proto, "maximize", Nat::Intl(IntlFn::LocMaximize(true)), 0)?;
        self.method(proto, "minimize", Nat::Intl(IntlFn::LocMaximize(false)), 0)?;
        for (i, n) in LOCALE_GETTERS.iter().enumerate() {
            self.getter(proto, n, Nat::Intl(IntlFn::LocGet(i as u8)))?;
        }
        self.intl.loc = IntlLoc { locales: FxMap::default(), rtfs: FxMap::default(), loc_proto: proto, rtf_proto, loc_ctor: ctor, rtf_ctor };
        Ok(())
    }

    fn this_locale(&mut self, this: Value, method: &str) -> Res<Box<str>> {
        if let Value::Object(o) = this
            && let Some(t) = self.intl.loc.locales.get(&o)
        {
            return Ok(t.clone());
        }
        let t = self.intl_show(this);
        Err(self.type_err(&format!("Method Intl.Locale.prototype.{method} called on incompatible receiver {t}")))
    }

    /// The canonical tag of `new Intl.Locale(tag, options)`.
    fn intl_locale_create(&mut self, tag: Value, options: Value) -> Res<String> {
        let tag = match tag {
            Value::Object(o) if let Some(t) = self.intl.loc.locales.get(&o) => t.to_string(),
            Value::String(_) | Value::Object(_) => {
                let s = self.coerce_string(tag)?;
                h!(self, self.heap.to_string_lossy(s))
            }
            _ => return Err(self.type_err("First argument to Intl.Locale constructor can't be empty or missing")),
        };
        let opts = self.intl_options(options, "Intl.Locale")?;
        if tag.is_empty() {
            return Err(self.range_error("First argument to Intl.Locale constructor can't be empty or missing"));
        }
        let Ok(canon) = intl::canonical_tag(&tag) else { return Err(self.range_error(BAD_TAG)) };
        let Some(_) = opts else { return Ok(canon) };
        // ApplyOptionsToTag, then ApplyUnicodeExtensionToTag
        let mut parts = [String::new(), String::new(), String::new()];
        for (p, n) in parts.iter_mut().zip(["language", "script", "region"]) {
            match self.intl_opt_str(opts, n)? {
                // a present but empty subtag is a RangeError, not an absent one
                Some(s) if s.is_empty() => return Err(self.range_error(BAD_TAG)),
                s => *p = s.unwrap_or_default(),
            }
        }
        let mut kws: Vec<(&str, String)> = Vec::new();
        for (i, n) in [(0, "calendar"), (1, "collation"), (3, "numberingSystem")] {
            if let Some(s) = self.intl_opt_str(opts, n)? {
                if !s.split('-').all(|p| (3..=8).contains(&p.len()) && p.bytes().all(|c| c.is_ascii_alphanumeric())) {
                    return Err(self.range_error(BAD_TAG));
                }
                kws.push((["ca", "co", "", "nu"][i], s.to_ascii_lowercase()));
            }
        }
        if let Some(i) = self.intl_opt_for("locale", opts, "hourCycle", &["h11", "h12", "h23", "h24"])? {
            kws.push(("hc", ["h11", "h12", "h23", "h24"][i].into()));
        }
        if let Some(i) = self.intl_opt_for("locale", opts, "caseFirst", &["upper", "lower", "false"])? {
            kws.push(("kf", ["upper", "lower", "false"][i].into()));
        }
        if let Some(b) = self.intl_opt_bool(opts, "numeric")? {
            kws.push(("kn", if b { String::new() } else { "false".into() }));
        }
        match intl_locale::apply(&canon, &parts[0], &parts[1], &parts[2], &kws) {
            Ok(t) => Ok(t),
            Err(_) => Err(self.range_error(BAD_TAG)),
        }
    }

    /// A `Locale` object with the canonical tag `tag`.
    fn intl_locale_object(&mut self, proto: ObjId, tag: String) -> Res<Value> {
        let o = h!(self, self.heap.new_object(Some(proto)));
        self.intl.loc.locales.insert(o, tag.into());
        Ok(Value::Object(o))
    }

    fn call_intl_locale(&mut self, f: IntlFn, this: Value, args: &[Value], is_new: bool) -> Res<Value> {
        let (a0, a1) = (arg(args, 0), arg(args, 1));
        match f {
            IntlFn::LocCtor => {
                let nt = self.cls.native_nt.take();
                if !is_new {
                    return Err(self.type_err("Constructor Intl.Locale requires 'new'"));
                }
                if self.intl_formatters() >= self.sc.limits.max_formatters {
                    return Err(Error::Limit(LimitKind::Sc(ScLimit::Formatters)));
                }
                let proto = self.nt_proto(nt, self.intl.loc.loc_proto)?;
                let tag = self.intl_locale_create(a0, a1)?;
                self.intl_locale_object(proto, tag)
            }
            IntlFn::LocToString => {
                let t = self.this_locale(this, "toString")?;
                self.str_value(&t)
            }
            IntlFn::LocMaximize(max) => {
                let t = self.this_locale(this, if max { "maximize" } else { "minimize" })?;
                if self.intl_formatters() >= self.sc.limits.max_formatters {
                    return Err(Error::Limit(LimitKind::Sc(ScLimit::Formatters)));
                }
                let t = if max { intl_locale::maximize(&t) } else { intl_locale::minimize(&t) };
                self.intl_locale_object(self.intl.loc.loc_proto, t)
            }
            IntlFn::LocGet(i) => {
                let t = self.this_locale(this, LOCALE_GETTERS[i as usize])?;
                let s = intl_locale::split(&t);
                let part = match i {
                    0 => intl_locale::base_name(&t),
                    1 => s.lang,
                    2 => s.script,
                    3 => s.region,
                    _ => {
                        let kw = intl_locale::keyword(&t, LOCALE_KEYS[i as usize - 4]);
                        return match (i, kw) {
                            (8, kw) => Ok(Value::Bool(kw.is_some_and(|v| v.is_empty() || v == "true"))),
                            (_, Some(v)) => self.str_value(&v),
                            (_, None) => Ok(Value::Undefined),
                        };
                    }
                };
                if part.is_empty() { Ok(Value::Undefined) } else { self.str_value(part) }
            }
            _ => self.call_intl_rtf(f, this, args, is_new),
        }
    }
}
