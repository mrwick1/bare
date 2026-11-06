//! `Document.getElementsByName` and the live `NodeList` (HTML Standard "getElementsByName", DOM Standard "NodeList").
//!
//! Standards (read from the primary text): `getElementsByName(elementName)` takes one required `DOMString`. It returns a
//! live `NodeList` of the HTML elements (HTML namespace) of the document that have a `name` attribute whose value is
//! identical to the argument (case-sensitive, code unit for code unit, the empty string included), in tree order. A detached
//! node is not in the document. `NodeList` has a readonly `length`, `item(unsigned long)` (null when absent), an indexed
//! getter without a setter, and `iterable<Node>`: the WebIDL rule for a value iterator on an interface with an indexed getter
//! is that `forEach`, `entries`, `keys`, `values` and `[Symbol.iterator]` are the `Array.prototype` functions, so the
//! iteration follows the live list the way Array iteration follows any array-like.
//!
//! The native `Document` is the one source of truth. A list stores only its name; it never stores a node wrapper.
//! - A list is a host-indexed heap object (`Heap::new_indexed`, like `NamedNodeMap`). `length` and each index are answered
//!   by `Dom::indexed_len` / `indexed_item`, from a cache of native `NodeId`s that is valid for one `Document::revision`.
//! - The revision moves at every public native change that can alter a tree query (link, detach, attribute set or removal,
//!   a namespace). A change made by script, parser, import or a native host call therefore invalidates the cache the same way.
//! - The heap callbacks have no `Interp` and cannot charge, so they never scan. When the revision is not the cached one
//!   they answer `HeapError::NeedsHost`. The evaluator then calls `Host::fill_indexed` (`fill`), which runs `prepare`: it
//!   charges the steps for each node it walks (before the node is looked at), checks the result cap before each push
//!   (`dom-results`) and the cap of all cached ids (`dom-nodelist-nodes`) before the vector grows, and then stores the new
//!   ids with the revision. The same routine runs for the `length` getter and `item`. A failed rebuild changes nothing.
//! - A wrapper of a member node is made the first time it is read (`fill`), and read back from the wrapper table each time,
//!   so a collected wrapper is never a stale handle. The list traces the document wrapper (its root).
//! - Cost: after a change the next read walks the document once (steps are charged for it). A loop that changes the tree
//!   and reads `length` each time is quadratic in steps, and the step budget bounds it.
//!
//! Caps: list objects count with `max_wrappers` (with wrappers, `Attr`s and maps). The names that lists hold are capped in
//! all (`dom-nodelist-names`). Every list costs a table entry until the collector prunes it.
//!
//! `Src::Labels` is the list behind `control.labels` (`browser_controls.rs`): the `label` elements of the tree of the control that name it.
//!
//! Gates: the lookup is on `Document` only (no `Element.getElementsByName` exists). `getElementsByTagName` and
//! `getElementsByClassName` are still snapshot arrays. `NodeList` is not returned by `childNodes` or `querySelectorAll`.

use crate::browser::*;
use crate::browser_dom::{arg, next_in};
use bare_html::dom::{NodeData, NodeId, Ns};
use bare_js::eval::Interp;
use bare_js::heap::{HeapError, Slot};
use bare_js::value::{ObjId, PropertyKey, Value};
use std::cell::{OnceCell, RefCell};
use std::rc::Rc;

type V = R<Value>;

/// Bytes of list names that are alive at one time.
const MAX_NAME_BYTES: usize = 1 << 20;

/// What a list holds: the elements with a `name` (`getElementsByName`), the options of a select (`browser_select.rs`), or the labels of
/// a control (`browser_controls.rs`).
pub(crate) enum Src {
    Name(Rc<str>),
    /// `select.options`.
    Options(NodeId),
    /// `select.selectedOptions`.
    Selected(NodeId),
    /// `control.labels` (`browser_controls.rs`).
    Labels(NodeId),
}

pub(crate) struct List {
    src: Src,
    /// `Document::revision` when `ids` was built. `None`: not built yet.
    rev: Option<u64>,
    ids: Vec<NodeId>,
}

#[derive(Default)]
pub(crate) struct Tables {
    pub(crate) lists: IdMap<ObjId, List>,
    name_bytes: usize,
    cached_ids: usize,
}

#[derive(Default)]
pub(crate) struct NodeListHost {
    pub(crate) s: RefCell<Tables>,
    proto: OnceCell<ObjId>,
    fixed: RefCell<Vec<ObjId>>,
}

