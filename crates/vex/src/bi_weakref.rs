//! `WeakRef` and `FinalizationRegistry`. Both globals are lazy (`lazy.rs`, `factory`): a page that never names them pays nothing.
//!
//! **`WeakRef`.** A plain `Object` cell of the kind [`K_WEAKREF`](crate::object::K_WEAKREF) with one hidden property, `Realm::k_prim`,
//! that holds a [`WeakTab`] with one entry: the target is the key and the value. The collector marks a value only while its key is
//! marked, so the entry does not keep the target, and it clears the entry when the target dies (`weaktab.rs`). `deref` reads the key
//! of the live entry.
//!
//! **`FinalizationRegistry`.** The spec lets an engine never call the cleanup callback, and Vex never does. So `register` keeps neither
//! the target nor the held value. The only state is for `unregister`: a `WeakTab` keyed by the unregister token, with the number of
//! registrations of that token as the value. It holds the token weakly, so it is bounded by the live tokens.
//!
//! Gaps: a target must be an object (as for `WeakMap` keys; a symbol throws a `TypeError`), and the spec's KeepDuringJob is not done
//! (a target that only a `WeakRef` names may be collected inside the job that made it).
use crate::bi_buffer::{instance_proto, make_ctor, plain_proto, tag};
use crate::builtins::{HIDDEN, NativeFn, arg};
use crate::cell::Layout;
use crate::interp_rt::{Key, R, Realm};
use crate::lazy::{Ent, factory};
use crate::object::{K_FINREG, K_WEAKREF, Obj};
use crate::value::Value;
use crate::weaktab::{WeakTab, weaktab_of};

pub(crate) static GLOBALS: &[Ent] = &[factory("WeakRef", weakref), factory("FinalizationRegistry", registry)];

fn method(rt: &mut Realm, proto: Obj, name: &'static str, len: u8, f: NativeFn) {
    let key = Key::Name(rt.intern(name));
    let m = rt.new_native(f, false, len, name);
    rt.define_attrs(proto, key, HIDDEN, m.value()).expect("define");
}

fn weakref(rt: &mut Realm) -> Value {
    let proto = plain_proto(rt, rt.object_proto);
    tag(rt, proto, "WeakRef");
    method(rt, proto, "deref", 0, deref);
    make_ctor(rt, "WeakRef", 1, weakref_ctor, proto).value()
}

fn registry(rt: &mut Realm) -> Value {
    let proto = plain_proto(rt, rt.object_proto);
    tag(rt, proto, "FinalizationRegistry");
    method(rt, proto, "register", 2, register);
    method(rt, proto, "unregister", 1, unregister);
    make_ctor(rt, "FinalizationRegistry", 1, registry_ctor, proto).value()
}

/// A new object of `kind` with prototype `proto` and `v` in its one hidden slot.
fn holder(rt: &mut Realm, proto: Obj, kind: u32, v: Value) -> Obj {
    let root = rt.shapes.root(Layout::Object, Some(proto.cell()), 4);
    let shape = rt.shapes.add(root, rt.k_prim, 0).expect("shape");
    let o = Obj::new(&mut rt.heap, &rt.shapes, shape).expect("memory");
    o.set_slot(&rt.shapes, 0, v);
    o.set_kind(kind);
    o
}

fn brand(rt: &mut Realm, this: Value, kind: u32, what: &str) -> R<Obj> {
    match Realm::obj(this).filter(|o| o.is_kind(kind)) {
        Some(o) => Ok(o),
        None => rt.type_error(&format!("Method {what} called on incompatible receiver")),
    }
}

fn weakref_ctor(rt: &mut Realm, _: Value, args: &[Value]) -> R<Value> {
    let proto = instance_proto(rt)?;
    let target = arg(args, 0);
    if Realm::obj(target).is_none() {
        return rt.type_error("WeakRef: invalid target");
    }
    let t = WeakTab::new(&mut rt.heap, 0).expect("memory");
    t.add(target, target);
    Ok(holder(rt, proto, K_WEAKREF, t.value()).value())
}

fn deref(rt: &mut Realm, this: Value, _: &[Value]) -> R<Value> {
    let o = brand(rt, this, K_WEAKREF, "WeakRef.prototype.deref")?;
    Ok(weaktab_of(o.slot(&rt.shapes, 0)).and_then(WeakTab::first_key).unwrap_or(Value::UNDEFINED))
}

fn registry_ctor(rt: &mut Realm, _: Value, args: &[Value]) -> R<Value> {
    let proto = instance_proto(rt)?;
    if !Realm::obj(arg(args, 0)).is_some_and(|f| f.is_callable()) {
        return rt.type_error("FinalizationRegistry: cleanup must be callable");
    }
    Ok(holder(rt, proto, K_FINREG, Value::UNDEFINED).value())
}

fn register(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let o = brand(rt, this, K_FINREG, "FinalizationRegistry.prototype.register")?;
    let (target, held, token) = (arg(args, 0), arg(args, 1), arg(args, 2));
    if Realm::obj(target).is_none() {
        return rt.type_error("FinalizationRegistry.prototype.register: invalid target");
    }
    if target == held {
        return rt.type_error("FinalizationRegistry.prototype.register: target and holdings must not be the same");
    }
    if Realm::obj(token).is_none() {
        if token.is_undefined() {
            return Ok(Value::UNDEFINED);
        }
        return rt.type_error("FinalizationRegistry.prototype.register: invalid unregister token");
    }
    let mut t = match weaktab_of(o.slot(&rt.shapes, 0)) {
        Some(t) => t,
        None => WeakTab::new(&mut rt.heap, 0).expect("memory"),
    };
    match t.find(token) {
        Some(i) => t.set_val(i, Value::int(t.val(i).as_int().unwrap_or(0).saturating_add(1))),
        None => {
            if t.full() {
                t = t.grown(&mut rt.heap).expect("memory");
            }
            t.add(token, Value::int(1));
        }
    }
    o.set_slot(&rt.shapes, 0, t.value());
    Ok(Value::UNDEFINED)
}

fn unregister(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let o = brand(rt, this, K_FINREG, "FinalizationRegistry.prototype.unregister")?;
    let token = arg(args, 0);
    if Realm::obj(token).is_none() {
        return rt.type_error("FinalizationRegistry.prototype.unregister: invalid unregister token");
    }
    let hit = weaktab_of(o.slot(&rt.shapes, 0)).and_then(|t| t.find(token).map(|i| (t, i)));
    hit.iter().for_each(|(t, i)| t.remove(*i));
    Ok(Value::bool(hit.is_some()))
}

#[cfg(test)]
#[path = "bi_weakref_tests.rs"]
mod tests;
