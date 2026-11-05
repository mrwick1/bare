// The Object, Object.prototype, Reflect and groupBy built-ins of batch 1. Included by `eval.rs` (`include!`), so it shares that
// module's imports and private items. The natives are `Nat::Refl(ReflFn::..)`; `call_refl` (in `eval_reflect.rs`) forwards the
// new ones to `call_object_more`.
//
// Supported
// - `Object`: getOwnPropertyDescriptors, getOwnPropertySymbols, hasOwn, is, preventExtensions, seal, freeze, isExtensible,
//   isFrozen, isSealed, defineProperties, setPrototypeOf, groupBy. `Map.groupBy`.
// - `Object.prototype`: isPrototypeOf, propertyIsEnumerable, toLocaleString, the `__proto__` accessor, `__defineGetter__`,
//   `__defineSetter__`, `__lookupGetter__`, `__lookupSetter__`.
// - `Reflect`: apply, defineProperty, deleteProperty, get, getOwnPropertyDescriptor, getPrototypeOf, has, isExtensible,
//   preventExtensions, set, setPrototypeOf. `get` and `set` take a receiver.
// - `seal` and `freeze` follow SetIntegrityLevel in one pass over the own properties (`Heap::set_integrity`): the object is
//   not extensible, every property is not configurable, and with `freeze` every data property is not writable. A frozen array
//   rejects writes, `push`, `pop`, `length` changes, and, in strict code, throws. `isSealed` and `isFrozen` are
//   TestIntegrityLevel (`Heap::test_integrity`).
// - `setPrototypeOf` and the `__proto__` setter use `Heap::set_prototype_of`. A cycle, a non-extensible object and
//   `Object.prototype` (an immutable prototype object) each have V8's message.
// - `groupBy` runs the shared iterator machinery: the iterator is closed when the callback or the key conversion throws.
//
// Primitives: a primitive argument is not boxed. A string argument shows its indices and `length` (`getOwnPropertyDescriptors`,
// `propertyIsEnumerable`, `hasOwn`); other primitives show no own keys and their prototype chain.
//
// Known differences: `Object.seal` of a byte view that has elements succeeds, as in V8 (the specification says it throws,
// because the elements are configurable). `Object.freeze` of such a view throws, as both do. `Reflect.set` with a receiver
// other than the target, on an integer index of a byte view, is `Unsupported` (TypedArray `[[Set]]` has its own rule).

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum GKey {
    P(PropertyKey),
    V(HKey),
}

/// Why a prototype change failed.
enum ProtoFail {
    Immutable,
    NotExtensible,
    Cyclic,
}

impl<'p> Interp<'p> {
    fn install_object_more(&mut self, oc: ObjId, op: ObjId, r: ObjId) -> Res<()> {
        use ReflFn::*;
        for (name, f, len) in [
            ("getOwnPropertyDescriptors", ObjGetOwnPropertyDescriptors, 1), ("getOwnPropertySymbols", ObjGetOwnPropertySymbols, 1),
            ("hasOwn", ObjHasOwn, 2), ("is", ObjIs, 2), ("preventExtensions", ObjPreventExtensions, 1), ("seal", ObjSeal, 1),
            ("freeze", ObjFreeze, 1), ("isExtensible", ObjIsExtensible, 1), ("isFrozen", ObjIsFrozen, 1), ("isSealed", ObjIsSealed, 1),
            ("defineProperties", ObjDefineProperties, 2), ("setPrototypeOf", ObjSetPrototypeOf, 2), ("groupBy", ObjGroupBy, 2),
        ] {
            self.method(oc, name, Nat::Refl(f), len)?;
        }
        for (name, f, len) in [
            ("isPrototypeOf", ProtoIsPrototypeOf, 1), ("propertyIsEnumerable", ProtoPropertyIsEnumerable, 1), ("toLocaleString", ProtoToLocaleString, 0),
            ("__defineGetter__", ProtoDefineGetter, 2), ("__defineSetter__", ProtoDefineSetter, 2), ("__lookupGetter__", ProtoLookupGetter, 1),
            ("__lookupSetter__", ProtoLookupSetter, 1),
        ] {
            self.method(op, name, Nat::Refl(f), len)?;
        }
        let (g, s) = (self.native(Nat::Refl(ProtoGet), "get __proto__", 0)?, self.native(Nat::Refl(ProtoSet), "set __proto__", 1)?);
        let d = PropertyDescriptor { get: Some(Some(g)), set: Some(Some(s)), enumerable: Some(false), configurable: Some(true), ..Default::default() };
        hxw!(self, self.heap.define_own_property(op, self.k_proto, &d));
        for (name, f, len) in [
            ("apply", RApply, 3), ("defineProperty", RDefineProperty, 3), ("deleteProperty", RDeleteProperty, 2), ("get", RGet, 2),
            ("getOwnPropertyDescriptor", RGetOwnPropertyDescriptor, 2), ("getPrototypeOf", RGetPrototypeOf, 1), ("has", RHas, 2),
            ("isExtensible", RIsExtensible, 1), ("preventExtensions", RPreventExtensions, 1), ("set", RSet, 3), ("setPrototypeOf", RSetPrototypeOf, 2),
        ] {
            self.method(r, name, Nat::Refl(f), len)?;
        }
        let mk = h!(self, self.heap.key_from_str("Map"));
        if let Some(Property::Data { value: Value::Object(mc), .. }) = h!(self, self.heap.get_own_property(self.global, mk)) {
            self.method(mc, "groupBy", Nat::Refl(MapGroupBy), 2)?;
        }
        Ok(())
    }

