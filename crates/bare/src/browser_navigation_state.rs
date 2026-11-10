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
    seen: HashMap<ObjId, u32>,
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
    match walk(d, it, v, b) {
        // The size is the page's choice: a catchable error, as in Firefox. The depth cap stays a limit.
        Err(Error::Limit(LimitKind::Host("history-state-bytes" | "history-state-nodes"))) => Err(clone_err(d, it, "A state over the size cap")),
        r => r,
    }
}

/// `structuredClone(v)`.
pub(crate) fn clone(d: &Dom, it: &mut Interp<'_>, v: Value) -> R<Value> {
    let st = clone_state(d, it, v)?;
    materialize(d, it, &st)
}

/// A snapshot with structured-clone limits. Resource limits remain uncatchable.
pub(crate) fn clone_state(d: &Dom, it: &mut Interp<'_>, v: Value) -> R<State> {
    walk(d, it, v, Bounds { bytes: 64 << 20, depth: 256, nodes: 1 << 20, names: ["clone-bytes", "clone-depth", "clone-nodes"] })
}

fn walk(d: &Dom, it: &mut Interp<'_>, v: Value, b: Bounds) -> R<State> {
    let mut c = Ser { seen: HashMap::new(), nodes: Vec::new(), bytes: 0, b };
    let root = ser(d, it, &mut c, v, 0)?;
    Ok(State { root, nodes: c.nodes, bytes: c.bytes })
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
    sv_value(it, &objs, &st.root)
}
