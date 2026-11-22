//! Lazy Intl bindings. ICU data and formatting live in bare-intl.
use crate::bi_buffer::{ctor_proto, instance_proto, make_ctor, plain_proto, tag};
use crate::bi_object::Own;
use crate::builtins::{Hint, Native, NativeFn, arg};
use crate::interp_rt::{ErrKind, Key, R, Realm};
use crate::lazy::{Ent, factory, getter, late_table, m};
use crate::object::Obj;
use crate::shapes::{A_C, A_E, A_W};
use crate::string::Str;
use crate::value::Value;
use bare_intl::display_names::{IntlDn, dn_of};
use bare_intl::number_options::{self, Options};
use bare_intl::relative_time::{IntlRtf, rtf_parts};
use bare_intl::{icu, intl};
use std::{collections::HashMap, rc::Rc};

#[path = "bi_intl_date.rs"]
mod date;
#[cfg(test)]
#[path = "bi_intl_tests.rs"]
mod tests;

pub(crate) static GLOBALS: &[Ent] = &[factory("Intl", namespace)];

const COLL: usize = 0;
const DATE: usize = 1;
const NAMES: usize = 2;
const NUMBER: usize = 3;
const LOCALE: usize = 4;
const SEGMENTER: usize = 5;
const RELATIVE: usize = 6;
const LIST: usize = 7;

const NAMESPACES: [&str; 8] = ["Collator", "DateTimeFormat", "DisplayNames", "NumberFormat", "Locale", "Segmenter", "RelativeTimeFormat", "ListFormat"];
const TAGS: [&str; 8] = ["Intl.Collator", "Intl.DateTimeFormat", "Intl.DisplayNames", "Intl.NumberFormat", "Intl.Locale", "Intl.Segmenter", "Intl.RelativeTimeFormat", "Intl.ListFormat"];
const STYLES: &[&str] = &["long", "short", "narrow"];
const GRANULARITIES: &[&str] = &["grapheme", "word", "sentence"];
const DN_TYPES: &[&str] = &["language", "region", "script", "currency", "calendar", "dateTimeField"];
const RTF_UNITS: &[&str] = &["second", "minute", "hour", "day", "week", "month", "quarter", "year"];
const LF_TYPES: &[&str] = &["conjunction", "disjunction", "unit"];
const CYCLES: &[&str] = &["h11", "h12", "h23", "h24"];
const CASE_FIRST: &[&str] = &["upper", "lower", "false"];

enum Format {
    Collator(icu::CollKey),
    Date(Rc<date::Formatter>),
    Names(IntlDn),
    Number(icu::NumFmt),
    Locale(Box<str>),
    Segmenter(u8, u8),
    Relative(IntlRtf),
    List(u8, u8, u8),
}

#[derive(Default)]
pub(crate) struct State {
    /// Weak object keys. Formatter data holds no values from the JS heap.
    formats: HashMap<u64, (Value, Format)>,
    icu: icu::Cache,
    bound_key: Option<Str>,
    bound_natives: Option<usize>,
    locale_proto: Option<Obj>,
}

impl Realm {
    fn intl_state(&mut self) -> &mut State {
        self.intl.get_or_insert_with(Box::default)
    }
}

impl State {
    pub(crate) fn retain_live(&mut self, mut live: impl FnMut(Value) -> bool) {
        self.formats.retain(|_, (object, _)| live(*object));
    }
}

#[cold]
fn namespace(rt: &mut Realm) -> Value {
    let object = plain_proto(rt, rt.object_proto);
    tag(rt, object, "Intl");
    rt.lazy_table(object, late_table![
        factory("Collator", constructor::<COLL>),
        factory("DateTimeFormat", constructor::<DATE>),
        factory("DisplayNames", constructor::<NAMES>),
        factory("NumberFormat", constructor::<NUMBER>),
        factory("Locale", constructor::<LOCALE>),
        factory("Segmenter", constructor::<SEGMENTER>),
        factory("RelativeTimeFormat", constructor::<RELATIVE>),
        factory("ListFormat", constructor::<LIST>),
    ]);
    object.value()
}

