//! `classList`, `relList` and `DOMTokenList` over native attributes (DOM Standard, "DOMTokenList").
//!
//! - The list stores its element and backing attribute (`class` or `rel`). Every call reads that attribute, splits it on ASCII
//!   white space into an ordered set (first occurrence wins) and, for a change, writes the serialized set back with the
//!   normal attribute path (`attr_set`: caps, charge, native cache invalidation). So `setAttribute("class", ...)`,
//!   `className` and the list always agree, and the native selector matcher sees every change.
//! - `el.classList` is one object for each element while the element wrapper lives (`SameObject`): the wrapper traces
//!   the list and the list traces the wrapper, so a detached element lives as long as its list or iterator is reachable.
//!   The setter forwards to `value` (`[PutForwards=value]`).
//! - Order of work, as WebIDL and the standard say: every argument is converted with `ToString` first (user code may
//!   run, no borrow is held), then tokens are validated in argument order (empty: `SyntaxError`, white space:
//!   `InvalidCharacterError`; `replace` checks both for empty first), then the attribute is read and changed. A bad
//!   token changes nothing. The update steps write nothing when the element has no `class` attribute and the set is
//!   empty. A write whose text equals the current attribute is skipped (not observable without mutation observers).
//! - `forEach` and the iterators read the live list at each step. An exhausted iterator stays done. The iterators are
//!   host objects with `next()` that inherit `%IteratorPrototype%`, so `for..of`, spread and `Array.from` accept them.
//!   `[Symbol.iterator]` is this module's `values` (the realm's own `Symbol.iterator`).
//! - Indexes: the list is a host-indexed object (`Heap::new_indexed`, like `NodeList` and `NamedNodeMap`). `list[i]` is `item(i)`,
//!   and `i` past the end is absent, so `in`, `Object.keys`, `slice` and `for..in` see the tokens, as Chrome does (the WebIDL
//!   indexed getter of `DOMTokenList`). The count and the strings are a cache of the attribute text (`Toks`). A stale cache
//!   asks the host first (`indexed_fill`, the `Host::fill_indexed` of this module), so no read is ever stale. The list traces
//!   the strings.
//!   Port: `item` follows Ladybird `DOMTokenList::item` (`Libraries/LibWeb/DOM/DOMTokenList.cpp`, BSD-2): an index at or past the
//!   size is null. The index rule is the WebIDL one: `list[i]` exists when `i` is less than `length`.
//!
//! Gates and deviations: class-list `supports()` throws `TypeError`, as `class` defines no supported tokens. The iterator has
//! no `Symbol.toStringTag`. A bad `classList` call on a wrong `this` is `TypeError: Illegal invocation`.

use crate::browser::*;
use crate::browser_dom::attr_set;
use crate::browser_platform::{TokIter, V, arg};
use bare_html::dom::{Atom, Document, NodeId};
use bare_js::eval::Interp;
use bare_js::heap::{HeapError, Slot};
use bare_js::value::{ObjId, PropertyKey, StrId, Value};
use std::collections::HashSet;

/// The indexes of one token list: the attribute text they were made from, and the token strings (the ordered set).
pub(crate) struct Toks {
    pub attr: String,
    pub vals: Vec<StrId>,
}

pub(crate) fn install(d: &Dom, it: &mut Interp<'_>, list: ObjId, iter: ObjId, sym: PropertyKey) -> R<()> {
    d.def_method(it, list, "item", 1, item)?;
    d.def_method(it, list, "contains", 1, contains)?;
    d.def_method(it, list, "add", 0, add)?;
    d.def_method(it, list, "remove", 0, remove)?;
    d.def_method(it, list, "toggle", 1, toggle)?;
    d.def_method(it, list, "replace", 2, replace)?;
    d.def_method(it, list, "supports", 1, supports)?;
    d.def_method(it, list, "toString", 0, value_get)?;
    d.def_method(it, list, "forEach", 1, for_each)?;
    d.def_method(it, list, "keys", 0, keys)?;
    d.def_method(it, list, "entries", 0, entries)?;
    let values = d.def_method(it, list, "values", 0, values)?;
    it.define_data(list, sym, Value::Object(values), true, false, true)?;
    d.def_get(it, list, "length", length, None)?;
    d.def_get(it, list, "value", value_get, Some(value_set))?;
    d.def_method(it, iter, "next", 0, iter_next)?;
    Ok(())
}

