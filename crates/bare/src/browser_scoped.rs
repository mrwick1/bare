//! Host functions on the interface (`docs/design-host-handlers.md`, step H0). A host function is a handler of the form `HS` (`Scope`,
//! `Local`). It is in `Dom::scoped` and runs through the interface `Host::call` (`Dom::call_scoped`). `Dom::call_scoped` spells the hooks
//! out with no closure, because it is the hot path.

use crate::eng::{Eng, Fault, Ops, Val};
use crate::browser::*;
use bare_css::selector::Selector;
use bare_html::dom::{Document, NodeId};
use std::rc::Rc;
pub(crate) use bare_js_api::{Attrs, HostFn, Intrinsic};
use bare_js_api::{Abort, Args, Engine, Local, Persistent, Scope, Type};

/// A host method of the new form: the DOM, the scope, `this`, the arguments.
pub(crate) type HS<E> = for<'s> fn(&Dom<E>, &mut Sc<'s, E>, Local<'s>, Args<'s>) -> Res<'s, Local<'s>>;
pub(crate) type Sc<'s, E> = Scope<'s, <E as Eng>::Vm>;
pub(crate) type Res<'s, T> = Result<T, bare_js_api::Error<'s>>;

/// The `style.cur` of a function of the new form: its index in `Dom::scoped`, with this bit, so it cannot equal an old function id.
pub(crate) const CUR_NEW: u32 = 1 << 31;

#[derive(Clone, Copy)]
pub(crate) struct Scoped<E: Eng> {
    f: HS<E>,
    needs_new: bool,
    /// What `call_scoped` stores in `style.cur`.
    cur: u32,
}

/// What a row of a table makes (see [`Row`]).
#[derive(Clone, Copy)]
pub(crate) enum Mem<E: Eng> {
    /// A method with this `length`.
    Method(HS<E>, u32),
    /// A getter, and a setter when there is one.
    Get(HS<E>, Option<HS<E>>),
    /// One function that is the getter and the setter: a lazy global.
    Both(HS<E>),
}

/// A member of a static table of host members. `on` is a mask of the targets that the install call names (bit `i` is `targets[i]`).
#[derive(Clone, Copy)]
pub(crate) struct Row<E: Eng> {
    on: u32,
    name: &'static str,
    mem: Mem<E>,
    attrs: Attrs,
}

impl<E: Eng> Row<E> {
    /// A method (not enumerable).
    pub(crate) const fn method(on: u32, name: &'static str, len: u32, h: HS<E>) -> Row<E> {
        Row { on, name, mem: Mem::Method(h, len), attrs: Attrs::METHOD }
    }

    /// An accessor of a WebIDL attribute (enumerable), or a getter when `set` is `None`.
    pub(crate) const fn get(on: u32, name: &'static str, get: HS<E>, set: Option<HS<E>>) -> Row<E> {
        Row { on, name, mem: Mem::Get(get, set), attrs: Attrs { writable: false, enumerable: true, configurable: true } }
    }

    /// A lazy global: one function is the getter and the setter (not enumerable).
    pub(crate) const fn both(on: u32, name: &'static str, h: HS<E>) -> Row<E> {
        Row { on, name, mem: Mem::Both(h), attrs: Attrs { writable: false, enumerable: false, configurable: true } }
    }

    /// The same row with `enumerable` set.
    pub(crate) const fn enumerable(mut self) -> Row<E> {
        self.attrs.enumerable = true;
        self
    }
}

fn no_handler<'s, E: Eng>(_: &Dom<E>, _: &mut Sc<'s, E>, _: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    unreachable!("a placeholder of a table")
}

/// How many handlers the rows hold (a getter with a setter holds two).
pub(crate) const fn handlers<E: Eng>(rows: &[Row<E>]) -> usize {
    let (mut i, mut n) = (0, 0);
    while i < rows.len() {
        n += match rows[i].mem {
            Mem::Get(_, Some(_)) => 2,
            _ => 1,
        };
        i += 1;
    }
    n
}

