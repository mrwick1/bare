//! Host functions on the interface (`docs/design-host-handlers.md`, step H0). `Dom` has two registries. A handler of the old form (`H`:
//! `Interp`, `Value`) is in `Dom::handlers` and runs through the old `Host::call`. A handler of the new form (`HS`: `Scope`, `Local`) is in
//! `Dom::scoped` and runs through the interface `Host::call`. The engine tells them apart by `API_ID` in the function id, so the old path
//! has no new branch beyond one test of that bit. Both entries run the same hooks in the same order (`Dom::dispatch_host` for the old one; `Dom::call_scoped` spells them out with no closure, because it is the hot path), so a handler sees the same hooks in either.
//!
//! A test build can put every old handler on the interface too (`BARE_ALL_API=1`): the entry then converts `this` and the arguments to raw
//! values, calls the old body and pushes the result. It proves that the interface entry does what the old entry does.

use crate::browser::*;
use bare_css::selector::Selector;
use bare_html::dom::{Document, NodeId};
use std::rc::Rc;
use bare_js::eval::{Error, Interp};
use bare_js::value::{ObjId, Value};
use bare_js_api::{Abort, Args, Attrs, Engine, HostFn, Local, Persistent, Scope, Type};

/// A host method of the new form: the DOM, the scope, `this`, the arguments.
pub(crate) type HS = for<'s, 'p> fn(&Dom, &mut Sc<'s, 'p>, Local<'s>, Args<'s>) -> Res<'s, Local<'s>>;
pub(crate) type Sc<'s, 'p> = Scope<'s, Interp<'p>>;
pub(crate) type Res<'s, T> = Result<T, bare_js_api::Error<'s>>;

/// The `style.cur` of a function of the new form: its index in `Dom::scoped`, with this bit, so it cannot equal an old function id.
pub(crate) const CUR_NEW: u32 = 1 << 31;

#[derive(Clone, Copy)]
pub(crate) enum Fun {
    New(HS),
    /// An old handler that a test build put on the interface.
    #[cfg(test)]
    Old(H),
}

#[derive(Clone, Copy)]
pub(crate) struct Scoped {
    f: Fun,
    needs_new: bool,
    /// What `Dom::dispatch_host` stores in `style.cur`.
    cur: u32,
}

/// What an entry gives to `Dom::dispatch_host`: the engine, and the way to give it an error of the old engine.
pub(crate) trait Entry<'p> {
    type Err;
    fn it(&mut self) -> &mut Interp<'p>;
    fn err(&mut self, e: Error) -> Self::Err;
}

impl<'p> Entry<'p> for Interp<'p> {
    type Err = Error;
    #[inline(always)]
    fn it(&mut self) -> &mut Interp<'p> {
        self
    }
    #[inline(always)]
    fn err(&mut self, e: Error) -> Error {
        e
    }
}

impl<'s, 'p> Entry<'p> for Sc<'s, 'p> {
    type Err = bare_js_api::Error<'s>;
    #[inline(always)]
    fn it(&mut self) -> &mut Interp<'p> {
        self.engine()
    }
    #[inline]
    fn err(&mut self, e: Error) -> bare_js_api::Error<'s> {
        self.engine().api_err(e)
    }
}

/// Is every old handler on the interface (a test build with `BARE_ALL_API` set)?
#[cfg(test)]
fn all_on_api() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("BARE_ALL_API").is_some())
}
#[cfg(not(test))]
#[inline(always)]
fn all_on_api() -> bool {
    false
}

impl Dom {
    /// One host call: the hooks around a handler, for both entries. `bad_new` is a call of a constructor without `new`.
    #[inline(always)]
    pub(crate) fn dispatch_host<'p, X: Entry<'p>, T>(&self, x: &mut X, cur: u32, bad_new: bool, body: impl FnOnce(&mut X) -> Result<T, X::Err>) -> Result<T, X::Err> {
        self.style.cur.set(cur);
        if bad_new {
            return Err(self.bad_new(x));
        }
        let r = if self.ce.on.get() { self.dispatch_ce(x, body) } else { body(x) };
        if self.mo.armed() {
            self.mo_done(x)?;
        }
        r
    }

