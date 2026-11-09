//! Live named access to inert iframe windows. No child document is fetched or executed.
use crate::browser::*;
use bare_html::dom::{NodeData, NodeId};
use bare_js::eval::Interp;
use bare_js::heap::HeapError;
use bare_js::value::{ObjId, Value};
use std::cell::{Cell, RefCell};

const FILL: u32 = u32::MAX - 8;
#[derive(Default)]
pub(crate) struct Frames(RefCell<Option<Box<State>>>);
struct State {
    owner: ObjId,
    document_owner: ObjId,
    contexts: IdMap<NodeId, ObjId>,
    pending: Cell<Option<NodeId>>,
    // Prepay a batch: one VM property operation can probe the same prototype more than twice.
    paid: Cell<u8>,
}

fn nodes(d: &Dom) -> Vec<NodeId> {
    let doc = d.doc.borrow();
    let root = doc.root();
    let (mut cur, mut depth) = (root, 0);
    let mut out = Vec::new();
    while let Some((n, dep)) = crate::browser_dom::next_in(&doc, root, cur, depth) {
        (cur, depth) = (n, dep);
        if matches!(doc.data(n), NodeData::Element { .. }) && !doc.is_foreign(n) && doc.element_name(n) == "iframe" { out.push(n); }
    }
    out
}
fn find(d: &Dom, key: &[u16], document: bool) -> Option<NodeId> {
    let name = String::from_utf16(key).ok()?;
    if name.is_empty() { return None; }
    let list = nodes(d);
    if !document && let Ok(i) = name.parse::<usize>() && i.to_string() == name { return list.get(i).copied(); }
    let doc = d.doc.borrow();
    list.into_iter().find(|&n| ["name", "id"].into_iter().any(|a| doc.find_attr_qname(n, a).and_then(|a| doc.attr(n, a)) == Some(&name)))
}

/// Activate only after the document has seen an iframe. Ordinary realms keep their prototype chains.
pub(crate) fn activate(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    if d.frames.0.borrow().is_some() || d.doc.try_borrow().map_err(|_| it.heap_error(HeapError::HostUnavailable))?.find_atom("iframe").is_none() { return Ok(()); }
    for name in ["frames", "parent", "top", "length", "name", "screen"] { crate::browser::define_window(d, it, name)?; }
    let owner = it.heap.new_named(Some(d.p().et)).map_err(|e| it.heap_error(e))?;
    let document_owner = it.heap.new_named(Some(d.p().document)).map_err(|e| it.heap_error(e))?;
    let g = it.global_object();
    it.heap.set_prototype_of(g, Some(owner)).map_err(|e| it.heap_error(e))?;
    let doc = *d.doc_wrapper.get().expect("installed");
    it.heap.set_prototype_of(doc, Some(document_owner)).map_err(|e| it.heap_error(e))?;
    *d.frames.0.borrow_mut() = Some(Box::new(State { owner, document_owner, contexts: IdMap::default(), pending: Cell::new(None), paid: Cell::new(0) }));
    Ok(())
}