    fn call_object_more(&mut self, f: ReflFn, this: Value, args: &[Value]) -> Res<Value> {
        use ReflFn::*;
        let (a0, a1, a2) = (arg(args, 0), arg(args, 1), arg(args, 2));
        match f {
            ObjGetOwnPropertyDescriptors => self.ro_descriptors(a0),
            ObjGetOwnPropertySymbols => {
                let keys = self.ro_keys(a0)?;
                let mut out = Vec::new();
                for k in keys {
                    if matches!(k, PropertyKey::Symbol(_)) {
                        out.push(h!(self, self.heap.key_to_value(k)));
                    }
                }
                self.new_array_from(&out)
            }
            ObjHasOwn => {
                self.ao_cvt(a0)?;
                let key = self.coerce_key(a1)?;
                Ok(Value::Bool(self.ro_has_own(a0, key)?))
            }
            ObjIs => Ok(Value::Bool(a0.same_value(a1))),
            ObjPreventExtensions => {
                if let Value::Object(o) = a0
                    && !self.o_prevent_extensions(o)?
                {
                    return Err(self.type_err("'preventExtensions' on proxy: trap returned falsish"));
                }
                Ok(a0)
            }
            ObjSeal | ObjFreeze => self.ro_integrity(a0, f == ObjFreeze),
            ObjIsExtensible => Ok(Value::Bool(match a0 {
                Value::Object(o) => self.o_is_extensible(o)?,
                _ => false,
            })),
            ObjIsFrozen | ObjIsSealed => Ok(Value::Bool(match a0 {
                Value::Object(o) => hx!(self, self.heap.test_integrity(o, f == ObjIsFrozen), self.px_test_integrity(o, f == ObjIsFrozen)?),
                _ => true,
            })),
            ObjDefineProperties => {
                let Value::Object(o) = a0 else { return Err(self.type_err("Object.defineProperties called on non-object")) };
                self.ro_define_properties(o, a1)?;
                Ok(a0)
            }
            ObjSetPrototypeOf => {
                if is_nullish(a0) {
                    return Err(self.type_err("Object.setPrototypeOf called on null or undefined"));
                }
                let proto = self.ro_proto_arg(a1)?;
                if let Value::Object(o) = a0
                    && let Some(why) = self.ro_set_proto(o, proto)?
                {
                    return Err(self.ro_proto_err(o, why));
                }
                Ok(a0)
            }
            ObjGroupBy => self.ro_group_by(a0, a1, false),
            MapGroupBy => self.ro_group_by(a0, a1, true),
            ProtoIsPrototypeOf => {
                let Value::Object(mut v) = a0 else { return Ok(Value::Bool(false)) };
                self.ao_cvt(this)?;
                let Value::Object(o) = this else { return Ok(Value::Bool(false)) };
                while let Some(p) = self.o_get_prototype_of(v)? {
                    self.tick()?;
                    if p == o {
                        return Ok(Value::Bool(true));
                    }
                    v = p;
                }
                Ok(Value::Bool(false))
            }
            ProtoPropertyIsEnumerable => {
                let key = self.coerce_key(a0)?;
                self.ao_cvt(this)?;
                Ok(Value::Bool(match this {
                    Value::Object(o) => matches!(self.o_get_own_property(o, key)?, Some(p) if p.enumerable()),
                    Value::String(s) => matches!(key, PropertyKey::Index(i) if (i as usize) < h!(self, self.heap.str_len(s))),
                    _ => false,
                }))
            }
            ProtoToLocaleString => {
                if is_nullish(this) {
                    return Err(self.type_err("Object.prototype.toLocaleString called on null or undefined"));
                }
                let m = self.get_prop(this, self.k_to_string)?;
                if !self.is_callable(m) {
                    return Err(self.ao_not_fn(m));
                }
                self.call_fn(m, this, &[])
            }
            ProtoGet => {
                self.ao_cvt(this)?;
                self.ro_proto_of(this)
            }
            ProtoSet => {
                if is_nullish(this) {
                    return Err(self.type_err("set Object.prototype.__proto__ called on null or undefined"));
                }
                let (Value::Object(o), true) = (this, matches!(a0, Value::Object(_) | Value::Null)) else { return Ok(Value::Undefined) };
                let proto = self.ro_proto_arg(a0)?;
                if let Some(why) = self.ro_set_proto(o, proto)? {
                    return Err(self.ro_proto_err(o, why));
                }
                Ok(Value::Undefined)
            }
            ProtoDefineGetter | ProtoDefineSetter => {
                self.ao_cvt(this)?;
                let getter = f == ProtoDefineGetter;
                let Value::Object(h) = a1 else { return Err(self.ro_expecting(getter)) };
                if !self.is_callable(a1) {
                    return Err(self.ro_expecting(getter));
                }
                let key = self.coerce_key(a0)?;
                let d = PropertyDescriptor {
                    get: getter.then_some(Some(h)),
                    set: (!getter).then_some(Some(h)),
                    enumerable: Some(true),
                    configurable: Some(true),
                    ..Default::default()
                };
                if let Value::Object(o) = this {
                    self.ro_define(o, key, &d)?;
                }
                Ok(Value::Undefined)
            }
            ProtoLookupGetter | ProtoLookupSetter => {
                self.ao_cvt(this)?;
                let key = self.coerce_key(a0)?;
                let start = match this {
                    Value::Object(o) => o,
                    v => self.ro_prim_proto(v),
                };
                // a proxy on the chain: the loop of the specification (own property, then [[GetPrototypeOf]])
                let found = hx!(self, self.heap.lookup(start, key), {
                    let (mut cur, mut found) = (Some(start), None);
                    while let Some(c) = cur {
                        self.tick()?;
                        if let Some(p) = self.o_get_own_property(c, key)? {
                            found = Some((c, p));
                            break;
                        }
                        cur = self.o_get_prototype_of(c)?;
                    }
                    found
                });
                Ok(match found {
                    Some((_, Property::Accessor { get, set, .. })) => (if f == ProtoLookupGetter { get } else { set }).map_or(Value::Undefined, Value::Object),
                    _ => Value::Undefined,
                })
            }
            RApply => {
                if !self.is_callable(a0) {
                    let (t, ty) = (self.ro_text(a0), self.ro_type_phrase(a0));
                    return Err(self.type_err(&format!("Function.prototype.apply was called on {t}, which is {ty} and not a function")));
                }
                if !matches!(a2, Value::Object(_)) {
                    return Err(self.type_err("CreateListFromArrayLike called on non-object"));
                }
                let list = self.array_like(a2)?;
                self.call_fn(a0, a1, &list)
            }
            RDefineProperty => {
                let o = self.ro_target(a0, "defineProperty")?;
                let key = self.coerce_key(a1)?;
                let d = self.property_descriptor(a2)?;
                Ok(Value::Bool(self.o_define_own_property(o, key, &d)?))
            }
            RDeleteProperty => {
                let o = self.ro_target(a0, "deleteProperty")?;
                let key = self.coerce_key(a1)?;
                Ok(Value::Bool(self.o_delete(o, key)?))
            }
            RGet => {
                let o = self.ro_target(a0, "get")?;
                let key = self.coerce_key(a1)?;
                let receiver = if args.len() > 2 { a2 } else { a0 };
                self.o_get(o, key, receiver)
            }
            RGetOwnPropertyDescriptor => {
                let o = self.ro_target(a0, "getOwnPropertyDescriptor")?;
                let key = self.coerce_key(a1)?;
                match self.o_get_own_property(o, key)? {
                    Some(p) => self.ro_descriptor_object(p),
                    None => Ok(Value::Undefined),
                }
            }
            RGetPrototypeOf => {
                let o = self.ro_target(a0, "getPrototypeOf")?;
                Ok(self.o_get_prototype_of(o)?.map_or(Value::Null, Value::Object))
            }
            RHas => {
                let o = self.ro_target(a0, "has")?;
                let key = self.coerce_key(a1)?;
                Ok(Value::Bool(self.o_has_property(o, key)?))
            }
            RIsExtensible => {
                let o = self.ro_target(a0, "isExtensible")?;
                Ok(Value::Bool(self.o_is_extensible(o)?))
            }
            RPreventExtensions => {
                let o = self.ro_target(a0, "preventExtensions")?;
                Ok(Value::Bool(self.o_prevent_extensions(o)?))
            }
            RSet => {
                let o = self.ro_target(a0, "set")?;
                let key = self.coerce_key(a1)?;
                let receiver = if args.len() > 3 { args[3] } else { a0 };
                Ok(Value::Bool(self.ro_reflect_set(o, key, a2, receiver)?))
            }
            RSetPrototypeOf => {
                let o = self.ro_target(a0, "setPrototypeOf")?;
                let proto = self.ro_proto_arg(a1)?;
                Ok(Value::Bool(self.ro_set_proto(o, proto)?.is_none()))
            }
            _ => unreachable!("call_refl handles the other natives"),
        }
    }