#[cold]
fn constructor<const K: usize>(rt: &mut Realm) -> Value {
    let proto = plain_proto(rt, rt.object_proto);
    tag(rt, proto, TAGS[K]);
    let ctor = make_ctor(rt, NAMESPACES[K], (K == LOCALE) as u8, construct::<K>, proto);
    if K != LOCALE {
        // Each constructor has its own function object.
        rt.method(ctor, "supportedLocalesOf", 1, supported_locales);
    }
    let methods: &[Ent] = match K {
        COLL => late_table![getter("compare", "get compare", compare_get)],
        DATE => late_table![getter("format", "get format", date_get), m("resolvedOptions", 0, date::resolved)],
        NAMES => late_table![m("of", 1, name_of)],
        NUMBER => late_table![getter("format", "get format", number_get)],
        LOCALE => {
            rt.intl_state().locale_proto = Some(proto);
            late_table![
                m("toString", 0, locale_string), m("maximize", 0, locale_resize::<true>), m("minimize", 0, locale_resize::<false>),
                getter("baseName", "get baseName", locale_get::<0>),
                getter("language", "get language", locale_get::<1>),
                getter("script", "get script", locale_get::<2>),
                getter("region", "get region", locale_get::<3>),
                getter("calendar", "get calendar", locale_get::<4>),
                getter("caseFirst", "get caseFirst", locale_get::<5>),
                getter("collation", "get collation", locale_get::<6>),
                getter("hourCycle", "get hourCycle", locale_get::<7>),
                getter("numeric", "get numeric", locale_get::<8>),
                getter("numberingSystem", "get numberingSystem", locale_get::<9>),
            ]
        }
        SEGMENTER => late_table![m("resolvedOptions", 0, segmenter_resolved)],
        RELATIVE => late_table![m("format", 2, relative_format)],
        LIST => late_table![m("resolvedOptions", 0, list_resolved)],
        _ => unreachable!(),
    };
    rt.lazy_table(proto, methods);
    // A table keeps the address of its prototype even if script replaces the constructor.
    rt.keep(ctor.value());
    ctor.value()
}

fn string(rt: &mut Realm, v: Value) -> R<String> {
    let s = rt.to_str(v)?;
    let s = rt.flat(s);
    Ok(rt.text(s))
}

fn range(rt: &mut Realm, message: &str) -> Value {
    rt.error(ErrKind::Range, message)
}

fn language_tag(rt: &mut Realm, value: Value) -> R<String> {
    match rt.intl.as_ref().and_then(|state| state.formats.get(&value.bits())) {
        Some((_, Format::Locale(tag))) => Ok(tag.to_string()),
        _ => {
            let s = string(rt, value)?;
            intl::canonical_tag(&s).map_err(|_| range(rt, "Incorrect locale information provided"))
        }
    }
}

fn locales(rt: &mut Realm, value: Value) -> R<Vec<String>> {
    if Realm::str_of(value).is_some()
        || matches!(rt.intl.as_ref().and_then(|s| s.formats.get(&value.bits())), Some((_, Format::Locale(_))))
    {
        return Ok(vec![language_tag(rt, value)?]);
    }
    if value.is_null() {
        return rt.type_error("Cannot convert null to object");
    }
    let mut tags = Vec::new();
    if Realm::obj(value).is_some() {
        let len = rt.get_named(value, "length")?;
        let len = rt.to_number(len)?.trunc().max(0.0) as u64;
        for i in 0..len.min(9_007_199_254_740_991) {
            if rt.fuel == 0 {
                return Err(crate::interp::STOP);
            }
            rt.fuel -= 1;
            let key = if i < u32::MAX as u64 { Key::Idx(i as u32) } else { Key::Name(rt.intern(&i.to_string())) };
            if rt.has_prop(value, key)? {
                let entry = rt.get_prop(value, key)?;
                if Realm::str_of(entry).is_none() && Realm::obj(entry).is_none() {
                    return rt.type_error("Language ID must be a string or object");
                }
                let tag = language_tag(rt, entry)?;
                if !tags.contains(&tag) {
                    tags.push(tag);
                }
            }
        }
    }
    Ok(tags)
}

struct Reader<'a> {
    rt: &'a mut Realm,
    opts: Value,
    who: &'static str,
}

impl<'a> Reader<'a> {
    fn new(rt: &'a mut Realm, opts: Value, who: &'static str) -> R<Self> {
        if opts.is_null() {
            return rt.type_error("Cannot convert null to object");
        }
        Ok(Self { rt, opts, who })
    }

    fn read(&mut self, name: &str, values: &[&str]) -> R<Option<u8>> {
        Ok(self.choice(self.who, name, values)?.map(|i| i as u8))
    }

