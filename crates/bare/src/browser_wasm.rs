//! `WebAssembly` (WebAssembly JavaScript Interface). The runtime is wasmi, an interpreter. Design: `docs/internal/design-webassembly.md`.
//! Ladybird (`Libraries/LibWeb/Bindings/Rust/src/web_assembly`) was the reference for the order of the steps.
//!
//! - `WebAssembly` is an accessor of the global until the first read. The first read makes the namespace, the classes and the wasmi
//!   engine. A page that never reads it pays for one function.
//! - One wasmi store for each page. It lives as long as the page: wasmi cannot free an instance before its store. The JS functions
//!   that a module imports and the values in `externref`s are `Persistent` for the same time.
//! - A `Module`, an `Instance` or a `Global` is an owner (`Owner::Wasm`) with an entry in `Wasm::items`. `prune` drops the entry.
//! - An exported function is a bound host function. Its `this` is the index of the wasmi function in `St::funcs`. The same wasmi
//!   function gives the same JS function each time (`St::func_objs`).
//! - The store is in a cell. A call into wasm takes it out and puts it back after. A JS import that runs during the call finds the
//!   store through the wasmi `Caller` (`CUR`), so JS can call into wasm again.
//! - `compile` and `instantiate` do the work in the call and settle the promise after it.
//! - Not here: `Memory`, `Table`, `Tag`, `Exception`, `compileStreaming`, `instantiateStreaming`, a step limit in wasm code, SIMD.
//!   A module that imports or exports a memory or a table fails with a `LinkError` that says so.

use crate::browser::*;
use crate::browser_scoped::*;
use crate::eng::Eng;
use bare_js_api::{Abort, Args, Attrs, Error, HostFn, Intrinsic, Local, Persistent, Traced, Type};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use wasmi::{AsContext, AsContextMut, Caller, Extern, ExternRef, ExternType, Func, FuncType, Global, Module, Mutability, Nullable, Store, StoreContextMut, Val, ValType};

pub(crate) enum Item<E: Eng> {
    Module(Module),
    /// An instance keeps its exports object.
    Instance(Traced<E::Raw>),
    Global(Global),
}

/// What a JS import threw or why the engine stopped. The nearest call from JS into wasm gives it back.
enum Pend {
    Throw(Persistent),
    Abort(Abort),
}

const MODULE: usize = 0;
const INSTANCE: usize = 1;
const GLOBAL: usize = 2;
const COMPILE_ERROR: usize = 3;
const LINK_ERROR: usize = 4;
const RUNTIME_ERROR: usize = 5;
const BIGINT: usize = 6;
const AS_INT_N: usize = 7;
const FREEZE: usize = 8;
const CALL: usize = 9;
const NAMESPACE: usize = 10;

struct St<E: Eng> {
    engine: wasmi::Engine,
    /// The prototypes, the error classes and the functions of the realm that the module uses, by the constants above.
    held: Vec<Persistent>,
    /// The JS functions that modules import, by the index that their wasmi function keeps.
    imports: Vec<Persistent>,
    /// The JS values in `externref`s, by the index that the `ExternRef` keeps.
    externs: Vec<Persistent>,
    /// The wasmi functions that JS has seen, by the `this` of their JS function.
    funcs: Vec<Func>,
    /// The JS function of a wasmi function, by its handle bytes.
    func_objs: HashMap<[u8; FUNC_BYTES], Persistent>,
    /// The wasmi function of a JS function.
    of_obj: IdMap<E::Obj, Func>,
}

pub(crate) struct Wasm<E: Eng> {
    st: RefCell<Option<Box<St<E>>>>,
    store: RefCell<Option<Store<()>>>,
    pub(crate) items: RefCell<IdMap<E::Obj, Item<E>>>,
    pend: RefCell<Option<Pend>>,
}

impl<E: Eng> Default for Wasm<E> {
    fn default() -> Self {
        Wasm { st: RefCell::new(None), store: RefCell::new(None), items: RefCell::new(IdMap::default()), pend: RefCell::new(None) }
    }
}

const FUNC_BYTES: usize = size_of::<Func>();

