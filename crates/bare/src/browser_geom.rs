//! Geometry Interfaces Level 1, the rectangles and the points: `DOMRectReadOnly`, `DOMRect`, `DOMRectList`, `DOMPointReadOnly`, `DOMPoint` and
//! `DOMQuad`. The matrices are in `browser_matrix.rs`. Chrome 155 is the model (`tools/fixtures/ui/geometry-cases.txt`, 48 cases).
//!
//! Storage: the engine has no internal slots for host objects, so each object is a plain object with a prototype, and its numbers are in
//! a side table (`Geom::slots`, pruned in `prune`). The object has no own property (a rectangle is as cheap as a bare object). A
//! `DOMRectList` is a plain object with the rectangles as its own indexed properties (read only), a `length` on the prototype, and
//! `Array.prototype.values` as its iterator. A `DOMQuad` keeps its four points in the table and traces them.
//!
//! The interfaces are lazy (`browser_lazy.rs`) in three groups. Rectangles and the list are made when a script reads one of the three globals or when
//! a layout read returns a rectangle (`rect_object`). Points and the quad are the second group, and the matrices (`browser_matrix.rs`) the third.
//!
//! Gaps (recorded, none is faked)
//! - The getters of `DOMRect.prototype` (`x`, `y`, `width`, `height`) also accept a `DOMRectReadOnly`; Chrome refuses it ("Illegal invocation"). The same for
//!   `DOMPoint` and `DOMMatrix`. The setters check the brand.
//! - The attributes are accessors of the prototype, as in Chrome. The observer entries keep their own data properties (a deviation of the
//!   entries, not of the rectangle).
//! - `structuredClone` of these objects is a `DataCloneError` (they are platform objects here); Chrome clones them.

use crate::browser_sources::Roots;
use crate::browser::*;
use crate::browser_lazy::{self, lazy_global};
use crate::browser_matrix::Mat;
use crate::browser_platform::V;
use crate::browser_scoped::*;
use bare_js::eval::{Error, Interp};
use bare_js::value::{ObjId, Value};
use bare_js_api::{Args, Attrs, HostFn, Local, Type};
use bare_layout::fragment::Rect;
use std::cell::{Cell, OnceCell, RefCell};

/// The internal slot of an object of this module.
pub(crate) enum Slot {
    /// x, y, width, height. `true`: a `DOMRect`, writable.
    Rect([f64; 4], bool),
    /// x, y, z, w.
    Point([f64; 4], bool),
    Quad([ObjId; 4]),
    /// The length of a `DOMRectList`.
    List(u32),
    Matrix(Box<Mat>),
}

/// A prototype and its constructor.
#[derive(Clone, Copy)]
pub(crate) struct Iface {
    pub proto: ObjId,
    pub ctor: ObjId,
}

#[derive(Default)]
pub(crate) struct Geom {
    pub slots: RefCell<IdMap<ObjId, Slot>>,
    /// `DOMRectReadOnly`, `DOMRect`, `DOMRectList`.
    r: OnceCell<[Iface; 3]>,
    /// `DOMPointReadOnly`, `DOMPoint`, `DOMQuad`.
    p: OnceCell<[Iface; 3]>,
    /// `DOMMatrixReadOnly`, `DOMMatrix`.
    pub m: OnceCell<[Iface; 2]>,
    /// A quad was made: `roots` has work.
    pub quad: Cell<bool>,
}

// ------------------------------------------------------------------------------------------ helpers

#[cold]
pub(crate) fn illegal_s<'s>(s: &mut Sc<'s, '_>) -> bare_js_api::Error<'s> {
    s.type_error("Illegal invocation")
}

/// `ToNumber(v)`; a Symbol gives the message of Chrome, with `ctx` before it. `ctx` is built only for that error.
pub(crate) fn num_s<'s>(s: &mut Sc<'s, '_>, v: Local<'s>, ctx: &dyn Fn() -> String) -> Res<'s, f64> {
    if s.type_of(v) == Type::Symbol {
        return Err(s.type_error(&format!("{}: Cannot convert a Symbol value to a number", ctx())));
    }
    s.to_number(v)
}

/// An object with `proto` and the slot.
pub(crate) fn new_obj_s<'s>(d: &Dom, s: &mut Sc<'s, '_>, proto: ObjId, slot: Slot) -> Res<'s, ObjId> {
    let o = d.obj_s(s, proto)?;
    d.geom.slots.borrow_mut().insert(o, slot);
    d.own_obj(s.engine(), o, crate::browser::Owner::Geom(o));
    Ok(o)
}