    fn locale_matcher(&mut self) -> R<()> {
        self.read("localeMatcher", &["lookup", "best fit"])?;
        Ok(())
    }

    fn boolean(&mut self, name: &str) -> R<Option<bool>> {
        let v = self.get(name)?;
        Ok((!v.is_undefined()).then(|| Realm::truthy(v)))
    }

    fn unicode_type(&mut self, name: &str) -> R<Option<String>> {
        let value = self.string_option(name)?;
        if value.as_ref().is_some_and(|s| !s.split('-').all(|p| (3..=8).contains(&p.len()) && p.bytes().all(|c| c.is_ascii_alphanumeric()))) {
            return Err(range(self.rt, &format!("Invalid {name}")));
        }
        Ok(value)
    }
}

impl Options for Reader<'_> {
    type Value = Value;
    type Error = Value;

    fn get(&mut self, name: &str) -> R<Value> {
        if self.opts.is_undefined() { Ok(Value::UNDEFINED) } else { self.rt.get_named(self.opts, name) }
    }

    fn is_undefined(v: Value) -> bool { v.is_undefined() }
    fn is_true(v: Value) -> bool { v.as_bool() == Some(true) }
    fn truthy(&mut self, v: Value) -> R<bool> { Ok(Realm::truthy(v)) }
    fn string(&mut self, v: Value) -> R<String> { string(self.rt, v) }
    fn show(&self, v: Value) -> String { self.rt.error_text(v) }

    fn number_option(&mut self, value: Value, name: &str, min: f64, max: f64) -> R<Option<u32>> {
        if value.is_undefined() {
            return Ok(None);
        }
        let n = self.rt.to_number(value)?;
        if !(min..=max).contains(&n) {
            return Err(self.range_error(&format!("{name} value is out of range")));
        }
        Ok(Some(n.floor() as u32))
    }

    fn range_error(&mut self, message: &str) -> Value { range(self.rt, message) }
    fn type_error(&mut self, message: &str) -> Value { self.rt.error(ErrKind::Type, message) }
    fn unsupported(&mut self, feature: &'static str) -> Value { range(self.rt, feature) }
}

fn supported_locales(rt: &mut Realm, _: Value, args: &[Value]) -> R<Value> {
    let tags = locales(rt, arg(args, 0))?;
    Reader::new(rt, arg(args, 1), "supportedLocalesOf")?.locale_matcher()?;
    let values: Vec<_> = tags.iter().filter(|s| intl::lookup(s).is_some()).map(|s| rt.string(s)).collect();
    Ok(rt.new_array(&values))
}

fn collator(options: &mut Reader<'_>, tags: &[String]) -> R<icu::CollKey> {
    options.read("usage", &["sort", "search"])?;
    options.locale_matcher()?;
    let collation = options.unicode_type("collation")?;
    let mut numeric = options.boolean("numeric")?;
    let mut case_first = options.read("caseFirst", CASE_FIRST)?.map(|i| [1, 2, 0][i as usize]);
    if let Some(tag) = tags.iter().find(|s| intl::lookup(s).is_some()) {
        if let Some(v) = intl::loc_keyword(tag, "kn") {
            numeric.get_or_insert(v != "false");
        }
        if let Some(i) = intl::loc_keyword(tag, "kf").and_then(|v| CASE_FIRST.iter().position(|s| *s == v)) {
            case_first.get_or_insert([1, 2, 0][i]);
        }
    }
    let sensitivity = options.read("sensitivity", &["base", "accent", "case", "variant"])?.unwrap_or(3);
    let ignore_punctuation = options.boolean("ignorePunctuation")?.unwrap_or(false);
    if collation.as_deref().is_some_and(|s| matches!(s, "emoji" | "eor")) {
        return Err(options.unsupported("Intl.Collator option collation"));
    }
    Ok(icu::CollKey { sensitivity, case_first: case_first.unwrap_or(0), numeric: numeric.unwrap_or(false), ignore_punctuation })
}

