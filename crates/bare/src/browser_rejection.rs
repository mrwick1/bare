//! `PromiseRejectionEvent` and HTML "notify about rejected promises": the end of a microtask checkpoint (`Host::promise_rejections`).
//!
//! For each promise that is still unhandled: a cancelable `unhandledrejection` at `window`, when something listens. When it is not
//! canceled, one console error: `Uncaught (in promise) <reason>`. For an error object the reason is `name: message` and the first
//! frame of its `stack`. For a reported promise that got a handler later: `rejectionhandled` (not cancelable). No JS runs to make
//! the text: it reads data properties only. The interface is lazy (`browser_lazy.rs`), as `ErrorEvent` is.
//! `promise` and `reason` are own read-only data properties of the event, not prototype accessors.

use crate::browser::*;
use crate::browser_console::{push_text, ConsoleLevel};
use bare_js::eval::{Interp, PromiseStatus, MAX_PENDING_REJECTIONS};
use bare_js::heap::Get;
use bare_js::value::{ObjId, Value};

#[cold]
#[inline(never)]
fn ensure(d: &Dom, it: &mut Interp<'_>) -> R<[ObjId; 2]> {
    if let Some(o) = d.hh.rej.get() {
        return Ok(o);
    }
    let proto = d.obj(it, d.p().event)?;
    let f = d.add_fn(it, ctor, "PromiseRejectionEvent", 2, true)?;
    let (kp, kc) = (d.key(it, "prototype")?, d.key(it, "constructor")?);
    it.define_data(f, kp, Value::Object(proto), false, false, false)?;
    it.define_data(proto, kc, Value::Object(f), true, false, true)?;
    crate::browser_lazy::tag(d, it, proto, "PromiseRejectionEvent")?;
    d.hh.rej.set(Some([proto, f]));
    Ok([proto, f])
}

fn ensure_unit(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    ensure(d, it).map(|_| ())
}

crate::browser_lazy::lazy_global!(lazy_rejection_event, "PromiseRejectionEvent", ensure_unit, |d| d.hh.rej.get().expect("made")[1]);

pub(crate) fn install(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    crate::browser_lazy::install(d, it, &[("PromiseRejectionEvent", lazy_rejection_event as H)])
}

fn build(d: &Dom, it: &mut Interp<'_>, ty: &str, flags: [bool; 3], promise: Value, reason: Value) -> R<ObjId> {
    let [proto, _] = ensure(d, it)?;
    let ev = d.new_event_proto(it, ty, flags[0], flags[1], flags[2], proto)?;
    for (name, v) in [("promise", promise), ("reason", reason)] {
        let k = d.key(it, name)?;
        it.define_data(ev, k, v, false, false, true)?;
    }
    Ok(ev)
}

/// `new PromiseRejectionEvent(type, { promise, reason })`. `promise` is required (any object, as the IDL type `object`).
fn ctor(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> R<Value> {
    if a.len() < 2 {
        return Err(it.type_error(&format!("Failed to construct 'PromiseRejectionEvent': 2 arguments required, but only {} present.", a.len())));
    }
    let ty = d.string_arg(it, a[0])?;
    let init = a[1];
    if !matches!(init, Value::Object(_)) {
        return Err(it.type_error("Failed to construct 'PromiseRejectionEvent': The provided value is not of type 'PromiseRejectionEventInit'."));
    }
    let mut flags = [false; 3];
    for (i, n) in ["bubbles", "cancelable", "composed"].into_iter().enumerate() {
        flags[i] = crate::browser_events::init_flag(d, it, init, n)?;
    }
    let (kp, kr) = (d.key(it, "promise")?, d.key(it, "reason")?);
    let promise = it.get_value(init, kp)?;
    if !matches!(promise, Value::Object(_)) {
        return Err(it.type_error("Failed to construct 'PromiseRejectionEvent': Failed to read the 'promise' property from 'PromiseRejectionEventInit': Required member is undefined."));
    }
    let reason = it.get_value(init, kr)?;
    Ok(Value::Object(build(d, it, &ty, flags, promise, reason)?))
}

/// Whether a listener or a handler property for `ty` is on `window`.
fn listens(d: &Dom, ty: &str) -> bool {
    crate::browser_handlers::sync(d, Target::Window, ty);
    d.st.borrow().listeners.get(&Target::Window).is_some_and(|v| v.iter().any(|l| &*l.ty == ty))
}

/// The `stack` text of an error object, if it is a string data property. No JS runs.
fn stack(it: &mut Interp<'_>, o: ObjId) -> Option<String> {
    let k = it.heap.key_from_str("stack").ok()?;
    let Ok(Get::Value(Value::String(s))) = it.heap.get(o, k) else { return None };
    Some(String::from_utf16_lossy(it.heap.str_units(s).ok()?))
}

/// The text after `Uncaught (in promise) `: `name: message` and the first frame for an object with a `stack`, else the value.
fn reason_text(d: &Dom, it: &mut Interp<'_>, reason: Value) -> String {
    let Value::Object(o) = reason else { return d.error_text(it, reason) };
    let Some(stack) = stack(it, o) else { return it.display(reason) };
    let head = d.error_text(it, reason);
    match stack.lines().map(str::trim).find(|l| l.starts_with("at ")) {
        Some(at) => format!("{head} {}", at.chars().take(512).collect::<String>()),
        None => head,
    }
}

pub(crate) fn notify(d: &Dom, it: &mut Interp<'_>, unhandled: &[Value], handled: &[Value], dropped: u32) -> R<()> {
    for &p in unhandled {
        let Some(PromiseStatus::Rejected(reason)) = it.promise_status(p) else { continue };
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
        let Some(PromiseStatus::Rejected(reason)) = it.promise_status(p) else { continue };
        let ev = build(d, it, "rejectionhandled", [false, false, false], p, reason)?;
        d.dispatch(it, Target::Window, ev)?;
    }
    Ok(())
}
