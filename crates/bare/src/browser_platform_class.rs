//! `classList`, `relList`, `sandbox` and `DOMTokenList` over native attributes (DOM Standard, "DOMTokenList").
//!
//! - The list stores its element and backing attribute (`class`, `rel` or `sandbox`). Every call reads that attribute, splits it on ASCII
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
use crate::browser_dom::attr_set_s;
use crate::browser_platform::TokIter;
use crate::browser_scoped::*;
use bare_html::dom::{Atom, Document, NodeId};
use bare_js::eval::Interp;
use bare_js::heap::{HeapError, Slot};
use bare_js::value::{ObjId, PropertyKey, StrId, Value};
use bare_js_api::{Args, Attrs, Engine, Local, Type};
use std::collections::HashSet;

/// The indexes of one token list: the attribute text they were made from, and the token strings (the ordered set).
pub(crate) struct Toks {
    pub attr: String,
    pub vals: Vec<StrId>,
}

pub(crate) fn install(d: &Dom, it: &mut Interp<'_>, list: ObjId, iter: ObjId, sym: PropertyKey) -> R<()> {
    d.def_method_s(it, list, "item", 1, item)?;
    d.def_method_s(it, list, "contains", 1, contains)?;
    d.def_method_s(it, list, "add", 0, add)?;
    d.def_method_s(it, list, "remove", 0, remove)?;
    d.def_method_s(it, list, "toggle", 1, toggle)?;
    d.def_method_s(it, list, "replace", 2, replace)?;
    d.def_method_s(it, list, "supports", 1, supports)?;
    d.def_method_s(it, list, "toString", 0, value_get)?;
    d.def_method_s(it, list, "forEach", 1, for_each)?;
    d.def_method_s(it, list, "keys", 0, keys)?;
    d.def_method_s(it, list, "entries", 0, entries)?;
    let values = d.def_method_s(it, list, "values", 0, values)?;
    it.define_data(list, sym, Value::Object(values), true, false, true)?;
    d.def_get_s(it, list, "length", length, None)?;
    d.def_get_s(it, list, "value", value_get, Some(value_set))?;
    d.def_method_s(it, iter, "next", 0, iter_next)?;
    Ok(())
}

fn element<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>) -> Res<'s, NodeId> {
    let n = d.nd_s(s, this)?;
    if d.is_element(n) { Ok(n) } else { Err(s.type_error("Illegal invocation")) }
}

fn list_of<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>) -> Res<'s, (ObjId, (NodeId, &'static str))> {
    if let Some(o) = obj_id(s, this)
        && let Some(&el) = d.plat.s.borrow().lists.get(&o)
    {
        return Ok((o, el));
    }
    Err(s.type_error("Illegal invocation"))
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
fn tokens<'s>(d: &Dom, s: &mut Sc<'s, '_>, el: (NodeId, &str)) -> Res<'s, Vec<String>> {
    let doc = d.doc.borrow();
    let v = attribute(&doc, el).unwrap_or("");
    s.charge(1 + (v.len() / 8) as u64)?;
    Ok(set(v))
}

/// The update steps: write the serialized set back.
fn write<'s>(d: &Dom, s: &mut Sc<'s, '_>, el: (NodeId, &str), toks: &[String]) -> Res<'s, ()> {
    let new = toks.join(" ");
    {
        let doc = d.doc.borrow();
        match attribute(&doc, el) {
            None if toks.is_empty() => return Ok(()),
            Some(c) if c == new => return Ok(()),
            _ => {}
        }
    }
    attr_set_s(d, s, el.0, el.1, &new)
}

fn required<'s>(s: &mut Sc<'s, '_>, a: Args<'s>, n: usize, name: &str) -> Res<'s, ()> {
    if a.len() < n {
        return Err(s.type_error(&format!("Failed to execute '{name}' on 'DOMTokenList': {n} argument{} required, but only {} present.", if n == 1 { "" } else { "s" }, a.len())));
    }
    Ok(())
}

