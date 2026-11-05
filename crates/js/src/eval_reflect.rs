// `Object.getOwnPropertyNames`, `Reflect.ownKeys` and `Reflect.construct`. Included by `eval.rs` (`include!`), so it shares
// that module's imports and private items. Every native here is `Nat::Refl(ReflFn)`; `call_refl` is the one dispatch hook.
// The rest of the Object and Reflect functions (batch 1) are in `eval_object_more.rs`; `call_refl` forwards them with `this`.
//
// Observed in the staging bundles (text scan only, not a runtime proof): `Object.getOwnPropertyNames` in the webpack
// runtime (`o`-style namespace copy over a prototype chain), `Reflect.ownKeys` inside a Proxy `ownKeys` trap, and
// `Reflect.construct(n, [])` and `Reflect.construct(e, [], n)` in React's native-frame probe.
//
// Supported
// - `Object.getOwnPropertyNames(O)`: strings only, enumerable or not, in native own-key order (`Heap::own_keys`: indices
//   ascending, then strings in creation order). An object (a String wrapper has its indices, then `length`, from the heap), a
//   string (the same keys, computed, no wrapper) and any other primitive (no keys). `undefined` and `null` are a TypeError.
// - `Reflect.ownKeys(O)`: the same keys plus symbols last. A non-object is a TypeError.
// - `Reflect.construct(target, argumentsList [, newTarget])`. The checks run in spec order: `target` is a constructor, a
//   present `newTarget` is a constructor (an explicit `undefined` is present), `argumentsList` is an object, then its
//   `length` and indices are read (`array_like`, bounded and charged before the list grows), then `Construct(target, list,
//   newTarget)` runs on the one dispatcher of `eval_classes.rs` (`construct_nt`), so `new.target`, the prototype of the
//   result, class fields, derived classes, bound functions and explicit object returns are the ones `new` and `super()` use.
// - The `Reflect` global is an ordinary object with `@@toStringTag` and these two methods plus the eleven of
//   `eval_object_more.rs`. There is no Proxy.
//
// Gates (never silent)
// - A native constructor that cannot take a `new.target` (Map, Set, WeakMap, WeakSet, Promise, AggregateError, the byte
//   classes, a host constructor) is `Unsupported` when `newTarget` differs from it, as `class extends` already is. With the
//   same `newTarget` it is a plain `new`.
// - Own keys of a function lack the `arguments` and `caller` that V8 adds to sloppy functions.
//
// State: none. The natives are function objects reachable from the global `Reflect` and `Object`, like every builtin.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ReflFn {
    OwnNames,
    OwnKeys,
    Construct,
    // Object (batch 1, `eval_object_more.rs`)
    ObjGetOwnPropertyDescriptors, ObjGetOwnPropertySymbols, ObjHasOwn, ObjIs, ObjPreventExtensions, ObjSeal, ObjFreeze,
    ObjIsExtensible, ObjIsFrozen, ObjIsSealed, ObjDefineProperties, ObjSetPrototypeOf, ObjGroupBy, MapGroupBy,
    // Object.prototype
    ProtoIsPrototypeOf, ProtoPropertyIsEnumerable, ProtoToLocaleString, ProtoGet, ProtoSet, ProtoDefineGetter,
    ProtoDefineSetter, ProtoLookupGetter, ProtoLookupSetter,
    // Reflect
    RApply, RDefineProperty, RDeleteProperty, RGet, RGetOwnPropertyDescriptor, RGetPrototypeOf, RHas, RIsExtensible,
    RPreventExtensions, RSet, RSetPrototypeOf,
}

