use super::{CYCLES, Format, Reader, arg, format as data, intl, owner, parts_array, properties, property, range};
use crate::bi_date::{self, Local};
use crate::bi_date_calc::{fields, time_clip};
use crate::interp_rt::{R, Realm};
use crate::value::Value;
use bare_intl::{number_options::Options, tz_names};
use std::rc::Rc;

const STYLES: &[&str] = &["full", "long", "medium", "short"];
const TEXT: &[&str] = &["narrow", "short", "long"];
const NUM: &[&str] = &["numeric", "2-digit"];
const MONTH: &[&str] = &["numeric", "2-digit", "narrow", "short", "long"];

pub(super) struct Formatter {
    locale: String,
    loc: u8,
    pattern: intl::Pattern,
    zone: Rc<Local>,
    zone_kind: u8,
    fraction: u8,
    styles: (u8, u8),
}

pub(super) fn new(o: &mut Reader<'_>, loc: u8, tags: &[String]) -> R<Formatter> {
    o.locale_matcher()?;
    let calendar = o.unicode_type("calendar")?;
    let numbering = o.unicode_type("numberingSystem")?;
    let hour12 = o.boolean("hour12")?;
    let cycle = o.read("hourCycle", CYCLES)?;
    let tag = tags.iter().find(|t| intl::lookup(t).is_some());
    let keyword = |key| tag.and_then(|t| intl::loc_keyword(t, key));
    let hc = keyword("hc").and_then(|v| CYCLES.iter().position(|s| *s == v)).map(|i| i as u8);
    let want_gregory = keyword("ca").is_some_and(|v| v == "gregory") && calendar.as_ref().is_none_or(|v| v == "gregory");
    let want_latn = keyword("nu").is_some_and(|v| v == "latn") && numbering.as_ref().is_none_or(|v| v == "latn");
    let zone = match o.string_option("timeZone")? {
        Some(name) => Rc::new(bi_date::zone_named(&name).ok_or_else(|| o.range_error(&format!("Invalid time zone: {name}")))?),
        None => bi_date::local(),
    };
    let mut r = intl::Request::default();
    r.weekday = o.read("weekday", TEXT)?.map_or(0, |i| i + intl::NARROW);
    if o.read("era", TEXT)?.is_some() {
        return Err(o.unsupported("Intl.DateTimeFormat option era"));
    }
    let width = |o: &mut Reader<'_>, name, choices| o.read(name, choices).map(|v| v.map_or(0, |i| i + 1));
    r.year = width(o, "year", NUM)?;
    r.month = width(o, "month", MONTH)?;
    r.day = width(o, "day", NUM)?;
    if o.read("dayPeriod", TEXT)?.is_some() {
        return Err(o.unsupported("Intl.DateTimeFormat option dayPeriod"));
    }
    r.hour = width(o, "hour", NUM)?;
    r.minute = width(o, "minute", NUM)?;
    r.second = width(o, "second", NUM)?;
    let digits = o.get("fractionalSecondDigits")?;
    let fraction = o.number_option(digits, "fractionalSecondDigits", 1.0, 3.0)?.unwrap_or(0) as u8;
    let mut zone_kind = width(o, "timeZoneName", &tz_names::KINDS)?;
    r.zone = zone_kind != 0;
    o.read("formatMatcher", &["basic", "best fit"])?;
    r.date_style = width(o, "dateStyle", STYLES)?;
    r.time_style = width(o, "timeStyle", STYLES)?;
    let explicit = r.weekday | r.year | r.month | r.day | r.hour | r.minute | r.second | zone_kind | fraction != 0;
    if r.date_style != 0 || r.time_style != 0 {
        if explicit {
            return Err(o.type_error("Date and time styles cannot be combined with fields"));
        }
    } else if !explicit {
        (r.year, r.month, r.day) = (intl::NUMERIC, intl::NUMERIC, intl::NUMERIC);
    }
    if fraction != 0 && r.second == 0 {
        return Err(o.unsupported("Intl.DateTimeFormat: fractionalSecondDigits without second"));
    }
    r.hc = intl::hour_cycle(loc, hour12, if hour12.is_some() { None } else { cycle.or(hc) });
    zone_kind = match r.time_style {
        1 => tz_names::LONG + 1,
        2 => tz_names::SHORT + 1,
        _ => zone_kind,
    };
    let pattern = intl::resolve(loc, &r);
    let mut locale = intl::locale_name(loc).to_owned();
    let keep_hc = hc.filter(|h| hour12.is_none() && cycle.is_none() || intl::shown(pattern).1 == Some(*h));
    if want_gregory || keep_hc.is_some() || want_latn {
        locale.push_str("-u");
        if want_gregory { locale.push_str("-ca-gregory"); }
        if let Some(h) = keep_hc {
            locale.push_str("-hc-");
            locale.push_str(CYCLES[h as usize]);
        }
        if want_latn { locale.push_str("-nu-latn"); }
    }
    Ok(Formatter { locale, loc, pattern, zone, zone_kind, fraction, styles: (r.date_style, r.time_style) })
}

