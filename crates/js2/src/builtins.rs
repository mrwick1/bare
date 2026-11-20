//! The core built-ins (plan step S2.7): how a function written in Rust is called, `Object`, `Function.prototype`, `Error` and its
//! subclasses, and `ToPrimitive`. The others are in `bi_array.rs`, `bi_string.rs` and `bi_number.rs`. Safe code over [`Realm`].
//!
//! **A native function** is an object with the flags `F_CALLABLE | F_NATIVE` and the shape `Realm::s_native`. Its slot 0 is the index
//! of a [`Native`] in `Realm::natives`; slots 1 to 3 are free for the function (a bound function keeps its target, `this` and
//! arguments there). The flag, not the shape, tells a native from a script function, because a property that a script adds changes the
//! shape. The call of a script function does not change: `Realm::function` and `Realm::function_new` answer `None` for a native, and the
//! interpreter asks [`Realm::call_native`] only then. So the hot path of a call has no new test.
//!
//! **A native cannot call a script function and wait for the result.** The interpreter keeps the JS stack in `run`, and a collection
//! walks it there, so a native has no frame to call from, and the result of a call would have to be rooted by hand. There are two ways
//! around it:
//!
//! 1. A native that only wants a call made (`call`, `apply`, a bound function) sets `Realm::tail` and `Realm::tail_args` and returns.
//!    [`Realm::call_native`] gives [`Outcome::Enter`] and the interpreter pushes the frame, with the result going to the register of the
//!    call. A tail to another native runs in the loop of `call_native`.
//! 2. A built-in that has to call back many times (`forEach`, `map`, `sort`, ...) is written in JavaScript: `prelude.js`, compiled with
//!    each script (`script.rs`). Those calls are plain `Call` ops, so a callback needs no re-entry, and each live value is in a
//!    register that a root map names.
//!
//! `ToPrimitive` runs a `valueOf` or `toString` only when it is a native. A script function there throws a `TypeError` (known gap, see
//! `docs/progress.md`): it needs a call from inside a conversion, which needs the same re-entry.
//!
//! **GC.** A native never collects: the interpreter runs `gate!` before it, and the values in its arguments and `this` are registers of
//! the caller, which the root map of the call names. So a native may keep any cell in a Rust local, and may allocate. A native that
//! allocates more than the slack of the heap limit (8 MiB) only makes the next gate collect.
use crate::cell::Layout;
use crate::interp_rt::{ErrKind, Key, R, Realm, Slot};
use crate::object::{F_CALLABLE, F_CLASS, F_NATIVE, Obj};
use crate::shapes::{A_ACC, A_C, A_E, A_W};
use crate::string::Str;
use crate::value::Value;

/// A built-in: the realm, `this`, the arguments. The arguments are the registers of the caller, so a native that calls into the realm
/// cannot change them.
pub type NativeFn = fn(&mut Realm, Value, &[Value]) -> R<Value>;

#[derive(Clone, Copy)]
pub struct Native {
    pub f: NativeFn,
    /// May a script `new` it?
    pub ctor: bool,
}

/// What the interpreter does after a native returned.
pub enum Outcome {
    Val(Value),
    /// Call the script function `f` with `this` and the arguments in `Realm::tail_args`; the result is the result of the call.
    Enter { f: Value, this: Value },
}

/// The hint of `ToPrimitive`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Hint {
    Default,
    Number,
    String,
}

pub fn arg(args: &[Value], i: usize) -> Value {
    args.get(i).copied().unwrap_or(Value::UNDEFINED)
}

/// Properties of a built-in: not enumerable (a method).
pub const HIDDEN: u8 = A_W | A_C;

impl Realm {
    // ---- native functions ----

    /// The index of the native of `v`, if `v` is a native function.
    #[inline]
    pub fn native_id(&self, v: Value) -> Option<usize> {
        let o = Realm::obj(v).filter(|o| o.has_flag(F_NATIVE))?;
        Some(o.slot(&self.shapes, 0).as_int()? as usize)
    }

