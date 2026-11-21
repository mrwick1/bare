//! The core built-ins (plan step S2.7): how a function written in Rust is called, `Object`, `Function.prototype`, `Error` and its
//! subclasses, and `ToPrimitive`. The others are in `bi_array.rs`, `bi_string.rs` and `bi_number.rs`. Safe code over [`Realm`].
//!
//! **A native function** is an object with the flags `F_CALLABLE | F_NATIVE` and the shape `Realm::s_native`. Its slot 0 is the index
//! of a [`Native`] in `Realm::natives`; slots 1 to 3 are free for the function (a bound function keeps its target, `this` and
//! arguments there). The flag, not the shape, tells a native from a script function, because a property that a script adds changes the
//! shape. The call of a script function does not change: `Realm::function` and `Realm::function_new` answer `None` for a native, and the
//! interpreter asks [`Realm::call_native`] only then. So the hot path of a call has no new test.
//!
//! **Calling a script function from a native.** There are three ways:
//!
//! 1. A native that only wants a call made (`call`, `apply`, a bound function) sets `Realm::tail` and `Realm::tail_args` and returns.
//!    [`Realm::call_native`] gives [`Outcome::Enter`] and the interpreter pushes the frame, with the result going to the register of the
//!    call. A tail to another native runs in the loop of `call_native`.
//! 2. A built-in that has to call back many times (`forEach`, `map`, `sort`, ...) is written in JavaScript: `prelude.js`, compiled with
//!    each script (`script.rs`). Those calls are plain `Call` ops, so a callback needs no re-entry.
//! 3. A native or a conversion that needs the result in the middle of its work (`ToPrimitive`, a getter, a setter) calls
//!    [`Realm::call_function`] (`reenter.rs`): a nested run of the interpreter on a machine of its own.
//!
//! **GC.** A native never collects: the interpreter runs `gate!` before it, and the values in its arguments and `this` are registers of
//! the caller, which the root map of the call names. A nested call holds the collector too (`reenter.rs`). So a native may keep any
//! cell in a Rust local, and may allocate and call. A native that allocates more than the slack of the heap limit (8 MiB) only makes
//! the next gate collect.
use crate::cell::Layout;
use crate::bi_object::Own;
use crate::interp_rt::{ErrKind, Key, R, Realm};
use crate::object::{F_CALLABLE, F_CLASS, F_FNPROPS, F_NATIVE, Obj};
use crate::shapes::{A_C, A_W};
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
    /// The `length` and the `name` of the function, made into own properties at the first use ([`Realm::fn_prep`]).
    pub len: u8,
    pub name: &'static str,
}

/// What the interpreter does after a native returned.
pub enum Outcome {
    Val(Value),
    /// Call the script function `f` with `this` and the arguments in `Realm::tail_args`; the result is the result of the call.
    ///
    /// With the hole as `this` (no script can pass it), `f` is a suspended generator: push its frame (`generator.rs`).
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

    pub(crate) fn new_native(&mut self, f: NativeFn, ctor: bool, len: u8, name: &'static str) -> Obj {
        self.natives.push(Native { f, ctor, len, name });
        self.native_cell(self.natives.len() - 1)
    }

    /// Define the method `name` of `o` with the function `length` `len`.
    pub(crate) fn method(&mut self, o: Obj, name: &'static str, len: u8, f: NativeFn) {
        let k = self.intern(name);
        let c = self.new_native(f, false, len, name);
        self.define_attrs(o, Key::Name(k), HIDDEN, c.value()).expect("define");
    }

    /// Keep `v` alive for the life of the realm (a root).
    pub(crate) fn keep(&mut self, v: Value) {
        self.consts.push(v);
    }

    /// Give a built-in function its own `length` and `name` before a script reads, changes or lists them. A function has them from its
    /// first use only, so installing a built-in costs no property. Call it with the key that is being used: nothing happens for another
    /// key, so `f.call` does not make them. A script function cell gets its own (`Realm::fn_props`) at any key.
    #[inline]
    pub(crate) fn fn_prep(&mut self, o: Obj, k: Str) {
        self.fn_props(o);
        if o.has_flag(F_NATIVE) && !o.has_flag(F_FNPROPS) && (k == self.k_length || k == self.k_name) {
            self.materialize(o);
        }
    }

    /// As [`Realm::fn_prep`] for an operation on all properties.
    pub(crate) fn fn_prep_all(&mut self, o: Obj) {
        self.fn_props(o);
        if o.has_flag(F_NATIVE) && !o.has_flag(F_FNPROPS) {
            self.materialize(o);
        }
    }

