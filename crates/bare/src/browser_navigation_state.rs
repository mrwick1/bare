//! Structured clone: `structuredClone(v)` and the `history` state. `serialize` copies `v` into a native `State` (no JS object is
//! kept) and `materialize` builds a fresh object graph from it. `pushState(data)` keeps the `State` and `history.state` builds the
//! graph once per entry (cached, so `history.state === history.state`). `structuredClone` runs both at once. It is not JSON:
//! cycles, shared references, `undefined`, `NaN`, `-0`, sparse arrays and lone surrogates survive.
//!
//! Supported (HTML "StructuredSerializeInternal"): primitives including BigInt; Boolean, Number, String and BigInt wrapper
//! objects; `Date`; `RegExp` (`lastIndex` is 0 in the copy); `Map`; `Set`; arrays and ordinary objects (own enumerable string
//! keys, copied through `[[Get]]` so getters run, in key order, with no host borrow; the prototype is lost, so a class instance
//! becomes a plain object); `ArrayBuffer`, every typed array and `DataView` (a buffer shared by two views is copied once);
//! `Error` and its six subtypes (`name`, `message`, `stack` and `cause`).
//! Transfer list (HTML "StructuredSerializeWithTransfer"): `clone_state_with` takes the listed objects (`ArrayBuffer` or `MessagePort`,
//! checked by `Dom::transfer_list`). A listed item is not copied. Its node is `SKind::Moved(i)`, and `Transfer` number `i` holds it
//! in a list beside the `State`: the bytes of a buffer (the source is detached), the key of a parked port, or (for a message to another
//! page, after `Dom::export_ports`) the end ids of a port in the hub. `Transfer` has no JS object, so the pair can go to another heap.
//! `materialize_with` builds the buffers and ports from that list. A buffer
//! in the list and in the graph is one object in the result. The sources change only after the walk succeeded.
//! `DataCloneError`: a function, a symbol, a DOM wrapper, `URL`, `URLSearchParams`, an `AbortController` or `AbortSignal`, a Proxy, a
//! Promise, a WeakMap, a WeakSet, a WeakRef, an iterator, a generator, an `Intl` object, a Symbol wrapper, a detached buffer.
//! There is no internal Error slot in the engine: an object with an `Error.prototype` in its chain is an Error.
//! Bounds: `history` uses `NavLimits` (depth, nodes and bytes of one state, and the bytes of all states of a document). Over the bytes or nodes
//! cap, `pushState` throws a `DataCloneError`; over the depth cap it stops with a `Limit`.
//! `structuredClone` has its own: depth 256, 2^20 nodes, 64 MiB.
//!
//! The walk (`walk`, `build`) is generic over `bare_js_api::CloneOps`, so it runs on any engine (plan step S0.4, option E of
//! `docs/design-host-navigation.md`). It holds handles, never raw values: each step that makes locals runs in an inner scope (`scoped`),
//! so a walk over a large graph keeps a handle stack of the size of the depth. The public functions take the old engine and its values,
//! and enter a scope (`run`). What only the browser knows (the platform objects, the `DataCloneError`) goes through `CloneHost`.

use crate::browser::*;
use crate::browser_navigation::NavLimits;
use bare_js::eval::{CloneClass as OldClass, Error, Interp};
use bare_js::value::{ObjId, Value};
use bare_js_api::{Abort, Attrs, CloneClass, CloneKey, CloneName, CloneOps, Engine, Error as AErr, Local, Scope, Str, Type, ViewType};
use std::collections::HashMap;
use std::rc::Rc;

/// The result of a step of the walk. The error is an exception (a local of the scope) or an abort.
type AR<'s, T> = Result<T, AErr<'s>>;

pub(crate) enum SV {
    Undef,
    Null,
    Bool(bool),
    Num(f64),
    Str(Rc<[u16]>),
    /// `(negative, limbs)`.
    Big(Rc<(bool, Vec<u64>)>),
    Obj(u32),
}

