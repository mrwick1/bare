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
//! A value that an object keeps through host state (a listener of a target, the target of an event) is not a root. The host keeps it
//! in a [`Traced`] and registers the object as an owner with a host key ([`Scope::set_host_owner`]). When the collector reaches the
//! owner, it calls [`Host::trace`] with the key, and the host visits the owner's references. After the collection, [`Host::prune`]
//! gets the keys of dead owners. So a detached cycle of node, wrapper, listener and closure is collected
//! (`docs/design-host-tracing.md`).
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
//! A traced value has one place, so it cannot be copied:
//!
//! ```compile_fail,E0599
//! fn dup<R>(t: &bare_js_api::Traced<R>) -> bare_js_api::Traced<R> {
//!     t.clone()
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

pub mod clone;
pub use clone::{CloneHost, CloneIn, CloneLimits, CloneOpts, Cloned, Moved, Refused};

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

/// A value that an owner keeps through host state. It is no root: the owner's [`Host::trace`] visits it. Not `Clone`: one place,
/// one owner. `R` is the engine's raw value ([`Engine::Raw`]).
#[derive(Debug)]
pub struct Traced<R> {
    raw: R,
}

impl<R> Traced<R> {
    /// For engines: a reference to `raw`.
    #[doc(hidden)]
    #[inline]
    pub fn from_raw(raw: R) -> Self {
        Traced { raw }
    }

    /// For engines: the raw value.
    #[doc(hidden)]
    #[inline]
    pub fn raw(&self) -> &R {
        &self.raw
    }

    /// For engines: replace the raw value.
    #[doc(hidden)]
    #[inline]
    pub fn set_raw(&mut self, raw: R) {
        self.raw = raw;
    }
}

/// The work list of the collector, given to [`Host::trace`].
pub struct Tracer<'a, R> {
    out: &'a mut Vec<R>,
}

impl<'a, R: Clone> Tracer<'a, R> {
    /// For engines: a tracer that adds to `out`.
    #[doc(hidden)]
    pub fn new(out: &'a mut Vec<R>) -> Self {
        Tracer { out }
    }

    /// Keep the value of `t` alive.
    #[inline]
    pub fn visit(&mut self, t: &Traced<R>) {
        self.out.push(t.raw.clone());
    }
}

/// Why a host cannot answer a read of a host-filled property now. The engine stops the read; it is never an empty answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pending {
    /// The host's state is in use (for example borrowed by the call that runs).
    Busy,
    /// The host must do work first: the engine calls the host's fill step with this index, then reads again. `u32::MAX` asks
    /// the host to charge a long scan.
    Fill(u32),
}

/// A property at an index of a host-indexed object.
#[derive(Clone, Debug)]
pub enum Item<R> {
    /// No property at this index.
    Absent,
    /// The property exists, but its object does not exist yet. The engine asks the host's fill step for it, then reads again.
    Missing,
    /// The value: an object or a string.
    Value(R),
}

/// The value of a named property of a host named object.
#[derive(Clone, Debug)]
pub enum Named<R> {
    /// A string, as UTF-16 code units.
    Text(Vec<u16>),
    Value(R),
}

/// What the engine asks of [`Host::lazy_member`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lazy<'a> {
    /// A lookup, a write or an own-property query of this string name missed on the object. Define it on the object (or on the
    /// objects that share it), if the host has it.
    Name(&'a str),
    /// A listing of the object's own names (`Object.keys` and the like): define every lazy member of the object.
    All,
    /// A lookup of a symbol key missed on the object: define the symbol members (the `Symbol.toStringTag` of the interface prototypes).
    Symbols,
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

/// An object or a value that every realm has, for [`Scope::intrinsic`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Intrinsic {
    ObjectPrototype,
    ArrayPrototype,
    /// `%IteratorPrototype%`: the prototype of a host iterator object.
    IteratorPrototype,
    /// `Error.prototype`.
    ErrorPrototype,
    /// The symbol `Symbol.iterator` of the realm. It does not depend on the global `Symbol`, which a script can replace.
    SymbolIterator,
    /// The symbol `Symbol.toStringTag` of the realm.
    SymbolToStringTag,
}

/// How a host function takes a call ([`Scope::host_function`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostFn {
    /// A plain function: `new` on it is a `TypeError`.
    Method,
    /// A constructor. A `new.target` that is not the function itself (`class extends`, `Reflect.construct`) is refused.
    Ctor,
    /// A constructor that reads `new.target` with [`Scope::proto_from_new_target`].
    CtorNewTarget,
}

/// The element type of a typed array ([`Scope::new_typed_array`], [`View`]). `Data` is a `DataView`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Elem {
    I8,
    U8,
    U8C,
    I16,
    U16,
    I32,
    U32,
    F32,
    F64,
    BI64,
    BU64,
    Data,
}

/// A typed array or `DataView` ([`Scope::view`]). `len` is in elements (bytes for a `DataView`); `offset` is in bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct View<'s> {
    pub buffer: Local<'s>,
    pub offset: usize,
    pub len: usize,
    pub elem: Elem,
}

