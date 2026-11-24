//! Named access on `Window` (the `WindowProperties` object) and named properties of `Document`, and live named access to inert
//! iframe windows. A blank frame has a document of its own that shares the parent realm. An iframe with an http(s) `src` is a child page
//! (`browser_child.rs`): this module only gives its `RemoteWindow` as `contentWindow`, and `null` as `contentDocument`. Where no child
//! driver runs (a `Browser` that `run_page` never ran), no child document is fetched or executed, and a same-origin `src` is asked for only
//! to catch a download (`Dom::frame_src`).
//!
//! - The inert window of an iframe is an `EventTarget` (a `Target::Obj`: `addEventListener`, `removeEventListener`, `dispatchEvent`).
//!   An iframe with no `src` (or an empty one, or `about:blank`) and no `srcdoc` gets an initial `about:blank` document at the first
//!   `contentWindow`: `contentWindow.document` and `contentDocument` are that one object while the iframe is connected. It is made
//!   as `document.implementation.createHTMLDocument('')` is. Any other iframe has `contentDocument` `null` (the child is never
//!   loaded, so it has no document), as for a cross-origin frame. The frame window gets no `resize` event.
//! - The blank document has `defaultView` (its window), `URL` `about:blank`, `activeElement` (its body), and owns the nodes made by its
//!   `createElement`, `createTextNode` and `createComment` (`ownerDocument`). Other `create*` methods still give the main document.
//! - The window of a blank frame has an own `Location` for `about:blank` (read only), and inherits the parent global: bare has one
//!   realm, so `w.Array === Array` and `w.setTimeout` is the parent's. `w.postMessage` drops the message. Other frames keep `et`.
//! - Two host named objects sit in the chains: `window -> Window.prototype -> owner -> EventTarget.prototype` and `document -> document_owner ->
//!   Document.prototype`. Own properties of `window` (declared `var` and `function`, the window API) and the global lexical names
//!   (`let`, `const`) come first, so the names are only a fallback after a miss.
//! - Window names: an iframe by index, `name` or `id` (its inert window), then the first element in tree order with that `id`, or with
//!   that `name` on a, embed, form, iframe, img or object. Several matches give the first one, not an `HTMLCollection`.
//! - Document names: embed, form, iframe, img and object by `name`, iframe and object by `id`, and img by `id` when it has a `name`.
//! - A name that the page asks for after a change of named elements is answered by one walk that stops at the first answer (`walk`):
//!   no list, no allocation. A page probes many names that do not exist, between changes, so a list would be built for one key and
//!   dropped. A name that no element ever had (`Document::may_have_name`) is a miss with no walk at all. An index (`window[0]`),
//!   a key list (`Object.keys(window)`) and the `WALKS`-th name asked at one revision build the list (one walk). Then an ask
//!   costs one hash look-up.
//! - Both objects are open (`Heap::new_named_open`): a property of the chain behind them wins over a name, and the heap finds it
//!   without asking the host. So `document.createElement` and `window.addEventListener` never wait for a list. This differs from the
//!   standard (`[LegacyOverrideBuiltIns]` on `Document`, `WindowProperties` before `EventTarget.prototype`): a page cannot hide the
//!   DOM API with an element name.
//! - A name that the host defines lazily on a miss (`lazy_chain_name`: `name`, `top`, `open`, `on*`) wins too: the host
//!   defines it before the element can answer.
//! - The list and the walk follow `Document::name_revision`, which moves only for changes of named elements. A miss after other
//!   changes (a text edit, a bare element added) costs no walk.
use crate::eng_new::New;
use crate::eng::{Eng, Fault, Ops, Val};
use crate::browser::*;
use crate::browser_scoped::*;
use bare_html::dom::{Atom, NodeData, NodeId};
use bare_js_api::Pending;
use bare_js_api::{Args, Local, Type};
use std::cell::{Cell, OnceCell, Ref, RefCell, RefMut};
use std::collections::{HashMap, HashSet};

