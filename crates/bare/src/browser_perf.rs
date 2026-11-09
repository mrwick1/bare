//! The user timing part of `performance` (Performance Timeline, User Timing): `mark`, `measure`, `getEntries`, `getEntriesByType`,
//! `getEntriesByName`, `clearMarks` and `clearMeasures`.
//!
//! Cause: Bing, GitLab and other pages call `performance.getEntriesByType('navigation')` or `performance.mark(...)` at load. A missing
//! method threw `TypeError` and stopped the script.
//!
//! - Only `mark` and `measure` entries exist. `navigation`, `resource`, `paint` and every other type give an empty array.
//! - An entry is plain Rust data. Each read makes new plain objects with `name`, `entryType`, `startTime`, `duration` and `detail`, so
//!   two reads of one entry are not `===`, and there is nothing to root. The object that `mark` or `measure` returns carries the `detail`
//!   of its options; a later read has `detail: null`.
//! - No `PerformanceEntry`, `PerformanceMark` or `PerformanceMeasure` global. An entry inherits from `Object.prototype`.
//! - A `measure` argument that names a mark uses the last mark of that name. `navigationStart` is 0. An unknown name is a `SyntaxError`.
//! - After `MAX_ENTRIES` entries, a new one is returned but not kept.
//! - The methods and legacy navigation accessors are installed with the scheduler. The entry list is empty until the first `mark`.

use crate::browser::*;
use bare_js::eval::Interp;
use bare_js::value::{ObjId, Value};

const MAX_ENTRIES: usize = 50_000;

pub(crate) struct Entry {
    name: String,
    measure: bool,
    start: f64,
    dur: f64,
}

fn arg(a: &[Value], i: usize) -> Value {
    a.get(i).copied().unwrap_or(Value::Undefined)
}

fn need(it: &mut Interp<'_>, a: &[Value], what: &str) -> R<()> {
    if a.is_empty() {
        return Err(it.type_error(&format!("Failed to execute '{what}' on 'Performance': 1 argument required, but only 0 present.")));
    }
    Ok(())
}

fn member(d: &Dom, it: &mut Interp<'_>, o: Value, name: &str) -> R<Value> {
    let k = d.key(it, name)?;
    it.get_value(o, k)
}

/// The plain object of an entry.
fn make(d: &Dom, it: &mut Interp<'_>, e: &Entry, detail: Value) -> R<Value> {
    let o = d.obj(it, it.object_prototype())?;
    for (k, v) in [
        ("name", d.js_str(it, &e.name)?),
        ("entryType", d.js_str(it, if e.measure { "measure" } else { "mark" })?),
        ("startTime", Value::Number(e.start)),
        ("duration", Value::Number(e.dur)),
        ("detail", detail),
    ] {
        let k = d.key(it, k)?;
        it.define_data(o, k, v, true, true, true)?;
    }
    Ok(Value::Object(o))
}

fn keep(d: &Dom, e: Entry) {
    let mut l = d.perf_entries.borrow_mut();
    if l.len() < MAX_ENTRIES {
        l.push(e);
    }
}

