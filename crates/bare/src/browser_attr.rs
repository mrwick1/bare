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
//! Named properties alias the live indexed Attrs. They are not enumerable and do not hide prototype properties.
//! - `setAttributeNode` of an `Attr` that has the namespace and local name of an existing attribute but another prefix is
//!   `Unsupported`: the native list would move the attribute to the end instead of replacing it in place.
//! - The other `Node` methods on an `Attr` (`cloneNode`, `appendChild`, events) throw `TypeError: Illegal
//!   invocation`. `new Attr()` and `new NamedNodeMap()` are illegal constructors. `toggleAttribute` is in `browser_custom.rs`.
//! - A JS string with a lone surrogate cannot be stored: `Unsupported` (the shared rule of the DOM host).
//!
//! Caps: `Attr` and map objects count with `max_wrappers` (checked before anything is made, `dom-wrappers`); values and new
//! names use the document caps (`dom-text`, `dom-attrs`, `dom-names`) through the same path as `setAttribute`. Scans charge
//! the step budget. No document or table borrow is held while JS runs (string conversion comes first).

use crate::browser_sources::Roots;
use crate::browser::*;
use crate::browser_scoped::*;
use bare_js_api::{Args, Local, Type};
use crate::browser_dom::{valid_name};
use crate::browser_namespace as ns;
use bare_html::dom::{Atom, NodeData, NodeId, RemovedAttr, Span};
use bare_js::eval::{Interp};
use bare_js::heap::{HeapError, Slot};
use bare_js::value::{ObjId, Value};
use std::cell::{Cell, OnceCell, RefCell};
use std::collections::{HashMap, HashSet};

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
    pub attrs: IdTab<ObjId, AttrObj>,
    /// The attached `Attr`s of an element, by name atom. A short list: an element has few attributes.
    pub by_el: HashMap<NodeId, Vec<(Atom, ObjId)>>,
    pub maps: IdMap<ObjId, NodeId>,
    pub map_of: HashMap<NodeId, ObjId>,
}

#[derive(Default)]
pub(crate) struct AttrHost {
    pub s: RefCell<Tables>,
    /// `Attr.prototype`, `NamedNodeMap.prototype`.
    pub protos: OnceCell<(ObjId, ObjId)>,
    pub fixed: RefCell<Vec<ObjId>>,
    scan_paid: Cell<bool>,
}

impl AttrHost {
    fn scan(&self, owner: ObjId, len: usize) -> Result<(), HeapError> {
        if len <= 32 || self.scan_paid.replace(false) {
            Ok(())
        } else {
            Err(HeapError::NeedsHost { owner, index: u32::MAX })
        }
    }
}

// ------------------------------------------------------------------------------------ GC and heap hooks

pub(crate) fn roots(d: &Dom, out: &mut Roots<'_, '_>) {
    out.extend(d.attr.fixed.borrow().iter().map(|&o| Value::Object(o)));
}

/// The references of the `Attr` object `o`: the wrapper of its element. The maps have owner entries (`trace_map`).
pub(crate) fn trace(d: &Dom, o: ObjId, t: &mut bare_js_api::Tracer<'_, Value>) {
    let a = d.attr.s.borrow();
    if let Some(a) = a.attrs.get(&o)
        && let Some(w) = a.owner.and_then(|el| d.st.borrow().by_node.get(&el).map(|w| *w.raw()))
    {
        crate::browser_sources::visit(t, w);
    }
}

/// The references of the attribute map `o` (its owner entry, `browser_sources.rs`): the wrapper of its element.
pub(crate) fn trace_map(d: &Dom, o: ObjId, t: &mut bare_js_api::Tracer<'_, Value>) {
    if let Some(&el) = d.attr.s.borrow().maps.get(&o)
        && let Some(w) = d.st.borrow().by_node.get(&el)
    {
        t.visit(w);
    }
}

/// The references of the wrapper of the element `n`: its attribute map and its `Attr` objects.
pub(crate) fn trace_node(d: &Dom, n: NodeId, t: &mut bare_js_api::Tracer<'_, Value>) {
    let a = d.attr.s.borrow();
    if a.map_of.is_empty() && a.by_el.is_empty() {
        return;
    }
    if let Some(&o) = a.map_of.get(&n) {
        crate::browser_sources::visit(t, Value::Object(o));
    }
    for &(_, o) in a.by_el.get(&n).into_iter().flatten() {
        crate::browser_sources::visit(t, Value::Object(o));
    }
}