    // ------------------------------------------------------------ helpers

    /// V8's text of a value in a message: `#<Object>` for an object, `[object Array]` for an array.
    fn ro_text(&mut self, v: Value) -> String {
        match v {
            Value::Object(o) if !self.funcs.contains_key(&o) => self.ao_sort_text(v),
            _ => self.display(v),
        }
    }

    /// "a number", "an object", "a undefined" and "null": the words V8 puts after "which is".
    fn ro_type_phrase(&self, v: Value) -> String {
        match v {
            Value::Null => "null".to_string(),
            Value::Object(_) if !self.is_callable(v) => "an object".to_string(),
            _ => format!("a {}", self.type_of(v)),
        }
    }

    /// The target of a Reflect function: an object, or a TypeError.
    fn ro_target(&mut self, v: Value, name: &str) -> Res<ObjId> {
        match v {
            Value::Object(o) => Ok(o),
            _ => Err(self.type_err(&format!("Reflect.{name} called on non-object"))),
        }
    }

    fn ro_expecting(&mut self, getter: bool) -> Error {
        self.type_err(&format!("Object.prototype.__define{}__: Expecting function", if getter { "Getter" } else { "Setter" }))
    }

    /// The prototype that a primitive has: what `Object.getPrototypeOf` gives for it.
    fn ro_prim_proto(&self, v: Value) -> ObjId {
        match v {
            Value::String(_) => self.ext.string_proto,
            Value::Symbol(_) => self.sc.symbol_proto,
            Value::Number(_) => self.ao.number_proto.unwrap_or(self.object_proto),
            _ => self.object_proto,
        }
    }

