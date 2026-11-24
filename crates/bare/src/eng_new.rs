//! The new engine (`bare-vex`) as an engine for the host (S0.4 PR 6c-2). Every object, string, symbol and key of the host is a
//! `Value` of Vex in a wrapper that has `Hash`. The operations go through `Engine` (with a handle that lasts for one operation) and
//! through the raw helpers of `api_raw.rs`. What Vex cannot do yet answers `Fault::Unsupported` with the name `Vex: ...`: these
//! are the lines of the phase 2 queue (`docs/progress/2026-11-21-s04-pr6c2-js2-browser.md`).

use crate::eng::{Collected, Desc, Eng, Fault, Got, HeapStats, KeyKind, Ops, PState, Prop, Val, WallClock};
use crate::js_limits::LimitKind;
use bare_vex::api::Clock;
use bare_vex::interp_rt::Realm;
use bare_vex::value::{Kind, Value};
use bare_js_api::{Abort, Args, Attrs, Engine, Error, Intrinsic, Iter, Local, Persistent, Traced};
use std::borrow::Cow;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct New;

/// A `Value` of Vex that is an object, a string, a symbol or a key. It hashes by its bits: Vex never moves a cell.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct NV(pub Value);

impl std::fmt::Debug for NV {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "NV({:#x})", self.0.bits())
    }
}
impl std::hash::Hash for NV {
    fn hash<H: std::hash::Hasher>(&self, h: &mut H) {
        h.write_u64(self.0.bits());
    }
}

/// The neutral error of Vex, for the tests that name it.
pub type NewFault = Fault<New>;

/// The index that a string key names, if it is a canonical array index.
fn index_of(units: &[u16]) -> Option<u32> {
    if units.is_empty() || units.len() > 10 || (units[0] == b'0' as u16 && units.len() > 1) {
        return None;
    }
    let mut n: u64 = 0;
    for &u in units {
        if !(b'0' as u16..=b'9' as u16).contains(&u) {
            return None;
        }
        n = n * 10 + (u - b'0' as u16) as u64;
    }
    (n < u32::MAX as u64).then_some(n as u32)
}

impl Eng for New {
    type Vm = Realm;
    type Iter = Iter<'static>;
    type Obj = NV;
    type Raw = Value;
    type Str = NV;
    type Sym = NV;
    type Key = NV;

    fn realm(eval: crate::js_limits::EvalLimits, _heap: crate::js_limits::HeapLimits) -> Result<Realm, Fault<New>> {
        bare_vex::script::load("").map(|mut vm| {
            vm.realm.init_host_intrinsics();
            // A task stops after `max_steps` back edges, so a runaway script ends only its own task.
            vm.realm.set_budget(eval.max_steps);
            vm.realm
        }).map_err(|_| Fault::Heap("the realm could not be made"))
    }

    fn kind(v: Value) -> Val<New> {
        if v.is_undefined() {
            return Val::Undefined;
        }
        if v.is_null() {
            return Val::Null;
        }
        if let Some(b) = v.as_bool() {
            return Val::Bool(b);
        }
        if let Some(n) = v.as_f64() {
            return Val::Number(n);
        }
        if let Some(i) = v.as_int() {
            return Val::Number(i as f64);
        }
        match v.as_pointer() {
            Some((Kind::String, _)) => Val::String(NV(v)),
            Some((Kind::Symbol, _)) => Val::Symbol(NV(v)),
            Some((Kind::Object, _)) => Val::Object(NV(v)),
            Some((Kind::BigInt, _)) | None => Val::BigInt,
        }
    }
    fn undef() -> Value {
        Value::UNDEFINED
    }
    fn null() -> Value {
        Value::NULL
    }
    fn bool_raw(b: bool) -> Value {
        Value::bool(b)
    }
    fn num(n: f64) -> Value {
        Realm::num(n)
    }
    fn obj_raw(o: NV) -> Value {
        o.0
    }
    fn str_raw(s: NV) -> Value {
        s.0
    }
    fn is_bigint(v: Value) -> bool {
        v.is_bigint()
    }
    fn sym_key(s: NV) -> NV {
        s
    }
    fn key_kind(k: NV) -> KeyKind<New> {
        if let Some(i) = k.0.as_int() {
            return KeyKind::Index(i as u32);
        }
        match k.0.as_pointer() {
            Some((Kind::Symbol, _)) => KeyKind::Symbol(k),
            _ => KeyKind::Str(k),
        }
    }
}