// ------------------------------------------------------------------------------------ GC and heap hooks

pub(crate) fn roots(d: &Dom, out: &mut Vec<Value>) {
    out.extend(d.nl.fixed.borrow().iter().map(|&o| Value::Object(o)));
}

pub(crate) fn trace(d: &Dom, owner: ObjId, out: &mut Vec<Value>) {
    if d.nl.s.borrow().lists.contains_key(&owner) {
        out.extend(d.doc_wrapper.get().map(|&o| Value::Object(o)));
    }
}

pub(crate) fn prune(d: &Dom, live: &dyn Fn(ObjId) -> bool) {
    let mut t = d.nl.s.borrow_mut();
    if t.lists.is_empty() {
        return;
    }
    let t = &mut *t;
    let (mut names, mut ids) = (0, 0);
    t.lists.retain(|&o, l| {
        let keep = live(o);
        if keep {
            if let Src::Name(n) = &l.src {
                names += n.len();
            }
            ids += l.ids.len();
        }
        keep
    });
    (t.name_bytes, t.cached_ids) = (names, ids);
}

/// `IndexedSource::indexed_len` for a list. `None`: `owner` is not a list.
pub(crate) fn len(d: &Dom, owner: ObjId) -> Option<Result<usize, HeapError>> {
    let t = match d.nl.s.try_borrow() {
        Ok(t) => t,
        Err(_) => return Some(Err(HeapError::HostUnavailable)),
    };
    let l = t.lists.get(&owner)?;
    Some(match fresh(d, l) {
        Ok(true) => Ok(l.ids.len()),
        Ok(false) => Err(HeapError::NeedsHost { owner, index: 0 }),
        Err(e) => Err(e),
    })
}

/// `IndexedSource::indexed_item` for a list. `None`: `owner` is not a list.
pub(crate) fn item(d: &Dom, owner: ObjId, index: usize) -> Option<Result<Slot, HeapError>> {
    let t = match d.nl.s.try_borrow() {
        Ok(t) => t,
        Err(_) => return Some(Err(HeapError::HostUnavailable)),
    };
    let l = t.lists.get(&owner)?;
    Some((|| {
        if !fresh(d, l)? {
            return Err(HeapError::NeedsHost { owner, index: index.min(u32::MAX as usize) as u32 });
        }
        let Some(&id) = l.ids.get(index) else { return Ok(Slot::Absent) };
        let st = d.st.try_borrow().map_err(|_| HeapError::HostUnavailable)?;
        Ok(match st.by_node.get(&id) {
            Some(&o) => Slot::Ready(o),
            None => Slot::Missing,
        })
    })())
}

fn fresh(d: &Dom, l: &List) -> Result<bool, HeapError> {
    let doc = d.doc.try_borrow().map_err(|_| HeapError::HostUnavailable)?;
    Ok(l.rev == Some(doc.revision()))
}

/// `Host::fill_indexed`: make the cache fresh and the wrapper of member `index`. `Ok(false)`: `owner` is not a list.
pub(crate) fn fill(d: &Dom, it: &mut Interp<'_>, owner: ObjId, index: u32) -> R<bool> {
    if !d.nl.s.borrow().lists.contains_key(&owner) {
        return Ok(false);
    }
    prepare(d, it, owner)?;
    let id = d.nl.s.borrow().lists.get(&owner).and_then(|l| l.ids.get(index as usize).copied());
    if let Some(id) = id {
        d.wrap(it, id)?;
    }
    Ok(true)
}

// ------------------------------------------------------------------------------------ the rebuild

