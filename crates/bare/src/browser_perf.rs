//! The user timing part of `performance` (Performance Timeline, User Timing): `mark`, `measure`, `getEntries`, `getEntriesByType`,
//! `getEntriesByName`, `clearMarks` and `clearMeasures`.
//!
//! Cause: Bing, GitLab and other pages call `performance.getEntriesByType('navigation')` or `performance.mark(...)` at load. A missing
//! method threw `TypeError` and stopped the script.
//!
//! - `mark` and `measure` entries exist, and one `navigation` entry per document. `resource`, `paint` and every other type give an empty array.
//! - An entry is plain Rust data. Each read makes new plain objects with `name`, `entryType`, `startTime`, `duration` and `detail`, so
//!   two reads of one entry are not `===`, and there is nothing to root. The object that `mark` or `measure` returns carries the `detail`
//!   of its options; a later read has `detail: null`.
//! - The interfaces `PerformanceEntry`, `PerformanceResourceTiming`, `PerformanceNavigationTiming`, `PerformanceMark` and `PerformanceMeasure`
//!   are lazy globals (`install_interfaces`). Each is an illegal constructor. A mark or a measure is an instance of its interface, and
//!   `toJSON` gives the plain copy. A resource entry is never made. Gap: `new PerformanceMark(name)` throws.
//! - The navigation entry is made on each read. Its times are the ones the loader stamps in `LoadTimes`. A phase not reached is `0`.
//!   A document without a URL has no navigation entry.
//! - A `measure` argument that names a mark uses the last mark of that name. `navigationStart` is 0. An unknown name is a `SyntaxError`.
//! - After `MAX_ENTRIES` entries, a new one is returned but not kept.
//! - The methods and legacy navigation accessors are installed with the scheduler. The entry list is empty until the first `mark`.

use crate::browser::*;
use crate::browser_geom::Iface;
use crate::browser_scoped::*;
use bare_js::eval::Interp;
use bare_js::value::ObjId;
use bare_js_api::{Args, Attrs, HostFn, Local, Type};

const MAX_ENTRIES: usize = 50_000;

/// The interfaces of the timeline. Made once by `ensure`. Their prototypes and constructors are in `Dom::fixed` (`browser_geom::make`).
#[derive(Clone, Copy)]
pub(crate) struct Ifaces {
    entry: Iface,
    resource: Iface,
    navigation: Iface,
    mark: Iface,
    measure: Iface,
}

fn ensure(d: &Dom, it: &mut Interp<'_>) -> R<Ifaces> {
    if let Some(i) = d.perf_ifaces.get() {
        return Ok(i);
    }
    let entry = crate::browser_geom::make_s(d, it, it.object_prototype(), "PerformanceEntry", crate::browser::illegal_constructor, HostFn::Ctor)?;
    let sub = |it: &mut Interp<'_>, name: &str| crate::browser_geom::make_s(d, it, entry.proto, name, crate::browser::illegal_constructor, HostFn::Ctor);
    let (resource, navigation, mark, measure) = (sub(it, "PerformanceResourceTiming")?, sub(it, "PerformanceNavigationTiming")?, sub(it, "PerformanceMark")?, sub(it, "PerformanceMeasure")?);
    crate::browser_geom::finish_s(d, it, entry, "PerformanceEntry", &[("toJSON", 0, entry_json as HS)])?;
    crate::browser_geom::finish_s(d, it, resource, "PerformanceResourceTiming", &[])?;
    crate::browser_geom::finish_s(d, it, navigation, "PerformanceNavigationTiming", &[])?;
    crate::browser_geom::finish_s(d, it, mark, "PerformanceMark", &[("toJSON", 0, detail_json as HS)])?;
    crate::browser_geom::finish_s(d, it, measure, "PerformanceMeasure", &[("toJSON", 0, detail_json as HS)])?;
    let i = Ifaces { entry, resource, navigation, mark, measure };
    d.perf_ifaces.set(Some(i));
    Ok(i)
}

