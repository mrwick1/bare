//! History state as cloned data. `pushState(data)` copies `data` into a native `State` (no JS object is kept), and
//! `history.state` builds a fresh object graph from it (once per entry: the result is cached, so `history.state ===
//! history.state`). It is not JSON: cycles, shared references, `undefined`, `NaN`, `-0`, sparse arrays and lone
//! surrogates survive.
//!
//! Supported: `undefined`, `null`, booleans, numbers, strings, arrays and plain objects (prototype `Object.prototype` or
//! `null`). Own enumerable string keys are copied through `[[Get]]`, so getters run, in key order, with no host borrow.
//! Gated: a function or a symbol is a `DataCloneError`; a DOM wrapper, `URL` or `URLSearchParams` is a `DataCloneError`;
//! any other object (`Date`, `Map`, typed array, class instance, ...) is `Unsupported`, never copied as a plain object.
//! Bounds (`NavLimits`): depth, nodes and bytes of one state, and the bytes of all states of a document.

use crate::browser::*;
use crate::browser_navigation::NavLimits;
use bare_js::eval::{Error, Interp};
use bare_js::heap::Property;
use bare_js::value::{ObjId, PropertyKey, Value};
use std::collections::HashMap;
use std::rc::Rc;

pub(crate) enum SV {
    Undef,
    Null,
    Bool(bool),
    Num(f64),
    Str(Rc<[u16]>),
    Obj(u32),
}

pub(crate) enum SKey {
    Idx(u32),
    Str(Rc<[u16]>),
}

pub(crate) struct SNode {
    /// `Some(length)` for an array.
    pub len: Option<u32>,
    pub props: Vec<(SKey, SV)>,
}

pub(crate) struct State {
    pub root: SV,
    pub nodes: Vec<SNode>,
    pub bytes: usize,
}

struct Ser {
    seen: HashMap<ObjId, u32>,
    nodes: Vec<SNode>,
    bytes: usize,
    lim: NavLimits,
}

fn add(c: &mut Ser, d: &Dom, n: usize) -> R<()> {
    c.bytes = c.bytes.saturating_add(n);
    if c.bytes > c.lim.max_state_bytes {
        return Err(d.lim("history-state-bytes"));
    }
    Ok(())
}

fn clone_err(d: &Dom, it: &mut Interp<'_>, what: &str) -> Error {
    d.dom_err(it, "DataCloneError", &format!("{what} could not be cloned."))
}

pub(crate) fn serialize(d: &Dom, it: &mut Interp<'_>, v: Value) -> R<State> {
    let mut c = Ser { seen: HashMap::new(), nodes: Vec::new(), bytes: 0, lim: d.nav.lim.get() };
    let root = ser(d, it, &mut c, v, 0)?;
    Ok(State { root, nodes: c.nodes, bytes: c.bytes })
}

fn ser(d: &Dom, it: &mut Interp<'_>, c: &mut Ser, v: Value, depth: usize) -> R<SV> {
    it.charge(1)?;
    Ok(match v {
        Value::Undefined => SV::Undef,
        Value::Null => SV::Null,
        Value::Bool(b) => SV::Bool(b),
        Value::Number(n) => SV::Num(n),
        Value::Symbol(_) => return Err(clone_err(d, it, "Symbol")),
        Value::String(s) => {
            let len = it.heap.str_len(s).map_err(Error::Heap)?;
            add(c, d, len.saturating_mul(2) + 16)?;
            it.charge((len / 32) as u64)?;
            let u: Rc<[u16]> = match it.heap.str_units(s) {
                Ok(u) => Rc::from(u),
                Err(e) => return Err(it.heap_error(e)),
            };
            SV::Str(u)
        }
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
            // A byte view is never plain data, whatever its prototype. Refuse it before `own_keys` lists one key per byte.
            if it.heap.view_parts(o).is_some() {
                return Err(Error::Unsupported(unsupported("structured clone of an object that is not a plain object or array")));
            }
            let is_arr = it.heap.is_array(o).map_err(Error::Heap)?;
            let proto = it.heap.get_prototype_of(o).map_err(Error::Heap)?;
            let plain = if is_arr { proto == Some(it.array_prototype()) } else { proto.is_none() || proto == Some(it.object_prototype()) };
            if !plain {
                return Err(Error::Unsupported(unsupported("structured clone of an object that is not a plain object or array")));
            }
            if depth >= c.lim.max_state_depth.min(128) {
                return Err(d.lim("history-state-depth"));
            }
            if c.nodes.len() >= c.lim.max_state_nodes {
                return Err(d.lim("history-state-nodes"));
            }
            add(c, d, 48)?;
            let idx = c.nodes.len() as u32;
            c.seen.insert(o, idx);
            let len = if is_arr {
                let k = d.key(it, "length")?;
                match it.heap.get_own_property(o, k) {
                    Ok(Some(Property::Data { value: Value::Number(n), .. })) => Some(n as u32),
                    _ => Some(0),
                }
            } else {
                None
            };
            c.nodes.push(SNode { len, props: Vec::new() });
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
            SV::Obj(idx)
        }
    })
}

fn sv_value(it: &mut Interp<'_>, objs: &[ObjId], v: &SV) -> R<Value> {
    Ok(match v {
        SV::Undef => Value::Undefined,
        SV::Null => Value::Null,
        SV::Bool(b) => Value::Bool(*b),
        SV::Num(n) => Value::Number(*n),
        SV::Obj(i) => Value::Object(objs[*i as usize]),
        SV::Str(u) => match it.heap.intern_units(u) {
            Ok(s) => Value::String(s),
            Err(e) => return Err(it.heap_error(e)),
        },
    })
}

/// A new object graph for `st`. No JS runs (the objects are new, so no setter can be hit).
pub(crate) fn materialize(d: &Dom, it: &mut Interp<'_>, st: &State) -> R<Value> {
    let mut objs = Vec::with_capacity(st.nodes.len());
    for n in &st.nodes {
        it.charge(1)?;
        let o = match n.len {
            Some(_) => it.heap.new_array(Some(it.array_prototype())),
            None => it.heap.new_object(Some(it.object_prototype())),
        };
        match o {
            Ok(o) => objs.push(o),
            Err(e) => return Err(it.heap_error(e)),
        }
    }
    let k_len = d.key(it, "length")?;
    for (n, &o) in st.nodes.iter().zip(&objs) {
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
        if let Some(len) = n.len {
            it.put_value(Value::Object(o), k_len, Value::Number(len as f64))?;
        }
    }
    sv_value(it, &objs, &st.root)
}
