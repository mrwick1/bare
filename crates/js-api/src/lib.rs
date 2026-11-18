//! The interface between the browser and a JavaScript engine (plan step S0.2, design 6.8 of `docs/design-engine-rewrite.md`).
//!
//! The browser is generic over [`Engine`]. It is compiled once for each engine, and a session picks one when it starts
//! (`BARE_JS_EXEC`). So an engine call costs what a direct call costs.
//!
//! # Handles
//!
//! The host never holds a raw engine value. It holds handles, and the engine treats every handle as a root:
//!
//! - A [`Local`] is a slot in the handle stack of a [`Scope`]. It is `Copy` and costs 4 bytes. It lives until its scope ends, and the
//!   borrow checker stops it from leaving the scope.
//! - A [`Persistent`] lives in a table of the realm until it is dropped or the realm ends. Host structures that live longer than a
//!   call (listeners, timers, wrappers) keep these. A drop needs no engine: it clears the bit of its slot in [`Slots`], so the slot
//!   is free at once and is no root at the next collection.
//!
//! The collector may run in any method that takes `&mut Scope` (an allocation, a call into script, [`Scope::safepoint`]). A method
//! that takes `&Scope` never collects. A borrow of engine memory ([`Scope::as_str`]) is tied to `&Scope`, so it cannot be held across
//! a call that may collect.
//!
//! ```
//! use bare_js_api::{Engine, Error, Local, Persistent, Scope, enter};
//!
//! // Keep a callback after the call that gave it, and call it later.
//! fn keep<'s, E: Engine>(s: &mut Scope<'s, E>, f: Local<'_>) -> Persistent {
//!     s.persist(f)
//! }
//!
//! fn fire<E: Engine>(e: &mut E, p: &Persistent) -> Option<f64> {
//!     enter(e, |s| {
//!         let f = s.local(p);
//!         let this = s.undefined();
//!         let arg = s.number(1.0);
//!         let r = s.call(f, this, &[arg]).ok()?;
//!         s.as_number(r)
//!     })
//! }
//!
//! // Build a value in an inner scope and give only the result to the outer one.
//! fn label<'s, E: Engine>(s: &mut Scope<'s, E>, o: Local<'s>) -> Result<Local<'s>, Error<'s>> {
//!     s.escape_scope(|inner| {
//!         let name = inner.get_str(o, "name")?;
//!         let text = inner.as_str(name).map(|t| t.to_string_lossy()).unwrap_or_default();
//!         inner.string(&format!("<{text}>"))
//!     })
//! }
//! ```
//!
//! A local cannot leave its scope:
//!
//! ```compile_fail,E0521
//! use bare_js_api::{Engine, Local, enter};
//!
//! fn leak<E: Engine>(e: &mut E) {
//!     let mut kept = None;
//!     enter(e, |s| kept = Some(s.undefined()));
//! }
//! ```
//!
//! A local of an inner scope cannot go to the outer one, except through [`Scope::escape_scope`]:
//!
//! ```compile_fail,E0521
//! use bare_js_api::{Engine, Local, Scope};
//!
//! fn leak<'s, E: Engine>(s: &mut Scope<'s, E>) {
//!     let mut kept: Vec<Local<'s>> = Vec::new();
//!     s.scope(|inner| kept.push(inner.undefined()));
//! }
//! ```
//!
//! A borrow of engine memory ends before a call that may collect:
//!
//! ```compile_fail,E0502
//! use bare_js_api::{Engine, Local, Scope};
//!
//! fn stale<'s, E: Engine>(s: &mut Scope<'s, E>, text: Local<'s>, f: Local<'_>) {
//!     let units = s.as_str(text);
//!     let this = s.undefined();
//!     let _ = s.call(f, this, &[]);
//!     drop(units);
//! }
//! ```
//!
//! A released persistent handle cannot be used:
//!
//! ```compile_fail,E0382
//! use bare_js_api::{Engine, Persistent, Scope};
//!
//! fn twice<'s, E: Engine>(s: &mut Scope<'s, E>, p: Persistent) {
//!     s.release(p);
//!     let _ = s.local(&p);
//! }
//! ```
//!
//! # For engines
//!
//! An engine implements [`Engine`] and makes handles with [`Local::from_slot`] and [`Args::from_slots`]. A slot is an index into its
//! handle stack, and the engine checks the bounds (a host can forge a slot number, but not memory access). The values in the handle
//! stack and in the persistent table are roots. Host stores of values go through [`Engine::set`] and [`Engine::define`], so the
//! engine runs its write barrier there.
#![forbid(unsafe_code)]