crate::browser_lazy::lazy_global!(lazy_entry, "PerformanceEntry", ensure_unit, |d| d.perf_ifaces.get().expect("made").entry.ctor);
crate::browser_lazy::lazy_global!(lazy_resource, "PerformanceResourceTiming", ensure_unit, |d| d.perf_ifaces.get().expect("made").resource.ctor);
crate::browser_lazy::lazy_global!(lazy_navigation, "PerformanceNavigationTiming", ensure_unit, |d| d.perf_ifaces.get().expect("made").navigation.ctor);
crate::browser_lazy::lazy_global!(lazy_mark, "PerformanceMark", ensure_unit, |d| d.perf_ifaces.get().expect("made").mark.ctor);
crate::browser_lazy::lazy_global!(lazy_measure, "PerformanceMeasure", ensure_unit, |d| d.perf_ifaces.get().expect("made").measure.ctor);

fn ensure_unit(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    ensure(d, it).map(|_| ())
}

pub(crate) fn install_interfaces(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    crate::browser_lazy::install(d, it, &[("PerformanceEntry", lazy_entry as HS), ("PerformanceResourceTiming", lazy_resource), ("PerformanceNavigationTiming", lazy_navigation), ("PerformanceMark", lazy_mark), ("PerformanceMeasure", lazy_measure)])
}

/// `PerformanceEntry.prototype.toJSON`: the four members of an entry.
fn entry_json<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    json(d, s, this, false)
}

/// `toJSON` of a mark and a measure: the four members and `detail`.
fn detail_json<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    json(d, s, this, true)
}

fn json<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, detail: bool) -> Res<'s, Local<'s>> {
    if !is_object(s, this) {
        return Err(crate::browser_geom::illegal_s(s));
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

pub(crate) struct Entry {
    name: String,
    measure: bool,
    start: f64,
    dur: f64,
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
fn object<'s>(d: &Dom, s: &mut Sc<'s, '_>, proto: ObjId, props: &[(&str, Local<'s>)]) -> Res<'s, Local<'s>> {
    let o = d.obj_s(s, proto)?;
    let l = d.obj_local(s, o);
    for &(k, v) in props {
        s.define_str(l, k, v, Attrs::ALL)?;
    }
    Ok(l)
}

/// The navigation entry of the document `url`. Each read makes a new object. Its times are those of `LoadTimes`.
fn nav_entry<'s>(d: &Dom, s: &mut Sc<'s, '_>, url: &str) -> Res<'s, Local<'s>> {
    let i = old(s, |it| ensure(d, it))?;
    let t = *d.load_times.borrow();
    let (name, ty, nav) = (d.js_str_s(s, url)?, d.js_str_s(s, "navigation")?, d.js_str_s(s, "navigate")?);
    let n = |s: &mut Sc<'s, '_>, v: f64| s.number(v);
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

fn need<'s>(s: &mut Sc<'s, '_>, a: Args<'s>, what: &str) -> Res<'s, ()> {
    if a.len() == 0 {
        return Err(s.type_error(&format!("Failed to execute '{what}' on 'Performance': 1 argument required, but only 0 present.")));
    }
    Ok(())
}

fn present(s: &Sc<'_, '_>, v: Local<'_>) -> bool {
    s.type_of(v) != Type::Undefined
}

/// The object of an entry: an instance of `PerformanceMark` or `PerformanceMeasure`.
fn make<'s>(d: &Dom, s: &mut Sc<'s, '_>, e: &Entry, detail: Local<'s>) -> Res<'s, Local<'s>> {
    let i = old(s, |it| ensure(d, it))?;
    let (name, ty) = (d.js_str_s(s, &e.name)?, d.js_str_s(s, if e.measure { "measure" } else { "mark" })?);
    let (start, dur) = (s.number(e.start), s.number(e.dur));
    object(d, s, if e.measure { i.measure.proto } else { i.mark.proto }, &[("name", name), ("entryType", ty), ("startTime", start), ("duration", dur), ("detail", detail)])
}