pub(super) fn format(rt: &mut Realm, _: Value, args: &[Value]) -> R<Value> {
    let text = render(rt, owner(rt), arg(args, 0), None)?;
    Ok(rt.string(&text))
}

pub(super) fn format_to_parts(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let mut parts = Vec::new();
    let text = render(rt, this, arg(args, 0), Some(&mut parts))?;
    Ok(parts_array(rt, &text, parts.into_iter().map(|(p, end)| (intl::PART_NAMES[p as usize], end))))
}

fn render(rt: &mut Realm, this: Value, value: Value, mut parts: Option<&mut Vec<(intl::Part, usize)>>) -> R<String> {
    let Some(Format::Date(f)) = data(rt, this) else { return rt.type_error("incompatible Intl.DateTimeFormat receiver") };
    let f = Rc::clone(f);
    let t = time_clip(if value.is_undefined() { rt.clock_now_ms() } else { rt.to_number(value)? });
    let offset = f.zone.zone.offset((t / 1000.0).floor() as i64);
    let local = fields(t + offset as f64 * 1000.0).ok_or_else(|| range(rt, "Invalid time value"))?;
    let mut text = String::new();
    let zone = if f.zone_kind == 0 { String::new() } else { tz_names::name(&f.zone.name, offset, f.zone_kind - 1, f.loc) };
    let extras = intl::Extras { zone: &zone, fsd: f.fraction };
    intl::format(f.loc, f.pattern, &local, &extras, &mut text, parts.as_deref_mut())
        .map_err(|_| range(rt, "Unsupported date and time fields"))?;
    if f.pattern.has_zone() {
        intl::zone_tail(f.pattern, &zone, &mut text, parts);
    }
    Ok(text)
}

pub(super) fn resolved(rt: &mut Realm, this: Value, _: &[Value]) -> R<Value> {
    let Some(Format::Date(f)) = data(rt, this) else { return rt.type_error("incompatible Intl.DateTimeFormat receiver") };
    let f = Rc::clone(f);
    let result = properties(rt, &[
        ("locale", &f.locale), ("calendar", "gregory"), ("numberingSystem", "latn"), ("timeZone", &f.zone.name),
    ]);
    let (fields, cycle) = intl::shown(f.pattern);
    if let Some(cycle) = cycle {
        let value = rt.string(CYCLES[cycle as usize]);
        property(rt, result, "hourCycle", value);
        property(rt, result, "hour12", Value::bool(cycle < 2));
    }
    if f.styles == (0, 0) {
        for (key, value) in fields {
            let value = rt.string(value);
            property(rt, result, key, value);
        }
        if f.fraction != 0 {
            property(rt, result, "fractionalSecondDigits", Value::int(f.fraction as i32));
        }
        if f.zone_kind != 0 {
            let value = rt.string(tz_names::KINDS[f.zone_kind as usize - 1]);
            property(rt, result, "timeZoneName", value);
        }
    } else {
        for (key, style) in [("dateStyle", f.styles.0), ("timeStyle", f.styles.1)] {
            if style != 0 {
                let value = rt.string(STYLES[style as usize - 1]);
                property(rt, result, key, value);
            }
        }
    }
    Ok(result.value())
}