use std::cell::{Cell, RefCell};
use std::marker::PhantomData;
use std::rc::Rc;

/// A value in the handle stack of the scope `'s`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Local<'s> {
    slot: u32,
    _scope: PhantomData<&'s ()>,
}

impl<'s> Local<'s> {
    /// For engines: the handle of stack slot `slot`.
    #[doc(hidden)]
    pub fn from_slot(slot: u32) -> Self {
        Local { slot, _scope: PhantomData }
    }

    /// For engines: the stack slot.
    #[doc(hidden)]
    pub fn slot(self) -> u32 {
        self.slot
    }
}

/// A value that the realm keeps until this handle is dropped or the realm ends. Not `Clone`: one handle, one slot.
#[must_use = "a persistent handle keeps its value alive until it is dropped"]
#[derive(Debug)]
pub struct Persistent {
    id: u32,
    slots: Rc<Slots>,
}

impl Persistent {
    /// For engines: a handle of slot `id`, which [`Slots::take`] gave and the engine filled.
    #[doc(hidden)]
    #[inline]
    pub fn from_id(id: u32, slots: Rc<Slots>) -> Self {
        Persistent { id, slots }
    }

    /// For engines: the slot in the persistent table.
    #[doc(hidden)]
    #[inline]
    pub fn id(&self) -> u32 {
        self.id
    }
}

impl Drop for Persistent {
    #[inline]
    fn drop(&mut self) {
        self.slots.free(self.id);
    }
}

/// The slots of the persistent table that hold live handles: one bit per slot, shared by the realm and its handles. The live slots
/// are the roots, so a table that once held many handles costs one word per 64 slots at a collection.
#[derive(Debug, Default)]
pub struct Slots {
    bits: RefCell<Vec<u64>>,
    /// The words before this one have no free bit.
    first: Cell<usize>,
}

impl Slots {
    /// For engines: mark the lowest free slot live and give it.
    #[doc(hidden)]
    #[inline]
    pub fn take(&self) -> u32 {
        let mut bits = self.bits.borrow_mut();
        let mut w = self.first.get();
        while w < bits.len() && bits[w] == !0 {
            w += 1;
        }
        if w == bits.len() {
            bits.push(0);
        }
        let b = (!bits[w]).trailing_zeros();
        bits[w] |= 1 << b;
        self.first.set(w);
        w as u32 * 64 + b
    }

    /// For engines: each live slot, in order.
    #[doc(hidden)]
    pub fn for_each(&self, mut f: impl FnMut(u32)) {
        for (w, &word) in self.bits.borrow().iter().enumerate() {
            let mut b = word;
            while b != 0 {
                f(w as u32 * 64 + b.trailing_zeros());
                b &= b - 1;
            }
        }
    }

    #[inline]
    fn free(&self, id: u32) {
        let w = id as usize / 64;
        self.bits.borrow_mut()[w] &= !(1 << (id % 64));
        if w < self.first.get() {
            self.first.set(w);
        }
    }
}

/// The arguments of a host call: consecutive slots of the handle stack.
#[derive(Clone, Copy, Debug)]
pub struct Args<'s> {
    base: u32,
    len: u32,
    _scope: PhantomData<&'s ()>,
}

impl<'s> Args<'s> {
    /// For engines: the `len` slots from `base`.
    #[doc(hidden)]
    pub fn from_slots(base: u32, len: u32) -> Self {
        Args { base, len, _scope: PhantomData }
    }

    pub fn len(&self) -> usize {
        self.len as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn get(&self, i: usize) -> Option<Local<'s>> {
        (i < self.len as usize).then(|| Local::from_slot(self.base + i as u32))
    }

    pub fn iter(&self) -> impl Iterator<Item = Local<'s>> {
        (self.base..self.base + self.len).map(Local::from_slot)
    }
}

/// The result of `typeof`, with `null` apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Type {
    Undefined,
    Null,
    Boolean,
    Number,
    String,
    Symbol,
    BigInt,
    Object,
    Function,
}

