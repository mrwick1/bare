//! Named access on `Window` (the `WindowProperties` object) and named properties of `Document`, and live named access to inert
//! iframe windows. No child document is fetched or executed.
//!
//! - Two host named objects sit in the chains: `window -> Window.prototype -> owner -> EventTarget.prototype` and `document -> document_owner ->
//!   Document.prototype`. Own properties of `window` (declared `var` and `function`, the window API) and the global lexical names
//!   (`let`, `const`) come first, so the names are only a fallback after a miss.
//! - Window names: an iframe by index, `name` or `id` (its inert window), then the first element in tree order with that `id`, or with
//!   that `name` on a, embed, form, iframe, img or object. Several matches give the first one, not an `HTMLCollection`.
//! - Document names: embed, form, iframe, img and object by `name`, iframe and object by `id`, and img by `id` when it has a `name`.
//! - The lists are built by one walk on the first ask after a change. An ask costs one hash look-up.
//! - Both objects are open (`Heap::new_named_open`): a property of the chain behind them wins over a name, and the heap finds it
//!   without asking the host. So `document.createElement` and `window.addEventListener` never wait for a list. This differs from the
//!   standard (`[LegacyOverrideBuiltIns]` on `Document`, `WindowProperties` before `EventTarget.prototype`): a page cannot hide the
//!   DOM API with an element name.
//! - A name that the host defines lazily on a miss (`Heap::lazy_chain_name`: `name`, `top`, `open`, `on*`) wins too: the host
//!   defines it before the element can answer.
//! - The list follows `Document::name_revision`, which moves only for changes of named elements. A miss after other changes (a
//!   text edit, a bare element added) costs no walk.
use crate::browser::*;
use bare_html::dom::{Atom, NodeData, NodeId};
use bare_js::eval::Interp;
use bare_js::heap::{Heap, HeapError};
use bare_js::value::{ObjId, Value};
use std::cell::{Cell, OnceCell, Ref, RefCell, RefMut};
use std::collections::{HashMap, HashSet};

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
    /// The node whose wrapper (`false`) or inert window (`true`) the next fill makes.
    pending: Cell<Option<(NodeId, bool)>>,
    /// The names, built by a fill for the document name revision `rev` and revision `full` (`u64::MAX` before the first fill). A
    /// named query reads only this list, so it costs no walk of the tree. An index key needs `full` too: a bare iframe moves no
    /// name revision.
    list: List,
    rev: u64,
    full: u64,
    /// The name of the query that found the list out of date: its fill builds the list and makes the wrapper of that name, so
    /// one property operation asks at most once here (see `hx!` in `bare_js::eval`).
    want: RefCell<Vec<u16>>,
    /// The next fill gives the lazy window name `want` its chance first (`lazy_tried` then holds it).
    lazy: Cell<bool>,
    lazy_tried: RefCell<HashSet<Vec<u16>>>,
    /// The iframe getters and window names are defined.
    iframes: bool,
}
#[derive(Default)]
struct List {
    nodes: Vec<NodeId>,
    /// The own keys of the window owner (indices, iframe names, element names) and of the document owner, in `keys` order.
    win_keys: Vec<Vec<u16>>,
    doc_keys: Vec<Vec<u16>>,
    /// The window name: the first iframe in tree order with a `name` or `id` of this value (`true`: its inert window), else the
    /// first element in tree order with that `id` or `name`. One map, so a miss hashes once.
    win: HashMap<Vec<u16>, (NodeId, bool)>,
    /// The first element in tree order with a document name of this value.
    docs: HashMap<Vec<u16>, NodeId>,
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
fn add(map: &mut HashMap<Vec<u16>, NodeId>, keys: &mut Vec<Vec<u16>>, v: &str, n: NodeId) {
    let k: Vec<u16> = v.encode_utf16().collect();
    if !map.contains_key(&k) {
        keys.push(k.clone());
        map.insert(k, n);
    }
}
fn build(d: &Dom) -> List {
    let doc = d.doc.borrow();
    let name_atom = doc.find_atom("name");
    let mut l = List::default();
    let (mut el_keys, mut frame_keys) = (Vec::new(), HashSet::new());
    let root = doc.root();
    let (mut cur, mut depth) = (root, 0);
    while let Some((n, dep)) = crate::browser_dom::next_in(&doc, root, cur, depth) {
        (cur, depth) = (n, dep);
        if !matches!(doc.data(n), NodeData::Element { .. }) {
            continue;
        }
        let id = doc.attr(n, Atom::ID).filter(|v| !v.is_empty());
        let name = name_atom.and_then(|a| doc.attr(n, a)).filter(|v| !v.is_empty());
        if id.is_none() && name.is_none() && doc.is_foreign(n) {
            continue;
        }
        let tag = if doc.is_foreign(n) { "" } else { doc.element_name(n) };
        if tag == "iframe" {
            l.win_keys.push(l.nodes.len().to_string().encode_utf16().collect());
            l.nodes.push(n);
            for v in [name, id].into_iter().flatten() {
                let k: Vec<u16> = v.encode_utf16().collect();
                // An iframe wins over an element that came first; an earlier iframe stays.
                if !matches!(l.win.get(&k), Some((_, true))) {
                    if !frame_keys.contains(&k) {
                        l.win_keys.push(k.clone());
                        frame_keys.insert(k.clone());
                    }
                    l.win.insert(k, (n, true));
                }
            }
        } else {
            for v in [id, name.filter(|_| matches!(tag, "a" | "embed" | "form" | "img" | "object"))].into_iter().flatten() {
                let k: Vec<u16> = v.encode_utf16().collect();
                if !l.win.contains_key(&k) {
                    el_keys.push(k.clone());
                    l.win.insert(k, (n, false));
                }
            }
        }
        if let Some(v) = name.filter(|_| matches!(tag, "embed" | "form" | "iframe" | "img" | "object")) {
            add(&mut l.docs, &mut l.doc_keys, v, n);
        }
        // An iframe by `id` too: the earlier frames code did it, and its tests keep it. The standard has `name` only.
        if let Some(v) = id.filter(|_| tag == "object" || tag == "iframe" || tag == "img" && name.is_some()) {
            add(&mut l.docs, &mut l.doc_keys, v, n);
        }
    }
    l.win_keys.extend(el_keys.into_iter().filter(|k| !frame_keys.contains(k)));
    l
}
/// The array index of a key (`"0"`, not `"00"`).
fn index_key(key: &[u16]) -> Option<usize> {
    if key.is_empty() || key.len() > 1 && key[0] == u16::from(b'0') {
        return None;
    }
    key.iter().try_fold(0usize, |n, &u| {
        let d = u.checked_sub(u16::from(b'0')).filter(|&d| d < 10)?;
        n.checked_mul(10)?.checked_add(usize::from(d))
    })
}
impl State {
    /// Is the list out of date? `index`: the query reads iframe indices.
    fn stale(&self, doc: &bare_html::dom::Document, index: bool) -> bool {
        self.rev != doc.name_revision() || index && self.full != doc.revision()
    }
}
impl List {
    /// The node of the name `key`, and whether its value is the inert window of an iframe.
    fn find(&self, key: &[u16], document: bool) -> Option<(NodeId, bool)> {
        if document {
            return self.docs.get(key).map(|&n| (n, false));
        }
        if let Some(i) = index_key(key) {
            return self.nodes.get(i).map(|&n| (n, true));
        }
        self.win.get(key).copied()
    }
}

/// Make the two named objects once per realm. Define the iframe getters only after the document has seen an iframe.
pub(crate) fn activate(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    if !d.frames.active() {
        install(d, it)?;
    }
    if d.frames.state().is_some_and(|s| s.iframes)
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
    if let Some(s) = d.frames.state_mut().as_mut() {
        s.iframes = true;
    }
    Ok(())
}
fn install(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    let owner = it
        .heap
        .new_named_open(Some(d.p().et))
        .map_err(|e| it.heap_error(e))?;
    let document_owner = it
        .heap
        .new_named_open(Some(d.p().document))
        .map_err(|e| it.heap_error(e))?;
    // The chain is `window -> Window.prototype -> owner -> EventTarget.prototype`, as in the standard.
    let g = it.global_object();
    let win = it.heap.get_prototype_of(g).map_err(|e| it.heap_error(e))?.expect("Window.prototype");
    it.heap
        .set_prototype_of(win, Some(owner))
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
            full: u64::MAX,
            want: RefCell::default(),
            lazy: Cell::new(false),
            lazy_tried: RefCell::default(),
            iframes: false,
        })))
        .map_err(|_| d.lim("dom-frames"))?;
    Ok(())
}