    /// A cell for the native `id`.
    pub(crate) fn native_cell(&mut self, id: usize) -> Obj {
        let o = Obj::new(&mut self.heap, &self.shapes, self.s_native).expect("memory");
        o.set_slot(&self.shapes, 0, Value::int(id as i32));
        o.set_flag(F_CALLABLE | F_NATIVE);
        o
    }

    pub(crate) fn new_native(&mut self, f: NativeFn, ctor: bool) -> Obj {
        self.natives.push(Native { f, ctor });
        self.native_cell(self.natives.len() - 1)
    }

    /// Define the method `name` of `o`.
    pub(crate) fn method(&mut self, o: Obj, name: &str, f: NativeFn) {
        let k = self.intern(name);
        let c = self.new_native(f, false);
        self.define_attrs(o, Key::Name(k), HIDDEN, c.value()).expect("define");
    }

    /// Define a data property of `o` that is not enumerable.
    pub(crate) fn hidden(&mut self, o: Obj, name: &str, v: Value) {
        let k = self.intern(name);
        self.define_attrs(o, Key::Name(k), HIDDEN, v).expect("define");
    }

    /// A constructor `name` that is a global, with `proto` as its `prototype`.
    pub(crate) fn ctor(&mut self, name: &str, f: NativeFn, proto: Obj) -> Obj {
        let c = self.new_native(f, true);
        self.define_attrs(c, Key::Name(self.k_prototype), 0, proto.value()).expect("define");
        self.define_attrs(proto, Key::Name(self.k_constructor), HIDDEN, c.value()).expect("define");
        let g = self.global;
        self.hidden(g, name, c.value());
        c
    }

    /// Call the native `callee` (a loop over tail calls to natives). See [`Outcome`].
    #[inline(never)]
    pub fn call_native(&mut self, callee: Value, this: Value, args: &[Value]) -> R<Outcome> {
        let Some(id) = self.native_id(callee) else { return Ok(Outcome::Enter { f: callee, this }) };
        let f = self.natives[id].f;
        (self.callee, self.nt) = (callee, Value::UNDEFINED);
        let v = f(self, this, args)?;
        match self.tail.take() {
            None => Ok(Outcome::Val(v)),
            Some((f2, this2)) if self.native_id(f2).is_some() => {
                let a = std::mem::take(&mut self.tail_args);
                self.call_native(f2, this2, &a)
            }
            Some((f2, this2)) => Ok(Outcome::Enter { f: f2, this: this2 }),
        }
    }

    /// `new callee(...args)` for a native constructor. `nt` is `new.target`.
    #[inline(never)]
    pub fn construct_native(&mut self, callee: Value, args: &[Value], nt: Value) -> R<Value> {
        let Some(id) = self.native_id(callee).filter(|i| self.natives[*i].ctor) else { return self.type_error("not a constructor") };
        let f = self.natives[id].f;
        (self.callee, self.nt) = (callee, nt);
        let v = f(self, Value::UNDEFINED, args);
        self.nt = Value::UNDEFINED;
        v
    }

    /// The prototype of an object that a native constructor makes: `new.target.prototype` for a `new` (a class that extends the
    /// built-in), else the `prototype` of the native itself.
    pub(crate) fn new_proto(&mut self) -> R<Obj> {
        let f = if Realm::obj(self.nt).is_some() { self.nt } else { self.callee };
        let p = Realm::obj(f).map(|o| self.lookup(o, self.k_prototype)).transpose()?.flatten();
        match p.and_then(Realm::obj) {
            Some(p) => Ok(p),
            None => Ok(self.object_proto),
        }
    }

    pub(crate) fn new_array(&mut self, vals: &[Value]) -> Value {
        Obj::array(&mut self.heap, &self.shapes, self.s_array, vals).expect("memory").value()
    }

    pub(crate) fn get_named(&mut self, v: Value, name: &str) -> R<Value> {
        let k = self.intern(name);
        self.get_prop(v, Key::Name(k))
    }

    // ---- prototype of a plain function ----

