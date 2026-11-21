//! The old engine (`bare-js`) as an engine for the host.

use crate::eng::{Collected, Desc, Eng, Fault, Got, HeapStats, KeyKind, Ops, PState, Prop, Val, WallClock};
use bare_js::heap::{Get, Property, PropertyDescriptor};
use bare_js_api::{Persistent, Traced};
use bare_js::eval::{Error, HostIter, Interp, PromiseStatus};
use bare_js::value::{ObjId, PropertyKey, StrId, SymId, Value};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Old;

/// The neutral error of the old engine, for the tests that name it.
pub type OldFault = Fault<Old>;

impl Eng for Old {
    type Vm = Interp<'static>;
    type Iter = HostIter;
    type Obj = ObjId;
    type Raw = Value;
    type Str = StrId;
    type Sym = SymId;
    type Key = PropertyKey;

    #[inline(always)]
    fn realm(eval: bare_js::eval::EvalLimits, heap: bare_js::heap::HeapLimits) -> Result<Interp<'static>, Fault<Old>> {
        let heap = bare_js::heap::Heap::new(heap).map_err(|e| Fault::from(Error::Heap(e)))?;
        Ok(Interp::realm(heap, eval)?)
    }

    // Keep the host and source vtables in bare when a caller builds a child realm.
    #[inline(never)]
    fn install_dom(vm: &mut Interp<'static>, dom: &std::rc::Rc<crate::browser::Dom<Self>>) -> Result<(), Fault<Self>> {
        dom.install_on(vm)
    }

    fn kind(v: Value) -> Val<Old> {
        match v {
            Value::Undefined => Val::Undefined,
            Value::Null => Val::Null,
            Value::Bool(b) => Val::Bool(b),
            Value::Number(n) => Val::Number(n),
            Value::String(s) => Val::String(s),
            Value::Symbol(s) => Val::Symbol(s),
            Value::Object(o) => Val::Object(o),
            Value::BigInt(_) | Value::BigHeap(_) => Val::BigInt,
        }
    }
    #[inline(always)]
    fn undef() -> Value {
        Value::Undefined
    }
    #[inline(always)]
    fn null() -> Value {
        Value::Null
    }
    #[inline(always)]
    fn bool_raw(b: bool) -> Value {
        Value::Bool(b)
    }
    #[inline(always)]
    fn num(n: f64) -> Value {
        Value::Number(n)
    }
    #[inline(always)]
    fn obj_raw(o: ObjId) -> Value {
        Value::Object(o)
    }
    #[inline(always)]
    fn str_raw(s: StrId) -> Value {
        Value::String(s)
    }
    #[inline(always)]
    fn is_bigint(v: Value) -> bool {
        v.is_bigint()
    }
    #[inline(always)]
    fn sym_key(s: SymId) -> PropertyKey {
        PropertyKey::Symbol(s)
    }
    #[inline(always)]
    fn key_kind(k: PropertyKey) -> KeyKind<Old> {
        match k {
            PropertyKey::Symbol(s) => KeyKind::Symbol(s),
            PropertyKey::Index(i) => KeyKind::Index(i),
            PropertyKey::String(s) => KeyKind::Str(s),
        }
    }
}

impl From<Error> for Fault<Old> {
    #[inline(always)]
    fn from(e: Error) -> Self {
        match e {
            Error::Throw(v) => Fault::Throw(v),
            Error::Unsupported(u) => Fault::Unsupported(u.what),
            Error::Limit(l) => Fault::Limit(l),
            Error::Heap(e) => {
                use bare_js::heap::HeapError::*;
                Fault::Heap(match e {
                    Stale => "Stale",
                    Limit(_) => "Limit",
                    InvalidKey => "InvalidKey",
                    InvalidDescriptor => "InvalidDescriptor",
                    InvalidArrayLength => "InvalidArrayLength",
                    SymbolConversion => "SymbolConversion",
                    SymbolToNumber => "SymbolToNumber",
                    BigIntToNumber => "BigIntToNumber",
                    NeedsExecution(_) => "NeedsExecution",
                    NeedsBigInt => "NeedsBigInt",
                    BigUnsigned(_) => "BigUnsigned",
                    BigElement => "BigElement",
                    NeedsHost { .. } => "NeedsHost",
                    HostUnavailable => "HostUnavailable",
                    NeedsNamed => "NeedsNamed",
                    Proxy(_) => "Proxy",
                })
            }
        }
    }
}