    /// `Object.getPrototypeOf(v)`.
    fn ro_proto_of(&mut self, v: Value) -> Res<Value> {
        match v {
            Value::Object(o) => Ok(self.o_get_prototype_of(o)?.map_or(Value::Null, Value::Object)),
            Value::Undefined | Value::Null => Err(self.type_err("Cannot convert undefined or null to object")),
            _ => Ok(Value::Object(self.ro_prim_proto(v))),
        }
    }

    /// The prototype argument of `setPrototypeOf`: an object or `null`.
    fn ro_proto_arg(&mut self, v: Value) -> Res<Option<ObjId>> {
        match v {
            Value::Object(p) => Ok(Some(p)),
            Value::Null => Ok(None),
            _ => {
                let t = self.ro_text(v);
                Err(self.type_err(&format!("Object prototype may only be an Object or null: {t}")))
            }
        }
    }

    /// `o.[[SetPrototypeOf]](proto)`: `None` when it worked.
    fn ro_set_proto(&mut self, o: ObjId, proto: Option<ObjId>) -> Res<Option<ProtoFail>> {
        if o == self.object_proto {
            return Ok((self.o_get_prototype_of(o)? != proto).then_some(ProtoFail::Immutable));
        }
        if self.o_set_prototype_of(o, proto)? {
            return Ok(None);
        }
        Ok(Some(if self.o_is_extensible(o)? { ProtoFail::Cyclic } else { ProtoFail::NotExtensible }))
    }