/// A data property that a script may change (`CreateDataProperty`).
pub(crate) fn put_s<'s>(s: &mut Sc<'s, '_>, o: Local<'s>, name: &str, v: Local<'s>) -> Res<'s, ()> {
    s.define_str(o, name, v, Attrs { writable: true, enumerable: true, configurable: true })
}

/// An operation of an interface: a writable, enumerable, configurable method.
pub(crate) fn method_s(d: &Dom, it: &mut Interp<'_>, target: ObjId, name: &str, len: u32, h: HS) -> R<()> {
    let k = d.key(it, name)?;
    let f = d.add_fn_s_obj(it, h, name, len, HostFn::Method)?;
    it.define_data(target, k, Value::Object(f), true, true, true)
}

/// A prototype and a constructor that are linked by `prototype`. Both are roots: the realm keeps them while the page can drop the global.
pub(crate) fn make_s(d: &Dom, it: &mut Interp<'_>, parent: ObjId, name: &str, ctor: HS, kind: HostFn) -> R<Iface> {
    let proto = d.obj(it, parent)?;
    let ctor = d.add_fn_s_obj(it, ctor, name, 0, kind)?;
    link(d, it, proto, ctor)
}

fn link(d: &Dom, it: &mut Interp<'_>, proto: ObjId, ctor: ObjId) -> R<Iface> {
    let kp = d.key(it, "prototype")?;
    it.define_data(ctor, kp, Value::Object(proto), false, false, false)?;
    d.fixed.borrow_mut().extend([proto, ctor]);
    Ok(Iface { proto, ctor })
}

/// `proto.constructor`, then the members in `extra`, then `Symbol.toStringTag`: the order of Chrome's own keys.
pub(crate) fn finish_s(d: &Dom, it: &mut Interp<'_>, i: Iface, name: &str, extra: &[(&str, u32, HS)]) -> R<()> {
    let kc = d.key(it, "constructor")?;
    it.define_data(i.proto, kc, Value::Object(i.ctor), true, false, true)?;
    for &(n, len, h) in extra {
        method_s(d, it, i.proto, n, len, h)?;
    }
    browser_lazy::tag(d, it, i.proto, name)
}

/// Read the members `names` of the dictionary `dict` from `v`, in this order. Undefined and null are the empty dictionary. A member that is
/// undefined is `None`. `who` starts the messages ("Failed to construct 'DOMQuad'"). `member_dict` names the dictionary of member `i`.
pub(crate) fn read_dict_s<'s, const N: usize>(s: &mut Sc<'s, '_>, v: Local<'s>, who: &str, dict: &str, member_dict: &dyn Fn(usize) -> &'static str, names: [&str; N]) -> Res<'s, [Option<f64>; N]> {
    let mut out = [None; N];
    match s.type_of(v) {
        Type::Undefined | Type::Null => {}
        Type::Object | Type::Function => {
            for (i, name) in names.into_iter().enumerate() {
                let x = s.get_str(v, name)?;
                if s.type_of(x) != Type::Undefined {
                    out[i] = Some(num_s(s, x, &|| format!("{who}: Failed to read the '{name}' property from '{}'", member_dict(i)))?);
                }
            }
        }
        _ => return Err(s.type_error(&format!("{who}: The provided value is not of type '{dict}'."))),
    }
    Ok(out)
}

fn lo(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() { f64::NAN } else if b < a { b } else { a }
}

fn hi(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() { f64::NAN } else if a < b { b } else { a }
}

/// x, y, width, height, top, right, bottom, left.
fn rect_field(v: [f64; 4], i: usize) -> f64 {
    let [x, y, w, h] = v;
    match i {
        0 => x,
        1 => y,
        2 => w,
        3 => h,
        4 => lo(y, y + h),
        5 => hi(x, x + w),
        6 => hi(y, y + h),
        _ => lo(x, x + w),
    }
}

fn rect_slot(d: &Dom, s: &mut Sc<'_, '_>, this: Local<'_>) -> Option<([f64; 4], bool)> {
    let o = obj_id(s, this)?;
    match d.geom.slots.borrow().get(&o) {
        Some(Slot::Rect(v, rw)) => Some((*v, *rw)),
        _ => None,
    }
}