    #[cold]
    #[inline(never)]
    fn bad_new<'p, X: Entry<'p>>(&self, x: &mut X) -> X::Err {
        let e = crate::browser_custom::needs_new(x.it());
        x.err(e)
    }

    /// `bad_new` for a handler of the new form: the constructors of `browser_geom` and `browser_custom` have their own message.
    #[cold]
    #[inline(never)]
    fn bad_new_s<'s, 'p>(&self, s: &mut Sc<'s, 'p>, f: HS) -> bare_js_api::Error<'s> {
        let it = s.engine();
        match crate::browser_geom::needs_new(it, f).or_else(|| crate::browser_custom::needs_new_s(it, f)) {
            Some(e) => s.engine().api_err(e),
            None => self.bad_new(s),
        }
    }

    /// The mutation observer hook after a host call.
    #[inline(never)]
    fn mo_done<'p, X: Entry<'p>>(&self, x: &mut X) -> Result<(), X::Err> {
        match self.mo_after(x.it()) {
            Ok(()) => Ok(()),
            Err(e) => Err(x.err(e)),
        }
    }

    /// `body` with a scope of custom element reactions around it.
    #[inline(never)]
    fn dispatch_ce<'p, X: Entry<'p>, T>(&self, x: &mut X, body: impl FnOnce(&mut X) -> Result<T, X::Err>) -> Result<T, X::Err> {
        self.ce_enter();
        let r = body(x);
        let q = self.ce_pop(x.it());
        match (r, q) {
            (Ok(v), Ok(())) => Ok(v),
            (Err(e), _) => Err(e),
            (Ok(_), Err(e)) => Err(x.err(e)),
        }
    }

    /// The entry of the interface `Host::call`. It runs the hooks of `dispatch_host` in the same order, with no closure: the call
    /// path of a handler is the hot path, and a closure that both the plain and the custom element branch use sits in memory first.
    pub(crate) fn call_scoped<'s, 'p>(&self, s: &mut Sc<'s, 'p>, id: u32, this: Local<'s>, args: Args<'s>, is_new: bool) -> Res<'s, Local<'s>> {
        // SAFETY: the entry is copied out at once. Nothing runs between the read and the copy, so no push (`add_fn_s`) can move the vector under it.
        let Scoped { f, needs_new, cur } = unsafe { &*self.scoped.as_ptr() }[id as usize];
        self.style.cur.set(cur);
        if needs_new && !is_new {
            return Err(match f {
                Fun::New(f) => self.bad_new_s(s, f),
                #[cfg(test)]
                Fun::Old(_) => self.bad_new(s),
            });
        }
        let r = if self.ce.on.get() { self.call_scoped_ce(s, f, this, args) } else { self.run_fun(s, f, this, args) };
        if self.mo.armed() {
            self.mo_done(s)?;
        }
        r
    }

    #[inline(always)]
    fn run_fun<'s, 'p>(&self, s: &mut Sc<'s, 'p>, f: Fun, this: Local<'s>, args: Args<'s>) -> Res<'s, Local<'s>> {
        match f {
            Fun::New(f) => f(self, s, this, args),
            #[cfg(test)]
            Fun::Old(h) => self.call_old(s, h, this, args),
        }
    }

    /// `call_scoped` with a scope of custom element reactions around the handler.
    #[inline(never)]
    fn call_scoped_ce<'s, 'p>(&self, s: &mut Sc<'s, 'p>, f: Fun, this: Local<'s>, args: Args<'s>) -> Res<'s, Local<'s>> {
        self.dispatch_ce(s, |s| self.run_fun(s, f, this, args))
    }

    #[cfg(test)]
    /// An old handler through the interface: `this` and the arguments as raw values, the result as a local.
    fn call_old<'s, 'p>(&self, s: &mut Sc<'s, 'p>, h: H, this: Local<'s>, args: Args<'s>) -> Res<'s, Local<'s>> {
        let it = s.engine();
        let this = it.raw(this);
        let args: Vec<Value> = args.iter().map(|a| it.raw(a)).collect();
        match h(self, it, this, &args) {
            Ok(v) => Ok(it.raw_local(&v)),
            Err(e) => Err(it.api_err(e)),
        }
    }

    /// Make a function of the new form. Its id is its index in `Dom::scoped`.
    pub(crate) fn add_fn_s<'s, 'p>(&self, s: &mut Sc<'s, 'p>, f: HS, name: &str, len: u32, kind: HostFn) -> Res<'s, Local<'s>> {
        let id = self.scoped.borrow().len() as u32;
        self.scoped.borrow_mut().push(Scoped { f: Fun::New(f), needs_new: kind != HostFn::Method, cur: id | CUR_NEW });
        s.host_function(name, id, len, kind)
    }

    /// `add_fn_s` for code that has an interpreter and wants the function object.
    pub(crate) fn add_fn_s_obj(&self, it: &mut Interp<'_>, f: HS, name: &str, len: u32, kind: HostFn) -> R<ObjId> {
        bare_js_api::enter(it, |s| self.add_fn_s_raw(s, f, name, len, kind))
    }

    /// `add_fn_s_obj` inside a scope that the caller holds, for a loop that makes many functions in one scope.
    pub(crate) fn add_fn_s_raw(&self, s: &mut Sc<'_, '_>, f: HS, name: &str, len: u32, kind: HostFn) -> R<ObjId> {
        match self.add_fn_s(s, f, name, len, kind) {
            Ok(l) => Ok(obj_of(s, l)),
            Err(e) => Err(s.engine().api_unerr(e)),
        }
    }

    /// An old handler on the interface (a test build with `BARE_ALL_API`): the same ids as the old registry, in `Dom::handlers`.
    #[cfg(test)]
    fn add_old_on_api(&self, it: &mut Interp<'_>, h: H, name: &str, len: u32, kind: HostFn) -> R<ObjId> {
        let (cur, needs_new) = (self.handlers.borrow().len() as u32, kind != HostFn::Method);
        self.handlers.borrow_mut().push((h, needs_new));
        let id = self.scoped.borrow().len() as u32;
        self.scoped.borrow_mut().push(Scoped { f: Fun::Old(h), needs_new, cur });
        bare_js_api::enter(it, |s| match s.host_function(name, id, len, kind) {
            Ok(l) => Ok(obj_of(s, l)),
            Err(e) => Err(s.engine().api_unerr(e)),
        })
    }

    /// Make a function of the old form: on the old registry, or on the interface in a test build with `BARE_ALL_API`.
    #[inline]
    pub(crate) fn add_old(&self, it: &mut Interp<'_>, h: H, name: &str, len: u32, kind: HostFn) -> R<ObjId> {
        #[cfg(test)]
        if all_on_api() {
            return self.add_old_on_api(it, h, name, len, kind);
        }
        let id = self.handlers.borrow().len() as u32;
        self.handlers.borrow_mut().push((h, kind != HostFn::Method));
        match kind {
            HostFn::Method => it.host_function(id, name, len, false),
            HostFn::Ctor => it.host_function(id, name, len, true),
            HostFn::CtorNewTarget => it.host_ctor_with_new_target(id, name, len),
        }
    }

    /// Does the table of methods put the methods on the interface? (A test build with `BARE_ALL_API` does, with the interned name lost.)
    #[inline(always)]
    pub(crate) fn methods_on_api(&self) -> bool {
        all_on_api()
    }

    /// The node of `this`, or `None`.
    #[inline]
    pub(crate) fn node_of_s(&self, s: &mut Sc<'_, '_>, this: Local<'_>) -> Option<NodeId> {
        self.node_of(s.engine().raw(this))
    }

    /// `Dom::nd` on a local.
    #[inline]
    pub(crate) fn nd_s<'s>(&self, s: &mut Sc<'s, '_>, this: Local<'s>) -> Res<'s, NodeId> {
        match self.node_of_s(s, this) {
            Some(n) => Ok(n),
            None => Err(illegal(s)),
        }
    }

    /// `Dom::el` on a local.
    #[inline]
    pub(crate) fn el_s<'s>(&self, s: &mut Sc<'s, '_>, this: Local<'s>) -> Res<'s, NodeId> {
        let n = self.nd_s(s, this)?;
        if self.is_element(n) { Ok(n) } else { Err(illegal(s)) }
    }

    /// `Dom::string_arg` on a local: `ToString`, the caps of a DOM string, and the UTF-8 text.
    pub(crate) fn string_arg_s<'s>(&self, s: &mut Sc<'s, '_>, v: Local<'s>) -> Res<'s, String> {
        // A string is read in place: `to_string` pushes a local only for another value.
        let r = match s.as_str(v) {
            Some(text) => self.text_of(text),
            None => {
                let t = s.to_string(v)?;
                match s.as_str(t) {
                    Some(text) => self.text_of(text),
                    None => return Err(s.type_error("not a string")),
                }
            }
        };
        match r {
            Ok((t, len)) => {
                s.charge((len / 32) as u64)?;
                Ok(t)
            }
            Err(e) => Err(s.engine().api_err(e)),
        }
    }

    /// The UTF-8 text of a string value and its length in units, or the error of a cap.
    fn text_of(&self, text: bare_js_api::Str<'_>) -> Result<(String, usize), Error> {
        let len = text.len();
        if len > self.lim.max_string_bytes {
            return Err(self.lim("dom-string"));
        }
        let t = match text {
            bare_js_api::Str::Latin1(b) => b.iter().map(|&c| c as char).collect::<String>(),
            bare_js_api::Str::Utf16(u) => String::from_utf16(u).map_err(|_| Error::Unsupported(unsupported("lone surrogate in a DOM string (the native document is UTF-8)")))?,
        };
        if t.len() > self.lim.max_string_bytes {
            return Err(self.lim("dom-string"));
        }
        Ok((t, len))
    }

    /// `Dom::js_str` into a local.
    pub(crate) fn js_str_s<'s>(&self, s: &mut Sc<'s, '_>, text: &str) -> Res<'s, Local<'s>> {
        if text.len() > self.lim.max_string_bytes {
            return Err(s.engine().api_err(self.lim("dom-string")));
        }
        s.charge((text.len() / 32) as u64)?;
        s.string(text)
    }
}