fn mark(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> R<Value> {
    need(it, a, "mark")?;
    let name = d.string_arg(it, a[0])?;
    let (mut start, mut detail) = (d.now_ms(), Value::Null);
    if let o @ Value::Object(_) = arg(a, 1) {
        match member(d, it, o, "startTime")? {
            Value::Undefined => {}
            v => {
                start = it.to_js_number(v)?;
                if !start.is_finite() || start < 0.0 {
                    return Err(it.type_error("Failed to execute 'mark' on 'Performance': 'startTime' must be a non-negative finite number."));
                }
            }
        }
        match member(d, it, o, "detail")? {
            Value::Undefined => {}
            v => detail = v,
        }
    }
    let e = Entry { name, measure: false, start, dur: 0.0 };
    let v = make(d, it, &e, detail)?;
    keep(d, e);
    Ok(v)
}

/// A time of `measure`: a number, or the name of a mark. `None` for `undefined`.
fn time(d: &Dom, it: &mut Interp<'_>, v: Value) -> R<Option<f64>> {
    match v {
        Value::Undefined => Ok(None),
        Value::Number(n) => Ok(Some(n)),
        v => {
            let n = d.string_arg(it, v)?;
            if n == "navigationStart" {
                return Ok(Some(0.0));
            }
            match d.perf_entries.borrow().iter().rev().find(|e| !e.measure && e.name == n) {
                Some(e) => Ok(Some(e.start)),
                None => Err(d.dom_err(it, "SyntaxError", &format!("Failed to execute 'measure' on 'Performance': The mark '{n}' does not exist."))),
            }
        }
    }
}

fn measure(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> R<Value> {
    need(it, a, "measure")?;
    let name = d.string_arg(it, a[0])?;
    let (start, end);
    let (mut dur, mut detail) = (None, Value::Null);
    if let o @ Value::Object(_) = arg(a, 1) {
        let (s, e, du, de) = (member(d, it, o, "start")?, member(d, it, o, "end")?, member(d, it, o, "duration")?, member(d, it, o, "detail")?);
        let present = |v| !matches!(v, Value::Undefined);
        if [s, e, du, de].into_iter().any(present) {
            if present(arg(a, 2)) {
                return Err(it.type_error("Failed to execute 'measure' on 'Performance': The measureOptions and endMark cannot both be passed."));
            }
            if (!present(s) && !present(e)) || (present(s) && present(e) && present(du)) {
                return Err(it.type_error("Failed to execute 'measure' on 'Performance': Invalid measure options."));
            }
        }
        (start, end) = (time(d, it, s)?, time(d, it, if present(arg(a, 2)) { arg(a, 2) } else { e })?);
        if !matches!(du, Value::Undefined) {
            dur = Some(it.to_js_number(du)?);
        }
        if !matches!(de, Value::Undefined) {
            detail = de;
        }
    } else {
        (start, end) = (time(d, it, arg(a, 1))?, time(d, it, arg(a, 2))?);
    }
    let now = d.now_ms();
    let (s, e) = match (start, end, dur) {
        (Some(s), None, Some(du)) => (s, s + du),
        (None, Some(e), Some(du)) => (e - du, e),
        (s, e, _) => (s.unwrap_or(0.0), e.unwrap_or(now)),
    };
    let en = Entry { name, measure: true, start: s, dur: e - s };
    let v = make(d, it, &en, detail)?;
    keep(d, en);
    Ok(v)
}

fn list(d: &Dom, it: &mut Interp<'_>, ty: Option<&str>, name: Option<&str>) -> R<Value> {
    let mut idx: Vec<usize> = {
        let l = d.perf_entries.borrow();
        (0..l.len()).filter(|&i| ty.is_none_or(|t| t == if l[i].measure { "measure" } else { "mark" }) && name.is_none_or(|n| n == l[i].name)).collect()
    };
    it.charge(idx.len() as u64)?;
    idx.sort_by(|&x, &y| {
        let l = d.perf_entries.borrow();
        l[x].start.total_cmp(&l[y].start)
    });
    let mut out = Vec::with_capacity(idx.len());
    for i in idx {
        let e = {
            let l = d.perf_entries.borrow();
            Entry { name: l[i].name.clone(), measure: l[i].measure, start: l[i].start, dur: l[i].dur }
        };
        out.push(make(d, it, &e, Value::Null)?);
    }
    it.array_from(&out)
}

fn get_entries(d: &Dom, it: &mut Interp<'_>, _: Value, _: &[Value]) -> R<Value> {
    list(d, it, None, None)
}

fn by_type(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> R<Value> {
    need(it, a, "getEntriesByType")?;
    let t = d.string_arg(it, a[0])?;
    if t != "mark" && t != "measure" {
        return it.array_from(&[]);
    }
    list(d, it, Some(&t), None)
}

fn by_name(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> R<Value> {
    need(it, a, "getEntriesByName")?;
    let n = d.string_arg(it, a[0])?;
    let t = match arg(a, 1) {
        Value::Undefined => None,
        v => Some(d.string_arg(it, v)?),
    };
    if t.as_deref().is_some_and(|t| t != "mark" && t != "measure") {
        return it.array_from(&[]);
    }
    list(d, it, t.as_deref(), Some(&n))
}

fn clear(d: &Dom, it: &mut Interp<'_>, a: &[Value], measure: bool) -> R<Value> {
    let name = match arg(a, 0) {
        Value::Undefined => None,
        v => Some(d.string_arg(it, v)?),
    };
    d.perf_entries.borrow_mut().retain(|e| e.measure != measure || name.as_ref().is_some_and(|n| *n != e.name));
    Ok(Value::Undefined)
}

fn clear_marks(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> R<Value> {
    clear(d, it, a, false)
}

fn clear_measures(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> R<Value> {
    clear(d, it, a, true)
}

/// Define the methods on `perf`, the prototype of the `performance` object.
pub(crate) fn install(d: &Dom, it: &mut Interp<'_>, perf: ObjId) -> R<()> {
    for (name, len, h) in [
        ("mark", 1, mark as H),
        ("measure", 1, measure),
        ("getEntries", 0, get_entries),
        ("getEntriesByType", 1, by_type),
        ("getEntriesByName", 1, by_name),
        ("clearMarks", 0, clear_marks),
        ("clearMeasures", 0, clear_measures),
    ] {
        d.def_method(it, perf, name, len, h)?;
    }
    for (name, h) in [("timing", timing as H), ("navigation", navigation as H)] {
        let f = d.add_fn(it, h, name, 0, false)?;
        let k = d.key(it, name)?;
        let desc = bare_js::heap::PropertyDescriptor { get: Some(Some(f)), enumerable: Some(true), configurable: Some(true), ..Default::default() };
        it.heap.define_own_property(perf, k, &desc).map_err(|e| it.heap_error(e))?;
    }
    Ok(())
}

/// Legacy navigation data is synthesized on first access. It does not add realm state.
#[cold]
fn legacy(d: &Dom, it: &mut Interp<'_>, this: Value, timing: bool) -> R<Value> {
    let o = d.obj(it, it.object_prototype())?;
    crate::browser_lazy::tag(d, it, o, if timing { "PerformanceTiming" } else { "PerformanceNavigation" })?;
    if timing {
        let origin = (it.wall_now_ms() - d.now_ms()).max(1.0).floor();
        for name in ["navigationStart", "unloadEventStart", "unloadEventEnd", "redirectStart", "redirectEnd", "fetchStart", "domainLookupStart", "domainLookupEnd", "connectStart", "connectEnd", "secureConnectionStart", "requestStart", "responseStart", "responseEnd", "domLoading", "domInteractive", "domContentLoadedEventStart", "domContentLoadedEventEnd", "domComplete", "loadEventStart", "loadEventEnd"] {
            let zero = matches!(name, "unloadEventStart" | "unloadEventEnd" | "redirectStart" | "redirectEnd" | "secureConnectionStart");
            let k = d.key(it, name)?;
            it.define_data(o, k, Value::Number(if zero { 0.0 } else { origin }), false, true, true)?;
        }
    } else {
        for (name, n) in [("type", 0.0), ("redirectCount", 0.0), ("TYPE_NAVIGATE", 0.0), ("TYPE_RELOAD", 1.0), ("TYPE_BACK_FORWARD", 2.0), ("TYPE_RESERVED", 255.0)] {
            let k = d.key(it, name)?;
            it.define_data(o, k, Value::Number(n), false, true, true)?;
        }
    }
    if let Value::Object(owner) = this {
        let k = d.key(it, if timing { "timing" } else { "navigation" })?;
        it.define_data(owner, k, Value::Object(o), false, true, true)?;
    }
    Ok(Value::Object(o))
}
fn timing(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> R<Value> {
    legacy(d, it, this, true)
}
fn navigation(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> R<Value> {
    legacy(d, it, this, false)
}
