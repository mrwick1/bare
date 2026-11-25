//! Proxy objects use a dictionary shape. Only the internal slots are stored in the cell.
use crate::bi_object::{Desc, Own, new_property};
use crate::bi_typed::numeric_key;
use crate::builtins::{Native, arg};
use crate::interp_rt::{ErrKind, Key, R, Realm};
use crate::lazy::{Ent, ctor, m};
use crate::object::{F_CALLABLE, F_FNPROPS, F_NATIVE, K_PROXY, K_TYPED, Obj};
use crate::shapes::{A_C, A_E, A_W};
use crate::value::Value;
use std::collections::HashSet;

const TARGET: u32 = 1;
const HANDLER: u32 = 2;

pub(crate) const PROXY: Ent = ctor("Proxy", 2, proxy_ctor, &[m("revocable", 2, revocable)]);

impl Realm {
    fn make_proxy(&mut self, target: Value, handler: Value) -> R<Obj> {
        let Some(t) = Self::obj(target) else { return self.type_error("Proxy target must be an object") };
        if Self::obj(handler).is_none() {
            return self.type_error("Proxy handler must be an object");
        }
        let base = self.shapes.with_proto(self.s_native, None);
        let shape = self.shapes.to_dict(base);
        if self.shapes.cacheable(shape) {
            return Err(self.error(ErrKind::Range, "no space for a Proxy shape"));
        }
        let o = Obj::new(&mut self.heap, &self.shapes, shape).ok_or_else(|| self.error(ErrKind::Range, "out of memory"))?;
        o.set_kind(K_PROXY);
        o.set_slot(&self.shapes, TARGET, target);
        o.set_slot(&self.shapes, HANDLER, handler);
        if t.is_callable() {
            let ctor = self.is_constructor(target);
            let base = self.proxy_native_ids();
            o.set_slot(&self.shapes, 0, Value::int((base + ctor as usize) as i32));
            o.set_flag(F_CALLABLE | F_NATIVE | F_FNPROPS);
        }
        Ok(o)
    }

    fn proxy_native_ids(&mut self) -> usize {
        if let Some(base) = self.proxy_natives {
            return base;
        }
        let base = self.natives.len();
        self.natives.extend([
            Native { f: proxy_invoke, ctor: false, len: 0, name: "" },
            Native { f: proxy_invoke, ctor: true, len: 0, name: "" },
            Native { f: revoke, ctor: false, len: 0, name: "" },
        ]);
        self.proxy_natives = Some(base);
        base
    }

    fn proxy_target(&mut self, proxy: Obj) -> R<Obj> {
        Self::obj(proxy.slot(&self.shapes, TARGET)).ok_or_else(|| self.error(ErrKind::Type, "Proxy has been revoked"))
    }

    fn proxy_op<T>(&mut self, proxy: Obj, name: &str, run: impl FnOnce(&mut Self, Obj, Value, Option<Value>) -> R<T>) -> R<T> {
        if self.stack_full() {
            return Err(self.error(ErrKind::Range, "maximum call stack size exceeded"));
        }
        if self.fuel == 0 {
            return Err(crate::interp::STOP);
        }
        self.fuel -= 1;
        let target = self.proxy_target(proxy)?;
        let handler = proxy.slot(&self.shapes, HANDLER);
        self.proxy_depth += 1;
        let result = self.grow(|this| {
            let trap = this.get_named(handler, name)?;
            let trap = if trap.is_undefined() || trap.is_null() {
                None
            } else if Self::obj(trap).is_some_and(|o| o.is_callable()) {
                Some(trap)
            } else {
                return this.type_error("Proxy trap must be callable");
            };
            run(this, target, handler, trap)
        });
        self.proxy_depth -= 1;
        result
    }

    fn proxy_key(&mut self, key: Key) -> Value {
        match key {
            Key::Idx(i) => self.idx_atom(i).value(),
            Key::Name(k) => k.key_value(),
        }
    }