// ------------------------------------------------------------------------------------------ helpers of the handler bodies

/// An error of the old form as an error of the interface (the old form is kept for the engine to give back).
#[cold]
pub(crate) fn lift_err<'s>(s: &mut Sc<'s, '_>, e: Error) -> bare_js_api::Error<'s> {
    s.engine().api_err(e)
}

/// The `TypeError` of a method called on a wrong `this`.
#[cold]
#[inline(never)]
fn illegal<'s>(s: &mut Sc<'s, '_>) -> bare_js_api::Error<'s> {
    s.type_error("Illegal invocation")
}

/// Argument `i` as a raw value, for code of the old form. `None` when the call has no such argument.
pub(crate) fn raw_arg<'s>(s: &mut Sc<'s, '_>, a: Args<'s>, i: usize) -> Option<Value> {
    let l = a.get(i)?;
    Some(s.engine().raw(l))
}

/// A result of the old form as one of the interface.
#[inline]
pub(crate) fn lift<'s, T>(s: &mut Sc<'s, '_>, r: R<T>) -> Res<'s, T> {
    match r {
        Ok(v) => Ok(v),
        Err(e) => Err(lift_err(s, e)),
    }
}

/// A feature that the browser does not have, as an error of the interface.
pub(crate) fn unsupp<'s>(what: &'static str) -> bare_js_api::Error<'s> {
    bare_js_api::Error::Abort(Abort::Unsupported(what))
}