impl From<Fault<Old>> for Error {
    #[inline(always)]
    fn from(f: Fault<Old>) -> Self {
        match f {
            Fault::Throw(v) => Error::Throw(v),
            Fault::Unsupported(what) => Error::Unsupported(bare_js::eval::Unsupported { what, span: Default::default() }),
            Fault::Limit(l) => Error::Limit(l),
            Fault::Heap(_) => Error::Unsupported(bare_js::eval::Unsupported { what: "heap error", span: Default::default() }),
        }
    }
}

impl crate::browser::SlotKey for ObjId {
    #[inline(always)]
    fn slot(self) -> usize {
        ObjId::slot(self)
    }
}

impl From<Property> for Prop<Old> {
    #[inline(always)]
    fn from(p: Property) -> Self {
        match p {
            Property::Data { value, writable, enumerable, configurable } => Prop::Data { value, writable, enumerable, configurable },
            Property::Accessor { get, set, enumerable, configurable } => Prop::Accessor { get, set, enumerable, configurable },
        }
    }
}

impl From<PromiseStatus> for PState<Old> {
    #[inline(always)]
    fn from(p: PromiseStatus) -> Self {
        match p {
            PromiseStatus::Pending => PState::Pending,
            PromiseStatus::Fulfilled(v) => PState::Fulfilled(v),
            PromiseStatus::Rejected(v) => PState::Rejected(v),
        }
    }
}

impl From<Get> for Got<Old> {
    #[inline(always)]
    fn from(g: Get) -> Self {
        match g {
            Get::Value(value) => Got::Value(value),
            Get::Accessor { getter } => Got::Accessor { getter },
        }
    }
}

impl Desc<Old> {
    #[inline(always)]
    fn to_old(&self) -> PropertyDescriptor {
        PropertyDescriptor { value: self.value, writable: self.writable, get: self.get, set: self.set, enumerable: self.enumerable, configurable: self.configurable }
    }
}

impl From<bare_js::heap::Stats> for HeapStats {
    fn from(s: bare_js::heap::Stats) -> Self {
        HeapStats { bytes: s.bytes, live_objects: s.live_objects, collections: s.collections, slab_capacity_bytes: s.slab_capacity_bytes }
    }
}