/// The rows of a table as the table of `Engine::install_table` and the handlers that its ids name. Both are made at compile time, so a
/// window pays for neither. Row `i` of `defs` has the ids of its handlers in `hs`.
pub(crate) struct Table<E: Eng, const N: usize, const H: usize> {
    defs: [bare_js_api::Def; N],
    hs: [HS<E>; H],
}

impl<E: Eng, const N: usize, const H: usize> Table<E, N, H> {
    pub(crate) const fn new(rows: &[Row<E>]) -> Self {
        assert!(rows.len() == N && handlers(rows) == H);
        let mut defs = [bare_js_api::Def::number("", 0, 0.0); N];
        let mut hs: [HS<E>; H] = [no_handler; H];
        let (mut i, mut h) = (0, 0);
        while i < N {
            let r = rows[i];
            defs[i] = match r.mem {
                Mem::Method(f, len) => {
                    hs[h] = f;
                    h += 1;
                    bare_js_api::Def::method(r.name, r.on, h as u32 - 1, len)
                }
                Mem::Get(g, set) => {
                    hs[h] = g;
                    h += 1;
                    let st = match set {
                        Some(f) => {
                            hs[h] = f;
                            h += 1;
                            Some(h as u32 - 1)
                        }
                        None => None,
                    };
                    bare_js_api::Def::accessor(r.name, r.on, if st.is_some() { h as u32 - 2 } else { h as u32 - 1 }, st)
                }
                Mem::Both(f) => {
                    hs[h] = f;
                    h += 1;
                    bare_js_api::Def::both(r.name, r.on, h as u32 - 1)
                }
            }
            .with(r.attrs);
            i += 1;
        }
        Table { defs, hs }
    }
}

/// A static table of host members: `table!(name = [Row::method(1, "item", 1, item), ...])` makes the module `name` with the table
/// `name::T`. The handlers are in scope of the module as in the file.
macro_rules! table {
    ($name:ident = [$($row:expr),* $(,)?]) => {
        mod $name {
            #[allow(unused_imports)]
            use super::*;
            #[allow(dead_code)]
            type E = $crate::eng_old::Old;
            use $crate::browser_scoped::{handlers, Table};
            /// How many rows and handlers: they do not depend on the engine, so the old engine's rows count them.
            const N: usize = [$(stringify!($row)),*].len();
            const H: usize = handlers::<$crate::eng_old::Old>(&[$($row),*]);
            /// The table for the engine `E`: `&name::T::<E>::V`.
            pub(super) struct T<E: $crate::eng::Eng>(std::marker::PhantomData<E>);
            impl<E: $crate::eng::Eng> T<E> {
                pub(super) const V: Table<E, N, H> = Table::new(&[$($row),*]);
            }
        }
    };
}
pub(crate) use table;

