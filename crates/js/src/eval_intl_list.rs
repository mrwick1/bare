// `Intl.ListFormat` (ECMA-402 section 13) and `Intl.DisplayNames` (section 12). Included by `eval_intl.rs`. The names are in
// `intl_names_data.rs` (made by `tools/js/intl_names_gen.mjs` from Node's ICU); the list separators are in `lf_seps` below.
//
// Observed need: GitHub's issues page calls `new Intl.ListFormat(...)` when it loads. Without it the page shows "Unable to load
// page." (bare#394).
//
// Supported:
// - `new Intl.ListFormat(locales, options)` (needs `new`), `supportedLocalesOf`, `format(list)`, `formatToParts(list)`,
//   `resolvedOptions`; `type` conjunction, disjunction and unit; `style` long, short and narrow. The separators are those of
//   ICU for the 5 locales of `intl.rs` (en and en-US have the serial comma, en-CA, en-GB and en-IN have not).
// - `new Intl.DisplayNames(locales, options)` (needs `new`, `type` is required), `supportedLocalesOf`, `of(code)`,
//   `resolvedOptions`; `type` language, region, script, currency, calendar and dateTimeField; `style`; `fallback` code and
//   none; `languageDisplay` dialect and standard. A language name is the language (or a dialect name like "American English")
//   with its script and region in parentheses: "Chinese (Traditional, Taiwan)".
//
// Known deviations: the names are a short list of common codes (`intl_names_data.rs`), not all of CLDR. An unknown code gives
// the code (fallback "code") or undefined (fallback "none"), as for a code that ICU does not know. The names are those of en-US
// for every locale (en-GB says "Russian Rouble", bare says "Russian Ruble"). A variant subtag of a language is shown as its code
// ("en-US-posix" is "American English (posix)", V8 says "(Computer)"). A lowercase region or script is made canonical before
// the lookup, as the spec says ("in" is "India"); V8 returns such a code as it is. Another locale (`de`) resolves to its English
// parent or to `en-US`, so its text is English.
//
// Both constructors are accessors on `Intl` until they are first read (see `intl_loc_lazy` in `eval_intl_locale.rs`).
//
// Storage: one `IntlLf` in `intl.loc.lfs` and one `IntlDn` in `intl.loc.dns` per object, capped with the other formatters by
// `max_formatters` and pruned by `collect`.

/// A list formatter. `#[derive(Clone, Copy)]` lets Rust copy the struct like a number, so a lookup can return it by value
/// (like reading a plain object in JavaScript and keeping the fields).
#[derive(Clone, Copy)]
struct IntlLf {
    loc: u8,
    /// An index of `LF_TYPES`.
    ty: u8,
    /// An index of `RTF_STYLES`: 0 long, 1 short, 2 narrow.
    style: u8,
}

/// A display names object.
#[derive(Clone, Copy)]
struct IntlDn {
    loc: u8,
    style: u8,
    /// An index of `DN_TYPES`.
    ty: u8,
    /// `fallback: "code"` (true) or `"none"`.
    code: bool,
    /// `languageDisplay: "dialect"` (true) or `"standard"`.
    dialect: bool,
}

const LF_TYPES: [&str; 3] = ["conjunction", "disjunction", "unit"];
const DN_TYPES: [&str; 6] = ["language", "region", "script", "currency", "calendar", "dateTimeField"];
/// The error of V8 for bad options of these two constructors and for a bad code of `of`.
const INVALID_ARGUMENT: &str = "invalid_argument";

/// The separators of a list: between the two items of a pair, between the first items of a longer list, before its last item.
/// `loc` is an index of `intl::LOCALES` (en-US, en, en-CA, en-IN, en-GB).
fn lf_seps(loc: u8, ty: u8, style: u8) -> (&'static str, &'static str, &'static str) {
    // en and en-US put a comma before the last item ("a, b, and c")
    let serial = loc <= 1;
    match (ty, style) {
        (2, 2) => (" ", " ", " "),
        (2, _) => (", ", ", ", ", "),
        (1, _) if serial => (" or ", ", ", ", or "),
        (1, _) => (" or ", ", ", " or "),
        (_, 2) if loc == 3 => (", ", ", ", ", and "),
        (_, 2) => (", ", ", ", ", "),
        (_, 0) if serial => (" and ", ", ", ", and "),
        (_, _) if serial => (" & ", ", ", ", & "),
        (_, _) => (" and ", ", ", " and "),
    }
}