    #[inline(never)]
    fn materialize(&mut self, o: Obj) {
        o.set_flag(F_FNPROPS);
        let Some(id) = self.native_id(o.value()) else { return };
        let (len, name) = if id == self.bound_id as usize {
            // A bound function: the length of the target less the bound arguments, and "bound " and the name of the target.
            let target = o.slot(&self.shapes, 1);
            let nb = Realm::obj(o.slot(&self.shapes, 3)).map_or(0, |a| a.len()) as f64;
            let tl = self.get_named(target, "length").ok().and_then(|v| v.as_f64()).unwrap_or(0.0);
            let tn = self.get_named(target, "name").ok().and_then(Realm::str_of).map(|s| self.text(s)).unwrap_or_default();
            ((tl - nb).max(0.0), format!("bound {tn}"))
        } else if let Some(p) = self.host_props(o) {
            p
        } else {
            (self.natives[id].len as f64, self.natives[id].name.to_string())
        };
        let nv = self.string(&name);
        let (kl, kn) = (self.k_length, self.k_name);
        let attrs = A_C;
        for (k, v) in [(kl, Realm::num(len)), (kn, nv)] {
            if self.shapes.find(o.shape(&self.shapes), k).is_none() {
                self.define_attrs(o, Key::Name(k), attrs, v).expect("define");
            }
        }
    }

    /// Define a data property of `o` that is not enumerable.
    pub(crate) fn hidden(&mut self, o: Obj, name: &str, v: Value) {
        let k = self.intern(name);
        self.define_attrs(o, Key::Name(k), HIDDEN, v).expect("define");
    }