/// An own property, as [`Scope::own_kind`] reads it. It runs no getter and no proxy trap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Own<'s> {
    Data(Local<'s>),
    Accessor { get: Option<Local<'s>>, set: Option<Local<'s>> },
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

/// Promises rejected with no handler that wait for the next checkpoint. Both engines keep at most this many and count the rest
/// (the `dropped` argument of [`Host::promise_rejections`]).
pub const MAX_PENDING_REJECTIONS: usize = 1024;

/// The state of a promise ([`Scope::promise_status`]). The value is a local of the scope.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromiseState<'s> {
    Pending,
    Fulfilled(Local<'s>),
    Rejected(Local<'s>),
}

/// An open iteration over a script iterable ([`Scope::iter_open`]). The engine keeps the iterator and its `next` method in two locals
/// of the scope, so they are roots. An engine that steps some iterables with no iterator object (an array by index) keeps the
/// position in `state`; the host does not read it. Not `Clone`: one iteration, one place. Use it in the scope that opened it.
#[derive(Debug)]
pub struct Iter<'s> {
    a: Local<'s>,
    b: Local<'s>,
    state: [u64; 3],
}

impl<'s> Iter<'s> {
    /// For engines: an iteration of the locals `a` and `b` and the position `state`.
    #[doc(hidden)]
    pub fn from_parts(a: Local<'s>, b: Local<'s>, state: [u64; 3]) -> Self {
        Iter { a, b, state }
    }

    /// For engines: the locals and the position.
    #[doc(hidden)]
    pub fn parts(&self) -> (Local<'s>, Local<'s>, [u64; 3]) {
        (self.a, self.b, self.state)
    }

    /// For engines: store the position.
    #[doc(hidden)]
    pub fn set_state(&mut self, state: [u64; 3]) {
        self.state = state;
    }
}

/// An `import()` call that waits for the host ([`Engine::take_dynamic_imports`]). The host holds `promise` until it settles it with
/// [`Engine::finish_dynamic_import`], [`Engine::reject_dynamic_import`] or [`Engine::fail_dynamic_import`]. `referrer` is the module
/// that asked (`None`: a classic script).
#[derive(Debug)]
pub struct ImportRequest {
    pub promise: Persistent,
    pub specifier: String,
    pub referrer: Option<u32>,
}

/// The host side of the browser: the engine calls it for host functions.
pub trait Host<E: Engine> {
    /// Run host function `id` (from [`Scope::host_function`]). `is_new` is true for `new f()`.
    fn call<'s>(&self, s: &mut Scope<'s, E>, id: u32, this: Local<'s>, args: Args<'s>, is_new: bool) -> Result<Local<'s>, Error<'s>>;
    /// The collector reached the owner with host key `key` ([`Scope::set_host_owner`]): visit the [`Traced`] values it keeps. The
    /// engine runs no script and allocates nothing here. The host must not hold a borrow that this method needs.
    fn trace(&self, _key: u32, _t: &mut Tracer<'_, E::Raw>) {}
    /// The owners with these keys died in the last collection. Their `Traced` values are dangling: drop them.
    fn prune(&self, _dead: &[u32]) {}

    // Microtasks. The engine will call these hooks with the handles of its own scope: it pushes the locals before the call and drops
    // them after it. (The old engine does not call them yet; it keeps its own hooks.)

    /// A `queueMicrotask` callback threw `thrown`. Record it without running script that can fail (read data properties only). The
    /// queue goes on.
    fn report_exception<'s>(&self, _s: &mut Scope<'s, E>, _thrown: Local<'s>) {}
    /// Before each microtask job and once when the queue is empty, with an empty script stack, so the host can run a script before
    /// the next job. An error ends the checkpoint like a failed job.
    fn between_jobs<'s>(&self, _s: &mut Scope<'s, E>) -> Result<(), Error<'s>> {
        Ok(())
    }
    /// The end of a microtask checkpoint. `unhandled`: promises rejected with no handler that still have none, oldest first.
    /// `handled`: promises given here before that got a handler since. `dropped`: rejections over [`MAX_PENDING_REJECTIONS`] since
    /// the last call. Script may run. An error ends the checkpoint.
    fn promise_rejections<'s>(&self, _s: &mut Scope<'s, E>, _unhandled: &[Local<'s>], _handled: &[Local<'s>], _dropped: u32) -> Result<(), Error<'s>> {
        Ok(())
    }

    // Host-filled properties. An object that the engine made with a host key (an indexed or a named object) has
    // properties that the host gives when the engine reads them, so they are always live. These methods get the object that was
    // asked (which can be a prototype of the object that the script touched) and its key, as a V8 interceptor gets its receiver.
    // A host can put what it needs for the dispatch in the key, for example the kind of the object. The methods must be cheap,
    // must not run script and must not allocate engine objects.

    /// How many indices are properties of the object `key` now.
    fn indexed_len(&self, _object: &E::Raw, _key: u32) -> Result<usize, Pending> {
        Ok(0)
    }
    /// The property at `index` of the object `key` now.
    fn indexed_item(&self, _object: &E::Raw, _key: u32, _index: usize) -> Result<Item<E::Raw>, Pending> {
        Ok(Item::Absent)
    }
    /// The index that the string `name` is a non-enumerable alias of, if any.
    fn indexed_name(&self, _object: &E::Raw, _key: u32, _name: &[u16]) -> Result<Option<u32>, Pending> {
        Ok(None)
    }
    /// The aliases of [`Host::indexed_name`], in the host's order.
    fn indexed_names(&self, _object: &E::Raw, _key: u32) -> Result<Vec<Vec<u16>>, Pending> {
        Ok(Vec::new())
    }
    /// Is `name` a named property of the object `key` now?
    fn named_has(&self, _object: &E::Raw, _key: u32, _name: &[u16]) -> Result<bool, Pending> {
        Ok(false)
    }
    /// The value of the named property `name`. The engine calls it only right after [`Host::named_has`] answered `true` for
    /// the same name.
    fn named_get(&self, _object: &E::Raw, _key: u32, _name: &[u16]) -> Result<Option<Named<E::Raw>>, Pending> {
        Ok(None)
    }
    /// How many names the object `key` has now. The engine calls it before [`Host::named_keys`].
    fn named_len(&self, _object: &E::Raw, _key: u32) -> Result<usize, Pending> {
        Ok(0)
    }
    /// The names of the object `key` now, in the host's order.
    fn named_keys(&self, _object: &E::Raw, _key: u32) -> Result<Vec<Vec<u16>>, Pending> {
        Ok(Vec::new())
    }

    /// A lazy member of `object`. The engine marks a prototype or a global object as lazy (`on*` accessors, interface tags) and asks
    /// here, once per name and once for all names, before it answers a miss. The host defines what it has (`Scope::define`), so
    /// the engine then finds it as a normal property. These are real properties made late, not host-filled ones: the host may run
    /// script and allocate here. Defining nothing is a valid answer.
    fn lazy_member<'s>(&self, _s: &mut Scope<'s, E>, _object: Local<'s>, _what: Lazy<'_>) -> Result<(), Error<'s>> {
        Ok(())
    }
}