    pub(crate) fn get_from(&mut self, mut target: Obj, key: Key, receiver: Value) -> R<Value> {
        loop {
            if target.is_kind(K_PROXY) {
                return self.proxy_get(target, key, receiver);
            }
            if target.is_kind(K_TYPED) && let Some(i) = numeric_key(key) {
                return match i {
                    Some(i) => Ok(self.typed_get(target, i)?.unwrap_or(Value::UNDEFINED)),
                    None => Ok(Value::UNDEFINED),
                };
            }
            match self.get_own_h(target, key)? {
                Some(Own::Data(v, _)) => return Ok(v),
                Some(Own::Acc(g, ..)) if !g.is_undefined() => return self.call_function(g, receiver, &[]),
                Some(Own::Acc(..)) => return Ok(Value::UNDEFINED),
                None if matches!(key, Key::Name(k) if k == self.k_prototype) && self.needs_prototype(target) => return self.make_prototype(target),
                None => {}
            }
            let Some(proto) = self.proto_of(target) else { return Ok(Value::UNDEFINED) };
            target = proto;
        }
    }

    pub(crate) fn set_from(&mut self, mut target: Obj, key: Key, value: Value, receiver: Value) -> R<bool> {
        loop {
            if target.is_kind(K_PROXY) {
                return self.proxy_set(target, key, value, receiver);
            }
            if target.is_kind(K_TYPED) && let Some(i) = numeric_key(key) {
                if self.typed_immutable(target) {
                    return Ok(false);
                }
                if target.value() == receiver {
                    self.typed_set(target, i, value)?;
                    return Ok(true);
                }
                if !i.is_some_and(|i| i < self.typed_len(target)) {
                    return Ok(true);
                }
                break;
            }
            match self.get_own_h(target, key)? {
                Some(Own::Acc(_, s, _)) => {
                    if s.is_undefined() {
                        return Ok(false);
                    }
                    self.call_function(s, receiver, &[value])?;
                    return Ok(true);
                }
                Some(Own::Data(_, at)) if at & A_W == 0 => return Ok(false),
                Some(Own::Data(..)) => break,
                None => {}
            }
            let Some(proto) = self.proto_of(target) else { break };
            target = proto;
        }
        let Some(o) = Self::obj(receiver) else { return Ok(false) };
        let d = match self.get_own_h(o, key)? {
            Some(Own::Acc(..)) => return Ok(false),
            Some(Own::Data(_, at)) if at & A_W == 0 => return Ok(false),
            Some(Own::Data(..)) => Desc { value: Some(value), ..Desc::default() },
            None => Desc { value: Some(value), w: Some(true), e: Some(true), c: Some(true), ..Desc::default() },
        };
        self.define_own(o, key, &d)
    }

    pub(crate) fn all_own_keys(&mut self, target: Obj) -> R<Vec<Value>> {
        if target.is_kind(K_PROXY) {
            return self.proxy_keys(target);
        }
        let mut keys = self.own_key_list_h(target, false)?;
        keys.extend(self.own_symbol_list(target, false));
        Ok(keys)
    }

    pub(crate) fn proxy_pairs(&mut self, proxy: Obj, symbols: bool) -> R<Vec<(Value, Value)>> {
        let mut out = Vec::new();
        for k in self.proxy_keys(proxy)? {
            if !symbols && Self::sym_of(k).is_some() {
                continue;
            }
            let key = self.key_of(k)?;
            if self.get_own_h(proxy, key)?.is_some_and(|d| d.attrs() & A_E != 0) {
                out.push((k, self.get_prop(proxy.value(), key)?));
            }
        }
        Ok(out)
    }

    pub(crate) fn assign_proxy_source(&mut self, recv: Value, source: Obj) -> R<()> {
        for k in self.all_own_keys(source)? {
            let key = self.key_of(k)?;
            if self.get_own_h(source, key)?.is_some_and(|d| d.attrs() & A_E != 0) {
                let value = self.get_prop(source.value(), key)?;
                let target = Self::obj(recv).expect("assignment target");
                if !self.set_from(target, key, value, recv)? {
                    return self.type_error("cannot assign to property");
                }
            }
        }
        Ok(())
    }

    pub(crate) fn is_array_value(&mut self, mut v: Value) -> R<bool> {
        while let Some(o) = Self::obj(v) {
            if !o.is_kind(K_PROXY) {
                return Ok(self.is_array(o));
            }
            v = self.proxy_target(o)?.value();
        }
        Ok(false)
    }

