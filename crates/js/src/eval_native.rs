// Native builtins of the evaluator. Included by `eval.rs` (`include!`), so it shares that module's imports.
// It holds the tiny builtin set: Object, Function.prototype, Array, Error and the conversion functions.

fn arg(args: &[Value], i: usize) -> Value {
    args.get(i).copied().unwrap_or(Value::Undefined)
}

impl<'p> Interp<'p> {
    fn call_native(&mut self, n: Nat, this: Value, args: &[Value], is_new: bool) -> Res<Value> {
        let a0 = arg(args, 0);
        match n {
            Nat::ObjectCtor => match a0 {
                Value::Undefined | Value::Null => Ok(Value::Object(h!(self, self.heap.new_object(Some(self.object_proto))))),
                Value::Object(_) => Ok(a0),
                _ => Err(unsupported("Object(primitive) wrapper object", Span::default())),
            },
            Nat::ObjectKeys => {
                let keys = match a0 {
                    Value::Object(o) => {
                        let mut out = Vec::new();
                        for k in h!(self, self.heap.own_keys(o)) {
                            if matches!(k, PropertyKey::Symbol(_)) {
                                continue;
                            }
                            if matches!(h!(self, self.heap.get_own_property(o, k)), Some(p) if p.enumerable()) {
                                out.push(k);
                            }
                        }
                        out
                    }
                    Value::String(s) => (0..h!(self, self.heap.str_len(s)) as u32).map(PropertyKey::Index).collect(),
                    Value::Undefined | Value::Null => return Err(self.type_err("Cannot convert undefined or null to object")),
                    _ => Vec::new(),
                };
                let mut vals = Vec::with_capacity(keys.len());
                for k in keys {
                    vals.push(h!(self, self.heap.key_to_value(k)));
                }
                self.new_array_from(&vals)
            }
            Nat::ObjectDefineProperty => {
                let Value::Object(o) = a0 else { return Err(self.type_err("Object.defineProperty called on non-object")) };
                let key = self.to_key(arg(args, 1))?;
                let d = self.to_descriptor(arg(args, 2))?;
                if !h!(self, self.heap.define_own_property(o, key, &d)) {
                    let k = self.key_text(key);
                    return Err(self.type_err(&format!("Cannot redefine property: {k}")));
                }
                Ok(a0)
            }
            Nat::ObjectGetOwnPropertyDescriptor => {
                let o = match a0 {
                    Value::Object(o) => o,
                    Value::Undefined | Value::Null => return Err(self.type_err("Cannot convert undefined or null to object")),
                    _ => return Ok(Value::Undefined),
                };
                let key = self.to_key(arg(args, 1))?;
                let Some(p) = h!(self, self.heap.get_own_property(o, key)) else { return Ok(Value::Undefined) };
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
            Nat::ObjectCreate => {
                let proto = match a0 {
                    Value::Object(p) => Some(p),
                    Value::Null => None,
                    _ => return Err(self.type_err("Object prototype may only be an Object or null")),
                };
                if !matches!(arg(args, 1), Value::Undefined) {
                    return Err(unsupported("Object.create properties argument", Span::default()));
                }
                Ok(Value::Object(h!(self, self.heap.new_object(proto))))
            }
            Nat::ObjectGetPrototypeOf => match a0 {
                Value::Object(o) => Ok(h!(self, self.heap.get_prototype_of(o)).map_or(Value::Null, Value::Object)),
                Value::Undefined | Value::Null => Err(self.type_err("Cannot convert undefined or null to object")),
                _ => Ok(Value::Object(self.object_proto)),
            },
            Nat::ObjHasOwn => {
                if is_nullish(this) {
                    return Err(self.type_err("Cannot convert undefined or null to object"));
                }
                let key = self.to_key(a0)?;
                Ok(Value::Bool(match this {
                    Value::Object(o) => h!(self, self.heap.has_own_property(o, key)),
                    Value::String(s) => key == self.k_length || matches!(key, PropertyKey::Index(i) if (i as usize) < h!(self, self.heap.str_len(s))),
                    _ => false,
                }))
            }
            Nat::ObjToString => {
                let tag = match this {
                    Value::Undefined => "Undefined",
                    Value::Null => "Null",
                    Value::Bool(_) => "Boolean",
                    Value::Number(_) => "Number",
                    Value::String(_) => "String",
                    Value::Symbol(_) => "Symbol",
                    Value::Object(o) if self.funcs.contains_key(&o) => "Function",
                    Value::Object(o) if h!(self, self.heap.is_array(o)) => "Array",
                    Value::Object(_) => "Object",
                };
                Ok(Value::String(h!(self, self.heap.intern_str(&format!("[object {tag}]")))))
            }
            Nat::ObjValueOf => {
                if is_nullish(this) {
                    return Err(self.type_err("Cannot convert undefined or null to object"));
                }
                Ok(this)
            }
            Nat::FnCall => {
                if !self.is_callable(this) {
                    return Err(self.type_err("Function.prototype.call called on a non-function"));
                }
                self.call_fn(this, a0, args.get(1..).unwrap_or(&[]))
            }
            Nat::FnApply => {
                if !self.is_callable(this) {
                    return Err(self.type_err("Function.prototype.apply was called on a non-function"));
                }
                let list = self.array_like(arg(args, 1))?;
                self.call_fn(this, a0, &list)
            }
            Nat::FnBind => {
                let Value::Object(t) = this else { return Err(self.type_err("Bind must be called on a function")) };
                if !self.funcs.contains_key(&t) {
                    return Err(self.type_err("Bind must be called on a function"));
                }
                let bargs = args.get(1..).unwrap_or(&[]).to_vec();
                let tl = self.get_prop(this, self.k_length)?;
                let len = if let Value::Number(l) = tl { (l - bargs.len() as f64).max(0.0) } else { 0.0 };
                let tn = self.get_prop(this, self.k_name)?;
                let tn = if let Value::String(s) = tn { self.heap.to_string_lossy(s).unwrap_or_default() } else { String::new() };
                let b = self.new_func_object(Callee::Bound { target: t, this: a0, args: bargs })?;
                let nm = h!(self, self.heap.intern_str(&format!("bound {tn}")));
                self.def(b, self.k_length, Value::Number(len), false, false, true)?;
                self.def(b, self.k_name, Value::String(nm), false, false, true)?;
                Ok(Value::Object(b))
            }
            Nat::FnCtor => Err(unsupported("Function constructor (dynamic code)", Span::default())),
            Nat::ArrayCtor => {
                if let ([Value::Number(len)], true) = (args, true) {
                    let a = h!(self, self.heap.new_array(Some(self.array_proto)));
                    if *len < 0.0 || len.fract() != 0.0 || *len > u32::MAX as f64 {
                        return Err(self.mk_err(RANGE_ERROR, "Invalid array length"));
                    }
                    h!(self, self.heap.set(a, self.k_length, Value::Number(*len)));
                    return Ok(Value::Object(a));
                }
                self.new_array_from(args)
            }
            Nat::ArrayIsArray => Ok(Value::Bool(matches!(a0, Value::Object(o) if h!(self, self.heap.is_array(o))))),
            Nat::ArrPush => {
                let len = self.length_of(this)?;
                for (i, &v) in args.iter().enumerate() {
                    let saved = std::mem::replace(&mut self.strict, true);
                    let r = self.put_prop(this, PropertyKey::Index((len + i as u64) as u32), v);
                    self.strict = saved;
                    r?;
                }
                let n = Value::Number((len + args.len() as u64) as f64);
                let saved = std::mem::replace(&mut self.strict, true);
                let r = self.put_prop(this, self.k_length, n);
                self.strict = saved;
                r?;
                Ok(n)
            }
            Nat::ArrPop => {
                let len = self.length_of(this)?;
                let saved = std::mem::replace(&mut self.strict, true);
                let r = (|| {
                    if len == 0 {
                        self.put_prop(this, self.k_length, Value::Number(0.0))?;
                        return Ok(Value::Undefined);
                    }
                    let k = PropertyKey::Index((len - 1) as u32);
                    let v = self.get_prop(this, k)?;
                    if let Value::Object(o) = this {
                        h!(self, self.heap.delete(o, k));
                    }
                    self.put_prop(this, self.k_length, Value::Number((len - 1) as f64))?;
                    Ok(v)
                })();
                self.strict = saved;
                r
            }
            Nat::ArrForEach => {
                let len = self.length_of(this)?;
                if !self.is_callable(a0) {
                    let t = self.display(a0);
                    return Err(self.type_err(&format!("{t} is not a function")));
                }
                let Value::Object(o) = this else { return Ok(Value::Undefined) };
                for i in 0..len {
                    let k = PropertyKey::Index(i as u32);
                    if h!(self, self.heap.has_property(o, k)) {
                        let v = self.get_prop(this, k)?;
                        self.call_fn(a0, arg(args, 1), &[v, Value::Number(i as f64), this])?;
                    }
                }
                Ok(Value::Undefined)
            }
            Nat::ArrJoin => {
                let len = self.length_of(this)?;
                let sep = if matches!(a0, Value::Undefined) { h!(self, self.heap.intern_str(",")) } else { self.to_str(a0)? };
                let sep_units: Vec<u16> = h!(self, self.heap.str_units(sep)).to_vec();
                let mut out: Vec<u16> = Vec::new();
                for i in 0..len {
                    if i > 0 {
                        out.extend_from_slice(&sep_units);
                    }
                    let v = self.get_prop(this, PropertyKey::Index(i as u32))?;
                    if !is_nullish(v) {
                        let s = self.to_str(v)?;
                        out.extend_from_slice(h!(self, self.heap.str_units(s)));
                    }
                }
                Ok(Value::String(h!(self, self.heap.intern_vec(out))))
            }
            Nat::ErrorCtor(k) => {
                let o = h!(self, self.heap.new_object(Some(self.error_protos[k as usize])));
                if !matches!(a0, Value::Undefined) {
                    let m = self.to_str(a0)?;
                    self.def(o, self.k_message, Value::String(m), true, false, true)?;
                }
                let _ = is_new;
                Ok(Value::Object(o))
            }
            Nat::ErrToString => {
                if !matches!(this, Value::Object(_)) {
                    return Err(self.type_err("Error.prototype.toString requires that 'this' be an Object"));
                }
                let nv = self.get_prop(this, self.k_name)?;
                let name = if matches!(nv, Value::Undefined) { "Error".to_string() } else { let s = self.to_str(nv)?; self.heap.to_string_lossy(s).unwrap_or_default() };
                let mv = self.get_prop(this, self.k_message)?;
                let msg = if matches!(mv, Value::Undefined) { String::new() } else { let s = self.to_str(mv)?; self.heap.to_string_lossy(s).unwrap_or_default() };
                let r = if name.is_empty() { msg } else if msg.is_empty() { name } else { format!("{name}: {msg}") };
                Ok(Value::String(h!(self, self.heap.intern_str(&r))))
            }
            Nat::StringFn => {
                if args.is_empty() {
                    return Ok(Value::String(h!(self, self.heap.intern_str(""))));
                }
                Ok(Value::String(self.to_str(a0)?))
            }
            Nat::NumberFn => Ok(Value::Number(if args.is_empty() { 0.0 } else { self.to_number(a0)? })),
            Nat::BooleanFn => Ok(Value::Bool(self.truthy(a0)?)),
            Nat::IsNaN => Ok(Value::Bool(self.to_number(a0)?.is_nan())),
        }
    }

    /// `length` of an array-like `this`, as `ToLength`.
    fn length_of(&mut self, this: Value) -> Res<u64> {
        let l = self.get_prop(this, self.k_length)?;
        let n = self.to_number(l)?;
        Ok(if n.is_nan() || n <= 0.0 { 0 } else { n.min(4294967295.0) as u64 })
    }

    /// The elements of an array-like (for `apply`). `undefined` and `null` are an empty list.
    fn array_like(&mut self, v: Value) -> Res<Vec<Value>> {
        match v {
            Value::Undefined | Value::Null => Ok(Vec::new()),
            Value::Object(_) => {
                let len = self.length_of(v)?;
                if len > (1 << 20) {
                    return Err(self.mk_err(RANGE_ERROR, "Too many arguments in function call"));
                }
                let mut out = Vec::with_capacity(len as usize);
                for i in 0..len {
                    out.push(self.get_prop(v, PropertyKey::Index(i as u32))?);
                }
                Ok(out)
            }
            _ => Err(self.type_err("CreateListFromArrayLike called on non-object")),
        }
    }

    fn to_descriptor(&mut self, d: Value) -> Res<PropertyDescriptor> {
        let Value::Object(o) = d else {
            let t = self.display(d);
            return Err(self.type_err(&format!("Property description must be an object: {t}")));
        };
        let mut out = PropertyDescriptor::default();
        for (k, which) in [(self.k_enumerable, 0), (self.k_configurable, 1), (self.k_value, 2), (self.k_writable, 3), (self.k_get, 4), (self.k_set, 5)] {
            if !h!(self, self.heap.has_property(o, k)) {
                continue;
            }
            let v = self.get_prop(d, k)?;
            match which {
                0 => out.enumerable = Some(self.truthy(v)?),
                1 => out.configurable = Some(self.truthy(v)?),
                2 => out.value = Some(v),
                3 => out.writable = Some(self.truthy(v)?),
                _ => {
                    let f = match v {
                        Value::Undefined => None,
                        Value::Object(f) if self.funcs.contains_key(&f) => Some(f),
                        _ => {
                            let (w, t) = (if which == 4 { "Getter" } else { "Setter" }, self.display(v));
                            return Err(self.type_err(&format!("{w} must be a function: {t}")));
                        }
                    };
                    if which == 4 { out.get = Some(f) } else { out.set = Some(f) }
                }
            }
        }
        if (out.get.is_some() || out.set.is_some()) && (out.value.is_some() || out.writable.is_some()) {
            return Err(self.type_err("Invalid property descriptor. Cannot both specify accessors and a value or writable attribute"));
        }
        Ok(out)
    }
}