fn element(d: &Dom, it: &mut Interp<'_>, this: Value) -> R<NodeId> {
    let n = d.nd(it, this)?;
    if d.is_element(n) { Ok(n) } else { Err(it.type_error("Illegal invocation")) }
}

fn list_of(d: &Dom, it: &mut Interp<'_>, this: Value) -> R<(ObjId, (NodeId, &'static str))> {
    if let Value::Object(o) = this
        && let Some(&el) = d.plat.s.borrow().lists.get(&o)
    {
        return Ok((o, el));
    }
    Err(it.type_error("Illegal invocation"))
}

fn attribute<'a>(doc: &'a Document, el: (NodeId, &str)) -> Option<&'a str> {
    let atom = if el.1 == "class" { Some(Atom::CLASS) } else { doc.find_atom(el.1) };
    atom.and_then(|a| doc.attr(el.0, a))
}

/// The ordered set of an attribute text (the first occurrence of a token wins).
fn set(v: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    v.split_ascii_whitespace().filter(|t| seen.insert(*t)).map(String::from).collect()
}

/// The ordered set of the backing attribute now.
fn tokens(d: &Dom, it: &mut Interp<'_>, el: (NodeId, &str)) -> R<Vec<String>> {
    let doc = d.doc.borrow();
    let v = attribute(&doc, el).unwrap_or("");
    it.charge(1 + (v.len() / 8) as u64)?;
    Ok(set(v))
}

/// The update steps: write the serialized set back.
fn write(d: &Dom, it: &mut Interp<'_>, el: (NodeId, &str), toks: &[String]) -> R<()> {
    let new = toks.join(" ");
    {
        let doc = d.doc.borrow();
        match attribute(&doc, el) {
            None if toks.is_empty() => return Ok(()),
            Some(c) if c == new => return Ok(()),
            _ => {}
        }
    }
    attr_set(d, it, el.0, el.1, &new)
}

fn required(it: &mut Interp<'_>, a: &[Value], n: usize, name: &str) -> R<()> {
    if a.len() < n {
        return Err(it.type_error(&format!("Failed to execute '{name}' on 'DOMTokenList': {n} argument{} required, but only {} present.", if n == 1 { "" } else { "s" }, a.len())));
    }
    Ok(())
}

/// `ToString` of every argument, in order (user code may run here).
fn strings(d: &Dom, it: &mut Interp<'_>, a: &[Value]) -> R<Vec<String>> {
    if a.len() > d.pl_limits().max_class_tokens {
        return Err(d.lim("class-tokens"));
    }
    a.iter().map(|&v| d.string_arg(it, v)).collect()
}

fn check_empty(d: &Dom, it: &mut Interp<'_>, t: &str) -> R<()> {
    if t.is_empty() { Err(d.dom_err(it, "SyntaxError", "The token provided must not be empty.")) } else { Ok(()) }
}

fn check_space(d: &Dom, it: &mut Interp<'_>, t: &str) -> R<()> {
    if t.contains(|c: char| c.is_ascii_whitespace()) {
        Err(d.dom_err(it, "InvalidCharacterError", &format!("The token provided ('{t}') contains HTML space characters, which are not valid in tokens.")))
    } else {
        Ok(())
    }
}

fn validate(d: &Dom, it: &mut Interp<'_>, toks: &[String]) -> R<()> {
    for t in toks {
        check_empty(d, it, t)?;
        check_space(d, it, t)?;
    }
    Ok(())
}

pub(crate) fn class_list(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    token_list(d, it, this, "class")
}