pub(crate) enum SKey {
    Idx(u32),
    Str(Rc<[u16]>),
}

pub(crate) enum SKind {
    Plain,
    /// The length of an array.
    Array(u32),
    Date(f64),
    /// `(source, flags)`.
    RegExp(Rc<[u16]>, Rc<[u16]>),
    /// `(key, value)`; a Set has `Undef` values.
    Map(Vec<(SV, SV)>),
    Set(Vec<(SV, SV)>),
    /// `(name, message, stack, cause)`.
    Error(Rc<[u16]>, Option<SV>, Option<SV>, Option<SV>),
    Wrapper(SV),
    Buffer(Vec<u8>),
    /// `(buffer node, byte offset, length in elements, type)`.
    View(u32, usize, usize, ViewType),
    /// A transferred buffer or port: the number of its item in the `Transfer` list.
    Moved(u32),
}

/// An item of a transfer list, as data. It holds no JS object.
pub(crate) enum Transfer {
    /// The bytes of a detached `ArrayBuffer`.
    Buffer(Vec<u8>),
    /// A port that `Dom::park_port` took out of the port table. It stays inside one page.
    Port(u64),
    /// A port that moves to another page (`browser_port_hub.rs`): its end in the hub and the end of its peer, if the peer is in the hub.
    /// `Dom::export_ports` makes it from a `Port`. `Dom::import_ends` makes the new port.
    End { me: u64, remote: Option<u64> },
}

pub(crate) struct SNode {
    pub kind: SKind,
    pub props: Vec<(SKey, SV)>,
}

pub(crate) struct State {
    pub root: SV,
    pub nodes: Vec<SNode>,
    pub bytes: usize,
}

/// Bounds of one walk and the names of their limit errors.
#[derive(Clone, Copy)]
struct Bounds {
    bytes: usize,
    depth: usize,
    nodes: usize,
    names: [&'static str; 3],
}

struct Ser {
    /// The identity number of an object ([`CloneOps::object_id`]) and its node.
    seen: HashMap<u64, u32>,
    /// The listed items and their numbers. Empty (no allocation) when there is no transfer list.
    xfer: HashMap<u64, u32>,
    nodes: Vec<SNode>,
    bytes: usize,
    b: Bounds,
}

/// What the walk asks of the browser: the objects that cannot be cloned and the error to throw.
pub(crate) trait CloneHost<E: CloneOps> {
    /// Is the object a platform object (a DOM wrapper, `URL`, a port, ...)? It cannot be cloned.
    fn uncloneable(&self, v: &E::Raw) -> bool;
    /// A `DataCloneError` that says `what` could not be cloned.
    fn clone_error<'s>(&self, s: &mut Scope<'s, E>, what: &str) -> AErr<'s>;
}

impl<'p> CloneHost<Interp<'p>> for Dom {
    fn uncloneable(&self, v: &Value) -> bool {
        matches!(v, Value::Object(o) if self.is_host_object(*o))
    }

    fn clone_error<'s>(&self, s: &mut Scope<'s, Interp<'p>>, what: &str) -> AErr<'s> {
        let it = s.engine();
        let e = self.dom_err(it, "DataCloneError", &format!("{what} could not be cloned."));
        it.api_err(e)
    }
}

/// A limit of the host, by name: `Error::Limit(LimitKind::Host(name))` for the old engine.
fn lim<'s>(name: &'static str) -> AErr<'s> {
    AErr::Abort(Abort::Limit(name))
}

fn add<'s>(c: &mut Ser, n: usize) -> AR<'s, ()> {
    c.bytes = c.bytes.saturating_add(n);
    if c.bytes > c.b.bytes {
        return Err(lim(c.b.names[0]));
    }
    Ok(())
}