    fn ro_proto_err(&mut self, o: ObjId, why: ProtoFail) -> Error {
        match why {
            ProtoFail::Immutable => self.type_err("Immutable prototype object 'Object.prototype' cannot have their prototype set"),
            ProtoFail::Cyclic => self.type_err("Cyclic __proto__ value"),
            ProtoFail::NotExtensible => {
                let t = self.ro_text(Value::Object(o));
                self.type_err(&format!("{t} is not extensible"))
            }
        }
    }

    /// `DefinePropertyOrThrow`.
    fn ro_define(&mut self, o: ObjId, key: PropertyKey, d: &PropertyDescriptor) -> Res<()> {
        if self.o_define_own_property(o, key, d)? {
            return Ok(());
        }
        if self.heap.is_proxy(o) {
            let k = self.key_text(key);
            return Err(self.type_err(&format!("'defineProperty' on proxy: trap returned falsish for property '{k}'")));
        }
        if self.heap.is_view_bad_index(o, key) {
            return Err(self.type_err("Invalid typed array index"));
        }
        let k = self.key_text(key);
        if !self.o_is_extensible(o)? && !self.o_has_own(o, key)? {
            return Err(self.type_err(&format!("Cannot define property {k}, object is not extensible")));
        }
        Err(self.type_err(&format!("Cannot redefine property: {k}")))
    }

    /// `HasOwnProperty` of a value that `ToObject` would accept.
    fn ro_has_own(&mut self, v: Value, key: PropertyKey) -> Res<bool> {
        Ok(match v {
            Value::Object(o) => self.o_has_own(o, key)?,
            Value::String(s) => key == self.k_length || matches!(key, PropertyKey::Index(i) if (i as usize) < h!(self, self.heap.str_len(s))),
            _ => false,
        })
    }

    /// The own keys of `ToObject(v)`, charged before the key list exists. A string has its indices and `length`.
    fn ro_keys(&mut self, v: Value) -> Res<Vec<PropertyKey>> {
        match v {
            Value::Object(o) => self.property_keys(o),
            Value::String(s) => {
                let len = h!(self, self.heap.str_len(s));
                self.spend(len as u64)?;
                self.ao_temp(len + 1, 16)?;
                let mut keys: Vec<PropertyKey> = (0..len as u32).map(PropertyKey::Index).collect();
                keys.push(self.k_length);
                Ok(keys)
            }
            Value::Undefined | Value::Null => Err(self.type_err("Cannot convert undefined or null to object")),
            _ => Ok(Vec::new()),
        }
    }

    /// The property of a string wrapper: a character, or `length`.
    fn ro_string_own(&mut self, s: StrId, key: PropertyKey) -> Res<Option<Property>> {
        let len = h!(self, self.heap.str_len(s));
        Ok(match key {
            PropertyKey::Index(i) if (i as usize) < len => {
                let c = h!(self, self.heap.substring(s, i as usize, i as usize + 1));
                Some(Property::Data { value: Value::String(c), writable: false, enumerable: true, configurable: false })
            }
            k if k == self.k_length => Some(Property::Data { value: Value::Number(len as f64), writable: false, enumerable: false, configurable: false }),
            _ => None,
        })
    }

