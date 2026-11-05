// Native builtins of the evaluator. Included by `eval.rs` (`include!`), so it shares that module's imports.
// It holds the tiny builtin set: Object, Function.prototype, Array, Error and the conversion functions.
// `Object.defineProperty` reports a non-extensible target as V8 does (`ro_define`, in `eval_object_more.rs`).

fn arg(args: &[Value], i: usize) -> Value {
    args.get(i).copied().unwrap_or(Value::Undefined)
}

impl<'p> Interp<'p> {
    fn call_native(&mut self, n: Nat, this: Value, args: &[Value], is_new: bool) -> Res<Value> {
        let a0 = arg(args, 0);
        match n {
            Nat::Bin(f) => self.call_binary(f, this, args, is_new),
            Nat::ObjectCtor => match a0 {
                Value::Undefined | Value::Null => Ok(Value::Object(h!(self, self.heap.new_object(Some(self.object_proto))))),
                Value::Object(_) => Ok(a0),
                _ => self.to_object(a0),
            },
            Nat::ObjectKeys => {
                let keys = match a0 {
                    Value::Object(o) => {
                        let mut out = Vec::new();
                        for k in self.property_keys(o)? {
                            self.tick()?;
                            if matches!(k, PropertyKey::Symbol(_)) {
                                continue;
                            }
                            if matches!(self.o_peek(o, k)?, Some(p) if p.enumerable()) {
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
                    self.tick()?;
                    vals.push(h!(self, self.heap.key_to_value(k)));
                }
                self.new_array_from(&vals)
            }
            Nat::ObjectDefineProperty => {
                let Value::Object(o) = a0 else { return Err(self.type_err("Object.defineProperty called on non-object")) };
                let key = self.coerce_key(arg(args, 1))?;
                let d = self.property_descriptor(arg(args, 2))?;
                self.ro_define(o, key, &d)?;
                Ok(a0)
            }
            Nat::ObjectGetOwnPropertyDescriptor => {
                let o = match a0 {
                    Value::Object(o) => o,
                    Value::Undefined | Value::Null => return Err(self.type_err("Cannot convert undefined or null to object")),
                    _ => return Ok(Value::Undefined),
                };
                let key = self.coerce_key(arg(args, 1))?;
                let Some(p) = self.o_get_own_property(o, key)? else { return Ok(Value::Undefined) };
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
                let o = h!(self, self.heap.new_object(proto));
                if !matches!(arg(args, 1), Value::Undefined) {
                    self.ro_define_properties(o, arg(args, 1))?;
                }
                Ok(Value::Object(o))
            }
            Nat::ObjectGetPrototypeOf => match a0 {
                Value::Object(o) => Ok(self.o_get_prototype_of(o)?.map_or(Value::Null, Value::Object)),
                Value::Undefined | Value::Null => Err(self.type_err("Cannot convert undefined or null to object")),
                Value::String(_) => Ok(Value::Object(self.ext.string_proto)),
                Value::Symbol(_) => Ok(Value::Object(self.sc.symbol_proto)),
                Value::Number(_) => Ok(Value::Object(self.ao.number_proto.unwrap_or(self.object_proto))),
                Value::Bool(_) => Ok(Value::Object(self.sc.bool_proto)),
                Value::BigInt(_) | Value::BigHeap(_) => Ok(Value::Object(self.big_proto()?)),
            },
            Nat::ObjHasOwn => {
                if is_nullish(this) {
                    return Err(self.type_err("Cannot convert undefined or null to object"));
                }
                let key = self.coerce_key(a0)?;
                Ok(Value::Bool(match this {
                    Value::Object(o) => self.o_has_own(o, key)?,
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
                    Value::BigInt(_) | Value::BigHeap(_) => "BigInt",
                    Value::Object(o) if self.funcs.contains_key(&o) => "Function",
                    Value::Object(o) if self.ext.regexps.contains_key(&o) => "RegExp",
                    Value::Object(o) if self.o_is_array(o)? => "Array",
                    Value::Object(o) if self.date.times.contains_key(&o) => "Date",
                    Value::Object(o) if o == self.sc.bool_proto => "Boolean",
                    Value::Object(o) if Some(o) == self.ao.number_proto => "Number",
                    Value::Object(o) if o == self.ext.string_proto => "String",
                    Value::Object(o) => match self.heap.primitive_value(o) {
                        Some(Value::Bool(_)) => "Boolean",
                        Some(Value::Number(_)) => "Number",
                        Some(Value::String(_)) => "String",
                        _ => "Object",
                    },
                };
                self.sc_object_to_string(this, tag)
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
                // a proxy target: BoundFunctionCreate asks [[GetPrototypeOf]], then HasOwnProperty("length") (spec order, visible to traps)
                let px = self.heap.is_proxy(t);
                if px {
                    self.o_get_prototype_of(t)?;
                }
                let tl = if !px || self.o_get_own_property(t, self.k_length)?.is_some() { self.get_prop(this, self.k_length)? } else { Value::Undefined };
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
            Nat::ArrayCtor => self.array_ctor(args, self.array_proto),
            Nat::ArrayIsArray => Ok(Value::Bool(matches!(a0, Value::Object(o) if self.o_is_array(o)?))),
            Nat::ArrPush => {
                let this = self.ao_recv(this)?;
                let len = self.length_of(this)?;
                if args.len() as u64 > 9007199254740991 - len { return Err(self.type_err("Array-like length exceeds the safe integer limit")); }
                for (i, &v) in args.iter().enumerate() {
                    self.tick()?;
                    let key = self.index_key(len + i as u64)?;
                    let saved = std::mem::replace(&mut self.strict, true);
                    let r = self.put_prop(this, key, v);
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
                let this = self.ao_recv(this)?;
                let len = self.length_of(this)?;
                let saved = std::mem::replace(&mut self.strict, true);
                let r = (|| {
                    if len == 0 {
                        self.put_prop(this, self.k_length, Value::Number(0.0))?;
                        return Ok(Value::Undefined);
                    }
                    let k = self.index_key(len - 1)?;
                    let v = self.get_prop(this, k)?;
                    if let Value::Object(o) = this
                        && !self.o_delete(o, k)? {
                        let key = self.key_text(k);
                        return Err(self.type_err(&format!("Cannot delete property '{key}'")));
                    }
                    self.put_prop(this, self.k_length, Value::Number((len - 1) as f64))?;
                    Ok(v)
                })();
                self.strict = saved;
                r
            }
            Nat::ArrForEach => {
                let this = self.ao_recv(this)?;
                let len = self.length_of(this)?;
                if !self.is_callable(a0) {
                    let t = self.display(a0);
                    return Err(self.type_err(&format!("{t} is not a function")));
                }
                for i in 0..len {
                    self.tick()?;
                    let k = self.index_key(i)?;
                    let has = match this { Value::Object(o) => self.o_has_property(o, k)?, _ => false };
                    if has {
                        let v = self.get_prop(this, k)?;
                        self.call_fn(a0, arg(args, 1), &[v, Value::Number(i as f64), this])?;
                    }
                }
                Ok(Value::Undefined)
            }
            Nat::ArrJoin => {
                let len = self.length_of(this)?;
                let sep = if matches!(a0, Value::Undefined) { h!(self, self.heap.intern_str(",")) } else { self.coerce_string(a0)? };
                let mut out: Vec<u16> = Vec::new();
                for i in 0..len {
                    self.tick()?;
                    if i > 0 {
                        self.append_string(&mut out, h!(self, self.heap.str_units(sep)))?;
                    }
                    let key = self.index_key(i)?;
                    let v = self.get_prop(this, key)?;
                    if !is_nullish(v) {
                        let s = self.coerce_string(v)?;
                        self.append_string(&mut out, h!(self, self.heap.str_units(s)))?;
                    }
                }
                Ok(Value::String(h!(self, self.heap.intern_vec(out))))
            }
            Nat::ErrorCtor(k) => self.error_ctor(a0, self.error_protos[k as usize]),
            Nat::ErrToString => {
                if !matches!(this, Value::Object(_)) {
                    return Err(self.type_err("Error.prototype.toString requires that 'this' be an Object"));
                }
                let nv = self.get_prop(this, self.k_name)?;
                let name = if matches!(nv, Value::Undefined) { "Error".to_string() } else { let s = self.coerce_string(nv)?; self.heap.to_string_lossy(s).unwrap_or_default() };
                let mv = self.get_prop(this, self.k_message)?;
                let msg = if matches!(mv, Value::Undefined) { String::new() } else { let s = self.coerce_string(mv)?; self.heap.to_string_lossy(s).unwrap_or_default() };
                let r = if name.is_empty() { msg } else if msg.is_empty() { name } else { format!("{name}: {msg}") };
                Ok(Value::String(h!(self, self.heap.intern_str(&r))))
            }
            Nat::StringFn | Nat::NumberFn | Nat::BooleanFn if is_new => self.wrapper_ctor(n, args, None),
            Nat::StringFn => {
                if args.is_empty() {
                    return Ok(Value::String(h!(self, self.heap.intern_str(""))));
                }
                if let (Value::Symbol(s), false) = (a0, is_new) {
                    return Ok(Value::String(self.symbol_descriptive(s)?));
                }
                Ok(Value::String(self.coerce_string(a0)?))
            }
            Nat::NumberFn => Ok(Value::Number(if args.is_empty() {
                0.0
            } else {
                match self.to_numeric(a0)? {
                    Value::Number(n) => n,
                    big => self.big_to_f64(big)?,
                }
            })),
            Nat::BooleanFn => Ok(Value::Bool(self.truthy(a0)?)),
            Nat::IsNaN => Ok(Value::Bool(self.coerce_number(a0)?.is_nan())),
            Nat::ParseInt => {
                let s = self.coerce_string(a0)?;
                let radix = to_int32(self.coerce_number(args.get(1).copied().unwrap_or(Value::Undefined))?);
                self.spend_units(h!(self, self.heap.str_len(s)))?;
                Ok(Value::Number(parse_int(h!(self, self.heap.str_units(s)), radix)))
            }
            Nat::EncodeUriComponent => {
                let s = self.coerce_string(a0)?;
                self.spend_units(h!(self, self.heap.str_len(s)))?;
                let mut size = 0usize;
                let cap = self.heap.limits().max_string_units;
                for c in char::decode_utf16(h!(self, self.heap.str_units(s)).iter().copied()) {
                    let Ok(c) = c else { return Err(self.mk_err(URI_ERROR, "URI malformed")) };
                    size = size.saturating_add(if c.is_ascii_alphanumeric() || "-_.!~*'()".contains(c) { 1 } else { 3 * c.len_utf8() });
                    if size > cap { return Err(Error::Limit(LimitKind::Heap(Limit::StringLength))); }
                }
                match encode_uri_component(h!(self, self.heap.str_units(s)), "") {
                    Some(out) => Ok(Value::String(h!(self, self.heap.intern_str(&out)))),
                    None => Err(self.mk_err(URI_ERROR, "URI malformed")),
                }
            }
            Nat::ParseFloat => {
                let s = self.coerce_string(a0)?;
                self.spend_units(h!(self, self.heap.str_len(s)))?;
                let mut scratch = String::new();
                Ok(Value::Number(number::float_prefix(h!(self, self.heap.str_units(s)), is_js_space, &mut scratch)))
            }
            Nat::IsFinite => Ok(Value::Bool(self.coerce_number(a0)?.is_finite())),
            Nat::EncodeUri | Nat::DecodeUri | Nat::DecodeUriComponent | Nat::Escape | Nat::Unescape => self.uri_fn(n, a0),
            Nat::Ext(x) => self.call_ext(x, this, args, is_new),
            Nat::Ao(f) => self.call_ao(f, this, args),
            Nat::Sc(f) => self.call_sc(f, this, args, is_new),
            Nat::Refl(f) => self.call_refl(f, this, args),
            Nat::Px(f) => self.call_px(f, args, is_new),
            Nat::Date(f) => self.call_date(f, this, args, is_new),
            Nat::Big(f) => self.call_bigint(f, this, args),
            Nat::Intl(f) => self.call_intl(f, this, args, is_new),
        }
    }

    /// `encodeURI`, `decodeURI`, `decodeURIComponent`, `escape` and `unescape`. The output size of an encoding is
    /// checked against the string cap before the text is made.
    fn uri_fn(&mut self, n: Nat, a0: Value) -> Res<Value> {
        let s = self.coerce_string(a0)?;
        self.spend_units(h!(self, self.heap.str_len(s)))?;
        let units = h!(self, self.heap.str_units(s));
        let cap = self.heap.limits().max_string_units;
        let size: usize = match n {
            Nat::EncodeUri => char::decode_utf16(units.iter().copied())
                .map(|c| c.map_or(3, |c| if c.is_ascii_alphanumeric() || "-_.!~*'()".contains(c) || URI_RESERVED.contains(c) { 1 } else { 3 * c.len_utf8() }))
                .fold(0, usize::saturating_add),
            Nat::Escape => units.iter().map(|&u| escape_size(u)).fold(0, usize::saturating_add),
            _ => 0,
        };
        if size > cap {
            return Err(Error::Limit(LimitKind::Heap(Limit::StringLength)));
        }
        let out = match n {
            Nat::EncodeUri => encode_uri_component(units, URI_RESERVED).map(|t| t.encode_utf16().collect()),
            Nat::DecodeUri => decode_uri(units, URI_RESERVED),
            Nat::DecodeUriComponent => decode_uri(units, ""),
            Nat::Escape => Some(escape(units).encode_utf16().collect()),
            _ => Some(unescape(units)),
        };
        match out {
            Some(u) => Ok(Value::String(h!(self, self.heap.intern_vec(u)))),
            None => Err(self.mk_err(URI_ERROR, "URI malformed")),
        }
    }

    fn array_ctor(&mut self, args: &[Value], proto: ObjId) -> Res<Value> {
        if let [Value::Number(len)] = args {
            let a = h!(self, self.heap.new_array(Some(proto)));
            if *len < 0.0 || len.fract() != 0.0 || *len > u32::MAX as f64 {
                return Err(self.mk_err(RANGE_ERROR, "Invalid array length"));
            }
            hxw!(self, self.heap.set(a, self.k_length, Value::Number(*len)));
            return Ok(Value::Object(a));
        }
        self.new_array_from_proto(args, proto)
    }

    fn error_ctor(&mut self, message: Value, proto: ObjId) -> Res<Value> {
        let o = h!(self, self.heap.new_object(Some(proto)));
        if !matches!(message, Value::Undefined) {
            let m = self.coerce_string(message)?;
            self.def(o, self.k_message, Value::String(m), true, false, true)?;
        }
        self.capture_stack(o)?;
        Ok(Value::Object(o))
    }

    fn index_key(&mut self, index: u64) -> Res<PropertyKey> {
        if index <= MAX_INDEX as u64 { return Ok(PropertyKey::Index(index as u32)); }
        Ok(h!(self, self.heap.key_from_str(&index.to_string())))
    }

    /// `length` of an array-like `this`, as `ToLength`.
    fn length_of(&mut self, this: Value) -> Res<u64> {
        let l = self.get_prop(this, self.k_length)?;
        let n = self.coerce_number(l)?;
        Ok(if n.is_nan() || n <= 0.0 { 0 } else { n.min(9007199254740991.0) as u64 })
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
                self.spend(len)?;
                let mut out = Vec::with_capacity(len as usize);
                for i in 0..len {
                    out.push(self.get_prop(v, PropertyKey::Index(i as u32))?);
                }
                Ok(out)
            }
            _ => Err(self.type_err("CreateListFromArrayLike called on non-object")),
        }
    }

    fn property_descriptor(&mut self, d: Value) -> Res<PropertyDescriptor> {
        let Value::Object(o) = d else {
            let t = self.display(d);
            return Err(self.type_err(&format!("Property description must be an object: {t}")));
        };
        let mut out = PropertyDescriptor::default();
        for (k, which) in [(self.k_enumerable, 0), (self.k_configurable, 1), (self.k_value, 2), (self.k_writable, 3), (self.k_get, 4), (self.k_set, 5)] {
            if !self.o_has_property(o, k)? {
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