fn point_slot(d: &Dom, o: ObjId) -> Option<([f64; 4], bool)> {
    match d.geom.slots.borrow().get(&o) {
        Some(Slot::Point(v, rw)) => Some((*v, *rw)),
        _ => None,
    }
}

// ------------------------------------------------------------------------------------------ rectangles

fn ensure_r(d: &Dom, it: &mut Interp<'_>) -> R<[Iface; 3]> {
    if let Some(r) = d.geom.r.get() {
        return Ok(*r);
    }
    make_r(d, it)
}

fn ensure_r_s<'s>(d: &Dom, s: &mut Sc<'s, '_>) -> Res<'s, [Iface; 3]> {
    if let Some(r) = d.geom.r.get() {
        return Ok(*r);
    }
    old(s, |it| make_r(d, it))
}

#[cold]
#[inline(never)]
fn make_r(d: &Dom, it: &mut Interp<'_>) -> R<[Iface; 3]> {
    let op = it.object_prototype();
    let ro = make_s(d, it, op, "DOMRectReadOnly", rect_ro_ctor, HostFn::CtorNewTarget)?;
    let rw = make_s(d, it, ro.proto, "DOMRect", rect_rw_ctor, HostFn::CtorNewTarget)?;
    let list = make_s(d, it, op, "DOMRectList", list_ctor, HostFn::Ctor)?;
    it.heap.set_prototype_of(rw.ctor, Some(ro.ctor)).map_err(|e| it.heap_error(e))?;
    method_s(d, it, ro.ctor, "fromRect", 0, rect_from_ro)?;
    method_s(d, it, rw.ctor, "fromRect", 0, rect_from_rw)?;
    // The attributes in the order of Chrome: x y width height, then top right bottom left (read only).
    let getters: [HS; 8] = [rect_get::<0>, rect_get::<1>, rect_get::<2>, rect_get::<3>, rect_get::<4>, rect_get::<5>, rect_get::<6>, rect_get::<7>];
    let setters: [HS; 4] = [rect_set::<0>, rect_set::<1>, rect_set::<2>, rect_set::<3>];
    let names = ["x", "y", "width", "height", "top", "right", "bottom", "left"];
    for (i, name) in names.into_iter().enumerate() {
        d.def_get_s(it, ro.proto, name, getters[i], None)?;
    }
    method_s(d, it, ro.proto, "toJSON", 0, rect_to_json)?;
    for (i, name) in names.into_iter().take(4).enumerate() {
        d.def_get_s(it, rw.proto, name, getters[i], Some(setters[i]))?;
    }
    finish_s(d, it, ro, "DOMRectReadOnly", &[])?;
    finish_s(d, it, rw, "DOMRect", &[])?;
    d.def_get_s(it, list.proto, "length", list_length, None)?;
    method_s(d, it, list.proto, "item", 1, list_item)?;
    finish_s(d, it, list, "DOMRectList", &[])?;
    let (kv, ki) = (d.key(it, "values")?, it.symbol_iterator());
    let values = it.get_value(Value::Object(it.array_prototype()), kv)?;
    it.define_data(list.proto, ki, values, true, false, true)?;
    let r = [ro, rw, list];
    let _ = d.geom.r.set(r);
    Ok(r)
}

fn ensure_r_unit(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    ensure_r(d, it).map(|_| ())
}

lazy_global!(lazy_rect_ro, "DOMRectReadOnly", ensure_r_unit, |d| d.geom.r.get().expect("made")[0].ctor);
lazy_global!(lazy_rect_rw, "DOMRect", ensure_r_unit, |d| d.geom.r.get().expect("made")[1].ctor);
lazy_global!(lazy_rect_list, "DOMRectList", ensure_r_unit, |d| d.geom.r.get().expect("made")[2].ctor);

/// A rectangle for a layout read or an observer entry. `rw`: a `DOMRect`, else a `DOMRectReadOnly`.
pub(crate) fn rect_object(d: &Dom, it: &mut Interp<'_>, v: [f64; 4], rw: bool) -> V {
    let proto = ensure_r(d, it)?[rw as usize].proto;
    let o = d.obj(it, proto)?;
    d.geom.slots.borrow_mut().insert(o, Slot::Rect(v, rw));
    d.own_obj(it, o, crate::browser::Owner::Geom(o));
    Ok(Value::Object(o))
}

/// `rect_object` as a local.
pub(crate) fn rect_object_s<'s>(d: &Dom, s: &mut Sc<'s, '_>, v: [f64; 4], rw: bool) -> Res<'s, Local<'s>> {
    let proto = ensure_r_s(d, s)?[rw as usize].proto;
    let o = new_obj_s(d, s, proto, Slot::Rect(v, rw))?;
    Ok(d.obj_local(s, o))
}