/// Drop the entries of the attribute map `o`, which the collector freed.
pub(crate) fn prune_map(d: &Dom, o: ObjId) {
    let mut t = d.attr.s.borrow_mut();
    if let Some(el) = t.maps.remove(&o)
        && t.map_of.get(&el) == Some(&o)
    {
        t.map_of.remove(&el);
    }
}

/// The `Attr` objects `dead` were collected: drop their entries, and the watches of their elements.
pub(crate) fn forget(d: &Dom, dead: &[ObjId]) {
    let mut t = d.attr.s.borrow_mut();
    let t = &mut *t;
    let mut unwatch = Vec::new();
    for o in dead {
        if let Some(a) = t.attrs.remove(o)
            && let Some(el) = a.owner
        {
            unwatch.push((el, a.atom));
            if let Some(l) = t.by_el.get_mut(&el) {
                l.retain(|(_, x)| x != o);
                if l.is_empty() {
                    t.by_el.remove(&el);
                }
            }
        }
    }
    // Free the empty pages of the table (`IdTab::remove`).
    t.attrs.retain(|_, _| true);
    if !unwatch.is_empty() {
        let mut doc = d.doc.borrow_mut();
        for (el, atom) in unwatch {
            doc.unwatch_attr(el, atom);
        }
    }
}

/// The index of the attribute named `key` of the map `owner` (a non-enumerable alias). `browser_sources.rs` calls these by key.
pub(crate) fn name(d: &Dom, owner: ObjId, key: &[u16]) -> Result<Option<u32>, HeapError> {
    let t = d.attr.s.try_borrow().map_err(|_| HeapError::HostUnavailable)?;
    let Some(&el) = t.maps.get(&owner) else { return Ok(None) };
    let doc = d.doc.try_borrow().map_err(|_| HeapError::HostUnavailable)?;
    let NodeData::Element { attrs, .. } = *doc.data(el) else { return Ok(None) };
    if doc.element_ns(el) == bare_html::dom::Ns::Html && key.iter().any(|u| (65..=90).contains(u)) {
        return Ok(None);
    }
    d.attr.scan(owner, doc.attr_count(el))?;
    Ok(doc.attrs(attrs).iter().position(|a| doc.atom_str(a.name).encode_utf16().eq(key.iter().copied())).map(|i| i as u32))
}

/// The names of the map `owner`.
pub(crate) fn names(d: &Dom, owner: ObjId) -> Result<Vec<Vec<u16>>, HeapError> {
    let t = d.attr.s.try_borrow().map_err(|_| HeapError::HostUnavailable)?;
    let Some(&el) = t.maps.get(&owner) else { return Ok(Vec::new()) };
    let doc = d.doc.try_borrow().map_err(|_| HeapError::HostUnavailable)?;
    let NodeData::Element { attrs, .. } = *doc.data(el) else { return Ok(Vec::new()) };
    d.attr.scan(owner, doc.attr_count(el))?;
    let html = doc.element_ns(el) == bare_html::dom::Ns::Html;
    let mut seen = HashSet::new();
    Ok(doc.attrs(attrs).iter().filter_map(|a| {
        let name = doc.atom_str(a.name);
        (!html || !name.bytes().any(|b| b.is_ascii_uppercase())).then_some(name)
            .filter(|name| seen.insert(*name)).map(|name| name.encode_utf16().collect())
    }).collect())
}

/// The number of attributes of the map `owner`.
pub(crate) fn len(d: &Dom, owner: ObjId) -> Result<usize, HeapError> {
    let t = d.attr.s.try_borrow().map_err(|_| HeapError::HostUnavailable)?;
    let Some(&el) = t.maps.get(&owner) else { return Ok(0) };
    Ok(d.doc.try_borrow().map_err(|_| HeapError::HostUnavailable)?.attr_count(el))
}