fn keep(d: &Dom, e: Entry) {
    let mut l = d.perf_entries.borrow_mut();
    if l.len() < MAX_ENTRIES {
        l.push(e);
    }
}

fn mark<'s>(d: &Dom, s: &mut Sc<'s, '_>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    need(s, a, "mark")?;
    let name = d.arg_str_s(s, a, 0)?;
    let (mut start, mut detail) = (d.now_ms(), s.null());
    let o = arg_s(s, a, 1);
    if is_object(s, o) {
        let v = s.get_str(o, "startTime")?;
        if present(s, v) {
            start = s.to_number(v)?;
            if !start.is_finite() || start < 0.0 {
                return Err(s.type_error("Failed to execute 'mark' on 'Performance': 'startTime' must be a non-negative finite number."));
            }
        }
        let v = s.get_str(o, "detail")?;
        if present(s, v) {
            detail = v;
        }
    }
    let e = Entry { name, measure: false, start, dur: 0.0 };
    let v = make(d, s, &e, detail)?;
    keep(d, e);
    Ok(v)
}

/// A time of `measure`: a number, or the name of a mark. `None` for `undefined`.
fn time<'s>(d: &Dom, s: &mut Sc<'s, '_>, v: Local<'s>) -> Res<'s, Option<f64>> {
    if !present(s, v) {
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

fn measure<'s>(d: &Dom, s: &mut Sc<'s, '_>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    need(s, a, "measure")?;
    let name = d.arg_str_s(s, a, 0)?;
    let (start, end);
    let (mut dur, mut detail) = (None, s.null());
    let (a1, a2) = (arg_s(s, a, 1), arg_s(s, a, 2));
    if is_object(s, a1) {
        let (st, e, du, de) = (s.get_str(a1, "start")?, s.get_str(a1, "end")?, s.get_str(a1, "duration")?, s.get_str(a1, "detail")?);
        if [st, e, du, de].into_iter().any(|v| present(s, v)) {
            if present(s, a2) {
                return Err(s.type_error("Failed to execute 'measure' on 'Performance': The measureOptions and endMark cannot both be passed."));
            }
            let (ps, pe, pd) = (present(s, st), present(s, e), present(s, du));
            if (!ps && !pe) || (ps && pe && pd) {
                return Err(s.type_error("Failed to execute 'measure' on 'Performance': Invalid measure options."));
            }
        }
        let end_arg = if present(s, a2) { a2 } else { e };
        (start, end) = (time(d, s, st)?, time(d, s, end_arg)?);
        if present(s, du) {
            dur = Some(s.to_number(du)?);
        }
        if present(s, de) {
            detail = de;
        }
    } else {
        (start, end) = (time(d, s, a1)?, time(d, s, a2)?);
    }
    let now = d.now_ms();
    let (st, e) = match (start, end, dur) {
        (Some(st), None, Some(du)) => (st, st + du),
        (None, Some(e), Some(du)) => (e - du, e),
        (st, e, _) => (st.unwrap_or(0.0), e.unwrap_or(now)),
    };
    let en = Entry { name, measure: true, start: st, dur: e - st };
    let v = make(d, s, &en, detail)?;
    keep(d, en);
    Ok(v)
}

fn list<'s>(d: &Dom, s: &mut Sc<'s, '_>, ty: Option<&str>, name: Option<&str>) -> Res<'s, Local<'s>> {
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
        let e = {
            let l = d.perf_entries.borrow();
            Entry { name: l[i].name.clone(), measure: l[i].measure, start: l[i].start, dur: l[i].dur }
        };
        let null = s.null();
        out.push(make(d, s, &e, null)?);
    }
    s.new_array(&out)
}

fn get_entries<'s>(d: &Dom, s: &mut Sc<'s, '_>, _: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    list(d, s, None, None)
}

