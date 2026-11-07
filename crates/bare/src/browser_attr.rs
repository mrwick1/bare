//! `Element.attributes` (`NamedNodeMap`) and `Attr` over the native attribute list (DOM Standard, "Attr", "NamedNodeMap").
//!
//! The native `Document` is the one source of truth. There is no second list of attributes.
//! - `attributes` is one `NamedNodeMap` object for each element while the element wrapper lives (`SameObject`). It stores only
//!   its element. Its `length` and every index are read from the native attribute list when JavaScript asks (the heap calls
//!   `Dom::indexed_len` and `Dom::indexed_item`, see `bare_js::heap::IndexedSource`). So it stays live after every mutation
//!   path: `setAttribute`, `removeAttribute`, the `*NS` methods, `style`, `classList`, `className`, the driver. Nothing caches
//!   the length or the elements.
//! - An `Attr` object is one object for each *attached* attribute while it is held: the key is (element, native name atom).
//!   Native attributes have no identity of their own, so the document logs the removal of a watched attribute
//!   (`Document::watch_attr`, `take_removed_attrs`). Before any `Attr` or map operation `drain` reads that log: the `Attr` of
//!   a removed attribute becomes detached and keeps the value it had (a span in the text buffer, which never frees).
//!   Adding the attribute back makes a new `Attr`: it never aliases the old one.
//! - The index `i` of the map is the `i`-th native attribute. Its `Attr` is made the first time it is read (`fill_indexed`):
//!   the heap asks the host for the missing object and repeats the read. So a map of a thousand attributes costs nothing
//!   until the page reads them.
//! - Attached `Attr`s are traced from their element wrapper, and an `Attr` and a map trace the element wrapper. A detached
//!   element lives while one of its `Attr`s or its map is reachable, and the `Attr` of a live element is `SameObject`.
//!
//! Supported: `Element.attributes`, `getAttributeNode`, `getAttributeNodeNS`, `setAttributeNode`, `setAttributeNodeNS`,
//! `removeAttributeNode`, `hasAttributes`; `NamedNodeMap`: `length`, indices, `item`, `getNamedItem`, `getNamedItemNS`,
//! `setNamedItem`, `setNamedItemNS`, `removeNamedItem`, `removeNamedItemNS`, `[Symbol.iterator]` (the array one);
//! `document.createAttribute`, `createAttributeNS`; `Attr`: `name localName prefix namespaceURI value nodeValue textContent
//! ownerElement ownerDocument specified nodeType nodeName`, and `null`/empty answers for the tree members that an `Attr` lacks.
//! Namespaces are the ones of `browser_namespace.rs` (none, XML, XMLNS, XLink): any other is `Unsupported`.
//!
//! Semantics worth knowing: `name`, `prefix` and `namespaceURI` come from the native name atom, so a prefix stays what it was
//! when the attribute was made (`setAttributeNS` on an existing attribute changes the value only, as the Standard says).
//! `removeAttributeNode` and `removeNamedItem*` give back the exact `Attr` object, now detached, with its old value.
//! A detached `Attr` can be set again with `setAttributeNode` and then is that attribute (same object). Errors:
//! `InUseAttributeError` for an `Attr` of another element, `NotFoundError` for removal of an attribute that is not there.
//!
//! Gates (never silent, never faked):
//! - Named properties of the map (`map.id`) are absent: use `getNamedItem`. Indices are real.
//! - `setAttributeNode` of an `Attr` that has the namespace and local name of an existing attribute but another prefix is
//!   `Unsupported`: the native list would move the attribute to the end instead of replacing it in place.
//! - The other `Node` methods on an `Attr` (`cloneNode`, `appendChild`, events) throw `TypeError: Illegal
//!   invocation`. `new Attr()` and `new NamedNodeMap()` are illegal constructors. There is no `toggleAttribute`.
//! - A JS string with a lone surrogate cannot be stored: `Unsupported` (the shared rule of the DOM host).
//!
//! Caps: `Attr` and map objects count with `max_wrappers` (checked before anything is made, `dom-wrappers`); values and new
//! names use the document caps (`dom-text`, `dom-attrs`, `dom-names`) through the same path as `setAttribute`. Scans charge
//! the step budget. No document or table borrow is held while JS runs (string conversion comes first).

