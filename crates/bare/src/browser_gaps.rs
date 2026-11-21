//! Small DOM and web API gaps that stopped real sites in `bare-drive`. They live in their own module so that the hot modules keep their
//! code, and so their inlining, unchanged (the cost rows move with the inlining of unrelated code: see `docs/progress.md`).
//!
//! - `HTMLScriptElement.prototype.noModule`: a reflected boolean for `nomodule`.
//! - `document.hasFocus()`: always `true`. The page always has system focus (`browser_focus.rs`).
//! - `window.event`: the event of the dispatch that runs (`Dom::dispatch`), else `undefined`. A write makes a data property (`[Replaceable]`).
//! - `wrap_coll`: the snapshot arrays of `children`, `childNodes` and `getElementsBy*` inherit from one prototype that has `item`.
//!   The array is made with that prototype (`set_prototype_of` would send each array to dictionary mode).
//!
//! Not here: `namedItem`, live collections, `IDBIndex` (see `browser_shims.rs` for `animate`, `Path2D` and `indexedDB`).

use crate::eng::{Eng, Ops};
use crate::browser::*;
use crate::browser_scoped::*;
use bare_js_api::{Args, HostFn, Local, Type};
use bare_html::dom::NodeId;

type V<E: Eng> = R<E, E::Raw>;

pub(crate) fn script_no_module<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    crate::browser_loader::flag(d, s, this, "nomodule")
}

pub(crate) fn set_script_no_module<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    crate::browser_loader::set_flag(d, s, this, a, "nomodule")
}

pub(crate) fn has_focus<'s, E: Eng>(_: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    Ok(s.boolean(true))
}

/// The accessor `window.event`. The same function is the getter and the setter: a write has an argument.
fn window_event<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    if let Some(v) = a.get(0) {
        let g = s.global();
        s.define_str(g, "event", v, bare_js_api::Attrs::ALL)?;
        return Ok(s.undefined());
    }
    let ev = d.st.borrow().hd.event;
    Ok(match ev {
        Some(o) => d.obj_local(s, o),
        None => s.undefined(),
    })
}

/// Define `window.event` (the first read of the name, `define_window`).
#[cold]
#[inline(never)]
pub(crate) fn define_event<E: Eng>(d: &Dom<E>, it: &mut E::Vm) -> R<E, ()> {
    let g = it.global_object();
    let k = d.key(it, "event")?;
    let f = d.add_fn_s_obj(it, window_event, "event", 0, HostFn::Method)?;
    let desc = crate::eng::Desc::<E> { get: Some(Some(f)), set: Some(Some(f)), enumerable: Some(true), configurable: Some(true), ..Default::default() };
    it.h_define_own(g, k, &desc).map(|_| ()).map_err(|e| e)
}

/// `wrap_all` for the snapshot arrays that stand for a `NodeList` or an `HTMLCollection`: the array inherits from a prototype that has `item`.
pub(crate) fn wrap_coll<E: Eng>(d: &Dom<E>, it: &mut E::Vm, ids: &[NodeId]) -> V<E> {
    let mut vals = Vec::with_capacity(ids.len());
    for &k in ids {
        vals.push(d.wrap(it, k)?);
    }
    let p = coll_proto(d, it)?;
    it.array_from_proto(&vals, p)
}

fn coll_proto<E: Eng>(d: &Dom<E>, it: &mut E::Vm) -> R<E, E::Obj> {
    if let Some(&p) = d.plat.coll.get() {
        return Ok(p);
    }
    make_coll_proto(d, it)
}

#[cold]
#[inline(never)]
fn make_coll_proto<E: Eng>(d: &Dom<E>, it: &mut E::Vm) -> R<E, E::Obj> {
    let p = d.obj(it, it.array_prototype())?;
    d.def_method_s(it, p, "item", 1, coll_item)?;
    d.plat.fixed.borrow_mut().push(p);
    let _ = d.plat.coll.set(p);
    Ok(p)
}

/// `item(index)`: the member at the index, else `null`.
fn coll_item<'s, E: Eng>(_d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    if a.is_empty() {
        return Err(s.type_error("Failed to execute 'item': 1 argument required, but only 0 present."));
    }
    let arg0 = arg_s::<E>(s, a, 0);
    let n = s.to_number(arg0)?;
    let i = if n.is_finite() { n.trunc().rem_euclid(4294967296.0) } else { 0.0 };
    let v = s.get_str(this, &i.to_string())?;
    Ok(if matches!(s.type_of(v), Type::Undefined) { s.null() } else { v })
}

/// Set `window.event` and give the old value. Out of line, so `Dom::dispatch` (inlined into the scheduler loop) keeps its size.
#[inline(never)]
pub(crate) fn swap_event<E: Eng>(d: &Dom<E>, ev: Option<E::Obj>) -> Option<E::Obj> {
    std::mem::replace(&mut d.st.borrow_mut().hd.event, ev)
}