/// Run `f` in an inner scope: the locals that it makes end with it. An exception crosses the end of the scope as a raw value.
fn scoped<'s, E: CloneOps, T>(s: &mut Scope<'s, E>, f: impl for<'i> FnOnce(&mut Scope<'i, E>) -> AR<'i, T>) -> AR<'s, T> {
    let r = s.scope(|i| {
        f(i).map_err(|e| match e {
            AErr::Throw(l) => Ok(i.engine().raw(l)),
            AErr::Abort(a) => Err(a),
        })
    });
    match r {
        Ok(t) => Ok(t),
        Err(Ok(raw)) => Err(AErr::Throw(s.engine().raw_local(&raw))),
        Err(Err(a)) => Err(AErr::Abort(a)),
    }
}

/// Enter a scope of the old engine and give the error of the interface back as an error of the old engine.
fn run<'p, T>(it: &mut Interp<'p>, f: impl for<'s> FnOnce(&mut Scope<'s, Interp<'p>>) -> AR<'s, T>) -> R<T> {
    bare_js_api::enter(it, |s| {
        let r = f(s);
        r.map_err(|e| s.engine().api_back(e))
    })
}

/// The state of `pushState`.
pub(crate) fn serialize(d: &Dom, it: &mut Interp<'_>, v: Value) -> R<State> {
    let l: NavLimits = d.nav.lim.get();
    let b = Bounds { bytes: l.max_state_bytes, depth: l.max_state_depth.min(128), nodes: l.max_state_nodes, names: ["history-state-bytes", "history-state-depth", "history-state-nodes"] };
    run(it, |s| {
        let v = s.engine().raw_local(&v);
        match walk(d, s, v, b, &[]) {
            // The size is the page's choice: a catchable error, as in Firefox. The depth cap stays a limit.
            Err(AErr::Abort(Abort::Limit("history-state-bytes" | "history-state-nodes"))) => Err(d.clone_error(s, "A state over the size cap")),
            r => r,
        }
    })
}

/// `structuredClone(v, { transfer })` and a message: the copy, the new ports of the transfer list (in list order) and the byte cost.
/// `list` comes from `Dom::transfer_list`.
pub(crate) fn clone_with(d: &Dom, it: &mut Interp<'_>, v: Value, list: &[ObjId]) -> R<(Value, Vec<ObjId>, usize)> {
    let (st, xf) = clone_state_with(d, it, v, list)?;
    let (val, ports) = materialize_with(d, it, &st, xf)?;
    Ok((val, ports, st.bytes))
}

/// A snapshot with structured-clone limits, and the transfer list as data. Resource limits remain uncatchable. The listed buffers
/// are detached and the listed ports are parked after the walk succeeded. A listed item that changed during the walk (a getter
/// detached it) throws `DataCloneError` before any item moves.
pub(crate) fn clone_state_with(d: &Dom, it: &mut Interp<'_>, v: Value, list: &[ObjId]) -> R<(State, Vec<Transfer>)> {
    let b = Bounds { bytes: 64 << 20, depth: 256, nodes: 1 << 20, names: ["clone-bytes", "clone-depth", "clone-nodes"] };
    let st = run(it, |s| {
        let v = s.engine().raw_local(&v);
        let l: Vec<Local<'_>> = list.iter().map(|&o| s.engine().raw_local(&Value::Object(o))).collect();
        walk(d, s, v, b, &l)
    })?;
    let is_buf = |it: &Interp<'_>, o: ObjId| matches!(it.clone_class(o), OldClass::Buffer);
    for &o in list {
        let ok = if is_buf(it, o) { !it.heap.buffer_detached(o) } else { d.port_movable(o) };
        if !ok {
            return Err(clone_err(d, it, "A transferred item is detached and"));
        }
    }
    let mut xf = Vec::with_capacity(list.len());
    for &o in list {
        xf.push(if is_buf(it, o) {
            let bytes = it.heap.bytes(o).expect("a buffer").to_vec();
            it.heap.buffer_transfer(None, o, 0).map_err(|e| it.heap_error(e))?;
            Transfer::Buffer(bytes)
        } else {
            Transfer::Port(d.park_port(o))
        });
    }
    Ok((st, xf))
}

