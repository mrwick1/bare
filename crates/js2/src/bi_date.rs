//! `Date` (plan phase 2: the census group `Date`, 19 of 50 workloads call `Date.now`, 12 make dates). Safe code over [`Realm`].
//!
//! **The object.** A plain `Object` cell with the flag [`F_DATE`] and one hidden property, `Realm::k_prim`, that holds the time value
//! (`[[DateValue]]`): a number, whole milliseconds since 1970-01-01T00:00:00Z within +-8.64e15, or NaN. It is the shape of a wrapper
//! (`bi_wrap.rs`) with another flag, so a script cannot make or move it, and a class that extends `Date` gets the same cell over its own
//! prototype. A `Date` costs one 24-byte cell.
//!
//! **The arithmetic** is `bi_date_calc.rs` (UTC only, the parser, the formatter) and the zone is `bi_date_tz.rs` (TZif rules of
//! `bare-tzdata`, which is in the binary). Both are copies of `crates/js/src/date.rs` and `tz.rs`.
//!
//! **The local zone** is found at the first local-time call and never before: `TZ` (a leading `:` is cut), else the zone of the OS
//! (`bare_os::tz::local_zone`), else UTC. A name that is not a zone is skipped. It is one per thread, as the environment is one per
//! process. A wall-clock time in an overlap (clocks go back) is the first instant; one in a gap (clocks go forward) uses the offset from
//! before the gap, as ECMA-262 says.
//!
//! **One native for most methods.** A method is a native whose slot 1 is an index into [`OPS`]; [`date_method`] reads it from the callee
//! and does one `match`. So a method costs one native cell, with no code of its own.
//!
//! **Known deviations from V8**
//! - The text in parentheses of `toString` is the long English name for the 13 zones of `bi_date_tz::long_name` and `GMT+hh:mm` for any
//!   other zone (V8 writes the ICU name of every zone). The old engine does the same.
//! - `toLocaleString`, `toLocaleDateString` and `toLocaleTimeString` write the `en-US` form (`1/2/2020, 3:04:05 PM`, with a plain space
//!   before `AM`) in the local zone, and ignore the locale and the options: js2 has no `Intl` yet. The old engine forwards to `Intl`.
//! - `Date.now` and `new Date()` read the system clock (no virtual clock).
use crate::lazy::{Ent, indexed, m, same};
use crate::bi_date_calc::{self as calc, Fields, LocalText, Parsed};
use crate::bi_date_tz::{self as tz, Zone};
use crate::bi_symbol::TO_PRIMITIVE;
use crate::builtins::{Hint, arg};
use crate::cell::Layout;
use crate::interp_rt::{ErrKind, Key, R, Realm};
use crate::object::Obj;
use crate::shapes::A_C;
use crate::value::Value;
use std::cell::RefCell;
use std::rc::Rc;

/// Object header flag bit 19: a `Date` object (bits 0 to 18 are taken, `object.rs`).
pub(crate) const F_DATE: u32 = 1 << 19;

/// What a prototype method does.
#[derive(Clone, Copy)]
enum Op {
    /// `getTime`, `valueOf`
    Time,
    SetTime,
    /// A field of the time (index in `Fields` order: year, month, date, day, hours, minutes, seconds, milliseconds); true for UTC.
    Get(u8, bool),
    GetYear,
    Offset,
    /// A setter: 0 milliseconds, 1 seconds, 2 minutes, 3 hours, 4 date, 5 month, 6 full year; true for UTC.
    Set(u8, bool),
    SetYear,
    Iso,
    Json,
    Utc,
    Text(LocalText),
    /// `toLocaleString` (0), `toLocaleDateString` (1), `toLocaleTimeString` (2).
    Locale(u8),
}

use Op::*;