pub(crate) fn rel_list<const KIND: usize>(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    rel_element::<KIND>(d, it, this)?;
    token_list(d, it, this, "rel")
}

fn rel_element<const KIND: usize>(d: &Dom, it: &mut Interp<'_>, this: Value) -> R<NodeId> {
    let el = element(d, it, this)?;
    if d.doc.borrow().is_html(el, ["link", "a", "area"][KIND]) {
        Ok(el)
    } else { Err(it.type_error("Illegal invocation")) }
}

pub(crate) fn set_rel_list<const KIND: usize>(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let el = rel_element::<KIND>(d, it, this)?;
    let v = d.string_arg(it, arg(a, 0))?;
    attr_set(d, it, el, "rel", &v)?;
    Ok(Value::Undefined)
}

fn token_list(d: &Dom, it: &mut Interp<'_>, this: Value, attr: &'static str) -> V {
    let el = (element(d, it, this)?, attr);
    if let Some(&o) = d.plat.s.borrow().by_el.get(&el) {
        return Ok(Value::Object(o));
    }
    if d.plat.s.borrow().lists.len() >= d.pl_limits().max_token_lists {
        return Err(d.lim("token-lists"));
    }
    let proto = d.plat.p.get().expect("installed").list;
    let o = it.heap.new_indexed(Some(proto)).map_err(|e| it.heap_error(e))?;
    let mut s = d.plat.s.borrow_mut();
    s.lists.insert(o, el);
    s.by_el.insert(el, o);
    Ok(Value::Object(o))
}

pub(crate) fn set_class_list(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let el = (element(d, it, this)?, "class");
    let v = d.string_arg(it, arg(a, 0))?;
    attr_set(d, it, el.0, el.1, &v)?;
    Ok(Value::Undefined)
}

/// The cache of token list `owner`, read by `f`, when it is fresh. `None`: `owner` is not a token list. A stale cache answers
/// `NeedsHost` (index 0: `indexed_fill` makes the whole cache), so the evaluator fills it and repeats the read.
fn fresh<T>(d: &Dom, owner: ObjId, f: impl FnOnce(&Toks) -> T) -> Option<Result<T, HeapError>> {
    let s = match d.plat.s.try_borrow() {
        Ok(s) => s,
        Err(_) => return Some(Err(HeapError::HostUnavailable)),
    };
    let el = *s.lists.get(&owner)?;
    let doc = match d.doc.try_borrow() {
        Ok(doc) => doc,
        Err(_) => return Some(Err(HeapError::HostUnavailable)),
    };
    let now = attribute(&doc, el).unwrap_or("");
    Some(match s.toks.get(&owner) {
        Some(t) if t.attr == now => Ok(f(t)),
        _ => Err(HeapError::NeedsHost { owner, index: 0 }),
    })
}

/// `IndexedSource::indexed_len` for a token list. `None`: `owner` is not a token list.
pub(crate) fn indexed_len(d: &Dom, owner: ObjId) -> Option<Result<usize, HeapError>> {
    fresh(d, owner, |t| t.vals.len())
}

/// `IndexedSource::indexed_item` for a token list. `None`: `owner` is not a token list.
pub(crate) fn indexed_item(d: &Dom, owner: ObjId, index: usize) -> Option<Result<Slot, HeapError>> {
    fresh(d, owner, |t| t.vals.get(index).map_or(Slot::Absent, |&v| Slot::Str(v)))
}

/// `Host::fill_indexed` for a token list: make the cache of `owner` fresh. `Ok(false)`: `owner` is not a token list.
pub(crate) fn indexed_fill(d: &Dom, it: &mut Interp<'_>, owner: ObjId) -> R<bool> {
    let el = d.plat.s.borrow().lists.get(&owner).copied();
    let Some(el) = el else { return Ok(false) };
    let attr = {
        let doc = d.doc.borrow();
        attribute(&doc, el).unwrap_or("").to_string()
    };
    it.charge(1 + (attr.len() / 8) as u64)?;
    let mut vals = Vec::new();
    for t in set(&attr) {
        match d.js_str(it, &t)? {
            Value::String(s) => vals.push(s),
            _ => unreachable!("js_str makes a string"),
        }
    }
    d.plat.s.borrow_mut().toks.insert(owner, Toks { attr, vals });
    Ok(true)
}