impl<E: Eng> Dom<E> {
    #[cold]
    #[inline(never)]
    fn bad_new<'s>(&self, x: &mut Sc<'s, E>) -> bare_js_api::Error<'s> {
        let e = crate::browser_custom::needs_new::<E>(x.engine());
        x.engine().api_err(e)
    }

    /// `bad_new` for a handler of the new form: the constructors of `browser_geom` and `browser_custom` have their own message.
    #[cold]
    #[inline(never)]
    fn bad_new_s<'s>(&self, s: &mut Sc<'s, E>, f: HS<E>) -> bare_js_api::Error<'s> {
        let it = s.engine();
        match crate::browser_geom::needs_new(it, f).or_else(|| crate::browser_custom::needs_new_s(it, f)) {
            Some(e) => s.engine().api_err(e),
            None => self.bad_new(s),
        }
    }

    /// The mutation observer hook after a host call.
    #[inline(never)]
    fn mo_done<'s>(&self, x: &mut Sc<'s, E>) -> Result<(), bare_js_api::Error<'s>> {
        match self.mo_after(x.engine()) {
            Ok(()) => Ok(()),
            Err(e) => Err(x.engine().api_err(e)),
        }
    }

    /// `body` with a scope of custom element reactions around it.
    #[inline(never)]
    fn dispatch_ce<'s, T>(&self, x: &mut Sc<'s, E>, body: impl FnOnce(&mut Sc<'s, E>) -> Result<T, bare_js_api::Error<'s>>) -> Result<T, bare_js_api::Error<'s>> {
        self.ce_enter();
        let r = body(x);
        let q = self.ce_pop(x.engine());
        match (r, q) {
            (Ok(v), Ok(())) => Ok(v),
            (Err(e), _) => Err(e),
            (Ok(_), Err(e)) => Err(x.engine().api_err(e)),
        }
    }

    /// The entry of the interface `Host::call`: the hooks around a handler, with no closure. The call path of a handler is the hot path,
    /// and a closure that both the plain and the custom element branch use sits in memory first.
    pub(crate) fn call_scoped<'s>(&self, s: &mut Sc<'s, E>, id: u32, this: Local<'s>, args: Args<'s>, is_new: bool) -> Res<'s, Local<'s>> {
        // SAFETY: the entry is copied out at once. Nothing runs between the read and the copy, so no push (`add_fn_s`) can move the vector under it.
        let Scoped { f, needs_new, cur } = unsafe { &*self.scoped.as_ptr() }[id as usize];
        self.style.cur.set(cur);
        if needs_new && !is_new {
            return Err(self.bad_new_s(s, f));
        }
        let r = if self.ce.on.get() { self.call_scoped_ce(s, f, this, args) } else { f(self, s, this, args) };
        if self.mo.armed() {
            self.mo_done(s)?;
        }
        r
    }

    /// `call_scoped` with a scope of custom element reactions around the handler.
    #[inline(never)]
    fn call_scoped_ce<'s>(&self, s: &mut Sc<'s, E>, f: HS<E>, this: Local<'s>, args: Args<'s>) -> Res<'s, Local<'s>> {
        self.dispatch_ce(s, |s| f(self, s, this, args))
    }

    /// Install a static table (`Engine::install_table`): the handlers of the table get the next ids of `Dom::scoped`.
    pub(crate) fn install_rows<'s, const N: usize, const H: usize>(&self, s: &mut Sc<'s, E>, targets: &[Local<'s>], t: &'static Table<E, N, H>) -> Res<'s, ()> {
        let base = {
            let mut v = self.scoped.borrow_mut();
            let base = v.len() as u32;
            v.extend(t.hs.iter().enumerate().map(|(i, &f)| Scoped { f, needs_new: false, cur: (base + i as u32) | CUR_NEW }));
            base
        };
        s.install_table(targets, base, &t.defs)
    }

    /// Make a function of the new form. Its id is its index in `Dom::scoped`.
    pub(crate) fn add_fn_s<'s>(&self, s: &mut Sc<'s, E>, f: HS<E>, name: &str, len: u32, kind: HostFn) -> Res<'s, Local<'s>> {
        let id = self.scoped.borrow().len() as u32;
        self.scoped.borrow_mut().push(Scoped { f, needs_new: kind != HostFn::Method, cur: id | CUR_NEW });
        s.host_function(name, id, len, kind)
    }

    /// `add_fn_s` for code that has an interpreter and wants the function object.
    pub(crate) fn add_fn_s_obj(&self, it: &mut E::Vm, f: HS<E>, name: &str, len: u32, kind: HostFn) -> R<E, E::Obj> {
        bare_js_api::enter(it, |s| self.add_fn_s_raw(s, f, name, len, kind))
    }

    /// `add_fn_s_obj` inside a scope that the caller holds, for a loop that makes many functions in one scope.
    pub(crate) fn add_fn_s_raw(&self, s: &mut Sc<'_, E>, f: HS<E>, name: &str, len: u32, kind: HostFn) -> R<E, E::Obj> {
        match self.add_fn_s(s, f, name, len, kind) {
            Ok(l) => Ok(obj_of::<E>(s, l)),
            Err(e) => Err(s.engine().api_unerr(e)),
        }
    }

    /// The node of `this`, or `None`.
    #[inline]
    pub(crate) fn node_of_s(&self, s: &mut Sc<'_, E>, this: Local<'_>) -> Option<NodeId> {
        self.node_of(s.engine().raw(this))
    }

    /// `Dom::nd` on a local.
    #[inline]
    pub(crate) fn nd_s<'s>(&self, s: &mut Sc<'s, E>, this: Local<'s>) -> Res<'s, NodeId> {
        match self.node_of_s(s, this) {
            Some(n) => Ok(n),
            None => Err(illegal::<E>(s)),
        }
    }

    /// `Dom::el` on a local.
    #[inline]
    pub(crate) fn el_s<'s>(&self, s: &mut Sc<'s, E>, this: Local<'s>) -> Res<'s, NodeId> {
        let n = self.nd_s(s, this)?;
        if self.is_element(n) { Ok(n) } else { Err(illegal::<E>(s)) }
    }

    /// `Dom::string_arg` on a local: `ToString`, the caps of a DOM string, and the UTF-8 text.
    pub(crate) fn string_arg_s<'s>(&self, s: &mut Sc<'s, E>, v: Local<'s>) -> Res<'s, String> {
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
    fn text_of(&self, text: bare_js_api::Str<'_>) -> Result<(String, usize), Fault<E>> {
        let len = text.len();
        if len > self.lim.max_string_bytes {
            return Err(self.lim("dom-string"));
        }
        let t = match text {
            bare_js_api::Str::Latin1(b) => b.iter().map(|&c| c as char).collect::<String>(),
            bare_js_api::Str::Utf16(u) => String::from_utf16(u).map_err(|_| Fault::Unsupported("lone surrogate in a DOM string (the native document is UTF-8)"))?,
        };
        if t.len() > self.lim.max_string_bytes {
            return Err(self.lim("dom-string"));
        }
        Ok((t, len))
    }

    /// `Dom::js_str` into a local.
    pub(crate) fn js_str_s<'s>(&self, s: &mut Sc<'s, E>, text: &str) -> Res<'s, Local<'s>> {
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
pub(crate) fn lift_err<'s, E: Eng>(s: &mut Sc<'s, E>, e: Fault<E>) -> bare_js_api::Error<'s> {
    s.engine().api_err(e)
}