    /// Does a read of `prototype` on `o` have to make it? A script function gets its `prototype` object at the first read, not when it
    /// is made: a closure that nobody uses as a constructor costs nothing.
    pub(crate) fn needs_prototype(&self, o: Obj) -> bool {
        o.is_callable() && !o.has_flag(F_NATIVE | F_CLASS)
    }

    pub(crate) fn make_prototype(&mut self, f: Obj) -> R<Value> {
        let p = Obj::new(&mut self.heap, &self.shapes, self.s_object).expect("memory");
        self.define_attrs(p, Key::Name(self.k_constructor), HIDDEN, f.value())?;
        self.define_attrs(f, Key::Name(self.k_prototype), A_W, p.value())?;
        Ok(p.value())
    }

    // ---- errors ----

    /// An error object with prototype `proto` and, when `msg` is given, the own property `message` (not enumerable).
    pub fn make_error(&mut self, proto: Obj, msg: Option<Value>) -> Value {
        let shape = self.shapes.root(Layout::Object, Some(proto.cell()), 4);
        let o = Obj::new(&mut self.heap, &self.shapes, shape).expect("memory");
        if let Some(m) = msg {
            o.add(&mut self.heap, &mut self.shapes, self.k_message, HIDDEN, &[m]).expect("memory");
        }
        o.value()
    }

    // ---- ToPrimitive ----

    /// `ToPrimitive` (`OrdinaryToPrimitive`: `valueOf` then `toString`, or the reverse for the hint `String`). There are no symbols, so
    /// no `Symbol.toPrimitive`. A method that is a script function throws a `TypeError` (see the module comment).
    pub fn to_primitive(&mut self, v: Value, hint: Hint) -> R<Value> {
        let Some(o) = Realm::obj(v) else { return Ok(v) };
        let order = if hint == Hint::String { [self.k_to_string, self.k_value_of] } else { [self.k_value_of, self.k_to_string] };
        if self.depth > 100 {
            return Err(self.error(ErrKind::Range, "maximum call stack size exceeded"));
        }
        self.depth += 1;
        let r = self.ordinary_to_primitive(v, o, order);
        self.depth -= 1;
        r
    }

    fn ordinary_to_primitive(&mut self, v: Value, o: Obj, order: [Str; 2]) -> R<Value> {
        for k in order {
            let Some(Slot::Data(f)) = self.lookup_g(o, k) else { continue };
            if !Realm::obj(f).is_some_and(|f| f.is_callable()) {
                continue;
            }
            if self.native_id(f).is_none() {
                let n = self.text(k);
                return self.type_error(&format!("a script function as {n} cannot be called from a conversion yet"));
            }
            if let Outcome::Val(r) = self.call_native(f, v, &[])?
                && Realm::obj(r).is_none()
            {
                return Ok(r);
            }
        }
        self.type_error("cannot convert an object to a primitive value")
    }

    // ---- installation ----

    pub(crate) fn install_builtins(&mut self) {
        // The native that runs a bound function has the index `bound_id`.
        self.natives.push(Native { f: bound_call, ctor: false });
        self.bound_id = 0;
        let g = self.global;
        self.method(g, "__def", prelude_def);
        self.hidden(g, "globalThis", g.value());
        self.hidden(g, "self", g.value());
        self.install_object();
        self.install_function();
        self.install_error();
        self.install_array();
        self.install_string();
        self.install_number();
    }

    fn install_object(&mut self) {
        let proto = self.object_proto;
        let c = self.ctor("Object", object_ctor, proto);
        for (n, f) in [
            ("keys", object_keys as NativeFn),
            ("values", object_values),
            ("entries", object_entries),
            ("assign", object_assign),
            ("create", object_create),
            ("defineProperty", object_define_property),
            ("defineProperties", object_define_properties),
            ("getPrototypeOf", object_get_prototype_of),
            ("setPrototypeOf", object_set_prototype_of),
            ("getOwnPropertyNames", object_own_names),
            ("getOwnPropertyDescriptor", object_own_descriptor),
            ("freeze", object_freeze),
            ("isFrozen", object_is_frozen),
        ] {
            self.method(c, n, f);
        }
        for (n, f) in [("hasOwnProperty", has_own as NativeFn), ("toString", object_to_string), ("valueOf", object_value_of), ("isPrototypeOf", is_prototype_of), ("propertyIsEnumerable", property_is_enumerable)] {
            self.method(proto, n, f);
        }
    }