/// The `Attr` of attribute `index` of the map `owner`.
pub(crate) fn item(d: &Dom, owner: ObjId, index: usize) -> Result<Slot, HeapError> {
    let t = d.attr.s.try_borrow().map_err(|_| HeapError::HostUnavailable)?;
    let Some(&el) = t.maps.get(&owner) else { return Ok(Slot::Absent) };
    let doc = d.doc.try_borrow().map_err(|_| HeapError::HostUnavailable)?;
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

/// `Host::fill_indexed`: make the `Attr` of attribute `index` of the map `owner`.
pub(crate) fn fill(d: &Dom, it: &mut Interp<'_>, owner: ObjId, index: u32) -> R<()> {
    drain(d);
    let Some(&el) = d.attr.s.borrow().maps.get(&owner) else { return Ok(()) };
    if index == u32::MAX {
        it.charge(d.doc.borrow().attr_count(el) as u64 + 1)?;
        d.attr.scan_paid.set(true);
        return Ok(());
    }
    let atom = {
        let doc = d.doc.borrow();
        match *doc.data(el) {
            NodeData::Element { attrs, .. } => doc.attrs(attrs).get(index as usize).map(|a| a.name),
            _ => None,
        }
    };
    if let Some(a) = atom {
        run(it, |s| wrapper_for(d, s, el, a))?;
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
fn wrapper_for<'s>(d: &Dom, s: &mut Sc<'s, '_>, el: NodeId, atom: Atom) -> Res<'s, ObjId> {
    drain(d);
    if let Some(&(_, o)) = d.attr.s.borrow().by_el.get(&el).and_then(|l| l.iter().find(|(a, _)| *a == atom)) {
        return Ok(o);
    }
    lift(s, room(d))?;
    let o = d.obj_s(s, protos(d).0)?;
    {
        let mut t = d.attr.s.borrow_mut();
        t.attrs.insert(o, AttrObj { atom, owner: Some(el), value: None });
        t.by_el.entry(el).or_default().push((atom, o));
        d.wrapper_edges.set(true);
    }
    d.own_obj(s.engine(), o, crate::browser::Owner::Attr(o));
    d.doc.borrow_mut().watch_attr(el, atom);
    Ok(o)
}

/// A new detached `Attr` with this name and value.
fn new_detached<'s>(d: &Dom, s: &mut Sc<'s, '_>, atom: Atom, value: Span) -> Res<'s, ObjId> {
    lift(s, room(d))?;
    let o = d.obj_s(s, protos(d).0)?;
    d.attr.s.borrow_mut().attrs.insert(o, AttrObj { atom, owner: None, value: Some(value) });
    d.own_obj(s.engine(), o, crate::browser::Owner::Attr(o));
    Ok(o)
}

/// The `Attr` object `this`, after the removal log is applied.
fn attr_of<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>) -> Res<'s, ObjId> {
    if let Some(o) = obj_id(s, this) {
        drain(d);
        if d.attr.s.borrow().attrs.contains_key(&o) {
            return Ok(o);
        }
    }
    Err(s.type_error("Illegal invocation"))
}

/// `o` as an `Attr` of this host, with the log of removed attributes applied first (so `state` is current).
pub(crate) fn probe_obj(d: &Dom, o: ObjId) -> Option<ObjId> {
    drain(d);
    d.attr.s.borrow().attrs.contains_key(&o).then_some(o)
}

/// An argument that must be an `Attr` of this host.
fn attr_arg<'s>(d: &Dom, s: &mut Sc<'s, '_>, a: Args<'s>, i: usize) -> Res<'s, ObjId> {
    let argi = arg_s(s, a, i);
    if let Some(o) = obj_id(s, argi) {
        drain(d);
        if d.attr.s.borrow().attrs.contains_key(&o) {
            return Ok(o);
        }
    }
    Err(s.type_error(&format!("parameter {} is not of type 'Attr'", i + 1)))
}

pub(crate) fn state(d: &Dom, o: ObjId) -> (Atom, Option<NodeId>, Option<Span>) {
    let t = d.attr.s.borrow();
    let a = t.attrs.get(&o).expect("checked");
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

fn map_el<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>) -> Res<'s, NodeId> {
    if let Some(o) = obj_id(s, this)
        && let Some(&el) = d.attr.s.borrow().maps.get(&o)
    {
        return Ok(el);
    }
    Err(s.type_error("Illegal invocation"))
}

// ------------------------------------------------------------------------------------ Element

pub(crate) fn attributes<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let el = d.el_s(s, this)?;
    if let Some(&o) = d.attr.s.borrow().map_of.get(&el) {
        return Ok(d.obj_local(s, o));
    }
    lift(s, room(d))?;
    let o = d.new_source_s(s, crate::browser_sources::Source::AttrMap, Some(protos(d).1))?;
    let mut t = d.attr.s.borrow_mut();
    t.maps.insert(o, el);
    t.map_of.insert(el, o);
    Ok(d.obj_local(s, o))
}