/// Is `v` an object (a function is one)?
#[inline]
pub(crate) fn is_object(s: &Sc<'_, '_>, v: Local<'_>) -> bool {
    matches!(s.type_of(v), Type::Object | Type::Function)
}

/// Is `v` `undefined` or `null`?
#[inline]
pub(crate) fn is_nullish(s: &Sc<'_, '_>, v: Local<'_>) -> bool {
    matches!(s.type_of(v), Type::Undefined | Type::Null)
}

/// Run a helper of the old form on the interpreter of `s`.
#[inline(always)]
pub(crate) fn old<'s, T>(s: &mut Sc<'s, '_>, f: impl FnOnce(&mut Interp<'_>) -> R<T>) -> Res<'s, T> {
    let it = s.engine();
    match f(it) {
        Ok(v) => Ok(v),
        Err(e) => Err(it.api_err(e)),
    }
}

/// `old` for a helper that gives a value.
#[inline(always)]
pub(crate) fn old_v<'s>(s: &mut Sc<'s, '_>, f: impl FnOnce(&mut Interp<'_>) -> R<Value>) -> Res<'s, Local<'s>> {
    let it = s.engine();
    match f(it) {
        Ok(v) => Ok(it.raw_local(&v)),
        Err(e) => Err(it.api_err(e)),
    }
}