/// The bytes of a wasmi function handle: a store id and an index, so equal for the same function of the store.
fn func_key(f: Func) -> [u8; FUNC_BYTES] {
    // SAFETY: `Func` is a `Copy` handle of two integers with no padding (`func_handle_has_no_padding` in the tests checks the size).
    unsafe { std::mem::transmute_copy(&f) }
}

// ------------------------------------------------------------------------------------------ the store during a call

thread_local! {
    /// The page, the scope and the wasmi caller of the call into wasm that runs now, as erased pointers; null outside a call.
    static CUR: Cell<(*const (), *mut (), *mut ())> = const { Cell::new((std::ptr::null(), std::ptr::null_mut(), std::ptr::null_mut())) };
}

/// Run `f` with the store: the caller's store during a JS import, else the page's store, which `f` takes out of its cell.
fn with_store<E: Eng, R>(d: &Dom<E>, f: impl FnOnce(StoreContextMut<'_, ()>) -> R) -> R {
    let caller = CUR.with(|c| c.get().2);
    if !caller.is_null() {
        // SAFETY: `CUR.2` is set only while `host_import` runs and holds the caller; the import's JS runs on this thread inside it.
        let c = unsafe { &mut *(caller as *mut Caller<'static, ()>) };
        return f(c.as_context_mut());
    }
    let mut store = d.wasm.store.borrow_mut().take().expect("the store is in its cell outside a call");
    let r = f(store.as_context_mut());
    *d.wasm.store.borrow_mut() = Some(store);
    r
}

/// Run `f`, a call into wasm, with `d` and `s` as the page and the scope of the JS imports that it runs.
fn enter<'s, E: Eng, R>(d: &Dom<E>, s: &mut Sc<'s, E>, f: impl FnOnce() -> R) -> R {
    // A call from a JS import keeps the import's caller: the store is there.
    let old = CUR.with(|c| c.get());
    CUR.with(|c| c.set((d as *const Dom<E> as *const (), s as *mut Sc<'s, E> as *mut (), old.2)));
    let r = f();
    CUR.with(|c| c.set(old));
    r
}

/// A JS import: wasmi runs it with the caller of the module.
fn host_import<E: Eng>(mut caller: Caller<'_, ()>, idx: usize, params: &[Val], results: &mut [Val]) -> Result<(), wasmi::Error> {
    let (dp, sp, old) = CUR.with(|c| c.get());
    // SAFETY: `enter` set both pointers for the call into wasm that runs this import, and they outlive it.
    let (d, s) = unsafe { (&*(dp as *const Dom<E>), &mut *(sp as *mut Sc<'static, E>)) };
    CUR.with(|c| c.set((dp, sp, &mut caller as *mut Caller<'_, ()> as *mut ())));
    let r = run_import(d, s, idx, params, results);
    CUR.with(|c| c.set((dp, sp, old)));
    r.map_err(|e| {
        let p = match e {
            Error::Throw(v) => Pend::Throw(s.persist(v)),
            Error::Abort(a) => Pend::Abort(a),
        };
        *d.wasm.pend.borrow_mut() = Some(p);
        wasmi::Error::new("a JavaScript import threw")
    })
}

fn run_import<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, idx: usize, params: &[Val], results: &mut [Val]) -> Res<'s, ()> {
    let f = local(d, s, |st| Some(&st.imports[idx])).expect("an import");
    let mut args = Vec::with_capacity(params.len());
    for p in params {
        args.push(to_js(d, s, p)?);
    }
    let undef = s.undefined();
    let r = s.call(f, undef, &args)?;
    match results.len() {
        0 => {}
        1 => results[0] = to_wasm(d, s, r, results[0].ty())?,
        n => {
            let Some(len) = s.array_len(r)? else { return Err(s.type_error("A WebAssembly import must return an array for several results")) };
            if len as usize != n {
                return Err(s.type_error("A WebAssembly import returned the wrong number of results"));
            }
            for (i, slot) in results.iter_mut().enumerate() {
                let k = s.number(i as f64);
                let v = s.get(r, k)?;
                *slot = to_wasm(d, s, v, slot.ty())?;
            }
        }
    }
    Ok(())
}

/// The JS error for a wasmi error of a call: the pending JS exception, else a `RuntimeError`.
fn call_error<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, e: wasmi::Error) -> Error<'s> {
    match d.wasm.pend.borrow_mut().take() {
        Some(Pend::Throw(p)) => Error::Throw(s.local(&p)),
        Some(Pend::Abort(a)) => Error::Abort(a),
        None => make_error(d, s, RUNTIME_ERROR, &e.to_string()),
    }
}

// ------------------------------------------------------------------------------------------ values

fn st<E: Eng, R>(d: &Dom<E>, f: impl FnOnce(&mut St<E>) -> R) -> R {
    f(d.wasm.st.borrow_mut().as_mut().expect("made"))
}

/// The local of a `Persistent` of the state.
fn local<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, f: impl FnOnce(&St<E>) -> Option<&Persistent>) -> Option<Local<'s>> {
    let g = d.wasm.st.borrow();
    f(g.as_ref().expect("made")).map(|p| s.local(p))
}

fn held<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, i: usize) -> Local<'s> {
    local(d, s, |st| Some(&st.held[i])).expect("held")
}

fn make_error<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, class: usize, msg: &str) -> Error<'s> {
    let c = held(d, s, class);
    match s.string(msg).and_then(|m| s.construct(c, &[m], None)) {
        Ok(e) => Error::Throw(e),
        Err(e) => e,
    }
}

/// `ToInt32`.
fn int32(n: f64) -> i32 {
    if n.is_finite() { (n.trunc().rem_euclid(4294967296.0) as u64) as u32 as i32 } else { 0 }
}

/// ToWebAssemblyValue.
fn to_wasm<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, v: Local<'s>, ty: ValType) -> Res<'s, Val> {
    Ok(match ty {
        ValType::I32 => Val::I32(int32(s.to_number(v)?)),
        ValType::F32 => Val::F32((s.to_number(v)? as f32).into()),
        ValType::F64 => Val::F64(s.to_number(v)?.into()),
        ValType::I64 => {
            let (f, b) = (held(d, s, AS_INT_N), held(d, s, BIGINT));
            let bits = s.number(64.0);
            let n = s.call(f, b, &[bits, v])?;
            let text = s.to_string(n)?;
            let text = s.as_str(text).map(|t| t.to_string_lossy()).unwrap_or_default();
            Val::I64(text.parse().map_err(|_| s.type_error("Cannot convert the value to a 64-bit integer"))?)
        }
        ValType::FuncRef => {
            if s.type_of(v) == Type::Null {
                Val::FuncRef(Nullable::Null)
            } else {
                let f = obj_id::<E>(s, v).and_then(|o| st(d, |st| st.of_obj.get(&o).copied()));
                Val::FuncRef(Nullable::Val(f.ok_or_else(|| s.type_error("The value is not an exported WebAssembly function"))?))
            }
        }
        ValType::ExternRef => {
            if s.type_of(v) == Type::Null {
                Val::ExternRef(Nullable::Null)
            } else {
                let p = s.persist(v);
                let i = st(d, |st| {
                    st.externs.push(p);
                    st.externs.len() - 1
                });
                Val::ExternRef(Nullable::Val(with_store(d, |c| ExternRef::new(c, i))))
            }
        }
        ValType::V128 => return Err(s.type_error("A v128 value cannot pass between JavaScript and WebAssembly")),
    })
}

/// ToJSValue.
fn to_js<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, v: &Val) -> Res<'s, Local<'s>> {
    Ok(match v {
        Val::I32(n) => s.number(*n as f64),
        Val::F32(n) => s.number(f32::from(*n) as f64),
        Val::F64(n) => s.number(f64::from(*n)),
        Val::I64(n) => {
            let b = held(d, s, BIGINT);
            let text = s.string(&n.to_string())?;
            let undef = s.undefined();
            s.call(b, undef, &[text])?
        }
        Val::FuncRef(Nullable::Val(f)) => func_obj(d, s, *f)?,
        Val::ExternRef(Nullable::Val(r)) => {
            let i = with_store(d, |c| r.data(c.as_context()).downcast_ref::<usize>().copied());
            match i.and_then(|i| local(d, s, |st| st.externs.get(i))) {
                Some(v) => v,
                None => s.undefined(),
            }
        }
        Val::FuncRef(Nullable::Null) | Val::ExternRef(Nullable::Null) => s.null(),
        Val::V128(_) => return Err(s.type_error("A v128 value cannot pass between JavaScript and WebAssembly")),
    })
}

