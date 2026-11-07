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

use crate::browser::*;
use crate::browser_lazy::{self, lazy_global};
use crate::browser_matrix::Mat;
use crate::browser_platform::{V, arg};
use bare_js::eval::{Error, Interp};
use bare_js::value::{ObjId, PropertyKey, Value};
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

pub(crate) fn illegal(it: &mut Interp<'_>) -> Error {
    it.type_error("Illegal invocation")
}

/// `ToNumber(v)`; a Symbol gives the message of Chrome, with `ctx` before it. `ctx` is built only for that error.
pub(crate) fn num(it: &mut Interp<'_>, v: Value, ctx: &dyn Fn() -> String) -> R<f64> {
    if matches!(v, Value::Symbol(_)) {
        return Err(it.type_error(&format!("{}: Cannot convert a Symbol value to a number", ctx())));
    }
    it.to_js_number(v)
}

/// An object with `proto` and the slot.
pub(crate) fn new_obj(d: &Dom, it: &mut Interp<'_>, proto: ObjId, slot: Slot) -> R<ObjId> {
    let o = d.obj(it, proto)?;
    d.geom.slots.borrow_mut().insert(o, slot);
    Ok(o)
}

/// A data property that a script may change (`CreateDataProperty`).
pub(crate) fn put(d: &Dom, it: &mut Interp<'_>, o: ObjId, name: &str, v: Value) -> R<()> {
    let k = d.key(it, name)?;
    it.define_data(o, k, v, true, true, true)
}

/// An operation of an interface: a writable, enumerable, configurable method.
pub(crate) fn method(d: &Dom, it: &mut Interp<'_>, target: ObjId, name: &str, len: u32, h: H) -> R<()> {
    let k = d.key(it, name)?;
    let f = d.add_fn(it, h, name, len, false)?;
    it.define_data(target, k, Value::Object(f), true, true, true)
}

/// A prototype and a constructor that are linked by `prototype`. Both are roots: the realm keeps them while the page can drop the global.
pub(crate) fn make(d: &Dom, it: &mut Interp<'_>, parent: ObjId, name: &str, ctor: H, nt: bool) -> R<Iface> {
    let proto = d.obj(it, parent)?;
    let ctor = if nt { d.add_ctor_nt(it, ctor, name, 0)? } else { d.add_fn(it, ctor, name, 0, true)? };
    let kp = d.key(it, "prototype")?;
    it.define_data(ctor, kp, Value::Object(proto), false, false, false)?;
    d.fixed.borrow_mut().extend([proto, ctor]);
    Ok(Iface { proto, ctor })
}

/// `proto.constructor`, then the members in `extra`, then `Symbol.toStringTag`: the order of Chrome's own keys.
pub(crate) fn finish(d: &Dom, it: &mut Interp<'_>, i: Iface, name: &str, extra: &[(&str, u32, H)]) -> R<()> {
    let kc = d.key(it, "constructor")?;
    it.define_data(i.proto, kc, Value::Object(i.ctor), true, false, true)?;
    for &(n, len, h) in extra {
        method(d, it, i.proto, n, len, h)?;
    }
    browser_lazy::tag(d, it, i.proto, name)
}

