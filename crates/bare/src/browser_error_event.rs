//! `ErrorEvent` (HTML Standard): the event of the `error` handler of `window`, made for an uncaught exception (`Dom::report`).
//!
//! The interface is lazy (`browser_lazy.rs`): a page that never reads `ErrorEvent` and has no window `error` listener pays nothing.
//! `message`, `filename`, `lineno`, `colno` and `error` are own read-only data properties of the event, not prototype accessors
//! (deviation: `hasOwnProperty('message')` is true). The event of an uncaught exception has `message` = `Uncaught Name: text`,
//! `filename` = the empty string, `lineno` = `colno` = 0 (the evaluator has no source position to give here).

use crate::browser::*;
use crate::browser_scoped::*;
use bare_js::eval::Interp;
use bare_js::value::{ObjId, Value};
use bare_js_api::{Args, Engine, HostFn, Local, Type};

#[cold]
#[inline(never)]
fn ensure(d: &Dom, it: &mut Interp<'_>) -> R<[ObjId; 2]> {
    if let Some(o) = d.hh.err.get() {
        return Ok(o);
    }
    let proto = d.obj(it, d.p().event)?;
    let f = d.add_fn_s_obj(it, ctor, "ErrorEvent", 1, HostFn::Ctor)?;
    let (kp, kc) = (d.key(it, "prototype")?, d.key(it, "constructor")?);
    it.define_data(f, kp, Value::Object(proto), false, false, false)?;
    it.define_data(proto, kc, Value::Object(f), true, false, true)?;
    crate::browser_lazy::tag(d, it, proto, "ErrorEvent")?;
    d.hh.err.set(Some([proto, f]));
    Ok([proto, f])
}

fn ensure_unit(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    ensure(d, it).map(|_| ())
}

crate::browser_lazy::lazy_global!(lazy_error_event, "ErrorEvent", ensure_unit, |d| d.hh.err.get().expect("made")[1]);

pub(crate) fn install<'s>(d: &Dom, s: &mut Sc<'s, '_>) -> Res<'s, ()> {
    crate::browser_lazy::install(d, s, &[("ErrorEvent", lazy_error_event as HS)])
}

/// The fields of an `ErrorEvent`.
pub(crate) struct Fields<'a> {
    pub message: &'a str,
    pub filename: &'a str,
    pub lineno: u32,
    pub colno: u32,
    pub error: Value,
}

/// A new `ErrorEvent` of type `ty`.
pub(crate) fn build(d: &Dom, it: &mut Interp<'_>, ty: &str, flags: [bool; 3], f: Fields<'_>) -> R<ObjId> {
    let [proto, _] = ensure(d, it)?;
    let ev = d.new_event_proto(it, ty, flags[0], flags[1], flags[2], proto)?;
    d.st.borrow_mut().events.get_mut(&ev).expect("just made").err = true;
    let (m, s) = (d.js_str(it, f.message)?, d.js_str(it, f.filename)?);
    for (name, v) in [("message", m), ("filename", s), ("lineno", Value::Number(f.lineno as f64)), ("colno", Value::Number(f.colno as f64)), ("error", f.error)] {
        let k = d.key(it, name)?;
        it.define_data(ev, k, v, false, false, true)?;
    }
    Ok(ev)
}

fn ctor<'s>(d: &Dom, s: &mut Sc<'s, '_>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    if a.is_empty() {
        return Err(s.type_error("Failed to construct 'ErrorEvent': 1 argument required, but only 0 present."));
    }
    let ty = d.arg_str_s(s, a, 0)?;
    let init = arg_s(s, a, 1);
    let (mut flags, mut message, mut filename, mut nums, mut error) = ([false; 3], String::new(), String::new(), [0u32; 2], Value::Null);
    if is_object(s, init) {
        for (i, n) in ["bubbles", "cancelable", "composed"].into_iter().enumerate() {
            flags[i] = crate::browser_events::init_flag_s(s, init, n)?;
        }
        for (name, out) in [("message", &mut message), ("filename", &mut filename)] {
            let v = s.get_str(init, name)?;
            if s.type_of(v) != Type::Undefined {
                *out = d.string_arg_s(s, v)?;
            }
        }
        for (i, n) in ["lineno", "colno"].into_iter().enumerate() {
            let v = s.get_str(init, n)?;
            if s.type_of(v) != Type::Undefined {
                let x = s.to_number(v)?;
                nums[i] = if x.is_finite() { (x.trunc() % 4294967296.0).rem_euclid(4294967296.0) as u32 } else { 0 };
            }
        }
        let v = s.get_str(init, "error")?;
        if s.type_of(v) != Type::Undefined {
            error = s.engine().raw(v);
        }
    } else if !is_nullish(s, init) {
        return Err(s.type_error("The provided value is not of type 'ErrorEventInit'."));
    }
    let ev = old(s, |it| build(d, it, &ty, flags, Fields { message: &message, filename: &filename, lineno: nums[0], colno: nums[1], error }))?;
    Ok(d.obj_local(s, ev))
}
