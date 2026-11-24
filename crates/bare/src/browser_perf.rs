//! The user timing part of `performance` (Performance Timeline, User Timing): `mark`, `measure`, `getEntries`, `getEntriesByType`,
//! `getEntriesByName`, `clearMarks` and `clearMeasures`.
//!
//! Cause: Bing, GitLab and other pages call `performance.getEntriesByType('navigation')` or `performance.mark(...)` at load. A missing
//! method threw `TypeError` and stopped the script.
//!
//! - `mark` and `measure` entries exist, and one `navigation` entry per document. `resource`, `paint` and every other type give an empty array.
//! - The timeline keeps the object that `mark` or `measure` returns (User Timing L3), as Chrome and Firefox do. A read gives the same
//!   objects and makes no new ones. `detail` is a structured clone of the `detail` option, made once. The list roots the kept objects.
//! - The interfaces `PerformanceEntry`, `PerformanceResourceTiming`, `PerformanceNavigationTiming`, `PerformanceMark` and `PerformanceMeasure`
//!   are lazy globals (`install_interfaces`). Each is an illegal constructor. A mark or a measure is an instance of its interface, and
//!   `toJSON` gives the plain copy. A resource entry is never made. Gap: `new PerformanceMark(name)` throws.
//! - The navigation entry is made on each read. Its times are the ones the loader stamps in `LoadTimes`. A phase not reached is `0`.
//!   A document without a URL has no navigation entry.
//! - A `measure` argument that names a mark uses the last mark of that name. `navigationStart` is 0. An unknown name is a `SyntaxError`.
//! - After `MAX_ENTRIES` entries, a new one is returned but not kept.
//! - The methods and legacy navigation accessors are installed with the scheduler. The entry list is empty until the first `mark`.

use crate::eng::{Eng, Ops};
use crate::browser::*;
use crate::browser_geom::Iface;
use crate::browser_sources::Roots;
use crate::browser_scoped::*;
use bare_js_api::{Args, Attrs, HostFn, Local, Type};

const MAX_ENTRIES: usize = 50_000;

/// The interfaces of the timeline. Made once by `ensure`. Their prototypes and constructors are in `Dom::fixed` (`browser_geom::make`).
#[derive(Clone, Copy)]
pub(crate) struct Ifaces<E: Eng> {
    entry: Iface<E>,
    resource: Iface<E>,
    navigation: Iface<E>,
    mark: Iface<E>,
    measure: Iface<E>,
}

fn ensure<E: Eng>(d: &Dom<E>, it: &mut E::Vm) -> R<E, Ifaces<E>> {
    if let Some(i) = d.perf_ifaces.get() {
        return Ok(i);
    }
    let entry = crate::browser_geom::make_s(d, it, it.object_prototype(), "PerformanceEntry", crate::browser::illegal_constructor, HostFn::Ctor)?;
    let sub = |it: &mut E::Vm, name: &str| crate::browser_geom::make_s(d, it, entry.proto, name, crate::browser::illegal_constructor, HostFn::Ctor);
    let (resource, navigation, mark, measure) = (sub(it, "PerformanceResourceTiming")?, sub(it, "PerformanceNavigationTiming")?, sub(it, "PerformanceMark")?, sub(it, "PerformanceMeasure")?);
    crate::browser_geom::finish_s(d, it, entry, "PerformanceEntry", &[("toJSON", 0, entry_json as HS<E>)])?;
    crate::browser_geom::finish_s(d, it, resource, "PerformanceResourceTiming", &[])?;
    crate::browser_geom::finish_s(d, it, navigation, "PerformanceNavigationTiming", &[])?;
    crate::browser_geom::finish_s(d, it, mark, "PerformanceMark", &[("toJSON", 0, detail_json as HS<E>)])?;
    crate::browser_geom::finish_s(d, it, measure, "PerformanceMeasure", &[("toJSON", 0, detail_json as HS<E>)])?;
    let i = Ifaces { entry, resource, navigation, mark, measure };
    d.perf_ifaces.set(Some(i));
    Ok(i)
}