    fn install_function(&mut self) {
        let proto = self.function_proto;
        self.ctor("Function", function_ctor, proto);
        for (n, f) in [("call", fn_call as NativeFn), ("apply", fn_apply), ("bind", fn_bind), ("toString", fn_to_string)] {
            self.method(proto, n, f);
        }
    }

    fn install_error(&mut self) {
        let names = ["TypeError", "ReferenceError", "RangeError", "SyntaxError", "Error"];
        for (i, n) in names.iter().enumerate() {
            let proto = self.error_protos[i];
            let nv = self.string(n);
            self.hidden(proto, "name", nv);
            let empty = self.string("");
            self.hidden(proto, "message", empty);
            self.ctor(n, error_ctor, proto);
        }
        let p = self.error_proto;
        self.method(p, "toString", error_to_string);
    }
}

// ---- Object ----

fn object_ctor(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    // `new Object()` from `super()` of a class that extends it: the prototype comes from `new.target`.
    // A primitive is not boxed (there are no wrapper objects): `Object(1)` is `1`.
    let a = arg(args, 0);
    if Realm::obj(rt.nt).is_none() && !a.is_undefined() && !a.is_null() {
        return Ok(a);
    }
    let proto = rt.new_proto()?;
    let shape = rt.shapes.root(Layout::Object, Some(proto.cell()), 4);
    Ok(Obj::new(&mut rt.heap, &rt.shapes, shape).expect("memory").value())
}

fn object_keys(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    rt.own_keys(arg(args, 0))
}

/// The own enumerable keys of `v` and the value of each.
fn own_pairs(rt: &mut Realm, v: Value) -> R<Vec<(Value, Value)>> {
    let keys = rt.own_keys(v)?;
    let ko = Realm::obj(keys).expect("an array");
    let mut out = Vec::with_capacity(ko.len() as usize);
    for i in 0..ko.len() {
        let k = ko.get_element(i).expect("a key");
        let key = rt.key_of(k)?;
        out.push((k, rt.get_prop(v, key)?));
    }
    Ok(out)
}

fn object_values(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let v: Vec<Value> = own_pairs(rt, arg(args, 0))?.into_iter().map(|p| p.1).collect();
    Ok(rt.new_array(&v))
}

fn object_entries(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let pairs = own_pairs(rt, arg(args, 0))?;
    let v: Vec<Value> = pairs.into_iter().map(|p| rt.new_array(&[p.0, p.1])).collect();
    Ok(rt.new_array(&v))
}

fn object_assign(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let target = arg(args, 0);
    if target.is_undefined() || target.is_null() {
        return rt.type_error("cannot convert undefined or null to an object");
    }
    for &src in args.iter().skip(1) {
        for (k, v) in own_pairs(rt, src)? {
            let key = rt.key_of(k)?;
            rt.set_prop(target, key, v)?;
        }
    }
    Ok(target)
}

fn object_create(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let p = arg(args, 0);
    let proto = match (Realm::obj(p), p.is_null()) {
        (Some(o), _) => Some(o.cell()),
        (None, true) => None,
        _ => return rt.type_error("object prototype may only be an object or null"),
    };
    let shape = rt.shapes.root(Layout::Object, proto, 4);
    let o = Obj::new(&mut rt.heap, &rt.shapes, shape).expect("memory");
    if !arg(args, 1).is_undefined() {
        object_define_properties(rt, Value::UNDEFINED, &[o.value(), arg(args, 1)])?;
    }
    Ok(o.value())
}