fn length(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let (_, el) = list_of(d, it, this)?;
    Ok(Value::Number(tokens(d, it, el)?.len() as f64))
}

fn value_get(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let (_, el) = list_of(d, it, this)?;
    let s = {
        let doc = d.doc.borrow();
        attribute(&doc, el).unwrap_or("").to_string()
    };
    d.js_str(it, &s)
}

fn value_set(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let (_, el) = list_of(d, it, this)?;
    let v = d.string_arg(it, arg(a, 0))?;
    attr_set(d, it, el.0, el.1, &v)?;
    Ok(Value::Undefined)
}

/// WebIDL `unsigned long`: ToNumber, then modulo 2^32.
pub(crate) fn to_u32(it: &mut Interp<'_>, v: Value) -> R<u32> {
    let n = it.to_js_number(v)?;
    Ok(if n.is_finite() { n.trunc().rem_euclid(4294967296.0) as u32 } else { 0 })
}

fn item(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let (_, el) = list_of(d, it, this)?;
    required(it, a, 1, "item")?;
    let i = to_u32(it, a[0])? as usize;
    match tokens(d, it, el)?.get(i) {
        Some(t) => d.js_str(it, t),
        None => Ok(Value::Null),
    }
}

fn contains(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let (_, el) = list_of(d, it, this)?;
    required(it, a, 1, "contains")?;
    let t = d.string_arg(it, a[0])?;
    let doc = d.doc.borrow();
    it.charge(1 + (attribute(&doc, el).map_or(0, str::len) / 32) as u64)?;
    Ok(Value::Bool(if el.1 == "class" { doc.has_class(el.0, &t) } else { attribute(&doc, el).is_some_and(|v| v.split_ascii_whitespace().any(|x| x == t)) }))
}

fn add(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let (_, el) = list_of(d, it, this)?;
    let new = strings(d, it, a)?;
    validate(d, it, &new)?;
    let mut cur = tokens(d, it, el)?;
    let mut have: HashSet<String> = cur.iter().cloned().collect();
    for t in new {
        if have.insert(t.clone()) {
            cur.push(t);
        }
    }
    write(d, it, el, &cur)?;
    Ok(Value::Undefined)
}

fn remove(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let (_, el) = list_of(d, it, this)?;
    let gone = strings(d, it, a)?;
    validate(d, it, &gone)?;
    let gone: HashSet<String> = gone.into_iter().collect();
    let mut cur = tokens(d, it, el)?;
    cur.retain(|t| !gone.contains(t));
    write(d, it, el, &cur)?;
    Ok(Value::Undefined)
}

fn toggle(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let (_, el) = list_of(d, it, this)?;
    required(it, a, 1, "toggle")?;
    let t = d.string_arg(it, a[0])?;
    validate(d, it, std::slice::from_ref(&t))?;
    let force = match arg(a, 1) {
        Value::Undefined => None,
        v => Some(it.to_js_bool(v)?),
    };
    let mut cur = tokens(d, it, el)?;
    if let Some(i) = cur.iter().position(|x| *x == t) {
        if force == Some(true) {
            return Ok(Value::Bool(true));
        }
        cur.remove(i);
        write(d, it, el, &cur)?;
        return Ok(Value::Bool(false));
    }
    if force == Some(false) {
        return Ok(Value::Bool(false));
    }
    cur.push(t);
    write(d, it, el, &cur)?;
    Ok(Value::Bool(true))
}