/// A JavaScript realm. The host calls these methods only through [`Scope`]; each value that a method returns is pushed onto the
/// handle stack. Methods that take `&self` must not collect.
pub trait Engine: Sized {
    /// A value as the engine stores it, for [`Traced`].
    type Raw: Clone + 'static;

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
    /// A function whose calls go to [`Host::call`] with `id`. `id` is below 2^31.
    fn host_function<'s>(&mut self, name: &str, id: u32, length: u32, kind: HostFn) -> Result<Local<'s>, Error<'s>>;
    fn set_host(&mut self, host: Rc<dyn Host<Self>>);

    /// The raw value of a local, for a [`Traced`].
    fn raw(&self, v: Local<'_>) -> Self::Raw;
    /// A local of a raw value from a [`Traced`].
    fn raw_local<'s>(&mut self, r: &Self::Raw) -> Local<'s>;
    /// A store of `v` into a `Traced` of an owner: the place of a write barrier.
    fn store_raw(&mut self, t: &mut Traced<Self::Raw>, v: Local<'_>) {
        t.set_raw(self.raw(v));
    }
    /// Register `obj` as an owner with host key `key`. A key that the host gives again replaces the old one of this object.
    fn set_host_owner<'s>(&mut self, obj: Local<'_>, key: u32) -> Result<(), Error<'s>>;

    // Promises and microtasks. The engine owns the promise state and the job queue. The host holds a promise as a `Local`
    // (settled in the call that made it), a `Persistent` (settled later) or a `Traced` (an owner's pending promise).

