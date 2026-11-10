//! Live named access to inert iframe windows. No child document is fetched or executed.
use crate::browser::*;
use bare_html::dom::{NodeData, NodeId};
use bare_js::eval::Interp;
use bare_js::heap::HeapError;
use bare_js::value::{ObjId, Value};
use std::cell::{Cell, OnceCell, Ref, RefCell, RefMut};
use std::collections::HashMap;

pub(crate) const FILL: u32 = u32::MAX - 8;
#[derive(Default)]
pub(crate) struct Frames(OnceCell<Box<RefCell<State>>>);
const _: () = assert!(std::mem::size_of::<Frames>() == std::mem::size_of::<usize>());
impl Frames {
    #[inline]
    pub(crate) fn active(&self) -> bool {
        self.0.get().is_some()
    }
    fn state(&self) -> Option<Ref<'_, State>> {
        self.0.get().map(|s| s.borrow())
    }
    fn state_mut(&self) -> Option<RefMut<'_, State>> {
        self.0.get().map(|s| s.borrow_mut())
    }
}
struct State {
    owner: ObjId,
    document_owner: ObjId,
    contexts: IdMap<NodeId, ObjId>,
    pending: Cell<Option<NodeId>>,
    /// The iframes and their names, built by a fill for the document revision `rev` (`u64::MAX` before the first fill). A
    /// named query reads only this list, so it costs no walk of the tree.
    list: List,
    rev: u64,
    /// The name of the query that found the list out of date: its fill builds the list and makes the context of that name, so
    /// one property operation asks at most once here (see `hx!` in `bare_js::eval`).
    want: RefCell<Vec<u16>>,
}
#[derive(Default)]
struct List {
    nodes: Vec<NodeId>,
    /// The own keys of the window owner (indices, then names) and of the document owner (names), in `keys` order.
    win_keys: Vec<Vec<u16>>,
    doc_keys: Vec<Vec<u16>>,
    /// The first iframe in tree order with a `name` or `id` of this value.
    names: HashMap<Vec<u16>, NodeId>,
}

fn nodes(d: &Dom) -> Vec<NodeId> {
    let doc = d.doc.borrow();
    let root = doc.root();
    let (mut cur, mut depth) = (root, 0);
    let mut out = Vec::new();
    while let Some((n, dep)) = crate::browser_dom::next_in(&doc, root, cur, depth) {
        (cur, depth) = (n, dep);
        if matches!(doc.data(n), NodeData::Element { .. })
            && !doc.is_foreign(n)
            && doc.element_name(n) == "iframe"
        {
            out.push(n);
        }
    }
    out
}
fn build(d: &Dom) -> List {
    let nodes = nodes(d);
    let doc = d.doc.borrow();
    let mut l = List::default();
    for (i, &n) in nodes.iter().enumerate() {
        l.win_keys.push(i.to_string().encode_utf16().collect());
        for a in ["name", "id"] {
            if let Some(v) = doc.find_attr_qname(n, a).and_then(|a| doc.attr(n, a)).filter(|v| !v.is_empty()) {
                let k: Vec<u16> = v.encode_utf16().collect();
                l.names.entry(k.clone()).or_insert(n);
                if !l.doc_keys.contains(&k) {
                    l.doc_keys.push(k.clone());
                }
                if !l.win_keys.contains(&k) {
                    l.win_keys.push(k);
                }
            }
        }
    }
    l.nodes = nodes;
    l
}
impl List {
    fn find(&self, key: &[u16], document: bool) -> Option<NodeId> {
        if !document
            && let Ok(name) = String::from_utf16(key)
            && let Ok(i) = name.parse::<usize>()
            && i.to_string() == name
        {
            return self.nodes.get(i).copied();
        }
        self.names.get(key).copied()
    }
}

/// Activate only after the document has seen an iframe. Ordinary realms keep their prototype chains.
pub(crate) fn activate(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    if d.frames.active()
        || d.doc
            .try_borrow()
            .map_err(|_| it.heap_error(HeapError::HostUnavailable))?
            .find_atom("iframe")
            .is_none()
    {
        return Ok(());
    }
    d.def_get(it, d.p().iframe, "contentWindow", content_window, None)?;
    d.def_get(it, d.p().iframe, "name", name, Some(set_name))?;
    for name in ["frames", "parent", "top", "length", "name", "screen"] {
        crate::browser::define_window(d, it, name)?;
    }
    let owner = it
        .heap
        .new_named(Some(d.p().et))
        .map_err(|e| it.heap_error(e))?;
    let document_owner = it
        .heap
        .new_named(Some(d.p().document))
        .map_err(|e| it.heap_error(e))?;
    let g = it.global_object();
    it.heap
        .set_prototype_of(g, Some(owner))
        .map_err(|e| it.heap_error(e))?;
    let doc = *d.doc_wrapper.get().expect("installed");
    it.heap
        .set_prototype_of(doc, Some(document_owner))
        .map_err(|e| it.heap_error(e))?;
    d.frames
        .0
        .set(Box::new(RefCell::new(State {
            owner,
            document_owner,
            contexts: IdMap::default(),
            pending: Cell::new(None),
            list: List::default(),
            rev: u64::MAX,
            want: RefCell::default(),
        })))
        .map_err(|_| d.lim("dom-frames"))?;
    Ok(())
}