/// The `TypeError` of a method called on a wrong `this`.
#[cold]
#[inline(never)]
fn illegal<'s, E: Eng>(s: &mut Sc<'s, E>) -> bare_js_api::Error<'s> {
    s.type_error("Illegal invocation")
}

/// Argument `i` as a raw value, for code of the old form. `None` when the call has no such argument.
pub(crate) fn raw_arg<'s, E: Eng>(s: &mut Sc<'s, E>, a: Args<'s>, i: usize) -> Option<E::Raw> {
    let l = a.get(i)?;
    Some(s.engine().raw(l))
}

/// A result of the old form as one of the interface.
#[inline]
pub(crate) fn lift<'s, E: Eng, T>(s: &mut Sc<'s, E>, r: R<E, T>) -> Res<'s, T> {
    match r {
        Ok(v) => Ok(v),
        Err(e) => Err(lift_err::<E>(s, e)),
    }
}

/// A feature that the browser does not have, as an error of the interface.
pub(crate) fn unsupp<'s>(what: &'static str) -> bare_js_api::Error<'s> {
    bare_js_api::Error::Abort(Abort::Unsupported(what))
}

/// Is `v` an object (a function is one)?
#[inline]
pub(crate) fn is_object<E: Eng>(s: &Sc<'_, E>, v: Local<'_>) -> bool {
    matches!(s.type_of(v), Type::Object | Type::Function)
}

/// Property names that a handler reads on every call, kept as strings that the engine holds (`Persistent`). The table is made at its
/// first use. A read then costs one handle and a key check, not a lookup of the text in the atom table (several hundred instructions).
#[derive(Default)]
pub(crate) struct Names(std::cell::OnceCell<Box<[Persistent]>>);

impl Names {
    /// The name `list[i]` as a local of this scope. `list` is the same list at every call.
    #[inline]
    pub(crate) fn local<'s, E: Eng>(&self, s: &mut Sc<'s, E>, list: &[&str], i: usize) -> Res<'s, Local<'s>> {
        match self.0.get() {
            Some(t) => Ok(s.local(&t[i])),
            None => self.make::<E>(s, list, i),
        }
    }

    #[cold]
    #[inline(never)]
    fn make<'s, E: Eng>(&self, s: &mut Sc<'s, E>, list: &[&str], i: usize) -> Res<'s, Local<'s>> {
        let mut t = Vec::with_capacity(list.len());
        for n in list {
            let l = s.string(n)?;
            t.push(s.persist(l));
        }
        let t = self.0.get_or_init(|| t.into_boxed_slice());
        Ok(s.local(&t[i]))
    }
}