use crate::browser::*;
use crate::browser_dom::{arg, valid_name};
use crate::browser_namespace as ns;
use bare_html::dom::{Atom, NodeData, NodeId, RemovedAttr, Span};
use bare_js::eval::{Error, Interp};
use bare_js::heap::{HeapError, IndexedSource, Slot};
use bare_js::value::{ObjId, Value};
use std::cell::{OnceCell, RefCell};
use std::collections::HashMap;

type V = R<Value>;

/// What the host knows about one `Attr` object.
pub(crate) struct AttrObj {
    pub atom: Atom,
    /// The element that has this attribute. `None` once it is detached.
    pub owner: Option<NodeId>,
    /// The value while detached. An attached `Attr` reads the native value.
    pub value: Option<Span>,
}

#[derive(Default)]
pub(crate) struct Tables {
    pub attrs: HashMap<ObjId, AttrObj>,
    /// The attached `Attr`s of an element, by name atom. A short list: an element has few attributes.
    pub by_el: HashMap<NodeId, Vec<(Atom, ObjId)>>,
    pub maps: HashMap<ObjId, NodeId>,
    pub map_of: HashMap<NodeId, ObjId>,
}

#[derive(Default)]
pub(crate) struct AttrHost {
    pub s: RefCell<Tables>,
    /// `Attr.prototype`, `NamedNodeMap.prototype`.
    pub protos: OnceCell<(ObjId, ObjId)>,
    pub fixed: RefCell<Vec<ObjId>>,
}

// ------------------------------------------------------------------------------------ GC and heap hooks

pub(crate) fn roots(d: &Dom, out: &mut Vec<Value>) {
    out.extend(d.attr.fixed.borrow().iter().map(|&o| Value::Object(o)));
}

pub(crate) fn trace(d: &Dom, owner: ObjId, out: &mut Vec<Value>) {
    let t = d.attr.s.borrow();
    if t.attrs.is_empty() && t.maps.is_empty() {
        return;
    }
    let st = d.st.borrow();
    if let Some(&el) = t.maps.get(&owner) {
        out.extend(st.by_node.get(&el).map(|&o| Value::Object(o)));
    } else if let Some(a) = t.attrs.get(&owner) {
        out.extend(a.owner.and_then(|el| st.by_node.get(&el)).map(|&o| Value::Object(o)));
    } else if let Some(&Target::Node(n)) = st.by_obj.get(&owner) {
        out.extend(t.map_of.get(&n).map(|&o| Value::Object(o)));
        out.extend(t.by_el.get(&n).into_iter().flatten().map(|&(_, o)| Value::Object(o)));
    }
}

pub(crate) fn prune(d: &Dom, live: &dyn Fn(ObjId) -> bool) {
    let mut t = d.attr.s.borrow_mut();
    let t = &mut *t;
    if t.attrs.is_empty() && t.maps.is_empty() {
        return;
    }
    let mut unwatch = Vec::new();
    t.attrs.retain(|&o, a| {
        let keep = live(o);
        if !keep && let Some(el) = a.owner {
            unwatch.push((el, a.atom));
        }
        keep
    });
    let attrs = &t.attrs;
    t.by_el.retain(|_, l| {
        l.retain(|(_, o)| attrs.contains_key(o));
        !l.is_empty()
    });
    t.maps.retain(|&o, _| live(o));
    let maps = &t.maps;
    t.map_of.retain(|_, o| maps.contains_key(o));
    if !unwatch.is_empty() {
        let mut doc = d.doc.borrow_mut();
        for (el, atom) in unwatch {
            doc.unwatch_attr(el, atom);
        }
    }
}