    /// A constructor `name` that is a global, with `proto` as its `prototype`.
    pub(crate) fn ctor(&mut self, name: &'static str, len: u8, f: NativeFn, proto: Obj) -> Obj {
        let c = self.new_native(f, true, len, name);
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
        if self.function(f.value()).is_some_and(|(ci, _)| self.prog[ci].gen_kind != crate::bytecode::GK_NONE) {
            // A generator function: its `prototype` inherits `%GeneratorPrototype%` and has no `constructor`.
            let shape = self.shapes.root(Layout::Object, Some(self.genr.proto.cell()), 4);
            let p = Obj::new(&mut self.heap, &self.shapes, shape).expect("memory");
            self.define_attrs(f, Key::Name(self.k_prototype), A_W, p.value())?;
            return Ok(p.value());
        }
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

    /// `ToPrimitive`: the method `Symbol.toPrimitive` of the value if it has one, else `OrdinaryToPrimitive` (`valueOf` then `toString`,
    /// or the reverse for the hint `String`). The methods may be script functions, and a getter (`Realm::call_function`).
    pub fn to_primitive(&mut self, v: Value, hint: Hint) -> R<Value> {
        let Some(o) = Realm::obj(v) else { return Ok(v) };
        let order = if hint == Hint::String { [self.k_to_string, self.k_value_of] } else { [self.k_value_of, self.k_to_string] };
        if self.depth > 100 {
            return Err(self.error(ErrKind::Range, "maximum call stack size exceeded"));
        }
        self.depth += 1;
        let r = self.exotic_to_primitive(v, hint).and_then(|e| match e {
            Some(r) => Ok(r),
            None => self.ordinary_to_primitive(v, o, order),
        });
        self.depth -= 1;
        r
    }

    /// The step of `ToPrimitive` that calls `v[Symbol.toPrimitive](hint)`. `None` when there is no such method.
    fn exotic_to_primitive(&mut self, v: Value, hint: Hint) -> R<Option<Value>> {
        let f = self.get_prop(v, Key::Name(self.wk[crate::bi_symbol::TO_PRIMITIVE]))?;
        if f.is_undefined() || f.is_null() {
            return Ok(None);
        }
        if !Realm::obj(f).is_some_and(|f| f.is_callable()) {
            return self.type_error("Symbol.toPrimitive is not a function");
        }
        let h = self.string(match hint {
            Hint::Default => "default",
            Hint::Number => "number",
            Hint::String => "string",
        });
        let r = self.call_function(f, v, &[h])?;
        if Realm::obj(r).is_some() {
            return self.type_error("cannot convert an object to a primitive value");
        }
        Ok(Some(r))
    }

    pub(crate) fn ordinary_to_primitive(&mut self, v: Value, o: Obj, order: [Str; 2]) -> R<Value> {
        for k in order {
            let f = self.get_prop(o.value(), Key::Name(k))?;
            if !Realm::obj(f).is_some_and(|f| f.is_callable()) {
                continue;
            }
            let r = self.call_function(f, v, &[])?;
            if Realm::obj(r).is_none() {
                return Ok(r);
            }
        }
        self.type_error("cannot convert an object to a primitive value")
    }

    // ---- installation ----

    pub(crate) fn install_builtins(&mut self) {
        // The native that runs a bound function has the index `bound_id`.
        self.natives.push(Native { f: bound_call, ctor: false, len: 0, name: "" });
        self.bound_id = 0;
        let g = self.global;
        self.method(g, "__def", 2, prelude_def);
        self.hidden(g, "globalThis", g.value());
        self.hidden(g, "self", g.value());
        for (n, v) in [("NaN", Value::number(f64::NAN)), ("Infinity", Value::number(f64::INFINITY)), ("undefined", Value::UNDEFINED)] {
            let k = self.intern(n);
            self.define_attrs(g, Key::Name(k), 0, v).expect("define");
        }
        self.install_object();
        self.install_function();
        self.install_error();
        self.install_array();
        self.install_string();
        self.install_number();
        self.install_promise();
        self.install_modules();
        self.install_symbol();
        self.install_generator();
        self.install_json();
        self.install_collections();
        self.install_date();
        self.install_reflect();
    }

    fn install_function(&mut self) {
        let proto = self.function_proto;
        self.ctor("Function", 1, function_ctor, proto);
        for (n, len, f) in [("apply", 2, fn_apply as NativeFn), ("bind", 1, fn_bind), ("call", 1, fn_call), ("toString", 0, fn_to_string)] {
            self.method(proto, n, len, f);
        }
        self.install_has_instance();
    }

    fn install_error(&mut self) {
        // The order is that of `ErrKind` and `Realm::error_protos`.
        let names = ["TypeError", "ReferenceError", "RangeError", "SyntaxError", "Error", "EvalError", "URIError"];
        let mut error_ctor_cell = None;
        for i in [4, 0, 1, 2, 3, 5, 6] {
            let n = names[i];
            let proto = self.error_protos[i];
            let nv = self.string(n);
            self.hidden(proto, "name", nv);
            let empty = self.string("");
            self.hidden(proto, "message", empty);
            let c = self.ctor(n, 1, error_ctor, proto);
            match error_ctor_cell {
                None => error_ctor_cell = Some(c),
                // The constructor of a native error inherits from `Error`.
                Some(e) => c.set_proto(&mut self.shapes, Some(e.cell())),
            }
        }
        let p = self.error_proto;
        self.method(p, "toString", 0, error_to_string);
        self.install_aggregate_error(error_ctor_cell.expect("Error"));
    }
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
    let Some(m) = Realm::obj(arg(args, 1)) else { return rt.type_error("__def needs an object") };
    let lens = Realm::obj(arg(args, 2));
    for k in rt.own_key_list(m, true) {
        let key = rt.key_of(k)?;
        let Some(Own::Data(v, _)) = rt.get_own(m, key) else { continue };
        rt.define_attrs(t, key, HIDDEN, v)?;
        // The functions of the prelude are script functions: they get their own `length` and `name` here.
        if let Some(f) = Realm::obj(v).filter(|f| f.is_callable()) {
            let len = lens.and_then(|l| rt.get_own(l, key)).map_or(0, |p| if let Own::Data(n, _) = p { n.as_int().unwrap_or(0) } else { 0 });
            rt.define_attrs(f, Key::Name(rt.k_length), A_C, Value::int(len))?;
            rt.define_attrs(f, Key::Name(rt.k_name), A_C, k)?;
        }
    }
    Ok(Value::UNDEFINED)
}

// ---- Error ----

fn error_ctor(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let proto = rt.new_proto()?;
    let m = arg(args, 0);
    let msg = if m.is_undefined() { None } else { Some(rt.to_str(m)?.value()) };
    let e = rt.make_error(proto, msg);
    // `new Error(message, { cause })`: an own `cause` when the options have the property.
    if let Some(opts) = Realm::obj(arg(args, 1)) {
        let k = rt.intern("cause");
        if rt.has_prop(opts.value(), Key::Name(k))? {
            let c = rt.get_prop(opts.value(), Key::Name(k))?;
            rt.hidden(Realm::obj(e).expect("an error"), "cause", c);
        }
    }
    Ok(e)
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