    pub(crate) fn proxy_get(&mut self, proxy: Obj, key: Key, receiver: Value) -> R<Value> {
        self.proxy_op(proxy, "get", |rt, target, handler, trap| {
            let Some(trap) = trap else { return rt.get_from(target, key, receiver) };
            let k = rt.proxy_key(key);
            let v = rt.call_function(trap, handler, &[target.value(), k, receiver])?;
            let bad = match rt.get_own_h(target, key)? {
                Some(Own::Data(x, at)) if at & (A_C | A_W) == 0 => !rt.same_value(v, x),
                Some(Own::Acc(g, _, at)) if at & A_C == 0 && g.is_undefined() => !v.is_undefined(),
                _ => false,
            };
            if bad { return rt.type_error("Proxy get trap conflicts with a fixed property"); }
            Ok(v)
        })
    }

    pub(crate) fn proxy_set(&mut self, proxy: Obj, key: Key, value: Value, receiver: Value) -> R<bool> {
        self.proxy_op(proxy, "set", |rt, target, handler, trap| {
            let Some(trap) = trap else { return rt.set_from(target, key, value, receiver) };
            let k = rt.proxy_key(key);
            if !Self::truthy(rt.call_function(trap, handler, &[target.value(), k, value, receiver])?) {
                return Ok(false);
            }
            let bad = match rt.get_own_h(target, key)? {
                Some(Own::Data(x, at)) if at & (A_C | A_W) == 0 => !rt.same_value(value, x),
                Some(Own::Acc(_, s, at)) if at & A_C == 0 => s.is_undefined(),
                _ => false,
            };
            if bad { return rt.type_error("Proxy set trap conflicts with a fixed property"); }
            Ok(true)
        })
    }

    pub(crate) fn proxy_has(&mut self, proxy: Obj, key: Key) -> R<bool> {
        self.proxy_op(proxy, "has", |rt, target, handler, trap| {
            let Some(trap) = trap else { return rt.has_prop(target.value(), key) };
            let k = rt.proxy_key(key);
            let found = Self::truthy(rt.call_function(trap, handler, &[target.value(), k])?);
            if !found && let Some(d) = rt.get_own_h(target, key)?
                && (d.attrs() & A_C == 0 || !rt.is_extensible_internal(target)?)
            {
                return rt.type_error("Proxy has trap hid a fixed property");
            }
            Ok(found)
        })
    }

    pub(crate) fn proxy_delete(&mut self, proxy: Obj, key: Key) -> R<bool> {
        self.proxy_op(proxy, "deleteProperty", |rt, target, handler, trap| {
            let Some(trap) = trap else { return rt.delete_prop(target.value(), key) };
            let k = rt.proxy_key(key);
            let deleted = Self::truthy(rt.call_function(trap, handler, &[target.value(), k])?);
            if deleted && let Some(d) = rt.get_own_h(target, key)?
                && (d.attrs() & A_C == 0 || !rt.is_extensible_internal(target)?)
            {
                return rt.type_error("Proxy deleteProperty trap left a fixed property");
            }
            Ok(deleted)
        })
    }

    pub(crate) fn proxy_descriptor(&mut self, proxy: Obj, key: Key) -> R<Option<Own>> {
        self.proxy_op(proxy, "getOwnPropertyDescriptor", |rt, target, handler, trap| {
            let Some(trap) = trap else { return rt.get_own_h(target, key) };
            let k = rt.proxy_key(key);
            let v = rt.call_function(trap, handler, &[target.value(), k])?;
            if !v.is_undefined() && Self::obj(v).is_none() {
                return rt.type_error("Proxy descriptor must be an object or undefined");
            }
            let current = rt.get_own_h(target, key)?;
            if v.is_undefined() {
                if let Some(p) = current && (p.attrs() & A_C == 0 || !rt.is_extensible_internal(target)?) {
                    return rt.type_error("Proxy descriptor trap hid a fixed property");
                }
                return Ok(None);
            }
            let extensible = rt.is_extensible_internal(target)?;
            let mut d = crate::bi_reflect::to_desc(rt, v)?;
            if d.is_accessor() {
                d.get.get_or_insert(Value::UNDEFINED);
                d.set.get_or_insert(Value::UNDEFINED);
            } else {
                d.w.get_or_insert(false);
                d.value.get_or_insert(Value::UNDEFINED);
            }
            d.e.get_or_insert(false);
            d.c.get_or_insert(false);
            rt.check_proxy_descriptor(current, extensible, &d)?;
            let (attrs, values) = new_property(&d);
            Ok(Some(if d.is_accessor() { Own::Acc(values[0], values[1], attrs) } else { Own::Data(values[0], attrs) }))
        })
    }