fn clone_err(d: &Dom, it: &mut Interp<'_>, what: &str) -> Error {
    d.dom_err(it, "DataCloneError", &format!("{what} could not be cloned."))
}

fn walk<'s, E: CloneOps, H: CloneHost<E>>(d: &H, s: &mut Scope<'s, E>, v: Local<'_>, b: Bounds, list: &[Local<'_>]) -> AR<'s, State> {
    let xfer = list.iter().enumerate().filter_map(|(i, &l)| s.engine().object_id(l).map(|id| (id, i as u32))).collect();
    let mut c = Ser { seen: HashMap::new(), xfer, nodes: Vec::new(), bytes: 0, b };
    let root = ser(d, s, &mut c, v, 0)?;
    // A listed item that the graph does not use still costs its bytes.
    for &l in list {
        if s.engine().object_id(l).is_some_and(|id| c.seen.contains_key(&id)) {
            continue;
        }
        let n = s.engine().buffer_len(l).unwrap_or(0);
        add(&mut c, 64 + n)?;
        s.charge((n / 64) as u64)?;
    }
    Ok(State { root, nodes: c.nodes, bytes: c.bytes })
}

/// The UTF-16 code units of the string `v`.
fn copy_units<E: CloneOps>(s: &Scope<'_, E>, v: Local<'_>) -> Rc<[u16]> {
    match s.as_str(v) {
        Some(Str::Utf16(u)) => Rc::from(u),
        Some(Str::Latin1(b)) => b.iter().map(|&x| x as u16).collect(),
        None => Rc::from([]),
    }
}

fn units<'s, E: CloneOps>(s: &mut Scope<'s, E>, c: &mut Ser, v: Local<'_>) -> AR<'s, Rc<[u16]>> {
    let len = s.as_str(v).map_or(0, |t| t.len());
    add(c, len.saturating_mul(2) + 16)?;
    s.charge((len / 32) as u64)?;
    Ok(copy_units(s, v))
}

fn ser<'s, E: CloneOps, H: CloneHost<E>>(d: &H, s: &mut Scope<'s, E>, c: &mut Ser, v: Local<'_>, depth: usize) -> AR<'s, SV> {
    s.charge(1)?;
    Ok(match s.type_of(v) {
        Type::Undefined => SV::Undef,
        Type::Null => SV::Null,
        Type::Boolean => SV::Bool(s.as_bool(v).unwrap_or(false)),
        Type::Number => SV::Num(s.as_number(v).unwrap_or(0.0)),
        Type::Symbol => return Err(d.clone_error(s, "Symbol")),
        Type::BigInt => {
            let p = s.engine().bigint_parts(v).expect("a BigInt");
            add(c, p.1.len() * 8 + 16)?;
            SV::Big(Rc::new(p))
        }
        Type::String => SV::Str(units(s, c, v)?),
        Type::Function => return Err(d.clone_error(s, "A function")),
        Type::Object => object(d, s, c, v, depth)?,
    })
}

