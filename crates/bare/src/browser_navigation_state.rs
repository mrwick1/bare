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

use crate::browser::*;
use crate::browser_navigation::NavLimits;
use bare_js::eval::{CloneClass, Error, Interp, LimitKind};
use bare_js::heap::{Elem, Property};
use bare_js::value::{ObjId, PropertyKey, Value};
use std::collections::HashMap;
use std::rc::Rc;

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
    View(u32, usize, usize, Elem),
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
    /// Spike: the native record of a graph of the data classes. `root` and `nodes` are empty then.
    pub rec: Option<Box<[u8]>>,
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
    seen: HashMap<ObjId, u32>,
    /// The listed items and their numbers. Empty (no allocation) when there is no transfer list.
    xfer: HashMap<ObjId, u32>,
    nodes: Vec<SNode>,
    bytes: usize,
    b: Bounds,
}

fn add(c: &mut Ser, d: &Dom, n: usize) -> R<()> {
    c.bytes = c.bytes.saturating_add(n);
    if c.bytes > c.b.bytes {
        return Err(d.lim(c.b.names[0]));
    }
    Ok(())
}

fn clone_err(d: &Dom, it: &mut Interp<'_>, what: &str) -> Error {
    d.dom_err(it, "DataCloneError", &format!("{what} could not be cloned."))
}

