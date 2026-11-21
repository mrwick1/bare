//! `Reflect` (the census names `RGet`, `RApply`, `RSet`, `RDefineProperty`, `RGetPrototypeOf`, `RGetOwnPropertyDescriptor`, `OwnKeys`):
//! thin wrappers over the object operations of [`Realm`] (`bi_object.rs`). js2 has no proxy, so every operation is the ordinary one.
//! The difference from `Object.*` is the result: a failed operation is `false`, not a `TypeError`. The receiver of `get` and `set` is a
//! parameter, so they walk the prototype chain by hand and call the accessor with the receiver.
//! `apply` leaves the call to the caller through the tail channel, so it adds no native frame.
use crate::lazy::{Ent, m};
use crate::bi_object::{Desc, Own};
use crate::builtins::arg;
use crate::interp_rt::{Key, R, Realm};
use crate::object::Obj;
use crate::shapes::{A_C, A_W};
use crate::value::Value;

/// The object that `v` is, or a `TypeError` that names the method.
fn target(rt: &mut Realm, v: Value, what: &str) -> R<Obj> {
    match Realm::obj(v) {
        Some(o) => Ok(o),
        None => rt.type_error(&format!("Reflect.{what} called on a non-object")),
    }
}

/// `ToPropertyDescriptor`: reads the six fields with `[[Get]]`, so an accessor on the descriptor object runs.
fn to_desc(rt: &mut Realm, v: Value) -> R<Desc> {
    if Realm::obj(v).is_none() {
        return rt.type_error("property description must be an object");
    }
    let mut d = Desc::default();
    for name in ["enumerable", "configurable", "value", "writable", "get", "set"] {
        let key = Key::Name(rt.intern(name));
        if !rt.has_prop(v, key)? {
            continue;
        }
        let x = rt.get_prop(v, key)?;
        match name {
            "enumerable" => d.e = Some(Realm::truthy(x)),
            "configurable" => d.c = Some(Realm::truthy(x)),
            "value" => d.value = Some(x),
            "writable" => d.w = Some(Realm::truthy(x)),
            "get" => d.get = Some(x),
            _ => d.set = Some(x),
        }
    }
    for f in [d.get, d.set].into_iter().flatten() {
        if !f.is_undefined() && !Realm::obj(f).is_some_and(|f| f.is_callable()) {
            return rt.type_error("getter and setter must be functions");
        }
    }
    if (d.get.is_some() || d.set.is_some()) && (d.value.is_some() || d.w.is_some()) {
        return rt.type_error("invalid property descriptor: cannot both specify accessors and a value or writable");
    }
    Ok(d)
}

/// `CreateListFromArrayLike`.
pub(crate) fn list_from(rt: &mut Realm, v: Value) -> R<Vec<Value>> {
    if Realm::obj(v).is_none() {
        return rt.type_error("argument list must be an array-like object");
    }
    let n = rt.get_named(v, "length")?;
    let n = rt.to_number(n)?;
    let n = if n.is_nan() || n <= 0.0 { 0 } else { n.min(9007199254740991.0) as u64 };
    if n > u32::MAX as u64 {
        return Err(rt.error(crate::interp_rt::ErrKind::Range, "invalid array length"));
    }
    (0..n as u32).map(|i| rt.get_prop(v, Key::Idx(i))).collect()
}

fn apply(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let f = arg(args, 0);
    if !Realm::obj(f).is_some_and(|f| f.is_callable()) {
        return rt.type_error("Reflect.apply target is not a function");
    }
    let list = list_from(rt, arg(args, 2))?;
    rt.tail_args.clear();
    rt.tail_args.extend(list);
    rt.tail = Some((f, arg(args, 1)));
    Ok(Value::UNDEFINED)
}

fn construct(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let c = arg(args, 0);
    let nt = if args.len() > 2 { args[2] } else { c };
    if !rt.is_constructor(c) || !rt.is_constructor(nt) {
        return rt.type_error("Reflect.construct needs constructors");
    }
    let list = list_from(rt, arg(args, 1))?;
    if rt.native_id(c).is_some() {
        let saved = (rt.callee, rt.nt, rt.tail.take(), std::mem::take(&mut rt.tail_args));
        let r = rt.construct_native(c, &list, nt);
        (rt.callee, rt.nt, rt.tail, rt.tail_args) = saved;
        return r;
    }
    rt.construct_script(c, &list, nt)
}

fn define_property(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let o = target(rt, arg(args, 0), "defineProperty")?;
    let key = rt.key_of(arg(args, 1))?;
    let d = to_desc(rt, arg(args, 2))?;
    Ok(Value::bool(rt.define_own(o, key, &d)?))
}

fn delete_property(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let o = target(rt, arg(args, 0), "deleteProperty")?;
    let key = rt.key_of(arg(args, 1))?;
    Ok(Value::bool(rt.delete_prop(o.value(), key)?))
}