/// A layout rectangle as a `DOMRect`.
pub(crate) fn layout_rect(d: &Dom, it: &mut Interp<'_>, r: Rect) -> V {
    rect_object(d, it, [r.x as f64, r.y as f64, r.w as f64, r.h as f64], true)
}

/// A layout rectangle as a `DOMRect`, as a local.
pub(crate) fn layout_rect_s<'s>(d: &Dom, s: &mut Sc<'s, '_>, r: Rect) -> Res<'s, Local<'s>> {
    rect_object_s(d, s, [r.x as f64, r.y as f64, r.w as f64, r.h as f64], true)
}

/// A `DOMRectList` of `DOMRect`.
pub(crate) fn rect_list_s<'s>(d: &Dom, s: &mut Sc<'s, '_>, rects: &[Rect]) -> Res<'s, Local<'s>> {
    let items = rects.iter().map(|&r| layout_rect_s(d, s, r)).collect::<Res<'s, Vec<_>>>()?;
    let proto = ensure_r_s(d, s)?[2].proto;
    let o = new_obj_s(d, s, proto, Slot::List(items.len() as u32))?;
    let o = d.obj_local(s, o);
    for (i, v) in items.into_iter().enumerate() {
        let k = s.number(i as f64);
        s.define(o, k, v, Attrs { writable: false, enumerable: true, configurable: false })?;
    }
    Ok(o)
}

fn rect_ctor<'s>(d: &Dom, s: &mut Sc<'s, '_>, a: Args<'s>, rw: bool) -> Res<'s, Local<'s>> {
    let name = if rw { "DOMRect" } else { "DOMRectReadOnly" };
    let mut v = [0.0; 4];
    for (i, slot) in v.iter_mut().enumerate() {
        if let Some(x) = a.get(i).filter(|&x| s.type_of(x) != Type::Undefined) {
            *slot = num_s(s, x, &|| format!("Failed to construct '{name}'"))?;
        }
    }
    let default = ensure_r_s(d, s)?[rw as usize].proto;
    let default = d.obj_local(s, default);
    let proto = s.proto_from_new_target(default)?;
    let proto = obj_id(s, proto).expect("a prototype");
    let o = new_obj_s(d, s, proto, Slot::Rect(v, rw))?;
    Ok(d.obj_local(s, o))
}

fn rect_ro_ctor<'s>(d: &Dom, s: &mut Sc<'s, '_>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    rect_ctor(d, s, a, false)
}

fn rect_rw_ctor<'s>(d: &Dom, s: &mut Sc<'s, '_>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    rect_ctor(d, s, a, true)
}

fn rect_from<'s>(d: &Dom, s: &mut Sc<'s, '_>, a: Args<'s>, rw: bool) -> Res<'s, Local<'s>> {
    let who = if rw { "Failed to execute 'fromRect' on 'DOMRect'" } else { "Failed to execute 'fromRect' on 'DOMRectReadOnly'" };
    let v = arg_s(s, a, 0);
    let v = rect_init(s, v, who)?;
    rect_object_s(d, s, v, rw)
}

fn rect_from_ro<'s>(d: &Dom, s: &mut Sc<'s, '_>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    rect_from(d, s, a, false)
}

fn rect_from_rw<'s>(d: &Dom, s: &mut Sc<'s, '_>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    rect_from(d, s, a, true)
}

/// A `DOMRectInit`: height, width, x, y are read in this order (the members are sorted), each defaults to 0.
fn rect_init<'s>(s: &mut Sc<'s, '_>, v: Local<'s>, who: &str) -> Res<'s, [f64; 4]> {
    let [h, w, x, y] = read_dict_s(s, v, who, "DOMRectInit", &|_| "DOMRectInit", ["height", "width", "x", "y"])?;
    Ok([x.unwrap_or(0.0), y.unwrap_or(0.0), w.unwrap_or(0.0), h.unwrap_or(0.0)])
}

fn rect_get<'s, const I: usize>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    match rect_slot(d, s, this) {
        Some((v, _)) => Ok(s.number(rect_field(v, I))),
        None => Err(illegal_s(s)),
    }
}