pub(crate) fn has_attributes<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let el = d.el_s(s, this)?;
    Ok(s.boolean(d.doc.borrow().attr_count(el) != 0))
}

/// The `Attr` for the first attribute with the qualified name `name`, or `null`.
fn by_name<'s>(d: &Dom, s: &mut Sc<'s, '_>, el: NodeId, name: &str) -> Res<'s, Local<'s>> {
    let atom = {
        let doc = d.doc.borrow();
        s.charge(doc.attr_count(el) as u64 + 1)?;
        doc.find_attr_qname(el, &ns::attr_key(&doc, el, name))
    };
    match atom {
        Some(a) => {
            let o = wrapper_for(d, s, el, a)?;
            Ok(d.obj_local(s, o))
        }
        None => Ok(s.null()),
    }
}

/// The attribute atom for a namespace and a local name. `None` also for a namespace that no attribute can have.
fn atom_ns<'s>(d: &Dom, s: &mut Sc<'s, '_>, el: NodeId, namespace: Option<&str>, local: &str) -> Res<'s, Option<Atom>> {
    let doc = d.doc.borrow();
    s.charge(doc.attr_count(el) as u64 + 1)?;
    Ok(match ns::attr_ns_of(namespace) {
        Ok(kind) => doc.find_attr_ns(el, kind, local),
        Err(()) => None,
    })
}

fn by_ns<'s>(d: &Dom, s: &mut Sc<'s, '_>, el: NodeId, a: Args<'s>) -> Res<'s, Local<'s>> {
    let arg0 = arg_s(s, a, 0);
    let namespace = ns::ns_arg(d, s, arg0)?;
    let local = d.arg_str_s(s, a, 1)?;
    match atom_ns(d, s, el, namespace.as_deref(), &local)? {
        Some(at) => {
            let o = wrapper_for(d, s, el, at)?;
            Ok(d.obj_local(s, o))
        }
        None => Ok(s.null()),
    }
}

pub(crate) fn get_attribute_node<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let el = d.el_s(s, this)?;
    let name = d.arg_str_s(s, a, 0)?;
    by_name(d, s, el, &name)
}

pub(crate) fn get_attribute_node_ns<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let el = d.el_s(s, this)?;
    by_ns(d, s, el, a)
}

/// Remove the attribute `atom` of `el` and give its `Attr`, detached with the old value.
fn remove_atom<'s>(d: &Dom, s: &mut Sc<'s, '_>, el: NodeId, atom: Atom) -> Res<'s, Local<'s>> {
    let o = wrapper_for(d, s, el, atom)?;
    // The document logs the removal of a watched attribute; `drain` then detaches `o` with the old value.
    d.doc.borrow_mut().remove_attr_atom(el, atom);
    drain(d);
    Ok(d.obj_local(s, o))
}

pub(crate) fn remove_attribute_node<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let el = d.el_s(s, this)?;
    let o = attr_arg(d, s, a, 0)?;
    let (atom, owner, _) = state(d, o);
    if owner != Some(el) {
        return Err(d.dom_err_s(s, "NotFoundError", "Failed to execute 'removeAttributeNode' on 'Element': The node provided is not an attribute of this element."));
    }
    remove_atom(d, s, el, atom)
}

/// `setAttributeNode`: attach the detached `Attr` `o` to `el`. Gives the `Attr` that it replaced, or `null`.
fn set_node<'s>(d: &Dom, s: &mut Sc<'s, '_>, el: NodeId, o: ObjId) -> Res<'s, Local<'s>> {
    let (atom, owner, span) = state(d, o);
    match owner {
        Some(e) if e == el => return Ok(d.obj_local(s, o)),
        Some(_) => return Err(d.dom_err_s(s, "InUseAttributeError", "The attribute is in use by another element.")),
        None => {}
    }
    let (kind, local, qname, value, existing) = {
        let doc = d.doc.borrow();
        let kind = doc.attr_ns(atom);
        let local = doc.attr_local(atom).to_string();
        let existing = doc.find_attr_ns(el, kind, &local);
        (kind, local, doc.atom_str(atom).to_string(), span.map_or(String::new(), |st| doc.text(st).to_string()), existing)
    };
    // The replaced attribute: its `Attr` is made first (it is the result), and its value is read before the write.
    let prev = match existing {
        Some(old_atom) if old_atom != atom => {
            return Err(unsupp("setAttributeNode of an attribute with the namespace and local name of an existing one but another prefix"));
        }
        Some(_) => {
            let old = wrapper_for(d, s, el, atom)?;
            let old_span = d.doc.borrow().attr_span(el, atom);
            Some((old, old_span))
        }
        None => None,
    };
    ns_put_s(d, s, el, Some(atom), &qname, &value)?;
    {
        let mut t = d.attr.s.borrow_mut();
        if let Some((old, old_span)) = prev {
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
        d.wrapper_edges.set(true);
    }
    d.doc.borrow_mut().watch_attr(el, atom);
    if kind.is_none() && local == "src" {
        script_src_changed_s(d, s, el)?;
    }
    match prev {
        Some((o, _)) => Ok(d.obj_local(s, o)),
        None => Ok(s.null()),
    }
}

