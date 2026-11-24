//! The engine behind the DOM host (`docs/internal/design-js2-host-api.md`, S0.4 PR 6c). `Dom<E>` and every type that holds an object or a
//! value of the engine are generic over a marker `E: Eng`. The marker is a plain zero-sized type, so the derives of these types
//! (`Default`, `Clone`, `Copy`) need nothing of the engine itself. `E::Vm` is the engine, `E::Obj` an object id that is `Copy`, `E::Raw` a
//! value that is `Copy`. Both stay valid while the object or the value is a root or is traced by an owner.

use bare_js_api::{Engine, Persistent, Traced};
use std::fmt::Debug;
use std::hash::Hash;

/// The kinds of value that the host looks at. `Other` is a symbol, a big integer or anything else that the host never takes apart.
pub enum Val<E: Eng> {
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    String(E::Str),
    Symbol(E::Sym),
    Object(E::Obj),
    BigInt,
}

impl<E: Eng> Clone for Val<E> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<E: Eng> Copy for Val<E> {}

/// An engine for the host: the marker.
pub trait Eng: Copy + Default + Debug + PartialEq + Eq + Hash + 'static {
    /// The engine. Its `Raw` is `Self::Raw`.
    type Vm: Engine<Raw = Self::Raw> + Ops<Self>;
    /// An open iteration of a script value (`Ops::host_iter_open`).
    type Iter;
    type Obj: crate::browser::SlotKey + Hash + Debug + 'static;
    type Raw: Copy + Debug + 'static;
    /// An interned string (a property name, a string value).
    type Str: Copy + Debug + 'static;
    type Sym: Copy + Debug + Eq + Hash + 'static;
    /// A property key.
    type Key: Copy + Debug + Eq + Hash + 'static;
    /// A new engine with its realm, for a page or a child frame.
    fn realm(eval: crate::js_limits::EvalLimits, heap: crate::js_limits::HeapLimits) -> Result<Self::Vm, Fault<Self>>;
    /// Attach the DOM and install its interfaces in the realm.
    fn install_dom(vm: &mut Self::Vm, dom: &std::rc::Rc<crate::browser::Dom<Self>>) -> Result<(), Fault<Self>> {
        dom.install_on(vm)
    }
    fn kind(v: Self::Raw) -> Val<Self>;
    fn undef() -> Self::Raw;
    fn null() -> Self::Raw;
    fn bool_raw(b: bool) -> Self::Raw;
    fn num(n: f64) -> Self::Raw;
    fn obj_raw(o: Self::Obj) -> Self::Raw;
    fn str_raw(s: Self::Str) -> Self::Raw;
    fn sym_key(s: Self::Sym) -> Self::Key;
    fn is_bigint(v: Self::Raw) -> bool;
    fn key_kind(k: Self::Key) -> KeyKind<Self>;
}

/// What a property key is.
pub enum KeyKind<E: Eng> {
    Symbol(E::Sym),
    Index(u32),
    Str(E::Str),
}

/// Why a call of the old form of the interface failed. `Throw` is a script exception, the other kinds end the script.
pub enum Fault<E: Eng> {
    Throw(E::Raw),
    Unsupported(&'static str),
    Limit(crate::js_limits::LimitKind),
    /// A heap error that no script can see: the name of the error.
    Heap(&'static str),
}

impl<E: Eng> Clone for Fault<E> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<E: Eng> Copy for Fault<E> {}
impl<E: Eng> Debug for Fault<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Fault::Throw(v) => write!(f, "Throw({v:?})"),
            Fault::Unsupported(w) => write!(f, "Unsupported({w:?})"),
            Fault::Limit(l) => write!(f, "Limit({l:?})"),
            Fault::Heap(h) => write!(f, "Heap({h})"),
        }
    }
}

/// An own property, as the host reads it.
pub enum Prop<E: Eng> {
    Data { value: E::Raw, writable: bool, enumerable: bool, configurable: bool },
    Accessor { get: Option<E::Obj>, set: Option<E::Obj>, enumerable: bool, configurable: bool },
}