/// The JS function of the wasmi function `f`: made at the first use, the same after.
fn func_obj<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, f: Func) -> Res<'s, Local<'s>> {
    if let Some(o) = local(d, s, |st| st.func_objs.get(&func_key(f))) {
        return Ok(o);
    }
    let i = st(d, |st| {
        st.funcs.push(f);
        st.funcs.len() - 1
    });
    let call = held(d, s, CALL);
    let this = s.number(i as f64);
    let o = s.bind(call, this)?;
    let len = with_store(d, |c| f.ty(c).params().len());
    let attrs = Attrs { writable: false, enumerable: false, configurable: true };
    let name = s.string(&i.to_string())?;
    s.define_str(o, "name", name, attrs)?;
    let len = s.number(len as f64);
    s.define_str(o, "length", len, attrs)?;
    let p = s.persist(o);
    let id = obj_id::<E>(s, o).expect("a function");
    st(d, |st| {
        st.func_objs.insert(func_key(f), p);
        st.of_obj.insert(id, f);
    });
    Ok(o)
}

/// The bytes of a `BufferSource`, copied.
fn source_bytes<'s, E: Eng>(s: &mut Sc<'s, E>, v: Option<Local<'s>>) -> Res<'s, Vec<u8>> {
    match v.filter(|v| s.type_of(*v) == Type::Object).and_then(|v| s.bytes(v).map(<[u8]>::to_vec)) {
        Some(b) => Ok(b),
        None => Err(s.type_error("The argument must be an ArrayBuffer or an ArrayBuffer view")),
    }
}