impl Ops<Old> for Interp<'static> {
// <<ops
    #[inline(always)]
    fn define_data(&mut self, o: ObjId, key: PropertyKey, v: Value, writable: bool, enumerable: bool, configurable: bool) -> Result<(), Fault<Old>> {
        (Interp::define_data(self, o, key, v, writable, enumerable, configurable)).map_err(Fault::from)
    }
    #[inline(always)]
    fn get_value(&mut self, base: Value, key: PropertyKey) -> Result<Value, Fault<Old>> {
        (Interp::get_value(self, base, key)).map_err(Fault::from)
    }
    #[inline(always)]
    fn put_value(&mut self, base: Value, key: PropertyKey, v: Value) -> Result<(), Fault<Old>> {
        (Interp::put_value(self, base, key, v)).map_err(Fault::from)
    }
    #[inline(always)]
    fn charge_o(&mut self, n: u64) -> Result<(), Fault<Old>> {
        (Interp::charge(self, n)).map_err(Fault::from)
    }
    #[inline(always)]
    fn type_error_o(&mut self, msg: &str) -> Fault<Old> {
        Fault::from(Interp::type_error(self, msg))
    }
    #[inline(always)]
    fn range_error_o(&mut self, msg: &str) -> Fault<Old> {
        Fault::from(Interp::range_error(self, msg))
    }
    #[inline(always)]
    fn call_o(&mut self, f: Value, this: Value, args: &[Value]) -> Result<Value, Fault<Old>> {
        (Interp::call(self, f, this, args)).map_err(Fault::from)
    }
    #[inline(always)]
    fn display_o(& self, v: Value) -> String {
        Interp::display(self, v)
    }
    #[inline(always)]
    fn new_error_o(&mut self, proto: ObjId, msg: &str) -> Result<ObjId, Fault<Old>> {
        (Interp::new_error(self, proto, msg)).map_err(Fault::from)
    }
    #[inline(always)]
    fn drain_microtasks_o(&mut self) -> Result<usize, Fault<Old>> {
        (Interp::drain_microtasks(self)).map_err(Fault::from)
    }
    #[inline(always)]
    fn compile_function_o(&mut self, name: &str, params: &str, body: &str) -> Result<ObjId, Fault<Old>> {
        (Interp::compile_function(self, name, params, body)).map_err(Fault::from)
    }
    #[inline(always)]
    fn eval_function_o(&mut self) -> Result<ObjId, Fault<Old>> {
        (Interp::eval_function(self)).map_err(Fault::from)
    }
    #[inline(always)]
    fn eval_global_o(&mut self, v: Value) -> Result<Value, Fault<Old>> {
        (Interp::eval_global(self, v)).map_err(Fault::from)
    }
    #[inline(always)]
    fn run_script_o(&mut self, source: &str) -> Result<Value, Fault<Old>> {
        (Interp::run_script(self, source)).map_err(Fault::from)
    }
    #[inline(always)]
    fn call_scoped_o(&mut self, f: Value, this: Value, args: &[Value], scope: &[ObjId]) -> Result<Value, Fault<Old>> {
        (Interp::call_scoped(self, f, this, args, scope)).map_err(Fault::from)
    }
    #[inline(always)]
    fn queue_microtask_o(&mut self, cb: Value) -> Result<(), Fault<Old>> {
        (Interp::queue_microtask(self, cb)).map_err(Fault::from)
    }
    #[inline(always)]
    fn host_scope_o<T>(&mut self, f: impl FnOnce(&mut Self) -> Result<T, Fault<Old>>) -> Result<T, Fault<Old>> {
        Interp::host_scope(self, |i| f(i).map_err(Error::from)).map_err(Fault::from)
    }
    #[inline(always)]
    fn host_checkpoint(&mut self) -> Result<usize, Fault<Old>> {
        (Interp::host_checkpoint(self)).map_err(Fault::from)
    }
    #[inline(always)]
    fn object_prototype(& self) -> ObjId {
        Interp::object_prototype(self)
    }
    #[inline(always)]
    fn array_prototype(& self) -> ObjId {
        Interp::array_prototype(self)
    }
    #[inline(always)]
    fn iterator_prototype(& self) -> ObjId {
        Interp::iterator_prototype(self)
    }
    #[inline(always)]
    fn error_prototype(& self) -> ObjId {
        Interp::error_prototype(self)
    }
    #[inline(always)]
    fn global_object(& self) -> ObjId {
        Interp::global_object(self)
    }
    #[inline(always)]
    fn symbol_iterator(& self) -> PropertyKey {
        Interp::symbol_iterator(self)
    }
    #[inline(always)]
    fn symbol_to_string_tag(& self) -> PropertyKey {
        Interp::symbol_to_string_tag(self)
    }
    #[inline(always)]
    fn persist_value(&mut self, v: Value) -> Persistent {
        Interp::persist_value(self, v)
    }
    #[inline(always)]
    fn persistent_value(& self, p: &Persistent) -> Value {
        Interp::persistent_value(self, p)
    }
    #[inline(always)]
    fn trace_value(& self, v: Value) -> Traced<Value> {
        Interp::trace_value(self, v)
    }
    #[inline(always)]
    fn traced_value(& self, t: &Traced<Value>) -> Value {
        Interp::traced_value(self, t)
    }
    #[inline(always)]
    fn store_value(&mut self, t: &mut Traced<Value>, v: Value) -> () {
        Interp::store_value(self, t, v)
    }
    #[inline(always)]
    fn set_host_owner_obj(&mut self, o: ObjId, key: u32) -> () {
        Interp::set_host_owner_obj(self, o, key)
    }
    #[inline(always)]
    fn array_from(&mut self, items: &[Value]) -> Result<Value, Fault<Old>> {
        (Interp::array_from(self, items)).map_err(Fault::from)
    }
    #[inline(always)]
    fn array_from_proto(&mut self, items: &[Value], proto: ObjId) -> Result<Value, Fault<Old>> {
        (Interp::array_from_proto(self, items, proto)).map_err(Fault::from)
    }
    #[inline(always)]
    fn is_callable_value(& self, v: Value) -> bool {
        Interp::is_callable_value(self, v)
    }
    #[inline(always)]
    fn to_js_string(&mut self, v: Value) -> Result<StrId, Fault<Old>> {
        (Interp::to_js_string(self, v)).map_err(Fault::from)
    }
    #[inline(always)]
    fn to_js_number(&mut self, v: Value) -> Result<f64, Fault<Old>> {
        (Interp::to_js_number(self, v)).map_err(Fault::from)
    }
    #[inline(always)]
    fn to_js_bool(& self, v: Value) -> Result<bool, Fault<Old>> {
        (Interp::to_js_bool(self, v)).map_err(Fault::from)
    }
    #[inline(always)]
    fn host_new_promise(&mut self) -> Result<Value, Fault<Old>> {
        (Interp::host_new_promise(self)).map_err(Fault::from)
    }
    #[inline(always)]
    fn host_resolve_promise(&mut self, p: Value, v: Value) -> Result<(), Fault<Old>> {
        (Interp::host_resolve_promise(self, p, v)).map_err(Fault::from)
    }
    #[inline(always)]
    fn host_reject_promise(&mut self, p: Value, v: Value) -> Result<(), Fault<Old>> {
        (Interp::host_reject_promise(self, p, v)).map_err(Fault::from)
    }
    #[inline(always)]
    fn host_promise_then(&mut self, p: Value, ok: Value, err: Value) -> Result<(), Fault<Old>> {
        (Interp::host_promise_then(self, p, ok, err)).map_err(Fault::from)
    }
    #[inline(always)]
    fn host_iter_result(&mut self, value: Value, done: bool) -> Result<Value, Fault<Old>> {
        (Interp::host_iter_result(self, value, done)).map_err(Fault::from)
    }
    #[inline(always)]
    fn host_construct(&mut self, f: Value, args: &[Value], nt: Option<ObjId>) -> Result<Value, Fault<Old>> {
        (Interp::host_construct(self, f, args, nt)).map_err(Fault::from)
    }
    #[inline(always)]
    fn host_proto_from_new_target(&mut self, default: ObjId) -> Result<ObjId, Fault<Old>> {
        (Interp::host_proto_from_new_target(self, default)).map_err(Fault::from)
    }
    #[inline(always)]
    fn is_scoped_function(& self, f: ObjId) -> bool {
        Interp::is_scoped_function(self, f)
    }
    #[inline(always)]
    fn external_live(& self) -> usize {
        Interp::external_live(self)
    }
    #[inline(always)]
    fn set_external_live(&mut self, n: usize) -> () {
        Interp::set_external_live(self, n)
    }
    #[inline(always)]
    fn add_external(&mut self, n: usize) -> () {
        Interp::add_external(self, n)
    }
    #[inline(always)]
    fn gc_due(& self) -> bool {
        Interp::gc_due(self)
    }
    #[inline(always)]
    fn api_err<'s>(&mut self, e: Fault<Old>) -> bare_js_api::Error<'s> {
        Interp::api_err(self, Error::from(e))
    }
    #[inline(always)]
    fn api_unerr(&mut self, e: bare_js_api::Error<'_>) -> Fault<Old> {
        Fault::from(Interp::api_unerr(self, e))
    }
    #[inline(always)]
    fn h_set_proto(&mut self, o: ObjId, proto: Option<ObjId>) -> Result<bool, Fault<Old>> {
        (self.heap.set_prototype_of(o, proto).map_err(|e| Interp::heap_error(self, e))).map_err(Fault::from)
    }
    #[inline(always)]
    fn h_get_proto(&mut self, o: ObjId) -> Result<Option<ObjId>, Fault<Old>> {
        (self.heap.get_prototype_of(o).map_err(|e| Interp::heap_error(self, e))).map_err(Fault::from)
    }
    #[inline(always)]
    fn h_peek_own(&mut self, o: ObjId, key: PropertyKey) -> Result<Option<Prop<Old>>, Fault<Old>> {
        (self.heap.peek_own_property(o, key).map(|p| p.map(Prop::from)).map_err(|e| Interp::heap_error(self, e))).map_err(Fault::from)
    }
    #[inline(always)]
    fn h_has_own(&mut self, o: ObjId, key: PropertyKey) -> Result<bool, Fault<Old>> {
        (self.heap.has_own_property(o, key).map_err(|e| Interp::heap_error(self, e))).map_err(Fault::from)
    }
    #[inline(always)]
    fn h_intern(&mut self, s: &str) -> Result<StrId, Fault<Old>> {
        (self.heap.intern_str(s).map_err(|e| Interp::heap_error(self, e))).map_err(Fault::from)
    }
    #[inline(always)]
    fn h_key(&mut self, s: &str) -> Result<PropertyKey, Fault<Old>> {
        (self.heap.key_from_str(s).map_err(|e| Interp::heap_error(self, e))).map_err(Fault::from)
    }
    #[inline(always)]
    fn h_str_units(& self, s: StrId) -> Result<std::borrow::Cow<'_, [u16]>, Fault<Old>> {
        (self.heap.str_units(s).map(std::borrow::Cow::Borrowed).map_err(Error::Heap)).map_err(Fault::from)
    }
    #[inline(always)]
    fn h_str_len(& self, s: StrId) -> Result<usize, Fault<Old>> {
        (self.heap.str_len(s).map_err(Error::Heap)).map_err(Fault::from)
    }
    #[inline(always)]
    fn h_to_string_lossy(& self, s: StrId) -> Result<String, Fault<Old>> {
        (self.heap.to_string_lossy(s).map_err(Error::Heap)).map_err(Fault::from)
    }
    #[inline(always)]
    fn h_symbol_description(& self, s: SymId) -> Result<Option<StrId>, Fault<Old>> {
        (self.heap.symbol_description(s).map_err(Error::Heap)).map_err(Fault::from)
    }
    #[inline(always)]
    fn h_new_object(&mut self, proto: Option<ObjId>) -> Result<ObjId, Fault<Old>> {
        (self.heap.new_object(proto).map_err(|e| Interp::heap_error(self, e))).map_err(Fault::from)
    }
    #[inline(always)]
    fn h_get(&mut self, o: ObjId, key: PropertyKey) -> Result<Got<Old>, Fault<Old>> {
        (self.heap.get(o, key).map(Got::from).map_err(|e| Interp::heap_error(self, e))).map_err(Fault::from)
    }
    #[inline(always)]
    fn h_bytes(& self, o: ObjId) -> Option<&[u8]> {
        self.heap.bytes(o)
    }
    #[inline(always)]
    fn h_view_parts(& self, o: ObjId) -> Option<(ObjId, usize, usize)> {
        self.heap.view_parts(o)
    }
    #[inline(always)]
    fn h_own_keys(&mut self, o: ObjId) -> Result<Vec<PropertyKey>, Fault<Old>> {
        (self.heap.own_keys(o).map_err(|e| Interp::heap_error(self, e))).map_err(Fault::from)
    }
    #[inline(always)]
    fn h_key_to_value(&mut self, k: PropertyKey) -> Result<Value, Fault<Old>> {
        (self.heap.key_to_value(k).map_err(|e| Interp::heap_error(self, e))).map_err(Fault::from)
    }
    #[inline(always)]
    fn h_get_own(&mut self, o: ObjId, key: PropertyKey) -> Result<Option<Prop<Old>>, Fault<Old>> {
        (self.heap.get_own_property(o, key).map(|p| p.map(Prop::from)).map_err(|e| Interp::heap_error(self, e))).map_err(Fault::from)
    }
    #[inline(always)]
    fn h_define_own(&mut self, o: ObjId, key: PropertyKey, d: &Desc<Old>) -> Result<bool, Fault<Old>> {
        (self.heap.define_own_property(o, key, &d.to_old()).map_err(|e| Interp::heap_error(self, e))).map_err(Fault::from)
    }
    #[inline(always)]
    fn h_set_lazy_on(&mut self, o: ObjId) -> Result<(), Fault<Old>> {
        (self.heap.set_lazy_on(o).map_err(|e| Interp::heap_error(self, e))).map_err(Fault::from)
    }
    #[inline(always)]
    fn h_clear_lazy_on(&mut self, o: ObjId) -> () {
        self.heap.clear_lazy_on(o)
    }
    #[inline(always)]
    fn h_new_named(&mut self, proto: Option<ObjId>, key: u32) -> Result<ObjId, Fault<Old>> {
        (self.heap.new_named(proto, key).map_err(|e| Interp::heap_error(self, e))).map_err(Fault::from)
    }
    #[inline(always)]
    fn h_new_named_open(&mut self, proto: Option<ObjId>, key: u32) -> Result<ObjId, Fault<Old>> {
        (self.heap.new_named_open(proto, key).map_err(|e| Interp::heap_error(self, e))).map_err(Fault::from)
    }
    #[inline(always)]
    fn h_new_indexed(&mut self, proto: Option<ObjId>, key: u32) -> Result<ObjId, Fault<Old>> {
        (self.heap.new_indexed(proto, key).map_err(|e| Interp::heap_error(self, e))).map_err(Fault::from)
    }
    #[inline(always)]
    fn h_host_key(& self, o: ObjId) -> Option<u32> {
        self.heap.host_key(o)
    }
    #[inline(always)]
    fn h_shared_text(&mut self, s: &str) -> Result<StrId, Fault<Old>> {
        (self.heap.shared_text(s).map_err(|e| Interp::heap_error(self, e))).map_err(Fault::from)
    }
    #[inline(always)]
    fn h_stats(& self) -> HeapStats {
        HeapStats::from(self.heap.stats())
    }
    #[inline(always)]
    fn host_iter_open(&mut self, v: Value) -> Result<Option<HostIter>, Fault<Old>> {
        (Interp::host_iter_open(self, v)).map_err(Fault::from)
    }
    #[inline(always)]
    fn host_iter_next(&mut self, it: &mut HostIter) -> Result<Option<Value>, Fault<Old>> {
        (Interp::host_iter_next(self, it)).map_err(Fault::from)
    }
    #[inline(always)]
    fn host_iter_fail(&mut self, it: &mut HostIter, err: Fault<Old>) -> Fault<Old> {
        Fault::from(Interp::host_iter_fail(self, it, Error::from(err)))
    }
    #[inline(always)]
    fn bytes_of(& self, v: Value) -> Option<&[u8]> {
        Interp::bytes_of(self, v)
    }
    #[inline(always)]
    fn set_wall_clock(&mut self, c: WallClock) -> Result<(), Fault<Old>> {
        (Interp::set_wall_clock(self, c)).map_err(Fault::from)
    }
    #[inline(always)]
    fn wall_clock(& self) -> WallClock {
        Interp::wall_clock(self)
    }
    #[inline(always)]
    fn wall_now_ms(& self) -> f64 {
        Interp::wall_now_ms(self)
    }
    #[inline(always)]
    fn promise_status(& self, v: Value) -> Option<PState<Old>> {
        Interp::promise_status(self, v).map(PState::from)
    }
    #[inline(always)]
    fn clear_failed_job(&mut self) -> () {
        Interp::clear_failed_job(self)
    }
    #[inline(always)]
    fn api_args_slice(& self, a: bare_js_api::Args<'_>, from: usize, to: usize) -> Option<&[Value]> {
        Interp::api_args_slice(self, a, from, to)
    }
    #[inline(always)]
    fn api_args(& self, a: bare_js_api::Args<'_>, from: usize, to: usize) -> Vec<Value> {
        Interp::api_args(self, a, from, to)
    }
    #[inline(always)]
    fn host_new_target(& self) -> Option<ObjId> {
        Interp::host_new_target(self)
    }
    #[inline(always)]
    fn collect(&mut self, extra: &[Value]) -> Result<Collected, Fault<Old>> {
        (Interp::collect(self, extra)).map_err(Fault::from)
    }
    #[inline(always)]
    fn set_sources(&mut self, dom: std::rc::Rc<crate::browser::Dom<Old>>) -> () {
        Interp::set_api_sources(self, dom)
    }
    #[inline(always)]
    fn module_parse_o(&mut self, source: &str, url: &str) -> Result<u32, Fault<Old>> {
        (Interp::module_parse(self, source, url)).map_err(Fault::from)
    }
    #[inline(always)]
    fn module_requests_o(& self, m: u32) -> Vec<String> {
        Interp::module_requests(self, m)
    }
    #[inline(always)]
    fn module_set_requested_o(&mut self, m: u32, deps: &[u32]) -> () {
        Interp::module_set_requested(self, m, deps)
    }
    #[inline(always)]
    fn module_url_o(& self, m: u32) -> &str {
        Interp::module_url(self, m)
    }
    #[inline(always)]
    fn module_link_o(&mut self, root: u32) -> Result<(), Fault<Old>> {
        (Interp::module_link(self, root)).map_err(Fault::from)
    }
    #[inline(always)]
    fn module_evaluate_o(&mut self, m: u32) -> Result<ObjId, Fault<Old>> {
        (Interp::module_evaluate(self, m)).map_err(Fault::from)
    }
    #[inline(always)]
    fn finish_dynamic_import_o(&mut self, promise: ObjId, m: u32) -> Result<(), Fault<Old>> {
        (Interp::finish_dynamic_import(self, promise, m)).map_err(Fault::from)
    }
    #[inline(always)]
    fn reject_dynamic_import_o(&mut self, promise: ObjId, e: Fault<Old>) -> Result<(), Fault<Old>> {
        (Interp::reject_dynamic_import(self, promise, Error::from(e))).map_err(Fault::from)
    }
    #[inline(always)]
    fn fail_dynamic_import_o(&mut self, promise: ObjId, message: &str) -> Result<(), Fault<Old>> {
        (Interp::fail_dynamic_import(self, promise, message)).map_err(Fault::from)
    }
    #[inline(always)]
    fn host_bind(&mut self, target: ObjId, this: Value) -> Result<ObjId, Fault<Old>> {
        (Interp::host_bind(self, target, this)).map_err(Fault::from)
    }
    #[inline(always)]
    fn is_constructor_value(&mut self, v: Value) -> Result<bool, Fault<Old>> {
        (Interp::is_constructor_value(self, v)).map_err(Fault::from)
    }
    #[inline(always)]
    fn inspect_o(&mut self, v: Value, cap: usize, node: &dyn Fn(ObjId) -> Option<String>) -> String {
        Interp::inspect(self, v, cap, node)
    }
// ops>>
}