    /// A new pending promise, with no resolving functions: the host settles it once with [`Engine::resolve_promise`] or
    /// [`Engine::reject_promise`].
    fn new_promise<'s>(&mut self) -> Result<Local<'s>, Error<'s>>;
    /// Resolve `p` with the promise resolve function rules (a thenable is assimilated in a job). Reactions run at the next
    /// checkpoint, never here. Not a promise: `Abort::Unsupported`.
    fn resolve_promise<'s>(&mut self, p: Local<'_>, v: Local<'_>) -> Result<(), Error<'s>>;
    /// Reject `p` with `reason`. See [`Engine::resolve_promise`].
    fn reject_promise<'s>(&mut self, p: Local<'_>, reason: Local<'_>) -> Result<(), Error<'s>>;
    /// Attach reactions to the native promise `p`. Reads no `then` or `constructor`. Not a promise: `Abort::Unsupported`.
    fn promise_then<'s>(&mut self, p: Local<'_>, ok: Local<'_>, err: Local<'_>) -> Result<(), Error<'s>>;
    /// `PromiseResolve(%Promise%, x)`, then [`Engine::promise_then`]. A thenable is assimilated in a job. Reading `x.constructor` may
    /// run script.
    fn promise_react<'s>(&mut self, x: Local<'_>, ok: Local<'_>, err: Local<'_>) -> Result<(), Error<'s>>;
    /// The state of `v`, or `None` when it is not a promise. The value of a settled promise is a new local.
    fn promise_status<'s>(&mut self, v: Local<'_>) -> Option<PromiseState<'s>>;
    /// Queue `cb` as a microtask. The engine keeps it alive until it ran. Not callable: a `TypeError`.
    fn queue_microtask<'s>(&mut self, cb: Local<'_>) -> Result<(), Error<'s>>;
    /// A microtask checkpoint from a host call with a script function on the stack below: it does nothing then (the HTML rule). Gives the
    /// number of jobs that ran. It may collect; the locals of the scope stay valid.
    fn checkpoint<'s>(&mut self) -> Result<usize, Error<'s>>;
    /// The microtask checkpoint, with an empty stack: run the jobs until the queue is empty. Gives the number of jobs that ran. A
    /// call with a script on the stack is `Abort::Unsupported`. It may collect; the locals of the scope stay valid.
    fn drain_microtasks<'s>(&mut self) -> Result<usize, Error<'s>>;
    /// The jobs in the queue.
    fn pending_jobs(&self) -> usize;
    /// Forget the error of a failed job, so that later drains work. The job that failed is lost.
    fn clear_failed_job(&mut self);

    /// `GetIterator(v)`. `None` when `v` is `undefined`, `null` or has no `Symbol.iterator`.
    fn iter_open<'s>(&mut self, v: Local<'_>) -> Result<Option<Iter<'s>>, Error<'s>>;
    /// The next value as a new local, or `None` at the end. After `None` or an error the iteration is done for good. Charges one step.
    fn iter_next<'s>(&mut self, it: &mut Iter<'s>) -> Result<Option<Local<'s>>, Error<'s>>;
    /// Stop because of `err` (`IteratorClose`): calls `return` and keeps `err`, unless `err` is an abort. Gives the error to pass on.
    fn iter_fail<'s>(&mut self, it: &mut Iter<'s>, err: Error<'s>) -> Error<'s>;

    // Modules. The host fetches, resolves and caches the files; the engine parses, links and evaluates. A module is a number (an index
    // of the engine's module table) and lives as long as the realm: the host never releases one, and holds no reference to it.

    /// Parse and keep a module. A syntax error is `Throw`. Refused inside a call (`Abort::Unsupported`). The module has no requested
    /// modules yet.
    fn module_parse<'s>(&mut self, source: &str, url: &str) -> Result<u32, Error<'s>>;
    /// The specifiers that module `m` asks for, in order. The host answers with [`Engine::module_set_requested`], in the same order.
    fn module_requests(&self, m: u32) -> Vec<String>;
    /// Set the module for each request of `m`.
    fn module_set_requested(&mut self, m: u32, deps: &[u32]);
    /// The URL that module `m` was parsed with. The borrow ends before any call that may collect.
    fn module_url(&self, m: u32) -> &str;
    /// Check the imports and make the environments of the graph of `root`. A bad import is `Throw`, and nothing changes then.
    fn module_link<'s>(&mut self, root: u32) -> Result<(), Error<'s>>;
    /// Evaluate a linked graph. Gives the promise of the evaluation; a second call gives the same promise. Not linked:
    /// `Abort::Unsupported`.
    fn module_evaluate<'s>(&mut self, m: u32) -> Result<Local<'s>, Error<'s>>;
    /// The `import()` calls since the last call, oldest first. The engine gives up its root: each promise is the host's
    /// [`Persistent`].
    fn take_dynamic_imports(&mut self) -> Vec<ImportRequest>;
    /// The host loaded the graph of `m` for the `import()` of `p`: link and evaluate it, and settle `p` with the namespace. A link or
    /// evaluation error rejects `p`.
    fn finish_dynamic_import<'s>(&mut self, p: &Persistent, m: u32) -> Result<(), Error<'s>>;
    /// Reject the `import()` of `p` with `reason`, the error of a parse.
    fn reject_dynamic_import<'s>(&mut self, p: &Persistent, reason: Local<'_>) -> Result<(), Error<'s>>;
    /// The host could not load the `import()` of `p`: reject it with a `TypeError` of `message`.
    fn fail_dynamic_import<'s>(&mut self, p: &Persistent, message: &str) -> Result<(), Error<'s>>;

    /// A point where the collector may run.
    fn safepoint(&mut self);

    // The base set of handler methods (plan step S0.4; `docs/design-host-forms.md`, `design-host-cssom.md`,
    // `design-host-navigation.md`). Each method that can run script or allocate takes `&mut self`.

    /// A `TypeError` object with this message. The caller returns it as `Err`.
    fn type_error<'s>(&mut self, msg: &str) -> Error<'s>;
    /// A `RangeError` object with this message.
    fn range_error<'s>(&mut self, msg: &str) -> Error<'s>;
    /// Add `n` to the step budget of the running script. `Abort::Limit` when the budget ends.
    fn charge<'s>(&mut self, n: u64) -> Result<(), Error<'s>>;
    /// `ToNumber(v)`. It can run script.
    fn to_number<'s>(&mut self, v: Local<'_>) -> Result<f64, Error<'s>>;
    /// `ToString(v)`. It can run script.
    fn to_string<'s>(&mut self, v: Local<'_>) -> Result<Local<'s>, Error<'s>>;
    /// `ToBoolean(v)`. It runs no script.
    fn to_boolean(&self, v: Local<'_>) -> bool;
    /// An array with these items.
    fn new_array<'s>(&mut self, items: &[Local<'_>]) -> Result<Local<'s>, Error<'s>>;
    /// `IsArray(v)`. A proxy answers for its target; a revoked proxy throws a `TypeError`.
    fn is_array<'s>(&mut self, v: Local<'_>) -> Result<bool, Error<'s>>;
    fn intrinsic<'s>(&mut self, which: Intrinsic) -> Local<'s>;
    /// `obj.[[GetPrototypeOf]]()`: `None` for `null`. A proxy runs its trap.
    fn get_prototype_of<'s>(&mut self, obj: Local<'_>) -> Result<Option<Local<'s>>, Error<'s>>;
    /// `obj.[[SetPrototypeOf]](proto)`, with `None` for `null`. `false` when the object is not extensible or the change makes a cycle.
    fn set_prototype_of<'s>(&mut self, obj: Local<'_>, proto: Option<Local<'_>>) -> Result<bool, Error<'s>>;
    /// Define an accessor property. `None` is `undefined`. `attrs.writable` has no meaning for an accessor and is ignored. A getter or
    /// setter that is not callable, or a property that cannot be defined, throws a `TypeError`.
    fn define_accessor<'s>(&mut self, obj: Local<'_>, key: Local<'_>, get: Option<Local<'_>>, set: Option<Local<'_>>, attrs: Attrs) -> Result<(), Error<'s>>;
    /// `GetPrototypeFromConstructor` of the host constructor that runs: `default` for a plain `new F`, else the `prototype` of the
    /// `new.target` (`Object.prototype` when that is not an object). Call it first, as Chrome does. It can run a getter.
    fn proto_from_new_target<'s>(&mut self, default: Local<'_>) -> Result<Local<'s>, Error<'s>>;
    /// A string of these UTF-16 code units. Lone surrogates stay.
    fn string_utf16<'s>(&mut self, units: &[u16]) -> Result<Local<'s>, Error<'s>>;
    /// The wall clock of `Date.now()` (virtual in a test), in ms since 1970.
    fn wall_now_ms(&self) -> f64;
    /// Does the object have an own property with this key? It runs no getter and no proxy trap: a proxy has none of its own.
    fn has_own<'s>(&mut self, obj: Local<'_>, key: Local<'_>) -> Result<bool, Error<'s>>;
    /// The own keys, as `Reflect.ownKeys`: array indices ascending (as strings), then strings, then symbols, each in creation order.
    fn own_keys<'s>(&mut self, obj: Local<'_>) -> Result<Vec<Local<'s>>, Error<'s>>;
    /// The value of the own enumerable property `key`, or `None` when there is none. It runs a getter, as `[[Get]]`.
    fn own_enumerable_data<'s>(&mut self, obj: Local<'_>, key: Local<'_>) -> Result<Option<Local<'s>>, Error<'s>>;
    /// Can `v` be called? A class constructor and a proxy of a function can.
    fn is_callable(&self, v: Local<'_>) -> bool;
    /// A function that calls `target` with `this` fixed, with the arguments of its own call. It reads no `Function.prototype.bind`.
    /// The engine traces both. `target` is a function, or a `TypeError`.
    fn bind<'s>(&mut self, target: Local<'_>, this: Local<'_>) -> Result<Local<'s>, Error<'s>>;

    // Handler bodies of the browser (`docs/design-host-handlers.md`, section 2). The default bodies say that the engine has no such
    // method yet; an engine that has it overrides them.

    /// The host key of a host-filled object (`new_host_indexed`, `new_host_named`). `None` for any other object, also for an object
    /// that has a key from [`Engine::set_host_owner`] (the old engine keeps no map from the object to that key).
    fn host_key(&self, _obj: Local<'_>) -> Option<u32> {
        None
    }
    /// Run `f` as one host operation: it starts the step budget when no call is active, counts as one call level, and refuses
    /// `collect`, `run_script` and `drain_microtasks` until it ends. `Err` when the call depth is full ([`task`] gives a scope).
    fn task<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> Result<R, Abort> {
        let _ = f;
        Err(Abort::Unsupported("task"))
    }
    /// `Construct(f, args, new_target)`; `None` is `f`. A value that is not a constructor is a `TypeError`.
    fn construct<'s>(&mut self, _f: Local<'_>, _args: &[Local<'_>], _new_target: Option<Local<'_>>) -> Result<Local<'s>, Error<'s>> {
        Err(Error::Abort(Abort::Unsupported("construct")))
    }
    /// Run script text as a script of the realm: its completion value.
    fn run_script<'s>(&mut self, _source: &str) -> Result<Local<'s>, Error<'s>> {
        Err(Error::Abort(Abort::Unsupported("run_script")))
    }
    /// An event handler: `function NAME(PARAMS) {\nBODY\n}` in the global environment. A body that does not parse alone is a `SyntaxError`.
    fn compile_function<'s>(&mut self, _name: &str, _params: &str, _body: &str) -> Result<Local<'s>, Error<'s>> {
        Err(Error::Abort(Abort::Unsupported("compile_function")))
    }
    /// `call(f, this, args)` while the names of the code of [`Engine::compile_function`] are looked up in `scope` first, innermost first.
    fn call_scoped<'s>(&mut self, _f: Local<'_>, _this: Local<'_>, _args: &[Local<'_>], _scope: &[Local<'_>]) -> Result<Local<'s>, Error<'s>> {
        Err(Error::Abort(Abort::Unsupported("call_scoped")))
    }
    /// Indirect eval of the string `src` in the global environment.
    fn eval_global<'s>(&mut self, _src: Local<'_>) -> Result<Local<'s>, Error<'s>> {
        Err(Error::Abort(Abort::Unsupported("eval_global")))
    }
    /// The `%eval%` function of the realm, for the global `eval`.
    fn eval_function<'s>(&mut self) -> Result<Local<'s>, Error<'s>> {
        Err(Error::Abort(Abort::Unsupported("eval_function")))
    }
    /// A new `ArrayBuffer` that owns `bytes`.
    fn new_array_buffer<'s>(&mut self, _bytes: Vec<u8>) -> Result<Local<'s>, Error<'s>> {
        Err(Error::Abort(Abort::Unsupported("new_array_buffer")))
    }
    /// A typed array of type `elem` over a new buffer that owns `bytes`. `Elem::Data` is not a typed array.
    fn new_typed_array<'s>(&mut self, _elem: Elem, _bytes: Vec<u8>) -> Result<Local<'s>, Error<'s>> {
        Err(Error::Abort(Abort::Unsupported("new_typed_array")))
    }
    /// The bytes of an `ArrayBuffer`, a typed array or a `DataView`, with no copy; empty when detached. The borrow ends before any
    /// call that may collect.
    fn bytes(&self, _v: Local<'_>) -> Option<&[u8]> {
        None
    }
    /// The bytes of a buffer or a view, to write. No script may run while the borrow is held.
    fn bytes_mut(&mut self, _v: Local<'_>) -> Option<&mut [u8]> {
        None
    }
    /// The parts of a typed array or `DataView`. The buffer is a new local.
    fn view<'s>(&mut self, _v: Local<'_>) -> Option<View<'s>> {
        None
    }
    /// Is the buffer detached (of a view: the buffer of the view)? `false` for another value.
    fn is_detached(&self, _v: Local<'_>) -> bool {
        false
    }
    /// The element `i` of a typed array as a number. `None` for a `BigInt` array, a `DataView`, a detached buffer or an index out of range.
    fn view_get(&self, _v: Local<'_>, _i: usize) -> Option<f64> {
        None
    }
    /// The number of 64-bit limbs of a `BigInt` value (0 for another value).
    fn bigint_limbs(&self, _v: Local<'_>) -> usize {
        0
    }
    /// An object whose integer-index properties come from [`Host::indexed_item`], with the host key `key`.
    fn new_host_indexed<'s>(&mut self, _proto: Option<Local<'_>>, _key: u32) -> Result<Local<'s>, Error<'s>> {
        Err(Error::Abort(Abort::Unsupported("new_host_indexed")))
    }
    /// An object whose string keys come from the `named_*` hooks, with the host key `key`. `open`: it accepts new names.
    fn new_host_named<'s>(&mut self, _proto: Option<Local<'_>>, _key: u32, _open: bool) -> Result<Local<'s>, Error<'s>> {
        Err(Error::Abort(Abort::Unsupported("new_host_named")))
    }
    /// Mark `obj` lazy: the engine calls [`Host::lazy_member`] before it answers a miss. `chain`: for the objects of its prototype chain too.
    fn set_lazy<'s>(&mut self, _obj: Local<'_>, _chain: bool) -> Result<(), Error<'s>> {
        Err(Error::Abort(Abort::Unsupported("set_lazy")))
    }
    /// Remove the lazy mark of `obj`.
    fn clear_lazy(&mut self, _obj: Local<'_>) {}
    /// The own property `key`, with no getter and no proxy trap. `None`: there is none (a proxy has none of its own).
    fn own_kind<'s>(&mut self, _obj: Local<'_>, _key: Local<'_>) -> Result<Option<Own<'s>>, Error<'s>> {
        Err(Error::Abort(Abort::Unsupported("own_kind")))
    }
    /// The description of a symbol, or `None` for `undefined`.
    fn symbol_description<'s>(&mut self, _v: Local<'_>) -> Result<Option<Local<'s>>, Error<'s>> {
        Err(Error::Abort(Abort::Unsupported("symbol_description")))
    }
    /// Text of a value for tools and tests: strings raw, numbers as JavaScript prints them, an object as `object` or `function`.
    fn display(&self, _v: Local<'_>) -> String {
        String::new()
    }
    /// Text of a value for tools: nested objects up to `cap` characters. `node` describes a host object, or gives `None`.
    fn inspect(&mut self, _v: Local<'_>, _cap: usize, _node: &dyn Fn(&Self::Raw) -> Option<String>) -> String {
        String::new()
    }
    /// `Object.freeze` (`frozen`) or `Object.seal`. `false` when it cannot (a host-filled object).
    fn set_integrity<'s>(&mut self, _obj: Local<'_>, _frozen: bool) -> Result<bool, Error<'s>> {
        Err(Error::Abort(Abort::Unsupported("set_integrity")))
    }
    /// The `length` of an array, or `None` for another value.
    fn array_len<'s>(&mut self, _v: Local<'_>) -> Result<Option<u32>, Error<'s>> {
        Err(Error::Abort(Abort::Unsupported("array_len")))
    }
    /// An error object whose prototype is `proto`, with own `message`, and the stack of the call.
    fn new_error<'s>(&mut self, _proto: Local<'_>, _msg: &str) -> Result<Local<'s>, Error<'s>> {
        Err(Error::Abort(Abort::Unsupported("new_error")))
    }

    // Structured clone (`docs/design-host-clone.md`). The engine walks plain data itself and writes a record (`clone`). It asks the
    // host only for host objects, the transfer list and the error. The default bodies say that the engine has no clone yet.

    /// Serialize `v` into a record, with the limits and the transfer list of `o`. The engine detaches the listed buffers and calls
    /// [`CloneHost::transfer_out`] for the other listed items only after the walk and every check passed.
    fn clone_out<'s>(&mut self, _v: Local<'_>, _o: &CloneOpts<'_, '_>, _h: &mut dyn CloneHost<Self>) -> Result<Cloned, Error<'s>> {
        Err(Error::Abort(Abort::Unsupported("clone_out")))
    }
    /// Build the object graph of a record, and the objects of its moved host items. The engine takes the moved items out of `c`
    /// ([`Cloned::take_moved`]), so a record with no moved item can be read again (a history entry), and one with moved items once.
    fn clone_in<'s>(&mut self, _c: &mut Cloned, _h: &mut dyn CloneHost<Self>) -> Result<CloneIn<'s>, Error<'s>> {
        Err(Error::Abort(Abort::Unsupported("clone_in")))
    }
    /// Serialize and build in one pass, with no record and no transfer list: the copy and its byte cost.
    fn clone_copy<'s>(&mut self, _v: Local<'_>, _l: &CloneLimits, _h: &mut dyn CloneHost<Self>) -> Result<(Local<'s>, usize), Error<'s>> {
        Err(Error::Abort(Abort::Unsupported("clone_copy")))
    }
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

    /// A reference that the owner's [`Host::trace`] keeps alive.
    pub fn trace_ref(&mut self, v: Local<'_>) -> Traced<E::Raw> {
        Traced::from_raw(self.e.raw(v))
    }

    pub fn traced(&mut self, t: &Traced<E::Raw>) -> Local<'s> {
        self.e.raw_local(t.raw())
    }

    pub fn store(&mut self, t: &mut Traced<E::Raw>, v: Local<'_>) {
        self.e.store_raw(t, v);
    }

    /// From now on the collector calls [`Host::trace`] with `key` when it reaches `obj`, and [`Host::prune`] when `obj` dies. Throws a
    /// `TypeError` when `obj` is not an object.
    pub fn set_host_owner(&mut self, obj: Local<'_>, key: u32) -> Result<(), Error<'s>> {
        self.e.set_host_owner(obj, key)
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

    pub fn host_function(&mut self, name: &str, id: u32, length: u32, kind: HostFn) -> Result<Local<'s>, Error<'s>> {
        self.e.host_function(name, id, length, kind)
    }

    pub fn set_host(&mut self, host: Rc<dyn Host<E>>) {
        self.e.set_host(host);
    }

    pub fn new_promise(&mut self) -> Result<Local<'s>, Error<'s>> {
        self.e.new_promise()
    }

    pub fn resolve_promise(&mut self, p: Local<'_>, v: Local<'_>) -> Result<(), Error<'s>> {
        self.e.resolve_promise(p, v)
    }

    pub fn reject_promise(&mut self, p: Local<'_>, reason: Local<'_>) -> Result<(), Error<'s>> {
        self.e.reject_promise(p, reason)
    }

    pub fn promise_then(&mut self, p: Local<'_>, ok: Local<'_>, err: Local<'_>) -> Result<(), Error<'s>> {
        self.e.promise_then(p, ok, err)
    }

    pub fn promise_react(&mut self, x: Local<'_>, ok: Local<'_>, err: Local<'_>) -> Result<(), Error<'s>> {
        self.e.promise_react(x, ok, err)
    }

    pub fn promise_status(&mut self, v: Local<'_>) -> Option<PromiseState<'s>> {
        self.e.promise_status(v)
    }

    pub fn queue_microtask(&mut self, cb: Local<'_>) -> Result<(), Error<'s>> {
        self.e.queue_microtask(cb)
    }

    pub fn checkpoint(&mut self) -> Result<usize, Error<'s>> {
        self.e.checkpoint()
    }

    pub fn drain_microtasks(&mut self) -> Result<usize, Error<'s>> {
        self.e.drain_microtasks()
    }

    pub fn pending_jobs(&self) -> usize {
        self.e.pending_jobs()
    }

    pub fn clear_failed_job(&mut self) {
        self.e.clear_failed_job();
    }

    pub fn iter_open(&mut self, v: Local<'_>) -> Result<Option<Iter<'s>>, Error<'s>> {
        self.e.iter_open(v)
    }

    /// The next item as a new local. A loop over a large iterable runs each step in an inner scope and moves the item out with
    /// [`Scope::escape_scope`], or keeps it in a [`Persistent`].
    pub fn iter_next(&mut self, it: &mut Iter<'s>) -> Result<Option<Local<'s>>, Error<'s>> {
        self.e.iter_next(it)
    }

    pub fn iter_fail(&mut self, it: &mut Iter<'s>, err: Error<'s>) -> Error<'s> {
        self.e.iter_fail(it, err)
    }

    pub fn module_parse(&mut self, source: &str, url: &str) -> Result<u32, Error<'s>> {
        self.e.module_parse(source, url)
    }

    pub fn module_requests(&self, m: u32) -> Vec<String> {
        self.e.module_requests(m)
    }

    pub fn module_set_requested(&mut self, m: u32, deps: &[u32]) {
        self.e.module_set_requested(m, deps);
    }

    pub fn module_url(&self, m: u32) -> &str {
        self.e.module_url(m)
    }

    pub fn module_link(&mut self, root: u32) -> Result<(), Error<'s>> {
        self.e.module_link(root)
    }

    pub fn module_evaluate(&mut self, m: u32) -> Result<Local<'s>, Error<'s>> {
        self.e.module_evaluate(m)
    }

    pub fn take_dynamic_imports(&mut self) -> Vec<ImportRequest> {
        self.e.take_dynamic_imports()
    }

    pub fn finish_dynamic_import(&mut self, p: &Persistent, m: u32) -> Result<(), Error<'s>> {
        self.e.finish_dynamic_import(p, m)
    }

    pub fn reject_dynamic_import(&mut self, p: &Persistent, reason: Local<'_>) -> Result<(), Error<'s>> {
        self.e.reject_dynamic_import(p, reason)
    }

    pub fn fail_dynamic_import(&mut self, p: &Persistent, message: &str) -> Result<(), Error<'s>> {
        self.e.fail_dynamic_import(p, message)
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

/// The base set of handler methods on a scope (plan step S0.4). See [`Engine`] for each one.
impl<'s, E: Engine> Scope<'s, E> {
    pub fn type_error(&mut self, msg: &str) -> Error<'s> {
        self.e.type_error(msg)
    }

    pub fn range_error(&mut self, msg: &str) -> Error<'s> {
        self.e.range_error(msg)
    }

    pub fn charge(&mut self, n: u64) -> Result<(), Error<'s>> {
        self.e.charge(n)
    }

    pub fn to_number(&mut self, v: Local<'_>) -> Result<f64, Error<'s>> {
        self.e.to_number(v)
    }

    pub fn to_string(&mut self, v: Local<'_>) -> Result<Local<'s>, Error<'s>> {
        self.e.to_string(v)
    }

    pub fn to_boolean(&self, v: Local<'_>) -> bool {
        self.e.to_boolean(v)
    }

    pub fn new_array(&mut self, items: &[Local<'_>]) -> Result<Local<'s>, Error<'s>> {
        self.e.new_array(items)
    }

    pub fn is_array(&mut self, v: Local<'_>) -> Result<bool, Error<'s>> {
        self.e.is_array(v)
    }

    pub fn intrinsic(&mut self, which: Intrinsic) -> Local<'s> {
        self.e.intrinsic(which)
    }

    pub fn get_prototype_of(&mut self, obj: Local<'_>) -> Result<Option<Local<'s>>, Error<'s>> {
        self.e.get_prototype_of(obj)
    }

    pub fn set_prototype_of(&mut self, obj: Local<'_>, proto: Option<Local<'_>>) -> Result<bool, Error<'s>> {
        self.e.set_prototype_of(obj, proto)
    }

    pub fn define_accessor(&mut self, obj: Local<'_>, key: Local<'_>, get: Option<Local<'_>>, set: Option<Local<'_>>, attrs: Attrs) -> Result<(), Error<'s>> {
        self.e.define_accessor(obj, key, get, set, attrs)
    }

    pub fn proto_from_new_target(&mut self, default: Local<'_>) -> Result<Local<'s>, Error<'s>> {
        self.e.proto_from_new_target(default)
    }

    pub fn string_utf16(&mut self, units: &[u16]) -> Result<Local<'s>, Error<'s>> {
        self.e.string_utf16(units)
    }

    pub fn wall_now_ms(&self) -> f64 {
        self.e.wall_now_ms()
    }

    pub fn has_own(&mut self, obj: Local<'_>, key: Local<'_>) -> Result<bool, Error<'s>> {
        self.e.has_own(obj, key)
    }

    pub fn own_keys(&mut self, obj: Local<'_>) -> Result<Vec<Local<'s>>, Error<'s>> {
        self.e.own_keys(obj)
    }

    pub fn own_enumerable_data(&mut self, obj: Local<'_>, key: Local<'_>) -> Result<Option<Local<'s>>, Error<'s>> {
        self.e.own_enumerable_data(obj, key)
    }

    pub fn is_callable(&self, v: Local<'_>) -> bool {
        self.e.is_callable(v)
    }

    pub fn bind(&mut self, target: Local<'_>, this: Local<'_>) -> Result<Local<'s>, Error<'s>> {
        self.e.bind(target, this)
    }
}