/// Read the field `name` of the descriptor `d` (own or inherited data property).
fn field(rt: &mut Realm, d: Obj, name: &str) -> R<Option<Value>> {
    let k = rt.intern(name);
    match rt.lookup_g(d, k) {
        Some(Slot::Data(v)) => Ok(Some(v)),
        Some(Slot::Acc(..)) => rt.type_error("accessors in a property descriptor are not supported here"),
        None => Ok(None),
    }
}

fn define_prop(rt: &mut Realm, o: Obj, key: Key, d: Value) -> R<()> {
    let Some(d) = Realm::obj(d) else { return rt.type_error("property description must be an object") };
    rt.fn_props(o);
    let (get, set) = (field(rt, d, "get")?, field(rt, d, "set")?);
    let (value, w, e, c) = (field(rt, d, "value")?, field(rt, d, "writable")?, field(rt, d, "enumerable")?, field(rt, d, "configurable")?);
    let k = match key {
        Key::Name(k) => k,
        Key::Idx(i) if o.is_indexed() && get.is_none() && set.is_none() => {
            return rt.set_prop(o.value(), Key::Idx(i), value.unwrap_or(Value::UNDEFINED));
        }
        Key::Idx(i) => rt.idx_atom(i),
    };
    let old = rt.shapes.find(o.shape(&rt.shapes), k);
    let base = old.map_or(0, |e| e.attrs());
    let flag = |x: Option<Value>, bit: u8, base: u8| match x {
        Some(v) if Realm::truthy(v) => bit,
        Some(_) => 0,
        None => base & bit,
    };
    let (e_bit, c_bit) = (flag(e, A_E, base), flag(c, A_C, base));
    if get.is_some() || set.is_some() {
        if value.is_some() || w.is_some() {
            return rt.type_error("invalid property descriptor: cannot both specify accessors and a value or writable");
        }
        let (og, os) = match old {
            Some(en) if en.attrs() & A_ACC != 0 => (o.slot(&rt.shapes, en.slot()), o.slot(&rt.shapes, en.slot() + 1)),
            _ => (Value::UNDEFINED, Value::UNDEFINED),
        };
        let vals = [get.unwrap_or(og), set.unwrap_or(os)];
        let attrs = A_ACC | e_bit | c_bit;
        return match old {
            Some(_) => rt.reconfigure(o, k, attrs, &vals),
            None => match o.add(&mut rt.heap, &mut rt.shapes, k, attrs, &vals) {
                Some(_) => Ok(()),
                None => Err(rt.error(ErrKind::Range, "out of memory")),
            },
        };
    }
    let w_bit = flag(w, A_W, if base & A_ACC != 0 { 0 } else { base });
    let cur = match old {
        Some(en) if en.attrs() & A_ACC == 0 => o.slot(&rt.shapes, en.slot()),
        _ => Value::UNDEFINED,
    };
    rt.define_attrs(o, Key::Name(k), w_bit | e_bit | c_bit, value.unwrap_or(cur))
}

fn object_define_property(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let Some(o) = Realm::obj(arg(args, 0)) else { return rt.type_error("Object.defineProperty called on a non-object") };
    let key = rt.key_of(arg(args, 1))?;
    define_prop(rt, o, key, arg(args, 2))?;
    Ok(o.value())
}

fn object_define_properties(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let Some(o) = Realm::obj(arg(args, 0)) else { return rt.type_error("Object.defineProperties called on a non-object") };
    for (k, d) in own_pairs(rt, arg(args, 1))? {
        let key = rt.key_of(k)?;
        define_prop(rt, o, key, d)?;
    }
    Ok(o.value())
}

fn object_get_prototype_of(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let v = arg(args, 0);
    if let Some(o) = Realm::obj(v) {
        return Ok(rt.proto_of(o).map_or(Value::NULL, |p| p.value()));
    }
    Ok(if Realm::str_of(v).is_some() {
        rt.string_proto.value()
    } else if v.is_number() {
        rt.number_proto.value()
    } else if v.as_bool().is_some() {
        rt.boolean_proto.value()
    } else {
        return rt.type_error("cannot convert undefined or null to an object");
    })
}