pub(crate) fn set_attribute_node<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let el = d.el_s(s, this)?;
    let o = attr_arg(d, s, a, 0)?;
    set_node(d, s, el, o)
}

// ------------------------------------------------------------------------------------ NamedNodeMap

pub(crate) fn map_length<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let el = map_el(d, s, this)?;
    Ok(s.number(d.doc.borrow().attr_count(el) as f64))
}

pub(crate) fn map_item<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let el = map_el(d, s, this)?;
    // WebIDL `unsigned long`: ToNumber, then modulo 2^32.
    let arg0 = arg_s(s, a, 0);
    let n = s.to_number(arg0)?;
    let i = if n.is_finite() { (n.trunc().rem_euclid(4294967296.0)) as u64 } else { 0 };
    let atom = {
        let doc = d.doc.borrow();
        match *doc.data(el) {
            NodeData::Element { attrs, .. } => doc.attrs(attrs).get(i as usize).map(|x| x.name),
            _ => None,
        }
    };
    match atom {
        Some(at) => {
            let o = wrapper_for(d, s, el, at)?;
            Ok(d.obj_local(s, o))
        }
        None => Ok(s.null()),
    }
}

pub(crate) fn get_named_item<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let el = map_el(d, s, this)?;
    let name = d.arg_str_s(s, a, 0)?;
    by_name(d, s, el, &name)
}

pub(crate) fn get_named_item_ns<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let el = map_el(d, s, this)?;
    by_ns(d, s, el, a)
}

pub(crate) fn set_named_item<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let el = map_el(d, s, this)?;
    let o = attr_arg(d, s, a, 0)?;
    set_node(d, s, el, o)
}

pub(crate) fn remove_named_item<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let el = map_el(d, s, this)?;
    let name = d.arg_str_s(s, a, 0)?;
    let atom = {
        let doc = d.doc.borrow();
        s.charge(doc.attr_count(el) as u64 + 1)?;
        doc.find_attr_qname(el, &ns::attr_key(&doc, el, &name))
    };
    match atom {
        Some(at) => remove_atom(d, s, el, at),
        None => Err(d.dom_err_s(s, "NotFoundError", &format!("Failed to execute 'removeNamedItem' on 'NamedNodeMap': No item with name '{name}' was found."))),
    }
}

pub(crate) fn remove_named_item_ns<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let el = map_el(d, s, this)?;
    let arg0 = arg_s(s, a, 0);
    let namespace = ns::ns_arg(d, s, arg0)?;
    let local = d.arg_str_s(s, a, 1)?;
    match atom_ns(d, s, el, namespace.as_deref(), &local)? {
        Some(at) => remove_atom(d, s, el, at),
        None => Err(d.dom_err_s(s, "NotFoundError", &format!("Failed to execute 'removeNamedItemNS' on 'NamedNodeMap': No item with name '{local}' was found."))),
    }
}

// ------------------------------------------------------------------------------------ Document

pub(crate) fn create_attribute<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    d.nd_s(s, this)?;
    let name = d.arg_str_s(s, a, 0)?;
    if !valid_name(&name) {
        return Err(d.dom_err_s(s, "InvalidCharacterError", &format!("'{name}' is not a valid attribute name.")));
    }
    // The document is an HTML document: the name is lowercased, as `setAttribute` does for an HTML element.
    make_detached(d, s, None, &name.to_ascii_lowercase())
}