/// The state of `pushState`.
pub(crate) fn serialize(d: &Dom, it: &mut Interp<'_>, v: Value) -> R<State> {
    let l: NavLimits = d.nav.lim.get();
    let b = Bounds { bytes: l.max_state_bytes, depth: l.max_state_depth.min(128), nodes: l.max_state_nodes, names: ["history-state-bytes", "history-state-depth", "history-state-nodes"] };
    if let Some(rec) = it.clone_out_fast(v, b.bytes, b.depth, b.nodes, &|o| d.is_host_object(o))? {
        return Ok(State { root: SV::Undef, nodes: Vec::new(), bytes: rec.cost, rec: Some(rec.bytes) });
    }
    match walk(d, it, v, b, &[]) {
        // The size is the page's choice: a catchable error, as in Firefox. The depth cap stays a limit.
        Err(Error::Limit(LimitKind::Host("history-state-bytes" | "history-state-nodes"))) => Err(clone_err(d, it, "A state over the size cap")),
        r => r,
    }
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
    if list.is_empty()
        && let Some(rec) = it.clone_out_fast(v, 64 << 20, 256, 1 << 20, &|o| d.is_host_object(o))?
    {
        return Ok((State { root: SV::Undef, nodes: Vec::new(), bytes: rec.cost, rec: Some(rec.bytes) }, Vec::new()));
    }
    let st = walk(d, it, v, Bounds { bytes: 64 << 20, depth: 256, nodes: 1 << 20, names: ["clone-bytes", "clone-depth", "clone-nodes"] }, list)?;
    let is_buf = |it: &Interp<'_>, o: ObjId| matches!(it.clone_class(o), CloneClass::Buffer);
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

fn walk(d: &Dom, it: &mut Interp<'_>, v: Value, b: Bounds, list: &[ObjId]) -> R<State> {
    let xfer = list.iter().enumerate().map(|(i, &o)| (o, i as u32)).collect();
    let mut c = Ser { seen: HashMap::new(), xfer, nodes: Vec::new(), bytes: 0, b };
    let root = ser(d, it, &mut c, v, 0)?;
    // A listed item that the graph does not use still costs its bytes.
    for o in list {
        if c.seen.contains_key(o) {
            continue;
        }
        let n = it.heap.buffer_len(*o).unwrap_or(0);
        add(&mut c, d, 64 + n)?;
        it.charge((n / 64) as u64)?;
    }
    Ok(State { root, nodes: c.nodes, bytes: c.bytes, rec: None })
}

fn units(d: &Dom, it: &mut Interp<'_>, c: &mut Ser, s: bare_js::value::StrId) -> R<Rc<[u16]>> {
    let len = it.heap.str_len(s).map_err(Error::Heap)?;
    add(c, d, len.saturating_mul(2) + 16)?;
    it.charge((len / 32) as u64)?;
    match it.heap.str_units(s) {
        Ok(u) => Ok(Rc::from(u)),
        Err(e) => Err(it.heap_error(e)),
    }
}

fn ser(d: &Dom, it: &mut Interp<'_>, c: &mut Ser, v: Value, depth: usize) -> R<SV> {
    it.charge(1)?;
    Ok(match v {
        Value::Undefined => SV::Undef,
        Value::Null => SV::Null,
        Value::Bool(b) => SV::Bool(b),
        Value::Number(n) => SV::Num(n),
        Value::Symbol(_) => return Err(clone_err(d, it, "Symbol")),
        Value::BigInt(_) | Value::BigHeap(_) => {
            let p = it.heap.bigint_parts(v).expect("a BigInt");
            add(c, d, p.1.len() * 8 + 16)?;
            SV::Big(Rc::new(p))
        }
        Value::String(s) => SV::Str(units(d, it, c, s)?),
        Value::Object(o) => {
            if let Some(&i) = c.seen.get(&o) {
                return Ok(SV::Obj(i));
            }
            if !c.xfer.is_empty()
                && let Some(&i) = c.xfer.get(&o)
            {
                if c.nodes.len() >= c.b.nodes {
                    return Err(d.lim(c.b.names[2]));
                }
                let n = it.heap.buffer_len(o).unwrap_or(0);
                add(c, d, 64 + n)?;
                it.charge((n / 64) as u64)?;
                let idx = c.nodes.len() as u32;
                c.seen.insert(o, idx);
                c.nodes.push(SNode { kind: SKind::Moved(i), props: Vec::new() });
                return Ok(SV::Obj(idx));
            }
            if it.is_callable_value(v) {
                return Err(clone_err(d, it, "A function"));
            }
            if d.is_host_object(o) {
                return Err(clone_err(d, it, "An object"));
            }
            let class = it.clone_class(o);
            if matches!(class, CloneClass::Other) {
                return Err(clone_err(d, it, "An object"));
            }
            if depth >= c.b.depth {
                return Err(d.lim(c.b.names[1]));
            }
            if c.nodes.len() >= c.b.nodes {
                return Err(d.lim(c.b.names[2]));
            }
            add(c, d, 48)?;
            let idx = c.nodes.len() as u32;
            c.seen.insert(o, idx);
            c.nodes.push(SNode { kind: SKind::Plain, props: Vec::new() });
            let kind = match class {
                CloneClass::Plain => {
                    if it.heap.is_array(o).map_err(Error::Heap)? {
                        let k = d.key(it, "length")?;
                        let len = match it.heap.get_own_property(o, k) {
                            Ok(Some(Property::Data { value: Value::Number(n), .. })) => n as u32,
                            _ => 0,
                        };
                        props(d, it, c, o, idx, depth)?;
                        SKind::Array(len)
                    } else {
                        props(d, it, c, o, idx, depth)?;
                        SKind::Plain
                    }
                }
                CloneClass::Date(t) => SKind::Date(t),
                CloneClass::RegExp(s, f) => SKind::RegExp(units(d, it, c, s)?, units(d, it, c, f)?),
                CloneClass::Map | CloneClass::Set => {
                    let is_map = matches!(class, CloneClass::Map);
                    let mut out = Vec::new();
                    for (k, val) in it.clone_coll_entries(o) {
                        it.charge(1)?;
                        add(c, d, 16)?;
                        let k = ser(d, it, c, k, depth + 1)?;
                        let val = if is_map { ser(d, it, c, val, depth + 1)? } else { SV::Undef };
                        out.push((k, val));
                    }
                    if is_map { SKind::Map(out) } else { SKind::Set(out) }
                }
                CloneClass::Error => {
                    let kn = d.key(it, "name")?;
                    let name = match it.get_value(v, kn)? {
                        Value::String(s) => units(d, it, c, s)?,
                        _ => Rc::from([]),
                    };
                    let field = |it: &mut Interp<'_>, c: &mut Ser, k: &str, to_str: bool, only_str: bool| -> R<Option<SV>> {
                        let key = d.key(it, k)?;
                        let Some(Property::Data { value, .. }) = it.heap.get_own_property(o, key).map_err(Error::Heap)? else { return Ok(None) };
                        if only_str && !matches!(value, Value::String(_)) {
                            return Ok(None);
                        }
                        let value = if to_str { Value::String(it.to_js_string(value)?) } else { value };
                        Ok(Some(ser(d, it, c, value, depth + 1)?))
                    };
                    let message = field(it, c, "message", true, false)?;
                    let stack = field(it, c, "stack", false, true)?;
                    let cause = field(it, c, "cause", false, false)?;
                    SKind::Error(name, message, stack, cause)
                }
                CloneClass::Wrapper(p) => SKind::Wrapper(ser(d, it, c, p, depth + 1)?),
                CloneClass::Buffer => {
                    if it.heap.buffer_detached(o) {
                        return Err(clone_err(d, it, "An ArrayBuffer is detached and"));
                    }
                    let n = it.heap.buffer_len(o).expect("a buffer");
                    add(c, d, n)?;
                    it.charge((n / 64) as u64)?;
                    SKind::Buffer(it.heap.bytes(o).expect("a buffer").to_vec())
                }
                CloneClass::View => {
                    let (buf, off, len) = it.heap.view_parts(o).expect("a view");
                    if it.heap.view_detached(o) {
                        return Err(clone_err(d, it, "An ArrayBuffer is detached and"));
                    }
                    let ty = it.heap.view_type(o).expect("a view");
                    let SV::Obj(bi) = ser(d, it, c, Value::Object(buf), depth + 1)? else { unreachable!() };
                    SKind::View(bi, off, len, ty)
                }
                CloneClass::Other => unreachable!(),
            };
            c.nodes[idx as usize].kind = kind;
            SV::Obj(idx)
        }
    })
}