fn object_set_prototype_of(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let (v, p) = (arg(args, 0), arg(args, 1));
    let proto = match (Realm::obj(p), p.is_null()) {
        (Some(o), _) => Some(o.cell()),
        (None, true) => None,
        _ => return rt.type_error("object prototype may only be an object or null"),
    };
    if let Some(o) = Realm::obj(v) {
        o.set_proto(&mut rt.shapes, proto);
    }
    Ok(v)
}

fn object_own_names(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let Some(o) = Realm::obj(arg(args, 0)) else { return Ok(rt.new_array(&[])) };
    rt.fn_props(o);
    let mut out: Vec<Value> = Vec::new();
    if o.is_indexed() {
        for i in 0..o.len() {
            if o.get_element(i).is_some() {
                out.push(rt.string(&i.to_string()));
            }
        }
        out.push(rt.k_length.value());
    }
    let keys: Vec<Str> = rt.shapes.entries(o.shape(&rt.shapes)).map(|e| e.key()).collect();
    out.extend(keys.into_iter().filter(|k| !rt.text(*k).starts_with('\0')).map(|k| k.value()));
    Ok(rt.new_array(&out))
}

fn object_own_descriptor(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let Some(o) = Realm::obj(arg(args, 0)) else { return Ok(Value::UNDEFINED) };
    rt.fn_props(o);
    let key = rt.key_of(arg(args, 1))?;
    let k = match key {
        Key::Name(k) => k,
        Key::Idx(i) => {
            let Some(v) = o.is_indexed().then(|| o.get_element(i)).flatten() else { return Ok(Value::UNDEFINED) };
            return Ok(descriptor(rt, Some(v), None, A_W | A_E | A_C));
        }
    };
    let Some(e) = rt.shapes.find(o.shape(&rt.shapes), k) else {
        if k == rt.k_length && o.is_indexed() {
            return Ok(descriptor(rt, Some(Realm::num(o.len() as f64)), None, A_W));
        }
        return Ok(Value::UNDEFINED);
    };
    let at = e.attrs();
    Ok(if at & A_ACC != 0 {
        let (g, s) = (o.slot(&rt.shapes, e.slot()), o.slot(&rt.shapes, e.slot() + 1));
        descriptor(rt, None, Some((g, s)), at)
    } else {
        descriptor(rt, Some(o.slot(&rt.shapes, e.slot())), None, at)
    })
}

fn descriptor(rt: &mut Realm, value: Option<Value>, acc: Option<(Value, Value)>, attrs: u8) -> Value {
    let d = Obj::new(&mut rt.heap, &rt.shapes, rt.s_object).expect("memory");
    let put = |rt: &mut Realm, n: &str, v: Value| rt.set_own(d, n, v);
    if let Some(v) = value {
        put(rt, "value", v);
        put(rt, "writable", Value::bool(attrs & A_W != 0));
    }
    if let Some((g, s)) = acc {
        put(rt, "get", g);
        put(rt, "set", s);
    }
    put(rt, "enumerable", Value::bool(attrs & A_E != 0));
    put(rt, "configurable", Value::bool(attrs & A_C != 0));
    d.value()
}

/// Make each property of `o` read-only and not configurable. The elements of an array stay writable (the element records have no
/// attributes), and `o` can still get new properties (there is no `preventExtensions`): known gaps.
fn object_freeze(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let v = arg(args, 0);
    let Some(o) = Realm::obj(v) else { return Ok(v) };
    rt.fn_props(o);
    let entries: Vec<_> = rt.shapes.entries(o.shape(&rt.shapes)).collect();
    for e in entries {
        let at = e.attrs();
        let frozen = if at & A_ACC != 0 { at & !A_C } else { at & !(A_W | A_C) };
        if frozen != at {
            let vals: Vec<Value> = (0..e.width()).map(|j| o.slot(&rt.shapes, e.slot() + j)).collect();
            rt.reconfigure(o, e.key(), frozen, &vals)?;
        }
    }
    Ok(v)
}