const OPS: &[(&str, u8, Op)] = &[
    ("getTime", 0, Time),
    ("valueOf", 0, Time),
    ("setTime", 1, SetTime),
    ("toISOString", 0, Iso),
    ("toJSON", 1, Json),
    ("toUTCString", 0, Utc),
    ("getUTCFullYear", 0, Get(0, true)),
    ("getUTCMonth", 0, Get(1, true)),
    ("getUTCDate", 0, Get(2, true)),
    ("getUTCDay", 0, Get(3, true)),
    ("getUTCHours", 0, Get(4, true)),
    ("getUTCMinutes", 0, Get(5, true)),
    ("getUTCSeconds", 0, Get(6, true)),
    ("getUTCMilliseconds", 0, Get(7, true)),
    ("setUTCMilliseconds", 1, Set(0, true)),
    ("setUTCSeconds", 2, Set(1, true)),
    ("setUTCMinutes", 3, Set(2, true)),
    ("setUTCHours", 4, Set(3, true)),
    ("setUTCDate", 1, Set(4, true)),
    ("setUTCMonth", 2, Set(5, true)),
    ("setUTCFullYear", 3, Set(6, true)),
    ("getFullYear", 0, Get(0, false)),
    ("getMonth", 0, Get(1, false)),
    ("getDate", 0, Get(2, false)),
    ("getDay", 0, Get(3, false)),
    ("getHours", 0, Get(4, false)),
    ("getMinutes", 0, Get(5, false)),
    ("getSeconds", 0, Get(6, false)),
    ("getMilliseconds", 0, Get(7, false)),
    ("getTimezoneOffset", 0, Offset),
    ("getYear", 0, GetYear),
    ("setMilliseconds", 1, Set(0, false)),
    ("setSeconds", 2, Set(1, false)),
    ("setMinutes", 3, Set(2, false)),
    ("setHours", 4, Set(3, false)),
    ("setDate", 1, Set(4, false)),
    ("setMonth", 2, Set(5, false)),
    ("setFullYear", 3, Set(6, false)),
    ("setYear", 1, SetYear),
    ("toString", 0, Text(LocalText::Full)),
    ("toDateString", 0, Text(LocalText::Date)),
    ("toTimeString", 0, Text(LocalText::Time)),
    ("toLocaleString", 0, Locale(0)),
    ("toLocaleDateString", 0, Locale(1)),
    ("toLocaleTimeString", 0, Locale(2)),
];

// ---- the local zone ----

/// A zone and the name it was asked for.
struct Local {
    name: String,
    zone: Zone,
}

thread_local! {
    static LOCAL: RefCell<Option<Rc<Local>>> = const { RefCell::new(None) };
}

/// A zone by name: the UTC names, an offset, or a zone of `bare-tzdata`. `None` when it is not a zone.
fn zone_named(name: &str) -> Option<Local> {
    if tz::is_utc_name(name) {
        return Some(Local { name: "UTC".into(), zone: Zone::fixed(0) });
    }
    if let Some(off) = tz::parse_offset_name(name) {
        let a = off.abs();
        return Some(Local { name: format!("{}{:02}:{:02}", if off < 0 { '-' } else { '+' }, a / 3600, a / 60 % 60), zone: Zone::fixed(off) });
    }
    Some(Local { name: name.into(), zone: Zone::parse(bare_tzdata::find(name)?)? })
}

/// The local zone of this thread (see the module text).
fn local() -> Rc<Local> {
    LOCAL.with(|c| {
        c.borrow_mut()
            .get_or_insert_with(|| {
                let from_tz = std::env::var("TZ").ok().map(|s| s.trim_start_matches(':').to_string());
                let z = [from_tz, bare_os::tz::local_zone()].into_iter().flatten().find_map(|n| zone_named(&n));
                Rc::new(z.unwrap_or(Local { name: "UTC".into(), zone: Zone::fixed(0) }))
            })
            .clone()
    })
}

/// Set the local zone of this thread (for tests). `false` when `name` is not a zone.
#[cfg(test)]
pub(crate) fn set_local_zone(name: &str) -> bool {
    let Some(z) = zone_named(name) else { return false };
    LOCAL.with(|c| *c.borrow_mut() = Some(Rc::new(z)));
    true
}

/// The offset of the local zone (seconds east of UTC) at the valid time value `t`.
fn offset_at(z: &Local, t: f64) -> i32 {
    z.zone.offset((t / 1000.0).floor() as i64)
}

/// Wall-clock time of the time value `t` (NaN stays NaN).
fn to_local(t: f64) -> f64 {
    if !t.is_finite() {
        return f64::NAN;
    }
    t + offset_at(&local(), t) as f64 * 1000.0
}