/// Run code of the new form where the caller has an interpreter: a scope, the result as an old error.
pub(crate) fn run<'p, T>(it: &mut Interp<'p>, f: impl for<'s> FnOnce(&mut Sc<'s, 'p>) -> Res<'s, T>) -> R<T> {
    bare_js_api::enter(it, |s| match f(s) {
        Ok(v) => Ok(v),
        Err(e) => Err(s.engine().api_unerr(e)),
    })
}

/// `run` for code that gives a value.
pub(crate) fn run_v<'p>(it: &mut Interp<'p>, f: impl for<'s> FnOnce(&mut Sc<'s, 'p>) -> Res<'s, Local<'s>>) -> R<Value> {
    bare_js_api::enter(it, |s| match f(s) {
        Ok(l) => Ok(s.engine().raw(l)),
        Err(e) => Err(s.engine().api_unerr(e)),
    })
}

/// The arguments of a call to a handler of the new form: copies of `vals` in new slots. For code that runs rarely.
fn args_of<'s>(s: &mut Sc<'s, '_>, vals: &[Local<'s>]) -> Args<'s> {
    let e = s.engine();
    let base = e.stack_len();
    for &v in vals {
        let r = e.raw(v);
        e.raw_local(&r);
    }
    Args::from_slots(base, vals.len() as u32)
}

/// `h(this, vals)`: a call to a handler of the new form from code of the new form.
pub(crate) fn call_hs<'s>(d: &Dom, s: &mut Sc<'s, '_>, h: HS, this: Local<'s>, vals: &[Local<'s>]) -> Res<'s, Local<'s>> {
    let a = args_of(s, vals);
    h(d, s, this, a)
}