/// `ToString` of every argument, in order (user code may run here).
fn strings<'s>(d: &Dom, s: &mut Sc<'s, '_>, a: Args<'s>) -> Res<'s, Vec<String>> {
    if a.len() > d.pl_limits().max_class_tokens {
        return Err(d.lim_s(s, "class-tokens"));
    }
    let mut out = Vec::with_capacity(a.len());
    for v in a.iter() {
        out.push(d.string_arg_s(s, v)?);
    }
    Ok(out)
}

fn check_empty<'s>(d: &Dom, s: &mut Sc<'s, '_>, t: &str) -> Res<'s, ()> {
    if t.is_empty() { Err(d.dom_err_s(s, "SyntaxError", "The token provided must not be empty.")) } else { Ok(()) }
}

fn check_space<'s>(d: &Dom, s: &mut Sc<'s, '_>, t: &str) -> Res<'s, ()> {
    if t.contains(|c: char| c.is_ascii_whitespace()) {
        Err(d.dom_err_s(s, "InvalidCharacterError", &format!("The token provided ('{t}') contains HTML space characters, which are not valid in tokens.")))
    } else {
        Ok(())
    }
}

fn validate<'s>(d: &Dom, s: &mut Sc<'s, '_>, toks: &[String]) -> Res<'s, ()> {
    for t in toks {
        check_empty(d, s, t)?;
        check_space(d, s, t)?;
    }
    Ok(())
}

pub(crate) fn class_list<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    token_list(d, s, this, "class")
}

pub(crate) fn rel_list<'s, const KIND: usize>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    rel_element::<KIND>(d, s, this)?;
    token_list(d, s, this, "rel")
}

fn rel_element<'s, const KIND: usize>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>) -> Res<'s, NodeId> {
    let el = element(d, s, this)?;
    if d.doc.borrow().is_html(el, ["link", "a", "area"][KIND]) {
        Ok(el)
    } else { Err(s.type_error("Illegal invocation")) }
}

pub(crate) fn set_rel_list<'s, const KIND: usize>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let el = rel_element::<KIND>(d, s, this)?;
    let v = d.arg_str_s(s, a, 0)?;
    attr_set_s(d, s, el, "rel", &v)?;
    Ok(s.undefined())
}

pub(crate) fn sandbox_list<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    sandbox_element(d, s, this)?;
    token_list(d, s, this, "sandbox")
}

fn sandbox_element<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>) -> Res<'s, NodeId> {
    let el = element(d, s, this)?;
    if d.doc.borrow().is_html(el, "iframe") {
        Ok(el)
    } else { Err(s.type_error("Illegal invocation")) }
}

pub(crate) fn set_sandbox_list<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let el = sandbox_element(d, s, this)?;
    let v = d.arg_str_s(s, a, 0)?;
    attr_set_s(d, s, el, "sandbox", &v)?;
    Ok(s.undefined())
}

fn token_list<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, attr: &'static str) -> Res<'s, Local<'s>> {
    let el = (element(d, s, this)?, attr);
    let known = d.plat.s.borrow().by_el.get(&el).copied();
    if let Some(o) = known {
        return Ok(d.obj_local(s, o));
    }
    if d.plat.s.borrow().lists.len() >= d.pl_limits().max_token_lists {
        return Err(d.lim_s(s, "token-lists"));
    }
    let proto = d.plat.p.get().expect("installed").list;
    let o = d.new_source_s(s, crate::browser_sources::Source::TokenList, Some(proto))?;
    let mut m = d.plat.s.borrow_mut();
    m.lists.insert(o, el);
    m.by_el.insert(el, o);
    drop(m);
    Ok(d.obj_local(s, o))
}

pub(crate) fn set_class_list<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let el = (element(d, s, this)?, "class");
    let v = d.arg_str_s(s, a, 0)?;
    attr_set_s(d, s, el.0, el.1, &v)?;
    Ok(s.undefined())
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

fn length<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let (_, el) = list_of(d, s, this)?;
    let n = tokens(d, s, el)?.len();
    Ok(s.number(n as f64))
}