impl<E: Eng> Prop<E> {
    pub fn enumerable(&self) -> bool {
        match *self {
            Prop::Data { enumerable, .. } | Prop::Accessor { enumerable, .. } => enumerable,
        }
    }
}

/// What a property descriptor says: a field that is `None` is absent.
pub struct Desc<E: Eng> {
    pub value: Option<E::Raw>,
    pub writable: Option<bool>,
    pub get: Option<Option<E::Obj>>,
    pub set: Option<Option<E::Obj>>,
    pub enumerable: Option<bool>,
    pub configurable: Option<bool>,
}

impl<E: Eng> Default for Desc<E> {
    fn default() -> Self {
        Desc { value: None, writable: None, get: None, set: None, enumerable: None, configurable: None }
    }
}

/// An element that an indexed source answers for.
pub enum Slot<E: Eng> {
    Absent,
    Missing,
    Ready(E::Obj),
    Str(E::Str),
}

/// The state of a promise.
pub enum PState<E: Eng> {
    Pending,
    Fulfilled(E::Raw),
    Rejected(E::Raw),
}

pub use crate::js_limits::WallClock;
pub use crate::js_limits::Collected;

/// A write to a named property of a host object.
pub enum NamedOp<E: Eng> {
    Set(E::Raw),
    Delete,
    Define(Desc<E>),
}

/// The result of a read of a property along the chain.
pub enum Got<E: Eng> {
    Value(E::Raw),
    Accessor { getter: Option<E::Obj> },
}

/// What the load tool reads from the heap.
#[derive(Clone, Copy, Debug, Default)]
pub struct HeapStats {
    pub bytes: usize,
    pub live_objects: usize,
    pub collections: u64,
    pub slab_capacity_bytes: usize,
}