    /// `FromPropertyDescriptor`: the object `Object.getOwnPropertyDescriptor` returns.
    fn ro_descriptor_object(&mut self, p: Property) -> Res<Value> {
        let r = h!(self, self.heap.new_object(Some(self.object_proto)));
        let (k_value, k_writable, k_get, k_set, k_enum, k_conf) = (self.k_value, self.k_writable, self.k_get, self.k_set, self.k_enumerable, self.k_configurable);
        match p {
            Property::Data { value, writable, enumerable, configurable } => {
                self.def(r, k_value, value, true, true, true)?;
                self.def(r, k_writable, Value::Bool(writable), true, true, true)?;
                self.def(r, k_enum, Value::Bool(enumerable), true, true, true)?;
                self.def(r, k_conf, Value::Bool(configurable), true, true, true)?;
            }
            Property::Accessor { get, set, enumerable, configurable } => {
                self.def(r, k_get, get.map_or(Value::Undefined, Value::Object), true, true, true)?;
                self.def(r, k_set, set.map_or(Value::Undefined, Value::Object), true, true, true)?;
                self.def(r, k_enum, Value::Bool(enumerable), true, true, true)?;
                self.def(r, k_conf, Value::Bool(configurable), true, true, true)?;
            }
        }
        Ok(Value::Object(r))
    }

    fn ro_descriptors(&mut self, v: Value) -> Res<Value> {
        let keys = self.ro_keys(v)?;
        let out = h!(self, self.heap.new_object(Some(self.object_proto)));
        for k in keys {
            self.tick()?;
            let p = match v {
                Value::Object(o) => self.o_get_own_property(o, k)?,
                Value::String(s) => self.ro_string_own(s, k)?,
                _ => None,
            };
            if let Some(p) = p {
                let d = self.ro_descriptor_object(p)?;
                self.def(out, k, d, true, true, true)?;
            }
        }
        Ok(Value::Object(out))
    }

    /// `Object.seal` and `Object.freeze`. A non-object is returned as it is.
    fn ro_integrity(&mut self, v: Value, frozen: bool) -> Res<Value> {
        let Value::Object(o) = v else { return Ok(v) };
        let slots = hx!(self, self.heap.own_key_slots(o)) as u64;
        self.ao_reserve(slots / 8)?;
        if !hx!(self, self.heap.set_integrity(o, frozen), self.px_set_integrity(o, frozen)?) {
            return Err(self.type_err("Cannot freeze array buffer views with elements"));
        }
        self.spend(slots / 8 + 1)?;
        Ok(v)
    }

    /// `ObjectDefineProperties`: every descriptor is read before one is applied.
    fn ro_define_properties(&mut self, o: ObjId, props: Value) -> Res<()> {
        let keys = match props {
            Value::String(s) => self.ro_keys(Value::String(s))?.into_iter().filter(|&k| k != self.k_length).collect(),
            v => self.ro_keys(v)?,
        };
        self.ao_temp(keys.len(), 80)?;
        let mut descs: Vec<(PropertyKey, PropertyDescriptor)> = Vec::new();
        for k in keys {
            self.tick()?;
            let enumerable = match props {
                Value::Object(p) => matches!(self.o_get_own_property(p, k)?, Some(x) if x.enumerable()),
                _ => true,
            };
            if enumerable {
                let d = self.get_prop(props, k)?;
                descs.push((k, self.property_descriptor(d)?));
            }
        }
        for (k, d) in descs {
            self.ro_define(o, k, &d)?;
        }
        Ok(())
    }

