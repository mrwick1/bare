//! Small DOM and web API gaps that stopped real sites in `bare-drive`. They live in their own module so that the hot modules keep their
//! code, and so their inlining, unchanged (the cost rows move with the inlining of unrelated code: see `docs/progress.md`).
//!
//! - `console.assert`: nothing when the condition is truthy, else an error record that starts with "Assertion failed". The label is its own
//!   argument (the Standard joins it to a string first argument); the formatter prints the same text.
//! - `HTMLScriptElement.prototype.noModule`: a reflected boolean for `nomodule`.
//! - `document.hasFocus()`: always `true`. The page always has system focus (`browser_focus.rs`).
//! - `window.event`: the event of the dispatch that runs (`Dom::dispatch`), else `undefined`. A write makes a data property (`[Replaceable]`).
//! - `wrap_coll`: the snapshot arrays of `children`, `childNodes` and `getElementsBy*` inherit from one prototype that has `item`.
//!   The array is made with that prototype (`set_prototype_of` would send each array to dictionary mode).
//!
//! Not here: `namedItem`, live collections, `IDBIndex` (see `browser_shims.rs` for `animate`, `Path2D` and `indexedDB`).

use crate::browser::*;
use crate::browser_console::{ConsoleLevel, record};
use bare_html::dom::NodeId;
use bare_js::eval::Interp;
use bare_js::value::{ObjId, Value};

type V = R<Value>;

pub(crate) fn console_assert(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> V {
    if it.to_js_bool(a.first().copied().unwrap_or(Value::Undefined))? {
        return Ok(Value::Undefined);
    }
    let mut args = vec![d.js_str(it, if a.get(1).is_some_and(|v| matches!(v, Value::String(_))) { "Assertion failed:" } else { "Assertion failed" })?];
    args.extend(a.iter().skip(1));
    record(d, it, ConsoleLevel::Error, &args)
}

pub(crate) fn script_no_module(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    crate::browser_loader::flag(d, it, this, "nomodule")
}

pub(crate) fn set_script_no_module(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    crate::browser_loader::set_flag(d, it, this, a, "nomodule")
}

pub(crate) fn has_focus(_: &Dom, _: &mut Interp<'_>, _: Value, _: &[Value]) -> V {
    Ok(Value::Bool(true))
}

/// The accessor `window.event`. The same function is the getter and the setter: a write has an argument.
fn window_event(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> V {
    if let Some(&v) = a.first() {
        let g = it.global_object();
        let k = d.key(it, "event")?;
        it.define_data(g, k, v, true, true, true)?;
        return Ok(Value::Undefined);
    }
    Ok(d.st.borrow().hd.event.map_or(Value::Undefined, Value::Object))
}

/// Define `window.event` (the first read of the name, `define_window`).
#[cold]
#[inline(never)]
pub(crate) fn define_event(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    let g = it.global_object();
    let k = d.key(it, "event")?;
    let f = d.add_fn(it, window_event, "event", 0, false)?;
    let desc = bare_js::heap::PropertyDescriptor { get: Some(Some(f)), set: Some(Some(f)), enumerable: Some(true), configurable: Some(true), ..Default::default() };
    it.heap.define_own_property(g, k, &desc).map(|_| ()).map_err(|e| it.heap_error(e))
}

/// `wrap_all` for the snapshot arrays that stand for a `NodeList` or an `HTMLCollection`: the array inherits from a prototype that has `item`.
pub(crate) fn wrap_coll(d: &Dom, it: &mut Interp<'_>, ids: &[NodeId]) -> V {
    let mut vals = Vec::with_capacity(ids.len());
    for &k in ids {
        vals.push(d.wrap(it, k)?);
    }
    let p = coll_proto(d, it)?;
    it.array_from_proto(&vals, p)
}

fn coll_proto(d: &Dom, it: &mut Interp<'_>) -> R<ObjId> {
    if let Some(&p) = d.plat.coll.get() {
        return Ok(p);
    }
    make_coll_proto(d, it)
}

#[cold]
#[inline(never)]
fn make_coll_proto(d: &Dom, it: &mut Interp<'_>) -> R<ObjId> {
    let p = d.obj(it, it.array_prototype())?;
    d.def_method(it, p, "item", 1, coll_item)?;
    d.plat.fixed.borrow_mut().push(p);
    let _ = d.plat.coll.set(p);
    Ok(p)
}

/// `item(index)`: the member at the index, else `null`.
fn coll_item(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    if a.is_empty() {
        return Err(it.type_error("Failed to execute 'item': 1 argument required, but only 0 present."));
    }
    let n = it.to_js_number(a[0])?;
    let i = if n.is_finite() { n.trunc().rem_euclid(4294967296.0) } else { 0.0 };
    let k = d.key(it, &i.to_string())?;
    let v = it.get_value(this, k)?;
    Ok(if matches!(v, Value::Undefined) { Value::Null } else { v })
}

/// Set `window.event` and give the old value. Out of line, so `Dom::dispatch` (inlined into the scheduler loop) keeps its size.
#[inline(never)]
pub(crate) fn swap_event(d: &Dom, ev: Option<ObjId>) -> Option<ObjId> {
    std::mem::replace(&mut d.st.borrow_mut().hd.event, ev)
}