/// What the host does with the engine, in the old form (`E::Obj`, `E::Raw`, `Fault<E>`).
pub trait Ops<E: Eng>: Sized {
// <<ops
    fn define_data(&mut self, o: E::Obj, key: E::Key, v: E::Raw, writable: bool, enumerable: bool, configurable: bool) -> Result<(), Fault<E>>;
    fn get_value(&mut self, base: E::Raw, key: E::Key) -> Result<E::Raw, Fault<E>>;
    fn put_value(&mut self, base: E::Raw, key: E::Key, v: E::Raw) -> Result<(), Fault<E>>;
    fn charge_o(&mut self, n: u64) -> Result<(), Fault<E>>;
    fn type_error_o(&mut self, msg: &str) -> Fault<E>;
    fn range_error_o(&mut self, msg: &str) -> Fault<E>;
    fn call_o(&mut self, f: E::Raw, this: E::Raw, args: &[E::Raw]) -> Result<E::Raw, Fault<E>>;
    fn display_o(& self, v: E::Raw) -> String;
    fn rejection_text_o(&self, v: E::Raw) -> String { self.display_o(v) }
    fn new_error_o(&mut self, proto: E::Obj, msg: &str) -> Result<E::Obj, Fault<E>>;
    fn drain_microtasks_o(&mut self) -> Result<usize, Fault<E>>;
    fn compile_function_o(&mut self, name: &str, params: &str, body: &str) -> Result<E::Obj, Fault<E>>;
    fn eval_function_o(&mut self) -> Result<E::Obj, Fault<E>>;
    fn eval_global_o(&mut self, v: E::Raw) -> Result<E::Raw, Fault<E>>;
    fn run_script_o(&mut self, source: &str) -> Result<E::Raw, Fault<E>>;
    fn call_scoped_o(&mut self, f: E::Raw, this: E::Raw, args: &[E::Raw], scope: &[E::Obj]) -> Result<E::Raw, Fault<E>>;
    fn queue_microtask_o(&mut self, cb: E::Raw) -> Result<(), Fault<E>>;
    fn host_scope_o<T>(&mut self, f: impl FnOnce(&mut Self) -> Result<T, Fault<E>>) -> Result<T, Fault<E>>;
    fn host_checkpoint(&mut self) -> Result<usize, Fault<E>>;
    fn object_prototype(& self) -> E::Obj;
    fn array_prototype(& self) -> E::Obj;
    fn iterator_prototype(& self) -> E::Obj;
    fn error_prototype(& self) -> E::Obj;
    fn global_object(& self) -> E::Obj;
    fn symbol_iterator(& self) -> E::Key;
    fn symbol_to_string_tag(& self) -> E::Key;
    fn persist_value(&mut self, v: E::Raw) -> Persistent;
    fn persistent_value(& self, p: &Persistent) -> E::Raw;
    fn trace_value(& self, v: E::Raw) -> Traced<E::Raw>;
    fn traced_value(& self, t: &Traced<E::Raw>) -> E::Raw;
    fn store_value(&mut self, t: &mut Traced<E::Raw>, v: E::Raw) -> ();
    fn set_host_owner_obj(&mut self, o: E::Obj, key: u32) -> ();
    fn array_from(&mut self, items: &[E::Raw]) -> Result<E::Raw, Fault<E>>;
    fn array_from_proto(&mut self, items: &[E::Raw], proto: E::Obj) -> Result<E::Raw, Fault<E>>;
    fn is_callable_value(& self, v: E::Raw) -> bool;
    fn to_js_string(&mut self, v: E::Raw) -> Result<E::Str, Fault<E>>;
    fn to_js_number(&mut self, v: E::Raw) -> Result<f64, Fault<E>>;
    fn to_js_bool(& self, v: E::Raw) -> Result<bool, Fault<E>>;
    fn host_new_promise(&mut self) -> Result<E::Raw, Fault<E>>;
    fn host_resolve_promise(&mut self, p: E::Raw, v: E::Raw) -> Result<(), Fault<E>>;
    fn host_reject_promise(&mut self, p: E::Raw, v: E::Raw) -> Result<(), Fault<E>>;
    fn host_promise_then(&mut self, p: E::Raw, ok: E::Raw, err: E::Raw) -> Result<(), Fault<E>>;
    fn host_iter_result(&mut self, value: E::Raw, done: bool) -> Result<E::Raw, Fault<E>>;
    fn host_construct(&mut self, f: E::Raw, args: &[E::Raw], nt: Option<E::Obj>) -> Result<E::Raw, Fault<E>>;
    fn host_proto_from_new_target(&mut self, default: E::Obj) -> Result<E::Obj, Fault<E>>;
    fn is_scoped_function(& self, f: E::Obj) -> bool;
    fn external_live(& self) -> usize;
    fn set_external_live(&mut self, n: usize) -> ();
    fn add_external(&mut self, n: usize) -> ();
    fn gc_due(& self) -> bool;
    fn api_err<'s>(&mut self, e: Fault<E>) -> bare_js_api::Error<'s>;
    fn api_unerr(&mut self, e: bare_js_api::Error<'_>) -> Fault<E>;
    fn h_set_proto(&mut self, o: E::Obj, proto: Option<E::Obj>) -> Result<bool, Fault<E>>;
    fn h_get_proto(&mut self, o: E::Obj) -> Result<Option<E::Obj>, Fault<E>>;
    fn h_peek_own(&mut self, o: E::Obj, key: E::Key) -> Result<Option<Prop<E>>, Fault<E>>;
    fn h_has_own(&mut self, o: E::Obj, key: E::Key) -> Result<bool, Fault<E>>;
    fn h_intern(&mut self, s: &str) -> Result<E::Str, Fault<E>>;
    fn h_key(&mut self, s: &str) -> Result<E::Key, Fault<E>>;
    fn h_str_units(& self, s: E::Str) -> Result<std::borrow::Cow<'_, [u16]>, Fault<E>>;
    fn h_str_len(& self, s: E::Str) -> Result<usize, Fault<E>>;
    fn h_to_string_lossy(& self, s: E::Str) -> Result<String, Fault<E>>;
    fn h_symbol_description(& self, s: E::Sym) -> Result<Option<E::Str>, Fault<E>>;
    fn h_new_object(&mut self, proto: Option<E::Obj>) -> Result<E::Obj, Fault<E>>;
    fn h_get(&mut self, o: E::Obj, key: E::Key) -> Result<Got<E>, Fault<E>>;
    fn h_bytes(& self, o: E::Obj) -> Option<&[u8]>;
    fn h_view_parts(& self, o: E::Obj) -> Option<(E::Obj, usize, usize)>;
    fn h_own_keys(&mut self, o: E::Obj) -> Result<Vec<E::Key>, Fault<E>>;
    fn h_key_to_value(&mut self, k: E::Key) -> Result<E::Raw, Fault<E>>;
    fn h_get_own(&mut self, o: E::Obj, key: E::Key) -> Result<Option<Prop<E>>, Fault<E>>;
    fn h_define_own(&mut self, o: E::Obj, key: E::Key, d: &Desc<E>) -> Result<bool, Fault<E>>;
    fn h_set_lazy_on(&mut self, o: E::Obj) -> Result<(), Fault<E>>;
    fn h_clear_lazy_on(&mut self, o: E::Obj) -> ();
    fn h_new_named(&mut self, proto: Option<E::Obj>, key: u32) -> Result<E::Obj, Fault<E>>;
    fn h_new_named_open(&mut self, proto: Option<E::Obj>, key: u32) -> Result<E::Obj, Fault<E>>;
    fn h_new_indexed(&mut self, proto: Option<E::Obj>, key: u32) -> Result<E::Obj, Fault<E>>;
    fn h_host_key(& self, o: E::Obj) -> Option<u32>;
    fn h_shared_text(&mut self, s: &str) -> Result<E::Str, Fault<E>>;
    fn h_stats(& self) -> HeapStats;
    fn host_iter_open(&mut self, v: E::Raw) -> Result<Option<E::Iter>, Fault<E>>;
    fn host_iter_next(&mut self, it: &mut E::Iter) -> Result<Option<E::Raw>, Fault<E>>;
    fn host_iter_fail(&mut self, it: &mut E::Iter, err: Fault<E>) -> Fault<E>;
    fn bytes_of(& self, v: E::Raw) -> Option<&[u8]>;
    fn set_wall_clock(&mut self, c: WallClock) -> Result<(), Fault<E>>;
    fn wall_clock(& self) -> WallClock;
    fn wall_now_ms(& self) -> f64;
    fn promise_status(& self, v: E::Raw) -> Option<PState<E>>;
    fn clear_failed_job(&mut self) -> ();
    fn api_args_slice(& self, a: bare_js_api::Args<'_>, from: usize, to: usize) -> Option<&[E::Raw]>;
    fn api_args(& self, a: bare_js_api::Args<'_>, from: usize, to: usize) -> Vec<E::Raw>;
    fn host_new_target(& self) -> Option<E::Obj>;
    fn collect(&mut self, extra: &[E::Raw]) -> Result<Collected, Fault<E>>;
    fn set_sources(&mut self, dom: std::rc::Rc<crate::browser::Dom<E>>) -> ();
    fn module_parse_o(&mut self, source: &str, url: &str) -> Result<u32, Fault<E>>;
    fn module_requests_o(& self, m: u32) -> Vec<String>;
    fn module_set_requested_o(&mut self, m: u32, deps: &[u32]) -> ();
    fn module_url_o(& self, m: u32) -> &str;
    fn module_link_o(&mut self, root: u32) -> Result<(), Fault<E>>;
    fn module_evaluate_o(&mut self, m: u32) -> Result<E::Obj, Fault<E>>;
    fn finish_dynamic_import_o(&mut self, promise: E::Obj, m: u32) -> Result<(), Fault<E>>;
    fn reject_dynamic_import_o(&mut self, promise: E::Obj, e: Fault<E>) -> Result<(), Fault<E>>;
    fn fail_dynamic_import_o(&mut self, promise: E::Obj, message: &str) -> Result<(), Fault<E>>;
    fn host_bind(&mut self, target: E::Obj, this: E::Raw) -> Result<E::Obj, Fault<E>>;
    fn is_constructor_value(&mut self, v: E::Raw) -> Result<bool, Fault<E>>;
    fn inspect_o(&mut self, v: E::Raw, cap: usize, node: &dyn Fn(E::Obj) -> Option<String>) -> String;
// ops>>
}