fn replace(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let (_, el) = list_of(d, it, this)?;
    required(it, a, 2, "replace")?;
    let (old, new) = (d.string_arg(it, a[0])?, d.string_arg(it, a[1])?);
    check_empty(d, it, &old)?;
    check_empty(d, it, &new)?;
    check_space(d, it, &old)?;
    check_space(d, it, &new)?;
    let cur = tokens(d, it, el)?;
    if !cur.contains(&old) {
        return Ok(Value::Bool(false));
    }
    let mut out = Vec::with_capacity(cur.len());
    let mut placed = false;
    for x in cur {
        if x == old || x == new {
            if !placed {
                out.push(new.clone());
                placed = true;
            }
        } else {
            out.push(x);
        }
    }
    write(d, it, el, &out)?;
    Ok(Value::Bool(true))
}

fn supports(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let (_, el) = list_of(d, it, this)?;
    required(it, a, 1, "supports")?;
    let token = d.string_arg(it, a[0])?;
    if el.1 == "rel" {
        let token = token.to_ascii_lowercase();
        // Report only link processing implemented by this browser. Navigation and
        // the other resource-hint lifecycles are not implemented.
        let supported = d.doc.borrow().is_html(el.0, "link") && matches!(token.as_str(), "preload" | "stylesheet");
        return Ok(Value::Bool(supported));
    }
    Err(it.type_error("Failed to execute 'supports' on 'DOMTokenList': DOMTokenList has no supported tokens."))
}

fn for_each(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let (_, el) = list_of(d, it, this)?;
    required(it, a, 1, "forEach")?;
    let cb = a[0];
    if !it.is_callable_value(cb) {
        return Err(it.type_error("Failed to execute 'forEach' on 'DOMTokenList': parameter 1 is not of type 'Function'."));
    }
    let mut i = 0usize;
    loop {
        let t = match tokens(d, it, el)?.get(i) {
            Some(t) => d.js_str(it, t)?,
            None => return Ok(Value::Undefined),
        };
        it.call(cb, arg(a, 1), &[t, Value::Number(i as f64), this])?;
        i += 1;
    }
}

fn make_iter(d: &Dom, it: &mut Interp<'_>, this: Value, kind: u8) -> V {
    let (list, _) = list_of(d, it, this)?;
    if d.plat.s.borrow().iters.len() >= d.pl_limits().max_token_iters {
        return Err(d.lim("token-iterators"));
    }
    let o = d.obj(it, d.plat.p.get().expect("installed").iter)?;
    d.plat.s.borrow_mut().iters.insert(o, TokIter { list, idx: 0, kind, done: false });
    Ok(Value::Object(o))
}

fn keys(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    make_iter(d, it, this, 1)
}

fn values(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    make_iter(d, it, this, 0)
}

fn entries(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    make_iter(d, it, this, 2)
}

fn result(d: &Dom, it: &mut Interp<'_>, value: Value, done: bool) -> V {
    let o = d.obj(it, it.object_prototype())?;
    let (kv, kd) = (d.key(it, "value")?, d.key(it, "done")?);
    it.define_data(o, kv, value, true, true, true)?;
    it.define_data(o, kd, Value::Bool(done), true, true, true)?;
    Ok(Value::Object(o))
}

fn iter_next(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let Value::Object(o) = this else { return Err(it.type_error("Illegal invocation")) };
    let (list, idx, kind, done) = match d.plat.s.borrow().iters.get(&o) {
        Some(i) => (i.list, i.idx, i.kind, i.done),
        None => return Err(it.type_error("Illegal invocation")),
    };
    let el = d.plat.s.borrow().lists.get(&list).copied();
    let tok = match (done, el) {
        (false, Some(el)) => tokens(d, it, el)?.into_iter().nth(idx),
        _ => None,
    };
    let Some(tok) = tok else {
        d.plat.s.borrow_mut().iters.get_mut(&o).expect("checked").done = true;
        return result(d, it, Value::Undefined, true);
    };
    d.plat.s.borrow_mut().iters.get_mut(&o).expect("checked").idx = idx + 1;
    let s = d.js_str(it, &tok)?;
    let v = match kind {
        0 => s,
        1 => Value::Number(idx as f64),
        _ => it.array_from(&[Value::Number(idx as f64), s])?,
    };
    result(d, it, v, false)
}