/// Is `v` `undefined` or `null`?
#[inline]
pub(crate) fn is_nullish<E: Eng>(s: &Sc<'_, E>, v: Local<'_>) -> bool {
    matches!(s.type_of(v), Type::Undefined | Type::Null)
}

/// Run a helper of the old form on the interpreter of `s`.
#[inline(always)]
pub(crate) fn old<'s, E: Eng, T>(s: &mut Sc<'s, E>, f: impl FnOnce(&mut E::Vm) -> R<E, T>) -> Res<'s, T> {
    let it = s.engine();
    match f(it) {
        Ok(v) => Ok(v),
        Err(e) => Err(it.api_err(e)),
    }
}

/// `old` for a helper that gives a value.
#[inline(always)]
pub(crate) fn old_v<'s, E: Eng>(s: &mut Sc<'s, E>, f: impl FnOnce(&mut E::Vm) -> R<E, E::Raw>) -> Res<'s, Local<'s>> {
    let it = s.engine();
    match f(it) {
        Ok(v) => Ok(it.raw_local(&v)),
        Err(e) => Err(it.api_err(e)),
    }
}

/// The iterator result `{ value, done }`.
pub(crate) fn iter_result_s<'s, E: Eng>(s: &mut Sc<'s, E>, value: Local<'s>, done: bool) -> Res<'s, Local<'s>> {
    let value = s.engine().raw(value);
    old_v::<E>(s, |it| it.host_iter_result(value, done))
}

/// Run code of the new form where the caller has an interpreter: a scope, the result as an old error.
pub(crate) fn run<'p, E: Eng, T>(it: &mut E::Vm, f: impl for<'s> FnOnce(&mut Sc<'s, E>) -> Res<'s, T>) -> R<E, T> {
    bare_js_api::enter(it, |s| match f(s) {
        Ok(v) => Ok(v),
        Err(e) => Err(s.engine().api_unerr(e)),
    })
}

/// `run` for code that gives a value.
pub(crate) fn run_v<'p, E: Eng>(it: &mut E::Vm, f: impl for<'s> FnOnce(&mut Sc<'s, E>) -> Res<'s, Local<'s>>) -> R<E, E::Raw> {
    bare_js_api::enter(it, |s| match f(s) {
        Ok(l) => Ok(s.engine().raw(l)),
        Err(e) => Err(s.engine().api_unerr(e)),
    })
}

/// The arguments of a call to a handler of the new form: copies of `vals` in new slots. For code that runs rarely.
fn args_of<'s, E: Eng>(s: &mut Sc<'s, E>, vals: &[Local<'s>]) -> Args<'s> {
    let e = s.engine();
    let base = e.stack_len();
    for &v in vals {
        let r = e.raw(v);
        e.raw_local(&r);
    }
    Args::from_slots(base, vals.len() as u32)
}

/// `h(this, vals)`: a call to a handler of the new form from code of the new form.
pub(crate) fn call_hs<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, h: HS<E>, this: Local<'s>, vals: &[Local<'s>]) -> Res<'s, Local<'s>> {
    let a = args_of::<E>(s, vals);
    h(d, s, this, a)
}

impl<E: Eng> Dom<E> {
    /// `Dom::lim` as an error of the interface.
    #[cold]
    pub(crate) fn lim_s<'s>(&self, s: &mut Sc<'s, E>, what: &'static str) -> bare_js_api::Error<'s> {
        s.engine().api_err(self.lim(what))
    }