/// The time value of wall-clock time `l` (`UTC(t)` of ECMA-262; NaN stays NaN). The caller clips.
fn from_local(l: f64) -> f64 {
    if !l.is_finite() || l.abs() > calc::MAX_TIME + 172_800_000.0 {
        return f64::NAN;
    }
    let secs = (l / 1000.0).floor() as i64;
    l + (local().zone.to_utc(secs) - secs) as f64 * 1000.0
}

// ---- the object ----

/// A new `Date` with prototype `proto` and the clipped time value `t`.
fn new_date(rt: &mut Realm, proto: Obj, t: f64) -> Value {
    let root = rt.shapes.root(Layout::Object, Some(proto.cell()), 4);
    let shape = rt.shapes.add(root, rt.k_prim, 0).expect("shape");
    let o = Obj::new(&mut rt.heap, &rt.shapes, shape).expect("memory");
    o.set_slot(&rt.shapes, 0, Value::number(t));
    o.set_flag(F_DATE);
    o.value()
}

/// The object and the slot of the time value of a `Date`, or `None` for any other value.
pub(crate) fn date_slot(rt: &Realm, v: Value) -> Option<(Obj, u32)> {
    let o = Realm::obj(v).filter(|o| o.has_flag(F_DATE))?;
    Some((o, rt.shapes.find(o.shape(&rt.shapes), rt.k_prim)?.slot()))
}

fn this_date(rt: &mut Realm, this: Value) -> R<(Obj, u32, f64)> {
    match date_slot(rt, this) {
        Some((o, s)) => Ok((o, s, o.slot(&rt.shapes, s).as_f64().unwrap_or(f64::NAN))),
        None => rt.type_error("this is not a Date object."),
    }
}

fn put(rt: &mut Realm, o: Obj, slot: u32, t: f64) -> Value {
    o.set_slot(&rt.shapes, slot, Value::number(t));
    Realm::num(t)
}

fn text_value(rt: &mut Realm, s: Option<String>) -> Value {
    rt.string(s.as_deref().unwrap_or("Invalid Date"))
}

fn local_text(rt: &mut Realm, t: f64, part: LocalText) -> Value {
    if t.is_nan() {
        return text_value(rt, None);
    }
    let z = local();
    let off = offset_at(&z, t);
    text_value(rt, calc::to_local_string(t, off, &tz::long_name(&z.name, off), part))
}

/// The `en-US` text for the `toLocale*String` methods (see the module text).
fn locale_text(t: f64, kind: u8) -> Option<String> {
    let f = calc::fields(to_local(t))?;
    let date = format!("{}/{}/{}", f.month + 1, f.day, f.year);
    let h = if f.hour % 12 == 0 { 12 } else { f.hour % 12 };
    let time = format!("{h}:{:02}:{:02} {}", f.minute, f.second, if f.hour < 12 { "AM" } else { "PM" });
    Some(match kind {
        0 => format!("{date}, {time}"),
        1 => date,
        _ => time,
    })
}

fn field(f: &Fields, k: u8) -> i64 {
    [f.year, f.month, f.day, f.weekday, f.hour, f.minute, f.second, f.ms][k as usize]
}

/// Parse a date string to a time value.
fn parse_str(rt: &mut Realm, s: crate::string::Str) -> f64 {
    let u = rt.units(s);
    match calc::parse(&u) {
        Parsed::Time(t) => t,
        Parsed::Local(l) => calc::time_clip(from_local(l)),
    }
}

/// The 7 arguments of `Date.UTC` and of `new Date(y, m, ...)`: MakeDate of them, coerced in order.
fn components(rt: &mut Realm, args: &[Value]) -> R<f64> {
    let mut n = [f64::NAN, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0];
    for (i, slot) in n.iter_mut().enumerate() {
        if i == 0 || i < args.len() {
            *slot = rt.to_number(arg(args, i))?;
        }
    }
    let y = if n[0].is_nan() { n[0] } else if (0.0..=99.0).contains(&n[0].trunc()) { 1900.0 + n[0].trunc() } else { n[0] };
    Ok(calc::make_date(calc::make_day(y, n[1], n[2]), calc::make_time(n[3], n[4], n[5], n[6])))
}

// ---- natives ----