    /// `Reflect.set`.
    fn ro_reflect_set(&mut self, o: ObjId, key: PropertyKey, mut v: Value, receiver: Value) -> Res<bool> {
        let view = self.heap.has_exotics() && self.heap.is_view_index_key(o, key);
        if matches!(receiver, Value::Object(r) if r == o) {
            if view && matches!(v, Value::Object(_)) {
                v = Value::Number(self.coerce_number(v)?);
            }
            return Ok(match hxw!(self, self.heap.set(o, key, v), if self.o_set(o, key, v, receiver)? { Set::Done } else { Set::Rejected }) {
                Set::Done => true,
                Set::Rejected => false,
                Set::Setter(s) => {
                    self.call_fn(Value::Object(s), receiver, &[v])?;
                    true
                }
            });
        }
        if view {
            return Err(unsupported("Reflect.set on an integer index of a byte view with another receiver", Span::default()));
        }
        match hx!(self, self.heap.lookup(o, key), return self.o_set(o, key, v, receiver)) {
            Some((_, Property::Accessor { set: Some(s), .. })) => {
                self.call_fn(Value::Object(s), receiver, &[v])?;
                Ok(true)
            }
            Some((_, Property::Accessor { set: None, .. } | Property::Data { writable: false, .. })) => Ok(false),
            _ => {
                let Value::Object(r) = receiver else { return Ok(false) };
                Ok(match self.o_get_own_property(r, key)? {
                    Some(Property::Accessor { .. } | Property::Data { writable: false, .. }) => false,
                    Some(Property::Data { .. }) => self.o_define_own_property(r, key, &PropertyDescriptor { value: Some(v), ..Default::default() })?,
                    None => self.o_define_own_property(r, key, &PropertyDescriptor::data(v))?,
                })
            }
        }
    }

    // ------------------------------------------------------------ groupBy

    /// `Object.groupBy` and, with `map`, `Map.groupBy` (GroupBy of the specification). The callback runs once per value with
    /// `(value, index)`. The iterator is closed when the callback or the key conversion throws.
    fn ro_group_by(&mut self, items: Value, cb: Value, map: bool) -> Res<Value> {
        if is_nullish(items) {
            return Err(self.type_err(&format!("{}.groupBy called on null or undefined", if map { "Map" } else { "Object" })));
        }
        if !self.is_callable(cb) {
            let t = self.ro_text(cb);
            return Err(self.type_err(&format!("{t} is not a function")));
        }
        let mut it = self.iter_start_val(items)?;
        let mut groups: Vec<(Value, Vec<Value>)> = Vec::new();
        let mut index: FxMap<GKey, usize> = FxMap::default();
        let mut k = 0u64;
        while let Some(v) = self.iter_next(&mut it)? {
            let key = match self.ro_group_key(cb, v, k, map) {
                Ok(key) => key,
                Err(e) => return Err(self.iter_close_throw(&mut it, e)),
            };
            let gk = if map { GKey::V(hkey(key)) } else { GKey::P(self.coerce_key(key)?) };
            let slot = match index.get(&gk) {
                Some(&i) => i,
                None => {
                    self.ao_temp(groups.len() + 1, 64)?;
                    groups.push((key, Vec::new()));
                    index.insert(gk, groups.len() - 1);
                    groups.len() - 1
                }
            };
            self.ao_temp(groups[slot].1.len() + 1, 16)?;
            groups[slot].1.push(v);
            k += 1;
        }
        if map {
            let m = self.coll_new(true, self.sc.map_proto)?;
            for (key, vals) in groups {
                let arr = self.new_array_from(&vals)?;
                self.coll_set(m, key, arr)?;
            }
            return Ok(Value::Object(m));
        }
        let obj = h!(self, self.heap.new_object(None));
        for (key, vals) in groups {
            let key = self.coerce_key(key)?;
            let arr = self.new_array_from(&vals)?;
            self.def(obj, key, arr, true, true, true)?;
        }
        Ok(Value::Object(obj))
    }

    /// The callback result for one value, converted to the key: a property key for `Object.groupBy` (done once, in the
    /// caller, so a throwing conversion closes the iterator) and `-0` as `+0` for `Map.groupBy`.
    fn ro_group_key(&mut self, cb: Value, v: Value, k: u64, map: bool) -> Res<Value> {
        let r = self.call_fn(cb, Value::Undefined, &[v, Value::Number(k as f64)])?;
        if map {
            return Ok(norm_zero(r));
        }
        let key = self.coerce_key(r)?;
        Ok(h!(self, self.heap.key_to_value(key)))
    }
}