fn compile_bytes<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, bytes: &[u8]) -> Res<'s, Module> {
    let engine = st(d, |st| st.engine.clone());
    Module::new(&engine, bytes).map_err(|e| make_error(d, s, COMPILE_ERROR, &e.to_string()))
}

/// A new object of the class `proto` (an index of `held`) with the entry `item`.
fn new_item<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, proto: Local<'s>, item: Item<E>) -> Res<'s, Local<'s>> {
    let o = s.new_object_with(proto)?;
    let id = obj_id::<E>(s, o).expect("an object");
    d.wasm.items.borrow_mut().insert(id, item);
    d.own_obj(s.engine(), id, Owner::Wasm(id));
    Ok(o)
}

fn module_of<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, v: Option<Local<'s>>) -> Res<'s, Module> {
    let m = v.and_then(|v| obj_id::<E>(s, v)).and_then(|o| match d.wasm.items.borrow().get(&o) {
        Some(Item::Module(m)) => Some(m.clone()),
        _ => None,
    });
    m.ok_or_else(|| s.type_error("The argument must be a WebAssembly.Module"))
}

fn kind_name(t: &ExternType) -> &'static str {
    match t {
        ExternType::Func(_) => "function",
        ExternType::Table(_) => "table",
        ExternType::Memory(_) => "memory",
        ExternType::Global(_) => "global",
    }
}

// ------------------------------------------------------------------------------------------ instantiate