fn construct<const K: usize>(rt: &mut Realm, _: Value, args: &[Value]) -> R<Value> {
    let proto = if rt.nt.is_undefined() && matches!(K, COLL | DATE | NUMBER) {
        ctor_proto(rt, Realm::obj(rt.callee).expect("Intl constructor"))
    } else {
        instance_proto(rt)?
    };
    let a0 = arg(args, 0);
    if K == LOCALE {
        if Realm::str_of(a0).is_none() && Realm::obj(a0).is_none() {
            return rt.type_error("Intl.Locale requires a language tag");
        }
        let tag = language_tag(rt, a0)?;
        let mut opts = Reader::new(rt, arg(args, 1), TAGS[K])?;
        let tag = locale_options(&mut opts, tag)?;
        return Ok(remember(rt, proto, Format::Locale(tag.into())));
    }
    let tags = locales(rt, a0)?;
    let loc = tags.iter().find_map(|t| intl::lookup(t)).unwrap_or(0);
    let mut opts = Reader::new(rt, arg(args, 1), TAGS[K])?;
    let data = match K {
        COLL => Format::Collator(collator(&mut opts, &tags)?),
        DATE => Format::Date(Rc::new(date::new(&mut opts, loc, &tags)?)),
        NAMES => {
            if !opts.opts.is_undefined() && Realm::obj(opts.opts).is_none() {
                return Err(opts.type_error("Intl.DisplayNames options must be an object"));
            }
            opts.locale_matcher()?;
            let style = opts.read("style", STYLES)?.unwrap_or(0);
            let ty = opts.read("type", DN_TYPES)?.ok_or_else(|| opts.type_error("Intl.DisplayNames requires a type"))?;
            let code = opts.read("fallback", &["code", "none"])? != Some(1);
            let dialect = opts.read("languageDisplay", &["dialect", "standard"])? != Some(1);
            Format::Names(IntlDn { loc, style, ty, code, dialect })
        }
        NUMBER => {
            opts.locale_matcher()?;
            let nu = opts.unicode_type("numberingSystem")?;
            let gate = nu.filter(|s| !s.eq_ignore_ascii_case("latn")).map(|_| "Intl.NumberFormat option numberingSystem");
            Format::Number(number_options::initialize(&mut opts, TAGS[K], loc, gate)?)
        }
        SEGMENTER => {
            opts.locale_matcher()?;
            Format::Segmenter(loc, opts.read("granularity", GRANULARITIES)?.unwrap_or(0))
        }
        RELATIVE => {
            opts.locale_matcher()?;
            let nu = opts.unicode_type("numberingSystem")?;
            let style = opts.read("style", STYLES)?.unwrap_or(0);
            let auto = opts.read("numeric", &["always", "auto"])? == Some(1);
            if nu.is_some_and(|s| !s.eq_ignore_ascii_case("latn")) {
                return Err(opts.unsupported("Intl.RelativeTimeFormat option numberingSystem"));
            }
            Format::Relative(IntlRtf { loc, style, auto })
        }
        LIST => {
            opts.locale_matcher()?;
            let ty = opts.read("type", LF_TYPES)?.unwrap_or(0);
            Format::List(loc, ty, opts.read("style", STYLES)?.unwrap_or(0))
        }
        _ => unreachable!(),
    };
    Ok(remember(rt, proto, data))
}

fn remember(rt: &mut Realm, proto: Obj, data: Format) -> Value {
    let object = plain_proto(rt, proto);
    if matches!(data, Format::Collator(_) | Format::Date(_) | Format::Number(_)) {
        let key = bound_key(rt);
        rt.define_attrs(object, Key::Name(key), 0, Value::UNDEFINED).expect("private formatter slot");
    }
    let value = object.value();
    rt.intl_state().formats.insert(value.bits(), (value, data));
    value
}

fn format(rt: &Realm, object: Value) -> Option<&Format> {
    Some(&rt.intl.as_ref()?.formats.get(&object.bits())?.1)
}

fn bound_key(rt: &mut Realm) -> Str {
    if let Some(key) = rt.intl_state().bound_key {
        return key;
    }
    // Use a private key outside the atom table, as for internal buffer slots.
    let key = Str::from_str(&mut rt.heap, "\0Intl bound").expect("memory");
    rt.keep(key.value());
    rt.intl_state().bound_key = Some(key);
    key
}