    /// The local of an object of the host (a prototype, an interface object).
    #[inline]
    pub(crate) fn obj_local<'s>(&self, s: &mut Sc<'s, E>, o: E::Obj) -> Local<'s> {
        s.engine().raw_local(&E::obj_raw(o))
    }

    /// `Dom::obj`: a new object of the host with the prototype `proto`.
    pub(crate) fn obj_s<'s>(&self, s: &mut Sc<'s, E>, proto: E::Obj) -> Res<'s, E::Obj> {
        let o = self.obj_with_s(s, proto)?;
        Ok(obj_of::<E>(s, o))
    }

    /// `Dom::obj(it, it.object_prototype())`: a new ordinary object.
    pub(crate) fn plain_s<'s>(&self, s: &mut Sc<'s, E>) -> Res<'s, E::Obj> {
        let o = s.new_object()?;
        Ok(obj_of::<E>(s, o))
    }

    /// `Dom::obj` into a local: a new object with the prototype `proto`.
    pub(crate) fn obj_with_s<'s>(&self, s: &mut Sc<'s, E>, proto: E::Obj) -> Res<'s, Local<'s>> {
        let p = self.obj_local(s, proto);
        s.new_object_with(p)
    }

    /// An interface object: a function `name` with the prototype `proto`, linked both ways, and a property of `window`. Gives the function.
    pub(crate) fn interface_s<'s>(&self, s: &mut Sc<'s, E>, proto: E::Obj, name: &str, h: HS<E>, len: u32, kind: HostFn) -> Res<'s, E::Obj> {
        let f = self.add_fn_s(s, h, name, len, kind)?;
        self.link_s(s, f, proto, name)
    }

    /// `interface_s` for a function that is made already.
    pub(crate) fn link_s<'s>(&self, s: &mut Sc<'s, E>, f: Local<'s>, proto: E::Obj, name: &str) -> Res<'s, E::Obj> {
        let p = self.obj_local(s, proto);
        s.define_str(f, "prototype", p, Attrs { writable: false, enumerable: false, configurable: false })?;
        s.define_str(p, "constructor", f, Attrs::METHOD)?;
        let g = s.global();
        s.define_str(g, name, f, Attrs::METHOD)?;
        Ok(obj_of::<E>(s, f))
    }

    /// `proto[Symbol.toStringTag] = name`, so `Object.prototype.toString` says `[object name]`.
    pub(crate) fn tag_s<'s>(&self, s: &mut Sc<'s, E>, proto: E::Obj, name: &str) -> Res<'s, ()> {
        let p = self.obj_local(s, proto);
        let key = s.intrinsic(Intrinsic::SymbolToStringTag);
        let t = s.string(name)?;
        s.define(p, key, t, Attrs { writable: false, enumerable: false, configurable: true })
    }

    /// `def_method` on an object that a handler made: a function of the new form as a method of `target`. `enumerable` is false for an
    /// ordinary method and true for a WebIDL one.
    pub(crate) fn def_method_l<'s>(&self, s: &mut Sc<'s, E>, target: Local<'s>, name: &str, len: u32, h: HS<E>, enumerable: bool) -> Res<'s, Local<'s>> {
        let f = self.add_fn_s(s, h, name, len, HostFn::Method)?;
        s.define_str(target, name, f, Attrs { writable: true, enumerable, configurable: true })?;
        Ok(f)
    }

    /// `def_get` on an object that a handler made: an accessor of functions of the new form.
    pub(crate) fn def_get_l<'s>(&self, s: &mut Sc<'s, E>, target: Local<'s>, name: &str, get: HS<E>, set: Option<HS<E>>) -> Res<'s, ()> {
        let g = self.add_fn_s(s, get, &format!("get {name}"), 0, HostFn::Method)?;
        let st = match set {
            Some(h) => Some(self.add_fn_s(s, h, &format!("set {name}"), 1, HostFn::Method)?),
            None => None,
        };
        s.define_accessor_str(target, name, Some(g), st, Attrs { writable: false, enumerable: true, configurable: true })
    }

    /// `def_method_s` for a scope: a function of the new form as a method of the object `target`.
    pub(crate) fn def_method_at<'s>(&self, s: &mut Sc<'s, E>, target: E::Obj, name: &str, len: u32, h: HS<E>) -> Res<'s, Local<'s>> {
        let t = self.obj_local(s, target);
        self.def_method_l(s, t, name, len, h, false)
    }

    /// `def_get_s` for a scope: an accessor of functions of the new form on the object `target`.
    pub(crate) fn def_get_at<'s>(&self, s: &mut Sc<'s, E>, target: E::Obj, name: &str, get: HS<E>, set: Option<HS<E>>) -> Res<'s, ()> {
        let t = self.obj_local(s, target);
        self.def_get_l(s, t, name, get, set)
    }

    /// `Dom::new_source`.
    pub(crate) fn new_source_s<'s>(&self, s: &mut Sc<'s, E>, k: crate::browser_sources::Source, proto: Option<E::Obj>) -> Res<'s, E::Obj> {
        old::<E, _>(s, |it| self.new_source(it, k, proto))
    }

    /// Is `v` the object `o`?
    pub(crate) fn is_obj_s(&self, s: &mut Sc<'_, E>, v: Local<'_>, o: E::Obj) -> bool {
        matches!(E::kind(s.engine().raw(v)), Val::Object(x) if x == o)
    }

    /// The wrapper of `node` (`Dom::wrap`).
    #[inline]
    pub(crate) fn wrap_s<'s>(&self, s: &mut Sc<'s, E>, node: NodeId) -> Res<'s, Local<'s>> {
        old_v::<E>(s, |it| self.wrap(it, node))
    }

    /// `Dom::wrap_opt`.
    pub(crate) fn wrap_opt_s<'s>(&self, s: &mut Sc<'s, E>, n: Option<NodeId>) -> Res<'s, Local<'s>> {
        match n {
            Some(n) => self.wrap_s(s, n),
            None => Ok(s.null()),
        }
    }

    /// `Dom::dom_err` on a scope: the exception to throw.
    pub(crate) fn dom_err_s<'s>(&self, s: &mut Sc<'s, E>, name: &str, msg: &str) -> bare_js_api::Error<'s> {
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
    pub(crate) fn arg_node_s<'s>(&self, s: &mut Sc<'s, E>, a: Args<'s>, i: usize) -> Res<'s, NodeId> {
        let v = arg_s::<E>(s, a, i);
        match self.node_of_s(s, v) {
            Some(n) => Ok(n),
            None => Err(s.type_error(&format!("parameter {} is not of type 'Node'", i + 1))),
        }
    }

    /// `Dom::string_arg` of the argument `i`.
    pub(crate) fn arg_str_s<'s>(&self, s: &mut Sc<'s, E>, a: Args<'s>, i: usize) -> Res<'s, String> {
        let v = arg_s::<E>(s, a, i);
        self.string_arg_s(s, v)
    }

    /// `Dom::selectors_at`.
    pub(crate) fn selectors_at_s<'s>(&self, s: &mut Sc<'s, E>, text: &str, scope: NodeId) -> Res<'s, Rc<Vec<Selector>>> {
        old::<E, _>(s, |it| self.selectors_at(it, text, scope))
    }

    /// `Dom::room`, with the cap as an error of the interface.
    pub(crate) fn room_s<'s>(&self, s: &mut Sc<'s, E>, doc: &Document, nodes: usize, text: usize) -> Res<'s, ()> {
        match self.room(doc, nodes, text) {
            Ok(()) => Ok(()),
            Err(e) => Err(lift_err::<E>(s, e)),
        }
    }

    /// `Dom::attr_room`, with the cap as an error of the interface.
    pub(crate) fn attr_room_s<'s>(&self, s: &mut Sc<'s, E>, doc: &Document, el: NodeId, name: &str, value: &str) -> Res<'s, ()> {
        match self.attr_room(doc, el, name, value) {
            Ok(()) => Ok(()),
            Err(e) => Err(lift_err::<E>(s, e)),
        }
    }
}