fn object<'s, E: CloneOps, H: CloneHost<E>>(d: &H, s: &mut Scope<'s, E>, c: &mut Ser, v: Local<'_>, depth: usize) -> AR<'s, SV> {
    let id = s.engine().object_id(v).expect("an object");
    if let Some(&i) = c.seen.get(&id) {
        return Ok(SV::Obj(i));
    }
    if !c.xfer.is_empty()
        && let Some(&i) = c.xfer.get(&id)
    {
        if c.nodes.len() >= c.b.nodes {
            return Err(lim(c.b.names[2]));
        }
        let n = s.engine().buffer_len(v).unwrap_or(0);
        add(c, 64 + n)?;
        s.charge((n / 64) as u64)?;
        let idx = c.nodes.len() as u32;
        c.seen.insert(id, idx);
        c.nodes.push(SNode { kind: SKind::Moved(i), props: Vec::new() });
        return Ok(SV::Obj(idx));
    }
    let raw = s.engine().raw(v);
    if d.uncloneable(&raw) {
        return Err(d.clone_error(s, "An object"));
    }
    let class = s.engine().clone_class(v);
    if matches!(class, CloneClass::Other) {
        return Err(d.clone_error(s, "An object"));
    }
    if depth >= c.b.depth {
        return Err(lim(c.b.names[1]));
    }
    if c.nodes.len() >= c.b.nodes {
        return Err(lim(c.b.names[2]));
    }
    add(c, 48)?;
    let idx = c.nodes.len() as u32;
    c.seen.insert(id, idx);
    c.nodes.push(SNode { kind: SKind::Plain, props: Vec::new() });
    let kind = match class {
        CloneClass::Plain => {
            props(d, s, c, v, idx, depth)?;
            SKind::Plain
        }
        CloneClass::Array(len) => {
            props(d, s, c, v, idx, depth)?;
            SKind::Array(len)
        }
        CloneClass::Date(t) => SKind::Date(t),
        CloneClass::RegExp(src, flags) => SKind::RegExp(units(s, c, src)?, units(s, c, flags)?),
        CloneClass::Map | CloneClass::Set => {
            let is_map = matches!(class, CloneClass::Map);
            let mut out = Vec::new();
            for (k, val) in s.engine().clone_coll_entries(v) {
                out.push(scoped(s, |i| {
                    i.charge(1)?;
                    add(c, 16)?;
                    let k = ser(d, i, c, k, depth + 1)?;
                    let val = if is_map { ser(d, i, c, val, depth + 1)? } else { SV::Undef };
                    Ok((k, val))
                })?);
            }
            if is_map { SKind::Map(out) } else { SKind::Set(out) }
        }
        CloneClass::Error => {
            let name = scoped(s, |i| {
                let n = i.get_str(v, "name")?;
                Ok(if i.type_of(n) == Type::String { units(i, c, n)? } else { Rc::from([]) })
            })?;
            let message = error_field(d, s, c, v, "message", true, false, depth)?;
            let stack = error_field(d, s, c, v, "stack", false, true, depth)?;
            let cause = error_field(d, s, c, v, "cause", false, false, depth)?;
            SKind::Error(name, message, stack, cause)
        }
        CloneClass::Wrapper(p) => SKind::Wrapper(ser(d, s, c, p, depth + 1)?),
        CloneClass::Buffer => {
            if s.engine().buffer_detached(v) {
                return Err(d.clone_error(s, "An ArrayBuffer is detached and"));
            }
            let n = s.engine().buffer_len(v).expect("a buffer");
            add(c, n)?;
            s.charge((n / 64) as u64)?;
            SKind::Buffer(s.engine().buffer_bytes(v).expect("a buffer").to_vec())
        }
        CloneClass::View => {
            let (buf, off, len) = s.engine().view_parts(v).expect("a view");
            if s.engine().view_detached(v) {
                return Err(d.clone_error(s, "An ArrayBuffer is detached and"));
            }
            let ty = s.engine().view_type(v).expect("a view");
            let SV::Obj(bi) = ser(d, s, c, buf, depth + 1)? else { unreachable!() };
            SKind::View(bi, off, len, ty)
        }
        CloneClass::Other => unreachable!(),
    };
    c.nodes[idx as usize].kind = kind;
    Ok(SV::Obj(idx))
}

/// An own data property of an error (`message`, `stack` or `cause`), or `None`. An accessor is not read. `only_str`: a value that is
/// not a string counts as absent. `to_str`: the value goes through `ToString` first.
#[allow(clippy::too_many_arguments)]
fn error_field<'s, E: CloneOps, H: CloneHost<E>>(d: &H, s: &mut Scope<'s, E>, c: &mut Ser, o: Local<'_>, name: &str, to_str: bool, only_str: bool, depth: usize) -> AR<'s, Option<SV>> {
    scoped(s, |i| {
        let key = i.string(name)?;
        let Some(value) = i.engine().own_data(o, key)? else { return Ok(None) };
        if only_str && i.type_of(value) != Type::String {
            return Ok(None);
        }
        let value = if to_str { i.to_string(value)? } else { value };
        Ok(Some(ser(d, i, c, value, depth + 1)?))
    })
}