/// Is `owner` one of the two named objects? Their names do not hide the properties of their chains.
pub(crate) fn is_owner(d: &Dom, owner: ObjId) -> bool {
    d.frames.state().is_some_and(|s| owner == s.owner || owner == s.document_owner)
}
pub(crate) fn has(d: &Dom, owner: ObjId, key: &[u16]) -> Result<Option<bool>, HeapError> {
    let state = d.frames.state();
    let Some(s) = state
        .as_ref()
        .filter(|s| owner == s.owner || owner == s.document_owner)
    else {
        return Ok(None);
    };
    let document = owner == s.document_owner;
    if s.stale(&*d.doc.try_borrow().map_err(|_| HeapError::HostUnavailable)?, !document && index_key(key).is_some()) {
        *s.want.borrow_mut() = key.to_vec();
        return Err(HeapError::NeedsHost { owner, index: FILL });
    }
    let Some((n, window)) = s.list.find(key, document) else {
        return Ok(Some(false));
    };
    if !window && Heap::lazy_chain_name(key) && !s.lazy_tried.borrow().contains(key) {
        *s.want.borrow_mut() = key.to_vec();
        s.lazy.set(true);
        return Err(HeapError::NeedsHost { owner, index: FILL });
    }
    let ready = if window {
        s.contexts.contains_key(&n)
    } else {
        d.st.try_borrow().map_err(|_| HeapError::HostUnavailable)?.by_node.contains_key(&n)
    };
    if !ready {
        s.pending.set(Some((n, window)));
        return Err(HeapError::NeedsHost { owner, index: FILL });
    }
    Ok(Some(true))
}
pub(crate) fn value(d: &Dom, owner: ObjId, key: &[u16]) -> Option<Value> {
    let state = d.frames.state();
    let s = state
        .as_ref()
        .filter(|s| owner == s.owner || owner == s.document_owner)?;
    let (n, window) = s.list.find(key, owner == s.document_owner)?;
    if window {
        s.contexts.get(&n).copied().map(Value::Object)
    } else {
        d.st.borrow().by_node.get(&n).copied().map(Value::Object)
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
    if s.stale(&d.doc.borrow(), owner == s.owner) {
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
    let l = if !s.stale(&d.doc.borrow(), owner == s.owner) {
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
    let (rev, full) = {
        let doc = d.doc.borrow();
        (doc.name_revision(), doc.revision())
    };
    if d.frames.state().is_some_and(|s| s.stale(&d.doc.borrow(), true)) {
        it.charge(d.doc.borrow().len() as u64)?;
        let list = build(d);
        if let Some(s) = d.frames.state_mut().as_mut() {
            (s.list, s.rev, s.full) = (list, rev, full);
        }
    }
    let lazy = d.frames.state().filter(|s| s.lazy.take()).map(|s| {
        let want = std::mem::take(&mut *s.want.borrow_mut());
        s.lazy_tried.borrow_mut().insert(want.clone());
        want
    });
    if let Some(key) = lazy {
        crate::browser_handlers::lazy_key(d, it, d.p().et, String::from_utf16_lossy(&key))?;
        return Ok(true);
    }
    let n = d.frames.state().and_then(|s| {
        let want = std::mem::take(&mut *s.want.borrow_mut());
        s.pending.take().or_else(|| s.list.find(&want, owner == s.document_owner))
    });
    match n {
        Some((n, true)) => {
            context(d, it, n)?;
        }
        Some((n, false)) => {
            d.wrap(it, n)?;
        }
        None => {}
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