pub(crate) fn has(d: &Dom, owner: ObjId, key: &[u16]) -> Result<Option<bool>, HeapError> {
    let state = d.frames.state();
    let Some(s) = state
        .as_ref()
        .filter(|s| owner == s.owner || owner == s.document_owner)
    else {
        return Ok(None);
    };
    if s.rev != d.doc.borrow().revision() {
        *s.want.borrow_mut() = key.to_vec();
        return Err(HeapError::NeedsHost { owner, index: FILL });
    }
    let Some(n) = s.list.find(key, owner == s.document_owner) else {
        return Ok(Some(false));
    };
    if !s.contexts.contains_key(&n) {
        s.pending.set(Some(n));
        return Err(HeapError::NeedsHost { owner, index: FILL });
    }
    Ok(Some(true))
}
pub(crate) fn value(d: &Dom, owner: ObjId, key: &[u16]) -> Option<Value> {
    let state = d.frames.state();
    let s = state
        .as_ref()
        .filter(|s| owner == s.owner || owner == s.document_owner)?;
    let n = s.list.find(key, owner == s.document_owner)?;
    if owner == s.document_owner {
        d.st.borrow().by_node.get(&n).copied().map(Value::Object)
    } else {
        s.contexts.get(&n).copied().map(Value::Object)
    }
}
pub(crate) fn len(d: &Dom, owner: ObjId) -> Result<Option<usize>, HeapError> {
    let state = d.frames.state();
    let Some(s) = state
        .as_ref()
        .filter(|s| owner == s.owner || owner == s.document_owner)
    else {
        return Ok(None);
    };
    if s.rev != d.doc.borrow().revision() {
        s.want.borrow_mut().clear();
        return Err(HeapError::NeedsHost { owner, index: FILL });
    }
    Ok(Some(if owner == s.owner { s.list.win_keys.len() } else { s.list.doc_keys.len() }))
}
pub(crate) fn keys(d: &Dom, owner: ObjId) -> Option<Vec<Vec<u16>>> {
    let state = d.frames.state();
    let s = state
        .as_ref()
        .filter(|s| owner == s.owner || owner == s.document_owner)?;
    let built;
    let l = if s.rev == d.doc.borrow().revision() {
        &s.list
    } else {
        built = build(d);
        &built
    };
    Some(if owner == s.owner { l.win_keys.clone() } else { l.doc_keys.clone() })
}
pub(crate) fn fill(d: &Dom, it: &mut Interp<'_>, owner: ObjId, index: u32) -> R<bool> {
    if index != FILL {
        return Ok(false);
    }
    let rev = d.doc.borrow().revision();
    if d.frames.state().is_some_and(|s| s.rev != rev) {
        it.charge(d.doc.borrow().len() as u64)?;
        let list = build(d);
        if let Some(s) = d.frames.state_mut().as_mut() {
            (s.list, s.rev) = (list, rev);
        }
    }
    let n = d.frames.state().and_then(|s| {
        let want = std::mem::take(&mut *s.want.borrow_mut());
        s.pending.take().or_else(|| s.list.find(&want, owner == s.document_owner).filter(|n| !s.contexts.contains_key(n)))
    });
    if let Some(n) = n {
        context(d, it, n)?;
    }
    Ok(true)
}
fn context(d: &Dom, it: &mut Interp<'_>, n: NodeId) -> R<Value> {
    activate(d, it)?;
    if let Some(o) = d
        .frames
        .state()
        .as_ref()
        .and_then(|s| s.contexts.get(&n))
        .copied()
    {
        return Ok(Value::Object(o));
    }
    if d.frames.state().as_ref().map_or(0, |s| s.contexts.len()) >= d.lim.max_wrappers {
        return Err(d.lim("dom-wrappers"));
    }
    let element = d.wrap(it, n)?;
    let o = d.obj(it, it.object_prototype())?;
    let g = Value::Object(it.global_object());
    for (name, v) in [
        ("window", Value::Object(o)),
        ("self", Value::Object(o)),
        ("frames", Value::Object(o)),
        ("parent", g),
        ("top", g),
        ("length", Value::Number(0.0)),
        ("frameElement", element),
    ] {
        let k = d.key(it, name)?;
        it.define_data(o, k, v, false, true, true)?;
    }
    d.frames
        .state_mut()
        .as_mut()
        .expect("activated")
        .contexts
        .insert(n, o);
    Ok(Value::Object(o))
}
pub(crate) fn content_window(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> R<Value> {
    let n = d.el(it, this)?;
    if d.doc.borrow().element_name(n) != "iframe" {
        return Err(it.type_error("Illegal invocation"));
    }
    if !d.is_connected_node(it, n)? {
        return Ok(Value::Null);
    }
    context(d, it, n)
}
pub(crate) fn length(d: &Dom, it: &mut Interp<'_>, _: Value, _: &[Value]) -> R<Value> {
    it.charge(d.doc.borrow().len() as u64)?;
    Ok(Value::Number(nodes(d).len() as f64))
}
pub(crate) fn name(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> R<Value> {
    let n = d.el(it, this)?;
    match crate::browser_dom::attr_get(d, it, n, "name")? {
        Value::Null => d.js_str(it, ""),
        v => Ok(v),
    }
}
pub(crate) fn set_name(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> R<Value> {
    let n = d.el(it, this)?;
    let s = d.string_arg(it, a.first().copied().unwrap_or(Value::Undefined))?;
    crate::browser_dom::attr_set(d, it, n, "name", &s)?;
    Ok(Value::Undefined)
}
#[cold]
pub(crate) fn trace(d: &Dom, owner: ObjId, out: &mut Vec<Value>) {
    if let Some(s) = d.frames.state().as_ref() {
        for (&n, &o) in &s.contexts {
            if d.st.borrow().by_node.get(&n) == Some(&owner) {
                out.push(Value::Object(o));
            }
        }
    }
}
pub(crate) fn prune(d: &Dom, live: &dyn Fn(ObjId) -> bool) {
    if let Some(s) = d.frames.state_mut().as_mut() {
        s.contexts.retain(|_, o| live(*o));
    }
}