impl IndexedSource for Dom {
    fn indexed_len(&self, owner: ObjId) -> Result<usize, HeapError> {
        if let Some(r) = crate::browser_nodelist::len(self, owner) {
            return r;
        }
        let t = self.attr.s.try_borrow().map_err(|_| HeapError::HostUnavailable)?;
        let Some(&el) = t.maps.get(&owner) else { return Ok(0) };
        Ok(self.doc.try_borrow().map_err(|_| HeapError::HostUnavailable)?.attr_count(el))
    }

    fn indexed_item(&self, owner: ObjId, index: usize) -> Result<Slot, HeapError> {
        if let Some(r) = crate::browser_nodelist::item(self, owner, index) {
            return r;
        }
        let t = self.attr.s.try_borrow().map_err(|_| HeapError::HostUnavailable)?;
        let Some(&el) = t.maps.get(&owner) else { return Ok(Slot::Absent) };
        let doc = self.doc.try_borrow().map_err(|_| HeapError::HostUnavailable)?;
        let NodeData::Element { attrs, .. } = *doc.data(el) else { return Ok(Slot::Absent) };
        let Some(a) = doc.attrs(attrs).get(index) else { return Ok(Slot::Absent) };
        // A removal is waiting in the log: the table can still hold the `Attr` of an attribute that was removed and added back.
        // `fill_indexed` drains the log, then the read repeats.
        if doc.has_removed_attrs() {
            return Ok(Slot::Missing);
        }
        Ok(match t.by_el.get(&el).and_then(|l| l.iter().find(|(x, _)| *x == a.name)) {
            Some(&(_, o)) => Slot::Ready(o),
            None => Slot::Missing,
        })
    }
}

/// `Host::fill_indexed`: make the `Attr` of attribute `index` of the map `owner`.
pub(crate) fn fill(d: &Dom, it: &mut Interp<'_>, owner: ObjId, index: u32) -> R<()> {
    drain(d);
    let Some(&el) = d.attr.s.borrow().maps.get(&owner) else { return Ok(()) };
    let atom = {
        let doc = d.doc.borrow();
        match *doc.data(el) {
            NodeData::Element { attrs, .. } => doc.attrs(attrs).get(index as usize).map(|a| a.name),
            _ => None,
        }
    };
    if let Some(a) = atom {
        wrapper_for(d, it, el, a)?;
    }
    Ok(())
}

// ------------------------------------------------------------------------------------ tables

/// Apply the removals that the document logged: the `Attr` of each becomes detached and keeps its value.
fn drain(d: &Dom) {
    if !d.doc.borrow().has_removed_attrs() {
        return;
    }
    let log = d.doc.borrow_mut().take_removed_attrs();
    let mut t = d.attr.s.borrow_mut();
    for RemovedAttr { el, name, value } in log {
        let Some(list) = t.by_el.get_mut(&el) else { continue };
        let Some(pos) = list.iter().position(|(a, _)| *a == name) else { continue };
        let (_, o) = list.swap_remove(pos);
        if list.is_empty() {
            t.by_el.remove(&el);
        }
        if let Some(a) = t.attrs.get_mut(&o) {
            a.owner = None;
            a.value = Some(value);
        }
    }
}

fn protos(d: &Dom) -> (ObjId, ObjId) {
    *d.attr.protos.get().expect("installed")
}

/// Room for one more `Attr` or map object, before it is made.
fn room(d: &Dom) -> R<()> {
    let t = d.attr.s.borrow();
    if d.st.borrow().by_obj.len() + t.attrs.len() + t.maps.len() + d.nl_count() >= d.lim.max_wrappers {
        return Err(d.lim("dom-wrappers"));
    }
    Ok(())
}

/// The `Attr` of the attached attribute `atom` of `el`: the same object while it is attached.
fn wrapper_for(d: &Dom, it: &mut Interp<'_>, el: NodeId, atom: Atom) -> R<ObjId> {
    drain(d);
    if let Some(&(_, o)) = d.attr.s.borrow().by_el.get(&el).and_then(|l| l.iter().find(|(a, _)| *a == atom)) {
        return Ok(o);
    }
    room(d)?;
    let o = d.obj(it, protos(d).0)?;
    {
        let mut t = d.attr.s.borrow_mut();
        t.attrs.insert(o, AttrObj { atom, owner: Some(el), value: None });
        t.by_el.entry(el).or_default().push((atom, o));
    }
    d.doc.borrow_mut().watch_attr(el, atom);
    Ok(o)
}