/// The characters of a string, in the form that the engine stores.
#[derive(Clone, Copy, Debug)]
pub enum Str<'a> {
    Latin1(&'a [u8]),
    Utf16(&'a [u16]),
}

impl Str<'_> {
    /// The length in UTF-16 code units.
    pub fn len(&self) -> usize {
        match self {
            Str::Latin1(b) => b.len(),
            Str::Utf16(u) => u.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The text, with U+FFFD for each lone surrogate.
    pub fn to_string_lossy(&self) -> String {
        match self {
            Str::Latin1(b) => b.iter().map(|&c| c as char).collect(),
            Str::Utf16(u) => String::from_utf16_lossy(u),
        }
    }
}

/// The attributes of a data property.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Attrs {
    pub writable: bool,
    pub enumerable: bool,
    pub configurable: bool,
}

impl Attrs {
    /// A property that a script assignment makes.
    pub const ALL: Attrs = Attrs { writable: true, enumerable: true, configurable: true };
    /// A method of a built-in prototype.
    pub const METHOD: Attrs = Attrs { writable: true, enumerable: false, configurable: true };
}

/// Why a call failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error<'s> {
    /// A JavaScript exception, rooted in the scope.
    Throw(Local<'s>),
    /// The engine stopped the script; a script cannot catch this.
    Abort(Abort),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Abort {
    /// A time, memory or depth limit.
    Limit(&'static str),
    /// A feature that the engine does not have.
    Unsupported(&'static str),
}

/// The host side of the browser: the engine calls it for host functions.
pub trait Host<E: Engine> {
    /// Run host function `id` (from [`Scope::host_function`]). `is_new` is true for `new f()`.
    fn call<'s>(&self, s: &mut Scope<'s, E>, id: u32, this: Local<'s>, args: Args<'s>, is_new: bool) -> Result<Local<'s>, Error<'s>>;
}

/// A JavaScript realm. The host calls these methods only through [`Scope`]; each value that a method returns is pushed onto the
/// handle stack. Methods that take `&self` must not collect.
pub trait Engine: Sized {
    /// The number of slots in the handle stack.
    fn stack_len(&self) -> u32;
    /// Drop the slots from `len` up.
    fn stack_truncate(&mut self, len: u32);
    /// Copy slot `from` into slot `to`.
    fn stack_set(&mut self, to: u32, from: u32);

    /// Put the value in the persistent table; give its id.
    fn persist(&mut self, v: Local<'_>) -> u32;
    fn persistent<'s>(&mut self, id: u32) -> Local<'s>;
    /// The live slots of the persistent table.
    fn slots(&self) -> &Rc<Slots>;

    fn undefined<'s>(&mut self) -> Local<'s>;
    fn null<'s>(&mut self) -> Local<'s>;
    fn boolean<'s>(&mut self, b: bool) -> Local<'s>;
    fn number<'s>(&mut self, n: f64) -> Local<'s>;
    fn string<'s>(&mut self, text: &str) -> Result<Local<'s>, Error<'s>>;

    fn type_of(&self, v: Local<'_>) -> Type;
    fn as_bool(&self, v: Local<'_>) -> Option<bool>;
    fn as_number(&self, v: Local<'_>) -> Option<f64>;
    fn as_str(&self, v: Local<'_>) -> Option<Str<'_>>;

    fn global<'s>(&mut self) -> Local<'s>;
    fn new_object<'s>(&mut self) -> Result<Local<'s>, Error<'s>>;
    /// `obj[key]`, with getters and proxies.
    fn get<'s>(&mut self, obj: Local<'_>, key: Local<'_>) -> Result<Local<'s>, Error<'s>>;
    /// `obj[key] = value`, with setters, in strict mode.
    fn set<'s>(&mut self, obj: Local<'_>, key: Local<'_>, value: Local<'_>) -> Result<(), Error<'s>>;
    /// `Object.defineProperty` of a data property.
    fn define<'s>(&mut self, obj: Local<'_>, key: Local<'_>, value: Local<'_>, attrs: Attrs) -> Result<(), Error<'s>>;
    fn call<'s>(&mut self, f: Local<'_>, this: Local<'_>, args: &[Local<'_>]) -> Result<Local<'s>, Error<'s>>;
    /// A function whose calls go to [`Host::call`] with `id`.
    fn host_function<'s>(&mut self, name: &str, id: u32, length: u32) -> Result<Local<'s>, Error<'s>>;
    fn set_host(&mut self, host: Rc<dyn Host<Self>>);

    /// A point where the collector may run.
    fn safepoint(&mut self);
}

/// Open the outermost scope of `e`.
pub fn enter<E: Engine, R>(e: &mut E, f: impl for<'s> FnOnce(&mut Scope<'s, E>) -> R) -> R {
    let base = e.stack_len();
    f(&mut Scope { e, base })
}

/// A handle scope: the slots that it pushes are dropped when it ends.
pub struct Scope<'s, E: Engine> {
    e: &'s mut E,
    base: u32,
}

impl<E: Engine> Drop for Scope<'_, E> {
    fn drop(&mut self) {
        self.e.stack_truncate(self.base);
    }
}

impl<'s, E: Engine> Scope<'s, E> {
    /// A nested scope. Its locals end with it, so `R` cannot hold them.
    pub fn scope<R>(&mut self, f: impl for<'i> FnOnce(&mut Scope<'i, E>) -> R) -> R {
        let base = self.e.stack_len();
        f(&mut Scope { e: &mut *self.e, base })
    }

    /// A nested scope that gives one value (or the exception) to this one.
    pub fn escape_scope(&mut self, f: impl for<'i> FnOnce(&mut Scope<'i, E>) -> Result<Local<'i>, Error<'i>>) -> Result<Local<'s>, Error<'s>> {
        let keep = self.e.undefined().slot;
        let mut inner = Scope { e: &mut *self.e, base: keep + 1 };
        // Ok(true): a value; Ok(false): an exception. Either one is copied to `keep` before the inner slots are dropped.
        let r = match f(&mut inner) {
            Ok(v) => Ok((v.slot, true)),
            Err(Error::Throw(v)) => Ok((v.slot, false)),
            Err(Error::Abort(a)) => Err(a),
        }
        .map(|(from, ok)| {
            inner.e.stack_set(keep, from);
            ok
        });
        drop(inner);
        let v = Local::from_slot(keep);
        match r {
            Ok(true) => Ok(v),
            Ok(false) => Err(Error::Throw(v)),
            Err(a) => Err(Error::Abort(a)),
        }
    }

    pub fn persist(&mut self, v: Local<'_>) -> Persistent {
        Persistent { id: self.e.persist(v), slots: self.e.slots().clone() }
    }

    pub fn local(&mut self, p: &Persistent) -> Local<'s> {
        self.e.persistent(p.id)
    }

    /// Drop `p`. The same as `drop(p)`.
    pub fn release(&mut self, p: Persistent) {
        drop(p);
    }

    pub fn undefined(&mut self) -> Local<'s> {
        self.e.undefined()
    }

    pub fn null(&mut self) -> Local<'s> {
        self.e.null()
    }

    pub fn boolean(&mut self, b: bool) -> Local<'s> {
        self.e.boolean(b)
    }

    pub fn number(&mut self, n: f64) -> Local<'s> {
        self.e.number(n)
    }

    pub fn string(&mut self, text: &str) -> Result<Local<'s>, Error<'s>> {
        self.e.string(text)
    }

    pub fn type_of(&self, v: Local<'_>) -> Type {
        self.e.type_of(v)
    }

    pub fn as_bool(&self, v: Local<'_>) -> Option<bool> {
        self.e.as_bool(v)
    }

    pub fn as_number(&self, v: Local<'_>) -> Option<f64> {
        self.e.as_number(v)
    }

    /// The characters of a string value. The borrow ends before the next call that may collect.
    pub fn as_str(&self, v: Local<'_>) -> Option<Str<'_>> {
        self.e.as_str(v)
    }

    pub fn global(&mut self) -> Local<'s> {
        self.e.global()
    }

    pub fn new_object(&mut self) -> Result<Local<'s>, Error<'s>> {
        self.e.new_object()
    }

    pub fn get(&mut self, obj: Local<'_>, key: Local<'_>) -> Result<Local<'s>, Error<'s>> {
        self.e.get(obj, key)
    }

    /// `obj[key]` with a key from Rust.
    pub fn get_str(&mut self, obj: Local<'_>, key: &str) -> Result<Local<'s>, Error<'s>> {
        let k = self.string(key)?;
        self.get(obj, k)
    }

    pub fn set(&mut self, obj: Local<'_>, key: Local<'_>, value: Local<'_>) -> Result<(), Error<'s>> {
        self.e.set(obj, key, value)
    }

    pub fn define(&mut self, obj: Local<'_>, key: Local<'_>, value: Local<'_>, attrs: Attrs) -> Result<(), Error<'s>> {
        self.e.define(obj, key, value, attrs)
    }

    pub fn call(&mut self, f: Local<'_>, this: Local<'_>, args: &[Local<'_>]) -> Result<Local<'s>, Error<'s>> {
        self.e.call(f, this, args)
    }

    pub fn host_function(&mut self, name: &str, id: u32, length: u32) -> Result<Local<'s>, Error<'s>> {
        self.e.host_function(name, id, length)
    }

    pub fn set_host(&mut self, host: Rc<dyn Host<E>>) {
        self.e.set_host(host);
    }

    pub fn safepoint(&mut self) {
        self.e.safepoint();
    }

    /// For engines: the engine of this scope, to read the value of a slot before the scope ends.
    #[doc(hidden)]
    pub fn engine(&mut self) -> &mut E {
        self.e
    }
}

#[cfg(test)]
mod tests;