/// Make the cache of the list `owner` fresh. It is the only place that scans the document for a list.
fn prepare(d: &Dom, it: &mut Interp<'_>, owner: ObjId) -> R<()> {
    let (src, cached, base) = {
        let t = d.nl.s.borrow();
        let Some(l) = t.lists.get(&owner) else { return Ok(()) };
        (match &l.src {
            Src::Name(n) => Ok(n.clone()),
            Src::Options(s) => Err((*s, 0)),
            Src::Selected(s) => Err((*s, 1)),
            Src::Labels(c) => Err((*c, 2)),
        }, l.rev, t.cached_ids - l.ids.len())
    };
    let name = match src {
        Ok(n) => n,
        Err((owner_node, mode)) => {
            if cached == Some(d.doc.borrow().revision()) {
                return Ok(());
            }
            let ids = if mode == 2 { crate::browser_controls::label_ids(d, it, owner_node)? } else { crate::browser_select::list_ids(d, it, owner_node, mode == 1)? };
            if ids.len() > d.lim.max_results {
                return Err(d.lim("dom-results"));
            }
            if base + ids.len() > d.lim.max_results.saturating_mul(4) {
                return Err(d.lim("dom-nodelist-nodes"));
            }
            let rev = d.doc.borrow().revision();
            let mut t = d.nl.s.borrow_mut();
            let t = &mut *t;
            if let Some(l) = t.lists.get_mut(&owner) {
                t.cached_ids = base + ids.len();
                l.ids = ids;
                l.rev = Some(rev);
            }
            return Ok(());
        }
    };
    let (rev, ids) = {
        let doc = d.doc.borrow();
        let rev = doc.revision();
        if cached == Some(rev) {
            return Ok(());
        }
        let mut out = Vec::new();
        it.charge(1)?;
        if let Some(atom) = doc.find_atom("name") {
            let root = doc.root();
            let (mut cur, mut depth) = (root, 0usize);
            while let Some((c, cd)) = next_in(&doc, root, cur, depth) {
                (cur, depth) = (c, cd);
                let NodeData::Element { .. } = *doc.data(c) else {
                    it.charge(1)?;
                    continue;
                };
                it.charge(1 + doc.attr_count(c) as u64 / 8)?;
                let value = if doc.element_ns(c) == Ns::Html { doc.attr(c, atom) } else { None };
                if let Some(value) = value { it.charge(value.len().min(name.len()) as u64 / 32 + 1)?; }
                if value == Some(&*name) {
                    if out.len() >= d.lim.max_results {
                        return Err(d.lim("dom-results"));
                    }
                    if base + out.len() >= d.lim.max_results.saturating_mul(4) {
                        return Err(d.lim("dom-nodelist-nodes"));
                    }
                    out.push(c);
                }
            }
        }
        (rev, out)
    };
    let mut t = d.nl.s.borrow_mut();
    let t = &mut *t;
    if let Some(l) = t.lists.get_mut(&owner) {
        t.cached_ids = base + ids.len();
        l.ids = ids;
        l.rev = Some(rev);
    }
    Ok(())
}

// ------------------------------------------------------------------------------------ methods

fn list_of(d: &Dom, it: &mut Interp<'_>, this: Value) -> R<ObjId> {
    if let Value::Object(o) = this
        && d.nl.s.borrow().lists.contains_key(&o)
    {
        return Ok(o);
    }
    Err(it.type_error("Illegal invocation"))
}

pub(crate) fn length(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let o = list_of(d, it, this)?;
    prepare(d, it, o)?;
    Ok(Value::Number(d.nl.s.borrow().lists[&o].ids.len() as f64))
}

pub(crate) fn item_method(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let o = list_of(d, it, this)?;
    if a.is_empty() {
        return Err(it.type_error("Failed to execute 'item' on 'NodeList': 1 argument required, but only 0 present."));
    }
    // WebIDL `unsigned long`: ToNumber, then modulo 2^32. Converting can run script, so no state is read before it.
    let n = it.to_js_number(arg(a, 0))?;
    let i = if n.is_finite() { n.trunc().rem_euclid(4294967296.0) as usize } else { 0 };
    prepare(d, it, o)?;
    let id = d.nl.s.borrow().lists[&o].ids.get(i).copied();
    match id {
        Some(id) => d.wrap(it, id),
        None => Ok(Value::Null),
    }
}

pub(crate) fn get_elements_by_name(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = d.nd(it, this)?;
    if !matches!(d.data(n), NodeData::Document) {
        return Err(it.type_error("Illegal invocation"));
    }
    if a.is_empty() {
        return Err(it.type_error("Failed to execute 'getElementsByName' on 'Document': 1 argument required, but only 0 present."));
    }
    let name = d.string_arg(it, arg(a, 0))?;
    let proto = *d.nl.proto.get().expect("installed");
    make_list(d, it, proto, Src::Name(name.into()))
}