fn object_is_frozen(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let Some(o) = Realm::obj(arg(args, 0)) else { return Ok(Value::TRUE) };
    rt.fn_props(o);
    let all = rt.shapes.entries(o.shape(&rt.shapes)).all(|e| e.attrs() & A_C == 0 && (e.attrs() & A_ACC != 0 || e.attrs() & A_W == 0));
    Ok(Value::bool(all && (!o.is_indexed() || o.is_empty())))
}

fn own_has(rt: &mut Realm, v: Value, key: Key) -> bool {
    if let Some(s) = Realm::str_of(v) {
        return match key {
            Key::Idx(i) => (i as usize) < s.len(),
            Key::Name(k) => k == rt.k_length,
        };
    }
    let Some(o) = Realm::obj(v) else { return false };
    rt.fn_props(o);
    match key {
        Key::Idx(i) if o.is_indexed() && o.get_element(i).is_some() => true,
        Key::Idx(i) => {
            let k = rt.idx_atom(i);
            rt.shapes.find(o.shape(&rt.shapes), k).is_some()
        }
        Key::Name(k) => rt.shapes.find(o.shape(&rt.shapes), k).is_some() || (k == rt.k_length && o.is_indexed()),
    }
}

fn has_own(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    if this.is_undefined() || this.is_null() {
        return rt.type_error("cannot convert undefined or null to an object");
    }
    let key = rt.key_of(arg(args, 0))?;
    Ok(Value::bool(own_has(rt, this, key)))
}

fn property_is_enumerable(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let key = rt.key_of(arg(args, 0))?;
    if !own_has(rt, this, key) {
        return Ok(Value::FALSE);
    }
    let Some(o) = Realm::obj(this) else { return Ok(Value::TRUE) };
    let k = match key {
        Key::Name(k) => k,
        Key::Idx(i) if o.is_indexed() && o.get_element(i).is_some() => return Ok(Value::TRUE),
        Key::Idx(i) => rt.idx_atom(i),
    };
    Ok(Value::bool(rt.shapes.find(o.shape(&rt.shapes), k).is_some_and(|e| e.attrs() & A_E != 0)))
}

pub(crate) fn object_to_string(rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    let tag = if this.is_undefined() {
        "Undefined"
    } else if this.is_null() {
        "Null"
    } else if Realm::str_of(this).is_some() {
        "String"
    } else if this.is_number() {
        "Number"
    } else if this.as_bool().is_some() {
        "Boolean"
    } else if let Some(o) = Realm::obj(this) {
        if o.is_callable() {
            "Function"
        } else if rt.is_array(o) {
            "Array"
        } else if rt.is_error(this) {
            "Error"
        } else if o.is_indexed() {
            "Arguments"
        } else {
            "Object"
        }
    } else {
        "Object"
    };
    Ok(rt.string(&format!("[object {tag}]")))
}

fn object_value_of(rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    if this.is_undefined() || this.is_null() {
        return rt.type_error("cannot convert undefined or null to an object");
    }
    Ok(this)
}

fn is_prototype_of(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let (Some(p), Some(mut o)) = (Realm::obj(this), Realm::obj(arg(args, 0))) else { return Ok(Value::FALSE) };
    while let Some(n) = rt.proto_of(o) {
        if n == p {
            return Ok(Value::TRUE);
        }
        o = n;
    }
    Ok(Value::FALSE)
}

impl Realm {
    /// An array (not an arguments object): an indexed object with a shape other than `s_args`.
    pub(crate) fn is_array(&self, o: Obj) -> bool {
        o.is_indexed() && o.shape(&self.shapes) != self.s_args
    }
}

// ---- Function.prototype ----

fn function_ctor(rt: &mut Realm, _this: Value, _args: &[Value]) -> R<Value> {
    rt.type_error("the Function constructor is not supported (there is no eval)")
}

fn callable_this(rt: &mut Realm, this: Value, what: &str) -> R<()> {
    if Realm::obj(this).is_some_and(|o| o.is_callable()) {
        Ok(())
    } else {
        rt.type_error(&format!("Function.prototype.{what} called on a value that is not a function"))
    }
}