/// A new detached `Attr` with this name and value.
fn new_detached(d: &Dom, it: &mut Interp<'_>, atom: Atom, value: Span) -> R<ObjId> {
    room(d)?;
    let o = d.obj(it, protos(d).0)?;
    d.attr.s.borrow_mut().attrs.insert(o, AttrObj { atom, owner: None, value: Some(value) });
    Ok(o)
}

/// The `Attr` object `this`, after the removal log is applied.
fn attr_of(d: &Dom, it: &mut Interp<'_>, this: Value) -> R<ObjId> {
    if let Value::Object(o) = this {
        drain(d);
        if d.attr.s.borrow().attrs.contains_key(&o) {
            return Ok(o);
        }
    }
    Err(it.type_error("Illegal invocation"))
}

/// `v` as an `Attr` of this host, with the log of removed attributes applied first (so `state` is current).
pub(crate) fn probe(d: &Dom, v: Value) -> Option<ObjId> {
    let Value::Object(o) = v else { return None };
    drain(d);
    d.attr.s.borrow().attrs.contains_key(&o).then_some(o)
}

/// An argument that must be an `Attr` of this host.
fn attr_arg(d: &Dom, it: &mut Interp<'_>, a: &[Value], i: usize) -> R<ObjId> {
    if let Value::Object(o) = arg(a, i) {
        drain(d);
        if d.attr.s.borrow().attrs.contains_key(&o) {
            return Ok(o);
        }
    }
    Err(it.type_error(&format!("parameter {} is not of type 'Attr'", i + 1)))
}

pub(crate) fn state(d: &Dom, o: ObjId) -> (Atom, Option<NodeId>, Option<Span>) {
    let t = d.attr.s.borrow();
    let a = &t.attrs[&o];
    (a.atom, a.owner, a.value)
}

/// The value of the `Attr` `o` now.
pub(crate) fn value_of(d: &Dom, o: ObjId) -> String {
    let (atom, owner, span) = state(d, o);
    let doc = d.doc.borrow();
    match (owner, span) {
        (Some(el), _) => doc.attr(el, atom).unwrap_or("").to_string(),
        (None, Some(s)) => doc.text(s).to_string(),
        (None, None) => String::new(),
    }
}

fn map_el(d: &Dom, it: &mut Interp<'_>, this: Value) -> R<NodeId> {
    if let Value::Object(o) = this
        && let Some(&el) = d.attr.s.borrow().maps.get(&o)
    {
        return Ok(el);
    }
    Err(it.type_error("Illegal invocation"))
}

// ------------------------------------------------------------------------------------ Element

pub(crate) fn attributes(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let el = d.el(it, this)?;
    if let Some(&o) = d.attr.s.borrow().map_of.get(&el) {
        return Ok(Value::Object(o));
    }
    room(d)?;
    let o = it.heap.new_indexed(Some(protos(d).1)).map_err(|e| it.heap_error(e))?;
    let mut t = d.attr.s.borrow_mut();
    t.maps.insert(o, el);
    t.map_of.insert(el, o);
    Ok(Value::Object(o))
}

pub(crate) fn has_attributes(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let el = d.el(it, this)?;
    Ok(Value::Bool(d.doc.borrow().attr_count(el) != 0))
}

/// The `Attr` for the first attribute with the qualified name `name`, or `null`.
fn by_name(d: &Dom, it: &mut Interp<'_>, el: NodeId, name: &str) -> V {
    let atom = {
        let doc = d.doc.borrow();
        it.charge(doc.attr_count(el) as u64 + 1)?;
        doc.find_attr_qname(el, &ns::attr_key(&doc, el, name))
    };
    match atom {
        Some(a) => Ok(Value::Object(wrapper_for(d, it, el, a)?)),
        None => Ok(Value::Null),
    }
}