/// The own enumerable keys of a plain object or array.
fn props(d: &Dom, it: &mut Interp<'_>, c: &mut Ser, o: ObjId, idx: u32, depth: usize) -> R<()> {
    let keys = match it.heap.own_keys(o) {
        Ok(k) => k,
        Err(e) => return Err(it.heap_error(e)),
    };
    for key in keys {
        it.charge(1)?;
        let sk = match key {
            PropertyKey::Symbol(_) => continue,
            PropertyKey::Index(i) => SKey::Idx(i),
            PropertyKey::String(s) => match it.heap.str_units(s) {
                Ok(u) => SKey::Str(Rc::from(u)),
                Err(e) => return Err(it.heap_error(e)),
            },
        };
        // Own and enumerable now, as the spec checks just before each `[[Get]]`.
        match it.heap.get_own_property(o, key) {
            Ok(Some(p)) if p.enumerable() => {}
            Ok(_) => continue,
            Err(e) => return Err(it.heap_error(e)),
        }
        add(c, d, 16 + if let SKey::Str(u) = &sk { u.len() * 2 } else { 4 })?;
        let val = it.get_value(Value::Object(o), key)?;
        let sv = ser(d, it, c, val, depth + 1)?;
        c.nodes[idx as usize].props.push((sk, sv));
    }
    Ok(())
}

fn sv_value(it: &mut Interp<'_>, objs: &[Option<ObjId>], v: &SV) -> R<Value> {
    Ok(match v {
        SV::Undef => Value::Undefined,
        SV::Null => Value::Null,
        SV::Bool(b) => Value::Bool(*b),
        SV::Num(n) => Value::Number(*n),
        SV::Obj(i) => Value::Object(objs[*i as usize].expect("made before use")),
        SV::Str(u) => match it.heap.intern_units(u) {
            Ok(s) => Value::String(s),
            Err(e) => return Err(it.heap_error(e)),
        },
        SV::Big(b) => match it.heap.bigint_from_parts(b.0, b.1.clone()) {
            Ok(v) => v,
            Err(e) => return Err(it.heap_error(e)),
        },
    })
}

/// A new object graph for `st`. No JS runs (the objects are new, so no setter can be hit).
pub(crate) fn materialize(d: &Dom, it: &mut Interp<'_>, st: &State) -> R<Value> {
    materialize_with(d, it, st, Vec::new()).map(|r| r.0)
}