fn fn_call(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    callable_this(rt, this, "call")?;
    rt.tail_args.clear();
    rt.tail_args.extend(args.iter().skip(1));
    rt.tail = Some((this, arg(args, 0)));
    Ok(Value::UNDEFINED)
}

fn fn_apply(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    callable_this(rt, this, "apply")?;
    let list = arg(args, 1);
    rt.tail_args.clear();
    if let Some(o) = Realm::obj(list) {
        if o.is_indexed() {
            let mut a = std::mem::take(&mut rt.tail_args);
            a.extend((0..o.len()).map(|i| o.get_element(i).unwrap_or(Value::UNDEFINED)));
            rt.tail_args = a;
        } else {
            let n = rt.get_named(list, "length")?;
            let n = rt.to_number(n)?;
            for i in 0..(n.max(0.0) as u32) {
                let v = rt.get_prop(list, Key::Idx(i))?;
                rt.tail_args.push(v);
            }
        }
    } else if !list.is_undefined() && !list.is_null() {
        return rt.type_error("CreateListFromArrayLike called on a non-object");
    }
    rt.tail = Some((this, arg(args, 0)));
    Ok(Value::UNDEFINED)
}

fn fn_bind(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    callable_this(rt, this, "bind")?;
    let rest = if args.len() > 1 { rt.new_array(&args[1..]) } else { Value::UNDEFINED };
    let b = rt.native_cell(rt.bound_id as usize);
    b.set_slot(&rt.shapes, 1, this);
    b.set_slot(&rt.shapes, 2, arg(args, 0));
    b.set_slot(&rt.shapes, 3, rest);
    Ok(b.value())
}

/// The call of a bound function: the target, with the bound `this` and the bound arguments before the arguments of the call.
fn bound_call(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let c = Realm::obj(rt.callee).expect("a bound function");
    let (target, bthis, bargs) = (c.slot(&rt.shapes, 1), c.slot(&rt.shapes, 2), c.slot(&rt.shapes, 3));
    rt.tail_args.clear();
    if let Some(a) = Realm::obj(bargs) {
        let mut v = std::mem::take(&mut rt.tail_args);
        v.extend((0..a.len()).map(|i| a.get_element(i).unwrap_or(Value::UNDEFINED)));
        rt.tail_args = v;
    }
    rt.tail_args.extend(args);
    rt.tail = Some((target, bthis));
    Ok(Value::UNDEFINED)
}

fn fn_to_string(rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    callable_this(rt, this, "toString")?;
    Ok(rt.string("function () { [native code] }"))
}

/// `__def(target, methods)` for `prelude.js`: copy the own enumerable properties of `methods` to `target`, not enumerable.
fn prelude_def(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let Some(t) = Realm::obj(arg(args, 0)) else { return rt.type_error("__def needs an object") };
    for (k, v) in own_pairs(rt, arg(args, 1))? {
        let key = rt.key_of(k)?;
        rt.define_attrs(t, key, HIDDEN, v)?;
    }
    Ok(Value::UNDEFINED)
}

// ---- Error ----

fn error_ctor(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let proto = rt.new_proto()?;
    let m = arg(args, 0);
    let msg = if m.is_undefined() { None } else { Some(rt.to_str(m)?.value()) };
    Ok(rt.make_error(proto, msg))
}

fn error_to_string(rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    if Realm::obj(this).is_none() {
        return rt.type_error("Error.prototype.toString called on a non-object");
    }
    let (n, m) = (rt.get_named(this, "name")?, rt.get_named(this, "message")?);
    let name = if n.is_undefined() { "Error".to_string() } else { let s = rt.to_str(n)?; rt.text(s) };
    let msg = if m.is_undefined() { String::new() } else { let s = rt.to_str(m)?; rt.text(s) };
    let t = match (name.is_empty(), msg.is_empty()) {
        (true, _) => msg,
        (_, true) => name,
        _ => format!("{name}: {msg}"),
    };
    Ok(rt.string(&t))
}