/// The methods for the handler bodies (`docs/design-host-handlers.md`). See [`Engine`] for each one.
impl<'s, E: Engine> Scope<'s, E> {
    pub fn host_key(&self, obj: Local<'_>) -> Option<u32> {
        self.e.host_key(obj)
    }

    pub fn construct(&mut self, f: Local<'_>, args: &[Local<'_>], new_target: Option<Local<'_>>) -> Result<Local<'s>, Error<'s>> {
        self.e.construct(f, args, new_target)
    }

    pub fn run_script(&mut self, source: &str) -> Result<Local<'s>, Error<'s>> {
        self.e.run_script(source)
    }

    pub fn compile_function(&mut self, name: &str, params: &str, body: &str) -> Result<Local<'s>, Error<'s>> {
        self.e.compile_function(name, params, body)
    }

    pub fn call_scoped(&mut self, f: Local<'_>, this: Local<'_>, args: &[Local<'_>], scope: &[Local<'_>]) -> Result<Local<'s>, Error<'s>> {
        self.e.call_scoped(f, this, args, scope)
    }

    pub fn eval_global(&mut self, src: Local<'_>) -> Result<Local<'s>, Error<'s>> {
        self.e.eval_global(src)
    }

    pub fn eval_function(&mut self) -> Result<Local<'s>, Error<'s>> {
        self.e.eval_function()
    }

    pub fn new_array_buffer(&mut self, bytes: Vec<u8>) -> Result<Local<'s>, Error<'s>> {
        self.e.new_array_buffer(bytes)
    }

    pub fn new_typed_array(&mut self, elem: Elem, bytes: Vec<u8>) -> Result<Local<'s>, Error<'s>> {
        self.e.new_typed_array(elem, bytes)
    }

    pub fn new_uint8_array(&mut self, bytes: Vec<u8>) -> Result<Local<'s>, Error<'s>> {
        self.e.new_typed_array(Elem::U8, bytes)
    }

    pub fn bytes(&self, v: Local<'_>) -> Option<&[u8]> {
        self.e.bytes(v)
    }

    pub fn bytes_mut(&mut self, v: Local<'_>) -> Option<&mut [u8]> {
        self.e.bytes_mut(v)
    }

    pub fn view(&mut self, v: Local<'_>) -> Option<View<'s>> {
        self.e.view(v)
    }

    pub fn is_detached(&self, v: Local<'_>) -> bool {
        self.e.is_detached(v)
    }

    pub fn view_get(&self, v: Local<'_>, i: usize) -> Option<f64> {
        self.e.view_get(v, i)
    }

    pub fn bigint_limbs(&self, v: Local<'_>) -> usize {
        self.e.bigint_limbs(v)
    }

    pub fn new_host_indexed(&mut self, proto: Option<Local<'_>>, key: u32) -> Result<Local<'s>, Error<'s>> {
        self.e.new_host_indexed(proto, key)
    }

    pub fn new_host_named(&mut self, proto: Option<Local<'_>>, key: u32, open: bool) -> Result<Local<'s>, Error<'s>> {
        self.e.new_host_named(proto, key, open)
    }

    pub fn set_lazy(&mut self, obj: Local<'_>, chain: bool) -> Result<(), Error<'s>> {
        self.e.set_lazy(obj, chain)
    }

    pub fn clear_lazy(&mut self, obj: Local<'_>) {
        self.e.clear_lazy(obj);
    }

    pub fn own_kind(&mut self, obj: Local<'_>, key: Local<'_>) -> Result<Option<Own<'s>>, Error<'s>> {
        self.e.own_kind(obj, key)
    }

    pub fn symbol_description(&mut self, v: Local<'_>) -> Result<Option<Local<'s>>, Error<'s>> {
        self.e.symbol_description(v)
    }

    pub fn display(&self, v: Local<'_>) -> String {
        self.e.display(v)
    }

    pub fn inspect(&mut self, v: Local<'_>, cap: usize, node: &dyn Fn(&E::Raw) -> Option<String>) -> String {
        self.e.inspect(v, cap, node)
    }

    pub fn set_integrity(&mut self, obj: Local<'_>, frozen: bool) -> Result<bool, Error<'s>> {
        self.e.set_integrity(obj, frozen)
    }

    pub fn array_len(&mut self, v: Local<'_>) -> Result<Option<u32>, Error<'s>> {
        self.e.array_len(v)
    }

    pub fn new_error(&mut self, proto: Local<'_>, msg: &str) -> Result<Local<'s>, Error<'s>> {
        self.e.new_error(proto, msg)
    }

    /// `define` with a key from Rust.
    pub fn define_str(&mut self, obj: Local<'_>, key: &str, value: Local<'_>, attrs: Attrs) -> Result<(), Error<'s>> {
        let k = self.string(key)?;
        self.define(obj, k, value, attrs)
    }

    /// `set` with a key from Rust.
    pub fn set_str(&mut self, obj: Local<'_>, key: &str, value: Local<'_>) -> Result<(), Error<'s>> {
        let k = self.string(key)?;
        self.set(obj, k, value)
    }

    /// `define_accessor` with a key from Rust.
    pub fn define_accessor_str(&mut self, obj: Local<'_>, key: &str, get: Option<Local<'_>>, set: Option<Local<'_>>, attrs: Attrs) -> Result<(), Error<'s>> {
        let k = self.string(key)?;
        self.define_accessor(obj, k, get, set, attrs)
    }
}

/// Run `f` as one host operation ([`Engine::task`]) in its own outermost scope.
pub fn task<E: Engine, R>(e: &mut E, f: impl for<'s> FnOnce(&mut Scope<'s, E>) -> R) -> Result<R, Abort> {
    e.task(|e| enter(e, f))
}

#[cfg(test)]
mod clone_tests;
#[cfg(test)]
mod tests;