fn instantiate<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, module: &Module, imports: Option<Local<'s>>) -> Res<'s, Local<'s>> {
    let imports = imports.filter(|v| s.type_of(*v) != Type::Undefined);
    if let Some(i) = imports
        && !matches!(s.type_of(i), Type::Object | Type::Function)
    {
        return Err(s.type_error("The imports argument must be an object"));
    }
    let mut externs = Vec::new();
    for imp in module.imports() {
        let Some(obj) = imports else { return Err(s.type_error("A module with imports needs an imports argument")) };
        let m = s.get_str(obj, imp.module())?;
        if !matches!(s.type_of(m), Type::Object | Type::Function) {
            return Err(s.type_error(&format!("The import module \"{}\" is not an object", imp.module())));
        }
        let v = s.get_str(m, imp.name())?;
        let link = |s: &mut Sc<'s, E>, what: &str| make_error(d, s, LINK_ERROR, &format!("Import \"{}\".\"{}\": {what}", imp.module(), imp.name()));
        externs.push(match imp.ty() {
            ExternType::Func(ty) => {
                if !s.is_callable(v) {
                    return Err(link(s, "the value must be a function"));
                }
                let known = obj_id::<E>(s, v).and_then(|o| st(d, |st| st.of_obj.get(&o).copied()));
                match known {
                    Some(f) => Extern::Func(f),
                    None => Extern::Func(import_func::<E>(d, s, v, ty.clone())),
                }
            }
            ExternType::Global(gt) => {
                let known = obj_id::<E>(s, v).and_then(|o| match d.wasm.items.borrow().get(&o) {
                    Some(Item::Global(g)) => Some(*g),
                    _ => None,
                });
                match known {
                    Some(g) => Extern::Global(g),
                    None if gt.mutability() == Mutability::Var => return Err(link(s, "a mutable global must be a WebAssembly.Global")),
                    None => {
                        let t = gt.content();
                        if t == ValType::I64 && s.type_of(v) != Type::BigInt {
                            return Err(link(s, "the value must be a BigInt"));
                        }
                        if matches!(t, ValType::I32 | ValType::F32 | ValType::F64) && s.type_of(v) != Type::Number {
                            return Err(link(s, "the value must be a number"));
                        }
                        let val = to_wasm(d, s, v, t)?;
                        Extern::Global(with_store(d, |c| Global::new(c, val, Mutability::Const)))
                    }
                }
            }
            ExternType::Memory(_) | ExternType::Table(_) => return Err(link(s, "bare does not support imports of memories and tables yet")),
        });
    }
    if let Some(e) = module.exports().find(|e| matches!(e.ty(), ExternType::Memory(_) | ExternType::Table(_))) {
        return Err(make_error(d, s, LINK_ERROR, &format!("Export \"{}\": bare does not support exports of memories and tables yet", e.name())));
    }
    let r = enter(d, s, || with_store(d, |mut c| wasmi::Instance::new(&mut c, module, &externs)));
    let inst = r.map_err(|e| {
        if d.wasm.pend.borrow().is_some() || e.as_trap_code().is_some() {
            call_error(d, s, e)
        } else {
            make_error(d, s, LINK_ERROR, &e.to_string())
        }
    })?;
    let exports = s.new_object()?;
    s.set_prototype_of(exports, None)?;
    let list: Vec<(String, Extern)> = with_store(d, |c| inst.exports(c.as_context()).map(|e| (e.name().to_string(), e.into_extern())).collect());
    let gp = held(d, s, GLOBAL);
    for (name, e) in list {
        let v = match e {
            Extern::Func(f) => func_obj(d, s, f)?,
            Extern::Global(g) => new_item(d, s, gp, Item::Global(g))?,
            Extern::Memory(_) | Extern::Table(_) => unreachable!("refused above"),
        };
        s.define_str(exports, &name, v, Attrs { writable: false, enumerable: true, configurable: false })?;
    }
    let (freeze, undef) = (held(d, s, FREEZE), s.undefined());
    s.call(freeze, undef, &[exports])?;
    let ip = held(d, s, INSTANCE);
    let t = s.trace_ref(exports);
    new_item(d, s, ip, Item::Instance(t))
}

fn import_func<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, f: Local<'s>, ty: FuncType) -> Func {
    let p = s.persist(f);
    let idx = st(d, |st| {
        st.imports.push(p);
        st.imports.len() - 1
    });
    with_store(d, |c| Func::new(c, ty, move |caller, params, results| host_import::<E>(caller, idx, params, results)))
}

// ------------------------------------------------------------------------------------------ handlers

fn validate<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let bytes = source_bytes::<E>(s, a.get(0))?;
    let engine = st(d, |st| st.engine.clone());
    Ok(s.boolean(Module::validate(&engine, &bytes).is_ok()))
}

/// Settle a new promise with `r`.
fn promise<'s, E: Eng>(s: &mut Sc<'s, E>, r: Res<'s, Local<'s>>) -> Res<'s, Local<'s>> {
    let p = s.new_promise()?;
    match r {
        Ok(v) => s.resolve_promise(p, v)?,
        Err(Error::Throw(e)) => s.reject_promise(p, e)?,
        Err(e) => return Err(e),
    }
    Ok(p)
}

fn compile<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let r = source_bytes::<E>(s, a.get(0)).and_then(|b| compile_bytes(d, s, &b)).and_then(|m| {
        let mp = held(d, s, MODULE);
        new_item(d, s, mp, Item::Module(m))
    });
    promise::<E>(s, r)
}