fn bound(rt: &mut Realm, this: Value, kind: usize) -> R<Value> {
    if !matches!((kind, format(rt, this)), (0, Some(Format::Collator(_))) | (1, Some(Format::Date(_))) | (2, Some(Format::Number(_)))) {
        return rt.type_error("incompatible Intl receiver");
    }
    let object = Realm::obj(this).expect("Intl object");
    let key = bound_key(rt);
    if let Some(Own::Data(value, _)) = rt.get_own(object, Key::Name(key))
        && !value.is_undefined()
    {
        return Ok(value);
    }
    let base = match rt.intl_state().bound_natives {
        Some(base) => base,
        None => {
            let base = rt.natives.len();
            let calls: [NativeFn; 3] = [compare_call, date::format, number_call];
            rt.natives.extend(calls.into_iter().enumerate().map(|(i, f)| Native { f, ctor: false, len: if i == 0 { 2 } else { 1 }, name: "" }));
            rt.intl_state().bound_natives = Some(base);
            base
        }
    };
    let function = rt.native_cell(base + kind);
    function.set_slot(&rt.shapes, 1, this);
    rt.define_attrs(object, Key::Name(key), 0, function.value())?;
    Ok(function.value())
}

fn compare_get(rt: &mut Realm, this: Value, _: &[Value]) -> R<Value> { bound(rt, this, 0) }
fn date_get(rt: &mut Realm, this: Value, _: &[Value]) -> R<Value> { bound(rt, this, 1) }
fn number_get(rt: &mut Realm, this: Value, _: &[Value]) -> R<Value> { bound(rt, this, 2) }

fn owner(rt: &Realm) -> Value {
    Realm::obj(rt.callee).expect("Intl function").slot(&rt.shapes, 1)
}

fn compare(rt: &mut Realm, key: icu::CollKey, left: &str, right: &str) -> Value {
    let c = rt.intl_state().icu.collator(key);
    Value::int(icu::compare(&c, left, right))
}

fn compare_call(rt: &mut Realm, _: Value, args: &[Value]) -> R<Value> {
    let Some(&Format::Collator(key)) = format(rt, owner(rt)) else { return rt.type_error("incompatible Intl.Collator receiver") };
    let left = string(rt, arg(args, 0))?;
    let right = string(rt, arg(args, 1))?;
    Ok(compare(rt, key, &left, &right))
}

pub(crate) fn locale_compare(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let s = rt.this_str(this, "localeCompare")?;
    let s = rt.flat(s);
    let left = rt.text(s);
    let right = string(rt, arg(args, 0))?;
    let tags = locales(rt, arg(args, 1))?;
    let key = collator(&mut Reader::new(rt, arg(args, 2), "String.prototype.localeCompare")?, &tags)?;
    Ok(compare(rt, key, &left, &right))
}

fn name_of(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let Some(&Format::Names(names)) = format(rt, this) else { return rt.type_error("incompatible Intl.DisplayNames receiver") };
    let code = string(rt, arg(args, 0))?;
    let name = dn_of(names, &code).map_err(|_| range(rt, "invalid display name code"))?;
    Ok(name.map_or(Value::UNDEFINED, |s| rt.string(&s)))
}

fn number_call(rt: &mut Realm, _: Value, args: &[Value]) -> R<Value> {
    let Some(&Format::Number(f)) = format(rt, owner(rt)) else { return rt.type_error("incompatible Intl.NumberFormat receiver") };
    let value = rt.to_primitive(arg(args, 0), Hint::Number)?;
    let exact = if Realm::str_of(value).is_some() || value.is_bigint() {
        let s = string(rt, value)?;
        icu::num_from_decimal_str(s.trim_matches(|c| u16::try_from(c as u32).is_ok_and(crate::bi_string::is_space)))
    } else {
        None
    };
    let n = match exact { Some(n) => n, None => icu::num_from_f64(rt.to_number(value)?) };
    let mut s = String::new();
    rt.intl_state().icu.format(&f, n, icu::Out { s: &mut s, parts: None });
    Ok(rt.string(&s))
}

fn locale_options(opts: &mut Reader<'_>, tag: String) -> R<String> {
    if opts.opts.is_undefined() {
        return Ok(tag);
    }
    let mut parts = [String::new(), String::new(), String::new()];
    for (part, name) in parts.iter_mut().zip(["language", "script", "region"]) {
        if let Some(value) = opts.string_option(name)? {
            if value.is_empty() {
                return Err(opts.range_error("Incorrect locale information provided"));
            }
            *part = value;
        }
    }
    let mut kws = Vec::new();
    for (key, name) in [("ca", "calendar"), ("co", "collation"), ("nu", "numberingSystem")] {
        if let Some(v) = opts.unicode_type(name)? { kws.push((key, v.to_ascii_lowercase())); }
    }
    for (key, name, choices) in [("hc", "hourCycle", CYCLES), ("kf", "caseFirst", CASE_FIRST)] {
        if let Some(i) = opts.read(name, choices)? { kws.push((key, choices[i as usize].to_owned())); }
    }
    if let Some(v) = opts.boolean("numeric")? {
        kws.push(("kn", if v { "" } else { "false" }.to_owned()));
    }
    intl::loc_apply(&tag, &parts[0], &parts[1], &parts[2], &kws)
        .map_err(|_| opts.range_error("Incorrect locale information provided"))
}