fn get(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let o = target(rt, arg(args, 0), "get")?;
    let key = rt.key_of(arg(args, 1))?;
    let recv = if args.len() > 2 { args[2] } else { o.value() };
    let mut p = Some(o);
    while let Some(c) = p {
        if c.is_kind(crate::object::K_TYPED)
            && let Some(i) = crate::bi_typed::numeric_key(key)
        {
            return Ok(i.and_then(|i| rt.typed_get(c, i)).unwrap_or(Value::UNDEFINED));
        }
        match rt.get_own(c, key) {
            Some(Own::Data(v, _)) => return Ok(v),
            Some(Own::Acc(g, ..)) => {
                if g.is_undefined() {
                    return Ok(Value::UNDEFINED);
                }
                rt.tail_args.clear();
                rt.tail = Some((g, recv));
                return Ok(Value::UNDEFINED);
            }
            None => p = rt.proto_of(c),
        }
    }
    Ok(Value::UNDEFINED)
}

fn get_own_property_descriptor(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let o = target(rt, arg(args, 0), "getOwnPropertyDescriptor")?;
    let key = rt.key_of(arg(args, 1))?;
    Ok(match rt.get_own(o, key) {
        Some(p) => rt.from_own(p),
        None => Value::UNDEFINED,
    })
}

fn get_prototype_of(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let o = target(rt, arg(args, 0), "getPrototypeOf")?;
    Ok(rt.proto_of(o).map_or(Value::NULL, |p| p.value()))
}

fn has(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let o = target(rt, arg(args, 0), "has")?;
    let key = rt.key_of(arg(args, 1))?;
    Ok(Value::bool(rt.has_prop(o.value(), key)?))
}

fn is_extensible(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    Ok(Value::bool(target(rt, arg(args, 0), "isExtensible")?.extensible()))
}

fn own_keys(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let o = target(rt, arg(args, 0), "ownKeys")?;
    let mut keys = rt.own_key_list(o, false);
    keys.extend(rt.own_symbol_list(o, false));
    Ok(rt.new_array(&keys))
}

fn prevent_extensions(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let o = target(rt, arg(args, 0), "preventExtensions")?;
    Ok(Value::bool(rt.prevent_extensions(o)))
}

/// `Reflect.set(target, key, v, receiver)`: `OrdinarySet`. The result is false where `Object.assign` and strict code would throw.
fn set(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let o = target(rt, arg(args, 0), "set")?;
    let key = rt.key_of(arg(args, 1))?;
    let (v, recv) = (arg(args, 2), if args.len() > 3 { args[3] } else { o.value() });
    let mut p = Some(o);
    while let Some(c) = p {
        if c.is_kind(crate::object::K_TYPED)
            && let Some(i) = crate::bi_typed::numeric_key(key)
        {
            if c.value() == recv {
                rt.typed_set(c, i, v)?;
                return Ok(Value::TRUE);
            }
            if !i.is_some_and(|i| i < rt.typed_len(c)) {
                return Ok(Value::TRUE);
            }
        }
        match rt.get_own(c, key) {
            Some(Own::Acc(_, s, _)) => {
                if s.is_undefined() {
                    return Ok(Value::FALSE);
                }
                rt.call_function(s, recv, &[v])?;
                return Ok(Value::TRUE);
            }
            Some(Own::Data(_, at)) => {
                if at & A_W == 0 {
                    return Ok(Value::FALSE);
                }
                break;
            }
            None => p = rt.proto_of(c),
        }
    }
    let Some(r) = Realm::obj(recv) else { return Ok(Value::FALSE) };
    let d = match rt.get_own(r, key) {
        Some(Own::Acc(..)) => return Ok(Value::FALSE),
        Some(Own::Data(_, at)) if at & A_W == 0 => return Ok(Value::FALSE),
        Some(Own::Data(..)) => Desc { value: Some(v), ..Desc::default() },
        None => Desc { value: Some(v), w: Some(true), e: Some(true), c: Some(true), ..Desc::default() },
    };
    Ok(Value::bool(rt.define_own(r, key, &d)?))
}

fn set_prototype_of(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let o = target(rt, arg(args, 0), "setPrototypeOf")?;
    let p = arg(args, 1);
    if Realm::obj(p).is_none() && !p.is_null() {
        return rt.type_error("object prototype may only be an object or null");
    }
    Ok(Value::bool(rt.set_proto_checked(o, Realm::obj(p))?))
}

impl Realm {
    pub(crate) fn install_reflect(&mut self) {
        let r = Obj::new(&mut self.heap, &self.shapes, self.s_object).expect("memory");
        static REFLECT: &[Ent] = &[
            m("apply", 3, apply),
            m("construct", 2, construct),
            m("defineProperty", 3, define_property),
            m("deleteProperty", 2, delete_property),
            m("get", 2, get),
            m("getOwnPropertyDescriptor", 2, get_own_property_descriptor),
            m("getPrototypeOf", 1, get_prototype_of),
            m("has", 2, has),
            m("isExtensible", 1, is_extensible),
            m("ownKeys", 1, own_keys),
            m("preventExtensions", 1, prevent_extensions),
            m("set", 3, set),
            m("setPrototypeOf", 2, set_prototype_of),
        ];
        self.lazy_table(r, REFLECT);
        let tag = Key::Name(self.wk[crate::bi_symbol::TO_STRING_TAG]);
        let name = self.string("Reflect");
        self.define_attrs(r, tag, A_C, name).expect("define");
        let g = self.global;
        self.hidden(g, "Reflect", r.value());
    }
}

#[cfg(test)]
#[path = "bi_reflect_tests.rs"]
mod tests;