    fn check_proxy_descriptor(&mut self, current: Option<Own>, extensible: bool, d: &Desc) -> R<()> {
        if current.is_none() && !extensible
            || d.c == Some(false) && current.is_none_or(|p| p.attrs() & A_C != 0)
            || current.is_some_and(|p| !self.compatible_property(p, d))
            || matches!(current, Some(Own::Data(_, at)) if at & (A_C | A_W) == A_W) && d.w == Some(false)
        {
            return self.type_error("Proxy descriptor conflicts with the target");
        }
        Ok(())
    }

    pub(crate) fn proxy_define(&mut self, proxy: Obj, key: Key, d: &Desc) -> R<bool> {
        self.proxy_op(proxy, "defineProperty", |rt, target, handler, trap| {
            let Some(trap) = trap else { return rt.define_own(target, key, d) };
            let k = rt.proxy_key(key);
            let obj = Obj::new(&mut rt.heap, &rt.shapes, rt.state.s_object).expect("memory");
            for (name, value) in [("value", d.value), ("writable", d.w.map(Value::bool)), ("get", d.get), ("set", d.set),
                ("enumerable", d.e.map(Value::bool)), ("configurable", d.c.map(Value::bool))]
            {
                if let Some(v) = value {
                    let key = Key::Name(rt.intern(name));
                    rt.define(obj, key, v)?;
                }
            }
            if !Self::truthy(rt.call_function(trap, handler, &[target.value(), k, obj.value()])?) {
                return Ok(false);
            }
            let current = rt.get_own_h(target, key)?;
            let extensible = rt.is_extensible_internal(target)?;
            rt.check_proxy_descriptor(current, extensible, d)?;
            Ok(true)
        })
    }

    pub(crate) fn proxy_keys(&mut self, proxy: Obj) -> R<Vec<Value>> {
        self.proxy_op(proxy, "ownKeys", |rt, target, handler, trap| {
            let Some(trap) = trap else { return rt.all_own_keys(target) };
            let result = rt.call_function(trap, handler, &[target.value()])?;
            let keys = crate::bi_reflect::list_from(rt, result)?;
            let mut seen = HashSet::with_capacity(keys.len());
            for &value in &keys {
                if Self::str_of(value).is_none() && Self::sym_of(value).is_none() {
                    return rt.type_error("Proxy ownKeys trap returned a key that is not a string or symbol");
                }
                if !seen.insert(rt.key_of(value)?.as_value().bits()) {
                    return rt.type_error("Proxy ownKeys trap returned duplicate keys");
                }
            }
            let extensible = rt.is_extensible_internal(target)?;
            for value in rt.all_own_keys(target)? {
                let key = rt.key_of(value)?;
                let fixed = rt.get_own_h(target, key)?.is_some_and(|p| p.attrs() & A_C == 0);
                if !seen.remove(&key.as_value().bits()) && (!extensible || fixed) {
                    return rt.type_error("Proxy ownKeys trap omitted a fixed key");
                }
            }
            if !extensible && !seen.is_empty() {
                return rt.type_error("Proxy ownKeys trap added keys to a fixed target");
            }
            Ok(keys)
        })
    }

    pub(crate) fn get_proto_internal(&mut self, obj: Obj) -> R<Option<Obj>> {
        if !obj.is_kind(K_PROXY) {
            return Ok(self.proto_of(obj));
        }
        self.proxy_op(obj, "getPrototypeOf", |rt, target, handler, trap| {
            let Some(trap) = trap else { return rt.get_proto_internal(target) };
            let v = rt.call_function(trap, handler, &[target.value()])?;
            let p = Self::obj(v);
            if p.is_none() && !v.is_null() || !rt.is_extensible_internal(target)? && rt.get_proto_internal(target)? != p {
                return rt.type_error("invalid Proxy prototype");
            }
            Ok(p)
        })
    }