impl Dom {
    /// `Dom::lim` as an error of the interface.
    #[cold]
    pub(crate) fn lim_s<'s>(&self, s: &mut Sc<'s, '_>, what: &'static str) -> bare_js_api::Error<'s> {
        s.engine().api_err(self.lim(what))
    }

    /// The local of an object of the host (a prototype, an interface object).
    #[inline]
    pub(crate) fn obj_local<'s>(&self, s: &mut Sc<'s, '_>, o: ObjId) -> Local<'s> {
        s.engine().raw_local(&Value::Object(o))
    }

    /// `Dom::obj`: a new object of the host with the prototype `proto`.
    pub(crate) fn obj_s<'s>(&self, s: &mut Sc<'s, '_>, proto: ObjId) -> Res<'s, ObjId> {
        old(s, |it| self.obj(it, proto))
    }

    /// `Dom::obj` into a local: a new object with the prototype `proto`.
    pub(crate) fn obj_with_s<'s>(&self, s: &mut Sc<'s, '_>, proto: ObjId) -> Res<'s, Local<'s>> {
        let o = self.obj_s(s, proto)?;
        Ok(self.obj_local(s, o))
    }

    /// `def_method` on an object that a handler made: a function of the new form as a method of `target`. `enumerable` is false for an
    /// ordinary method and true for a WebIDL one.
    pub(crate) fn def_method_l<'s>(&self, s: &mut Sc<'s, '_>, target: Local<'s>, name: &str, len: u32, h: HS, enumerable: bool) -> Res<'s, ()> {
        let f = self.add_fn_s(s, h, name, len, HostFn::Method)?;
        s.define_str(target, name, f, Attrs { writable: true, enumerable, configurable: true })
    }

    /// `def_get` on an object that a handler made: an accessor of functions of the new form.
    pub(crate) fn def_get_l<'s>(&self, s: &mut Sc<'s, '_>, target: Local<'s>, name: &str, get: HS, set: Option<HS>) -> Res<'s, ()> {
        let g = self.add_fn_s(s, get, &format!("get {name}"), 0, HostFn::Method)?;
        let st = match set {
            Some(h) => Some(self.add_fn_s(s, h, &format!("set {name}"), 1, HostFn::Method)?),
            None => None,
        };
        s.define_accessor_str(target, name, Some(g), st, Attrs { writable: false, enumerable: true, configurable: true })
    }

    /// `Dom::new_source`.
    pub(crate) fn new_source_s<'s>(&self, s: &mut Sc<'s, '_>, k: crate::browser_sources::Source, proto: Option<ObjId>) -> Res<'s, ObjId> {
        old(s, |it| self.new_source(it, k, proto))
    }

    /// Is `v` the object `o`?
    pub(crate) fn is_obj_s(&self, s: &mut Sc<'_, '_>, v: Local<'_>, o: ObjId) -> bool {
        matches!(s.engine().raw(v), Value::Object(x) if x == o)
    }

    /// The wrapper of `node` (`Dom::wrap`).
    pub(crate) fn wrap_s<'s>(&self, s: &mut Sc<'s, '_>, node: NodeId) -> Res<'s, Local<'s>> {
        old_v(s, |it| self.wrap(it, node))
    }

    /// `Dom::wrap_opt`.
    pub(crate) fn wrap_opt_s<'s>(&self, s: &mut Sc<'s, '_>, n: Option<NodeId>) -> Res<'s, Local<'s>> {
        match n {
            Some(n) => self.wrap_s(s, n),
            None => Ok(s.null()),
        }
    }

    /// `Dom::dom_err` on a scope: the exception to throw.
    pub(crate) fn dom_err_s<'s>(&self, s: &mut Sc<'s, '_>, name: &str, msg: &str) -> bare_js_api::Error<'s> {
        let r = (|| -> Res<'s, Local<'s>> {
            let proto = self.obj_local(s, self.p().domexc);
            let o = s.new_error(proto, msg)?;
            let n = self.js_str_s(s, name)?;
            let attrs = Attrs { writable: true, enumerable: false, configurable: true };
            s.define_str(o, "name", n, attrs)?;
            let code = s.number(domexc_codes(name) as f64);
            s.define_str(o, "code", code, attrs)?;
            Ok(o)
        })();
        match r {
            Ok(v) => bare_js_api::Error::Throw(v),
            Err(e) => e,
        }
    }

    /// `Dom::arg_node`.
    pub(crate) fn arg_node_s<'s>(&self, s: &mut Sc<'s, '_>, a: Args<'s>, i: usize) -> Res<'s, NodeId> {
        let v = arg_s(s, a, i);
        match self.node_of_s(s, v) {
            Some(n) => Ok(n),
            None => Err(s.type_error(&format!("parameter {} is not of type 'Node'", i + 1))),
        }
    }

    /// `Dom::string_arg` of the argument `i`.
    pub(crate) fn arg_str_s<'s>(&self, s: &mut Sc<'s, '_>, a: Args<'s>, i: usize) -> Res<'s, String> {
        let v = arg_s(s, a, i);
        self.string_arg_s(s, v)
    }

    /// `Dom::selectors_at`.
    pub(crate) fn selectors_at_s<'s>(&self, s: &mut Sc<'s, '_>, text: &str, scope: NodeId) -> Res<'s, Rc<Vec<Selector>>> {
        old(s, |it| self.selectors_at(it, text, scope))
    }

    /// `Dom::room`, with the cap as an error of the interface.
    pub(crate) fn room_s<'s>(&self, s: &mut Sc<'s, '_>, doc: &Document, nodes: usize, text: usize) -> Res<'s, ()> {
        match self.room(doc, nodes, text) {
            Ok(()) => Ok(()),
            Err(e) => Err(lift_err(s, e)),
        }
    }

    /// `Dom::attr_room`, with the cap as an error of the interface.
    pub(crate) fn attr_room_s<'s>(&self, s: &mut Sc<'s, '_>, doc: &Document, el: NodeId, name: &str, value: &str) -> Res<'s, ()> {
        match self.attr_room(doc, el, name, value) {
            Ok(()) => Ok(()),
            Err(e) => Err(lift_err(s, e)),
        }
    }
}

