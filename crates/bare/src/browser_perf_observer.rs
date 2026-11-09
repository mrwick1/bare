//! `PerformanceObserver` (Performance Timeline): an observer that never gets an entry, and `supportedEntryTypes` is empty.
//!
//! Cause: the MediaWiki modules of a Wikipedia article (`mediawiki.experiments`, `ext.wikimediaEvents`) call `new PerformanceObserver(cb)` and
//! threw `ReferenceError`. A page that tests `PerformanceObserver.supportedEntryTypes.includes('layout-shift')` before it observes skips its
//! use, and a page that observes `navigation` waits for a callback that does not come, as on a browser that has no such entry.
//!
//! - The global is lazy (`browser_lazy.rs`): the prototype, the constructor and the methods are made by the first read.
//! - `new PerformanceObserver(callback)` needs a function. `observe(options)` needs an object with `type` or `entryTypes` and does nothing else.
//!   `disconnect()` does nothing and `takeRecords()` gives a new empty array.
//! - Absent: `PerformanceEntry`, `PerformanceObserverEntryList`. The entries of `performance.getEntries*` are in `browser_perf.rs`.

use crate::browser::*;
use bare_js::eval::Interp;
use bare_js::value::Value;

fn arg(a: &[Value], i: usize) -> Value {
    a.get(i).copied().unwrap_or(Value::Undefined)
}

fn ctor(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> R<Value> {
    if !it.is_callable_value(arg(a, 0)) {
        return Err(it.type_error("Failed to construct 'PerformanceObserver': The callback provided as parameter 1 is not a function."));
    }
    let proto = d.perf_observer.get().expect("made");
    Ok(Value::Object(d.obj(it, proto)?))
}

fn observe(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> R<Value> {
    let o = arg(a, 0);
    if !matches!(o, Value::Object(_)) {
        return Err(it.type_error("Failed to execute 'observe' on 'PerformanceObserver': parameter 1 is not of type 'PerformanceObserverInit'."));
    }
    let (kt, ke) = (d.key(it, "type")?, d.key(it, "entryTypes")?);
    if matches!(it.get_value(o, kt)?, Value::Undefined) && matches!(it.get_value(o, ke)?, Value::Undefined) {
        return Err(it.type_error("Failed to execute 'observe' on 'PerformanceObserver': Either an 'entryTypes' or 'type' member must be present."));
    }
    Ok(Value::Undefined)
}

fn nothing(_: &Dom, _: &mut Interp<'_>, _: Value, _: &[Value]) -> R<Value> {
    Ok(Value::Undefined)
}

fn take_records(_: &Dom, it: &mut Interp<'_>, _: Value, _: &[Value]) -> R<Value> {
    it.array_from(&[])
}

#[cold]
#[inline(never)]
fn ensure(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    if d.perf_observer.get().is_some() {
        return Ok(());
    }
    let proto = d.obj(it, it.object_prototype())?;
    let f = d.add_fn(it, ctor, "PerformanceObserver", 1, true)?;
    let (kp, kc) = (d.key(it, "prototype")?, d.key(it, "constructor")?);
    it.define_data(f, kp, Value::Object(proto), false, false, false)?;
    it.define_data(proto, kc, Value::Object(f), true, false, true)?;
    d.def_method(it, proto, "observe", 1, observe)?;
    d.def_method(it, proto, "disconnect", 0, nothing)?;
    d.def_method(it, proto, "takeRecords", 0, take_records)?;
    crate::browser_lazy::tag(d, it, proto, "PerformanceObserver")?;
    let k = d.key(it, "supportedEntryTypes")?;
    let none = it.array_from(&[])?;
    it.define_data(f, k, none, false, true, true)?;
    d.perf_observer.set(Some(proto));
    d.fixed.borrow_mut().extend([proto, f]);
    d.perf_observer_ctor.set(Some(f));
    Ok(())
}

crate::browser_lazy::lazy_global!(lazy_observer, "PerformanceObserver", ensure, |d| d.perf_observer_ctor.get().expect("made"));

pub(crate) fn install(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    crate::browser_lazy::install(d, it, &[("PerformanceObserver", lazy_observer as H)])
}
