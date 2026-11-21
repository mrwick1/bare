//! `ErrorEvent` (HTML Standard): the event of the `error` handler of `window`, made for an uncaught exception (`Dom::report`).
//!
//! The interface is lazy (`browser_lazy.rs`): a page that never reads `ErrorEvent` and has no window `error` listener pays nothing.
//! `message`, `filename`, `lineno`, `colno` and `error` are own read-only data properties of the event, not prototype accessors
//! (deviation: `hasOwnProperty('message')` is true). The event of an uncaught exception has `message` = `Uncaught Name: text`,
//! `filename` = the empty string, `lineno` = `colno` = 0 (the evaluator has no source position to give here).

use crate::eng::{Eng, Ops};
use crate::browser::*;
use crate::browser_scoped::*;
use bare_js_api::{Args, Engine, HostFn, Local, Type};

#[cold]
#[inline(never)]
fn ensure<E: Eng>(d: &Dom<E>, it: &mut E::Vm) -> R<E, [E::Obj; 2]> {
    if let Some(o) = d.hh.err.get() {
        return Ok(o);
    }
    let proto = d.obj(it, d.p().event)?;
    let f = d.add_fn_s_obj(it, ctor, "ErrorEvent", 1, HostFn::Ctor)?;
    let (kp, kc) = (d.key(it, "prototype")?, d.key(it, "constructor")?);
    it.define_data(f, kp, E::obj_raw(proto), false, false, false)?;
    it.define_data(proto, kc, E::obj_raw(f), true, false, true)?;
    crate::browser_lazy::tag(d, it, proto, "ErrorEvent")?;
    d.hh.err.set(Some([proto, f]));
    Ok([proto, f])
}

fn ensure_unit<E: Eng>(d: &Dom<E>, it: &mut E::Vm) -> R<E, ()> {
    ensure(d, it).map(|_| ())
}

crate::browser_lazy::lazy_global!(lazy_error_event, "ErrorEvent", ensure_unit, |d| d.hh.err.get().expect("made")[1]);

pub(crate) fn install<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>) -> Res<'s, ()> {
    table!(lazy = [
        Row::both(1, "ErrorEvent", lazy_error_event),
    ]);
    let g = s.global();
    d.install_rows(s, &[g], &lazy::T::<E>::V)
}

/// The fields of an `ErrorEvent`.
pub(crate) struct Fields<'a, E: Eng> {
    pub message: &'a str,
    pub filename: &'a str,
    pub lineno: u32,
    pub colno: u32,
    pub error: E::Raw,
}

/// A new `ErrorEvent` of type `ty`.
pub(crate) fn build<E: Eng>(d: &Dom<E>, it: &mut E::Vm, ty: &str, flags: [bool; 3], f: Fields<'_, E>) -> R<E, E::Obj> {
    let [proto, _] = ensure(d, it)?;
    let ev = d.new_event_proto(it, ty, flags[0], flags[1], flags[2], proto)?;
    d.st.borrow_mut().events.get_mut(&ev).expect("just made").err = true;
    let (m, s) = (d.js_str(it, f.message)?, d.js_str(it, f.filename)?);
    for (name, v) in [("message", m), ("filename", s), ("lineno", E::num(f.lineno as f64)), ("colno", E::num(f.colno as f64)), ("error", f.error)] {
        let k = d.key(it, name)?;
        it.define_data(ev, k, v, false, false, true)?;
    }
    Ok(ev)
}

fn ctor<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    if a.is_empty() {
        return Err(s.type_error("Failed to construct 'ErrorEvent': 1 argument required, but only 0 present."));
    }
    let ty = d.arg_str_s(s, a, 0)?;
    let init = arg_s::<E>(s, a, 1);
    let (mut flags, mut message, mut filename, mut nums, mut error) = ([false; 3], String::new(), String::new(), [0u32; 2], E::null());
    if is_object::<E>(s, init) {
        for (i, n) in ["bubbles", "cancelable", "composed"].into_iter().enumerate() {
            flags[i] = crate::browser_events::init_flag_s::<E>(s, init, n)?;
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
    } else if !is_nullish::<E>(s, init) {
        return Err(s.type_error("The provided value is not of type 'ErrorEventInit'."));
    }
    let ev = old::<E, _>(s, |it| build(d, it, &ty, flags, Fields { message: &message, filename: &filename, lineno: nums[0], colno: nums[1], error }))?;
    Ok(d.obj_local(s, ev))
}