impl crate::browser::SlotKey for NV {
    /// A pointer is no dense slot: the tables of this engine do not use `slot` (`IdTab` has a sparse form for it).
    const DENSE: bool = false;
    fn slot(self) -> usize {
        (self.0.bits() >> 4) as usize
    }
}

fn lc(r: &mut Realm, v: Value) -> Local<'static> {
    r.raw_local(&v)
}

fn fault(r: &mut Realm, e: Error<'_>) -> Fault<New> {
    match e {
        Error::Throw(l) => Fault::Throw(r.raw(l)),
        Error::Abort(Abort::Limit(_)) => Fault::Limit(LimitKind::Steps),
        Error::Abort(Abort::Unsupported(w)) => Fault::Unsupported(w),
    }
}

fn error<'s>(r: &mut Realm, f: Fault<New>) -> Error<'s> {
    match f {
        Fault::Throw(v) => Error::Throw(r.raw_local(&v)),
        Fault::Limit(_) => Error::Abort(Abort::Limit("a limit of the host")),
        Fault::Unsupported(w) => Error::Abort(Abort::Unsupported(w)),
        Fault::Heap(w) => Error::Abort(Abort::Unsupported(w)),
    }
}

/// Run `f` with the handle stack as it is now, and cut it back to that length after it.
fn run<T>(r: &mut Realm, f: impl FnOnce(&mut Realm) -> Result<T, Error<'static>>) -> Result<T, Fault<New>> {
    let n = r.stack_len();
    let out = f(r).map_err(|e| fault(r, e));
    r.stack_truncate(n);
    out
}

/// The same, for a result that is a handle: it leaves as a value.
fn run_v(r: &mut Realm, f: impl FnOnce(&mut Realm) -> Result<Local<'static>, Error<'static>>) -> Result<Value, Fault<New>> {
    run(r, |r| {
        let l = f(r)?;
        Ok(r.raw(l))
    })
}

fn key_local(r: &mut Realm, k: NV) -> Local<'static> {
    lc(r, k.0)
}

fn attrs(w: bool, e: bool, c: bool) -> Attrs {
    Attrs { writable: w, enumerable: e, configurable: c }
}