/// The name of `code` in a table of `intl_names_data.rs` for `style`, or `None`. The rows are sorted by code.
fn dn_lookup(table: &[(&str, &'static str, &'static str, &'static str)], code: &str, style: u8) -> Option<&'static str> {
    let i = table.binary_search_by(|r| r.0.cmp(code)).ok()?;
    let (_, long, short, narrow) = table[i];
    // short and narrow are "" when they equal the long name
    let s = [long, short, narrow][style as usize];
    Some(if s.is_empty() { long } else { s })
}

/// `s` with its first letter upper case and the rest lower case (a script code: "latn" is "Latn").
fn title_case(s: &str) -> String {
    let mut out = s.to_ascii_lowercase();
    out[..1].make_ascii_uppercase();
    out
}

/// The display name of a language tag, or `None` when `fallback: "none"` and a part is unknown. `Err` when `tag` is not a
/// `unicode_language_id` (an extension or a private use part is an error, as in V8).
fn dn_language(d: IntlDn, tag: &str) -> Result<Option<String>, ()> {
    let canon = intl::canonical_tag(tag).map_err(|_| ())?;
    let s = intl::loc_split(&canon);
    if !s.ext.is_empty() {
        return Err(());
    }
    // a closure (like an arrow function): the name of a code, else the code itself with fallback "code"
    let named = |table: &[(&str, &'static str, &'static str, &'static str)], code: &str| dn_lookup(table, code, d.style).map(str::to_string).or_else(|| d.code.then(|| code.to_string()));
    // a dialect name covers the language with its script or with its region ("zh-Hans", "en-US"), never with both
    let dialect = if d.dialect && (s.script.is_empty() || s.region.is_empty()) {
        let key: Vec<&str> = [s.lang, s.script, s.region].into_iter().filter(|p| !p.is_empty()).collect();
        dn_lookup(&intl::NAMES_DIALECT, &key.join("-"), d.style)
    } else {
        None
    };
    let mut quals: Vec<String> = Vec::new();
    let base = match dialect {
        Some(n) => n.to_string(),
        None => {
            for (table, code) in [(&intl::NAMES_SCRIPT[..], s.script), (&intl::NAMES_REGION[..], s.region)] {
                if code.is_empty() {
                    continue;
                }
                let Some(n) = named(table, code) else { return Ok(None) };
                quals.push(n);
            }
            let Some(n) = named(&intl::NAMES_LANGUAGE, s.lang) else { return Ok(None) };
            n
        }
    };
    for v in s.variants.split('-').filter(|v| !v.is_empty()) {
        let Some(n) = d.code.then(|| v.to_string()) else { return Ok(None) };
        quals.push(n);
    }
    Ok(Some(if quals.is_empty() { base } else { format!("{base} ({})", quals.join(", ")) }))
}

impl<'p> Interp<'p> {
    /// Make `Intl.ListFormat` (`dn` false) or `Intl.DisplayNames` and define it on `Intl`.
    fn intl_list_make(&mut self, dn: bool) -> Res<ObjId> {
        let intl = self.intl.intl;
        if dn {
            let (ctor, proto) = self.intl_ctor(intl, "DisplayNames", IntlFn::DnCtor, IntlFn::Supported(6), "Intl.DisplayNames")?;
            self.def(ctor, self.k_length, Value::Number(2.0), false, false, true)?;
            self.method(proto, "resolvedOptions", Nat::Intl(IntlFn::DnResolved), 0)?;
            self.method(proto, "of", Nat::Intl(IntlFn::DnOf), 1)?;
            (self.intl.loc.dn_ctor, self.intl.loc.dn_proto) = (ctor, proto);
            return Ok(ctor);
        }
        let (ctor, proto) = self.intl_ctor(intl, "ListFormat", IntlFn::LfCtor, IntlFn::Supported(5), "Intl.ListFormat")?;
        self.method(proto, "resolvedOptions", Nat::Intl(IntlFn::LfResolved), 0)?;
        self.method(proto, "format", Nat::Intl(IntlFn::LfFormat(false)), 1)?;
        self.method(proto, "formatToParts", Nat::Intl(IntlFn::LfFormat(true)), 1)?;
        (self.intl.loc.lf_ctor, self.intl.loc.lf_proto) = (ctor, proto);
        Ok(ctor)
    }

    /// GetOptionsObject: `None` for undefined, a TypeError for a value that is not an object.
    fn intl_options_object(&mut self, options: Value) -> Res<Option<Value>> {
        match options {
            Value::Undefined => Ok(None),
            Value::Object(_) => Ok(Some(options)),
            _ => Err(self.type_err(INVALID_ARGUMENT)),
        }
    }

    fn intl_incompatible(&mut self, this: Value, method: &str) -> Error {
        let t = self.intl_show(this);
        self.type_err(&format!("Method {method} called on incompatible receiver {t}"))
    }

    /// The items of `format(list)`: StringListFromIterable. A value that is not a string closes the iterator.
    fn lf_items(&mut self, list: Value) -> Res<Vec<StrId>> {
        if matches!(list, Value::Undefined) {
            return Ok(Vec::new());
        }
        let mut it = self.iter_start_val(list)?;
        let mut out = Vec::new();
        while let Some(x) = self.iter_next(&mut it)? {
            let Value::String(s) = x else {
                let shown = match x {
                    Value::Object(_) => self.intl_show(x),
                    _ => {
                        let s = self.coerce_string(x)?;
                        h!(self, self.heap.to_string_lossy(s))
                    }
                };
                let e = self.type_err(&format!("Iterable yielded {shown} which is not a string"));
                return Err(self.iter_close_throw(&mut it, e));
            };
            self.iter_buffer_room(out.len())?;
            out.push(s);
        }
        Ok(out)
    }

    fn call_intl_list(&mut self, f: IntlFn, this: Value, args: &[Value], is_new: bool) -> Res<Value> {
        let (a0, a1) = (arg(args, 0), arg(args, 1));
        match f {
            IntlFn::LfCtor | IntlFn::DnCtor => {
                let dn = f == IntlFn::DnCtor;
                let who = if dn { "Intl.DisplayNames" } else { "Intl.ListFormat" };
                let nt = self.cls.native_nt.take();
                if !is_new {
                    return Err(self.type_err(&format!("Constructor {who} requires 'new'")));
                }
                if self.intl_formatters() >= self.sc.limits.max_formatters {
                    return Err(Error::Limit(LimitKind::Sc(ScLimit::Formatters)));
                }
                let proto = self.nt_proto(nt, if dn { self.intl.loc.dn_proto } else { self.intl.loc.lf_proto })?;
                let requested = self.intl_locales(a0)?;
                let opts = self.intl_options_object(a1)?;
                self.intl_opt_for(who, opts, "localeMatcher", &["lookup", "best fit"])?;
                let loc = self.intl_pick(&requested).unwrap_or(0);
                if !dn {
                    let ty = self.intl_opt_for(who, opts, "type", &LF_TYPES)?.unwrap_or(0) as u8;
                    let style = self.intl_opt_for(who, opts, "style", &RTF_STYLES)?.unwrap_or(0) as u8;
                    let o = h!(self, self.heap.new_object(Some(proto)));
                    self.intl.loc.lfs.insert(o, IntlLf { loc, ty, style });
                    return Ok(Value::Object(o));
                }
                let style = self.intl_opt_for(who, opts, "style", &RTF_STYLES)?.unwrap_or(0) as u8;
                let Some(ty) = self.intl_opt_for(who, opts, "type", &DN_TYPES)? else { return Err(self.type_err(INVALID_ARGUMENT)) };
                let code = self.intl_opt_for(who, opts, "fallback", &["code", "none"])? != Some(1);
                let dialect = self.intl_opt_for(who, opts, "languageDisplay", &["dialect", "standard"])? != Some(1);
                let o = h!(self, self.heap.new_object(Some(proto)));
                self.intl.loc.dns.insert(o, IntlDn { loc, style, ty: ty as u8, code, dialect });
                Ok(Value::Object(o))
            }
            IntlFn::LfFormat(to_parts) => {
                let method = if to_parts { "Intl.ListFormat.prototype.formatToParts" } else { "Intl.ListFormat.prototype.format" };
                let l = match this {
                    Value::Object(o) if let Some(l) = self.intl.loc.lfs.get(&o) => *l,
                    _ => return Err(self.intl_incompatible(this, method)),
                };
                let items = self.lf_items(a0)?;
                let (two, mid, end) = lf_seps(l.loc, l.ty, l.style);
                // the parts in order: (is an element, the string), separators between the elements
                let n = items.len();
                let mut parts: Vec<(bool, StrId)> = Vec::with_capacity(n * 2);
                for (i, &s) in items.iter().enumerate() {
                    if i > 0 {
                        let sep = if n == 2 { two } else if i == n - 1 { end } else { mid };
                        parts.push((false, h!(self, self.heap.intern_str(sep))));
                    }
                    parts.push((true, s));
                }
                if !to_parts {
                    let mut units: Vec<u16> = Vec::new();
                    for &(_, s) in &parts {
                        units.extend_from_slice(h!(self, self.heap.str_units(s)));
                    }
                    self.spend_units(units.len())?;
                    return self.new_str(units);
                }
                let (kt, kv) = (h!(self, self.heap.key_from_str("type")), h!(self, self.heap.key_from_str("value")));
                let (te, tl) = (self.str_value("element")?, self.str_value("literal")?);
                let mut out = Vec::with_capacity(parts.len());
                for (el, s) in parts {
                    let o = h!(self, self.heap.new_object(Some(self.object_proto)));
                    self.def(o, kt, if el { te } else { tl }, true, true, true)?;
                    self.def(o, kv, Value::String(s), true, true, true)?;
                    out.push(Value::Object(o));
                }
                self.new_array_from(&out)
            }
            IntlFn::LfResolved => {
                let l = match this {
                    Value::Object(o) if let Some(l) = self.intl.loc.lfs.get(&o) => *l,
                    _ => return Err(self.intl_incompatible(this, "Intl.ListFormat.prototype.resolvedOptions")),
                };
                let props: Vec<(&str, Value)> = vec![
                    ("locale", self.str_value(intl::locale_name(l.loc))?),
                    ("type", self.str_value(LF_TYPES[l.ty as usize])?),
                    ("style", self.str_value(RTF_STYLES[l.style as usize])?),
                ];
                let o = h!(self, self.heap.new_object(Some(self.object_proto)));
                self.intl_fill(o, props)?;
                Ok(Value::Object(o))
            }
            IntlFn::DnOf => {
                let d = match this {
                    Value::Object(o) if let Some(d) = self.intl.loc.dns.get(&o) => *d,
                    _ => return Err(self.intl_incompatible(this, "Intl.DisplayNames.prototype.of")),
                };
                let code = self.coerce_string(a0)?;
                let code = h!(self, self.heap.to_string_lossy(code));
                match self.dn_of(d, &code) {
                    Err(()) => Err(self.range_error(INVALID_ARGUMENT)),
                    Ok(None) => Ok(Value::Undefined),
                    Ok(Some(s)) => self.str_value(&s),
                }
            }
            IntlFn::DnResolved => {
                let d = match this {
                    Value::Object(o) if let Some(d) = self.intl.loc.dns.get(&o) => *d,
                    _ => return Err(self.intl_incompatible(this, "Intl.DisplayNames.prototype.resolvedOptions")),
                };
                let mut props: Vec<(&str, Value)> = vec![
                    ("locale", self.str_value(intl::locale_name(d.loc))?),
                    ("style", self.str_value(RTF_STYLES[d.style as usize])?),
                    ("type", self.str_value(DN_TYPES[d.ty as usize])?),
                    ("fallback", self.str_value(if d.code { "code" } else { "none" })?),
                ];
                if d.ty == 0 {
                    props.push(("languageDisplay", self.str_value(if d.dialect { "dialect" } else { "standard" })?));
                }
                let o = h!(self, self.heap.new_object(Some(self.object_proto)));
                self.intl_fill(o, props)?;
                Ok(Value::Object(o))
            }
            _ => unreachable!("handled in call_intl_num"),
        }
    }

    /// `of(code)`: `Err` for a code that is not valid for the type (a RangeError), `Ok(None)` for undefined.
    fn dn_of(&self, d: IntlDn, code: &str) -> Result<Option<String>, ()> {
        let alpha = |n: usize| code.len() == n && code.bytes().all(|c| c.is_ascii_alphabetic());
        // the canonical code and its table
        let (canon, table): (String, &[(&str, &'static str, &'static str, &'static str)]) = match d.ty {
            0 => return dn_language(d, code),
            1 if alpha(2) || (code.len() == 3 && code.bytes().all(|c| c.is_ascii_digit())) => (code.to_ascii_uppercase(), &intl::NAMES_REGION),
            2 if alpha(4) => (title_case(code), &intl::NAMES_SCRIPT),
            3 if alpha(3) => (code.to_ascii_uppercase(), &intl::NAMES_CURRENCY),
            4 if !code.is_empty() && code.split('-').all(|p| (3..=8).contains(&p.len()) && p.bytes().all(|c| c.is_ascii_alphanumeric())) => (code.to_ascii_lowercase(), &intl::NAMES_CALENDAR),
            5 if intl::NAMES_FIELD.iter().any(|r| r.0 == code) => (code.to_string(), &intl::NAMES_FIELD),
            _ => return Err(()),
        };
        match dn_lookup(table, &canon, d.style) {
            Some(n) => Ok(Some(n.to_string())),
            None if d.code => Ok(Some(canon)),
            None => Ok(None),
        }
    }
}