crate::browser_lazy::lazy_global!(lazy_entry, "PerformanceEntry", ensure_unit, |d| d.perf_ifaces.get().expect("made").entry.ctor);
crate::browser_lazy::lazy_global!(lazy_resource, "PerformanceResourceTiming", ensure_unit, |d| d.perf_ifaces.get().expect("made").resource.ctor);
crate::browser_lazy::lazy_global!(lazy_navigation, "PerformanceNavigationTiming", ensure_unit, |d| d.perf_ifaces.get().expect("made").navigation.ctor);
crate::browser_lazy::lazy_global!(lazy_mark, "PerformanceMark", ensure_unit, |d| d.perf_ifaces.get().expect("made").mark.ctor);
crate::browser_lazy::lazy_global!(lazy_measure, "PerformanceMeasure", ensure_unit, |d| d.perf_ifaces.get().expect("made").measure.ctor);

fn ensure_unit<E: Eng>(d: &Dom<E>, it: &mut E::Vm) -> R<E, ()> {
    ensure(d, it).map(|_| ())
}

pub(crate) fn install_interfaces<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>) -> Res<'s, ()> {
    table!(lazy = [
        Row::both(1, "PerformanceEntry", lazy_entry),
        Row::both(1, "PerformanceResourceTiming", lazy_resource),
        Row::both(1, "PerformanceNavigationTiming", lazy_navigation),
        Row::both(1, "PerformanceMark", lazy_mark),
        Row::both(1, "PerformanceMeasure", lazy_measure),
    ]);
    let g = s.global();
    d.install_rows(s, &[g], &lazy::T::<E>::V)
}

/// `PerformanceEntry.prototype.toJSON`: the four members of an entry.
fn entry_json<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    json(d, s, this, false)
}

/// `toJSON` of a mark and a measure: the four members and `detail`.
fn detail_json<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    json(d, s, this, true)
}

fn json<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, detail: bool) -> Res<'s, Local<'s>> {
    if !is_object::<E>(s, this) {
        return Err(crate::browser_geom::illegal_s::<E>(s));
    }
    let proto = s.engine().object_prototype();
    let o = d.obj_with_s(s, proto)?;
    let names: &[&str] = if detail { &["name", "entryType", "startTime", "duration", "detail"] } else { &["name", "entryType", "startTime", "duration"] };
    for &name in names {
        let v = s.get_str(this, name)?;
        s.define_str(o, name, v, Attrs::ALL)?;
    }
    Ok(o)
}

/// One kept entry. `obj` is its `PerformanceMark` or `PerformanceMeasure`; the other fields serve the lookups without a property read.
pub(crate) struct Entry<O> {
    name: String,
    measure: bool,
    start: f64,
    obj: O,
}

pub(crate) fn roots<E: Eng>(d: &Dom<E>, out: &mut Roots<'_, '_, E>) {
    out.extend(d.perf_entries.borrow().iter().map(|e| E::obj_raw(e.obj)));
}

/// The times of the load of one document, in ms since the origin. The loader stamps them (`browser_loader.rs`). `0` is a phase not reached.
#[derive(Clone, Copy, Default)]
pub(crate) struct LoadTimes {
    pub(crate) interactive: f64,
    pub(crate) dcl_start: f64,
    pub(crate) dcl_end: f64,
    pub(crate) complete: f64,
    pub(crate) load_start: f64,
    pub(crate) load_end: f64,
}