pub(crate) const FILL: u32 = u32::MAX - 8;
/// Walks at one name revision before a list is built: a walk is about 1/20 of a build, so the list pays back after this many.
const WALKS: u32 = 16;
#[derive(Default)]
pub(crate) struct Frames<E: Eng>(OnceCell<Box<RefCell<State<E>>>>);
const _: () = assert!(std::mem::size_of::<Frames<New>>() == std::mem::size_of::<usize>());
impl<E: Eng> Frames<E> {
    #[inline]
    pub(crate) fn active(&self) -> bool {
        self.0.get().is_some()
    }
    fn state(&self) -> Option<Ref<'_, State<E>>> {
        self.0.get().map(|s| s.borrow())
    }
    fn state_mut(&self) -> Option<RefMut<'_, State<E>>> {
        self.0.get().map(|s| s.borrow_mut())
    }
    /// Lists built so far, and nodes that walks visited.
    #[cfg(test)]
    pub(crate) fn counts(&self) -> (u32, u64) {
        self.state().map_or((0, 0), |s| (s.builds.get(), s.visited.get()))
    }
    /// The `Location` objects of frame windows that the state still holds.
    #[cfg(test)]
    pub(crate) fn locations(&self) -> usize {
        self.state().map_or(0, |s| s.locs.len())
    }
}
struct State<E: Eng> {
    owner: E::Obj,
    document_owner: E::Obj,
    contexts: IdMap<NodeId, E::Obj>,
    /// The initial `about:blank` document node of a frame, and its window (`defaultView`).
    docs: IdMap<NodeId, E::Obj>,
    /// A node made by the `createX` method of a frame document, and that document.
    owned: IdMap<NodeId, NodeId>,
    /// The `Location` objects of frame windows.
    locs: HashSet<E::Obj>,
    /// The node whose wrapper (`false`) or inert window (`true`) the next fill makes.
    pending: Cell<Option<(NodeId, bool)>>,
    /// The names, built by a fill for the document name revision `rev` and revision `full` (`u64::MAX` before the first fill). A
    /// named query reads only this list, so it costs no walk of the tree. An index key needs `full` too: a bare iframe moves no
    /// name revision.
    list: List,
    rev: u64,
    full: u64,
    /// The name of the query that found the list out of date: its fill builds the list and makes the wrapper of that name, so
    /// one property operation asks at most once here (see `hx!` in the engine).
    want: RefCell<Vec<u16>>,
    /// The next fill gives the lazy window name `want` its chance first (`lazy_tried` then holds it).
    lazy: Cell<bool>,
    lazy_tried: RefCell<HashSet<Vec<u16>>>,
    /// The iframe getters and window names are defined.
    iframes: bool,
    /// The answer of the last walk, while the list is out of date (see `State::find`).
    one: RefCell<One>,
    /// The name revision of the last walk, and the walks made at it.
    walks: Cell<(u64, u32)>,
    /// Lists built, and nodes visited by walks (for tests).
    builds: Cell<u32>,
    visited: Cell<u64>,
}
struct One {
    rev: u64,
    key: Vec<u16>,
    document: bool,
    hit: Option<(NodeId, bool)>,
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

fn nodes<E: Eng>(d: &Dom<E>) -> Vec<NodeId> {
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
    if let std::collections::hash_map::Entry::Vacant(e) = map.entry(k) {
        keys.push(e.key().clone());
        e.insert(n);
    }
}
/// What `build` would answer for `key` (not an index of the window), by one walk that stops at the first answer: the node, whether
/// it is the inert window of an iframe, and the nodes visited. Without an iframe in the document the first match is final, as an
/// iframe wins over an element that came first.
fn walk<E: Eng>(d: &Dom<E>, key: &[u16], document: bool) -> (Option<(NodeId, bool)>, u64) {
    let doc = d.doc.borrow();
    let name_atom = doc.find_atom("name");
    let frames = doc.find_atom("iframe").is_some();
    let same = |v: Option<&str>| v.is_some_and(|v| v.encode_utf16().eq(key.iter().copied()));
    let root = doc.root();
    let (mut cur, mut depth, mut seen, mut first) = (root, 0, 0, None);
    while let Some((n, dep)) = crate::browser_dom::next_in(&doc, root, cur, depth) {
        (cur, depth) = (n, dep);
        seen += 1;
        if !matches!(doc.data(n), NodeData::Element { .. }) {
            continue;
        }
        let id = doc.attr(n, Atom::ID).filter(|v| !v.is_empty());
        let name = name_atom.and_then(|a| doc.attr(n, a)).filter(|v| !v.is_empty());
        let (by_id, by_name) = (same(id), same(name));
        if !by_id && !by_name {
            continue;
        }
        let tag = if doc.is_foreign(n) { "" } else { doc.element_name(n) };
        if document {
            if by_name && matches!(tag, "embed" | "form" | "iframe" | "img" | "object")
                || by_id && (matches!(tag, "object" | "iframe") || tag == "img" && name.is_some())
            {
                return (Some((n, false)), seen);
            }
        } else if tag == "iframe" {
            return (Some((n, true)), seen);
        } else if by_id || by_name && matches!(tag, "a" | "embed" | "form" | "img" | "object") {
            first.get_or_insert(n);
            if !frames {
                break;
            }
        }
    }
    (first.map(|n| (n, false)), seen)
}
fn build<E: Eng>(d: &Dom<E>) -> List {
    if let Some(s) = d.frames.state() {
        s.builds.set(s.builds.get() + 1);
    }
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
                if let std::collections::hash_map::Entry::Vacant(e) = l.win.entry(k) {
                    el_keys.push(e.key().clone());
                    e.insert((n, false));
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
impl<E: Eng> State<E> {
    /// Is the list out of date? `index`: the query reads iframe indices.
    fn stale(&self, doc: &bare_html::dom::Document, index: bool) -> bool {
        self.rev != doc.name_revision() || index && self.full != doc.revision()
    }
    /// The node of the name `key` and whether it is an iframe window, from the list or from the last walk. `None`: neither is
    /// current, so a fill must answer. The walk answers no index of the window.
    fn find(&self, doc: &bare_html::dom::Document, key: &[u16], document: bool) -> Option<Option<(NodeId, bool)>> {
        if !self.stale(doc, !document && index_key(key).is_some()) {
            return Some(self.list.find(key, document));
        }
        let one = self.one.borrow();
        (one.rev == doc.name_revision() && one.document == document && one.key == key).then_some(one.hit)
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
pub(crate) fn activate<E: Eng>(d: &Dom<E>, it: &mut E::Vm) -> R<E, ()> {
    if !d.frames.active() {
        install(d, it)?;
    }
    if d.frames.state().is_some_and(|s| s.iframes)
        || d.doc
            .try_borrow()
            .map_err(|_| Fault::Heap("HostUnavailable"))?
            .find_atom("iframe")
            .is_none()
    {
        return Ok(());
    }
    d.def_get_s(it, d.p().iframe, "contentWindow", content_window, None)?;
    d.def_get_s(it, d.p().iframe, "contentDocument", content_document, None)?;
    d.def_get_s(it, d.p().iframe, "name", name, Some(set_name))?;
    crate::browser_reflect::reflect!(d, it, d.p().iframe, &["iframe"], {
        "src" => "src": Url, "srcdoc" => "srcdoc": Str, "allow" => "allow": Str, "width" => "width": Str, "height" => "height": Str,
        "loading" => "loading": Enum(&["lazy", "eager"], "eager"), "referrerPolicy" => "referrerpolicy": Str,
    });
    for name in ["frames", "parent", "top", "length", "name", "screen"] {
        crate::browser::define_window(d, it, name)?;
    }
    if let Some(s) = d.frames.state_mut().as_mut() {
        s.iframes = true;
    }
    Ok(())
}
fn install<E: Eng>(d: &Dom<E>, it: &mut E::Vm) -> R<E, ()> {
    let owner = d.new_source(it, crate::browser_sources::Source::Frames, Some(d.p().et))?;
    let document_owner = d.new_source(it, crate::browser_sources::Source::Frames, Some(d.p().document))?;
    // The chain is `window -> Window.prototype -> owner -> EventTarget.prototype`, as in the standard.
    let g = it.global_object();
    let win = it.h_get_proto(g)?.expect("Window.prototype");
    it.h_set_proto(win, Some(owner))?;
    let doc = *d.doc_wrapper.get().expect("installed");
    it.h_set_proto(doc, Some(document_owner))?;
    d.frames
        .0
        .set(Box::new(RefCell::new(State {
            owner,
            document_owner,
            contexts: IdMap::default(),
            docs: IdMap::default(),
            owned: IdMap::default(),
            locs: HashSet::new(),
            pending: Cell::new(None),
            list: List::default(),
            rev: u64::MAX,
            full: u64::MAX,
            want: RefCell::default(),
            lazy: Cell::new(false),
            lazy_tried: RefCell::default(),
            iframes: false,
            one: RefCell::new(One { rev: u64::MAX, key: Vec::new(), document: false, hit: None }),
            walks: Cell::new((u64::MAX, 0)),
            builds: Cell::new(0),
            visited: Cell::new(0),
        })))
        .map_err(|_| d.lim("dom-frames"))?;
    Ok(())
}

pub(crate) fn has<E: Eng>(d: &Dom<E>, owner: E::Obj, key: &[u16]) -> Result<Option<bool>, Pending> {
    let state = d.frames.state();
    let Some(s) = state
        .as_ref()
        .filter(|s| owner == s.owner || owner == s.document_owner)
    else {
        return Ok(None);
    };
    let document = owner == s.document_owner;
    let doc = d.doc.try_borrow().map_err(|_| Pending::Busy)?;
    let found = s.find(&doc, key, document);
    let Some(found) = found else {
        // No element ever had this `id` or `name`: a miss, with no walk (a window index is no name).
        if (document || index_key(key).is_none()) && !doc.may_have_name(char::decode_utf16(key.iter().copied()).map(|c| c.unwrap_or('\u{FFFD}'))) {
            return Ok(Some(false));
        }
        *s.want.borrow_mut() = key.to_vec();
        return Err(Pending::Fill(FILL));
    };
    let Some((n, window)) = found else {
        return Ok(Some(false));
    };
    drop(doc);
    if !window && lazy_chain_name(key) && !s.lazy_tried.borrow().contains(key) {
        *s.want.borrow_mut() = key.to_vec();
        s.lazy.set(true);
        return Err(Pending::Fill(FILL));
    }
    let ready = if window {
        s.contexts.contains_key(&n)
    } else {
        d.st.try_borrow().map_err(|_| Pending::Busy)?.by_node.contains_key(&n)
    };
    if !ready {
        s.pending.set(Some((n, window)));
        return Err(Pending::Fill(FILL));
    }
    Ok(Some(true))
}
pub(crate) fn value<E: Eng>(d: &Dom<E>, owner: E::Obj, key: &[u16]) -> Option<E::Raw> {
    let state = d.frames.state();
    let s = state
        .as_ref()
        .filter(|s| owner == s.owner || owner == s.document_owner)?;
    let (n, window) = s.find(&d.doc.borrow(), key, owner == s.document_owner)??;
    if window {
        s.contexts.get(&n).copied().map(E::obj_raw)
    } else {
        d.st.borrow().wrapper(n).map(E::obj_raw)
    }
}
pub(crate) fn len<E: Eng>(d: &Dom<E>, owner: E::Obj) -> Result<Option<usize>, Pending> {
    let state = d.frames.state();
    let Some(s) = state
        .as_ref()
        .filter(|s| owner == s.owner || owner == s.document_owner)
    else {
        return Ok(None);
    };
    if s.stale(&d.doc.borrow(), owner == s.owner) {
        s.want.borrow_mut().clear();
        return Err(Pending::Fill(FILL));
    }
    Ok(Some(if owner == s.owner { s.list.win_keys.len() } else { s.list.doc_keys.len() }))
}
pub(crate) fn keys<E: Eng>(d: &Dom<E>, owner: E::Obj) -> Option<Vec<Vec<u16>>> {
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
pub(crate) fn fill<E: Eng>(d: &Dom<E>, it: &mut E::Vm, owner: E::Obj, index: u32) -> R<E, bool> {
    if index != FILL {
        return Ok(false);
    }
    let (rev, full) = {
        let doc = d.doc.borrow();
        (doc.name_revision(), doc.revision())
    };
    // One name is answered by a walk; an index, a key list and the `WALKS`-th name at one revision build the list.
    let plan = d.frames.state().map(|s| {
        let (doc, want) = (d.doc.borrow(), s.want.borrow());
        let document = owner == s.document_owner;
        let stale = if want.is_empty() { s.pending.get().is_none() && s.stale(&doc, true) } else { s.find(&doc, &want, document).is_none() };
        let walks = if s.walks.get().0 == rev { s.walks.get().1 } else { 0 };
        let one = !want.is_empty() && (document || index_key(&want).is_none()) && walks < WALKS;
        (stale, one.then(|| (want.clone(), document, walks)))
    });
    match plan {
        Some((true, Some((key, document, walks)))) => {
            let (hit, seen) = walk(d, &key, document);
            it.charge_o(seen)?;
            if let Some(s) = d.frames.state() {
                s.visited.set(s.visited.get() + seen);
                *s.one.borrow_mut() = One { rev, key, document, hit };
                s.walks.set((rev, walks + 1));
            }
        }
        Some((true, None)) => {
            it.charge_o(d.doc.borrow().len() as u64)?;
            let list = build(d);
            if let Some(s) = d.frames.state_mut().as_mut() {
                (s.list, s.rev, s.full) = (list, rev, full);
            }
        }
        _ => {}
    }
    let lazy = d.frames.state().filter(|s| s.lazy.take()).map(|s| {
        let want = std::mem::take(&mut *s.want.borrow_mut());
        s.lazy_tried.borrow_mut().insert(want.clone());
        want
    });
    if let Some(key) = lazy {
        crate::browser_handlers::lazy_key(d, it, d.p().et, &String::from_utf16_lossy(&key))?;
        return Ok(true);
    }
    let n = d.frames.state().and_then(|s| {
        let want = std::mem::take(&mut *s.want.borrow_mut());
        s.pending.take().or_else(|| s.find(&d.doc.borrow(), &want, owner == s.document_owner).flatten())
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
fn context<E: Eng>(d: &Dom<E>, it: &mut E::Vm, n: NodeId) -> R<E, E::Raw> {
    activate(d, it)?;
    // A child page (`browser_child.rs`): its window is a `RemoteWindow`.
    if let Some(id) = d.kids.id_of(n) {
        let w = d.child_window(it, id)?;
        if let (Val::Object(o), Some(mut s)) = (E::kind(w), d.frames.state_mut()) {
            s.contexts.insert(n, o);
            d.wrapper_edges.set(true);
        }
        return Ok(w);
    }
    if let Some(o) = d
        .frames
        .state()
        .as_ref()
        .and_then(|s| s.contexts.get(&n))
        .copied()
    {
        return Ok(E::obj_raw(o));
    }
    if d.frames.state().as_ref().map_or(0, |s| s.contexts.len()) >= d.lim.max_wrappers {
        return Err(d.lim("dom-wrappers"));
    }
    if d.st.borrow().by_obj.len() >= d.lim.max_wrappers {
        return Err(d.lim("dom-wrappers"));
    }
    let element = d.wrap(it, n)?;
    // The frame window is an `EventTarget` (`Target::Obj`, as a worker or an abort signal is).
    let o = d.obj(it, d.p().et)?;
    d.st.borrow_mut().add_target(it, o, crate::browser::Owner::Frame(o));
    let g = E::obj_raw(it.global_object());
    for (name, v) in [
        ("window", E::obj_raw(o)),
        ("self", E::obj_raw(o)),
        ("frames", E::obj_raw(o)),
        ("parent", g),
        ("top", g),
        ("length", E::num(0.0)),
        ("frameElement", element),
    ] {
        let k = d.key(it, name)?;
        it.define_data(o, k, v, false, true, true)?;
    }
    let blank = initial_blank(d, n);
    let k = d.key(it, "document")?;
    if blank {
        let doc = d.new_blank_document(it)?;
        it.define_data(o, k, doc, false, true, true)?;
        let dn = d.nd(it, doc)?;
        let loc = crate::browser_navigation::frame_location(d, it)?;
        let k = d.key(it, "location")?;
        it.define_data(o, k, E::obj_raw(loc), false, true, true)?;
        // Same-origin approximation: bare has one realm, so a global the frame window does not own is the parent's (`w.Array === Array`).
        it.h_set_proto(o, Some(it.global_object()))?;
        let mut s = d.frames.state_mut().expect("activated");
        s.docs.insert(dn, o);
        d.wrapper_edges.set(true);
        s.locs.insert(loc);
        drop(s);
        let k = d.st.borrow_mut().owners.key(crate::browser::Owner::Loc(loc));
        it.set_host_owner_obj(loc, k);
    } else {
        // The parent global must not answer for a frame with a child document that is never loaded.
        it.define_data(o, k, E::undef(), false, true, true)?;
    }
    d.wrapper_edges.set(true);
    d.frames
        .state_mut()
        .as_mut()
        .expect("activated")
        .contexts
        .insert(n, o);
    Ok(E::obj_raw(o))
}
pub(crate) fn content_window<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = d.el_s(s, this)?;
    if d.doc.borrow().element_name(n) != "iframe" {
        return Err(s.type_error("Illegal invocation"));
    }
    if !old::<E, _>(s, |it| d.is_connected_node(it, n))? {
        return Ok(s.null());
    }
    old_v::<E>(s, |it| context(d, it, n))
}
/// Has the iframe `n` an initial `about:blank` document: no `src` (or an empty one) or `src="about:blank"`, and no `srcdoc`?
fn initial_blank<E: Eng>(d: &Dom<E>, n: NodeId) -> bool {
    let doc = d.doc.borrow();
    let attr = |name: &str| doc.find_atom(name).and_then(|a| doc.attr(n, a));
    attr("srcdoc").is_none() && attr("src").is_none_or(|v| v.trim().is_empty() || v.trim().eq_ignore_ascii_case("about:blank"))
}
/// `iframe.contentDocument`: the initial `about:blank` document of `contentWindow`, else `null` (see the header).
pub(crate) fn content_document<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let w = content_window(d, s, this, a)?;
    // A child page has no document that the parent can read.
    if d.kids.any() && is_object::<E>(s, w) && let Some(n) = d.node_of_s(s, this).filter(|&n| d.is_element(n)) && d.kids.id_of(n).is_some() {
        return Ok(s.null());
    }
    if s.type_of(w) == Type::Null {
        return Ok(w);
    }
    let v = s.get_str(w, "document")?;
    Ok(if is_object::<E>(s, v) { v } else { s.null() })
}
pub(crate) fn length<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    s.charge(d.doc.borrow().len() as u64)?;
    Ok(s.number(nodes(d).len() as f64))
}
pub(crate) fn name<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let n = d.el_s(s, this)?;
    let v = {
        let doc = d.doc.borrow();
        doc.find_atom("name").and_then(|a| doc.attr(n, a)).map(str::to_string)
    };
    d.js_str_s(s, v.as_deref().unwrap_or(""))
}
pub(crate) fn set_name<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let n = d.el_s(s, this)?;
    let t = d.arg_str_s(s, a, 0)?;
    crate::browser_dom::attr_set_s(d, s, n, "name", &t)?;
    Ok(s.undefined())
}
/// The window of the initial `about:blank` document `n` of a frame.
pub(crate) fn window_of<E: Eng>(d: &Dom<E>, n: NodeId) -> Option<E::Obj> {
    d.frames.state()?.docs.get(&n).copied()
}
/// Is `o` the window of a blank frame (it inherits the parent's globals)?
pub(crate) fn is_blank_window<E: Eng>(d: &Dom<E>, o: E::Obj) -> bool {
    d.frames.state().is_some_and(|s| s.docs.values().any(|&w| w == o))
}
/// Is `o` the `Location` of a frame window?
pub(crate) fn is_location<E: Eng>(d: &Dom<E>, o: E::Obj) -> bool {
    d.frames.state().is_some_and(|s| s.locs.contains(&o))
}
/// The frame document that owns `n`, if any: the root of its tree, or the document whose method made it.
pub(crate) fn owner_of<E: Eng>(d: &Dom<E>, n: NodeId) -> Option<NodeId> {
    let s = d.frames.state()?;
    if let Some(&o) = s.owned.get(&n) {
        return Some(o);
    }
    let doc = d.doc.borrow();
    let mut cur = n;
    while let Some(p) = doc.parent(cur) {
        cur = p;
    }
    if s.docs.contains_key(&cur) { Some(cur) } else { s.owned.get(&cur).copied() }
}
/// Record that `made` came from the method of the document `this`, when that is a frame document.
pub(crate) fn note_owner<E: Eng>(d: &Dom<E>, s: &mut Sc<'_, E>, this: Local<'_>, made: Local<'_>) {
    if !d.frames.active() {
        return;
    }
    if let (Some(t), Some(m)) = (d.node_of_s(s, this), d.node_of_s(s, made))
        && let Some(s) = d.frames.state_mut().as_mut().filter(|s| s.docs.contains_key(&t))
    {
        s.owned.insert(m, t);
    }
}
/// The references of the wrapper of the node `n`: the window and the document of its frame.
#[cold]
pub(crate) fn trace_node<E: Eng>(d: &Dom<E>, n: NodeId, t: &mut bare_js_api::Tracer<'_, E::Raw>) {
    if let Some(s) = d.frames.state().as_ref() {
        for &o in s.contexts.get(&n).into_iter().chain(s.docs.get(&n)) {
            crate::browser_sources::visit::<E>(t, E::obj_raw(o));
        }
    }
}
/// The window object `o` of a frame is dead: drop its entries.
pub(crate) fn forget_window<E: Eng>(d: &Dom<E>, o: E::Obj) {
    if let Some(s) = d.frames.state_mut().as_mut() {
        s.contexts.retain(|_, w| *w != o);
        s.docs.retain(|_, w| *w != o);
    }
}
/// The `Location` object `o` of a frame window is dead: drop its entry.
pub(crate) fn prune_loc<E: Eng>(d: &Dom<E>, o: E::Obj) {
    if let Some(s) = d.frames.state_mut().as_mut() {
        s.locs.remove(&o);
    }
}

// ------------------------------------------------------------------------------------------ arena reclaim (`browser_reclaim.rs`)

/// The nodes of iframes with a window, and the nodes that frame documents made. The name list and the answer of the last walk are of the
/// tree of the document: a node that left it moved `Document::name_revision` first.
pub(crate) fn held<E: Eng>(d: &Dom<E>, out: &mut Vec<NodeId>) {
    let Some(s) = d.frames.state() else { return };
    out.extend(s.contexts.keys().copied());
    out.extend(s.docs.keys().copied());
    out.extend(s.owned.iter().flat_map(|(&a, &b)| [a, b]));
}

/// Does a miss of the string key `u` ask the host? A host named object in front of the lazy chain uses it to let the lazy names win over its own names.
fn lazy_chain_name(u: &[u16]) -> bool {
    u.len() >= 3 && u[0] == b'o' as u16 && u[1] == b'n' as u16
        || u.len() > 11 && u[0] == b'H' as u16 && u[1] == b'T' as u16 && u[2] == b'M' as u16 && u[3] == b'L' as u16 && u.ends_with(&[b'E', b'l', b'e', b'm', b'e', b'n', b't'].map(u16::from))
        || browser_name(u)
}

#[cold]
#[inline(never)]
fn browser_name(u: &[u16]) -> bool {
    macro_rules! named { ($name:literal) => { u.iter().copied().eq($name.bytes().map(u16::from)) }; }
    match u.first().copied() {
        Some(102) => named!("frames") || named!("frameElement") || named!("focus") || named!("fonts"),
        Some(101) => named!("eval") || named!("event"),
        Some(97) => named!("animate") || named!("alert") || named!("adoptedStyleSheets"),
        Some(105) => named!("indexedDB") || named!("isSecureContext") || named!("inputEncoding"),
        Some(65) => named!("Animation"),
        Some(73) => u.starts_with(&[73, 68, 66]),
        Some(80) => named!("Path2D"),
        Some(112) => named!("parent") || named!("postMessage") || named!("print") || named!("prompt"),
        Some(116) => named!("top") || named!("toDataURL") || named!("transform"),
        Some(103) => named!("getContext") || named!("getBBox") || named!("getCTM") || named!("getScreenCTM") || named!("getTotalLength") || named!("getPointAtLength") || named!("getComputedTextLength") || named!("getSubStringLength"),
        Some(119) => named!("width") || named!("write") || named!("writeln"),
        Some(104) => named!("height") || named!("hasFocus") || named!("hidden"),
        Some(118) => named!("viewBox") || named!("visibilityState"),
        Some(111) => named!("open") || named!("opener") || named!("outerWidth") || named!("outerHeight"),
        Some(99) => named!("close") || named!("closed") || named!("confirm") || named!("contentType") || named!("compatMode") || named!("characterSet") || named!("charset") || named!("crossOriginIsolated"),
        Some(108) => named!("length") || named!("lastModified"),
        Some(110) => named!("name"),
        Some(115) => named!("speechSynthesis") || named!("screen") || named!("sheet") || named!("styleSheets") || named!("screenX") || named!("screenY"),
        Some(67) => named!("CompressionStream") || named!("CloseEvent") || named!("CSSStyleSheet") || named!("CSSGroupingRule") || named!("CSSImportRule") || named!("CSSFontFaceRule") || named!("CSSKeyframesRule") || named!("CSSKeyframeRule") || named!("CSSConditionRule") || named!("CSSMediaRule") || named!("CSSSupportsRule") || named!("CSSRule") || named!("CSSStyleRule") || named!("CSSRuleList"),
        Some(77) => named!("MediaList"),
        Some(83) => named!("StyleSheetList") || named!("SpeechSynthesis") || named!("SpeechSynthesisUtterance") || named!("SpeechSynthesisEvent") || named!("SpeechSynthesisErrorEvent") || named!("SVGAnimatedString") || (u.starts_with(&[83,86,71]) && u.ends_with(&[69,108,101,109,101,110,116])),
        Some(68) => named!("DocumentType") || named!("DOMException") || named!("DecompressionStream"),
        Some(100) => named!("devicePixelRatio") || named!("designMode") || named!("dir"),
        Some(98) => named!("blur"),
        Some(69) => named!("EventSource"),
        Some(70) => named!("FontFace") || named!("FileReader"),
        Some(84) => named!("TransformStream") || named!("TransformStreamDefaultController") || named!("TextDecoderStream") || named!("TextEncoderStream"),
        Some(87) => named!("WebSocket") || named!("WritableStream") || named!("WritableStreamDefaultWriter") || named!("WritableStreamDefaultController"),
        Some(114) => named!("referrer"),
        _ => false,
    }
}