fn instantiate_fn<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let given = a.get(0).filter(|v| obj_id::<E>(s, *v).is_some_and(|o| matches!(d.wasm.items.borrow().get(&o), Some(Item::Module(_)))));
    let r = match given {
        Some(m) => module_of(d, s, Some(m)).and_then(|module| instantiate(d, s, &module, a.get(1))),
        None => (|| {
            let bytes = source_bytes::<E>(s, a.get(0))?;
            let module = compile_bytes(d, s, &bytes)?;
            let mp = held(d, s, MODULE);
            let mo = new_item(d, s, mp, Item::Module(module.clone()))?;
            let inst = instantiate(d, s, &module, a.get(1))?;
            let pair = s.new_object()?;
            s.set_str(pair, "module", mo)?;
            s.set_str(pair, "instance", inst)?;
            Ok(pair)
        })(),
    };
    promise::<E>(s, r)
}

fn module_ctor<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let bytes = source_bytes::<E>(s, a.get(0))?;
    let module = compile_bytes(d, s, &bytes)?;
    let mp = held(d, s, MODULE);
    let proto = s.proto_from_new_target(mp)?;
    new_item(d, s, proto, Item::Module(module))
}

fn module_exports<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let m = module_of(d, s, a.get(0))?;
    let mut out = Vec::new();
    for e in m.exports() {
        let o = s.new_object()?;
        let (name, kind) = (s.string(e.name())?, s.string(kind_name(e.ty()))?);
        s.set_str(o, "name", name)?;
        s.set_str(o, "kind", kind)?;
        out.push(o);
    }
    s.new_array(&out)
}

fn module_imports<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let m = module_of(d, s, a.get(0))?;
    let mut out = Vec::new();
    for i in m.imports() {
        let o = s.new_object()?;
        let (module, name, kind) = (s.string(i.module())?, s.string(i.name())?, s.string(kind_name(i.ty()))?);
        s.set_str(o, "module", module)?;
        s.set_str(o, "name", name)?;
        s.set_str(o, "kind", kind)?;
        out.push(o);
    }
    s.new_array(&out)
}

fn module_sections<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let m = module_of(d, s, a.get(0))?;
    let Some(name) = a.get(1).filter(|v| s.type_of(*v) != Type::Undefined) else { return Err(s.type_error("The section name is required")) };
    let name = s.to_string(name)?;
    let name = s.as_str(name).map(|t| t.to_string_lossy()).unwrap_or_default();
    let mut out = Vec::new();
    for c in m.custom_sections().filter(|c| c.name() == name) {
        out.push(s.new_array_buffer(c.data().to_vec())?);
    }
    s.new_array(&out)
}

fn instance_ctor<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let module = module_of(d, s, a.get(0))?;
    let ip = held(d, s, INSTANCE);
    let proto = s.proto_from_new_target(ip)?;
    let inst = instantiate(d, s, &module, a.get(1))?;
    s.set_prototype_of(inst, Some(proto))?;
    Ok(inst)
}

fn instance_exports<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let o = obj_id::<E>(s, this);
    let items = d.wasm.items.borrow();
    match o.and_then(|o| items.get(&o)) {
        Some(Item::Instance(t)) => Ok(s.traced(t)),
        _ => Err(s.type_error("Illegal invocation")),
    }
}

fn val_type<'s, E: Eng>(s: &mut Sc<'s, E>, v: Local<'s>) -> Res<'s, ValType> {
    let v = s.to_string(v)?;
    let t = s.as_str(v).map(|t| t.to_string_lossy()).unwrap_or_default();
    Ok(match t.as_str() {
        "i32" => ValType::I32,
        "i64" => ValType::I64,
        "f32" => ValType::F32,
        "f64" => ValType::F64,
        "anyfunc" | "funcref" => ValType::FuncRef,
        "externref" => ValType::ExternRef,
        _ => return Err(s.type_error(&format!("\"{t}\" is not a value type of WebAssembly.Global"))),
    })
}