/// A new plain object with the prototype `proto` and the data properties `props`, in this order.
fn object<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, proto: E::Obj, props: &[(&str, Local<'s>)]) -> Res<'s, Local<'s>> {
    let o = d.obj_s(s, proto)?;
    let l = d.obj_local(s, o);
    for &(k, v) in props {
        s.define_str(l, k, v, Attrs::ALL)?;
    }
    Ok(l)
}

/// The navigation entry of the document `url`. Each read makes a new object. Its times are those of `LoadTimes`.
fn nav_entry<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, url: &str) -> Res<'s, Local<'s>> {
    let i = old::<E, _>(s, |it| ensure(d, it))?;
    let t = *d.load_times.borrow();
    let (name, ty, nav) = (d.js_str_s(s, url)?, d.js_str_s(s, "navigation")?, d.js_str_s(s, "navigate")?);
    let n = |s: &mut Sc<'s, E>, v: f64| s.number(v);
    let props = [
        ("name", name),
        ("entryType", ty),
        ("startTime", n(s, 0.0)),
        ("duration", n(s, t.load_end)),
        ("type", nav),
        ("domInteractive", n(s, t.interactive)),
        ("domContentLoadedEventStart", n(s, t.dcl_start)),
        ("domContentLoadedEventEnd", n(s, t.dcl_end)),
        ("domComplete", n(s, t.complete)),
        ("loadEventStart", n(s, t.load_start)),
        ("loadEventEnd", n(s, t.load_end)),
    ];
    object(d, s, i.navigation.proto, &props)
}

fn need<'s, E: Eng>(s: &mut Sc<'s, E>, a: Args<'s>, what: &str) -> Res<'s, ()> {
    if a.len() == 0 {
        return Err(s.type_error(&format!("Failed to execute '{what}' on 'Performance': 1 argument required, but only 0 present.")));
    }
    Ok(())
}

fn present<E: Eng>(s: &Sc<'_, E>, v: Local<'_>) -> bool {
    s.type_of(v) != Type::Undefined
}

/// Make the object of an entry (an instance of `PerformanceMark` or `PerformanceMeasure`) and keep it in the timeline.
/// `detail` is the option value; the entry gets a structured clone of it, or `null` when it is absent.
fn make<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, name: String, measure: bool, start: f64, dur: f64, detail: Local<'s>) -> Res<'s, Local<'s>> {
    let detail = if present::<E>(s, detail) { crate::browser_navigation_state::clone_with(d, s, detail, &[])?.0 } else { s.null() };
    let i = old::<E, _>(s, |it| ensure(d, it))?;
    let (n, ty) = (d.js_str_s(s, &name)?, d.js_str_s(s, if measure { "measure" } else { "mark" })?);
    let (st, du) = (s.number(start), s.number(dur));
    let obj = d.obj_s(s, if measure { i.measure.proto } else { i.mark.proto })?;
    let v = d.obj_local(s, obj);
    for (k, x) in [("name", n), ("entryType", ty), ("startTime", st), ("duration", du), ("detail", detail)] {
        s.define_str(v, k, x, Attrs::ALL)?;
    }
    let mut l = d.perf_entries.borrow_mut();
    if l.len() < MAX_ENTRIES {
        l.push(Entry { name, measure, start, obj });
    }
    Ok(v)
}

fn mark<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    need::<E>(s, a, "mark")?;
    let name = d.arg_str_s(s, a, 0)?;
    let (mut start, mut detail) = (d.now_ms(), s.undefined());
    let o = arg_s::<E>(s, a, 1);
    if is_object::<E>(s, o) {
        let v = s.get_str(o, "startTime")?;
        if present::<E>(s, v) {
            start = s.to_number(v)?;
            if !start.is_finite() || start < 0.0 {
                return Err(s.type_error("Failed to execute 'mark' on 'Performance': 'startTime' must be a non-negative finite number."));
            }
        }
        detail = s.get_str(o, "detail")?;
    }
    make(d, s, name, false, start, 0.0, detail)
}

/// A time of `measure`: a number, or the name of a mark. `None` for `undefined`.
fn time<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, v: Local<'s>) -> Res<'s, Option<f64>> {
    if !present::<E>(s, v) {
        return Ok(None);
    }
    if let Some(n) = s.as_number(v) {
        return Ok(Some(n));
    }
    let n = d.string_arg_s(s, v)?;
    if n == "navigationStart" {
        return Ok(Some(0.0));
    }
    let found = d.perf_entries.borrow().iter().rev().find(|e| !e.measure && e.name == n).map(|e| e.start);
    match found {
        Some(t) => Ok(Some(t)),
        None => Err(d.dom_err_s(s, "SyntaxError", &format!("Failed to execute 'measure' on 'Performance': The mark '{n}' does not exist."))),
    }
}