fn by_type<'s>(d: &Dom, s: &mut Sc<'s, '_>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    need(s, a, "getEntriesByType")?;
    let t = d.arg_str_s(s, a, 0)?;
    if !matches!(t.as_str(), "mark" | "measure" | "navigation") {
        return s.new_array(&[]);
    }
    list(d, s, Some(&t), None)
}

fn by_name<'s>(d: &Dom, s: &mut Sc<'s, '_>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    need(s, a, "getEntriesByName")?;
    let n = d.arg_str_s(s, a, 0)?;
    let a1 = arg_s(s, a, 1);
    let t = if present(s, a1) { Some(d.string_arg_s(s, a1)?) } else { None };
    if t.as_deref().is_some_and(|t| !matches!(t, "mark" | "measure" | "navigation")) {
        return s.new_array(&[]);
    }
    list(d, s, t.as_deref(), Some(&n))
}

fn clear<'s>(d: &Dom, s: &mut Sc<'s, '_>, a: Args<'s>, measure: bool) -> Res<'s, Local<'s>> {
    let a0 = arg_s(s, a, 0);
    let name = if present(s, a0) { Some(d.string_arg_s(s, a0)?) } else { None };
    d.perf_entries.borrow_mut().retain(|e| e.measure != measure || name.as_ref().is_some_and(|n| *n != e.name));
    Ok(s.undefined())
}

fn clear_marks<'s>(d: &Dom, s: &mut Sc<'s, '_>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    clear(d, s, a, false)
}

fn clear_measures<'s>(d: &Dom, s: &mut Sc<'s, '_>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    clear(d, s, a, true)
}

/// Define the methods on `perf`, the prototype of the `performance` object.
pub(crate) fn install(d: &Dom, it: &mut Interp<'_>, perf: ObjId) -> R<()> {
    for (name, len, h) in [
        ("mark", 1, mark as HS),
        ("measure", 1, measure),
        ("getEntries", 0, get_entries),
        ("getEntriesByType", 1, by_type),
        ("getEntriesByName", 1, by_name),
        ("clearMarks", 0, clear_marks),
        ("clearMeasures", 0, clear_measures),
    ] {
        d.def_method_s(it, perf, name, len, h)?;
    }
    for (name, h) in [("timing", timing as HS), ("navigation", navigation as HS), ("timeOrigin", time_origin as HS)] {
        let f = d.add_fn_s_obj(it, h, name, 0, HostFn::Method)?;
        let k = d.key(it, name)?;
        let desc = bare_js::heap::PropertyDescriptor { get: Some(Some(f)), enumerable: Some(true), configurable: Some(true), ..Default::default() };
        it.heap.define_own_property(perf, k, &desc).map_err(|e| it.heap_error(e))?;
    }
    Ok(())
}

/// Legacy navigation data is synthesized on first access. It does not add realm state.
#[cold]
fn legacy<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, timing: bool) -> Res<'s, Local<'s>> {
    let proto = old(s, |it| Ok(it.object_prototype()))?;
    let o = d.obj_s(s, proto)?;
    old(s, |it| crate::browser_lazy::tag(d, it, o, if timing { "PerformanceTiming" } else { "PerformanceNavigation" }))?;
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
    if is_object(s, this) {
        s.define_str(this, if timing { "timing" } else { "navigation" }, l, fixed)?;
    }
    Ok(l)
}
fn timing<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    legacy(d, s, this, true)
}
fn navigation<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    legacy(d, s, this, false)
}
/// The wall clock at time 0 of the scheduler clock, in ms. The first read fixes it on the owner, as `legacy` does.
fn time_origin<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let t = s.wall_now_ms() - d.now_ms();
    let v = s.number(t);
    if is_object(s, this) {
        let fixed = Attrs { writable: false, enumerable: true, configurable: true };
        s.define_str(this, "timeOrigin", v, fixed)?;
    }
    Ok(v)
}