fn global_ctor<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let Some(desc) = a.get(0).filter(|v| matches!(s.type_of(*v), Type::Object | Type::Function)) else {
        return Err(s.type_error("The descriptor must be an object"));
    };
    let mutable = s.get_str(desc, "mutable")?;
    let mutable = s.to_boolean(mutable);
    let ty = s.get_str(desc, "value")?;
    let ty = val_type::<E>(s, ty)?;
    let val = match a.get(1).filter(|v| s.type_of(*v) != Type::Undefined) {
        Some(v) => to_wasm(d, s, v, ty)?,
        None if ty == ValType::ExternRef => {
            let u = s.undefined();
            to_wasm(d, s, u, ty)?
        }
        None => Val::default(ty),
    };
    let m = if mutable { Mutability::Var } else { Mutability::Const };
    let g = with_store(d, |c| Global::new(c, val, m));
    let gp = held(d, s, GLOBAL);
    let proto = s.proto_from_new_target(gp)?;
    new_item(d, s, proto, Item::Global(g))
}

fn global_of<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>) -> Res<'s, Global> {
    let g = obj_id::<E>(s, this).and_then(|o| match d.wasm.items.borrow().get(&o) {
        Some(Item::Global(g)) => Some(*g),
        _ => None,
    });
    g.ok_or_else(|| s.type_error("Illegal invocation"))
}

fn global_value<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let g = global_of(d, s, this)?;
    if let Some(v) = a.get(0) {
        let (ty, m) = with_store(d, |c| (g.ty(&c).content(), g.ty(&c).mutability()));
        if m == Mutability::Const {
            return Err(s.type_error("The WebAssembly.Global is immutable"));
        }
        let val = to_wasm(d, s, v, ty)?;
        with_store(d, |c| g.set(c, val)).map_err(|e| s.type_error(&e.to_string()))?;
        return Ok(s.undefined());
    }
    let val = with_store(d, |c| g.get(c));
    to_js(d, s, &val)
}

fn global_get<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let g = global_of(d, s, this)?;
    let val = with_store(d, |c| g.get(c));
    to_js(d, s, &val)
}

/// An exported function: `this` is the index of its wasmi function.
fn call_export<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let i = s.as_number(this).expect("a bound index") as usize;
    let f = st(d, |st| st.funcs[i]);
    let ty = with_store(d, |c| f.ty(c));
    let mut args = Vec::with_capacity(ty.params().len());
    for (i, t) in ty.params().iter().enumerate() {
        let v = match a.get(i) {
            Some(v) => v,
            None => s.undefined(),
        };
        args.push(to_wasm(d, s, v, *t)?);
    }
    let mut out: Vec<Val> = ty.results().iter().map(|t| Val::default(*t)).collect();
    let r = enter(d, s, || with_store(d, |c| f.call(c, &args, &mut out)));
    r.map_err(|e| call_error(d, s, e))?;
    match out.len() {
        0 => Ok(s.undefined()),
        1 => to_js(d, s, &out[0]),
        _ => {
            let mut vs = Vec::with_capacity(out.len());
            for v in &out {
                vs.push(to_js(d, s, v)?);
            }
            s.new_array(&vs)
        }
    }
}

// ------------------------------------------------------------------------------------------ install

/// The classes of the namespace, as a script: the error classes are ordinary subclasses of `Error`.
const ERRORS: &str = "(function () { var out = []; for (var name of ['CompileError', 'LinkError', 'RuntimeError']) { var C = ({ [name]: class extends Error {} })[name]; Object.defineProperty(C.prototype, 'name', { value: name, writable: true, configurable: true }); out.push(C); } return out; })()";