/// The attribute atom for a namespace and a local name. `None` also for a namespace that no attribute can have.
fn atom_ns(d: &Dom, it: &mut Interp<'_>, el: NodeId, namespace: Option<&str>, local: &str) -> R<Option<Atom>> {
    let doc = d.doc.borrow();
    it.charge(doc.attr_count(el) as u64 + 1)?;
    Ok(match ns::attr_ns_of(namespace) {
        Ok(kind) => doc.find_attr_ns(el, kind, local),
        Err(()) => None,
    })
}

fn by_ns(d: &Dom, it: &mut Interp<'_>, el: NodeId, a: &[Value]) -> V {
    let namespace = ns::ns_arg(d, it, arg(a, 0))?;
    let local = d.string_arg(it, arg(a, 1))?;
    match atom_ns(d, it, el, namespace.as_deref(), &local)? {
        Some(at) => Ok(Value::Object(wrapper_for(d, it, el, at)?)),
        None => Ok(Value::Null),
    }
}

pub(crate) fn get_attribute_node(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let el = d.el(it, this)?;
    let name = d.string_arg(it, arg(a, 0))?;
    by_name(d, it, el, &name)
}

pub(crate) fn get_attribute_node_ns(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let el = d.el(it, this)?;
    by_ns(d, it, el, a)
}

/// Remove the attribute `atom` of `el` and give its `Attr`, detached with the old value.
fn remove_atom(d: &Dom, it: &mut Interp<'_>, el: NodeId, atom: Atom) -> V {
    let o = wrapper_for(d, it, el, atom)?;
    // The document logs the removal of a watched attribute; `drain` then detaches `o` with the old value.
    d.doc.borrow_mut().remove_attr_atom(el, atom);
    drain(d);
    Ok(Value::Object(o))
}

pub(crate) fn remove_attribute_node(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let el = d.el(it, this)?;
    let o = attr_arg(d, it, a, 0)?;
    let (atom, owner, _) = state(d, o);
    if owner != Some(el) {
        return Err(d.dom_err(it, "NotFoundError", "Failed to execute 'removeAttributeNode' on 'Element': The node provided is not an attribute of this element."));
    }
    remove_atom(d, it, el, atom)
}

/// `setAttributeNode`: attach the detached `Attr` `o` to `el`. Gives the `Attr` that it replaced, or `null`.
fn set_node(d: &Dom, it: &mut Interp<'_>, el: NodeId, o: ObjId) -> V {
    let (atom, owner, span) = state(d, o);
    match owner {
        Some(e) if e == el => return Ok(Value::Object(o)),
        Some(_) => return Err(d.dom_err(it, "InUseAttributeError", "The attribute is in use by another element.")),
        None => {}
    }
    let (kind, local, qname, value, existing) = {
        let doc = d.doc.borrow();
        let kind = doc.attr_ns(atom);
        let local = doc.attr_local(atom).to_string();
        let existing = doc.find_attr_ns(el, kind, &local);
        (kind, local, doc.atom_str(atom).to_string(), span.map_or(String::new(), |s| doc.text(s).to_string()), existing)
    };
    // The replaced attribute: its `Attr` is made first (it is the result), and its value is read before the write.
    let old = match existing {
        Some(old_atom) if old_atom != atom => {
            return Err(Error::Unsupported(unsupported("setAttributeNode of an attribute with the namespace and local name of an existing one but another prefix")));
        }
        Some(_) => {
            let old = wrapper_for(d, it, el, atom)?;
            let old_span = d.doc.borrow().attr_span(el, atom);
            Some((old, old_span))
        }
        None => None,
    };
    ns::put(d, it, el, Some(atom), None, &qname, &value)?;
    {
        let mut t = d.attr.s.borrow_mut();
        if let Some((old, old_span)) = old {
            if let Some(list) = t.by_el.get_mut(&el) {
                list.retain(|(_, x)| *x != old);
            }
            if let Some(a) = t.attrs.get_mut(&old) {
                a.owner = None;
                a.value = old_span;
            }
        }
        if let Some(a) = t.attrs.get_mut(&o) {
            a.owner = Some(el);
            a.value = None;
        }
        t.by_el.entry(el).or_default().push((atom, o));
    }
    d.doc.borrow_mut().watch_attr(el, atom);
    if kind.is_none() && local == "src" {
        d.script_src_changed(it, el)?;
    }
    Ok(old.map_or(Value::Null, |(o, _)| Value::Object(o)))
}