fn date_ctor(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    // A conversion below may call a script, so read these first.
    let (callee, nt) = (rt.callee, rt.nt);
    if nt.is_undefined() {
        return Ok(local_text(rt, calc::time_clip(rt.clock_now_ms()), LocalText::Full));
    }
    let t = match args.len() {
        0 => calc::time_clip(rt.clock_now_ms()),
        1 => match date_slot(rt, args[0]) {
            Some((o, s)) => o.slot(&rt.shapes, s).as_f64().unwrap_or(f64::NAN),
            None => {
                let p = rt.to_primitive(args[0], Hint::Default)?;
                match Realm::str_of(p) {
                    Some(s) => parse_str(rt, s),
                    None => calc::time_clip(rt.to_number(p)?),
                }
            }
        },
        _ => {
            let l = components(rt, args)?;
            calc::time_clip(from_local(l))
        }
    };
    // `new.target.prototype`, else the `Date.prototype` of the constructor itself.
    let f = if Realm::obj(nt).is_some() { nt } else { callee };
    let p = rt.get_prop(f, Key::Name(rt.k_prototype))?;
    let p = match Realm::obj(p) {
        Some(p) => p,
        None => Realm::obj(rt.get_prop(callee, Key::Name(rt.k_prototype))?).expect("Date.prototype is an object"),
    };
    Ok(new_date(rt, p, t))
}

fn date_now(rt: &mut Realm, _this: Value, _args: &[Value]) -> R<Value> {
    Ok(Realm::num(calc::time_clip(rt.clock_now_ms())))
}

fn date_parse(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let s = rt.to_str(arg(args, 0))?;
    Ok(Realm::num(parse_str(rt, s)))
}

fn date_utc(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let t = components(rt, args)?;
    Ok(Realm::num(calc::time_clip(t)))
}

/// `Date.prototype[Symbol.toPrimitive](hint)`.
fn date_to_prim(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let Some(o) = Realm::obj(this) else { return rt.type_error("Date.prototype[Symbol.toPrimitive] called on a non-object") };
    let hint = Realm::str_of(arg(args, 0)).map(|s| rt.text(s));
    let order = match hint.as_deref() {
        Some("string" | "default") => [rt.k_to_string, rt.k_value_of],
        Some("number") => [rt.k_value_of, rt.k_to_string],
        _ => return rt.type_error("invalid hint"),
    };
    rt.ordinary_to_primitive(this, o, order)
}

/// Every other method of `Date.prototype`: the index of the row of [`OPS`] is in slot 1 of the callee.
fn date_method(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let op = OPS[Realm::obj(rt.callee).and_then(|c| c.slot(&rt.shapes, 1).as_int()).expect("a Date method") as usize].2;
    if let Json = op {
        return to_json(rt, this);
    }
    let (o, slot, t) = this_date(rt, this)?;
    match op {
        Time => Ok(Realm::num(t)),
        SetTime => {
            let t = calc::time_clip(rt.to_number(arg(args, 0))?);
            Ok(put(rt, o, slot, t))
        }
        Get(k, utc) => {
            let t = if utc { t } else { to_local(t) };
            Ok(Realm::num(calc::fields(t).map_or(f64::NAN, |f| field(&f, k) as f64)))
        }
        GetYear => Ok(Realm::num(calc::fields(to_local(t)).map_or(f64::NAN, |f| (f.year - 1900) as f64))),
        // whole minutes, cut toward zero, as V8 does for a zone with seconds in its offset
        Offset => Ok(Realm::num(if t.is_nan() { f64::NAN } else { -(offset_at(&local(), t) / 60) as f64 })),
        Set(i, utc) => set(rt, (o, slot, t), i, !utc, args),
        SetYear => {
            let y = rt.to_number(arg(args, 0))?;
            if y.is_nan() {
                return Ok(put(rt, o, slot, f64::NAN));
            }
            let yi = y.trunc();
            set(rt, (o, slot, t), 6, true, &[Realm::num(if (0.0..=99.0).contains(&yi) { 1900.0 + yi } else { y })])
        }
        Iso => match calc::to_iso(t) {
            Some(s) => Ok(rt.string(&s)),
            None => Err(rt.error(ErrKind::Range, "Invalid time value")),
        },
        Utc => Ok(text_value(rt, calc::to_utc_string(t))),
        Text(part) => Ok(local_text(rt, t, part)),
        Locale(kind) => Ok(text_value(rt, locale_text(t, kind))),
        Json => unreachable!("handled above"),
    }
}