impl Ops<New> for Realm {
    fn define_data(&mut self, o: NV, key: NV, v: Value, writable: bool, enumerable: bool, configurable: bool) -> Result<(), Fault<New>> {
        run(self, |r| {
            let (ol, kl, vl) = (lc(r, o.0), key_local(r, key), lc(r, v));
            Engine::define(r, ol, kl, vl, attrs(writable, enumerable, configurable))
        })
    }
    fn get_value(&mut self, base: Value, key: NV) -> Result<Value, Fault<New>> {
        run_v(self, |r| {
            let (bl, kl) = (lc(r, base), key_local(r, key));
            r.get(bl, kl)
        })
    }
    fn put_value(&mut self, base: Value, key: NV, v: Value) -> Result<(), Fault<New>> {
        run(self, |r| {
            let (bl, kl, vl) = (lc(r, base), key_local(r, key), lc(r, v));
            r.set(bl, kl, vl)
        })
    }
    fn charge_o(&mut self, n: u64) -> Result<(), Fault<New>> {
        run(self, |r| r.charge(n))
    }
    fn type_error_o(&mut self, msg: &str) -> Fault<New> {
        let e = Engine::type_error(self, msg);
        fault(self, e)
    }
    fn range_error_o(&mut self, msg: &str) -> Fault<New> {
        let e = Engine::range_error(self, msg);
        fault(self, e)
    }
    fn call_o(&mut self, f: Value, this: Value, args: &[Value]) -> Result<Value, Fault<New>> {
        run_v(self, |r| {
            let (fl, tl) = (lc(r, f), lc(r, this));
            let al: Vec<_> = args.iter().map(|&a| lc(r, a)).collect();
            r.call(fl, tl, &al)
        })
    }
    fn display_o(&self, v: Value) -> String {
        match Val::<New>::from(v) {
            Val::Undefined => "undefined".into(),
            Val::Null => "null".into(),
            Val::Bool(b) => b.to_string(),
            Val::Number(n) => Realm::num_to_string(n),
            Val::String(_) => String::from_utf16_lossy(&self.raw_units(v).unwrap_or_default()),
            Val::Symbol(_) => "Symbol()".into(),
            Val::Object(_) => "[object Object]".into(),
            Val::BigInt => "0n".into(),
        }
    }
    fn rejection_text_o(&self, v: Value) -> String {
        if self.is_error(v) { self.error_text(v) } else { self.display_o(v) }
    }
    fn new_error_o(&mut self, proto: NV, msg: &str) -> Result<NV, Fault<New>> {
        run_v(self, |r| {
            let pl = lc(r, proto.0);
            r.new_error(pl, msg)
        })
        .map(NV)
    }
    fn drain_microtasks_o(&mut self) -> Result<usize, Fault<New>> {
        run(self, |r| r.drain_microtasks())
    }
    fn compile_function_o(&mut self, name: &str, params: &str, body: &str) -> Result<NV, Fault<New>> {
        run_v(self, |r| r.compile_function(name, params, body)).map(NV)
    }
    fn eval_function_o(&mut self) -> Result<NV, Fault<New>> {
        run_v(self, |r| r.eval_function()).map(NV)
    }
    fn eval_global_o(&mut self, v: Value) -> Result<Value, Fault<New>> {
        run_v(self, |r| {
            let l = lc(r, v);
            r.eval_global(l)
        })
    }
    fn run_script_o(&mut self, source: &str) -> Result<Value, Fault<New>> {
        run_v(self, |r| r.run_script(source))
    }
    fn call_scoped_o(&mut self, f: Value, this: Value, args: &[Value], scope: &[NV]) -> Result<Value, Fault<New>> {
        run_v(self, |r| {
            let (fl, tl) = (lc(r, f), lc(r, this));
            let al: Vec<_> = args.iter().map(|&a| lc(r, a)).collect();
            let sl: Vec<_> = scope.iter().map(|s| lc(r, s.0)).collect();
            r.call_scoped(fl, tl, &al, &sl)
        })
    }
    fn queue_microtask_o(&mut self, cb: Value) -> Result<(), Fault<New>> {
        run(self, |r| {
            let l = lc(r, cb);
            r.queue_microtask(l)
        })
    }
    fn host_scope_o<T>(&mut self, f: impl FnOnce(&mut Self) -> Result<T, Fault<New>>) -> Result<T, Fault<New>> {
        let n = self.stack_len();
        let out = match self.task(f) {
            Ok(r) => r,
            Err(Abort::Limit(_)) => Err(Fault::Limit(LimitKind::CallDepth)),
            Err(Abort::Unsupported(w)) => Err(Fault::Unsupported(w)),
        };
        self.stack_truncate(n);
        out
    }
    fn host_checkpoint(&mut self) -> Result<usize, Fault<New>> {
        run(self, |r| r.checkpoint())
    }
    fn object_prototype(&self) -> NV {
        NV(self.raw_intrinsic(Intrinsic::ObjectPrototype))
    }
    fn array_prototype(&self) -> NV {
        NV(self.raw_intrinsic(Intrinsic::ArrayPrototype))
    }
    fn iterator_prototype(&self) -> NV {
        NV(self.raw_intrinsic(Intrinsic::IteratorPrototype))
    }
    fn error_prototype(&self) -> NV {
        NV(self.raw_intrinsic(Intrinsic::ErrorPrototype))
    }
    fn global_object(&self) -> NV {
        NV(self.global.value())
    }
    fn symbol_iterator(&self) -> NV {
        NV(self.raw_intrinsic(Intrinsic::SymbolIterator))
    }
    fn symbol_to_string_tag(&self) -> NV {
        NV(self.raw_intrinsic(Intrinsic::SymbolToStringTag))
    }
    fn persist_value(&mut self, v: Value) -> Persistent {
        let n = self.stack_len();
        let l = lc(self, v);
        let id = self.persist(l);
        self.stack_truncate(n);
        Persistent::from_id(id, self.slots().clone())
    }
    fn persistent_value(&self, p: &Persistent) -> Value {
        self.raw_persistent(p.id())
    }
    fn trace_value(&self, v: Value) -> Traced<Value> {
        Traced::from_raw(v)
    }
    fn traced_value(&self, t: &Traced<Value>) -> Value {
        *t.raw()
    }
    fn store_value(&mut self, t: &mut Traced<Value>, v: Value) {
        t.set_raw(v);
    }
    fn set_host_owner_obj(&mut self, o: NV, key: u32) {
        let n = self.stack_len();
        let l = lc(self, o.0);
        let _ = self.set_host_owner(l, key);
        self.stack_truncate(n);
    }
    fn array_from(&mut self, items: &[Value]) -> Result<Value, Fault<New>> {
        run_v(self, |r| {
            let il: Vec<_> = items.iter().map(|&a| lc(r, a)).collect();
            r.new_array(&il)
        })
    }
    fn array_from_proto(&mut self, items: &[Value], proto: NV) -> Result<Value, Fault<New>> {
        run_v(self, |r| {
            let il: Vec<_> = items.iter().map(|&a| lc(r, a)).collect();
            let a = r.new_array(&il)?;
            let p = lc(r, proto.0);
            r.set_prototype_of(a, Some(p))?;
            Ok(a)
        })
    }
    fn is_callable_value(&self, v: Value) -> bool {
        self.raw_callable(v)
    }
    fn to_js_string(&mut self, v: Value) -> Result<NV, Fault<New>> {
        run_v(self, |r| {
            let l = lc(r, v);
            r.to_string(l)
        })
        .map(NV)
    }
    fn to_js_number(&mut self, v: Value) -> Result<f64, Fault<New>> {
        run(self, |r| {
            let l = lc(r, v);
            Engine::to_number(r, l)
        })
    }
    fn to_js_bool(&self, v: Value) -> Result<bool, Fault<New>> {
        Ok(Realm::truthy(v))
    }
    fn host_new_promise(&mut self) -> Result<Value, Fault<New>> {
        run_v(self, |r| Engine::new_promise(r))
    }
    fn host_resolve_promise(&mut self, p: Value, v: Value) -> Result<(), Fault<New>> {
        run(self, |r| {
            let (pl, vl) = (lc(r, p), lc(r, v));
            r.resolve_promise(pl, vl)
        })
    }
    fn host_reject_promise(&mut self, p: Value, v: Value) -> Result<(), Fault<New>> {
        run(self, |r| {
            let (pl, vl) = (lc(r, p), lc(r, v));
            r.reject_promise(pl, vl)
        })
    }
    fn host_promise_then(&mut self, p: Value, ok: Value, err: Value) -> Result<(), Fault<New>> {
        run(self, |r| {
            let (pl, ol, el) = (lc(r, p), lc(r, ok), lc(r, err));
            r.promise_then(pl, ol, el)
        })
    }
    fn host_iter_result(&mut self, value: Value, done: bool) -> Result<Value, Fault<New>> {
        run_v(self, |r| {
            let o = r.new_object()?;
            let v = lc(r, value);
            r.define_named(o, "value", v, Attrs::ALL)?;
            let d = r.boolean(done);
            r.define_named(o, "done", d, Attrs::ALL)?;
            Ok(o)
        })
    }
    fn host_construct(&mut self, f: Value, args: &[Value], nt: Option<NV>) -> Result<Value, Fault<New>> {
        run_v(self, |r| {
            let fl = lc(r, f);
            let al: Vec<_> = args.iter().map(|&a| lc(r, a)).collect();
            let ntl = nt.map(|n| lc(r, n.0));
            r.construct(fl, &al, ntl)
        })
    }
    fn host_proto_from_new_target(&mut self, default: NV) -> Result<NV, Fault<New>> {
        run_v(self, |r| {
            let l = lc(r, default.0);
            r.proto_from_new_target(l)
        })
        .map(NV)
    }
    fn is_scoped_function(&self, _f: NV) -> bool {
        false
    }
    fn external_live(&self) -> usize {
        self.raw_external_live()
    }
    fn set_external_live(&mut self, n: usize) {
        self.raw_set_external_live(n);
    }
    fn add_external(&mut self, n: usize) {
        self.raw_add_external(n);
    }
    fn gc_due(&self) -> bool {
        self.raw_gc_due()
    }
    fn api_err<'s>(&mut self, e: Fault<New>) -> bare_js_api::Error<'s> {
        error(self, e)
    }
    fn api_unerr(&mut self, e: bare_js_api::Error<'_>) -> Fault<New> {
        fault(self, e)
    }
    fn h_set_proto(&mut self, o: NV, proto: Option<NV>) -> Result<bool, Fault<New>> {
        run(self, |r| {
            let ol = lc(r, o.0);
            let pl = proto.map(|p| lc(r, p.0));
            r.set_prototype_of(ol, pl)
        })
    }
    fn h_get_proto(&mut self, o: NV) -> Result<Option<NV>, Fault<New>> {
        run(self, |r| {
            let ol = lc(r, o.0);
            Ok(r.get_prototype_of(ol)?.map(|l| NV(r.raw(l))))
        })
    }
    fn h_peek_own(&mut self, o: NV, key: NV) -> Result<Option<Prop<New>>, Fault<New>> {
        self.h_get_own(o, key)
    }
    fn h_has_own(&mut self, o: NV, key: NV) -> Result<bool, Fault<New>> {
        run(self, |r| {
            let (ol, kl) = (lc(r, o.0), key_local(r, key));
            r.has_own(ol, kl)
        })
    }
    fn h_intern(&mut self, s: &str) -> Result<NV, Fault<New>> {
        Ok(NV(self.intern(s).value()))
    }
    fn h_key(&mut self, s: &str) -> Result<NV, Fault<New>> {
        let u: Vec<u16> = s.encode_utf16().collect();
        Ok(match index_of(&u) {
            Some(i) if i <= i32::MAX as u32 => NV(Value::int(i as i32)),
            _ => NV(self.intern(s).value()),
        })
    }
    fn h_str_units(&self, s: NV) -> Result<Cow<'_, [u16]>, Fault<New>> {
        self.raw_units(s.0).ok_or(Fault::Heap("Stale"))
    }
    fn h_str_len(&self, s: NV) -> Result<usize, Fault<New>> {
        self.raw_units(s.0).map(|u| u.len()).ok_or(Fault::Heap("Stale"))
    }
    fn h_to_string_lossy(&self, s: NV) -> Result<String, Fault<New>> {
        self.raw_units(s.0).map(|u| String::from_utf16_lossy(&u)).ok_or(Fault::Heap("Stale"))
    }
    fn h_symbol_description(&self, s: NV) -> Result<Option<NV>, Fault<New>> {
        self.raw_symbol_description(s.0).map(|d| d.map(NV)).ok_or(Fault::Heap("not a symbol"))
    }
    fn h_new_object(&mut self, proto: Option<NV>) -> Result<NV, Fault<New>> {
        run_v(self, |r| match proto {
            Some(p) => {
                let pl = lc(r, p.0);
                r.new_object_with(pl)
            }
            None => {
                let o = r.new_object()?;
                r.set_prototype_of(o, None)?;
                Ok(o)
            }
        })
        .map(NV)
    }
    fn h_get(&mut self, o: NV, key: NV) -> Result<Got<New>, Fault<New>> {
        self.get_value(o.0, key).map(Got::Value)
    }
    fn h_bytes(&self, _o: NV) -> Option<&[u8]> {
        None
    }
    fn h_view_parts(&self, _o: NV) -> Option<(NV, usize, usize)> {
        None
    }
    fn h_own_keys(&mut self, o: NV) -> Result<Vec<NV>, Fault<New>> {
        let keys = run(self, |r| {
            let ol = lc(r, o.0);
            Ok(Engine::own_keys(r, ol)?.into_iter().map(|l| r.raw(l)).collect::<Vec<_>>())
        })?;
        Ok(keys
            .into_iter()
            .map(|k| match self.raw_units(k).as_deref().and_then(index_of) {
                Some(i) if i <= i32::MAX as u32 => NV(Value::int(i as i32)),
                _ => NV(k),
            })
            .collect())
    }
    fn h_key_to_value(&mut self, k: NV) -> Result<Value, Fault<New>> {
        if k.0.as_int().is_some() {
            return self.to_js_string(k.0).map(|s| s.0);
        }
        Ok(k.0)
    }
    fn h_get_own(&mut self, o: NV, key: NV) -> Result<Option<Prop<New>>, Fault<New>> {
        use bare_vex::api::RawOwn;
        run(self, |r| {
            Ok(r.raw_own(o.0, key.0)?.map(|p| match p {
                RawOwn::Data { value, writable, enumerable, configurable } => Prop::Data { value, writable, enumerable, configurable },
                RawOwn::Accessor { get, set, enumerable, configurable } => Prop::Accessor { get: get.map(NV), set: set.map(NV), enumerable, configurable },
            }))
        })
    }
    fn h_define_own(&mut self, o: NV, key: NV, d: &Desc<New>) -> Result<bool, Fault<New>> {
        run(self, |r| {
            let (ol, kl) = (lc(r, o.0), key_local(r, key));
            let a = attrs(d.writable.unwrap_or(false), d.enumerable.unwrap_or(false), d.configurable.unwrap_or(false));
            if d.get.is_some() || d.set.is_some() {
                let g = d.get.flatten().map(|g| lc(r, g.0));
                let s = d.set.flatten().map(|s| lc(r, s.0));
                Engine::define_accessor(r, ol, kl, g, s, a)?;
            } else {
                let vl = lc(r, d.value.unwrap_or(Value::UNDEFINED));
                Engine::define(r, ol, kl, vl, a)?;
            }
            Ok(true)
        })
    }
    fn h_set_lazy_on(&mut self, o: NV) -> Result<(), Fault<New>> {
        run(self, |r| {
            let l = lc(r, o.0);
            r.set_lazy(l, false)
        })
    }
    fn h_clear_lazy_on(&mut self, o: NV) {
        let n = self.stack_len();
        let l = lc(self, o.0);
        self.clear_lazy(l);
        self.stack_truncate(n);
    }
    fn h_new_named(&mut self, proto: Option<NV>, key: u32) -> Result<NV, Fault<New>> {
        run_v(self, |r| {
            let pl = proto.map(|p| lc(r, p.0));
            r.new_host_named(pl, key, false)
        })
        .map(NV)
    }
    fn h_new_named_open(&mut self, proto: Option<NV>, key: u32) -> Result<NV, Fault<New>> {
        run_v(self, |r| {
            let pl = proto.map(|p| lc(r, p.0));
            r.new_host_named(pl, key, true)
        })
        .map(NV)
    }
    fn h_new_indexed(&mut self, proto: Option<NV>, key: u32) -> Result<NV, Fault<New>> {
        run_v(self, |r| {
            let pl = proto.map(|p| lc(r, p.0));
            r.new_host_indexed(pl, key)
        })
        .map(NV)
    }
    fn h_host_key(&self, o: NV) -> Option<u32> {
        self.raw_host_key(o.0)
    }
    fn h_shared_text(&mut self, s: &str) -> Result<NV, Fault<New>> {
        Ok(NV(self.string(s)))
    }
    fn h_stats(&self) -> HeapStats {
        HeapStats::default()
    }
    fn host_iter_open(&mut self, v: Value) -> Result<Option<Iter<'static>>, Fault<New>> {
        // The two handles of the iteration stay on the stack until the host call ends (no cut here).
        let l = lc(self, v);
        self.iter_open(l).map_err(|e| fault(self, e))
    }
    fn host_iter_next(&mut self, it: &mut Iter<'static>) -> Result<Option<Value>, Fault<New>> {
        match self.iter_next(it) {
            Ok(x) => Ok(x.map(|l| self.raw(l))),
            Err(e) => Err(fault(self, e)),
        }
    }
    fn host_iter_fail(&mut self, it: &mut Iter<'static>, err: Fault<New>) -> Fault<New> {
        let e = error(self, err);
        let e = self.iter_fail(it, e);
        fault(self, e)
    }
    fn bytes_of(&self, _v: Value) -> Option<&[u8]> {
        // Vex has no ArrayBuffer, TypedArray or DataView cells yet.
        None
    }
    fn set_wall_clock(&mut self, c: WallClock) -> Result<(), Fault<New>> {
        let clock = match c {
            WallClock::System => Clock::System,
            WallClock::Virtual(t) => Clock::Virtual(t),
            WallClock::Host if bare_net::vclock::on() => Clock::Host(Some(Box::new(|| {
                bare_net::vclock::advance_by(bare_net::vclock::READ_MS);
                bare_net::vclock::epoch_ms()
            }))),
            WallClock::Host => Clock::Host(None),
        };
        if self.raw_set_clock(clock) {
            Ok(())
        } else {
            Err(Fault::Unsupported("virtual wall clock must be a whole number of milliseconds within +-8.64e15"))
        }
    }
    fn wall_clock(&self) -> WallClock {
        match self.raw_clock() {
            Clock::System => WallClock::System,
            Clock::Virtual(t) => WallClock::Virtual(*t),
            Clock::Host(_) => WallClock::Host,
        }
    }
    fn wall_now_ms(&self) -> f64 {
        Engine::wall_now_ms(self)
    }
    fn promise_status(&self, v: Value) -> Option<PState<New>> {
        self.raw_promise(v).map(|(s, x)| match s {
            0 => PState::Pending,
            1 => PState::Fulfilled(x),
            _ => PState::Rejected(x),
        })
    }
    fn clear_failed_job(&mut self) {
        Engine::clear_failed_job(self)
    }
    fn api_args_slice(&self, a: Args<'_>, from: usize, to: usize) -> Option<&[Value]> {
        self.raw_args_slice(a, from, to)
    }
    fn api_args(&self, a: Args<'_>, from: usize, to: usize) -> Vec<Value> {
        if let Some(args) = self.raw_args_slice(a, from, to) {
            return args.to_vec();
        }
        a.iter().skip(from).take(to.saturating_sub(from)).map(|l| self.raw(l)).collect()
    }
    fn host_new_target(&self) -> Option<NV> {
        self.raw_new_target().map(NV)
    }
    fn collect(&mut self, extra: &[Value]) -> Result<Collected, Fault<New>> {
        let count = self.raw_collect(extra);
        Ok(Collected { objects: count.objects, strings: count.strings, symbols: count.symbols })
    }
    fn set_sources(&mut self, dom: std::rc::Rc<crate::browser::Dom<New>>) {
        // Vex uses the same host for source reads, calls and owner tracing.
        Engine::set_host(self, dom);
    }
    fn module_parse_o(&mut self, source: &str, url: &str) -> Result<u32, Fault<New>> {
        run(self, |r| r.module_parse(source, url))
    }
    fn module_requests_o(&self, m: u32) -> Vec<String> {
        Engine::module_requests(self, m)
    }
    fn module_set_requested_o(&mut self, m: u32, deps: &[u32]) {
        Engine::module_set_requested(self, m, deps)
    }
    fn module_url_o(&self, m: u32) -> &str {
        Engine::module_url(self, m)
    }
    fn module_link_o(&mut self, root: u32) -> Result<(), Fault<New>> {
        run(self, |r| r.module_link(root))
    }
    fn module_evaluate_o(&mut self, m: u32) -> Result<NV, Fault<New>> {
        run_v(self, |r| r.module_evaluate(m)).map(NV)
    }
    fn finish_dynamic_import_o(&mut self, promise: NV, m: u32) -> Result<(), Fault<New>> {
        let p = self.persist_value(promise.0);
        run(self, |r| r.finish_dynamic_import(&p, m))
    }
    fn reject_dynamic_import_o(&mut self, promise: NV, e: Fault<New>) -> Result<(), Fault<New>> {
        let p = self.persist_value(promise.0);
        run(self, |r| {
            let el = match e {
                Fault::Throw(v) => lc(r, v),
                _ => r.undefined(),
            };
            r.reject_dynamic_import(&p, el)
        })
    }
    fn fail_dynamic_import_o(&mut self, promise: NV, message: &str) -> Result<(), Fault<New>> {
        let p = self.persist_value(promise.0);
        run(self, |r| r.fail_dynamic_import(&p, message))
    }
    fn host_bind(&mut self, target: NV, this: Value) -> Result<NV, Fault<New>> {
        run_v(self, |r| {
            let (tl, thl) = (lc(r, target.0), lc(r, this));
            r.bind(tl, thl)
        })
        .map(NV)
    }
    fn is_constructor_value(&mut self, v: Value) -> Result<bool, Fault<New>> {
        Ok(self.raw_constructor(v))
    }
    fn inspect_o(&mut self, v: Value, cap: usize, node: &dyn Fn(NV) -> Option<String>) -> String {
        self.raw_inspect(v, cap, &|o| node(NV(o)))
    }
}

impl crate::browser::Browser<New> {
    /// Anchor the wall clock to the scheduler of a virtual session without replay.
    pub(crate) fn with_session_wall_clock(mut self, enabled: bool) -> Self {
        if enabled && !bare_net::vclock::on() {
            let Self { it: vm, dom, .. } = &mut self;
            let dom = std::rc::Rc::clone(dom);
            let origin = Engine::wall_now_ms(vm) - dom.now_ms();
            vm.raw_set_clock(Clock::Host(Some(Box::new(move || (origin + dom.now_ms()).floor()))));
        }
        self
    }
}

impl From<Value> for Val<New> {
    fn from(v: Value) -> Self {
        <New as Eng>::kind(v)
    }
}

#[cfg(test)]
#[path = "eng_new_tests.rs"]
mod browser_new_adapter_tests;