/// `materialize` with a transfer list: the graph and the new ports of the list, in list order. A listed buffer that no node uses is dropped.
pub(crate) fn materialize_with(d: &Dom, it: &mut Interp<'_>, st: &State, mut xf: Vec<Transfer>) -> R<(Value, Vec<ObjId>)> {
    if let Some(rec) = &st.rec {
        return Ok((it.clone_in(rec)?, Vec::new()));
    }
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
    let mut objs: Vec<Option<ObjId>> = vec![None; st.nodes.len()];
    // Views come second: the buffer of a view can be a later node.
    for views in [false, true] {
        for (i, n) in st.nodes.iter().enumerate() {
            if matches!(n.kind, SKind::View(..)) != views {
                continue;
            }
            it.charge(1)?;
            let o = match &n.kind {
                SKind::Plain => it.heap.new_object(Some(it.object_prototype())).map_err(|e| it.heap_error(e))?,
                SKind::Array(_) => it.heap.new_array(Some(it.array_prototype())).map_err(|e| it.heap_error(e))?,
                SKind::Date(t) => it.clone_new_date(*t)?,
                SKind::RegExp(s, f) => {
                    let (s, f) = (it.heap.intern_units(s).map_err(|e| it.heap_error(e))?, it.heap.intern_units(f).map_err(|e| it.heap_error(e))?);
                    it.clone_new_regexp(s, f)?
                }
                SKind::Map(_) => it.clone_new_coll(true)?,
                SKind::Set(_) => it.clone_new_coll(false)?,
                SKind::Error(name, ..) => it.clone_new_error(name)?,
                SKind::Wrapper(p) => {
                    let p = sv_value(it, &objs, p)?;
                    it.clone_new_wrapper(p)?
                }
                SKind::Buffer(b) => match it.new_array_buffer(b.clone())? {
                    Value::Object(o) => o,
                    _ => unreachable!(),
                },
                SKind::View(b, off, len, ty) => it.clone_new_view(objs[*b as usize].expect("buffers come first"), *off, *len, *ty)?,
                SKind::Moved(i) => match &mut xf[*i as usize] {
                    Transfer::Buffer(b) => match it.new_array_buffer(std::mem::take(b))? {
                        Value::Object(o) => o,
                        _ => unreachable!(),
                    },
                    Transfer::Port(_) | Transfer::End { .. } => by_item[*i as usize].expect("made"),
                },
            };
            objs[i] = Some(o);
        }
    }
    let k_len = d.key(it, "length")?;
    for (n, o) in st.nodes.iter().zip(objs.iter().map(|o| o.expect("made"))) {
        for (k, v) in &n.props {
            it.charge(1)?;
            let key = match k {
                SKey::Idx(i) => PropertyKey::Index(*i),
                SKey::Str(u) => match it.heap.intern_units(u).and_then(|s| it.heap.key_from_string(s)) {
                    Ok(k) => k,
                    Err(e) => return Err(it.heap_error(e)),
                },
            };
            let val = sv_value(it, &objs, v)?;
            it.define_data(o, key, val, true, true, true)?;
        }
        match &n.kind {
            SKind::Array(len) => it.put_value(Value::Object(o), k_len, Value::Number(*len as f64))?,
            SKind::Map(es) | SKind::Set(es) => {
                let is_map = matches!(n.kind, SKind::Map(_));
                for (k, v) in es {
                    it.charge(1)?;
                    let k = sv_value(it, &objs, k)?;
                    let v = if is_map { sv_value(it, &objs, v)? } else { k };
                    it.clone_coll_put(o, k, v)?;
                }
            }
            SKind::Error(_, m, s, cause) => {
                for (name, v) in [("message", m), ("stack", s), ("cause", cause)] {
                    if let Some(v) = v {
                        let key = d.key(it, name)?;
                        let val = sv_value(it, &objs, v)?;
                        it.define_data(o, key, val, true, false, true)?;
                    }
                }
            }
            _ => {}
        }
    }
    Ok((sv_value(it, &objs, &st.root)?, ports))
}