fn rect_set<'s, const I: usize>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let Some((_, true)) = rect_slot(d, s, this) else { return Err(illegal_s(s)) };
    let name = ["x", "y", "width", "height"][I];
    let x = arg_s(s, a, 0);
    let x = num_s(s, x, &|| format!("Failed to set the '{name}' property on 'DOMRect'"))?;
    if let Some(o) = obj_id(s, this)
        && let Some(Slot::Rect(v, _)) = d.geom.slots.borrow_mut().get_mut(&o)
    {
        v[I] = x;
    }
    Ok(s.undefined())
}

fn rect_to_json<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let Some((v, _)) = rect_slot(d, s, this) else { return Err(illegal_s(s)) };
    let o = s.new_object()?;
    for (i, name) in ["x", "y", "width", "height", "top", "right", "bottom", "left"].into_iter().enumerate() {
        let n = s.number(rect_field(v, i));
        put_s(s, o, name, n)?;
    }
    Ok(o)
}

// ------------------------------------------------------------------------------------------ the list

fn list_ctor<'s>(_: &Dom, s: &mut Sc<'s, '_>, _: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    Err(s.type_error("Failed to construct 'DOMRectList': Illegal constructor"))
}

fn list_len(d: &Dom, s: &mut Sc<'_, '_>, this: Local<'_>) -> Option<u32> {
    let o = obj_id(s, this)?;
    match d.geom.slots.borrow().get(&o) {
        Some(Slot::List(n)) => Some(*n),
        _ => None,
    }
}

fn list_length<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    match list_len(d, s, this) {
        Some(n) => Ok(s.number(n as f64)),
        None => Err(illegal_s(s)),
    }
}

fn list_item<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let Some(len) = list_len(d, s, this) else { return Err(illegal_s(s)) };
    let Some(i) = a.get(0) else {
        return Err(s.type_error("Failed to execute 'item' on 'DOMRectList': 1 argument required, but only 0 present."));
    };
    let n = s.to_number(i)?;
    let i = if n.is_finite() { n.trunc().rem_euclid(4294967296.0) as u32 } else { 0 };
    if i < len {
        let k = s.number(i as f64);
        s.get(this, k)
    } else {
        Ok(s.null())
    }
}

// ------------------------------------------------------------------------------------------ points and the quad

fn ensure_p(d: &Dom, it: &mut Interp<'_>) -> R<[Iface; 3]> {
    if let Some(p) = d.geom.p.get() {
        return Ok(*p);
    }
    make_p(d, it)
}

fn ensure_p_s<'s>(d: &Dom, s: &mut Sc<'s, '_>) -> Res<'s, [Iface; 3]> {
    if let Some(p) = d.geom.p.get() {
        return Ok(*p);
    }
    old(s, |it| make_p(d, it))
}

#[cold]
#[inline(never)]
fn make_p(d: &Dom, it: &mut Interp<'_>) -> R<[Iface; 3]> {
    let op = it.object_prototype();
    let ro = make_s(d, it, op, "DOMPointReadOnly", point_ro_ctor, HostFn::CtorNewTarget)?;
    let rw = make_s(d, it, ro.proto, "DOMPoint", point_rw_ctor, HostFn::CtorNewTarget)?;
    let quad = make_s(d, it, op, "DOMQuad", quad_ctor, HostFn::CtorNewTarget)?;
    it.heap.set_prototype_of(rw.ctor, Some(ro.ctor)).map_err(|e| it.heap_error(e))?;
    method_s(d, it, ro.ctor, "fromPoint", 0, point_from_ro)?;
    method_s(d, it, rw.ctor, "fromPoint", 0, point_from_rw)?;
    let getters: [HS; 4] = [point_get::<0>, point_get::<1>, point_get::<2>, point_get::<3>];
    let setters: [HS; 4] = [point_set::<0>, point_set::<1>, point_set::<2>, point_set::<3>];
    let names = ["x", "y", "z", "w"];
    for (i, name) in names.into_iter().enumerate() {
        d.def_get_s(it, ro.proto, name, getters[i], None)?;
    }
    method_s(d, it, ro.proto, "matrixTransform", 0, point_matrix_transform)?;
    method_s(d, it, ro.proto, "toJSON", 0, point_to_json)?;
    for (i, name) in names.into_iter().enumerate() {
        d.def_get_s(it, rw.proto, name, getters[i], Some(setters[i]))?;
    }
    finish_s(d, it, ro, "DOMPointReadOnly", &[])?;
    finish_s(d, it, rw, "DOMPoint", &[])?;
    method_s(d, it, quad.ctor, "fromQuad", 0, quad_from_quad)?;
    method_s(d, it, quad.ctor, "fromRect", 0, quad_from_rect)?;
    let quad_getters: [HS; 4] = [quad_get::<0>, quad_get::<1>, quad_get::<2>, quad_get::<3>];
    for (i, name) in ["p1", "p2", "p3", "p4"].into_iter().enumerate() {
        d.def_get_s(it, quad.proto, name, quad_getters[i], None)?;
    }
    method_s(d, it, quad.proto, "getBounds", 0, quad_bounds)?;
    method_s(d, it, quad.proto, "toJSON", 0, quad_to_json)?;
    finish_s(d, it, quad, "DOMQuad", &[])?;
    let p = [ro, rw, quad];
    let _ = d.geom.p.set(p);
    Ok(p)
}