pub(crate) fn has(d: &Dom, owner: ObjId, key: &[u16]) -> Result<Option<bool>, HeapError> {
    let state = d.frames.0.borrow();
    let Some(s) = state.as_ref().filter(|s| owner == s.owner || owner == s.document_owner) else { return Ok(None); };
    if d.doc.borrow().len() > 32 {
        let paid = s.paid.get();
        if paid == 0 { return Err(HeapError::NeedsHost { owner, index: FILL }); }
        s.paid.set(paid - 1);
    }
    let Some(n) = find(d, key, owner == s.document_owner) else { return Ok(Some(false)); };
    if !s.contexts.contains_key(&n) {
        s.pending.set(Some(n));
        return Err(HeapError::NeedsHost { owner, index: FILL });
    }
    Ok(Some(true))
}
pub(crate) fn value(d: &Dom, owner: ObjId, key: &[u16]) -> Option<Value> {
    let state = d.frames.0.borrow();
    let s = state.as_ref().filter(|s| owner == s.owner || owner == s.document_owner)?;
    let n = find(d, key, owner == s.document_owner)?;
    if owner == s.document_owner { d.st.borrow().by_node.get(&n).copied().map(Value::Object) }
    else { s.contexts.get(&n).copied().map(Value::Object) }
}
pub(crate) fn len(d: &Dom, owner: ObjId) -> Result<Option<usize>, HeapError> {
    let state = d.frames.0.borrow();
    let Some(s) = state.as_ref().filter(|s| owner == s.owner || owner == s.document_owner) else { return Ok(None); };
    if d.doc.borrow().len() > 32 {
        let paid = s.paid.get();
        if paid == 0 { return Err(HeapError::NeedsHost { owner, index: FILL }); }
        s.paid.set(paid - 1);
    }
    Ok(keys(d, owner).map(|k| k.len()))
}
pub(crate) fn keys(d: &Dom, owner: ObjId) -> Option<Vec<Vec<u16>>> {
    let state = d.frames.0.borrow();
    let s = state.as_ref().filter(|s| owner == s.owner || owner == s.document_owner)?;
    let list = nodes(d);
    let doc = d.doc.borrow();
    let mut out = Vec::new();
    for (i, n) in list.into_iter().enumerate() {
        if owner == s.owner { out.push(i.to_string().encode_utf16().collect()); }
        for a in ["name", "id"] {
            if let Some(v) = doc.find_attr_qname(n, a).and_then(|a| doc.attr(n, a)).filter(|v| !v.is_empty()) {
                let k = v.encode_utf16().collect();
                if !out.contains(&k) { out.push(k); }
            }
        }
    }
    Some(out)
}
pub(crate) fn fill(d: &Dom, it: &mut Interp<'_>, owner: ObjId, index: u32) -> R<bool> {
    if index != FILL { return Ok(false); }
    let n = d.frames.0.borrow().as_ref().and_then(|s| s.pending.take());
    it.charge(d.doc.borrow().len() as u64 * 8)?;
    if let Some(n) = n { context(d, it, n)?; }
    if let Some(s) = d.frames.0.borrow().as_ref().filter(|s| owner == s.owner || owner == s.document_owner) { s.paid.set(8); }
    Ok(true)
}
fn context(d: &Dom, it: &mut Interp<'_>, n: NodeId) -> R<Value> {
    activate(d, it)?;
    if let Some(o) = d.frames.0.borrow().as_ref().and_then(|s| s.contexts.get(&n)).copied() { return Ok(Value::Object(o)); }
    if d.frames.0.borrow().as_ref().map_or(0, |s| s.contexts.len()) >= d.lim.max_wrappers { return Err(d.lim("dom-wrappers")); }
    let element = d.wrap(it, n)?;
    let o = d.obj(it, it.object_prototype())?;
    let g = Value::Object(it.global_object());
    for (name, v) in [("window", Value::Object(o)), ("self", Value::Object(o)), ("frames", Value::Object(o)), ("parent", g), ("top", g), ("length", Value::Number(0.0)), ("frameElement", element)] {
        let k = d.key(it, name)?;
        it.define_data(o, k, v, false, true, true)?;
    }
    d.frames.0.borrow_mut().as_mut().expect("activated").contexts.insert(n, o);
    Ok(Value::Object(o))
}
pub(crate) fn content_window(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> R<Value> {
    let n = d.el(it, this)?;
    if d.doc.borrow().element_name(n) != "iframe" { return Err(it.type_error("Illegal invocation")); }
    if !d.is_connected_node(it, n)? { return Ok(Value::Null); }
    context(d, it, n)
}
pub(crate) fn length(d: &Dom, it: &mut Interp<'_>, _: Value, _: &[Value]) -> R<Value> {
    it.charge(d.doc.borrow().len() as u64)?;
    Ok(Value::Number(nodes(d).len() as f64))
}
pub(crate) fn name(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> R<Value> {
    let n = d.el(it, this)?;
    match crate::browser_dom::attr_get(d, it, n, "name")? { Value::Null => d.js_str(it, ""), v => Ok(v) }
}
pub(crate) fn set_name(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> R<Value> {
    let n = d.el(it, this)?;
    let s = d.string_arg(it, a.first().copied().unwrap_or(Value::Undefined))?;
    crate::browser_dom::attr_set(d, it, n, "name", &s)?;
    Ok(Value::Undefined)
}
pub(crate) fn trace(d: &Dom, owner: ObjId, out: &mut Vec<Value>) {
    if let Some(s) = d.frames.0.borrow().as_ref() {
        for (&n, &o) in &s.contexts {
            if d.st.borrow().by_node.get(&n) == Some(&owner) { out.push(Value::Object(o)); }
        }
    }
}
pub(crate) fn prune(d: &Dom, live: &dyn Fn(ObjId) -> bool) {
    if let Some(s) = d.frames.0.borrow_mut().as_mut() { s.contexts.retain(|_, o| live(*o)); }
}
