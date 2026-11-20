//! Host functions on the interface (`docs/design-host-handlers.md`, step H0). `Dom` has two registries. A handler of the old form (`H`:
//! `Interp`, `Value`) is in `Dom::handlers` and runs through the old `Host::call`. A handler of the new form (`HS`: `Scope`, `Local`) is in
//! `Dom::scoped` and runs through the interface `Host::call`. The engine tells them apart by `API_ID` in the function id, so the old path
//! has no new branch beyond one test of that bit. Both entries run the same `Dom::dispatch_host`, so a handler sees the same hooks in either.
//!
//! A test build can put every old handler on the interface too (`BARE_ALL_API=1`): the entry then converts `this` and the arguments to raw
//! values, calls the old body and pushes the result. It proves that the interface entry does what the old entry does.

use crate::browser::*;
use bare_html::dom::NodeId;
use bare_js::eval::{Error, Interp};
use bare_js::value::{ObjId, Value};
use bare_js_api::{Args, Engine, HostFn, Local, Persistent, Scope};

/// A host method of the new form: the DOM, the scope, `this`, the arguments.
pub(crate) type HS = for<'s, 'p> fn(&Dom, &mut Sc<'s, 'p>, Local<'s>, Args<'s>) -> Res<'s, Local<'s>>;
pub(crate) type Sc<'s, 'p> = Scope<'s, Interp<'p>>;
pub(crate) type Res<'s, T> = Result<T, bare_js_api::Error<'s>>;

/// The `style.cur` of a function of the new form: its index in `Dom::scoped`, with this bit, so it cannot equal an old function id.
const CUR_NEW: u32 = 1 << 31;

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
    /// One host call: the hooks around a handler, for both entries. `bad_new` is a call of a constructor without `new`; `h` names it.
    #[inline(always)]
    pub(crate) fn dispatch_host<'p, X: Entry<'p>, T>(&self, x: &mut X, cur: u32, bad_new: bool, h: Option<H>, body: impl FnOnce(&mut X) -> Result<T, X::Err>) -> Result<T, X::Err> {
        self.style.cur.set(cur);
        if bad_new {
            return Err(self.bad_new(x, h));
        }
        let r = if self.ce.on.get() { self.dispatch_ce(x, body) } else { body(x) };
        if self.mo.armed() {
            self.mo_done(x)?;
        }
        r
    }

    #[cold]
    #[inline(never)]
    fn bad_new<'p, X: Entry<'p>>(&self, x: &mut X, h: Option<H>) -> X::Err {
        let e = crate::browser_custom::needs_new(x.it(), h);
        x.err(e)
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

    /// The entry of the interface `Host::call`.
    pub(crate) fn call_scoped<'s, 'p>(&self, s: &mut Sc<'s, 'p>, id: u32, this: Local<'s>, args: Args<'s>, is_new: bool) -> Res<'s, Local<'s>> {
        let e = self.scoped.borrow()[id as usize];
        #[cfg(test)]
        let h = match e.f {
            Fun::Old(h) => Some(h),
            Fun::New(_) => None,
        };
        #[cfg(not(test))]
        let h = None;
        self.dispatch_host(s, e.cur, e.needs_new && !is_new, h, |s| match e.f {
            Fun::New(f) => f(self, s, this, args),
            #[cfg(test)]
            Fun::Old(h) => self.call_old(s, h, this, args),
        })
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
        bare_js_api::enter(it, |s| match self.add_fn_s(s, f, name, len, kind) {
            Ok(l) => Ok(obj_of(s, l)),
            Err(e) => Err(s.engine().api_unerr(e)),
        })
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
    pub(crate) fn node_of_s(&self, s: &mut Sc<'_, '_>, this: Local<'_>) -> Option<NodeId> {
        self.node_of(s.engine().raw(this))
    }

    /// `Dom::nd` on a local.
    pub(crate) fn nd_s<'s>(&self, s: &mut Sc<'s, '_>, this: Local<'s>) -> Res<'s, NodeId> {
        match self.node_of_s(s, this) {
            Some(n) => Ok(n),
            None => Err(s.type_error("Illegal invocation")),
        }
    }

    /// `Dom::el` on a local.
    pub(crate) fn el_s<'s>(&self, s: &mut Sc<'s, '_>, this: Local<'s>) -> Res<'s, NodeId> {
        let n = self.nd_s(s, this)?;
        if self.is_element(n) { Ok(n) } else { Err(s.type_error("Illegal invocation")) }
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

/// The object of a local that holds one.
fn obj_of(s: &mut Sc<'_, '_>, l: Local<'_>) -> ObjId {
    match s.engine().raw(l) {
        Value::Object(o) => o,
        _ => unreachable!("a function is an object"),
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