fn ensure_p_unit(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    ensure_p(d, it).map(|_| ())
}

lazy_global!(lazy_point_ro, "DOMPointReadOnly", ensure_p_unit, |d| d.geom.p.get().expect("made")[0].ctor);
lazy_global!(lazy_point_rw, "DOMPoint", ensure_p_unit, |d| d.geom.p.get().expect("made")[1].ctor);
lazy_global!(lazy_quad, "DOMQuad", ensure_p_unit, |d| d.geom.p.get().expect("made")[2].ctor);

/// A `DOMPoint` (or `DOMPointReadOnly`) with these numbers.
pub(crate) fn point_object_s<'s>(d: &Dom, s: &mut Sc<'s, '_>, v: [f64; 4], rw: bool) -> Res<'s, ObjId> {
    let proto = ensure_p_s(d, s)?[rw as usize].proto;
    new_obj_s(d, s, proto, Slot::Point(v, rw))
}

const POINT_DEFAULT: [f64; 4] = [0.0, 0.0, 0.0, 1.0];

fn point_ctor<'s>(d: &Dom, s: &mut Sc<'s, '_>, a: Args<'s>, rw: bool) -> Res<'s, Local<'s>> {
    let name = if rw { "DOMPoint" } else { "DOMPointReadOnly" };
    let mut v = POINT_DEFAULT;
    for (i, slot) in v.iter_mut().enumerate() {
        if let Some(x) = a.get(i).filter(|&x| s.type_of(x) != Type::Undefined) {
            *slot = num_s(s, x, &|| format!("Failed to construct '{name}'"))?;
        }
    }
    let default = ensure_p_s(d, s)?[rw as usize].proto;
    let default = d.obj_local(s, default);
    let proto = s.proto_from_new_target(default)?;
    let proto = obj_id(s, proto).expect("a prototype");
    let o = new_obj_s(d, s, proto, Slot::Point(v, rw))?;
    Ok(d.obj_local(s, o))
}

fn point_ro_ctor<'s>(d: &Dom, s: &mut Sc<'s, '_>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    point_ctor(d, s, a, false)
}

fn point_rw_ctor<'s>(d: &Dom, s: &mut Sc<'s, '_>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    point_ctor(d, s, a, true)
}

/// A `DOMPointInit` (x, y, z, w; the defaults are 0, 0, 0, 1).
pub(crate) fn point_init<'s>(s: &mut Sc<'s, '_>, v: Local<'s>, who: &str) -> Res<'s, [f64; 4]> {
    let r = read_dict_s(s, v, who, "DOMPointInit", &|_| "DOMPointInit", ["w", "x", "y", "z"])?;
    // The members are read in the sorted order w, x, y, z.
    Ok([r[1].unwrap_or(0.0), r[2].unwrap_or(0.0), r[3].unwrap_or(0.0), r[0].unwrap_or(1.0)])
}

fn point_from<'s>(d: &Dom, s: &mut Sc<'s, '_>, a: Args<'s>, rw: bool) -> Res<'s, Local<'s>> {
    let who = if rw { "Failed to execute 'fromPoint' on 'DOMPoint'" } else { "Failed to execute 'fromPoint' on 'DOMPointReadOnly'" };
    let v = arg_s(s, a, 0);
    let v = point_init(s, v, who)?;
    let o = point_object_s(d, s, v, rw)?;
    Ok(d.obj_local(s, o))
}

fn point_from_ro<'s>(d: &Dom, s: &mut Sc<'s, '_>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    point_from(d, s, a, false)
}

fn point_from_rw<'s>(d: &Dom, s: &mut Sc<'s, '_>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    point_from(d, s, a, true)
}

