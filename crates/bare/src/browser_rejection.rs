//! `PromiseRejectionEvent` and HTML "notify about rejected promises": the end of a microtask checkpoint (`Host::promise_rejections`).
//!
//! For each promise that is still unhandled: a cancelable `unhandledrejection` at `window`, when something listens. When it is not
//! canceled, one console error: `Uncaught (in promise) <reason>`. For an error object the reason is `name: message` and the first
//! frame of its `stack`. For a reported promise that got a handler later: `rejectionhandled` (not cancelable). No JS runs to make
//! the text: it reads data properties only. The interface is lazy (`browser_lazy.rs`), as `ErrorEvent` is.
//! `promise` and `reason` are own read-only data properties of the event, not prototype accessors.

use crate::eng::{Eng, Got, Ops, PState, Val};
use crate::browser::*;
use crate::browser_console::{push_text, ConsoleLevel};
use crate::browser_scoped::*;
use bare_js_api::{Args, HostFn, Local};
use bare_js::eval::MAX_PENDING_REJECTIONS;

#[cold]
#[inline(never)]
fn ensure<E: Eng>(d: &Dom<E>, it: &mut E::Vm) -> R<E, [E::Obj; 2]> {
    if let Some(o) = d.hh.rej.get() {
        return Ok(o);
    }
    let proto = d.obj(it, d.p().event)?;
    let f = d.add_fn_s_obj(it, ctor, "PromiseRejectionEvent", 2, HostFn::Ctor)?;
    let (kp, kc) = (d.key(it, "prototype")?, d.key(it, "constructor")?);
    it.define_data(f, kp, E::obj_raw(proto), false, false, false)?;
    it.define_data(proto, kc, E::obj_raw(f), true, false, true)?;
    crate::browser_lazy::tag(d, it, proto, "PromiseRejectionEvent")?;
    d.hh.rej.set(Some([proto, f]));
    Ok([proto, f])
}

fn ensure_unit<E: Eng>(d: &Dom<E>, it: &mut E::Vm) -> R<E, ()> {
    ensure(d, it).map(|_| ())
}

crate::browser_lazy::lazy_global!(lazy_rejection_event, "PromiseRejectionEvent", ensure_unit, |d| d.hh.rej.get().expect("made")[1]);

pub(crate) fn install<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>) -> Res<'s, ()> {
    table!(lazy = [
        Row::both(1, "PromiseRejectionEvent", lazy_rejection_event),
    ]);
    let g = s.global();
    d.install_rows(s, &[g], &lazy::T::<E>::V)
}

fn build<E: Eng>(d: &Dom<E>, it: &mut E::Vm, ty: &str, flags: [bool; 3], promise: E::Raw, reason: E::Raw) -> R<E, E::Obj> {
    let [proto, _] = ensure(d, it)?;
    let ev = d.new_event_proto(it, ty, flags[0], flags[1], flags[2], proto)?;
    for (name, v) in [("promise", promise), ("reason", reason)] {
        let k = d.key(it, name)?;
        it.define_data(ev, k, v, false, false, true)?;
    }
    Ok(ev)
}

/// `new PromiseRejectionEvent(type, { promise, reason })`. `promise` is required (any object, as the IDL type `object`).
fn ctor<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    if a.len() < 2 {
        return Err(s.type_error(&format!("Failed to construct 'PromiseRejectionEvent': 2 arguments required, but only {} present.", a.len())));
    }
    let ty = d.arg_str_s(s, a, 0)?;
    let init = arg_s::<E>(s, a, 1);
    if !is_object::<E>(s, init) {
        return Err(s.type_error("Failed to construct 'PromiseRejectionEvent': The provided value is not of type 'PromiseRejectionEventInit'."));
    }
    let mut flags = [false; 3];
    for (i, n) in ["bubbles", "cancelable", "composed"].into_iter().enumerate() {
        flags[i] = crate::browser_events::init_flag_s::<E>(s, init, n)?;
    }
    let promise = s.get_str(init, "promise")?;
    if !is_object::<E>(s, promise) {
        return Err(s.type_error("Failed to construct 'PromiseRejectionEvent': Failed to read the 'promise' property from 'PromiseRejectionEventInit': Required member is undefined."));
    }
    let reason = s.get_str(init, "reason")?;
    let (promise, reason) = (bare_js_api::Engine::raw(s.engine(), promise), bare_js_api::Engine::raw(s.engine(), reason));
    let ev = old::<E, _>(s, |it| build(d, it, &ty, flags, promise, reason))?;
    Ok(d.obj_local(s, ev))
}

/// Whether a listener or a handler property for `ty` is on `window`.
fn listens<E: Eng>(d: &Dom<E>, ty: &str) -> bool {
    crate::browser_handlers::sync(d, Target::Window, ty);
    d.st.borrow().listeners.get(&Target::Window).is_some_and(|v| v.iter().any(|l| &*l.ty == ty))
}

/// The `stack` text of an error object, if it is a string data property. No JS runs.
fn stack<E: Eng>(it: &mut E::Vm, o: E::Obj) -> Option<String> {
    let k = it.h_key("stack").ok()?;
    let Ok(Got::Value(v)) = it.h_get(o, k) else { return None };
    let Val::String(s) = E::kind(v) else { return None };
    Some(String::from_utf16_lossy(&it.h_str_units(s).ok()?))
}

/// The text after `Uncaught (in promise) `: `name: message` and the first frame for an object with a `stack`, else the value.
fn reason_text<E: Eng>(d: &Dom<E>, it: &mut E::Vm, reason: E::Raw) -> String {
    let Val::Object(o) = E::kind(reason) else { return d.error_text(it, reason) };
    let Some(stack) = stack::<E>(it, o) else { return it.rejection_text_o(reason) };
    let head = d.error_text(it, reason);
    match stack.lines().map(str::trim).find(|l| l.starts_with("at ")) {
        Some(at) => format!("{head} {}", at.chars().take(512).collect::<String>()),
        None => head,
    }
}

pub(crate) fn notify<E: Eng>(d: &Dom<E>, it: &mut E::Vm, unhandled: &[E::Raw], handled: &[E::Raw], dropped: u32) -> R<E, ()> {
    for &p in unhandled {
        let Some(PState::Rejected(reason)) = it.promise_status(p) else { continue };
        let not_canceled = if listens(d, "unhandledrejection") {
            let ev = build(d, it, "unhandledrejection", [false, true, false], p, reason)?;
            d.dispatch(it, Target::Window, ev)?
        } else {
            true
        };
        if not_canceled {
            let text = reason_text(d, it, reason);
            push_text(d, it, ConsoleLevel::Error, &format!("Uncaught (in promise) {text}"));
        }
    }
    if dropped > 0 {
        push_text(d, it, ConsoleLevel::Error, &format!("Uncaught (in promise): {dropped} more rejections in one checkpoint were not reported (the bound is {MAX_PENDING_REJECTIONS})"));
    }
    for &p in handled {
        if !listens(d, "rejectionhandled") {
            break;
        }
        let Some(PState::Rejected(reason)) = it.promise_status(p) else { continue };
        let ev = build(d, it, "rejectionhandled", [false, false, false], p, reason)?;
        d.dispatch(it, Target::Window, ev)?;
    }
    Ok(())
}