// ------------------------------------------------------------------------------------------ bridges to code of the old form
// Each of these gives a function of a file that has not moved a form on `Scope`. The file that owns the function removes the bridge when it moves.

pub(crate) fn wrap_coll_s<'s>(d: &Dom, s: &mut Sc<'s, '_>, ids: &[NodeId]) -> Res<'s, Local<'s>> {
    old_v(s, |it| crate::browser_gaps::wrap_coll(d, it, ids))
}

pub(crate) fn ns_put_s<'s>(d: &Dom, s: &mut Sc<'s, '_>, n: NodeId, existing: Option<bare_html::dom::Atom>, qname: &str, value: &str) -> Res<'s, ()> {
    old(s, |it| crate::browser_namespace::put(d, it, n, existing, None, qname, value))
}

pub(crate) fn media_src_changed_s<'s>(d: &Dom, s: &mut Sc<'s, '_>, n: NodeId) -> Res<'s, ()> {
    crate::browser_media::src_changed(d, s, n)
}

pub(crate) fn script_src_changed_s<'s>(d: &Dom, s: &mut Sc<'s, '_>, n: NodeId) -> Res<'s, ()> {
    old(s, |it| d.script_src_changed(it, n))
}

pub(crate) fn script_inserted_s<'s>(d: &Dom, s: &mut Sc<'s, '_>, n: NodeId) -> Res<'s, ()> {
    old(s, |it| d.script_inserted(it, n))
}

pub(crate) fn file_from_ref_s<'s>(d: &Dom, s: &mut Sc<'s, '_>, f: &FileRef) -> Res<'s, Local<'s>> {
    old_v(s, |it| crate::browser_web::file_from_ref(d, it, f))
}

pub(crate) fn radio_group_s<'s>(d: &Dom, s: &mut Sc<'s, '_>, n: NodeId) -> Res<'s, Vec<NodeId>> {
    old(s, |it| d.radio_group(it, n))
}

pub(crate) fn set_value_attr_s<'s>(d: &Dom, s: &mut Sc<'s, '_>, n: NodeId, text: &str) -> Res<'s, ()> {
    old(s, |it| d.set_value_attr(it, n, text))
}

pub(crate) fn tag_fragments_s<'s>(d: &Dom, s: &mut Sc<'s, '_>) -> Res<'s, ()> {
    old(s, |it| crate::browser_elements::tag_fragments(d, it))
}

pub(crate) fn put_input_value_s<'s>(d: &Dom, s: &mut Sc<'s, '_>, n: NodeId, v: String) -> Res<'s, ()> {
    old(s, |it| d.put_input_value(Some(it), n, v))
}

/// The window object of the frame of `n` (`browser_frames::window_of`), as a local.
pub(crate) fn window_of_s<'s>(d: &Dom, s: &mut Sc<'s, '_>, n: NodeId) -> Option<Local<'s>> {
    let o = crate::browser_frames::window_of(d, n)?;
    Some(d.obj_local(s, o))
}

/// Is `v` an `Attr` object (a key of `Dom::attr`)?
pub(crate) fn is_attr_node_s(d: &Dom, s: &mut Sc<'_, '_>, v: Local<'_>) -> bool {
    match s.engine().raw(v) {
        Value::Object(o) => d.attr.s.borrow().attrs.contains_key(&o),
        _ => false,
    }
}