pub(crate) fn set_attribute_node(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let el = d.el(it, this)?;
    let o = attr_arg(d, it, a, 0)?;
    set_node(d, it, el, o)
}

// ------------------------------------------------------------------------------------ NamedNodeMap

pub(crate) fn map_length(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let el = map_el(d, it, this)?;
    Ok(Value::Number(d.doc.borrow().attr_count(el) as f64))
}

pub(crate) fn map_item(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let el = map_el(d, it, this)?;
    // WebIDL `unsigned long`: ToNumber, then modulo 2^32.
    let n = it.to_js_number(arg(a, 0))?;
    let i = if n.is_finite() { (n.trunc().rem_euclid(4294967296.0)) as u64 } else { 0 };
    let atom = {
        let doc = d.doc.borrow();
        match *doc.data(el) {
            NodeData::Element { attrs, .. } => doc.attrs(attrs).get(i as usize).map(|x| x.name),
            _ => None,
        }
    };
    match atom {
        Some(at) => Ok(Value::Object(wrapper_for(d, it, el, at)?)),
        None => Ok(Value::Null),
    }
}

pub(crate) fn get_named_item(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let el = map_el(d, it, this)?;
    let name = d.string_arg(it, arg(a, 0))?;
    by_name(d, it, el, &name)
}

pub(crate) fn get_named_item_ns(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let el = map_el(d, it, this)?;
    by_ns(d, it, el, a)
}

pub(crate) fn set_named_item(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let el = map_el(d, it, this)?;
    let o = attr_arg(d, it, a, 0)?;
    set_node(d, it, el, o)
}

pub(crate) fn remove_named_item(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let el = map_el(d, it, this)?;
    let name = d.string_arg(it, arg(a, 0))?;
    let atom = {
        let doc = d.doc.borrow();
        it.charge(doc.attr_count(el) as u64 + 1)?;
        doc.find_attr_qname(el, &ns::attr_key(&doc, el, &name))
    };
    match atom {
        Some(at) => remove_atom(d, it, el, at),
        None => Err(d.dom_err(it, "NotFoundError", &format!("Failed to execute 'removeNamedItem' on 'NamedNodeMap': No item with name '{name}' was found."))),
    }
}

pub(crate) fn remove_named_item_ns(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let el = map_el(d, it, this)?;
    let namespace = ns::ns_arg(d, it, arg(a, 0))?;
    let local = d.string_arg(it, arg(a, 1))?;
    match atom_ns(d, it, el, namespace.as_deref(), &local)? {
        Some(at) => remove_atom(d, it, el, at),
        None => Err(d.dom_err(it, "NotFoundError", &format!("Failed to execute 'removeNamedItemNS' on 'NamedNodeMap': No item with name '{local}' was found."))),
    }
}

// ------------------------------------------------------------------------------------ Document

pub(crate) fn create_attribute(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    d.nd(it, this)?;
    let name = d.string_arg(it, arg(a, 0))?;
    if !valid_name(&name) {
        return Err(d.dom_err(it, "InvalidCharacterError", &format!("'{name}' is not a valid attribute name.")));
    }
    // The document is an HTML document: the name is lowercased, as `setAttribute` does for an HTML element.
    make_detached(d, it, None, &name.to_ascii_lowercase())
}

pub(crate) fn create_attribute_ns(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    d.nd(it, this)?;
    let namespace = ns::ns_arg(d, it, arg(a, 0))?;
    let qname = d.string_arg(it, arg(a, 1))?;
    ns::extract(d, it, namespace.as_deref(), &qname)?;
    let Ok(kind) = ns::attr_ns_of(namespace.as_deref()) else {
        return Err(Error::Unsupported(unsupported("createAttributeNS with a namespace other than none, XML, XMLNS or XLink")));
    };
    make_detached(d, it, kind, &qname)
}