/// Read the members `names` of the dictionary `dict` from `v`, in this order. Undefined and null are the empty dictionary. A member that is
/// undefined is `None`. `who` starts the messages ("Failed to construct 'DOMQuad'"). `member_dict` names the dictionary of member `i`.
pub(crate) fn read_dict<const N: usize>(d: &Dom, it: &mut Interp<'_>, v: Value, who: &str, dict: &str, member_dict: &dyn Fn(usize) -> &'static str, names: [&str; N]) -> R<[Option<f64>; N]> {
    let mut out = [None; N];
    match v {
        Value::Undefined | Value::Null => {}
        Value::Object(_) => {
            for (i, name) in names.into_iter().enumerate() {
                let k = d.key(it, name)?;
                let x = it.get_value(v, k)?;
                if !matches!(x, Value::Undefined) {
                    out[i] = Some(num(it, x, &|| format!("{who}: Failed to read the '{name}' property from '{}'", member_dict(i)))?);
                }
            }
        }
        _ => return Err(it.type_error(&format!("{who}: The provided value is not of type '{dict}'."))),
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

fn rect_slot(d: &Dom, this: Value) -> Option<([f64; 4], bool)> {
    let Value::Object(o) = this else { return None };
    match d.geom.slots.borrow().get(&o) {
        Some(Slot::Rect(v, rw)) => Some((*v, *rw)),
        _ => None,
    }
}

fn point_slot(d: &Dom, this: Value) -> Option<([f64; 4], bool)> {
    let Value::Object(o) = this else { return None };
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

#[cold]
#[inline(never)]
fn make_r(d: &Dom, it: &mut Interp<'_>) -> R<[Iface; 3]> {
    let op = it.object_prototype();
    let ro = make(d, it, op, "DOMRectReadOnly", rect_ro_ctor, true)?;
    let rw = make(d, it, ro.proto, "DOMRect", rect_rw_ctor, true)?;
    let list = make(d, it, op, "DOMRectList", list_ctor, false)?;
    it.heap.set_prototype_of(rw.ctor, Some(ro.ctor)).map_err(|e| it.heap_error(e))?;
    method(d, it, ro.ctor, "fromRect", 0, rect_from_ro)?;
    method(d, it, rw.ctor, "fromRect", 0, rect_from_rw)?;
    // The attributes in the order of Chrome: x y width height, then top right bottom left (read only).
    let getters: [H; 8] = [rect_get::<0>, rect_get::<1>, rect_get::<2>, rect_get::<3>, rect_get::<4>, rect_get::<5>, rect_get::<6>, rect_get::<7>];
    let setters: [H; 4] = [rect_set::<0>, rect_set::<1>, rect_set::<2>, rect_set::<3>];
    let names = ["x", "y", "width", "height", "top", "right", "bottom", "left"];
    for (i, name) in names.into_iter().enumerate() {
        d.def_get(it, ro.proto, name, getters[i], None)?;
    }
    method(d, it, ro.proto, "toJSON", 0, rect_to_json)?;
    for (i, name) in names.into_iter().take(4).enumerate() {
        d.def_get(it, rw.proto, name, getters[i], Some(setters[i]))?;
    }
    finish(d, it, ro, "DOMRectReadOnly", &[])?;
    finish(d, it, rw, "DOMRect", &[])?;
    d.def_get(it, list.proto, "length", list_length, None)?;
    method(d, it, list.proto, "item", 1, list_item)?;
    finish(d, it, list, "DOMRectList", &[])?;
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
    Ok(Value::Object(new_obj(d, it, proto, Slot::Rect(v, rw))?))
}

/// A layout rectangle as a `DOMRect`.
pub(crate) fn layout_rect(d: &Dom, it: &mut Interp<'_>, r: Rect) -> V {
    rect_object(d, it, [r.x as f64, r.y as f64, r.w as f64, r.h as f64], true)
}

/// A `DOMRectList` of `DOMRect`.
pub(crate) fn rect_list(d: &Dom, it: &mut Interp<'_>, rects: &[Rect]) -> V {
    let items = rects.iter().map(|&r| layout_rect(d, it, r)).collect::<R<Vec<_>>>()?;
    let proto = ensure_r(d, it)?[2].proto;
    let o = new_obj(d, it, proto, Slot::List(items.len() as u32))?;
    for (i, v) in items.into_iter().enumerate() {
        it.define_data(o, PropertyKey::Index(i as u32), v, false, true, false)?;
    }
    Ok(Value::Object(o))
}

fn rect_ctor(d: &Dom, it: &mut Interp<'_>, a: &[Value], rw: bool) -> V {
    let name = if rw { "DOMRect" } else { "DOMRectReadOnly" };
    let mut v = [0.0; 4];
    for (i, slot) in v.iter_mut().enumerate() {
        let x = arg(a, i);
        if !matches!(x, Value::Undefined) {
            *slot = num(it, x, &|| format!("Failed to construct '{name}'"))?;
        }
    }
    let default = ensure_r(d, it)?[rw as usize].proto;
    let proto = it.host_proto_from_new_target(default)?;
    Ok(Value::Object(new_obj(d, it, proto, Slot::Rect(v, rw))?))
}

fn rect_ro_ctor(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> V {
    rect_ctor(d, it, a, false)
}

fn rect_rw_ctor(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> V {
    rect_ctor(d, it, a, true)
}

fn rect_from(d: &Dom, it: &mut Interp<'_>, a: &[Value], rw: bool) -> V {
    let who = if rw { "Failed to execute 'fromRect' on 'DOMRect'" } else { "Failed to execute 'fromRect' on 'DOMRectReadOnly'" };
    let v = rect_init(d, it, arg(a, 0), who)?;
    rect_object(d, it, v, rw)
}

fn rect_from_ro(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> V {
    rect_from(d, it, a, false)
}

fn rect_from_rw(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> V {
    rect_from(d, it, a, true)
}

/// A `DOMRectInit`: height, width, x, y are read in this order (the members are sorted), each defaults to 0.
fn rect_init(d: &Dom, it: &mut Interp<'_>, v: Value, who: &str) -> R<[f64; 4]> {
    let [h, w, x, y] = read_dict(d, it, v, who, "DOMRectInit", &|_| "DOMRectInit", ["height", "width", "x", "y"])?;
    Ok([x.unwrap_or(0.0), y.unwrap_or(0.0), w.unwrap_or(0.0), h.unwrap_or(0.0)])
}

fn rect_get<const I: usize>(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    match rect_slot(d, this) {
        Some((v, _)) => Ok(Value::Number(rect_field(v, I))),
        None => Err(illegal(it)),
    }
}

fn rect_set<const I: usize>(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let Some((_, true)) = rect_slot(d, this) else { return Err(illegal(it)) };
    let name = ["x", "y", "width", "height"][I];
    let x = num(it, arg(a, 0), &|| format!("Failed to set the '{name}' property on 'DOMRect'"))?;
    if let Value::Object(o) = this
        && let Some(Slot::Rect(v, _)) = d.geom.slots.borrow_mut().get_mut(&o)
    {
        v[I] = x;
    }
    Ok(Value::Undefined)
}

fn rect_to_json(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let Some((v, _)) = rect_slot(d, this) else { return Err(illegal(it)) };
    let o = d.obj(it, it.object_prototype())?;
    for (i, name) in ["x", "y", "width", "height", "top", "right", "bottom", "left"].into_iter().enumerate() {
        put(d, it, o, name, Value::Number(rect_field(v, i)))?;
    }
    Ok(Value::Object(o))
}

// ------------------------------------------------------------------------------------------ the list

fn list_ctor(_: &Dom, it: &mut Interp<'_>, _: Value, _: &[Value]) -> V {
    Err(it.type_error("Failed to construct 'DOMRectList': Illegal constructor"))
}

fn list_len(d: &Dom, this: Value) -> Option<u32> {
    let Value::Object(o) = this else { return None };
    match d.geom.slots.borrow().get(&o) {
        Some(Slot::List(n)) => Some(*n),
        _ => None,
    }
}

fn list_length(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    list_len(d, this).map(|n| Value::Number(n as f64)).ok_or_else(|| illegal(it))
}

fn list_item(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let Some(len) = list_len(d, this) else { return Err(illegal(it)) };
    let Some(&i) = a.first() else {
        return Err(it.type_error("Failed to execute 'item' on 'DOMRectList': 1 argument required, but only 0 present."));
    };
    let n = it.to_js_number(i)?;
    let i = if n.is_finite() { n.trunc().rem_euclid(4294967296.0) as u32 } else { 0 };
    if i < len { it.get_value(this, PropertyKey::Index(i)) } else { Ok(Value::Null) }
}

// ------------------------------------------------------------------------------------------ points and the quad

fn ensure_p(d: &Dom, it: &mut Interp<'_>) -> R<[Iface; 3]> {
    if let Some(p) = d.geom.p.get() {
        return Ok(*p);
    }
    make_p(d, it)
}

#[cold]
#[inline(never)]
fn make_p(d: &Dom, it: &mut Interp<'_>) -> R<[Iface; 3]> {
    let op = it.object_prototype();
    let ro = make(d, it, op, "DOMPointReadOnly", point_ro_ctor, true)?;
    let rw = make(d, it, ro.proto, "DOMPoint", point_rw_ctor, true)?;
    let quad = make(d, it, op, "DOMQuad", quad_ctor, true)?;
    it.heap.set_prototype_of(rw.ctor, Some(ro.ctor)).map_err(|e| it.heap_error(e))?;
    method(d, it, ro.ctor, "fromPoint", 0, point_from_ro)?;
    method(d, it, rw.ctor, "fromPoint", 0, point_from_rw)?;
    let getters: [H; 4] = [point_get::<0>, point_get::<1>, point_get::<2>, point_get::<3>];
    let setters: [H; 4] = [point_set::<0>, point_set::<1>, point_set::<2>, point_set::<3>];
    let names = ["x", "y", "z", "w"];
    for (i, name) in names.into_iter().enumerate() {
        d.def_get(it, ro.proto, name, getters[i], None)?;
    }
    method(d, it, ro.proto, "matrixTransform", 0, point_matrix_transform)?;
    method(d, it, ro.proto, "toJSON", 0, point_to_json)?;
    for (i, name) in names.into_iter().enumerate() {
        d.def_get(it, rw.proto, name, getters[i], Some(setters[i]))?;
    }
    finish(d, it, ro, "DOMPointReadOnly", &[])?;
    finish(d, it, rw, "DOMPoint", &[])?;
    method(d, it, quad.ctor, "fromQuad", 0, quad_from_quad)?;
    method(d, it, quad.ctor, "fromRect", 0, quad_from_rect)?;
    let quad_getters: [H; 4] = [quad_get::<0>, quad_get::<1>, quad_get::<2>, quad_get::<3>];
    for (i, name) in ["p1", "p2", "p3", "p4"].into_iter().enumerate() {
        d.def_get(it, quad.proto, name, quad_getters[i], None)?;
    }
    method(d, it, quad.proto, "getBounds", 0, quad_bounds)?;
    method(d, it, quad.proto, "toJSON", 0, quad_to_json)?;
    finish(d, it, quad, "DOMQuad", &[])?;
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
pub(crate) fn point_object(d: &Dom, it: &mut Interp<'_>, v: [f64; 4], rw: bool) -> R<ObjId> {
    let proto = ensure_p(d, it)?[rw as usize].proto;
    new_obj(d, it, proto, Slot::Point(v, rw))
}

const POINT_DEFAULT: [f64; 4] = [0.0, 0.0, 0.0, 1.0];

fn point_ctor(d: &Dom, it: &mut Interp<'_>, a: &[Value], rw: bool) -> V {
    let name = if rw { "DOMPoint" } else { "DOMPointReadOnly" };
    let mut v = POINT_DEFAULT;
    for (i, slot) in v.iter_mut().enumerate() {
        let x = arg(a, i);
        if !matches!(x, Value::Undefined) {
            *slot = num(it, x, &|| format!("Failed to construct '{name}'"))?;
        }
    }
    let default = ensure_p(d, it)?[rw as usize].proto;
    let proto = it.host_proto_from_new_target(default)?;
    Ok(Value::Object(new_obj(d, it, proto, Slot::Point(v, rw))?))
}

fn point_ro_ctor(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> V {
    point_ctor(d, it, a, false)
}

fn point_rw_ctor(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> V {
    point_ctor(d, it, a, true)
}

/// A `DOMPointInit` (x, y, z, w; the defaults are 0, 0, 0, 1).
pub(crate) fn point_init(d: &Dom, it: &mut Interp<'_>, v: Value, who: &str) -> R<[f64; 4]> {
    let r = read_dict(d, it, v, who, "DOMPointInit", &|_| "DOMPointInit", ["w", "x", "y", "z"])?;
    // The members are read in the sorted order w, x, y, z.
    Ok([r[1].unwrap_or(0.0), r[2].unwrap_or(0.0), r[3].unwrap_or(0.0), r[0].unwrap_or(1.0)])
}

fn point_from(d: &Dom, it: &mut Interp<'_>, a: &[Value], rw: bool) -> V {
    let who = if rw { "Failed to execute 'fromPoint' on 'DOMPoint'" } else { "Failed to execute 'fromPoint' on 'DOMPointReadOnly'" };
    let v = point_init(d, it, arg(a, 0), who)?;
    Ok(Value::Object(point_object(d, it, v, rw)?))
}

fn point_from_ro(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> V {
    point_from(d, it, a, false)
}

fn point_from_rw(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> V {
    point_from(d, it, a, true)
}

fn point_get<const I: usize>(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    match point_slot(d, this) {
        Some((v, _)) => Ok(Value::Number(v[I])),
        None => Err(illegal(it)),
    }
}

fn point_set<const I: usize>(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let Some((_, true)) = point_slot(d, this) else { return Err(illegal(it)) };
    let name = ["x", "y", "z", "w"][I];
    let x = num(it, arg(a, 0), &|| format!("Failed to set the '{name}' property on 'DOMPoint'"))?;
    if let Value::Object(o) = this
        && let Some(Slot::Point(v, _)) = d.geom.slots.borrow_mut().get_mut(&o)
    {
        v[I] = x;
    }
    Ok(Value::Undefined)
}

fn point_to_json(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let Some((v, _)) = point_slot(d, this) else { return Err(illegal(it)) };
    let o = d.obj(it, it.object_prototype())?;
    for (i, name) in ["x", "y", "z", "w"].into_iter().enumerate() {
        put(d, it, o, name, Value::Number(v[i]))?;
    }
    Ok(Value::Object(o))
}

fn point_matrix_transform(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let Some((v, _)) = point_slot(d, this) else { return Err(illegal(it)) };
    let m = crate::browser_matrix::init_matrix(d, it, arg(a, 0), "Failed to execute 'matrixTransform' on 'DOMPointReadOnly'")?;
    Ok(Value::Object(point_object(d, it, m.transform(v), true)?))
}

// The quad.

fn quad_ctor(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> V {
    let mut pts = [POINT_DEFAULT; 4];
    for (i, p) in pts.iter_mut().enumerate() {
        *p = point_init(d, it, arg(a, i), "Failed to construct 'DOMQuad'")?;
    }
    let default = ensure_p(d, it)?[2].proto;
    let proto = it.host_proto_from_new_target(default)?;
    Ok(Value::Object(make_quad(d, it, proto, pts)?))
}

fn make_quad(d: &Dom, it: &mut Interp<'_>, proto: ObjId, pts: [[f64; 4]; 4]) -> R<ObjId> {
    let mut ids = [proto; 4];
    for (id, p) in ids.iter_mut().zip(pts) {
        *id = point_object(d, it, p, true)?;
    }
    d.geom.quad.set(true);
    new_obj(d, it, proto, Slot::Quad(ids))
}

fn quad_from_rect(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> V {
    let [x, y, w, h] = rect_init(d, it, arg(a, 0), "Failed to execute 'fromRect' on 'DOMQuad'")?;
    let proto = ensure_p(d, it)?[2].proto;
    let p = |x, y| [x, y, 0.0, 1.0];
    Ok(Value::Object(make_quad(d, it, proto, [p(x, y), p(x + w, y), p(x + w, y + h), p(x, y + h)])?))
}

fn quad_from_quad(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> V {
    let who = "Failed to execute 'fromQuad' on 'DOMQuad'";
    let v = arg(a, 0);
    let mut pts = [POINT_DEFAULT; 4];
    match v {
        Value::Undefined | Value::Null => {}
        Value::Object(_) => {
            for (i, name) in ["p1", "p2", "p3", "p4"].into_iter().enumerate() {
                let k = d.key(it, name)?;
                let x = it.get_value(v, k)?;
                if !matches!(x, Value::Undefined) {
                    pts[i] = point_init(d, it, x, &format!("{who}: Failed to read the '{name}' property from 'DOMQuadInit'"))?;
                }
            }
        }
        _ => return Err(it.type_error(&format!("{who}: The provided value is not of type 'DOMQuadInit'."))),
    }
    let proto = ensure_p(d, it)?[2].proto;
    Ok(Value::Object(make_quad(d, it, proto, pts)?))
}

fn quad_points(d: &Dom, this: Value) -> Option<[ObjId; 4]> {
    let Value::Object(o) = this else { return None };
    match d.geom.slots.borrow().get(&o) {
        Some(Slot::Quad(p)) => Some(*p),
        _ => None,
    }
}

fn quad_get<const I: usize>(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    match quad_points(d, this) {
        Some(p) => Ok(Value::Object(p[I])),
        None => Err(illegal(it)),
    }
}

fn quad_bounds(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let Some(ids) = quad_points(d, this) else { return Err(illegal(it)) };
    let pts: Vec<[f64; 4]> = ids.iter().map(|&o| point_slot(d, Value::Object(o)).map_or(POINT_DEFAULT, |p| p.0)).collect();
    let (mut left, mut top, mut right, mut bottom) = (pts[0][0], pts[0][1], pts[0][0], pts[0][1]);
    for p in &pts[1..] {
        left = lo(left, p[0]);
        top = lo(top, p[1]);
        right = hi(right, p[0]);
        bottom = hi(bottom, p[1]);
    }
    rect_object(d, it, [left, top, right - left, bottom - top], true)
}

fn quad_to_json(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let Some(p) = quad_points(d, this) else { return Err(illegal(it)) };
    let o = d.obj(it, it.object_prototype())?;
    for (i, name) in ["p1", "p2", "p3", "p4"].into_iter().enumerate() {
        put(d, it, o, name, Value::Object(p[i]))?;
    }
    Ok(Value::Object(o))
}

// ------------------------------------------------------------------------------------------ the realm

#[inline(never)]
pub(crate) fn install(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    browser_lazy::install(
        d,
        it,
        &[
            ("DOMQuad", lazy_quad as H),
            ("DOMPoint", lazy_point_rw),
            ("DOMPointReadOnly", lazy_point_ro),
            ("DOMRectList", lazy_rect_list),
            ("DOMRect", lazy_rect_rw),
            ("DOMRectReadOnly", lazy_rect_ro),
        ],
    )?;
    crate::browser_matrix::install(d, it)
}

/// The points of every quad are roots, not traced from the quad: a trace hook runs for every object of a collection, and a root list runs once.
/// A dead quad keeps its points until the next collection (`prune` drops its slot after the mark).
pub(crate) fn roots(d: &Dom, out: &mut Vec<Value>) {
    if d.geom.quad.get() {
        for s in d.geom.slots.borrow().values() {
            if let Slot::Quad(p) = s {
                out.extend(p.iter().map(|&o| Value::Object(o)));
            }
        }
    }
}

pub(crate) fn prune(d: &Dom, live: &dyn Fn(ObjId) -> bool) {
    let mut s = d.geom.slots.borrow_mut();
    if !s.is_empty() {
        s.retain(|&o, _| live(o));
    }
}

impl Dom {
    /// Is `o` a rectangle, a point, a quad, a list or a matrix (so not plain data for structured clone)?
    pub(crate) fn is_geom_object(&self, o: ObjId) -> bool {
        self.geom.slots.borrow().contains_key(&o)
    }
}

/// The error of a call without `new` of the constructor `h`, when `h` is one of this module's.
pub(crate) fn needs_new(it: &mut Interp<'_>, h: H) -> Option<Error> {
    let list: [(H, &str); 7] = [
        (rect_ro_ctor, "DOMRectReadOnly"),
        (rect_rw_ctor, "DOMRect"),
        (point_ro_ctor, "DOMPointReadOnly"),
        (point_rw_ctor, "DOMPoint"),
        (quad_ctor, "DOMQuad"),
        (crate::browser_matrix::ro_ctor, "DOMMatrixReadOnly"),
        (crate::browser_matrix::rw_ctor, "DOMMatrix"),
    ];
    if h as usize == list_ctor as H as usize {
        return Some(it.type_error("Illegal constructor"));
    }
    let n = list.iter().find(|&&(f, _)| f as usize == h as usize).map(|&(_, n)| n)?;
    Some(it.type_error(&format!("Failed to construct '{n}': Please use the 'new' operator, this DOM object constructor cannot be called as a function.")))
}