/// `Date.prototype.toJSON(key)`: generic. A non-finite number gives `null`, else `toISOString` of the receiver.
fn to_json(rt: &mut Realm, this: Value) -> R<Value> {
    let o = rt.to_object(this)?.value();
    if let Some(n) = rt.to_primitive(o, Hint::Number)?.as_f64()
        && !n.is_finite()
    {
        return Ok(Value::NULL);
    }
    let m = rt.get_named(o, "toISOString")?;
    if !Realm::obj(m).is_some_and(|f| f.is_callable()) {
        return rt.type_error("toISOString is not a function");
    }
    rt.call_function(m, o, &[])
}

/// A setter (section 21.4.4.20 and its siblings): `local` is for the wall-clock form. The time value was read before the arguments are
/// coerced, in order, all of them; an invalid date stays invalid (except for `setFullYear`, which starts from +0).
fn set(rt: &mut Realm, (o, slot, t): (Obj, u32, f64), i: u8, local: bool, args: &[Value]) -> R<Value> {
    let max = [1, 2, 3, 4, 1, 2, 3][i as usize];
    let mut n = [f64::NAN; 4];
    for (k, v) in n.iter_mut().enumerate().take(max) {
        if k == 0 || k < args.len() {
            *v = rt.to_number(arg(args, k))?;
        }
    }
    let t = if t.is_nan() && i == 6 { 0.0 } else if local { to_local(t) } else { t };
    // An invalid date stays as it is, even when an argument changed it while it was coerced.
    let Some(f) = calc::fields(t) else { return Ok(Value::number(f64::NAN)) };
    let given = |k: usize, default: i64| if k < args.len() { n[k] } else { default as f64 };
    let (day, year, month) = (calc::day(t), f.year as f64, f.month as f64);
    let (h, mi, s) = (f.hour as f64, f.minute as f64, f.second as f64);
    let v = match i {
        0 => calc::make_date(day, calc::make_time(h, mi, s, n[0])),
        1 => calc::make_date(day, calc::make_time(h, mi, n[0], given(1, f.ms))),
        2 => calc::make_date(day, calc::make_time(h, n[0], given(1, f.second), given(2, f.ms))),
        3 => calc::make_date(day, calc::make_time(n[0], given(1, f.minute), given(2, f.second), given(3, f.ms))),
        4 => calc::make_date(calc::make_day(year, month, n[0]), calc::time_within_day(t)),
        5 => calc::make_date(calc::make_day(year, n[0], given(1, f.day)), calc::time_within_day(t)),
        _ => calc::make_date(calc::make_day(n[0], given(1, f.month), given(2, f.day)), calc::time_within_day(t)),
    };
    let v = calc::time_clip(if local { from_local(v) } else { v });
    Ok(put(rt, o, slot, v))
}

/// The table of `Date.prototype`: a row of [`OPS`] each, and `toGMTString`.
const fn date_table() -> [Ent; OPS.len() + 1] {
    let mut a = [same("toGMTString", "toUTCString"); OPS.len() + 1];
    let mut i = 0;
    while i < OPS.len() {
        a[i] = indexed(OPS[i].0, OPS[i].1, date_method, i as i32);
        i += 1;
    }
    a
}

impl Realm {
    pub(crate) fn install_date(&mut self) {
        let proto = Obj::new(&mut self.heap, &self.shapes, self.s_object).expect("memory");
        let c = self.ctor("Date", 7, date_ctor, proto);
        static DATE_STATICS: &[Ent] = &[m("now", 0, date_now), m("parse", 1, date_parse), m("UTC", 7, date_utc)];
        self.lazy_table(c, DATE_STATICS);
        // `toGMTString` is the same function object as `toUTCString`.
        static DATE_PROTO: [Ent; OPS.len() + 1] = date_table();
        self.lazy_table(proto, &DATE_PROTO);
        let tp = self.new_native(date_to_prim, false, 1, "[Symbol.toPrimitive]");
        self.define_attrs(proto, Key::Name(self.wk[TO_PRIMITIVE]), A_C, tp.value()).expect("define");
    }
}

#[cfg(test)]
#[path = "bi_date_tests.rs"]
mod tests;