fn measure<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    need::<E>(s, a, "measure")?;
    let name = d.arg_str_s(s, a, 0)?;
    let (start, end);
    let (mut dur, mut detail) = (None, s.undefined());
    let (a1, a2) = (arg_s::<E>(s, a, 1), arg_s::<E>(s, a, 2));
    if is_object::<E>(s, a1) {
        let (st, e, du, de) = (s.get_str(a1, "start")?, s.get_str(a1, "end")?, s.get_str(a1, "duration")?, s.get_str(a1, "detail")?);
        if [st, e, du, de].into_iter().any(|v| present::<E>(s, v)) {
            if present::<E>(s, a2) {
                return Err(s.type_error("Failed to execute 'measure' on 'Performance': The measureOptions and endMark cannot both be passed."));
            }
            let (ps, pe, pd) = (present::<E>(s, st), present::<E>(s, e), present::<E>(s, du));
            if (!ps && !pe) || (ps && pe && pd) {
                return Err(s.type_error("Failed to execute 'measure' on 'Performance': Invalid measure options."));
            }
        }
        let end_arg = if present::<E>(s, a2) { a2 } else { e };
        (start, end) = (time(d, s, st)?, time(d, s, end_arg)?);
        if present::<E>(s, du) {
            dur = Some(s.to_number(du)?);
        }
        detail = de;
    } else {
        (start, end) = (time(d, s, a1)?, time(d, s, a2)?);
    }
    let now = d.now_ms();
    let (st, e) = match (start, end, dur) {
        (Some(st), None, Some(du)) => (st, st + du),
        (None, Some(e), Some(du)) => (e - du, e),
        (st, e, _) => (st.unwrap_or(0.0), e.unwrap_or(now)),
    };
    make(d, s, name, true, st, e - st, detail)
}

fn list<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, ty: Option<&str>, name: Option<&str>) -> Res<'s, Local<'s>> {
    let mut out = Vec::new();
    if let Ok(rec) = d.cur_rec() {
        let url = rec.href();
        if ty.is_none_or(|t| t == "navigation") && name.is_none_or(|n| n == url) {
            out.push(nav_entry(d, s, &url)?);
        }
    }
    let mut idx: Vec<usize> = {
        let l = d.perf_entries.borrow();
        (0..l.len()).filter(|&i| ty.is_none_or(|t| t == if l[i].measure { "measure" } else { "mark" }) && name.is_none_or(|n| n == l[i].name)).collect()
    };
    s.charge(idx.len() as u64)?;
    idx.sort_by(|&x, &y| {
        let l = d.perf_entries.borrow();
        l[x].start.total_cmp(&l[y].start)
    });
    for i in idx {
        let o = d.perf_entries.borrow()[i].obj;
        out.push(d.obj_local(s, o));
    }
    s.new_array(&out)
}

fn get_entries<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    list(d, s, None, None)
}

fn by_type<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    need::<E>(s, a, "getEntriesByType")?;
    let t = d.arg_str_s(s, a, 0)?;
    if !matches!(t.as_str(), "mark" | "measure" | "navigation") {
        return s.new_array(&[]);
    }
    list(d, s, Some(&t), None)
}

fn by_name<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    need::<E>(s, a, "getEntriesByName")?;
    let n = d.arg_str_s(s, a, 0)?;
    let a1 = arg_s::<E>(s, a, 1);
    let t = if present::<E>(s, a1) { Some(d.string_arg_s(s, a1)?) } else { None };
    if t.as_deref().is_some_and(|t| !matches!(t, "mark" | "measure" | "navigation")) {
        return s.new_array(&[]);
    }
    list(d, s, t.as_deref(), Some(&n))
}