/// A new live list over `src`, with the prototype `proto`. It counts with the wrappers.
pub(crate) fn make_list(d: &Dom, it: &mut Interp<'_>, proto: ObjId, src: Src) -> V {
    let n = if let Src::Name(n) = &src { n.len() } else { 0 };
    {
        let (t, at) = (d.nl.s.borrow(), d.attr.s.borrow());
        if d.st.borrow().by_obj.len() + at.attrs.len() + at.maps.len() + t.lists.len() >= d.lim.max_wrappers {
            return Err(d.lim("dom-wrappers"));
        }
        if t.name_bytes + n > MAX_NAME_BYTES {
            return Err(d.lim("dom-nodelist-names"));
        }
    }
    let o = it.heap.new_indexed(Some(proto)).map_err(|e| it.heap_error(e))?;
    let mut t = d.nl.s.borrow_mut();
    t.name_bytes += n;
    t.lists.insert(o, List { src, rev: None, ids: Vec::new() });
    Ok(Value::Object(o))
}

/// The members of the list `this`, fresh. `None`: `this` is not a list.
pub(crate) fn members(d: &Dom, it: &mut Interp<'_>, this: Value) -> R<Option<Vec<NodeId>>> {
    let Value::Object(o) = this else { return Ok(None) };
    if !d.nl.s.borrow().lists.contains_key(&o) {
        return Ok(None);
    }
    prepare(d, it, o)?;
    Ok(Some(d.nl.s.borrow().lists[&o].ids.clone()))
}

/// The select of a list made by `make_list` over the options of a select.
pub(crate) fn select_of(d: &Dom, this: Value) -> Option<NodeId> {
    let Value::Object(o) = this else { return None };
    match d.nl.s.borrow().lists.get(&o)?.src {
        Src::Options(s) | Src::Selected(s) => Some(s),
        Src::Name(_) | Src::Labels(_) => None,
    }
}

/// The prototype of a plain `NodeList`.
pub(crate) fn list_proto(d: &Dom) -> ObjId {
    *d.nl.proto.get().expect("installed")
}

// ------------------------------------------------------------------------------------ install

pub(crate) fn install(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    let g = *d.global.get().expect("installed");
    let proto = d.obj(it, it.object_prototype())?;
    let _ = d.nl.proto.set(proto);
    let mut fixed = vec![proto];
    d.def_get(it, proto, "length", length, None)?;
    d.def_method(it, proto, "item", 1, item_method)?;
    // The value iterator of an interface with an indexed getter: the `Array.prototype` functions (WebIDL "iterable").
    let ap = Value::Object(it.array_prototype());
    for name in ["entries", "keys", "values", "forEach"] {
        let k = d.key(it, name)?;
        let f = it.get_value(ap, k)?;
        it.define_data(proto, k, f, true, true, true)?;
    }
    let sym = d.plat.sym_iter.get().copied().expect("platform installed first");
    let values = it.get_value(ap, sym)?;
    it.define_data(proto, sym, values, true, false, true)?;
    // `NodeList.prototype[Symbol.toStringTag]` is "NodeList".
    let (ks, kt) = (d.key(it, "Symbol")?, d.key(it, "toStringTag")?);
    let sy = it.get_value(Value::Object(g), ks)?;
    if let Value::Symbol(s) = it.get_value(sy, kt)? {
        let tag = d.js_str(it, "NodeList")?;
        it.define_data(proto, PropertyKey::Symbol(s), tag, false, false, true)?;
    }
    let (kp, kc) = (d.key(it, "prototype")?, d.key(it, "constructor")?);
    let f = d.add_fn(it, illegal_constructor, "NodeList", 0, true)?;
    it.define_data(f, kp, Value::Object(proto), false, false, false)?;
    it.define_data(proto, kc, Value::Object(f), true, false, true)?;
    let k = d.key(it, "NodeList")?;
    it.define_data(g, k, Value::Object(f), true, false, true)?;
    fixed.push(f);
    *d.nl.fixed.borrow_mut() = fixed;
    Ok(())
}

impl Dom {
    /// How many `NodeList` objects are alive in the table (they count with `max_wrappers`).
    pub(crate) fn nl_count(&self) -> usize {
        self.nl.s.borrow().lists.len()
    }

    /// Is `o` a `NodeList` of this module (so not plain data for structured clone)?
    pub(crate) fn is_nodelist_object(&self, o: ObjId) -> bool {
        self.nl.s.borrow().lists.contains_key(&o)
    }
}

/// Counters for tests and the load tool.
#[derive(Clone, Copy, Debug, Default)]
pub struct NodeListStats {
    pub lists: usize,
    pub cached_ids: usize,
    pub name_bytes: usize,
}

impl Browser {
    pub fn nodelist_stats(&self) -> NodeListStats {
        let t = self.dom.nl.s.borrow();
        NodeListStats { lists: t.lists.len(), cached_ids: t.cached_ids, name_bytes: t.name_bytes }
    }
}