/// The own enumerable keys of a plain object or array.
fn props<'s, E: CloneOps, H: CloneHost<E>>(d: &H, s: &mut Scope<'s, E>, c: &mut Ser, o: Local<'_>, idx: u32, depth: usize) -> AR<'s, ()> {
    let keys = s.engine().clone_keys(o)?;
    s.charge(keys.len() as u64)?;
    for key in keys {
        scoped(s, |i| {
            let sk = match key {
                CloneKey::Index(n) => SKey::Idx(n),
                CloneKey::Name(l) => SKey::Str(copy_units(i, l)),
            };
            // Own and enumerable now, as the spec checks just before each `[[Get]]`.
            let Some(val) = i.engine().clone_get(o, key)? else { return Ok(()) };
            add(c, 16 + if let SKey::Str(u) = &sk { u.len() * 2 } else { 4 })?;
            let sv = ser(d, i, c, val, depth + 1)?;
            c.nodes[idx as usize].props.push((sk, sv));
            Ok(())
        })?;
    }
    Ok(())
}

/// The value of `v`: a primitive as a new local, an object as its local in `objs`.
fn sv_value<'x, 'a: 'x, 'b: 'x, E: CloneOps>(s: &mut Scope<'a, E>, objs: &[Local<'b>], v: &SV) -> AR<'a, Local<'x>> {
    Ok(match v {
        SV::Undef => s.undefined(),
        SV::Null => s.null(),
        SV::Bool(b) => s.boolean(*b),
        SV::Num(n) => s.number(*n),
        SV::Obj(i) => objs[*i as usize],
        SV::Str(u) => s.string_utf16(u)?,
        SV::Big(b) => s.engine().bigint_from_parts(b.0, b.1.clone())?,
    })
}

/// A new object graph for `st`. No JS runs (the objects are new, so no setter can be hit).
pub(crate) fn materialize(d: &Dom, it: &mut Interp<'_>, st: &State) -> R<Value> {
    materialize_with(d, it, st, Vec::new()).map(|r| r.0)
}

/// `materialize` with a transfer list: the graph and the new ports of the list, in list order. A listed buffer that no node uses is dropped.
pub(crate) fn materialize_with(d: &Dom, it: &mut Interp<'_>, st: &State, mut xf: Vec<Transfer>) -> R<(Value, Vec<ObjId>)> {
    let keys: Vec<u64> = xf.iter().filter_map(|t| if let Transfer::Port(k) = t { Some(*k) } else { None }).collect();
    let mut ports = d.make_ports(it, &keys)?;
    let mut by_item: Vec<Option<ObjId>> = Vec::new();
    if !xf.is_empty() {
        let ends: Vec<(u64, Option<u64>)> = xf.iter().filter_map(|t| if let Transfer::End { me, remote } = t { Some((*me, *remote)) } else { None }).collect();
        let moved = if ends.is_empty() { Vec::new() } else { d.import_ends(it, &ends)? };
        let (mut next, mut next_end) = (ports.iter(), moved.iter());
        by_item = xf.iter().map(|t| match t {
            Transfer::Port(_) => next.next().copied(),
            Transfer::End { .. } => next_end.next().copied(),
            Transfer::Buffer(_) => None,
        }).collect();
        if !moved.is_empty() {
            // The ports of the result, in list order.
            ports = by_item.iter().flatten().copied().collect();
        }
    }
    let root = run(it, |s| {
        let by_item: Vec<Option<Local<'_>>> = by_item.iter().map(|o| o.map(|o| s.engine().raw_local(&Value::Object(o)))).collect();
        let r = build(s, st, &mut xf, &by_item)?;
        Ok(s.engine().raw(r))
    })?;
    Ok((root, ports))
}