pub(crate) fn create_attribute_ns<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    d.nd_s(s, this)?;
    let arg0 = arg_s(s, a, 0);
    let namespace = ns::ns_arg(d, s, arg0)?;
    let qname = d.arg_str_s(s, a, 1)?;
    ns::extract(d, s, namespace.as_deref(), &qname)?;
    let Ok(kind) = ns::attr_ns_of(namespace.as_deref()) else {
        return Err(unsupp("createAttributeNS with a namespace other than none, XML, XMLNS or XLink"));
    };
    make_detached(d, s, kind, &qname)
}

fn make_detached<'s>(d: &Dom, s: &mut Sc<'s, '_>, kind: Option<bare_html::dom::Ns>, qname: &str) -> Res<'s, Local<'s>> {
    lift(s, room(d))?;
    let (atom, span) = {
        let mut doc = d.doc.borrow_mut();
        if doc.find_ns_atom(kind, qname).is_none() && doc.name_count() + 1 > d.lim.max_names {
            return Err(d.lim_s(s, "dom-names"));
        }
        let atom = doc.intern_ns(kind, qname);
        (atom, doc.new_span(""))
    };
    let o = new_detached(d, s, atom, span)?;
    Ok(d.obj_local(s, o))
}

// ------------------------------------------------------------------------------------ Attr

fn atom_str<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, f: impl Fn(&bare_html::dom::Document, Atom) -> Option<String>) -> Res<'s, Local<'s>> {
    let o = attr_of(d, s, this)?;
    let atom = state(d, o).0;
    let st = f(&d.doc.borrow(), atom);
    match st {
        Some(st) => d.js_str_s(s, &st),
        None => Ok(s.null()),
    }
}

fn attr_name<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    atom_str(d, s, this, |doc, a| Some(doc.atom_str(a).to_string()))
}

fn attr_local_name<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    atom_str(d, s, this, |doc, a| Some(doc.attr_local(a).to_string()))
}

fn attr_prefix<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    atom_str(d, s, this, |doc, a| doc.attr_prefix(a).map(str::to_string))
}

fn attr_namespace_uri<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    atom_str(d, s, this, |doc, a| doc.attr_ns(a).map(|n| n.uri().to_string()))
}

fn attr_value_get<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let o = attr_of(d, s, this)?;
    let st = value_of(d, o);
    d.js_str_s(s, &st)
}

/// Set the value of an `Attr`: through the normal attribute path when attached, in the text buffer when detached.
fn set_value<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, v: Local<'s>, null_is_empty: bool) -> Res<'s, Local<'s>> {
    let o = attr_of(d, s, this)?;
    let st = if null_is_empty && matches!(s.type_of(v), Type::Null) { String::new() } else { d.string_arg_s(s, v)? };
    // Converting `v` ran JS: it can have removed the attribute, so read the state again.
    drain(d);
    let (atom, owner, _) = state(d, o);
    match owner {
        Some(el) => {
            let (qname, kind, local) = {
                let doc = d.doc.borrow();
                (doc.atom_str(atom).to_string(), doc.attr_ns(atom), doc.attr_local(atom).to_string())
            };
            ns_put_s(d, s, el, Some(atom), &qname, &st)?;
            if kind.is_none() && local == "src" {
                script_src_changed_s(d, s, el)?;
            }
        }
        None => {
            let span = {
                let mut doc = d.doc.borrow_mut();
                d.room_s(s, &doc, 0, st.len())?;
                doc.new_span(&st)
            };
            d.attr.s.borrow_mut().attrs.get_mut(&o).expect("checked").value = Some(span);
        }
    }
    Ok(s.undefined())
}

fn attr_value_set<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let arg0 = arg_s(s, a, 0);
    set_value(d, s, this, arg0, false)
}

fn attr_node_value_set<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let arg0 = arg_s(s, a, 0);
    set_value(d, s, this, arg0, true)
}

fn attr_owner_element<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let o = attr_of(d, s, this)?;
    match state(d, o).1 {
        Some(el) => d.wrap_s(s, el),
        None => Ok(s.null()),
    }
}

fn attr_owner_document<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    attr_of(d, s, this)?;
    let root = d.doc.borrow().root();
    d.wrap_s(s, root)
}

fn attr_node_type<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    attr_of(d, s, this)?;
    Ok(s.number(2.0))
}

fn attr_true<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    attr_of(d, s, this)?;
    Ok(s.boolean(true))
}

