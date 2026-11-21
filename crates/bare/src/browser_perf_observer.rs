//! `PerformanceObserver` (Performance Timeline): an observer that never gets an entry, and `supportedEntryTypes` is empty.
//!
//! Cause: the MediaWiki modules of a Wikipedia article (`mediawiki.experiments`, `ext.wikimediaEvents`) call `new PerformanceObserver(cb)` and
//! threw `ReferenceError`. A page that tests `PerformanceObserver.supportedEntryTypes.includes('layout-shift')` before it observes skips its
//! use, and a page that observes `navigation` waits for a callback that does not come, as on a browser that has no such entry.
//!
//! - The global is lazy (`browser_lazy.rs`): the prototype, the constructor and the methods are made by the first read.
//! - `new PerformanceObserver(callback)` needs a function. `observe(options)` needs an object with `type` or `entryTypes` and does nothing else.
//!   `disconnect()` does nothing and `takeRecords()` gives a new empty array.
//! - Absent: `PerformanceObserverEntryList`. `PerformanceEntry` and its subclasses are in `browser_perf.rs`, with the entries of `performance.getEntries*`.

use crate::browser::*;
use bare_js::eval::Interp;
use bare_js::value::Value;
use crate::browser_scoped::*;
use bare_js_api::{Args, HostFn, Local, Type};

fn ctor<'s>(d: &Dom, s: &mut Sc<'s, '_>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let cb = arg_s(s, a, 0);
    if !s.is_callable(cb) {
        return Err(s.type_error("Failed to construct 'PerformanceObserver': The callback provided as parameter 1 is not a function."));
    }
    let proto = d.perf_observer.get().expect("made");
    let o = d.obj_s(s, proto)?;
    Ok(d.obj_local(s, o))
}

fn observe<'s>(_: &Dom, s: &mut Sc<'s, '_>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let o = arg_s(s, a, 0);
    if !is_object(s, o) {
        return Err(s.type_error("Failed to execute 'observe' on 'PerformanceObserver': parameter 1 is not of type 'PerformanceObserverInit'."));
    }
    let (t, e) = (s.get_str(o, "type")?, s.get_str(o, "entryTypes")?);
    if s.type_of(t) == Type::Undefined && s.type_of(e) == Type::Undefined {
        return Err(s.type_error("Failed to execute 'observe' on 'PerformanceObserver': Either an 'entryTypes' or 'type' member must be present."));
    }
    Ok(s.undefined())
}

fn nothing<'s>(_: &Dom, s: &mut Sc<'s, '_>, _: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    Ok(s.undefined())
}

fn take_records<'s>(_: &Dom, s: &mut Sc<'s, '_>, _: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    s.new_array(&[])
}

#[cold]
#[inline(never)]
fn ensure(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    if d.perf_observer.get().is_some() {
        return Ok(());
    }
    let proto = d.obj(it, it.object_prototype())?;
    let f = d.add_fn_s_obj(it, ctor, "PerformanceObserver", 1, HostFn::Ctor)?;
    let (kp, kc) = (d.key(it, "prototype")?, d.key(it, "constructor")?);
    it.define_data(f, kp, Value::Object(proto), false, false, false)?;
    it.define_data(proto, kc, Value::Object(f), true, false, true)?;
    d.def_method_s(it, proto, "observe", 1, observe)?;
    d.def_method_s(it, proto, "disconnect", 0, nothing)?;
    d.def_method_s(it, proto, "takeRecords", 0, take_records)?;
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

pub(crate) fn install<'s>(d: &Dom, s: &mut Sc<'s, '_>) -> Res<'s, ()> {
    table!(lazy = [
        Row::both(1, "PerformanceObserver", lazy_observer),
    ]);
    let g = s.global();
    d.install_rows(s, &[g], &lazy::T)
}