impl<'p> Interp<'p> {
    fn install_reflect(&mut self) -> Res<()> {
        let (op, g) = (self.object_proto, self.global);
        let Get::Value(Value::Object(oc)) = h!(self, self.heap.get(op, self.k_constructor)) else { return Err(unsupported("Object constructor missing", Span::default())) };
        self.method(oc, "getOwnPropertyNames", Nat::Refl(ReflFn::OwnNames), 1)?;
        let r = h!(self, self.heap.new_object(Some(op)));
        self.method(r, "construct", Nat::Refl(ReflFn::Construct), 2)?;
        self.method(r, "ownKeys", Nat::Refl(ReflFn::OwnKeys), 1)?;
        self.sc_tag(r, "Reflect")?;
        let k = h!(self, self.heap.key_from_str("Reflect"));
        self.def(g, k, Value::Object(r), true, false, true)?;
        self.install_object_more(oc, op, r)
    }

    fn call_refl(&mut self, f: ReflFn, this: Value, args: &[Value]) -> Res<Value> {
        let a0 = arg(args, 0);
        match f {
            ReflFn::OwnNames => match a0 {
                Value::Object(o) => self.own_key_array(o, false),
                Value::String(s) => {
                    // The keys of a string wrapper, without the wrapper: indices, then `length`. Charged before the first key.
                    let len = h!(self, self.heap.str_len(s));
                    self.spend(len as u64)?;
                    let a = h!(self, self.heap.new_array(Some(self.array_proto)));
                    for i in 0..len as u32 {
                        let v = h!(self, self.heap.key_to_value(PropertyKey::Index(i)));
                        self.def(a, PropertyKey::Index(i), v, true, true, true)?;
                    }
                    let v = h!(self, self.heap.key_to_value(self.k_length));
                    self.def(a, PropertyKey::Index(len as u32), v, true, true, true)?;
                    Ok(Value::Object(a))
                }
                Value::Undefined | Value::Null => Err(self.type_err("Cannot convert undefined or null to object")),
                _ => self.new_array_from(&[]),
            },
            ReflFn::OwnKeys => match a0 {
                Value::Object(o) => self.own_key_array(o, true),
                _ => Err(self.type_err("Reflect.ownKeys called on non-object")),
            },
            ReflFn::Construct => self.reflect_construct(args),
            _ => self.call_object_more(f, this, args),
        }
    }

    /// The own keys of `o` as an array of strings (and symbols when `symbols`). `property_keys` charges the virtual indices of
    /// a byte view before the key list exists, and the array grows one checked element at a time.
    fn own_key_array(&mut self, o: ObjId, symbols: bool) -> Res<Value> {
        let keys = self.property_keys(o)?;
        let a = h!(self, self.heap.new_array(Some(self.array_proto)));
        let mut n = 0u32;
        for k in keys {
            self.tick()?;
            if !symbols && matches!(k, PropertyKey::Symbol(_)) {
                continue;
            }
            let v = h!(self, self.heap.key_to_value(k));
            self.def(a, PropertyKey::Index(n), v, true, true, true)?;
            n += 1;
        }
        Ok(Value::Object(a))
    }

    /// `is_constructor`, with one step for each bound function that it unwraps (a long chain must not be free).
    fn is_constructor_spending(&mut self, mut o: ObjId) -> Res<bool> {
        loop {
            self.tick()?;
            match self.funcs.get(&o) {
                Some(&Callee::Bound { target, .. }) => o = target,
                _ => return Ok(self.is_constructor(o)),
            }
        }
    }

    fn reflect_construct(&mut self, args: &[Value]) -> Res<Value> {
        let not_ctor = |s: &mut Self, v: Value| {
            let t = s.display(v);
            Err(s.type_err(&format!("{t} is not a constructor")))
        };
        let target = arg(args, 0);
        let Value::Object(f) = target else { return not_ctor(self, target) };
        if !self.is_constructor_spending(f)? {
            return not_ctor(self, target);
        }
        let nt = if args.len() >= 3 {
            match args[2] {
                Value::Object(n) if self.is_constructor_spending(n)? => n,
                v => return not_ctor(self, v),
            }
        } else {
            f
        };
        let list = arg(args, 1);
        if !matches!(list, Value::Object(_)) {
            return Err(self.type_err("CreateListFromArrayLike called on non-object"));
        }
        let list = self.array_like(list)?;
        self.construct_nt(f, &list, nt, ExprId::from_index(0))
    }
}