fn attr_false<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    attr_of(d, s, this)?;
    Ok(s.boolean(false))
}

fn attr_null<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    attr_of(d, s, this)?;
    Ok(s.null())
}

fn attr_no_children<'s>(d: &Dom, s: &mut Sc<'s, '_>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    attr_of(d, s, this)?;
    s.new_array(&[])
}

// ------------------------------------------------------------------------------------ install

pub(crate) fn install<'s>(d: &Dom, s: &mut Sc<'s, '_>) -> Res<'s, ()> {
    let p = d.p();
    let (attr, map) = (d.obj_s(s, p.node)?, d.plain_s(s)?);
    let _ = d.attr.protos.set((attr, map));
    let mut fixed = vec![attr, map];

    // Attr
    table!(rows1 = [
        Row::get(1, "name", attr_name, None),
        Row::get(1, "localName", attr_local_name, None),
        Row::get(1, "prefix", attr_prefix, None),
        Row::get(1, "namespaceURI", attr_namespace_uri, None),
        Row::get(1, "value", attr_value_get, Some(attr_value_set)),
        Row::get(1, "nodeValue", attr_value_get, Some(attr_node_value_set)),
        Row::get(1, "textContent", attr_value_get, Some(attr_node_value_set)),
        Row::get(1, "nodeName", attr_name, None),
        Row::get(1, "nodeType", attr_node_type, None),
        Row::get(1, "ownerElement", attr_owner_element, None),
        Row::get(1, "ownerDocument", attr_owner_document, None),
        Row::get(1, "specified", attr_true, None),
        Row::get(1, "isConnected", attr_false, None),
    ]);
    let targets = [d.obj_local(s, attr)];
    d.install_rows(s, &targets, &rows1::T)?;
    for name in ["parentNode", "parentElement", "firstChild", "lastChild", "previousSibling", "nextSibling"] {
        d.def_get_at(s, attr, name, attr_null, None)?;
    }
    table!(rows2 = [
        Row::get(1, "childNodes", attr_no_children, None),
        Row::method(1, "hasChildNodes", 0, attr_false),
    ]);
    let targets = [d.obj_local(s, attr)];
    d.install_rows(s, &targets, &rows2::T)?;

    // NamedNodeMap
    table!(rows3 = [
        Row::get(1, "length", map_length, None),
        Row::method(1, "item", 1, map_item),
        Row::method(1, "getNamedItem", 1, get_named_item),
        Row::method(1, "getNamedItemNS", 2, get_named_item_ns),
        Row::method(1, "setNamedItem", 1, set_named_item),
        Row::method(1, "setNamedItemNS", 1, set_named_item),
        Row::method(1, "removeNamedItem", 1, remove_named_item),
        Row::method(1, "removeNamedItemNS", 2, remove_named_item_ns),
    ]);
    let targets = [d.obj_local(s, map)];
    d.install_rows(s, &targets, &rows3::T)?;
    // `[Symbol.iterator]` is `Array.prototype[Symbol.iterator]`: the map has indices and a `length`.
    let ap = s.intrinsic(Intrinsic::ArrayPrototype);
    let sym = s.intrinsic(Intrinsic::SymbolIterator);
    let values = s.get(ap, sym)?;
    let m = d.obj_local(s, map);
    s.define(m, sym, values, Attrs::METHOD)?;

    for (name, proto) in [("Attr", attr), ("NamedNodeMap", map)] {
        fixed.push(d.interface_s(s, proto, name, illegal_constructor, 0, HostFn::Method)?);
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

// ------------------------------------------------------------------------------------ arena reclaim (`browser_reclaim.rs`)

/// The nodes that `Attr` objects and `NamedNodeMap`s name.
pub(crate) fn held(d: &Dom, out: &mut Vec<NodeId>) {
    let t = d.attr.s.borrow();
    out.extend(t.attrs.iter().filter_map(|(_, a)| a.owner));
    out.extend(t.by_el.keys().copied());
    out.extend(t.maps.values().copied());
    out.extend(t.map_of.keys().copied());
}

/// The spans of detached `Attr` values: the reclaim moves them when it builds the text list again.
pub(crate) fn pins(d: &Dom, f: &mut dyn FnMut(&mut Span)) {
    for (_, a) in d.attr.s.borrow_mut().attrs.iter_mut() {
        if let Some(v) = a.value.as_mut() {
            f(v);
        }
    }
}