fn clear<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, a: Args<'s>, measure: bool) -> Res<'s, Local<'s>> {
    let a0 = arg_s::<E>(s, a, 0);
    let name = if present::<E>(s, a0) { Some(d.string_arg_s(s, a0)?) } else { None };
    d.perf_entries.borrow_mut().retain(|e| e.measure != measure || name.as_ref().is_some_and(|n| *n != e.name));
    Ok(s.undefined())
}

fn clear_marks<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    clear(d, s, a, false)
}

fn clear_measures<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    clear(d, s, a, true)
}

/// Define the methods on `perf`, the prototype of the `performance` object.
pub(crate) fn install<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, perf: E::Obj) -> Res<'s, ()> {
    for (name, len, h) in [
        ("mark", 1, mark as HS<E>),
        ("measure", 1, measure),
        ("getEntries", 0, get_entries),
        ("getEntriesByType", 1, by_type),
        ("getEntriesByName", 1, by_name),
        ("clearMarks", 0, clear_marks),
        ("clearMeasures", 0, clear_measures),
    ] {
        d.def_method_at(s, perf, name, len, h)?;
    }
    let po = d.obj_local(s, perf);
    for (name, h) in [("timing", timing as HS<E>), ("navigation", navigation as HS<E>), ("timeOrigin", time_origin as HS<E>)] {
        let f = d.add_fn_s(s, h, name, 0, HostFn::Method)?;
        s.define_accessor_str(po, name, Some(f), None, Attrs { writable: false, enumerable: true, configurable: true })?;
    }
    Ok(())
}

/// Legacy navigation data is synthesized on first access. It does not add realm state.
#[cold]
fn legacy<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, timing: bool) -> Res<'s, Local<'s>> {
    let proto = old::<E, _>(s, |it| Ok(it.object_prototype()))?;
    let o = d.obj_s(s, proto)?;
    old::<E, _>(s, |it| crate::browser_lazy::tag(d, it, o, if timing { "PerformanceTiming" } else { "PerformanceNavigation" }))?;
    let l = d.obj_local(s, o);
    let fixed = Attrs { writable: false, enumerable: true, configurable: true };
    if timing {
        let origin = (s.wall_now_ms() - d.now_ms()).max(1.0).floor();
        for name in ["navigationStart", "unloadEventStart", "unloadEventEnd", "redirectStart", "redirectEnd", "fetchStart", "domainLookupStart", "domainLookupEnd", "connectStart", "connectEnd", "secureConnectionStart", "requestStart", "responseStart", "responseEnd", "domLoading", "domInteractive", "domContentLoadedEventStart", "domContentLoadedEventEnd", "domComplete", "loadEventStart", "loadEventEnd"] {
            let zero = matches!(name, "unloadEventStart" | "unloadEventEnd" | "redirectStart" | "redirectEnd" | "secureConnectionStart");
            let v = s.number(if zero { 0.0 } else { origin });
            s.define_str(l, name, v, fixed)?;
        }
    } else {
        for (name, n) in [("type", 0.0), ("redirectCount", 0.0), ("TYPE_NAVIGATE", 0.0), ("TYPE_RELOAD", 1.0), ("TYPE_BACK_FORWARD", 2.0), ("TYPE_RESERVED", 255.0)] {
            let v = s.number(n);
            s.define_str(l, name, v, fixed)?;
        }
    }
    if is_object::<E>(s, this) {
        s.define_str(this, if timing { "timing" } else { "navigation" }, l, fixed)?;
    }
    Ok(l)
}
fn timing<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    legacy(d, s, this, true)
}
fn navigation<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    legacy(d, s, this, false)
}
/// The wall clock at time 0 of the scheduler clock, in ms. The first read fixes it on the owner, as `legacy` does.
fn time_origin<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let t = s.wall_now_ms() - d.now_ms();
    let v = s.number(t);
    if is_object::<E>(s, this) {
        let fixed = Attrs { writable: false, enumerable: true, configurable: true };
        s.define_str(this, "timeOrigin", v, fixed)?;
    }
    Ok(v)
}