fn value_get<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let (_, el) = list_of(d, s, this)?;
    let text = {
        let doc = d.doc.borrow();
        attribute(&doc, el).unwrap_or("").to_string()
    };
    d.js_str_s(s, &text)
}

fn value_set<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let (_, el) = list_of(d, s, this)?;
    let v = d.arg_str_s(s, a, 0)?;
    attr_set_s(d, s, el.0, el.1, &v)?;
    Ok(s.undefined())
}

fn item<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let (_, el) = list_of(d, s, this)?;
    required(s, a, 1, "item")?;
    let v = arg_s(s, a, 0);
    let i = bare_js::value::to_uint32(s.to_number(v)?) as usize;
    match tokens(d, s, el)?.get(i) {
        Some(t) => d.js_str_s(s, t),
        None => Ok(s.null()),
    }
}

fn contains<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let (_, el) = list_of(d, s, this)?;
    required(s, a, 1, "contains")?;
    let t = d.arg_str_s(s, a, 0)?;
    let hit = {
        let doc = d.doc.borrow();
        s.charge(1 + (attribute(&doc, el).map_or(0, str::len) / 32) as u64)?;
        if el.1 == "class" { doc.has_class(el.0, &t) } else { attribute(&doc, el).is_some_and(|v| v.split_ascii_whitespace().any(|x| x == t)) }
    };
    Ok(s.boolean(hit))
}

fn add<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let (_, el) = list_of(d, s, this)?;
    let new = strings(d, s, a)?;
    validate(d, s, &new)?;
    let mut cur = tokens(d, s, el)?;
    let mut have: HashSet<String> = cur.iter().cloned().collect();
    for t in new {
        if have.insert(t.clone()) {
            cur.push(t);
        }
    }
    write(d, s, el, &cur)?;
    Ok(s.undefined())
}

fn remove<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let (_, el) = list_of(d, s, this)?;
    let gone = strings(d, s, a)?;
    validate(d, s, &gone)?;
    let gone: HashSet<String> = gone.into_iter().collect();
    let mut cur = tokens(d, s, el)?;
    cur.retain(|t| !gone.contains(t));
    write(d, s, el, &cur)?;
    Ok(s.undefined())
}

fn toggle<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let (_, el) = list_of(d, s, this)?;
    required(s, a, 1, "toggle")?;
    let t = d.arg_str_s(s, a, 0)?;
    validate(d, s, std::slice::from_ref(&t))?;
    let f = arg_s(s, a, 1);
    let force = if s.type_of(f) == Type::Undefined { None } else { Some(s.to_boolean(f)) };
    let mut cur = tokens(d, s, el)?;
    if let Some(i) = cur.iter().position(|x| *x == t) {
        if force == Some(true) {
            return Ok(s.boolean(true));
        }
        cur.remove(i);
        write(d, s, el, &cur)?;
        return Ok(s.boolean(false));
    }
    if force == Some(false) {
        return Ok(s.boolean(false));
    }
    cur.push(t);
    write(d, s, el, &cur)?;
    Ok(s.boolean(true))
}

fn replace<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let (_, el) = list_of(d, s, this)?;
    required(s, a, 2, "replace")?;
    let (old, new) = (d.arg_str_s(s, a, 0)?, d.arg_str_s(s, a, 1)?);
    check_empty(d, s, &old)?;
    check_empty(d, s, &new)?;
    check_space(d, s, &old)?;
    check_space(d, s, &new)?;
    let cur = tokens(d, s, el)?;
    if !cur.contains(&old) {
        return Ok(s.boolean(false));
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
    write(d, s, el, &out)?;
    Ok(s.boolean(true))
}