/// The numbers of the point object `this`.
fn point_of(d: &Dom, s: &mut Sc<'_, '_>, this: Local<'_>) -> Option<([f64; 4], bool)> {
    point_slot(d, obj_id(s, this)?)
}

fn point_get<'s, const I: usize>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    match point_of(d, s, this) {
        Some((v, _)) => Ok(s.number(v[I])),
        None => Err(illegal_s(s)),
    }
}

fn point_set<'s, const I: usize>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let Some((_, true)) = point_of(d, s, this) else { return Err(illegal_s(s)) };
    let name = ["x", "y", "z", "w"][I];
    let x = arg_s(s, a, 0);
    let x = num_s(s, x, &|| format!("Failed to set the '{name}' property on 'DOMPoint'"))?;
    if let Some(o) = obj_id(s, this)
        && let Some(Slot::Point(v, _)) = d.geom.slots.borrow_mut().get_mut(&o)
    {
        v[I] = x;
    }
    Ok(s.undefined())
}

fn point_to_json<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let Some((v, _)) = point_of(d, s, this) else { return Err(illegal_s(s)) };
    let o = s.new_object()?;
    for (i, name) in ["x", "y", "z", "w"].into_iter().enumerate() {
        let n = s.number(v[i]);
        put_s(s, o, name, n)?;
    }
    Ok(o)
}

fn point_matrix_transform<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let Some((v, _)) = point_of(d, s, this) else { return Err(illegal_s(s)) };
    let init = arg_s(s, a, 0);
    let m = crate::browser_matrix::init_matrix(d, s, init, "Failed to execute 'matrixTransform' on 'DOMPointReadOnly'")?;
    let o = point_object_s(d, s, m.transform(v), true)?;
    Ok(d.obj_local(s, o))
}

// The quad.

fn quad_ctor<'s>(d: &Dom, s: &mut Sc<'s, '_>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let mut pts = [POINT_DEFAULT; 4];
    for (i, p) in pts.iter_mut().enumerate() {
        let v = arg_s(s, a, i);
        *p = point_init(s, v, "Failed to construct 'DOMQuad'")?;
    }
    let default = ensure_p_s(d, s)?[2].proto;
    let default = d.obj_local(s, default);
    let proto = s.proto_from_new_target(default)?;
    let proto = obj_id(s, proto).expect("a prototype");
    let q = make_quad(d, s, proto, pts)?;
    Ok(d.obj_local(s, q))
}

fn make_quad<'s>(d: &Dom, s: &mut Sc<'s, '_>, proto: ObjId, pts: [[f64; 4]; 4]) -> Res<'s, ObjId> {
    let mut ids = [proto; 4];
    for (id, p) in ids.iter_mut().zip(pts) {
        *id = point_object_s(d, s, p, true)?;
    }
    d.geom.quad.set(true);
    new_obj_s(d, s, proto, Slot::Quad(ids))
}

fn quad_from_rect<'s>(d: &Dom, s: &mut Sc<'s, '_>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let v = arg_s(s, a, 0);
    let [x, y, w, h] = rect_init(s, v, "Failed to execute 'fromRect' on 'DOMQuad'")?;
    let proto = ensure_p_s(d, s)?[2].proto;
    let p = |x, y| [x, y, 0.0, 1.0];
    let q = make_quad(d, s, proto, [p(x, y), p(x + w, y), p(x + w, y + h), p(x, y + h)])?;
    Ok(d.obj_local(s, q))
}

fn quad_from_quad<'s>(d: &Dom, s: &mut Sc<'s, '_>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let who = "Failed to execute 'fromQuad' on 'DOMQuad'";
    let v = arg_s(s, a, 0);
    let mut pts = [POINT_DEFAULT; 4];
    match s.type_of(v) {
        Type::Undefined | Type::Null => {}
        Type::Object | Type::Function => {
            for (i, name) in ["p1", "p2", "p3", "p4"].into_iter().enumerate() {
                let x = s.get_str(v, name)?;
                if s.type_of(x) != Type::Undefined {
                    pts[i] = point_init(s, x, &format!("{who}: Failed to read the '{name}' property from 'DOMQuadInit'"))?;
                }
            }
        }
        _ => return Err(s.type_error(&format!("{who}: The provided value is not of type 'DOMQuadInit'."))),
    }
    let proto = ensure_p_s(d, s)?[2].proto;
    let q = make_quad(d, s, proto, pts)?;
    Ok(d.obj_local(s, q))
}