/// The object of a local that holds one.
fn obj_of(s: &mut Sc<'_, '_>, l: Local<'_>) -> ObjId {
    match s.engine().raw(l) {
        Value::Object(o) => o,
        _ => unreachable!("a function is an object"),
    }
}

/// The object of a local, or `None` for another value.
#[inline]
pub(crate) fn obj_id(s: &mut Sc<'_, '_>, v: Local<'_>) -> Option<ObjId> {
    match s.engine().raw(v) {
        Value::Object(o) => Some(o),
        _ => None,
    }
}

/// The argument `i`, or `undefined`.
#[inline]
pub(crate) fn arg_s<'s>(s: &mut Sc<'s, '_>, a: Args<'s>, i: usize) -> Local<'s> {
    match a.get(i) {
        Some(v) => v,
        None => s.undefined(),
    }
}

/// Why a script or a call through the facade of `Browser` failed.
#[derive(Debug, PartialEq, Eq)]
pub enum Failed {
    /// An exception that is an object: its `name` and `message` as text.
    Throw { name: String, message: String },
    /// Any other error, as text (an exception that is not an object, a limit, an unsupported feature).
    Other(String),
}

/// The facade of `Browser` for tests, examples and drivers: they hold values as [`Persistent`] and need not name `Interp`, `Value` or
/// `ObjId` (`docs/design-host-handlers.md`, question 5). Their text goes through `Interp::display`, as the old tests did.
impl Browser {
    /// Run `f` in a handle scope of the engine.
    pub fn with_scope<R>(&mut self, f: impl for<'s> FnOnce(&mut Sc<'s, 'static>) -> R) -> R {
        bare_js_api::enter(&mut self.it, f)
    }

    /// Run script text (`Browser::run_script`) and keep the completion value.
    pub fn eval(&mut self, src: &str) -> Result<Persistent, Failed> {
        let r = self.run_script(src);
        self.keep(r)
    }

    /// The text of a value (`Interp::display`).
    pub fn display(&mut self, v: &Persistent) -> String {
        self.with_scope(|s| {
            let l = s.local(v);
            s.display(l)
        })
    }

    /// `ToString(v[key])`, or `?` when it fails.
    pub fn get_str(&mut self, v: &Persistent, key: &str) -> String {
        self.with_scope(|s| {
            let l = s.local(v);
            let r = s.get_str(l, key).and_then(|x| s.to_string(x));
            match r {
                Ok(t) => s.as_str(t).map(|t| t.to_string_lossy()).unwrap_or_default(),
                Err(_) => "?".into(),
            }
        })
    }

    /// `f.call(this, ...args)`; `this` is `undefined` for `None`.
    pub fn call(&mut self, f: &Persistent, this: Option<&Persistent>, args: &[&Persistent]) -> Result<Persistent, Failed> {
        let r = self.with_scope(|s| {
            let f = s.local(f);
            let this = match this {
                Some(t) => s.local(t),
                None => s.undefined(),
            };
            let args: Vec<_> = args.iter().map(|&a| s.local(a)).collect();
            match s.call(f, this, &args) {
                Ok(l) => Ok(s.engine().raw(l)),
                Err(e) => Err(s.engine().api_unerr(e)),
            }
        });
        self.keep(r)
    }

    /// The value of a result of the old engine as a [`Persistent`], or the failure as text.
    fn keep(&mut self, r: R<Value>) -> Result<Persistent, Failed> {
        match r {
            Ok(v) => Ok(self.with_scope(|s| {
                let l = s.engine().raw_local(&v);
                s.persist(l)
            })),
            Err(Error::Throw(v @ Value::Object(_))) => {
                let p = self.with_scope(|s| {
                    let l = s.engine().raw_local(&v);
                    s.persist(l)
                });
                Err(Failed::Throw { name: self.get_str(&p, "name"), message: self.get_str(&p, "message") })
            }
            Err(Error::Throw(v)) => Err(Failed::Other(format!("throw:{}", self.it.display(v)))),
            Err(e) => Err(Failed::Other(format!("{e:?}"))),
        }
    }
}