fn make_detached(d: &Dom, it: &mut Interp<'_>, kind: Option<bare_html::dom::Ns>, qname: &str) -> V {
    room(d)?;
    let (atom, span) = {
        let mut doc = d.doc.borrow_mut();
        if doc.find_ns_atom(kind, qname).is_none() && doc.name_count() + 1 > d.lim.max_names {
            return Err(d.lim("dom-names"));
        }
        let atom = doc.intern_ns(kind, qname);
        (atom, doc.new_span(""))
    };
    Ok(Value::Object(new_detached(d, it, atom, span)?))
}

// ------------------------------------------------------------------------------------ Attr

fn atom_str(d: &Dom, it: &mut Interp<'_>, this: Value, f: impl Fn(&bare_html::dom::Document, Atom) -> Option<String>) -> V {
    let o = attr_of(d, it, this)?;
    let atom = state(d, o).0;
    let s = f(&d.doc.borrow(), atom);
    match s {
        Some(s) => d.js_str(it, &s),
        None => Ok(Value::Null),
    }
}

fn attr_name(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    atom_str(d, it, this, |doc, a| Some(doc.atom_str(a).to_string()))
}

fn attr_local_name(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    atom_str(d, it, this, |doc, a| Some(doc.attr_local(a).to_string()))
}

fn attr_prefix(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    atom_str(d, it, this, |doc, a| doc.attr_prefix(a).map(str::to_string))
}

fn attr_namespace_uri(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    atom_str(d, it, this, |doc, a| doc.attr_ns(a).map(|n| n.uri().to_string()))
}

fn attr_value_get(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let o = attr_of(d, it, this)?;
    let s = value_of(d, o);
    d.js_str(it, &s)
}

/// Set the value of an `Attr`: through the normal attribute path when attached, in the text buffer when detached.
fn set_value(d: &Dom, it: &mut Interp<'_>, this: Value, v: Value, null_is_empty: bool) -> V {
    let o = attr_of(d, it, this)?;
    let s = if null_is_empty && matches!(v, Value::Null) { String::new() } else { d.string_arg(it, v)? };
    // Converting `v` ran JS: it can have removed the attribute, so read the state again.
    drain(d);
    let (atom, owner, _) = state(d, o);
    match owner {
        Some(el) => {
            let (qname, kind, local) = {
                let doc = d.doc.borrow();
                (doc.atom_str(atom).to_string(), doc.attr_ns(atom), doc.attr_local(atom).to_string())
            };
            ns::put(d, it, el, Some(atom), None, &qname, &s)?;
            if kind.is_none() && local == "src" {
                d.script_src_changed(it, el)?;
            }
        }
        None => {
            let span = {
                let mut doc = d.doc.borrow_mut();
                d.room(&doc, 0, s.len())?;
                doc.new_span(&s)
            };
            d.attr.s.borrow_mut().attrs.get_mut(&o).expect("checked").value = Some(span);
        }
    }
    Ok(Value::Undefined)
}

fn attr_value_set(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    set_value(d, it, this, arg(a, 0), false)
}

fn attr_node_value_set(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    set_value(d, it, this, arg(a, 0), true)
}

fn attr_owner_element(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let o = attr_of(d, it, this)?;
    match state(d, o).1 {
        Some(el) => d.wrap(it, el),
        None => Ok(Value::Null),
    }
}

fn attr_owner_document(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    attr_of(d, it, this)?;
    let root = d.doc.borrow().root();
    d.wrap(it, root)
}

fn attr_node_type(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    attr_of(d, it, this)?;
    Ok(Value::Number(2.0))
}

fn attr_true(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    attr_of(d, it, this)?;
    Ok(Value::Bool(true))
}

fn attr_false(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    attr_of(d, it, this)?;
    Ok(Value::Bool(false))
}

fn attr_null(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    attr_of(d, it, this)?;
    Ok(Value::Null)
}

fn attr_no_children(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    attr_of(d, it, this)?;
    it.array_from(&[])
}

// ------------------------------------------------------------------------------------ install