#[cold]
#[inline(never)]
fn make<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>) -> Res<'s, Local<'s>> {
    let ns = s.new_object()?;
    let tag = s.intrinsic(Intrinsic::SymbolToStringTag);
    let attrs = Attrs { writable: false, enumerable: false, configurable: true };
    let hidden = Attrs { writable: true, enumerable: false, configurable: true };
    let t = s.string("WebAssembly")?;
    s.define(ns, tag, t, attrs)?;
    let mut held = Vec::new();
    for (name, ctor, len) in [("Module", module_ctor as HS<E>, 1), ("Instance", instance_ctor as HS<E>, 1), ("Global", global_ctor as HS<E>, 1)] {
        let f = d.add_fn_s(s, ctor, name, len, HostFn::CtorNewTarget)?;
        let p = s.new_object()?;
        s.define_str(f, "prototype", p, Attrs { writable: false, enumerable: false, configurable: false })?;
        s.define_str(p, "constructor", f, hidden)?;
        let t = s.string(&format!("WebAssembly.{name}"))?;
        s.define(p, tag, t, attrs)?;
        s.define_str(ns, name, f, hidden)?;
        held.push((f, p));
    }
    let (mf, ip, gp) = (held[0].0, held[1].1, held[2].1);
    for (name, h) in [("exports", module_exports as HS<E>), ("imports", module_imports as HS<E>)] {
        d.def_method_l(s, mf, name, 1, h, true)?;
    }
    d.def_method_l(s, mf, "customSections", 2, module_sections, true)?;
    let g = d.add_fn_s(s, instance_exports, "get exports", 0, HostFn::Method)?;
    s.define_accessor_str(ip, "exports", Some(g), None, Attrs { writable: false, enumerable: true, configurable: true })?;
    let (get, set) = (d.add_fn_s(s, global_get, "get value", 0, HostFn::Method)?, d.add_fn_s(s, global_value, "set value", 1, HostFn::Method)?);
    s.define_accessor_str(gp, "value", Some(get), Some(set), Attrs { writable: false, enumerable: true, configurable: true })?;
    d.def_method_l(s, gp, "valueOf", 0, global_get, true)?;
    for (name, h, len) in [("validate", validate as HS<E>, 1), ("compile", compile as HS<E>, 1), ("instantiate", instantiate_fn as HS<E>, 1)] {
        d.def_method_l(s, ns, name, len, h, true)?;
    }
    let errors = s.run_script(ERRORS)?;
    let mut errs = Vec::new();
    for (i, name) in ["CompileError", "LinkError", "RuntimeError"].into_iter().enumerate() {
        let k = s.number(i as f64);
        let c = s.get(errors, k)?;
        s.define_str(ns, name, c, hidden)?;
        errs.push(c);
    }
    let g = s.global();
    let bigint = s.get_str(g, "BigInt")?;
    let as_int_n = s.get_str(bigint, "asIntN")?;
    let object = s.get_str(g, "Object")?;
    let freeze = s.get_str(object, "freeze")?;
    let call = d.add_fn_s(s, call_export, "", 0, HostFn::Method)?;
    let list = [held[0].1, ip, gp, errs[0], errs[1], errs[2], bigint, as_int_n, freeze, call, ns];
    let held: Vec<Persistent> = list.iter().map(|v| s.persist(*v)).collect();
    let engine = wasmi::Engine::default();
    *d.wasm.store.borrow_mut() = Some(Store::new(&engine, ()));
    *d.wasm.st.borrow_mut() = Some(Box::new(St {
        engine,
        held,
        imports: Vec::new(),
        externs: Vec::new(),
        funcs: Vec::new(),
        func_objs: HashMap::new(),
        of_obj: IdMap::default(),
    }));
    Ok(ns)
}

/// The global is an accessor until the first read. A call with an argument is the setter.
fn lazy<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let g = s.global();
    let attrs = Attrs { writable: true, enumerable: false, configurable: true };
    if let Some(v) = a.get(0) {
        s.define_str(g, "WebAssembly", v, attrs)?;
        return Ok(s.undefined());
    }
    let made = d.wasm.st.borrow().is_some();
    let ns = if made { held(d, s, NAMESPACE) } else { make(d, s)? };
    s.define_str(g, "WebAssembly", ns, attrs)?;
    Ok(ns)
}

pub(crate) fn install<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>) -> Res<'s, ()> {
    let g = s.global();
    let f = d.add_fn_s(s, lazy, "WebAssembly", 0, HostFn::Method)?;
    s.define_accessor_str(g, "WebAssembly", Some(f), Some(f), Attrs { writable: false, enumerable: false, configurable: true })
}

/// The references of the owner `Owner::Wasm(o)`.
pub(crate) fn trace<E: Eng>(d: &Dom<E>, o: E::Obj, t: &mut bare_js_api::Tracer<'_, E::Raw>) {
    if let Some(Item::Instance(x)) = d.wasm.items.borrow().get(&o) {
        t.visit(x);
    }
}

pub(crate) fn forget<E: Eng>(d: &Dom<E>, o: E::Obj) {
    d.wasm.items.borrow_mut().remove(&o);
}