fn locale_string(rt: &mut Realm, this: Value, _: &[Value]) -> R<Value> {
    let Some(Format::Locale(tag)) = format(rt, this) else { return rt.type_error("incompatible Intl.Locale receiver") };
    let tag = tag.clone();
    Ok(rt.string(&tag))
}

fn locale_resize<const MAX: bool>(rt: &mut Realm, this: Value, _: &[Value]) -> R<Value> {
    let Some(Format::Locale(tag)) = format(rt, this) else { return rt.type_error("incompatible Intl.Locale receiver") };
    let tag = if MAX { intl::loc_maximize(tag) } else { intl::loc_minimize(tag) };
    let proto = rt.intl_state().locale_proto.expect("Locale prototype");
    Ok(remember(rt, proto, Format::Locale(tag.into())))
}

fn locale_get<const I: usize>(rt: &mut Realm, this: Value, _: &[Value]) -> R<Value> {
    let Some(Format::Locale(tag)) = format(rt, this) else { return rt.type_error("incompatible Intl.Locale receiver") };
    let p = intl::loc_split(tag);
    let value = match I {
        0 => Some(intl::loc_base_name(tag).to_owned()),
        1..=3 => Some([p.lang, p.script, p.region][I - 1].to_owned()),
        _ => intl::loc_keyword(tag, ["ca", "kf", "co", "hc", "kn", "nu"][I - 4]),
    };
    if I == 8 {
        return Ok(Value::bool(value.is_some_and(|v| v.is_empty() || v == "true")));
    }
    Ok(value.filter(|v| !v.is_empty()).map_or(Value::UNDEFINED, |s| rt.string(&s)))
}

fn properties(rt: &mut Realm, key_values: &[(&str, &str)]) -> Obj {
    let object = plain_proto(rt, rt.object_proto);
    for &(key, value) in key_values {
        let value = rt.string(value);
        property(rt, object, key, value);
    }
    object
}

fn property(rt: &mut Realm, object: Obj, key: &str, value: Value) {
    let key = rt.intern(key);
    rt.define_attrs(object, Key::Name(key), A_W | A_E | A_C, value).expect("new result property");
}

fn segmenter_resolved(rt: &mut Realm, this: Value, _: &[Value]) -> R<Value> {
    let Some(&Format::Segmenter(loc, gran)) = format(rt, this) else { return rt.type_error("incompatible Intl.Segmenter receiver") };
    Ok(properties(rt, &[("locale", intl::locale_name(loc)), ("granularity", GRANULARITIES[gran as usize])]).value())
}

fn relative_format(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let Some(&Format::Relative(r)) = format(rt, this) else { return rt.type_error("incompatible Intl.RelativeTimeFormat receiver") };
    let n = rt.to_number(arg(args, 0))?;
    let unit = string(rt, arg(args, 1))?;
    if !n.is_finite() {
        return Err(range(rt, "relative time must be finite"));
    }
    let index = RTF_UNITS.iter().position(|u| unit == *u || unit.strip_suffix('s') == Some(*u))
        .ok_or_else(|| range(rt, "invalid relative time unit"))?;
    let parts = rtf_parts(&mut rt.intl_state().icu, r, n, index);
    let text: String = parts.into_iter().map(|(_, s)| s).collect();
    Ok(rt.string(&text))
}

fn list_resolved(rt: &mut Realm, this: Value, _: &[Value]) -> R<Value> {
    let Some(&Format::List(loc, ty, style)) = format(rt, this) else { return rt.type_error("incompatible Intl.ListFormat receiver") };
    Ok(properties(rt, &[("locale", intl::locale_name(loc)), ("type", LF_TYPES[ty as usize]), ("style", STYLES[style as usize])]).value())
}