pub(crate) fn install(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    let g = *d.global.get().expect("installed");
    let p = d.p();
    let op = it.object_prototype();
    let (attr, map) = (d.obj(it, p.node)?, d.obj(it, op)?);
    let _ = d.attr.protos.set((attr, map));
    let mut fixed = vec![attr, map];

    // Attr
    d.def_get(it, attr, "name", attr_name, None)?;
    d.def_get(it, attr, "localName", attr_local_name, None)?;
    d.def_get(it, attr, "prefix", attr_prefix, None)?;
    d.def_get(it, attr, "namespaceURI", attr_namespace_uri, None)?;
    d.def_get(it, attr, "value", attr_value_get, Some(attr_value_set))?;
    d.def_get(it, attr, "nodeValue", attr_value_get, Some(attr_node_value_set))?;
    d.def_get(it, attr, "textContent", attr_value_get, Some(attr_node_value_set))?;
    d.def_get(it, attr, "nodeName", attr_name, None)?;
    d.def_get(it, attr, "nodeType", attr_node_type, None)?;
    d.def_get(it, attr, "ownerElement", attr_owner_element, None)?;
    d.def_get(it, attr, "ownerDocument", attr_owner_document, None)?;
    d.def_get(it, attr, "specified", attr_true, None)?;
    d.def_get(it, attr, "isConnected", attr_false, None)?;
    for name in ["parentNode", "parentElement", "firstChild", "lastChild", "previousSibling", "nextSibling"] {
        d.def_get(it, attr, name, attr_null, None)?;
    }
    d.def_get(it, attr, "childNodes", attr_no_children, None)?;
    d.def_method(it, attr, "hasChildNodes", 0, attr_false)?;

    // NamedNodeMap
    d.def_get(it, map, "length", map_length, None)?;
    d.def_method(it, map, "item", 1, map_item)?;
    d.def_method(it, map, "getNamedItem", 1, get_named_item)?;
    d.def_method(it, map, "getNamedItemNS", 2, get_named_item_ns)?;
    d.def_method(it, map, "setNamedItem", 1, set_named_item)?;
    d.def_method(it, map, "setNamedItemNS", 1, set_named_item)?;
    d.def_method(it, map, "removeNamedItem", 1, remove_named_item)?;
    d.def_method(it, map, "removeNamedItemNS", 2, remove_named_item_ns)?;
    // `[Symbol.iterator]` is `Array.prototype[Symbol.iterator]`: the map has indices and a `length`.
    let sym = d.plat.sym_iter.get().copied().expect("platform installed first");
    let ap = it.array_prototype();
    let values = it.get_value(Value::Object(ap), sym)?;
    it.define_data(map, sym, values, true, false, true)?;

    let (kp, kc) = (d.key(it, "prototype")?, d.key(it, "constructor")?);
    for (name, proto) in [("Attr", attr), ("NamedNodeMap", map)] {
        let f = d.add_fn(it, illegal_constructor, name, 0, false)?;
        it.define_data(f, kp, Value::Object(proto), false, false, false)?;
        it.define_data(proto, kc, Value::Object(f), true, false, true)?;
        let k = d.key(it, name)?;
        it.define_data(g, k, Value::Object(f), true, false, true)?;
        fixed.push(f);
    }
    *d.attr.fixed.borrow_mut() = fixed;
    Ok(())
}

impl Dom {
    /// Is `o` an `Attr` or a `NamedNodeMap` of this module (so not plain data for structured clone)?
    pub(crate) fn is_attr_object(&self, o: ObjId) -> bool {
        let t = self.attr.s.borrow();
        t.attrs.contains_key(&o) || t.maps.contains_key(&o)
    }
}

/// Counters for tests and the load tool.
#[derive(Clone, Copy, Debug, Default)]
pub struct AttrStats {
    pub attrs: usize,
    pub maps: usize,
}

impl Browser {
    pub fn attr_stats(&self) -> AttrStats {
        let s = self.dom.attr.s.borrow();
        AttrStats { attrs: s.attrs.len(), maps: s.maps.len() }
    }
}