fn quad_points(d: &Dom, s: &mut Sc<'_, '_>, this: Local<'_>) -> Option<[ObjId; 4]> {
    let o = obj_id(s, this)?;
    match d.geom.slots.borrow().get(&o) {
        Some(Slot::Quad(p)) => Some(*p),
        _ => None,
    }
}

fn quad_get<'s, const I: usize>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    match quad_points(d, s, this) {
        Some(p) => Ok(d.obj_local(s, p[I])),
        None => Err(illegal_s(s)),
    }
}

fn quad_bounds<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let Some(ids) = quad_points(d, s, this) else { return Err(illegal_s(s)) };
    let pts: Vec<[f64; 4]> = ids.iter().map(|&o| point_slot(d, o).map_or(POINT_DEFAULT, |p| p.0)).collect();
    let (mut left, mut top, mut right, mut bottom) = (pts[0][0], pts[0][1], pts[0][0], pts[0][1]);
    for p in &pts[1..] {
        left = lo(left, p[0]);
        top = lo(top, p[1]);
        right = hi(right, p[0]);
        bottom = hi(bottom, p[1]);
    }
    rect_object_s(d, s, [left, top, right - left, bottom - top], true)
}

fn quad_to_json<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let Some(p) = quad_points(d, s, this) else { return Err(illegal_s(s)) };
    let o = s.new_object()?;
    for (i, name) in ["p1", "p2", "p3", "p4"].into_iter().enumerate() {
        let l = d.obj_local(s, p[i]);
        put_s(s, o, name, l)?;
    }
    Ok(o)
}

// ------------------------------------------------------------------------------------------ the realm

#[inline(never)]
pub(crate) fn install<'s>(d: &Dom, s: &mut Sc<'s, '_>) -> Res<'s, ()> {
    table!(lazy = [
        Row::both(1, "DOMQuad", lazy_quad),
        Row::both(1, "DOMPoint", lazy_point_rw),
        Row::both(1, "DOMPointReadOnly", lazy_point_ro),
        Row::both(1, "DOMRectList", lazy_rect_list),
        Row::both(1, "DOMRect", lazy_rect_rw),
        Row::both(1, "DOMRectReadOnly", lazy_rect_ro),
    ]);
    let g = s.global();
    d.install_rows(s, &[g], &lazy::T)?;
    crate::browser_matrix::install(d, s)
}

/// The points of every quad are roots, not traced from the quad: a trace hook runs for every object of a collection, and a root list runs once.
/// A dead quad keeps its points until the next collection (`prune` drops its slot after the mark).
pub(crate) fn roots(d: &Dom, out: &mut Roots<'_, '_>) {
    if d.geom.quad.get() {
        for s in d.geom.slots.borrow().values() {
            if let Slot::Quad(p) = s {
                out.extend(p.iter().map(|&o| Value::Object(o)));
            }
        }
    }
}

/// The geometry object `o` is dead: drop its slot.
pub(crate) fn forget(d: &Dom, o: ObjId) {
    d.geom.slots.borrow_mut().remove(&o);
}

impl Dom {
    /// Is `o` a rectangle, a point, a quad, a list or a matrix (so not plain data for structured clone)?
    pub(crate) fn is_geom_object(&self, o: ObjId) -> bool {
        self.geom.slots.borrow().contains_key(&o)
    }
}

/// The error of a call without `new` of the constructor `h`, when `h` is one of this module's.
pub(crate) fn needs_new(it: &mut Interp<'_>, h: HS) -> Option<Error> {
    let list: [(HS, &str); 7] = [
        (rect_ro_ctor, "DOMRectReadOnly"),
        (rect_rw_ctor, "DOMRect"),
        (point_ro_ctor, "DOMPointReadOnly"),
        (point_rw_ctor, "DOMPoint"),
        (quad_ctor, "DOMQuad"),
        (crate::browser_matrix::ro_ctor, "DOMMatrixReadOnly"),
        (crate::browser_matrix::rw_ctor, "DOMMatrix"),
    ];
    if h as usize == list_ctor as HS as usize {
        return Some(it.type_error("Illegal constructor"));
    }
    let n = list.iter().find(|&&(f, _)| f as usize == h as usize).map(|&(_, n)| n)?;
    Some(it.type_error(&format!("Failed to construct '{n}': Please use the 'new' operator, this DOM object constructor cannot be called as a function.")))
}