/// The objects of `st`, then their properties. `by_item` holds the port of each transfer item.
fn build<'s, E: CloneOps>(s: &mut Scope<'s, E>, st: &State, xf: &mut [Transfer], by_item: &[Option<Local<'s>>]) -> AR<'s, Local<'s>> {
    let placeholder = s.undefined();
    let mut objs: Vec<Local<'s>> = vec![placeholder; st.nodes.len()];
    // Views come second: the buffer of a view can be a later node.
    for views in [false, true] {
        for (i, n) in st.nodes.iter().enumerate() {
            if matches!(n.kind, SKind::View(..)) != views {
                continue;
            }
            s.charge(1)?;
            objs[i] = match &n.kind {
                SKind::Moved(x) if !matches!(xf[*x as usize], Transfer::Buffer(_)) => by_item[*x as usize].expect("made"),
                kind => s.escape_scope(|i| make(i, &objs, kind, xf))?,
            };
        }
    }
    let k_len = s.string("length")?;
    for (n, &o) in st.nodes.iter().zip(&objs) {
        for (k, v) in &n.props {
            scoped(s, |i| {
                i.charge(1)?;
                let name = match k {
                    SKey::Idx(x) => CloneName::Index(*x),
                    SKey::Str(u) => CloneName::Units(u),
                };
                let val = sv_value(i, &objs, v)?;
                i.engine().clone_define(o, name, val)
            })?;
        }
        match &n.kind {
            SKind::Array(len) => scoped(s, |i| {
                let l = i.number(*len as f64);
                i.set(o, k_len, l)
            })?,
            SKind::Map(es) | SKind::Set(es) => {
                let is_map = matches!(n.kind, SKind::Map(_));
                for (k, v) in es {
                    scoped(s, |i| {
                        i.charge(1)?;
                        let k = sv_value(i, &objs, k)?;
                        let v = if is_map { sv_value(i, &objs, v)? } else { k };
                        i.engine().clone_coll_put(o, k, v)
                    })?;
                }
            }
            SKind::Error(_, m, st, cause) => {
                for (name, v) in [("message", m), ("stack", st), ("cause", cause)] {
                    if let Some(v) = v {
                        scoped(s, |i| {
                            let key = i.string(name)?;
                            let val = sv_value(i, &objs, v)?;
                            i.define(o, key, val, Attrs::METHOD)
                        })?;
                    }
                }
            }
            _ => {}
        }
    }
    sv_value(s, &objs, &st.root)
}

/// A new object for a node. A node that is a moved port is not here (its object exists).
fn make<'i, 'b, E: CloneOps>(i: &mut Scope<'i, E>, objs: &[Local<'b>], kind: &SKind, xf: &mut [Transfer]) -> AR<'i, Local<'i>> {
    match kind {
        SKind::Plain => i.new_object(),
        SKind::Array(_) => i.new_array(&[]),
        SKind::Date(t) => i.engine().clone_new_date(*t),
        SKind::RegExp(src, flags) => {
            let (src, flags) = (i.string_utf16(src)?, i.string_utf16(flags)?);
            i.engine().clone_new_regexp(src, flags)
        }
        SKind::Map(_) => i.engine().clone_new_coll(true),
        SKind::Set(_) => i.engine().clone_new_coll(false),
        SKind::Error(name, ..) => i.engine().clone_new_error(name),
        SKind::Wrapper(p) => {
            let p = sv_value(i, objs, p)?;
            i.engine().clone_new_wrapper(p)
        }
        SKind::Buffer(b) => i.engine().new_array_buffer(b.clone()),
        SKind::View(b, off, len, ty) => i.engine().clone_new_view(objs[*b as usize], *off, *len, *ty),
        SKind::Moved(x) => match &mut xf[*x as usize] {
            Transfer::Buffer(b) => i.engine().new_array_buffer(std::mem::take(b)),
            Transfer::Port(_) | Transfer::End { .. } => unreachable!("a port is not made here"),
        },
    }
}