    pub(crate) fn proxy_set_proto(&mut self, obj: Obj, proto: Option<Obj>) -> R<bool> {
        self.proxy_op(obj, "setPrototypeOf", |rt, target, handler, trap| {
            let Some(trap) = trap else { return rt.set_proto_checked(target, proto) };
            let v = proto.map_or(Value::NULL, Obj::value);
            let ok = Self::truthy(rt.call_function(trap, handler, &[target.value(), v])?);
            if ok && !rt.is_extensible_internal(target)? && rt.get_proto_internal(target)? != proto {
                return rt.type_error("Proxy setPrototypeOf trap conflicts with a fixed target");
            }
            Ok(ok)
        })
    }

    pub(crate) fn is_extensible_internal(&mut self, obj: Obj) -> R<bool> {
        if !obj.is_kind(K_PROXY) {
            return Ok(obj.extensible());
        }
        self.proxy_op(obj, "isExtensible", |rt, target, handler, trap| {
            let Some(trap) = trap else { return rt.is_extensible_internal(target) };
            let ok = Self::truthy(rt.call_function(trap, handler, &[target.value()])?);
            if ok != rt.is_extensible_internal(target)? {
                return rt.type_error("Proxy isExtensible trap conflicts with the target");
            }
            Ok(ok)
        })
    }

    pub(crate) fn prevent_extensions_internal(&mut self, obj: Obj) -> R<bool> {
        if obj.is_kind(K_PROXY) {
            return self.proxy_op(obj, "preventExtensions", |rt, target, handler, trap| {
                let Some(trap) = trap else { return rt.prevent_extensions_internal(target) };
                let ok = Self::truthy(rt.call_function(trap, handler, &[target.value()])?);
                if ok && rt.is_extensible_internal(target)? {
                    return rt.type_error("Proxy preventExtensions trap left the target extensible");
                }
                Ok(ok)
            });
        }
        if obj.is_kind(K_TYPED) && !crate::bi_buffer::View::of(obj).expect("typed array").is_fixed() {
            return Ok(false);
        }
        self.lazy_all(obj);
        if obj.extensible() {
            obj.prevent_extensions(&mut self.shapes);
        }
        Ok(true)
    }
}

fn proxy_ctor(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    if rt.nt.is_undefined() {
        return rt.type_error("Proxy must be constructed with new");
    }
    Ok(rt.make_proxy(arg(args, 0), arg(args, 1))?.value())
}

fn revocable(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let p = rt.make_proxy(arg(args, 0), arg(args, 1))?;
    let id = rt.proxy_native_ids() + 2;
    let revoke = rt.native_cell(id);
    revoke.set_slot(&rt.shapes, 1, p.value());
    let result = Obj::new(&mut rt.heap, &rt.shapes, rt.state.s_object).expect("memory");
    for (name, value) in [("proxy", p.value()), ("revoke", revoke.value())] {
        let key = Key::Name(rt.intern(name));
        rt.define(result, key, value)?;
    }
    Ok(result.value())
}

fn revoke(rt: &mut Realm, _this: Value, _args: &[Value]) -> R<Value> {
    let f = Realm::obj(rt.callee).expect("revoker");
    if let Some(p) = Realm::obj(f.slot(&rt.shapes, 1)) {
        f.set_slot(&rt.shapes, 1, Value::UNDEFINED);
        p.set_slot(&rt.shapes, TARGET, Value::UNDEFINED);
        p.set_slot(&rt.shapes, HANDLER, Value::UNDEFINED);
    }
    Ok(Value::UNDEFINED)
}

fn proxy_invoke(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let proxy = Realm::obj(rt.callee).expect("Proxy");
    let nt = rt.nt;
    if nt.is_undefined() {
        return rt.proxy_op(proxy, "apply", |rt, target, handler, trap| {
            let Some(trap) = trap else { return rt.call_function(target.value(), this, args) };
            let args = rt.new_array(args);
            rt.call_function(trap, handler, &[target.value(), this, args])
        });
    }
    rt.proxy_op(proxy, "construct", |rt, target, handler, trap| {
        let Some(trap) = trap else {
            return if rt.native_id(target.value()).is_some() {
                rt.construct_native(target.value(), args, nt)
            } else {
                rt.construct_script(target.value(), args, nt)
            };
        };
        let args = rt.new_array(args);
        let v = rt.call_function(trap, handler, &[target.value(), args, nt])?;
        if Realm::obj(v).is_none() {
            return rt.type_error("Proxy construct trap returned a non-object");
        }
        Ok(v)
    })
}