fn supports<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let (_, el) = list_of(d, s, this)?;
    required(s, a, 1, "supports")?;
    let token = d.arg_str_s(s, a, 0)?;
    if el.1 == "rel" {
        let token = token.to_ascii_lowercase();
        // Report only link processing implemented by this browser. Navigation and
        // the other resource-hint lifecycles are not implemented.
        let supported = d.doc.borrow().is_html(el.0, "link") && matches!(token.as_str(), "preload" | "stylesheet");
        return Ok(s.boolean(supported));
    }
    if el.1 == "sandbox" {
        let token = token.to_ascii_lowercase();
        let supported = [
            "allow-downloads", "allow-forms", "allow-modals", "allow-orientation-lock", "allow-pointer-lock", "allow-popups",
            "allow-popups-to-escape-sandbox", "allow-presentation", "allow-same-origin", "allow-scripts", "allow-top-navigation",
            "allow-top-navigation-by-user-activation", "allow-top-navigation-to-custom-protocols",
        ].contains(&token.as_str());
        return Ok(s.boolean(supported));
    }
    Err(s.type_error("Failed to execute 'supports' on 'DOMTokenList': DOMTokenList has no supported tokens."))
}

fn for_each<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let (_, el) = list_of(d, s, this)?;
    required(s, a, 1, "forEach")?;
    let cb = arg_s(s, a, 0);
    if !s.is_callable(cb) {
        return Err(s.type_error("Failed to execute 'forEach' on 'DOMTokenList': parameter 1 is not of type 'Function'."));
    }
    let this_arg = arg_s(s, a, 1);
    let mut i = 0usize;
    // The locals of one step are dropped before the next one.
    let base = s.engine().stack_len();
    loop {
        let t = match tokens(d, s, el)?.get(i) {
            Some(t) => d.js_str_s(s, t)?,
            None => return Ok(s.undefined()),
        };
        let n = s.number(i as f64);
        s.call(cb, this_arg, &[t, n, this])?;
        s.engine().stack_truncate(base);
        i += 1;
    }
}

fn make_iter<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, kind: u8) -> Res<'s, Local<'s>> {
    let (list, _) = list_of(d, s, this)?;
    if d.plat.s.borrow().iters.len() >= d.pl_limits().max_token_iters {
        return Err(d.lim_s(s, "token-iterators"));
    }
    let o = d.obj_s(s, d.plat.p.get().expect("installed").iter)?;
    d.plat.s.borrow_mut().iters.insert(o, TokIter { list, idx: 0, kind, done: false });
    Ok(d.obj_local(s, o))
}

fn keys<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    make_iter(d, s, this, 1)
}

fn values<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    make_iter(d, s, this, 0)
}

fn entries<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    make_iter(d, s, this, 2)
}

fn result<'s>(s: &mut Sc<'s, '_>, value: Local<'s>, done: bool) -> Res<'s, Local<'s>> {
    let o = s.new_object()?;
    let all = Attrs { writable: true, enumerable: true, configurable: true };
    s.define_str(o, "value", value, all)?;
    let done = s.boolean(done);
    s.define_str(o, "done", done, all)?;
    Ok(o)
}

fn iter_next<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let Some(o) = obj_id(s, this) else { return Err(s.type_error("Illegal invocation")) };
    let found = d.plat.s.borrow().iters.get(&o).map(|i| (i.list, i.idx, i.kind, i.done));
    let Some((list, idx, kind, done)) = found else { return Err(s.type_error("Illegal invocation")) };
    let el = d.plat.s.borrow().lists.get(&list).copied();
    let tok = match (done, el) {
        (false, Some(el)) => tokens(d, s, el)?.into_iter().nth(idx),
        _ => None,
    };
    let Some(tok) = tok else {
        d.plat.s.borrow_mut().iters.get_mut(&o).expect("checked").done = true;
        let u = s.undefined();
        return result(s, u, true);
    };
    d.plat.s.borrow_mut().iters.get_mut(&o).expect("checked").idx = idx + 1;
    let text = d.js_str_s(s, &tok)?;
    let v = match kind {
        0 => text,
        1 => s.number(idx as f64),
        _ => {
            let n = s.number(idx as f64);
            s.new_array(&[n, text])?
        }
    };
    result(s, v, false)
}