// ------------------------------------------------------------------------------------------ bridges to code of the old form
// Each of these gives a function of a file that has not moved a form on `Scope`. The file that owns the function removes the bridge when it moves.

pub(crate) fn wrap_coll_s<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, ids: &[NodeId]) -> Res<'s, Local<'s>> {
    old_v::<E>(s, |it| crate::browser_gaps::wrap_coll(d, it, ids))
}

pub(crate) fn ns_put_s<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, n: NodeId, existing: Option<bare_html::dom::Atom>, qname: &str, value: &str) -> Res<'s, ()> {
    old::<E, _>(s, |it| crate::browser_namespace::put(d, it, n, existing, None, qname, value))
}

pub(crate) fn media_src_changed_s<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, n: NodeId) -> Res<'s, ()> {
    crate::browser_media::src_changed(d, s, n)
}

pub(crate) fn script_src_changed_s<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, n: NodeId) -> Res<'s, ()> {
    old::<E, _>(s, |it| d.script_src_changed(it, n))
}

pub(crate) fn script_inserted_s<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, n: NodeId) -> Res<'s, ()> {
    old::<E, _>(s, |it| d.script_inserted(it, n))
}

pub(crate) fn file_from_ref_s<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, f: &FileRef) -> Res<'s, Local<'s>> {
    old_v::<E>(s, |it| crate::browser_web::file_from_ref(d, it, f))
}

pub(crate) fn radio_group_s<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, n: NodeId) -> Res<'s, Vec<NodeId>> {
    old::<E, _>(s, |it| d.radio_group(it, n))
}

pub(crate) fn set_value_attr_s<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, n: NodeId, text: &str) -> Res<'s, ()> {
    old::<E, _>(s, |it| d.set_value_attr(it, n, text))
}

pub(crate) fn tag_fragments_s<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>) -> Res<'s, ()> {
    old::<E, _>(s, |it| crate::browser_elements::tag_fragments(d, it))
}

pub(crate) fn put_input_value_s<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, n: NodeId, v: String) -> Res<'s, ()> {
    old::<E, _>(s, |it| d.put_input_value(Some(it), n, v))
}

/// The window object of the frame of `n` (`browser_frames::window_of`), as a local.
pub(crate) fn window_of_s<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, n: NodeId) -> Option<Local<'s>> {
    let o = crate::browser_frames::window_of(d, n)?;
    Some(d.obj_local(s, o))
}

/// Is `v` an `Attr` object (a key of `Dom::attr`)?
pub(crate) fn is_attr_node_s<E: Eng>(d: &Dom<E>, s: &mut Sc<'_, E>, v: Local<'_>) -> bool {
    match E::kind(s.engine().raw(v)) {
        Val::Object(o) => d.attr.s.borrow().attrs.contains_key(&o),
        _ => false,
    }
}

/// The object of a local that holds one.
fn obj_of<E: Eng>(s: &mut Sc<'_, E>, l: Local<'_>) -> E::Obj {
    match E::kind(s.engine().raw(l)) {
        Val::Object(o) => o,
        _ => unreachable!("a function is an object"),
    }
}

/// The object of a local, or `None` for another value.
#[inline]
pub(crate) fn obj_id<E: Eng>(s: &mut Sc<'_, E>, v: Local<'_>) -> Option<E::Obj> {
    match E::kind(s.engine().raw(v)) {
        Val::Object(o) => Some(o),
        _ => None,
    }
}

/// The argument `i`, or `undefined`.
#[inline]
pub(crate) fn arg_s<'s, E: Eng>(s: &mut Sc<'s, E>, a: Args<'s>, i: usize) -> Local<'s> {
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
impl<E: Eng> Browser<E> {
    /// Run `f` in a handle scope of the engine.
    pub fn with_scope<R>(&mut self, f: impl for<'s> FnOnce(&mut Sc<'s, E>) -> R) -> R {
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
    fn keep(&mut self, r: R<E, E::Raw>) -> Result<Persistent, Failed> {
        match r {
            Ok(v) => Ok(self.with_scope(|s| {
                let l = s.engine().raw_local(&v);
                s.persist(l)
            })),
            Err(Fault::Throw(v)) if matches!(E::kind(v), Val::Object(_)) => {
                let p = self.with_scope(|s| {
                    let l = s.engine().raw_local(&v);
                    s.persist(l)
                });
                Err(Failed::Throw { name: self.get_str(&p, "name"), message: self.get_str(&p, "message") })
            }
            Err(Fault::Throw(v)) => Err(Failed::Other(format!("throw:{}", self.it.display_o(v)))),
            Err(e) => Err(Failed::Other(format!("{e:?}"))),
        }
    }
}
