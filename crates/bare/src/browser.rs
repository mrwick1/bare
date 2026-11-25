//! The first real DOM/JS bridge: `window`, `document`, nodes and events for the bounded JS evaluator.
//!
//! How it fits together
//! - The DOM is the native `bare_html::dom::Document` behind `Rc<RefCell<Document>>`. JS never sees a copy.
//! - `Dom` implements `bare_js_api::Host`. JS calls reach it as numbered methods (`Dom::scoped`). Getters and setters
//!   are ordinary accessor properties on the prototypes, so `el.id` runs through the same call path and obeys the
//!   same step and 100-call guards.
//! - A JS node is a wrapper object. There is one wrapper for each native node while it is alive, so `a === a`.
//!   A wrapper's ancestors always have wrappers too (made together), so the wrappers of one tree are connected. The
//!   wrapper of the document is a permanent root. Everything the collector can reach from it (attached wrappers,
//!   their listener callbacks) lives. A detached tree lives while JS holds one of its wrappers. When nothing holds it,
//!   its wrappers, listeners and events are pruned. The native node stays in the arena (existing `Document` limit: it
//!   never frees nodes or text), so the caps below bound it.
//! - No document borrow is ever held while JS runs (listeners, getters, `toString`).
//!
//! Caps (`BrowserLimits`): every JS-controlled growth is checked before it happens and ends with
//! `Fault::Limit(LimitKind::Host(name))` (never a catchable JS error): nodes, text bytes, attribute slots, names,
//! wrappers, listeners, events, result arrays, tree depth, selector size, reports. Walks charge the step budget.
//!
//! Supported (tested):
//! - Globals: `window`, `self`, `globalThis` (the same object), `document`, `Node`, `Element`, `HTMLElement`,
//!   `HTMLInputElement`, `HTMLIFrameElement` (inert named windows; no child document or fetch), `HTMLLinkElement`, `HTMLFormElement`, `HTMLButtonElement`, `CharacterData`, `Text`, `Comment`, `Document`, `DocumentFragment` and `ShadowRoot` (illegal constructors), `EventTarget`, `Event`, `CustomEvent`,
//!   `SubmitEvent`, `DOMException`.
//! - `new DocumentFragment()` and `document.createDocumentFragment()` make an empty fragment. A plain fragment (no host) goes in as its children (`Dom::place`): one change group, the fragment is left empty. A shadow root is still `Unsupported`. `before`, `after` and `replaceWith` are on `Element` and `CharacterData`. `replaceChildren` is on `Element` and `DocumentFragment` (one function object, not on `Document`). `insertAdjacentElement`, `insertAdjacentText` and `insertAdjacentHTML` are on `Element`. Installed after every other global (`install_late`).
//! - `HTMLAnchorElement`, `HTMLAreaElement` and `HTMLTemplateElement` (lazy, `browser_elements.rs`): `href` and the URL parts, `template.content`. `new FormData(form)` reads the form data set.
//! - Node: `nodeType nodeName nodeValue parentNode parentElement childNodes firstChild lastChild previousSibling
//!   nextSibling ownerDocument isConnected textContent appendChild insertBefore removeChild replaceChild cloneNode
//!   contains hasChildNodes getRootNode compareDocumentPosition isSameNode isEqualNode lookupNamespaceURI lookupPrefix isDefaultNamespace` and the
//!   `DOCUMENT_POSITION_*` constants (`browser_nodecmp.rs`).
//! - Element: `tagName localName namespaceURI prefix getAttributeNS setAttributeNS hasAttributeNS removeAttributeNS id className getAttribute setAttribute removeAttribute hasAttribute getAttributeNames
//!   children firstElementChild lastElementChild previousElementSibling nextElementSibling childElementCount append
//!   prepend remove matches closest querySelector querySelectorAll` (selectors from `bare_css`).
//! - CharacterData: `data length`. Document: `createElement createElementNS createTextNode createComment getElementById
//!   documentElement head body doctype defaultView querySelector(All)`.
//! - `HTMLElement.dataset` and `SVGElement.dataset` (`DOMStringMap`, `browser_dataset.rs`): live `data-*` attributes with the name conversion, named
//!   properties through the host named-property hook of the engine. MathML elements have no `dataset` (no `MathMLElement` brand).
//! - `<input>`: `value checked type defaultValue defaultChecked` with native state (dirty value, checkedness).
//! - `Element.innerHTML` and `outerHTML` (`browser_fragment.rs`): native fragment parsing and serialization with explicit gates (table, select, template, html, frameset and
//!   foreign contexts are `Unsupported`). Fragment scripts are never run.
//! - `<link>` (HTML namespace only, `HTMLLinkElement`): `rel`, `href`, `as` and `crossOrigin` as reflected attributes. Setting them loads nothing: the page driver loads connected `rel=preload` links (`browser_loader/preload.rs`).
//! - `<input>` text selection (`selectionStart selectionEnd selectionDirection setSelectionRange select`, `select` event task): `browser_selection.rs`.
//! - Events: `Event`, `CustomEvent` (`detail`), `addEventListener` (capture, once, passive; function or `handleEvent`
//!   object), `removeEventListener`, `dispatchEvent`, capture / target / bubble phases through the document to `window`,
//!   `preventDefault`, `stopPropagation`, `stopImmediatePropagation`, `composedPath`, `new EventTarget()`.
//! - A listener that throws is reported (`Browser::take_reports`) and dispatch goes on. A Limit, an Unsupported or a
//!   heap fault stops the dispatch and is the result of the call. The embedder then calls `Browser::survive`: a script budget
//!   (steps and the like) aborts only that task and the page goes on; the other errors stay fatal.
//!
//! Gates and deviations (on purpose, none is silently faked)
//! - `document.images`, `links`, `anchors`, `embeds`, `forms` and `scripts` are snapshot arrays too (`browser_platform_query.rs`).
//!   `document.title` reads and sets the first `<title>` (`browser_dom.rs`).
//! - Lists (`childNodes`, `children`, `querySelectorAll`, `getElementsByTagName`, `getElementsByClassName`) are plain
//!   snapshot arrays, not live `NodeList`/`HTMLCollection`.
//!   The one live `NodeList` is the result of `document.getElementsByName` (`browser_nodelist.rs`).
//! - A JS string with a lone surrogate cannot be stored in the UTF-8 document: `Error::Unsupported`. Valid pairs round-trip.
//! - Namespaces are in `browser_namespace.rs` (read its header): HTML, SVG and MathML elements, attributes in no namespace, XML,
//!   XMLNS and XLink. Any other namespace is `Unsupported`. Shadow roots are a tree foundation only (`browser_shadow.rs`: read its header for the gates). `new Document()`,
//!   `style`, layout APIs, `location` and
//!   network are absent. Timers, `queueMicrotask`, `performance.now` and `MessageChannel` are in `browser_scheduler.rs`. `Element.attributes` and `Attr` are in `browser_attr.rs`.
//! - `addEventListener` option `signal` removes the listener on abort (`browser_abort.rs`). `isTrusted` is false, except for the events of the window and of the
//!   user-like driver inputs (`browser_ui.rs`, `browser_drive.rs`). The events of `Browser::click`, `type_text` and `dispatch` are plain `Event`s
//!   (no `MouseEvent`/`InputEvent` fields), except the `submit` of a click activation: a `SubmitEvent`.
//! - `XMLHttpRequest`, its upload object and `ProgressEvent` are in `browser_xhr.rs` (read its header for the gates). Their events are trusted.
//! - Focus (`browser_focus.rs`): `focus()`, `blur()`, `document.activeElement`, `FocusEvent`. Read its header.
//! - Default actions are NOT implemented: no link navigation, no form validation or native form navigation, no checkbox toggle or
//!   radio-group exclusion on click. Focus does not follow a click. The test driver sets native input state itself, then dispatches events.
//!   The one exception is `Browser::click` at a submit button: it dispatches the `submit` event at the form owner, behind two named
//!   gates (validation, native navigation). `Browser::submit` stays dispatch-only. Read `browser_form.rs`.
//! - `<input>` value sanitization is `browser_controls::sanitize`. `textarea` has a `value` set by script (no
//!   events, no selection API, not painted or submitted).
//! - Microtasks: the test driver runs a checkpoint after each listener it started from the host. Dispatch from JS does not.
//! - `matchMedia`, `classList` and `getElementsBy*` are in `browser_platform.rs` (read its header).
//! - `getComputedStyle` and the read-only computed declaration are in `browser_computed.rs` (read its header: external sheets are supplied by the embedder, used values are gated).
//! - Subclassing host constructors with `class X extends Event` needs `new.target` plumbing in the evaluator (not done).
//!
//! Not browser-verified: nothing here was compared with Chrome. DOM behavior follows the DOM Standard by reading, and
//! is tested against structural expectations written by hand. Only plain `EventTarget`/`Event` semantics have a Node
//! oracle (`tools/js/browser_oracle.mjs`).

use crate::eng_new::New;
use crate::eng::{Desc, Eng, Fault, NamedOp, Ops, Val};
use crate::browser_sources::Roots;
use crate::browser_dom as nd;
use crate::browser_fragment as frag;
use crate::browser_namespace as ns;
use crate::browser_attr as at;
use crate::browser_events as ev;
use crate::browser_form as fm;
use crate::browser_ui as ui;
use crate::browser_selection as sel;
use crate::browser_shadow as sh;
use crate::browser_custom as ce;
pub use crate::browser_scheduler::{Pump, SchedLimits, SchedStats};
use bare_css::selector::Selector;
use bare_html::dom::{Document, NodeData, NodeId, Ns};
use crate::js_limits::{EvalLimits, LimitKind};
use crate::js_limits::HeapLimits;
use bare_js_api::HostFn;
use crate::browser_scoped::{obj_id, run, table, Attrs, Intrinsic, Res, Row, Sc, HS};
use std::cell::{Cell, OnceCell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::hash::{BuildHasherDefault, Hasher};
use std::rc::Rc;
use std::time::Instant;

pub(crate) type R<E, T> = Result<T, Fault<E>>;

/// A multiply hash for maps whose keys the engine numbers (`ObjId`, `NodeId`). SipHash costs about 60 instructions for one
/// key and these maps are read at each host property access. The page cannot choose these numbers.
#[derive(Default)]
pub(crate) struct IdHash(u64);
impl IdHash {
    fn add(&mut self, x: u64) {
        self.0 = (self.0.rotate_left(5) ^ x).wrapping_mul(0x517c_c1b7_2722_0a95);
    }
}
impl Hasher for IdHash {
    fn write(&mut self, b: &[u8]) {
        b.iter().for_each(|&x| self.add(x as u64));
    }
    fn write_u32(&mut self, x: u32) {
        self.add(x as u64);
    }
    fn write_u64(&mut self, x: u64) {
        self.add(x);
    }
    fn write_usize(&mut self, x: usize) {
        self.add(x as u64);
    }
    fn write_isize(&mut self, x: isize) {
        self.add(x as u64);
    }
    fn finish(&self) -> u64 {
        self.0.rotate_left(26) // the multiply leaves the best bits on top; hashbrown reads the low ones
    }
}
pub(crate) type IdMap<K, V> = HashMap<K, V, BuildHasherDefault<IdHash>>;

/// A key that is a slot number of the engine: `ObjId` (its slot is reused, so the whole handle tells a dead object from a new one)
/// or `NodeId` (the reclaim reuses a freed slot, so a holder of a `NodeId` is a root or is forgotten at once: `browser_reclaim.rs`).
pub trait SlotKey: Copy + Eq + std::hash::Hash {
    /// The slots are small numbers (`IdTab` indexes pages by them). A key that is a pointer says `false`: its table is a hash map.
    const DENSE: bool = true;
    fn slot(self) -> usize;
}
impl SlotKey for NodeId {
    fn slot(self) -> usize {
        self.index()
    }
}

const PAGE_BITS: usize = 5;
type Page<K, V> = [Option<(K, V)>; 1 << PAGE_BITS];

/// A table indexed by the slot of its key: two loads and one compare, with no hash. The pages hold 32 entries and exist only
/// where a key lives, so the memory follows the keys that are in use, not the largest slot (a heap of a million objects with
/// ten wrappers holds ten pages and a directory of 8 bytes for each 32 slots). The entry keeps the whole key, so the entry of a
/// dead object never answers for the new object of the same slot. The collector's `retain` frees an empty page and the empty end
/// of the directory. This is the idea of a `ScriptWrappable` field in Blink or an opaque pointer in QuickJS, as a side table,
/// because the DOM crate and the engine crate must not know each other.
pub(crate) struct IdTab<K, V> {
    pages: Vec<Option<Box<Page<K, V>>>>,
    len: usize,
    /// The entries of a key that is no dense slot (`SlotKey::DENSE`). Empty for the other keys.
    sparse: HashMap<K, V, BuildHasherDefault<IdHash>>,
}
impl<K, V> Default for IdTab<K, V> {
    fn default() -> Self {
        IdTab { pages: Vec::new(), len: 0, sparse: HashMap::default() }
    }
}
impl<K: SlotKey, V> IdTab<K, V> {
    pub fn len(&self) -> usize {
        if K::DENSE { self.len } else { self.sparse.len() }
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn get(&self, k: &K) -> Option<&V> {
        if !K::DENSE {
            return self.sparse.get(k);
        }
        let s = k.slot();
        match self.pages.get(s >> PAGE_BITS)? {
            Some(p) => match &p[s & ((1 << PAGE_BITS) - 1)] {
                Some((kk, v)) if kk == k => Some(v),
                _ => None,
            },
            None => None,
        }
    }
    pub fn get_mut(&mut self, k: &K) -> Option<&mut V> {
        if !K::DENSE {
            return self.sparse.get_mut(k);
        }
        let s = k.slot();
        match self.pages.get_mut(s >> PAGE_BITS)? {
            Some(p) => match &mut p[s & ((1 << PAGE_BITS) - 1)] {
                Some((kk, v)) if kk == k => Some(v),
                _ => None,
            },
            None => None,
        }
    }
    pub fn contains_key(&self, k: &K) -> bool {
        self.get(k).is_some()
    }
    pub fn iter(&self) -> impl Iterator<Item = (&K, &V)> {
        self.pages.iter().flatten().flat_map(|p| p.iter().flatten().map(|(k, v)| (k, v))).chain(self.sparse.iter())
    }
    /// The entry of `k` is `v` from now on. The old value comes back if `k` had one.
    pub fn insert(&mut self, k: K, v: V) -> Option<V> {
        if !K::DENSE {
            return self.sparse.insert(k, v);
        }
        let s = k.slot();
        if self.pages.len() <= s >> PAGE_BITS {
            self.pages.resize_with((s >> PAGE_BITS) + 1, || None);
        }
        let page = self.pages[s >> PAGE_BITS].get_or_insert_with(|| Box::new([const { None }; 1 << PAGE_BITS]));
        let old = page[s & ((1 << PAGE_BITS) - 1)].replace((k, v));
        self.len += old.is_none() as usize;
        old.filter(|(kk, _)| *kk == k).map(|(_, v)| v)
    }
    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&K, &mut V)> {
        self.pages.iter_mut().flatten().flat_map(|p| p.iter_mut().flatten().map(|(k, v)| (&*k, v))).chain(self.sparse.iter_mut())
    }
    /// The value of `k`; `v` is stored first if there is none.
    pub fn get_or_insert(&mut self, k: K, v: V) -> &mut V {
        if !self.contains_key(&k) {
            self.insert(k, v);
        }
        self.get_mut(&k).expect("just stored")
    }
    /// Drop the entry of `k`. The page stays: a test for an empty page reads the whole page, so `retain(|_, _| true)` frees the empty
    /// pages after a batch of removes.
    pub fn remove(&mut self, k: &K) -> Option<V> {
        if !K::DENSE {
            return self.sparse.remove(k);
        }
        let s = k.slot();
        let p = self.pages.get_mut(s >> PAGE_BITS)?.as_mut()?;
        let e = &mut p[s & ((1 << PAGE_BITS) - 1)];
        if !e.as_ref().is_some_and(|(kk, _)| kk == k) {
            return None;
        }
        self.len -= 1;
        e.take().map(|(_, v)| v)
    }
    pub fn retain(&mut self, mut keep: impl FnMut(&K, &mut V) -> bool) {
        if !K::DENSE {
            self.sparse.retain(|k, v| keep(k, v));
            return;
        }
        for slot in &mut self.pages {
            let Some(p) = slot else { continue };
            for e in p.iter_mut() {
                if e.as_mut().is_some_and(|(k, v)| !keep(k, v)) {
                    *e = None;
                    self.len -= 1;
                }
            }
            if p.iter().all(Option::is_none) {
                *slot = None;
            }
        }
        while self.pages.last().is_some_and(Option::is_none) {
            self.pages.pop();
        }
        self.pages.shrink_to(self.pages.len() * 2);
    }
}
/// A host method: the DOM, the interpreter, `this`, the arguments.

/// Hard bounds of one browser. Every one is checked before the thing grows.
#[derive(Clone, Copy, Debug)]
pub struct BrowserLimits {
    /// Nodes in the native document arena (it never frees one).
    pub max_nodes: usize,
    /// Bytes in the native text buffer (it never shrinks).
    pub max_text_bytes: usize,
    /// Slots in the native attribute list.
    pub max_attr_slots: usize,
    /// Names (tags, attributes) in the native name table.
    pub max_names: usize,
    /// Live wrappers (and plain event targets).
    pub max_wrappers: usize,
    pub max_listeners: usize,
    pub max_listeners_per_target: usize,
    /// Live event objects.
    pub max_events: usize,
    /// Elements of one result array.
    pub max_results: usize,
    /// Depth of a tree that JS builds, and the longest event path.
    pub max_tree_depth: usize,
    /// Bytes of one selector text. GitHub sends lists of 3 to 4 KB; Chrome has no cap.
    pub max_selector_len: usize,
    /// Bytes of one string that crosses between JS and the document.
    pub max_string_bytes: usize,
    /// Kept listener exception reports. The oldest is dropped (and counted) after this.
    pub max_reports: usize,
    /// Tasks that a script budget may abort (`Browser::survive`). The next one is fatal.
    pub max_task_aborts: usize,
    /// Child pages that are live in one top page, at all depths (`browser_child.rs`). An iframe over it stays empty.
    pub max_child_pages: usize,
    /// Nesting depth of child pages. An iframe over it stays empty.
    pub max_child_depth: usize,
}

impl Default for BrowserLimits {
    fn default() -> Self {
        BrowserLimits {
            max_nodes: 1 << 20,
            max_text_bytes: 64 << 20,
            max_attr_slots: 1 << 21,
            max_names: 1 << 16,
            max_wrappers: 1 << 16,
            max_listeners: 1 << 16,
            max_listeners_per_target: 1024,
            max_events: 1 << 14,
            max_results: 1 << 16,
            max_tree_depth: 512,
            max_selector_len: 1 << 16,
            max_string_bytes: 16 << 20,
            max_reports: 256,
            max_task_aborts: 5,
            max_child_pages: 64,
            max_child_depth: 8,
        }
    }
}

/// Bytes of selector text that `Dom::selectors` keeps parsed. A parsed selector is up to about 50 times its text.
pub(crate) const SEL_CACHE_BYTES: usize = 128 << 10;

/// What keeps traced values for a host key: the listeners of a target, the state of an event object, or a host-filled object
/// (`browser_sources.rs`).
#[derive(Clone, Copy, Debug)]
pub(crate) enum Owner<E: Eng> {
    Target(Target<E>),
    Event(E::Obj),
    /// A `getComputedStyle` object (`browser_computed.rs`).
    Computed(E::Obj),
    /// An `XMLHttpRequest` or its upload object: an event target with an entry in `browser_xhr.rs`.
    Xhr(E::Obj),
    /// An `AbortSignal` (an event target), with an entry in `browser_abort.rs`.
    Abort(E::Obj),
    /// An `AbortController`, with an entry in `browser_abort.rs`.
    Ctl(E::Obj),
    /// A host-filled object. Its kind is in the low bits of its key. `Reserved` is the entry between the key and the object.
    Source(E::Obj),
    Reserved,
    /// A `URL`, `URLSearchParams` or iterator object of the navigation module (`browser_navigation_params.rs`).
    Url(E::Obj),
    Params(E::Obj),
    Iter(E::Obj),
    /// The `Location` of a frame window (`browser_frames.rs`).
    Loc(E::Obj),
    /// A fetch object (`Headers`, `Request`, `Response`, a body stream, a reader or a headers iterator), with an entry in `browser_fetch.rs`.
    Fetch(E::Obj),
    /// A script-made `ReadableStream`, its controller or its reader, with an entry in `browser_streams.rs`.
    Stream(E::Obj),
    /// A `MessagePort`, an event target with an entry in `browser_scheduler.rs`.
    Port(E::Obj),
    /// A `Worker`, an event target with entries in `browser_worker.rs`.
    Worker(E::Obj),
    /// A `MediaQueryList`, an event target with an entry in `browser_platform.rs`.
    Mql(E::Obj),
    /// The window object of a frame, an event target with entries in `browser_frames.rs`.
    Frame(E::Obj),
    /// An iterator of a token list (`browser_platform_class.rs`).
    TokIter(E::Obj),
    /// A `ResizeObserver` (`browser_resize.rs`), a `MutationObserver` (`browser_mutation.rs`) or an `IntersectionObserver`
    /// (`browser_intersection.rs`): its callback and the wrappers of its targets.
    Resize(E::Obj),
    Mo(E::Obj),
    Io(E::Obj),
    /// A `TreeWalker` or `NodeIterator` (`browser_walker.rs`).
    Walk(E::Obj),
    /// A `Range` (`browser_range.rs`), and the `Selection` object.
    Range(E::Obj),
    Sel(E::Obj),
    /// An `Attr` object (`browser_attr.rs`).
    Attr(E::Obj),
    /// A `Blob`, `File`, `FormData` or `FormData` iterator (`browser_web.rs`).
    Web(E::Obj),
    /// A rectangle, point, quad, list or matrix (`browser_geom.rs`). It keeps no reference: its slot goes with it.
    Geom(E::Obj),
    /// An `IdleDeadline` (`browser_idle.rs`). It keeps no reference.
    Idle(E::Obj),
}

impl<E: Eng> Owner<E> {
    /// The listener target that the owner stands for.
    fn target(self) -> Option<Target<E>> {
        match self {
            Owner::Target(t) => Some(t),
            Owner::Xhr(o) | Owner::Abort(o) | Owner::Port(o) | Owner::Worker(o) | Owner::Mql(o) | Owner::Frame(o) => Some(Target::Obj(o)),
            Owner::Event(_) | Owner::TokIter(_) | Owner::Resize(_) | Owner::Mo(_) | Owner::Io(_) | Owner::Walk(_) | Owner::Range(_) | Owner::Sel(_) | Owner::Attr(_) | Owner::Web(_) | Owner::Geom(_) | Owner::Idle(_) | Owner::Ctl(_) | Owner::Computed(_) | Owner::Source(_) | Owner::Reserved | Owner::Url(_) | Owner::Params(_) | Owner::Iter(_) | Owner::Loc(_) | Owner::Fetch(_) | Owner::Stream(_) => None,
        }
    }
}

/// The owners of traced values, by host key (`docs/internal/design-host-tracing.md`). A key is a slot index above `KIND_BITS` bits. The low bits
/// are the kind of a host-filled object (`browser_sources::Source`), and 0 for any other owner. A free slot holds `None`.
#[derive(Default)]
pub(crate) struct Owners<E: Eng> {
    pub slots: Vec<Option<Owner<E>>>,
    free: Vec<u32>,
    /// The key of the global object, which `install` registers: its trace also visits the roots of the page (`Dom::trace_roots`).
    pub win: u32,
    /// The key of each event target object that `State::add_target` registered (`Dom::own` reads it).
    pub objs: IdTab<<E as Eng>::Obj, u32>,
}

/// The bits of a host key below the slot index: the kind of a host-filled object.
pub(crate) const KIND_BITS: u32 = 4;

impl<E: Eng> Owners<E> {
    /// A free host key for `o`, with kind 0.
    pub(crate) fn key(&mut self, o: Owner<E>) -> u32 {
        let slot = match self.free.pop() {
            Some(k) => {
                self.slots[k as usize] = Some(o);
                k
            }
            None => {
                self.slots.push(Some(o));
                self.slots.len() as u32 - 1
            }
        };
        slot << KIND_BITS
    }

    /// The owner of `key`.
    #[inline]
    pub(crate) fn get(&self, key: u32) -> Option<Owner<E>> {
        self.slots.get((key >> KIND_BITS) as usize).copied().flatten()
    }

    /// Set the owner of a key that is in use.
    pub(crate) fn set(&mut self, key: u32, o: Owner<E>) {
        self.slots[(key >> KIND_BITS) as usize] = Some(o);
    }

    pub(crate) fn release(&mut self, key: u32) {
        let k = key >> KIND_BITS;
        self.slots[k as usize] = None;
        self.free.push(k);
    }
}

impl<E: Eng> Dom<E> {
    /// Make the object of `t` the owner of its listeners, once. Call it where a function goes into a listener.
    pub(crate) fn own_listeners(&self, st: &mut State<E>, it: &mut E::Vm, t: Target<E>) {
        if let Some(l) = st.listeners.get_mut(&t) {
            self.own(&mut st.owners, it, t, l);
        }
    }

    /// `own_listeners` for the list `l` of `t`. The key in the list is checked against the owner table, so a freed key that another
    /// owner took is not trusted. A node needs nothing: the key of its wrapper (`set_wrapper`) covers its listeners. A node without a
    /// wrapper has listeners that hold the global object until a wrapper exists (`browser_handlers.rs`).
    #[inline]
    pub(crate) fn own(&self, owners: &mut Owners<E>, it: &mut E::Vm, t: Target<E>, l: &mut Listeners<E>) {
        if l.key != NO_KEY && matches!(owners.get(l.key), Some(x) if x.target() == Some(t)) {
            return;
        }
        // The object whose life the listeners follow: the global object or the target object.
        let o = match t {
            // The global object has its key from `install`.
            Target::Window => {
                l.key = owners.win;
                return;
            }
            Target::Node(_) => return,
            // An XHR object, an upload object or a signal has its key from its making.
            Target::Obj(o) => match owners.objs.get(&o).copied().or_else(|| crate::browser_xhr::key_of(self, o)).or_else(|| crate::browser_abort::key_of(self, o)) {
                Some(k) => {
                    l.key = k;
                    return;
                }
                None => o,
            },
        };
        l.key = owners.key(Owner::Target(t));
        it.set_host_owner_obj(o, l.key);
    }

    /// Make `o` the wrapper of `n`. The wrapper is the owner of its node target: its trace visits the wrappers next to it in the tree
    /// and the listeners of the node.
    pub(crate) fn set_wrapper(&self, st: &mut State<E>, it: &mut E::Vm, n: NodeId, o: E::Obj) {
        let key = st.owners.key(Owner::Target(Target::Node(n)));
        st.by_node.insert(n, it.trace_value(E::obj_raw(o)));
        st.by_obj.insert(o, Target::Node(n));
        it.set_host_owner_obj(o, key);
    }
}

impl<E: Eng> Dom<E> {
    /// Make `o` an owner of kind `w`: the collector calls `Host::trace` when it reaches `o`, and `Host::prune` when `o` is dead.
    pub(crate) fn own_obj(&self, it: &mut E::Vm, o: E::Obj, w: Owner<E>) {
        let k = self.st.borrow_mut().owners.key(w);
        it.set_host_owner_obj(o, k);
    }
}

impl<E: Eng> State<E> {
    /// `o` is a new event target of the page (`Target::Obj`). It is an owner from its making, so its listeners, its entry in `by_obj` and
    /// the entries of its kind end with it (`Host::prune`).
    pub(crate) fn add_target(&mut self, it: &mut E::Vm, o: E::Obj, w: Owner<E>) {
        self.by_obj.insert(o, Target::Obj(o));
        let k = self.owners.key(w);
        self.owners.objs.insert(o, k);
        it.set_host_owner_obj(o, k);
    }

    /// The wrapper object of `n`. Transitional: it reads the raw value, for code that still keeps `ObjId`s.
    #[inline]
    pub(crate) fn wrapper(&self, n: NodeId) -> Option<E::Obj> {
        match E::kind(*self.by_node.get(&n)?.raw()) {
            Val::Object(o) => Some(o),
            _ => None,
        }
    }

    /// Each node with a wrapper, and the wrapper object. Transitional, as `wrapper`.
    pub(crate) fn wrappers(&self) -> impl Iterator<Item = (NodeId, E::Obj)> + '_ {
        self.by_node.iter().filter_map(|(&n, w)| match E::kind(*w.raw()) {
            Val::Object(o) => Some((n, o)),
            _ => None,
        })
    }
}

/// No host key.
pub(crate) const NO_KEY: u32 = u32::MAX;

/// The listeners of one target, and the host key of their owner (`own_listeners`). An entry with a key stays when its list is empty,
/// so the target keeps one key; it goes when its target dies.
pub(crate) struct Listeners<E: Eng> {
    pub key: u32,
    pub list: Vec<Listener<E>>,
}

impl<E: Eng> Default for Listeners<E> {
    fn default() -> Self {
        Listeners { key: NO_KEY, list: Vec::new() }
    }
}

impl<E: Eng> Listeners<E> {
    /// An empty list without an owner key.
    pub(crate) fn removable(&self) -> bool {
        self.list.is_empty() && self.key == NO_KEY
    }
}

impl<E: Eng> std::ops::Deref for Listeners<E> {
    type Target = Vec<Listener<E>>;
    fn deref(&self) -> &Vec<Listener<E>> {
        &self.list
    }
}

impl<E: Eng> std::ops::DerefMut for Listeners<E> {
    fn deref_mut(&mut self) -> &mut Vec<Listener<E>> {
        &mut self.list
    }
}

impl<'a, E: Eng> IntoIterator for &'a Listeners<E> {
    type Item = &'a Listener<E>;
    type IntoIter = std::slice::Iter<'a, Listener<E>>;
    fn into_iter(self) -> Self::IntoIter {
        self.list.iter()
    }
}

/// What a JS event target is on the Rust side.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Target<E: Eng> {
    Window,
    Node(NodeId),
    /// A plain `new EventTarget()`.
    Obj(E::Obj),
}

pub(crate) struct Listener<E: Eng> {
    pub id: u64,
    pub ty: Rc<str>,
    /// A function or an object with `handleEvent`. The owner of the target traces it (`own_listeners`).
    pub cb: bare_js_api::Traced<E::Raw>,
    pub capture: bool,
    pub once: bool,
    pub passive: bool,
    /// The listener of an event handler property (`oninput`): `cb` is the handler's current value (`browser_handlers.rs`).
    pub handler: bool,
}

pub(crate) struct EventState<E: Eng> {
    /// An `ErrorEvent` (`browser_error_event.rs`).
    pub err: bool,
    pub ty: Rc<str>,
    pub bubbles: bool,
    pub cancelable: bool,
    pub composed: bool,
    pub canceled: bool,
    pub stop: bool,
    pub stop_imm: bool,
    pub dispatching: bool,
    pub in_passive: bool,
    pub phase: u8,
    /// The event object is the owner of these (`Owner::Event`).
    pub target: bare_js_api::Traced<E::Raw>,
    pub current: bare_js_api::Traced<E::Raw>,
    pub detail: bare_js_api::Traced<E::Raw>,
    pub path: Vec<Target<E>>,
    pub time: f64,
    /// `FocusEvent.relatedTarget` (null for other events). Traced, so a saved event keeps the node.
    pub related: bare_js_api::Traced<E::Raw>,
    pub is_focus: bool,
    /// `SubmitEvent.submitter` is in `related` (traced). Only a `SubmitEvent` has this set.
    pub is_submit: bool,
    /// `ToggleEvent.oldState` and `newState`. Only a `ToggleEvent` has this set.
    pub toggle: Option<(String, String)>,
    /// The fields of a mouse, pointer, keyboard or input event that the window made (`browser_ui.rs`). `related` holds `relatedTarget`.
    pub ui: Option<Box<crate::browser_ui::Ui>>,
    /// `isTrusted`: set for the events of `browser_ui.rs`.
    pub trusted: bool,
}

/// Is `e` a task budget that aborts a task and not the page (see [`Browser::survive`])?
pub(crate) fn is_budget<E: Eng>(e: &Fault<E>) -> bool {
    matches!(e, Fault::Limit(LimitKind::Steps | LimitKind::CallDepth | LimitKind::TraversalDepth | LimitKind::StatementFrames | LimitKind::DrainJobs | LimitKind::Host("dom-selector")))
}

/// What [`Browser::survive`] decided about the error of a task.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Survive {
    /// A script budget ended the task. It is aborted and logged. Go on.
    Aborted,
    /// Not a script budget: the error stays fatal.
    Fatal,
    /// A script budget, but the page used up its aborts. The text says why. Fatal.
    Exhausted(String),
}

/// A listener exception that did not stop dispatch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    pub event_type: String,
    /// `window`, a lowercase tag name, `#text`, `#document` or `object`.
    pub target: String,
    /// `name: message` of an error object, or the text of the thrown value. Read without running JS.
    pub message: String,
}

/// Native state of an `<input>`: the value and checkedness once JS or the driver changed them.
/// Until then they come from the `value` and `checked` attributes.
#[derive(Clone, Default)]
pub(crate) struct Input {
    pub value: Option<String>,
    pub checked: Option<bool>,
    /// Text selection (`browser_selection.rs`). `None`: caret at the end of the value.
    pub sel: Option<crate::browser_selection::Sel>,
    /// Selection API "has scheduled selectionchange event": a `Task::SelectionChange` of this input is queued.
    pub sc_pending: bool,
    /// The value before the first edit that no `change` event has reported yet (`browser_ui.rs`).
    pub edited: Option<String>,
    /// A user typed in it (the dirty value flag of the standard, for `minlength`).
    pub typed: bool,
    /// The `indeterminate` property of a checkbox.
    pub indeterminate: bool,
    /// The files of an `<input type=file>` that a driver set. The bytes are read when a script or a submit asks for them.
    pub files: Vec<FileRef>,
}

/// A file of a file input: where it is on disk, and what `File` reports.
#[derive(Clone, Debug)]
pub(crate) struct FileRef {
    pub path: std::path::PathBuf,
    pub name: String,
    pub modified_ms: f64,
}

#[derive(Default)]
pub(crate) struct State<E: Eng> {
    /// The state override of a caret (`browser_editing.rs`): where the caret was and what bold, italic and underline are to be.
    #[allow(clippy::type_complexity)] // A one-off tuple: a named alias would add nothing.
    pub style_override: Option<((NodeId, u32), [Option<bool>; 3])>,
    /// The wrapper of each node that has one. The wrapper is the owner of its node target (`set_wrapper`). The table is not
    /// visited as a whole, so its entries are weak.
    pub by_node: IdTab<NodeId, bare_js_api::Traced<<E as Eng>::Raw>>,
    pub by_obj: IdTab<<E as Eng>::Obj, Target<E>>,
    pub listeners: IdMap<Target<E>, Listeners<E>>,
    pub n_listeners: usize,
    pub owners: Owners<E>,
    pub next_id: u64,
    pub events: IdMap<<E as Eng>::Obj, EventState<E>>,
    pub inputs: IdMap<NodeId, Input>,
    /// Bytes of the dirty values in `inputs`.
    pub input_bytes: usize,
    pub reports: VecDeque<Report>,
    pub dropped_reports: u64,
    pub sel_cache: HashMap<String, Rc<Vec<Selector>>>,
    /// Bytes of the selector texts in `sel_cache`.
    pub sel_cache_bytes: usize,
    /// True while the test driver runs a dispatch from the host (a microtask checkpoint follows each listener).
    pub driver: bool,
    /// The focused element and the transition guards (`browser_focus.rs`).
    pub focus: crate::browser_focus::FocusState,
    /// Pointer state and counters of the window input (`browser_ui.rs`).
    pub ui: crate::browser_ui::UiState,
    /// Event handler properties and attributes (`browser_handlers.rs`).
    pub hd: crate::browser_handlers::Hd<E>,
}

#[derive(Clone, Copy)]
pub(crate) struct Protos<E: Eng> {
    pub et: E::Obj,
    pub node: E::Obj,
    pub element: E::Obj,
    pub html: E::Obj,
    pub svg: E::Obj,
    pub input: E::Obj,
    pub iframe: E::Obj,
    pub link: E::Obj,
    pub form: E::Obj,
    pub button: E::Obj,
    pub submit: E::Obj,
    pub toggle: E::Obj,
    pub chardata: E::Obj,
    pub text: E::Obj,
    pub comment: E::Obj,
    pub document: E::Obj,
    pub fragment: E::Obj,
    pub shadow: E::Obj,
    pub event: E::Obj,
    pub custom: E::Obj,
    pub focus: E::Obj,
    pub mouse: E::Obj,
    pub pointer: E::Obj,
    pub keyboard: E::Obj,
    pub inputev: E::Obj,
    pub domexc: E::Obj,
}

#[derive(Clone, Copy)]
pub(crate) enum P {
    Et,
    Node,
    Element,
    Html,
    Input,
    Link,
    Form,
    Button,
    Submit,
    Toggle,
    CharData,
    Text,
    Document,
    Fragment,
    Shadow,
    Event,
    Custom,
    Focus,
    Mouse,
    Pointer,
    Keyboard,
    InputEv,
}

/// The DOM host of one realm.
pub struct Dom<E: Eng = New> {
    pub(crate) doc: Rc<RefCell<Document>>,
    pub(crate) st: RefCell<State<E>>,
    pub(crate) lim: BrowserLimits,
    pub(crate) p: OnceCell<Protos<E>>,
    pub(crate) global: OnceCell<E::Obj>,
    selector_scope: Rc<std::cell::Cell<Option<NodeId>>>,
    pub(crate) doc_wrapper: OnceCell<E::Obj>,
    /// Some element has an object that its wrapper must keep (`browser_sources::trace_node`), or some frame has a window. It starts
    /// `false` and is never reset, so the trace of a wrapper of a page without such objects tests one flag. Set it where such an
    /// object is stored: `Dom::new_source`, the `Attr` tables and the frame tables.
    pub(crate) wrapper_edges: Cell<bool>,
    /// `(handler, needs new)` for each host function id.
    /// The functions of the new form (and, in a test build, every old one): the registry of the interface `Host::call`.
    pub(crate) scoped: RefCell<Vec<crate::browser_scoped::Scoped<E>>>,
    /// Prototypes and interface objects: permanent roots.
    pub(crate) fixed: RefCell<Vec<E::Obj>>,
    pub(crate) start: Instant,
    /// The time on the virtual clock (`bare_net::vclock`) when this document began: its time origin. 0 when that clock is off.
    pub(crate) vorigin: f64,
    pub(crate) sched: RefCell<crate::browser_scheduler::Sched<E>>,
    pub(crate) fetch: crate::browser_fetch::Fetch<E>,
    /// Script-made `ReadableStream`s (`browser_streams.rs`).
    pub(crate) streams: crate::browser_streams::Streams<E>,
    /// `console` and its bounded record sink (`browser_console.rs`).
    pub(crate) console: crate::browser_console::Console<E>,
    /// Tasks that a script budget aborted so far (`Browser::survive`).
    pub(crate) aborted: std::cell::Cell<usize>,
    /// Navigation, history and storage state (`browser_navigation.rs`).
    pub(crate) nav: crate::browser_navigation::Nav<E>,
    pub(crate) scripts: crate::browser_loader::Scripts<E>,
    /// `matchMedia`, `classList`, `getElementsBy*` (`browser_platform.rs`).
    pub(crate) plat: crate::browser_platform::Platform<E>,
    /// `HTMLElement.style` / `CSSStyleDeclaration` (`browser_style.rs`).
    pub(crate) style: crate::browser_style::Style<E>,
    /// `getComputedStyle` and its sheet state (`browser_computed.rs`).
    pub(crate) computed: crate::browser_computed::Computed<E>,
    pub(crate) cssom: crate::browser_cssom::Cssom<E>,
    /// `offsetWidth` and `offsetHeight` (`browser_geometry.rs`).
    pub(crate) geometry: crate::browser_geometry::Geometry,
    /// `document.implementation`, once read (`browser_implementation.rs`). It is also in `fixed`.
    pub(crate) parsing_protos: std::cell::Cell<[Option<E::Obj>; 3]>,
    pub(crate) implementation: std::cell::Cell<Option<E::Obj>>,
    /// The prototype and the constructor of `PerformanceObserver`, once read (`browser_perf_observer.rs`). Both are also in `fixed`.
    pub(crate) perf_observer: std::cell::Cell<Option<E::Obj>>,
    pub(crate) perf_observer_ctor: std::cell::Cell<Option<E::Obj>>,
    /// The timeline interfaces, once read (`browser_perf.rs`).
    pub(crate) perf_ifaces: std::cell::Cell<Option<crate::browser_perf::Ifaces<E>>>,
    /// The interface `Screen`, once read or once `window.screen` is made (`screen_iface`).
    pub(crate) screen_iface: std::cell::Cell<Option<crate::browser_geom::Iface<E>>>,
    /// The `mark` and `measure` entries of `performance` (`browser_perf.rs`).
    pub(crate) frames: crate::browser_frames::Frames<E>,
    pub(crate) kids: crate::browser_child::Kids<E>,
    pub(crate) perf_entries: RefCell<Vec<crate::browser_perf::Entry<E::Obj>>>,
    /// The times of the load of this document, for its navigation entry (`browser_perf.rs`).
    pub(crate) load_times: RefCell<crate::browser_perf::LoadTimes>,
    /// The prototype and constructor of each interface of `browser_html.rs`, once made.
    pub(crate) html_ifaces: RefCell<Vec<Option<(E::Obj, E::Obj)>>>,
    /// A global of `browser_html.rs` was read: new wrappers get their interface prototype.
    pub(crate) html_on: std::cell::Cell<bool>,
    /// The `returnValue` and the modal flag of each `<dialog>` that has one (`browser_html_members.rs`).
    pub(crate) dialogs: RefCell<std::collections::HashMap<NodeId, (String, bool)>>,
    /// The first `<link>` wrapper gave `HTMLLinkElement.prototype` its reflected members (`browser_dom::link_proto`).
    pub(crate) link_done: std::cell::Cell<bool>,
    /// Scroll offsets, page bars and `scroll` events (`browser_scroll.rs`).
    pub(crate) scrolling: crate::browser_scroll::Scrolling,
    /// `ResizeObserver` and its rendering step (`browser_resize.rs`).
    pub(crate) resize: crate::browser_resize::Resize<E>,
    /// CSS transition and animation events (`browser_motion.rs`).
    pub(crate) motion: crate::browser_motion::MotionState,
    /// `requestAnimationFrame` (`browser_frame.rs`).
    pub(crate) frame: crate::browser_frame::Frame<E>,
    /// `requestIdleCallback` (`browser_idle.rs`).
    pub(crate) idle: crate::browser_idle::Idle<E>,
    pub(crate) basics: crate::browser_basics::Basics<E>,
    /// `MutationObserver` (`browser_mutation.rs`).
    pub(crate) mo: crate::browser_mutation::Mo<E>,
    pub(crate) io: crate::browser_intersection::Io<E>,
    pub(crate) walk: crate::browser_walker::Walk<E>,
    pub(crate) range: crate::browser_range::Ranges<E>,
    /// The elements whose `click()` is running (`browser_element.rs`).
    pub(crate) clicking: RefCell<Vec<NodeId>>,
    /// `Element.attributes`, `NamedNodeMap` and `Attr` (`browser_attr.rs`).
    pub(crate) attr: crate::browser_attr::AttrHost<E>,
    /// `Document.getElementsByName` and the live `NodeList` (`browser_nodelist.rs`).
    pub(crate) nl: crate::browser_nodelist::NodeListHost<E>,
    /// `HTMLSelectElement`, `HTMLOptionElement` and the option lists (`browser_select.rs`).
    pub(crate) sel: crate::browser_select::SelHost<E>,
    /// `HTMLElement.dataset` and `DOMStringMap` (`browser_dataset.rs`).
    pub(crate) dataset: crate::browser_dataset::DatasetHost<E>,
    /// `AbortController` and `AbortSignal` (`browser_abort.rs`).
    pub(crate) abort: crate::browser_abort::AbortHost<E>,
    /// `XMLHttpRequest` and `ProgressEvent` (`browser_xhr.rs`).
    pub(crate) xhr: crate::browser_xhr::XhrHost<E>,
    /// `crypto`, `navigator`, `Blob`, `File` and `FormData` (`browser_web.rs`).
    pub(crate) web: crate::browser_web::WebHost<E>,
    /// `HTMLAnchorElement`, `HTMLAreaElement` and `HTMLTemplateElement` (`browser_elements.rs`).
    pub(crate) els: crate::browser_elements::Els<E>,
    /// `HTMLMediaElement` and its kin (`browser_media.rs`).
    pub(crate) media: crate::browser_media::Media<E>,
    /// `Image`, `decode()` and the picture requests of scripts (`browser_img.rs`).
    pub(crate) img: crate::browser_img::Img<E>,
    /// `HTMLBodyElement`, `HTMLFrameSetElement` and `ErrorEvent` (`browser_handlers.rs`).
    pub(crate) hh: crate::browser_handlers::Hh<E>,
    /// Constraint validation, `labels` and `form.reset()` (`browser_controls.rs`).
    pub(crate) ctl: crate::browser_controls::CtlHost<E>,
    /// Custom elements (`browser_custom.rs`).
    pub(crate) ce: crate::browser_custom::Ce<E>,
    /// Rectangles, points, quads and matrices (`browser_geom.rs`, `browser_matrix.rs`).
    pub(crate) geom: crate::browser_geom::Geom<E>,
    /// `Worker` (`browser_worker.rs`).
    pub(crate) worker: crate::browser_worker::WorkerHost<E>,
    pub(crate) broadcast: crate::browser_broadcast::BroadcastHost<E>,
    /// Freeing the nodes of dead trees (`browser_reclaim.rs`).
    pub(crate) reclaim: crate::browser_reclaim::Reclaim,
}

impl<E: Eng> Dom<E> {
    /// The values that live as long as the page: prototypes, constructors, queued callbacks. The trace of the global object visits them
    /// (`Owner::Target(Target::Window)`).
    pub(crate) fn trace_roots(&self, t: &mut bare_js_api::Tracer<'_, E::Raw>) {
        let out = &mut Roots::<E>(t);
        out.extend(self.fixed.borrow().iter().map(|&o| E::obj_raw(o)));
        out.extend(self.doc_wrapper.get().map(|&o| E::obj_raw(o)));
        self.sched_roots(out);
        self.console.roots(out);
        self.scripts_roots(out);
        crate::browser_platform::roots(self, out);
        crate::browser_style::roots(self, out);
        crate::browser_cssom::roots(self, out);
        crate::browser_computed::roots(self, out);
        at::roots(self, out);
        crate::browser_nodelist::roots(self, out);
        crate::browser_dataset::roots(self, out);
        crate::browser_web::roots(self, out);
        crate::browser_perf::roots(self, out);
        crate::browser_elements::roots(self, out);
        crate::browser_media::roots(self, out);
        crate::browser_img::roots(self, out);
        crate::browser_handlers::roots(self, out);
        self.resize_roots(out);
        self.frame_roots(out);
        self.idle_roots(out);
        self.mo_roots(out);
        self.io_roots(out);
        self.walk_roots(out);
        self.range_roots(out);
        self.ce_roots(out);
        crate::browser_geom::roots(self, out);
        crate::browser_worker::roots(self, out);
        crate::browser_broadcast::roots(self, out);
    }
}

/// The host of `bare-js-api`: the host functions, the references that owners keep and the entries that dead owners free
/// (`docs/internal/design-host-tracing.md`). The old `Host` below only fills host-indexed objects and writes host-named ones: the interface has no
/// method for these two yet.
impl<'p, E: Eng> bare_js_api::Host<E::Vm> for Dom<E> {
    #[inline]
    fn call<'s>(&self, s: &mut bare_js_api::Scope<'s, E::Vm>, id: u32, this: bare_js_api::Local<'s>, args: bare_js_api::Args<'s>, is_new: bool) -> Result<bare_js_api::Local<'s>, bare_js_api::Error<'s>> {
        self.call_scoped(s, id, this, args, is_new)
    }

    fn trace(&self, key: u32, t: &mut bare_js_api::Tracer<'_, E::Raw>) {
        let st = self.st.borrow();
        let owner = st.owners.get(key);
        match owner {
            Some(owner @ (Owner::Target(_) | Owner::Xhr(_) | Owner::Abort(_) | Owner::Port(_) | Owner::Worker(_) | Owner::Mql(_) | Owner::Frame(_))) => {
                let Some(tg) = owner.target() else { return };
                if st.n_listeners != 0 {
                    for l in st.listeners.get(&tg).into_iter().flatten() {
                        t.visit(&l.cb);
                    }
                }
                // A wrapper keeps the wrappers of its parent, its children, its shadow host or root and its template content. So the wrappers
                // of a tree are one cycle: a detached tree lives while a script holds one of its wrappers, and an expando is not lost.
                if let Target::Node(n) = tg {
                    let doc = self.doc.borrow();
                    // Separate loops: a chain checks its state for each child.
                    for x in doc.children(n) {
                        if let Some(w) = st.by_node.get(&x) {
                            t.visit(w);
                        }
                    }
                    for x in [doc.parent(n), doc.shadow_host(n), doc.shadow_root(n), doc.template_content(n)].into_iter().flatten() {
                        if let Some(w) = st.by_node.get(&x) {
                            t.visit(w);
                        }
                    }
                    drop(doc);
                    // And the host-filled objects of its node (`dataset`, `style`, `classList`, `attributes`, a frame's window).
                    crate::browser_sources::trace_node(self, n, t);
                } else if matches!(tg, Target::Window) {
                    self.trace_roots(t);
                }
                match owner {
                    Owner::Xhr(o) => crate::browser_xhr::trace(self, o, t),
                    Owner::Abort(o) => crate::browser_abort::trace(self, o, t),
                    Owner::Port(o) | Owner::Worker(o) => self.sched_trace_port(o, t),
                    _ => {}
                }
            }
            Some(Owner::TokIter(o)) => crate::browser_platform::trace_iter(self, o, t),
            Some(Owner::Resize(o)) => self.resize_trace(o, t),
            Some(Owner::Mo(o)) => self.mo_trace(o, t),
            Some(Owner::Io(o)) => self.io_trace(o, t),
            Some(Owner::Walk(o)) => self.walk_trace(o, t),
            Some(Owner::Sel(o)) => self.range_trace(o, t),
            Some(Owner::Attr(o)) => at::trace(self, o, t),
            Some(Owner::Web(o)) => crate::browser_web::trace(self, o, t),
            Some(Owner::Ctl(o)) => crate::browser_abort::trace_ctl(self, o, t),
            Some(Owner::Source(o)) => crate::browser_sources::trace(self, key, o, t),
            Some(Owner::Fetch(o)) => crate::browser_fetch::trace(self, o, t),
            Some(Owner::Stream(o)) => crate::browser_streams::trace(self, o, t),
            Some(Owner::Event(o)) => {
                self.sched_trace_event(o, t);
                let Some(e) = st.events.get(&o) else { return };
                for v in [&e.target, &e.current, &e.detail, &e.related] {
                    t.visit(v);
                }
                // The path lives only during a dispatch. An object target can only be first, and `target` holds it.
                for p in &e.path {
                    if let Target::Node(n) = p
                        && let Some(w) = st.by_node.get(n)
                    {
                        t.visit(w);
                    }
                }
            }
            Some(Owner::Computed(o)) => crate::browser_computed::trace_owner(self, &st, o, t),
            Some(w @ (Owner::Url(_) | Owner::Params(_) | Owner::Iter(_))) => crate::browser_navigation::trace_owner(self, w, t),
            Some(Owner::Loc(_) | Owner::Reserved | Owner::Range(_) | Owner::Geom(_) | Owner::Idle(_)) | None => {}
        }
    }

    fn prune(&self, dead: &[u32]) {
        let mut guard = self.st.borrow_mut();
        let st = &mut *guard;
        let n = st.by_node.len();
        let datasets = crate::browser_dataset::count(self);
        let mut ports_died = false;
        // The entries of the objects of the kinds that keep a list of objects or a total are dropped in one pass for each kind.
        let (mut resize, mut mo, mut io, mut walk, mut range, mut attr, mut web) = (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for &k in dead {
            let owner = st.owners.get(k);
            // A dead event target object takes its entry in `by_obj` and its listeners.
            if let Some(t @ Target::Obj(o)) = owner.and_then(Owner::target) {
                st.by_obj.remove(&o);
                st.owners.objs.remove(&o);
                if !st.listeners.is_empty()
                    && let Some(l) = st.listeners.remove(&t)
                {
                    st.n_listeners -= l.len();
                }
            }
            // A dead wrapper takes its identity entries and the listeners of its node.
            match owner {
                Some(Owner::Xhr(o)) => crate::browser_xhr::forget(self, o),
                Some(Owner::Port(o)) => {
                    self.sched_forget_port(o);
                    crate::browser_broadcast::forget(self, o);
                    ports_died = true;
                }
                Some(Owner::Worker(o)) => crate::browser_worker::forget(self, o),
                Some(Owner::Mql(o)) => crate::browser_platform::forget_mql(self, o),
                Some(Owner::Frame(o)) => crate::browser_frames::forget_window(self, o),
                Some(Owner::TokIter(o)) => crate::browser_platform::forget_iter(self, o),
                Some(Owner::Resize(o)) => resize.push(o),
                Some(Owner::Mo(o)) => mo.push(o),
                Some(Owner::Io(o)) => io.push(o),
                Some(Owner::Walk(o)) => walk.push(o),
                Some(Owner::Range(o)) => range.push(o),
                Some(Owner::Attr(o)) => attr.push(o),
                Some(Owner::Web(o)) => web.push(o),
                Some(Owner::Geom(o)) => crate::browser_geom::forget(self, o),
                Some(Owner::Idle(o)) => self.idle_forget(o),
                Some(Owner::Event(o)) => {
                    st.events.remove(&o);
                    self.sched_forget_event(o);
                    crate::browser_xhr::forget_event(self, o);
                }
                Some(Owner::Target(t @ Target::Node(n))) => {
                    if let Some(w) = st.by_node.remove(&n) {
                        if let Val::Object(o) = E::kind(*w.raw()) {
                            st.by_obj.remove(&o);
                        }
                        if !st.listeners.is_empty()
                            && let Some(l) = st.listeners.remove(&t)
                        {
                            st.n_listeners -= l.len();
                        }
                    }
                }
                Some(Owner::Computed(o)) => crate::browser_computed::drop_owner(self, o),
                Some(Owner::Source(o)) => crate::browser_sources::prune(self, k, o),
                Some(w @ (Owner::Url(_) | Owner::Params(_) | Owner::Iter(_))) => crate::browser_navigation::prune_owner(self, w),
                Some(Owner::Loc(o)) => crate::browser_frames::prune_loc(self, o),
                _ => {}
            }
            st.owners.release(k);
        }
        if st.by_node.len() != n {
            st.by_node.retain(|_, _| true);
        }
        if ports_died {
            self.sched_after_ports();
        }
        for (dead, forget) in [(&resize, Self::resize_forget as fn(&Dom<E>, &[E::Obj])), (&mo, Self::mo_forget), (&io, Self::io_forget), (&walk, Self::walk_forget), (&range, Self::range_forget), (&attr, at::forget), (&web, crate::browser_web::forget)] {
            if !dead.is_empty() {
                forget(self, dead);
            }
        }
        // After the keys are released.
        crate::browser_abort::prune(self, st);
        crate::browser_fetch::prune(self, st);
        crate::browser_streams::prune(self, st);
        crate::browser_dataset::compact(self, datasets);
    }

    /// The fill step of a host-indexed object. The tables that hold the entries still take the raw object (`Interp`, `ObjId`).
    fn fill<'s>(&self, s: &mut bare_js_api::Scope<'s, E::Vm>, object: bare_js_api::Local<'s>, _key: u32, index: u32) -> Result<(), bare_js_api::Error<'s>> {
        use bare_js_api::Engine as _;
        let Val::Object(owner) = E::kind(s.engine().raw(object)) else { return Ok(()) };
        let it = s.engine();
        let r = if index == crate::browser_frames::FILL {
            crate::browser_frames::fill(self, it, owner, index).map(|_| ())
        } else {
            crate::browser_sources::fill(self, it, owner, index).and_then(|done| if done { Ok(()) } else { at::fill(self, it, owner, index) })
        };
        r.map_err(|e| it.api_err(e))
    }

    fn named_write<'s>(&self, s: &mut bare_js_api::Scope<'s, E::Vm>, object: bare_js_api::Local<'s>, _key: u32, name: &[u16], op: bare_js_api::Write<'s>) -> Result<bool, bare_js_api::Error<'s>> {
        use bare_js_api::Engine as _;
        let it = s.engine();
        let Val::Object(owner) = E::kind(it.raw(object)) else { return Ok(false) };
        let val = |it: &E::Vm, l: Option<bare_js_api::Local<'s>>| l.map(|l| it.raw(l));
        let accessor = |it: &E::Vm, l: Option<bare_js_api::Local<'s>>| val(it, l).map(|v| if let Val::Object(o) = E::kind(v) { Some(o) } else { None });
        let op = match op {
            bare_js_api::Write::Set(v) => NamedOp::Set(it.raw(v)),
            bare_js_api::Write::Delete => NamedOp::Delete,
            bare_js_api::Write::Define(d) => NamedOp::Define(Desc::<E> {
                value: val(it, d.value),
                writable: d.writable,
                get: accessor(it, d.get),
                set: accessor(it, d.set),
                enumerable: d.enumerable,
                configurable: d.configurable,
            }),
        };
        let r = if self.storage_area(owner).is_some() { crate::browser_navigation_storage::write(self, it, owner, name, op) } else { crate::browser_dataset::write(self, it, owner, name, op) };
        r.map_err(|e| it.api_err(e))
    }

    #[inline]
    fn indexed_len(&self, object: &E::Raw, key: u32) -> Result<usize, bare_js_api::Pending> {
        crate::browser_sources::indexed_len(self, object, key)
    }
    #[inline]
    fn indexed_item(&self, object: &E::Raw, key: u32, index: usize) -> Result<bare_js_api::Item<E::Raw>, bare_js_api::Pending> {
        crate::browser_sources::indexed_item(self, object, key, index)
    }
    #[inline]
    fn indexed_name(&self, object: &E::Raw, key: u32, name: &[u16]) -> Result<Option<u32>, bare_js_api::Pending> {
        crate::browser_sources::indexed_name(self, object, key, name)
    }
    #[inline]
    fn indexed_names(&self, object: &E::Raw, key: u32) -> Result<Vec<Vec<u16>>, bare_js_api::Pending> {
        crate::browser_sources::indexed_names(self, object, key)
    }
    #[inline]
    fn named_has(&self, object: &E::Raw, key: u32, name: &[u16]) -> Result<bool, bare_js_api::Pending> {
        crate::browser_sources::named_has(self, object, key, name)
    }
    #[inline]
    fn named_get(&self, object: &E::Raw, key: u32, name: &[u16]) -> Result<Option<bare_js_api::Named<E::Raw>>, bare_js_api::Pending> {
        crate::browser_sources::named_get(self, object, key, name)
    }
    #[inline]
    fn named_len(&self, object: &E::Raw, key: u32) -> Result<usize, bare_js_api::Pending> {
        crate::browser_sources::named_len(self, object, key)
    }
    #[inline]
    fn named_keys(&self, object: &E::Raw, key: u32) -> Result<Vec<Vec<u16>>, bare_js_api::Pending> {
        crate::browser_sources::named_keys(self, object, key)
    }

    fn lazy_member<'s>(&self, s: &mut bare_js_api::Scope<'s, E::Vm>, object: bare_js_api::Local<'s>, what: bare_js_api::Lazy<'_>) -> Result<(), bare_js_api::Error<'s>> {
        let it = s.engine();
        let Val::Object(owner) = E::kind(bare_js_api::Engine::raw(it, object)) else { return Ok(()) };
        let r = match what {
            bare_js_api::Lazy::Name(n) => crate::browser_handlers::lazy_key(self, it, owner, n),
            bare_js_api::Lazy::All => crate::browser_handlers::lazy_all(self, it, owner),
            bare_js_api::Lazy::Symbols => crate::browser_handlers::tags(self, it),
        };
        r.map_err(|e| it.api_err(e))
    }

    fn report_exception<'s>(&self, s: &mut bare_js_api::Scope<'s, E::Vm>, thrown: bare_js_api::Local<'s>) {
        let it = s.engine();
        let v = bare_js_api::Engine::raw(it, thrown);
        self.report(it, v, "microtask", Target::Window);
    }

    fn between_jobs<'s>(&self, s: &mut bare_js_api::Scope<'s, E::Vm>) -> Result<(), bare_js_api::Error<'s>> {
        let it = s.engine();
        self.run_inline_scripts(it).map_err(|e| it.api_err(e))
    }

    fn promise_rejections<'s>(&self, s: &mut bare_js_api::Scope<'s, E::Vm>, unhandled: &[bare_js_api::Local<'s>], handled: &[bare_js_api::Local<'s>], dropped: u32) -> Result<(), bare_js_api::Error<'s>> {
        let it = s.engine();
        let raw = |it: &mut E::Vm, l: &[bare_js_api::Local<'s>]| l.iter().map(|&l| bare_js_api::Engine::raw(it, l)).collect::<Vec<_>>();
        let (unhandled, handled) = (raw(it, unhandled), raw(it, handled));
        crate::browser_rejection::notify(self, it, &unhandled, &handled, dropped).map_err(|e| it.api_err(e))
    }
}

pub(crate) fn domexc_codes(name: &str) -> u32 {
    match name {
        "IndexSizeError" => 1,
        "HierarchyRequestError" => 3,
        "WrongDocumentError" => 4,
        "InvalidCharacterError" => 5,
        "NoModificationAllowedError" => 7,
        "NotFoundError" => 8,
        "NotSupportedError" => 9,
        "InUseAttributeError" => 10,
        "InvalidStateError" => 11,
        "InvalidModificationError" => 13,
        "NamespaceError" => 14,
        "InvalidAccessError" => 15,
        "TypeMismatchError" => 17,
        "NetworkError" => 19,
        "URLMismatchError" => 21,
        "InvalidNodeTypeError" => 24,
        "SyntaxError" => 12,
        "SecurityError" => 18,
        "QuotaExceededError" => 22,
        "DataCloneError" => 25,
        "AbortError" => 20,
        "TimeoutError" => 23,
        _ => 0,
    }
}

// Constants are defined only when the public constructor is first requested.
fn domexception_global<E: Eng>(d: &Dom<E>, it: &mut E::Vm) -> R<E, ()> {
    let g = it.global_object();
    let key = d.key(it, "DOMException")?;
    if it.h_peek_own(g, key)?.is_some() { return Ok(()); }
    let proto = d.p().domexc;
    let f = d.add_fn_s_obj(it, domexception_ctor_s, "DOMException", 0, HostFn::CtorNewTarget)?;
    let kp = d.key(it, "prototype")?;
    it.define_data(f, kp, E::obj_raw(proto), false, false, false)?;
    let kc = d.key(it, "constructor")?;
    it.define_data(proto, kc, E::obj_raw(f), true, false, true)?;
    crate::browser_lazy::tag(d, it, proto, "DOMException")?;
    for (i, name) in ["INDEX_SIZE_ERR", "DOMSTRING_SIZE_ERR", "HIERARCHY_REQUEST_ERR", "WRONG_DOCUMENT_ERR", "INVALID_CHARACTER_ERR", "NO_DATA_ALLOWED_ERR", "NO_MODIFICATION_ALLOWED_ERR", "NOT_FOUND_ERR", "NOT_SUPPORTED_ERR", "INUSE_ATTRIBUTE_ERR", "INVALID_STATE_ERR", "SYNTAX_ERR", "INVALID_MODIFICATION_ERR", "NAMESPACE_ERR", "INVALID_ACCESS_ERR", "VALIDATION_ERR", "TYPE_MISMATCH_ERR", "SECURITY_ERR", "NETWORK_ERR", "ABORT_ERR", "URL_MISMATCH_ERR", "QUOTA_EXCEEDED_ERR", "TIMEOUT_ERR", "INVALID_NODE_TYPE_ERR", "DATA_CLONE_ERR"].iter().enumerate() {
        let k = d.key(it, name)?;
        for o in [f, proto] { it.define_data(o, k, E::num((i + 1) as f64), false, true, false)?; }
    }
    it.define_data(g, key, E::obj_raw(f), true, false, true)?;
    d.fixed.borrow_mut().push(f);
    Ok(())
}

fn domexception_ctor_s<'s, E: Eng>(d: &Dom<E>, s: &mut crate::browser_scoped::Sc<'s, E>, _: bare_js_api::Local<'s>, a: bare_js_api::Args<'s>) -> crate::browser_scoped::Res<'s, bare_js_api::Local<'s>> {
    let mut values = [s.undefined(); 2];
    for (i, default) in ["", "Error"].iter().enumerate() {
        let v = crate::browser_scoped::arg_s::<E>(s, a, i);
        values[i] = if s.type_of(v) == bare_js_api::Type::Undefined { d.js_str_s(s, default)? } else {
            let t = s.to_string(v)?;
            let n = s.as_str(t).map_or(0, |x| x.len());
            if n > d.lim.max_string_bytes { return Err(d.lim_s(s, "dom-string")); }
            s.charge((n / 32) as u64)?;
            t
        };
    }
    let default = d.obj_local(s, d.p().domexc);
    let proto = s.proto_from_new_target(default)?;
    let o = s.new_error(proto, "")?;
    let name_len = s.as_str(values[1]).map_or(0, |x| x.len());
    let code = if name_len <= 32 { domexc_codes(&s.display(values[1])) } else { 0 };
    let code = s.number(code as f64);
    let attrs = bare_js_api::Attrs { writable: true, enumerable: false, configurable: true };
    for (name, v) in [("message", values[0]), ("name", values[1]), ("code", code)] {
        s.define_str(o, name, v, attrs)?;
    }
    Ok(o)
}

#[cold]
pub(crate) fn define_window<E: Eng>(d: &Dom<E>, it: &mut E::Vm, name: &str) -> R<E, bool> {
    if name == "DOMException" {
        domexception_global(d, it)?;
        return Ok(true);
    }
    if name == "eval" {
        // The engine's own `%eval%`: a call `eval(x)` by that name is a direct eval.
        let (f, k) = (it.eval_function_o()?, d.key(it, "eval")?);
        it.define_data(it.global_object(), k, E::obj_raw(f), true, false, true)?;
        return Ok(true);
    }
    if name == "event" { crate::browser_gaps::define_event(d, it)?; return Ok(true); }
    if name == "DocumentType" { run(it, |s| crate::browser_implementation::doctype_define(d, s, None).map(|_| ()))?; return Ok(true); }
    if !matches!(name,"frames"|"parent"|"top"|"length"|"name"|"screen"){return Ok(false);}
    let g = it.global_object();
    let key=d.key(it,name)?;
    if it.h_peek_own(g,key)?.is_some(){return Ok(true);}
    if name == "length" {
        let f = d.add_fn_s_obj(it, crate::browser_frames::length, "get length", 0, bare_js_api::HostFn::Method)?;
        d.define_accessor(it, g, key, f, None)?;
        return Ok(true);
    }
    let value = match name {
        "frames" | "parent" | "top" => E::obj_raw(g),
        "length" => E::num(0.0),
        "name" => d.js_str(it, "")?,
        "screen" => { window_screen(d,it)?; return Ok(true); },
        _ => unreachable!(),
    };
    it.define_data(g,key,value,!matches!(name,"top"|"length"),true,name!="top")?;
    Ok(true)
}

/// The interface `Screen`: no constructor, no members of its own. `window.screen` is its instance.
fn screen_iface<E: Eng>(d: &Dom<E>, it: &mut E::Vm) -> R<E, crate::browser_geom::Iface<E>> {
    if let Some(i) = d.screen_iface.get() {
        return Ok(i);
    }
    let i = crate::browser_geom::make_s(d, it, it.object_prototype(), "Screen", illegal_constructor, HostFn::Ctor)?;
    crate::browser_geom::finish_s(d, it, i, "Screen", &[])?;
    d.screen_iface.set(Some(i));
    Ok(i)
}

fn ensure_screen<E: Eng>(d: &Dom<E>, it: &mut E::Vm) -> R<E, ()> {
    screen_iface(d, it).map(|_| ())
}

crate::browser_lazy::lazy_global!(lazy_screen, "Screen", ensure_screen, |d| d.screen_iface.get().expect("made").ctor);

// The virtual display has the default headless dimensions. Viewport resizing does not resize it.
#[cold]
fn window_screen<E: Eng>(d: &Dom<E>, it: &mut E::Vm) -> R<E, ()> {
    let proto = screen_iface(d, it)?.proto;
    let o = d.obj(it, proto)?;
    crate::browser_lazy::tag(d, it, o, "Screen")?;
    for (name, value) in [("width", 1280.0), ("height", 800.0), ("availWidth", 1280.0), ("availHeight", 800.0), ("availLeft", 0.0), ("availTop", 0.0), ("colorDepth", 24.0), ("pixelDepth", 24.0)] {
        let k = d.key(it, name)?;
        it.define_data(o, k, E::num(value), false, true, true)?;
    }
    let k = d.key(it, "screen")?;
    it.define_data(it.global_object(), k, E::obj_raw(o), true, false, true)
}

impl<E: Eng> Dom<E> {
    pub(crate) fn install_on(self: &Rc<Self>, it: &mut E::Vm) -> R<E, ()> {
        bare_js_api::Engine::set_host(it, self.clone());
        it.set_sources(self.clone());
        it.host_scope_o(|it| {
            run(it, |s| {
                self.install(s)?;
                self.install_scheduler(s)
            })?;
            crate::browser_frames::activate(self, it)
        })
    }

    pub(crate) fn p(&self) -> Protos<E> {
        *self.p.get().expect("installed")
    }

    pub(crate) fn lim(&self, what: &'static str) -> Fault<E> {
        Fault::Limit(LimitKind::Host(what))
    }

    #[inline(always)]
    pub(crate) fn obj(&self, it: &mut E::Vm, proto: E::Obj) -> R<E, E::Obj> {
        it.h_new_object(Some(proto))
    }

    #[inline(always)]
    pub(crate) fn key(&self, it: &mut E::Vm, name: &str) -> R<E, E::Key> {
        it.h_key(name)
    }

    /// A string value (the JS string has the same UTF-16 as `s`).
    #[inline]
    pub(crate) fn js_str(&self, it: &mut E::Vm, s: &str) -> R<E, E::Raw> {
        if s.len() > self.lim.max_string_bytes { return Err(self.lim("dom-string")); }
        it.charge_o((s.len() / 32) as u64)?;
        match it.h_intern(s) {
            Ok(s) => Ok(E::str_raw(s)),
            Err(e) => Err(e),
        }
    }

    /// Like `js_str`, for a body of text: an equal live text is the same string (`Heap::shared_text`).
    pub(crate) fn js_text(&self, it: &mut E::Vm, s: &str) -> R<E, E::Raw> {
        if s.len() > self.lim.max_string_bytes { return Err(self.lim("dom-string")); }
        it.charge_o((s.len() / 32) as u64)?;
        match it.h_shared_text(s) {
            Ok(s) => Ok(E::str_raw(s)),
            Err(e) => Err(e),
        }
    }

    /// `ToString(v)` as UTF-8. A lone surrogate cannot be stored: `Unsupported`, never a lossy copy.
    pub(crate) fn string_arg(&self, it: &mut E::Vm, v: E::Raw) -> R<E, String> {
        let s = it.to_js_string(v)?;
        let len = it.h_str_len(s)?;
        if len > self.lim.max_string_bytes { return Err(self.lim("dom-string")); }
        it.charge_o((len / 32) as u64)?;
        let units = match it.h_str_units(s) {
            Ok(u) => u,
            Err(e) => return Err(e),
        };
        if units.len() > self.lim.max_string_bytes {
            return Err(self.lim("dom-string"));
        }
        let bytes = char::decode_utf16(units.iter().copied()).try_fold(0usize, |n, c| {
            c.map(|c| n.saturating_add(c.len_utf8()))
        }).map_err(|_| Fault::Unsupported("lone surrogate in a DOM string (the native document is UTF-8)"))?;
        if bytes > self.lim.max_string_bytes {
            return Err(self.lim("dom-string"));
        }
        String::from_utf16(&units).map_err(|_| Fault::Unsupported("lone surrogate in a DOM string (the native document is UTF-8)"))
    }

    /// A DOMException-like error to throw: an object that inherits from `Error.prototype`, with `name`, `message`, `code`.
    pub(crate) fn dom_err(&self, it: &mut E::Vm, name: &str, msg: &str) -> Fault<E> {
        let r = (|| -> R<E, E::Raw> {
            let o = it.new_error_o(self.p().domexc, msg)?;
            let n = self.js_str(it, name)?;
            let (kn, kc) = (self.key(it, "name")?, self.key(it, "code")?);
            it.define_data(o, kn, n, true, false, true)?;
            it.define_data(o, kc, E::num(domexc_codes(name) as f64), true, false, true)?;
            Ok(E::obj_raw(o))
        })();
        match r {
            Ok(v) => Fault::Throw(v),
            Err(e) => e,
        }
    }

    // ------------------------------------------------------------ wrappers

    /// The wrapper of `node`. It is made with the wrappers of all its ancestors (see the header).
    pub(crate) fn wrap(&self, it: &mut E::Vm, node: NodeId) -> R<E, E::Raw> {
        if let Some(o) = self.st.borrow().wrapper(node) {
            return Ok(E::obj_raw(o));
        }
        let mut chain = Vec::new();
        {
            let (doc, st) = (self.doc.borrow(), self.st.borrow());
            let mut cur = Some(node);
            while let Some(n) = cur {
                if st.by_node.contains_key(&n) {
                    break;
                }
                it.charge_o(1)?;
                chain.push(n);
                cur = doc.parent(n);
            }
            let attrs = self.attr.s.borrow();
            if st.by_obj.len() + attrs.attrs.len() + attrs.maps.len() + self.nl.s.borrow().lists.len() + chain.len() > self.lim.max_wrappers {
                return Err(self.lim("dom-wrappers"));
            }
        }
        let p = self.p();
        let mut first = None;
        for &n in chain.iter() {
            let proto = {
                let doc = self.doc.borrow();
                let proto = match *doc.data(n) {
                    // The brand follows the namespace of the native node, not its name: an SVG `input` is not an `<input>`.
                    NodeData::Element { name, .. } => match doc.element_ns(n) {
                        Ns::Svg => crate::browser_svg::wrapper_proto(self,it,doc.atom_str(name))?,
                        Ns::MathMl => p.element,
                        _ => match doc.atom_str(name) {
                            "iframe" => { crate::browser_frames::activate(self, it)?; p.iframe },
                            "link" => crate::browser_dom::link_proto(self, it)?,
                            "style" => crate::browser_cssom::wrapper_proto(self,it)?,
                            "a" | "area" | "template" | "img" | "slot" => crate::browser_elements::proto(self, it, doc.atom_str(name))?,
                            t @ ("audio" | "video" | "track") => crate::browser_media::proto(self, it, n, t)?,
                            t @ ("input" | "form" | "button" | "label") => crate::browser_controls::proto_for(self, it, t)?,
                            "script" => self.scripts.proto.get().copied().unwrap_or(p.html),
                            t @ ("body" | "frameset") => crate::browser_handlers::body_proto(self, it, usize::from(t == "frameset"))?[0],
                            t @ ("select" | "option" | "optgroup" | "textarea") => crate::browser_select::proto_for(self, it, t)?,
                            n if crate::browser_custom::is_unknown(n) => crate::browser_custom::unknown_proto(self, it)?,
                            t => crate::browser_html::proto(self, it, t)?,
                        },
                    },
                    NodeData::Text(_) => p.text,
                    NodeData::Comment(_) => p.comment,
                    NodeData::Document => p.document,
                    NodeData::ShadowRoot if doc.shadow_host(n).is_some() => p.shadow,
                    NodeData::ShadowRoot => p.fragment,
                    NodeData::Doctype(_) => crate::browser_implementation::doctype_proto(self, it)?,
                };
                // A custom element whose wrapper was collected gets the prototype of its definition again.
                if self.ce.on.get() { self.ce_proto(&doc, n, proto) } else { proto }
            };
            let o = self.obj(it, proto)?;
            self.set_wrapper(&mut self.st.borrow_mut(), it, n, o);
            first.get_or_insert(o);
        }
        Ok(E::obj_raw(first.expect("chain has the node")))
    }

    pub(crate) fn wrap_opt(&self, it: &mut E::Vm, n: Option<NodeId>) -> R<E, E::Raw> {
        match n {
            Some(n) => self.wrap(it, n),
            None => Ok(E::null()),
        }
    }

    pub(crate) fn node_of(&self, v: E::Raw) -> Option<NodeId> {
        let Val::Object(o) = E::kind(v) else { return None };
        match self.st.borrow().by_obj.get(&o) {
            Some(&Target::Node(n)) => Some(n),
            _ => None,
        }
    }

    pub(crate) fn nd(&self, it: &mut E::Vm, this: E::Raw) -> R<E, NodeId> {
        match self.node_of(this) {
            Some(n) => Ok(n),
            None => Err(it.type_error_o("Illegal invocation")),
        }
    }

    pub(crate) fn data(&self, n: NodeId) -> NodeData {
        *self.doc.borrow().data(n)
    }

    pub(crate) fn is_element(&self, n: NodeId) -> bool {
        matches!(self.data(n), NodeData::Element { .. })
    }

    // ------------------------------------------------------------ selectors

    /// Parse (and cache) a selector list. The names it mentions go in the document name table: bounded first.
    pub(crate) fn selectors(&self, it: &mut E::Vm, text: &str) -> R<E, Rc<Vec<Selector>>> {
        if let Some(s) = self.st.borrow().sel_cache.get(text) {
            return Ok(s.clone());
        }
        if text.len() > self.lim.max_selector_len {
            return Err(self.lim("dom-selector"));
        }
        // A parse costs up to about 1,050 instructions for each byte (a list of one-letter tags) and a step about 370, so 4 steps per byte.
        it.charge_o(text.len() as u64 * 4)?;
        let parsed = {
            let mut doc = self.doc.borrow_mut();
            // Only tag and attribute names are new names, and each one needs a byte and a separator.
            if doc.name_count() + text.len() / 2 + 1 > self.lim.max_names {
                return Err(self.lim("dom-names"));
            }
            bare_css::selector::parse_scoped(text, &mut doc, bare_css::selector::Mode::Script, Some(self.selector_scope.clone()))
        };
        let Some(list) = parsed else { return Err(self.dom_err(it, "SyntaxError", &format!("'{text}' is not a valid selector"))) };
        // `a b c d e f g` can backtrack a lot: refuse more than five descendant or later-sibling combinators. A shorter chain is charged
        // before it runs (`nd::match_work`).
        if list.iter().any(|s| nd::backtracking(s) > 5) {
            return Err(self.lim("dom-selector-complexity"));
        }
        let list = Rc::new(list);
        let mut st = self.st.borrow_mut();
        if st.sel_cache.len() >= 256 || st.sel_cache_bytes + text.len() > SEL_CACHE_BYTES {
            st.sel_cache.clear();
            st.sel_cache_bytes = 0;
        }
        st.sel_cache_bytes += text.len();
        st.sel_cache.insert(text.to_string(), list.clone());
        Ok(list)
    }

    pub(crate) fn selectors_at(&self, it: &mut E::Vm, text: &str, scope: NodeId) -> R<E, Rc<Vec<Selector>>> {
        self.selector_scope.set(Some(scope));
        self.selectors(it, text)
    }

    // ------------------------------------------------------------ install

    fn pick(&self, p: P) -> E::Obj {
        let ps = self.p();
        match p {
            P::Et => ps.et,
            P::Node => ps.node,
            P::Element => ps.element,
            P::Html => ps.html,
            P::Input => ps.input,
            P::Link => ps.link,
            P::Form => ps.form,
            P::Button => ps.button,
            P::Submit => ps.submit,
            P::Toggle => ps.toggle,
            P::CharData => ps.chardata,
            P::Text => ps.text,
            P::Document => ps.document,
            P::Fragment => ps.fragment,
            P::Shadow => ps.shadow,
            P::Event => ps.event,
            P::Custom => ps.custom,
            P::Focus => ps.focus,
            P::Mouse => ps.mouse,
            P::Pointer => ps.pointer,
            P::Keyboard => ps.keyboard,
            P::InputEv => ps.inputev,
        }
    }

    pub(crate) fn install<'s>(&self, s: &mut Sc<'s, E>) -> Res<'s, ()> {
        let g = s.global();
        let go = obj_id::<E>(s, g).expect("the global is an object");
        let _ = self.global.set(go);
        let ep = s.intrinsic(Intrinsic::ErrorPrototype);
        let ep = obj_id::<E>(s, ep).expect("an object");
        let et = self.plain_s(s)?;
        let node = self.obj_s(s, et)?;
        let element = self.obj_s(s, node)?;
        let html = self.obj_s(s, element)?;
        let svg = self.obj_s(s, element)?;
        let input = self.obj_s(s, html)?;
        let iframe = self.obj_s(s, html)?;
        let link = self.obj_s(s, html)?;
        let form = self.obj_s(s, html)?;
        let button = self.obj_s(s, html)?;
        let chardata = self.obj_s(s, node)?;
        let text = self.obj_s(s, chardata)?;
        let comment = self.obj_s(s, chardata)?;
        let document = self.obj_s(s, node)?;
        let fragment = self.obj_s(s, node)?;
        let shadow = self.obj_s(s, fragment)?;
        let event = self.plain_s(s)?;
        let custom = self.obj_s(s, event)?;
        let focus = self.obj_s(s, event)?;
        let submit = self.obj_s(s, event)?;
        let toggle = self.obj_s(s, event)?;
        let mouse = self.obj_s(s, event)?;
        let pointer = self.obj_s(s, mouse)?;
        let keyboard = self.obj_s(s, event)?;
        let inputev = self.obj_s(s, event)?;
        let domexc = self.obj_s(s, ep)?;
        let _ = self.p.set(Protos { et, node, element, html, svg, input, iframe, link, form, button, submit, toggle, chardata, text, comment, document, fragment, shadow, event, custom, focus, mouse, pointer, keyboard, inputev, domexc });
        // The global object is `window`: its prototype is `Window.prototype`, an EventTarget.
        let win = self.obj_s(s, et)?;
        let wl = self.obj_local(s, win);
        s.set_prototype_of(g, Some(wl))?;
        let wkey = {
            let mut st = self.st.borrow_mut();
            st.by_obj.insert(go, Target::Window);
            st.owners.win = st.owners.key(Owner::Target(Target::Window));
            st.owners.win
        };
        s.set_host_owner(g, wkey)?;
        let mut fixed = vec![win, et, node, element, html, svg, input, iframe, link, form, button, submit, toggle, chardata, text, comment, document, fragment, shadow, event, custom, focus, mouse, pointer, keyboard, inputev, domexc];

        // methods and accessors. One function object serves every target of a row (`Element.prototype.append === DocumentFragment.prototype.append`,
        // unlike Chrome): a realm pays for each function it makes, and most rows of the table have 2 or 3 targets.
        let targets = ALL_P.map(|p| self.obj_local(s, self.pick(p)));
        self.install_rows(s, &targets, &defs::T::<E>::V)?;

        // interface objects
        let ifaces: [(&str, E::Obj, Option<HS<E>>); 26] = [
            ("EventTarget", et, None),
            ("Window", win, None),
            ("Node", node, None),
            ("Element", element, None),
            ("HTMLElement", html, None),
            ("SVGElement", svg, None),
            ("HTMLInputElement", input, None),
            ("HTMLIFrameElement", iframe, None),
            ("HTMLLinkElement", link, None),
            ("HTMLFormElement", form, None),
            ("HTMLButtonElement", button, None),
            ("CharacterData", chardata, None),
            ("Text", text, None),
            ("Comment", comment, None),
            ("Document", document, None),
            ("DocumentFragment", fragment, None),
            ("ShadowRoot", shadow, None),
            ("Event", event, None),
            ("CustomEvent", custom, None),
            ("FocusEvent", focus, None),
            ("SubmitEvent", submit, None),
            ("ToggleEvent", toggle, Some(crate::browser_details::toggle_event_ctor as HS<E>)),
            ("MouseEvent", mouse, None),
            ("PointerEvent", pointer, None),
            ("KeyboardEvent", keyboard, None),
            ("InputEvent", inputev, None),
        ];
        let mut ctors = Vec::new();
        for (name, proto, ctor) in ifaces {
            let f = match (name, ctor) {
                ("DocumentFragment", _) => self.add_fn_s(s, nd::fragment_ctor_s, name, 0, HostFn::CtorNewTarget)?,
                ("SubmitEvent", _) => self.add_fn_s(s, fm::submit_event_ctor_s, name, 1, HostFn::CtorNewTarget)?,
                (_, Some(h)) => self.add_fn_s(s, h, name, u32::from(name != "EventTarget" && !name.starts_with("HTML")), HostFn::CtorNewTarget)?,
                (_, None) if name.starts_with("HTML") => ce::interface_ctor_s(self, s, name, false)?,
                (_, None) => match ctor_s(name) {
                    Some(h) => self.add_fn_s(s, h, name, u32::from(name != "EventTarget"), HostFn::CtorNewTarget)?,
                    None => self.add_fn_s(s, illegal_constructor, name, 0, HostFn::Method)?,
                },
            };
            let f = self.link_s(s, f, proto, name)?;
            fixed.push(f);
            ctors.push((name, f));
        }
        // constants
        for (iface, consts) in [
            ("Node", &[("ELEMENT_NODE", 1.0), ("TEXT_NODE", 3.0), ("COMMENT_NODE", 8.0), ("DOCUMENT_NODE", 9.0), ("DOCUMENT_TYPE_NODE", 10.0), ("DOCUMENT_FRAGMENT_NODE", 11.0), ("DOCUMENT_POSITION_DISCONNECTED", 1.0), ("DOCUMENT_POSITION_PRECEDING", 2.0), ("DOCUMENT_POSITION_FOLLOWING", 4.0), ("DOCUMENT_POSITION_CONTAINS", 8.0), ("DOCUMENT_POSITION_CONTAINED_BY", 16.0), ("DOCUMENT_POSITION_IMPLEMENTATION_SPECIFIC", 32.0)][..]),
            ("Event", &[("NONE", 0.0), ("CAPTURING_PHASE", 1.0), ("AT_TARGET", 2.0), ("BUBBLING_PHASE", 3.0)][..]),
        ] {
            let f = ctors.iter().find(|c| c.0 == iface).expect("interface").1;
            let proto = if iface == "Node" { node } else { event };
            let (f, proto) = (self.obj_local(s, f), self.obj_local(s, proto));
            for &(n, v) in consts {
                let v = s.number(v);
                let attrs = Attrs { writable: false, enumerable: true, configurable: false };
                s.define_str(f, n, v, attrs)?;
                s.define_str(proto, n, v, attrs)?;
            }
        }
        *self.fixed.borrow_mut() = fixed;

        // globals: window, self, document
        let docw = self.wrap_s(s, self.doc.borrow().root())?;
        let _ = self.doc_wrapper.set(obj_id::<E>(s, docw).expect("an object"));
        for (name, v) in [("window", g), ("self", g), ("document", docw)] {
            s.define_str(g, name, v, Attrs::ALL)?;
        }
        crate::browser_navigation::install(self, s)?;
        crate::browser_fetch::install(self, s)?;
        crate::browser_loader::install(self, s)?;
        self.install_platform(s)?;
        at::install(self, s)?;
        crate::browser_nodelist::install(self, s)?;
        crate::browser_dataset::install(self, s)?;
        crate::browser_abort::install(self, s)?;
        crate::browser_editing::install(self, s)?;
        crate::browser_worker::install(self, s)?;
        crate::browser_broadcast::install(self, s)?;
        crate::browser_xhr::install(self, s)?;
        crate::browser_encoding::install(self, s)?;
        crate::browser_web::install(self, s)?;
        crate::browser_console::install(self, s)?;
        crate::browser_css_ns::install(self, s)?;
        crate::browser_style::install(self, s)?;
        crate::browser_computed::install(self, s)?;
        crate::browser_geometry::install(self, s)?;
        crate::browser_element::install(self, s)?;
        crate::browser_scroll::install(self, s)?;
        crate::browser_rects::install(self, s)?;
        crate::browser_resize::install(self, s)?;
        crate::browser_frame::install(self, s)?;
        crate::browser_idle::install(self, s)?;
        crate::browser_mutation::install(self, s)?;
        crate::browser_intersection::install(self, s)?;
        crate::browser_walker::install(self, s)?;
        crate::browser_range::install(self, s)?;
        crate::browser_handlers::install(self, s)?;
        crate::browser_error_event::install(self, s)?;
        crate::browser_canvas::install(self, s)?;
        crate::browser_rejection::install(self, s)?;
        crate::browser_perf_observer::install(self, s)?;
        crate::browser_perf::install_interfaces(self, s)?;
        table!(lazy = [
            Row::both(1, "Screen", lazy_screen),
        ]);
        let g = s.global();
        self.install_rows(s, &[g], &lazy::T::<E>::V)?;
        crate::browser_select::install(self, s)?;
        // Last: a new host function earlier in this list moves the id of every one after it.
        crate::browser_elements::install(self, s)?;
        nd::install_late(self, s)?;
        crate::browser_controls::install(self, s)?;
        crate::browser_media::install(self, s)?;
        crate::browser_img::install(self, s)?;
        crate::browser_custom::install(self, s)?;
        crate::browser_geom::install(self, s)?;
        crate::browser_basics::install(self, s)
    }

    pub(crate) fn define_accessor(&self, it: &mut E::Vm, o: E::Obj, key: E::Key, get: E::Obj, set: Option<E::Obj>) -> R<E, ()> {
        let d = Desc::<E> { get: Some(Some(get)), set: Some(set), enumerable: Some(true), configurable: Some(true), ..Default::default() };
        match it.h_define_own(o, key, &d) {
            Ok(_) => Ok(()),
            Err(e) => Err(e),
        }
    }
}

/// The constructors of the interface objects that are on the interface form.
fn ctor_s<E: Eng>(name: &str) -> Option<crate::browser_scoped::HS<E>> {
    Some(match name {
        "EventTarget" => ev::event_target_ctor,
        "Event" => ev::event_ctor,
        "CustomEvent" => ev::custom_event_ctor,
        "FocusEvent" => crate::browser_focus::focus_event_ctor,
        "MouseEvent" => crate::browser_uievent::ctor::<0, _>,
        "PointerEvent" => crate::browser_uievent::ctor::<1, _>,
        "KeyboardEvent" => crate::browser_uievent::ctor::<2, _>,
        "InputEvent" => crate::browser_uievent::ctor::<3, _>,
        _ => return None,
    })
}

pub(crate) fn illegal_constructor<'s, E: Eng>(_: &Dom<E>, s: &mut crate::browser_scoped::Sc<'s, E>, _: bare_js_api::Local<'s>, _: bare_js_api::Args<'s>) -> crate::browser_scoped::Res<'s, bare_js_api::Local<'s>> {
    Err(s.type_error("Illegal constructor"))
}

use P::*;

const PARENT: &[P] = &[Element, Document, Fragment];
const CHILD: &[P] = &[Element, CharData];

/// The mask of the targets of a row: bit `p` is the prototype `P::p` in the order of `ALL_P`.
const fn mask(on: &[P]) -> u32 {
    let (mut i, mut m) = (0, 0);
    while i < on.len() {
        m |= 1 << on[i] as u32;
        i += 1;
    }
    m
}

const fn ms<E: Eng>(on: &[P], name: &'static str, len: u32, h: HS<E>) -> Row<E> {
    Row::method(mask(on), name, len, h)
}

const fn gs<E: Eng>(on: &[P], name: &'static str, h: HS<E>) -> Row<E> {
    Row::get(mask(on), name, h, None)
}

const fn asr<E: Eng>(on: &[P], name: &'static str, get: HS<E>, set: HS<E>) -> Row<E> {
    Row::get(mask(on), name, get, Some(set))
}

/// The prototypes in the order of `P`: the targets of the table `defs::T`.
const ALL_P: [P; 22] = [Et, Node, Element, Html, Input, Link, Form, Button, Submit, Toggle, CharData, Text, Document, Fragment, Shadow, Event, Custom, Focus, Mouse, Pointer, Keyboard, InputEv];

// Every host method. The order is the order of function ids, but nothing depends on it.
table!(defs = [
    ms(&[Et], "addEventListener", 2, ev::add_event_listener),
    ms(&[Et], "removeEventListener", 2, ev::remove_event_listener),
    ms(&[Et], "dispatchEvent", 1, ev::dispatch_event),
    gs(&[Node], "nodeType", nd::node_type),
    gs(&[Node], "nodeName", nd::node_name),
    asr(&[Node], "nodeValue", nd::node_value, nd::set_node_value),
    gs(&[Node], "parentNode", nd::parent_node),
    gs(&[Node], "parentElement", nd::parent_element),
    gs(&[Node], "childNodes", nd::child_nodes),
    gs(&[Node], "firstChild", nd::first_child),
    gs(&[Node], "lastChild", nd::last_child),
    gs(&[Node], "previousSibling", nd::previous_sibling),
    gs(&[Node], "nextSibling", nd::next_sibling),
    gs(&[Node], "ownerDocument", nd::owner_document),
    gs(&[Node], "isConnected", nd::is_connected),
    asr(&[Node], "textContent", nd::text_content_s, nd::set_text_content_s),
    ms(&[Node], "appendChild", 1, nd::append_child_s),
    ms(&[Node], "insertBefore", 2, nd::insert_before),
    ms(&[Node], "removeChild", 1, nd::remove_child),
    ms(&[Node], "replaceChild", 2, nd::replace_child),
    ms(&[Node], "cloneNode", 0, nd::clone_node),
    ms(&[Node], "contains", 1, crate::browser_nodecmp::contains),
    ms(&[Node], "compareDocumentPosition", 1, crate::browser_nodecmp::compare_document_position),
    ms(&[Node], "isSameNode", 1, crate::browser_nodecmp::is_same_node),
    ms(&[Node], "isEqualNode", 1, crate::browser_nodecmp::is_equal_node),
    ms(&[Node], "lookupNamespaceURI", 1, crate::browser_nodecmp::lookup_namespace_uri),
    ms(&[Node], "lookupPrefix", 1, crate::browser_nodecmp::lookup_prefix),
    ms(&[Node], "isDefaultNamespace", 1, crate::browser_nodecmp::is_default_namespace),
    ms(&[Node], "getRootNode", 0, nd::get_root_node),
    ms(&[Node], "hasChildNodes", 0, nd::has_child_nodes),
    gs(PARENT, "children", nd::children),
    gs(PARENT, "firstElementChild", nd::first_element_child),
    gs(PARENT, "lastElementChild", nd::last_element_child),
    gs(PARENT, "childElementCount", nd::child_element_count),
    ms(PARENT, "querySelector", 1, nd::query_selector),
    ms(PARENT, "querySelectorAll", 1, nd::query_selector_all),
    ms(PARENT, "append", 0, nd::append),
    ms(PARENT, "prepend", 0, nd::prepend),
    gs(&[Element], "tagName", nd::tag_name),
    gs(&[Element], "localName", nd::local_name),
    gs(&[Element], "namespaceURI", ns::namespace_uri),
    gs(&[Element], "prefix", ns::prefix),
    gs(&[Element], "previousElementSibling", nd::previous_element_sibling),
    gs(&[Element, Text], "assignedSlot", crate::browser_slot::assigned_slot),
    gs(&[Element], "nextElementSibling", nd::next_element_sibling),
    asr(&[Element, Shadow], "innerHTML", frag::inner_html, frag::set_inner_html),
    asr(&[Element], "outerHTML", frag::outer_html, frag::set_outer_html),
    asr(&[Element], "id", nd::id, nd::set_id),
    asr(&[Element], "className", nd::class_name, nd::set_class_name),
    asr(&[Element], "lang", nd::lang, nd::set_lang),
    ms(&[Element], "getAttribute", 1, nd::get_attribute_s),
    ms(&[Element], "setAttribute", 2, nd::set_attribute),
    ms(&[Element], "removeAttribute", 1, nd::remove_attribute_s),
    ms(&[Element], "hasAttribute", 1, nd::has_attribute_s),
    ms(&[Element], "getAttributeNames", 0, nd::get_attribute_names),
    gs(&[Element], "attributes", at::attributes),
    ms(&[Element], "hasAttributes", 0, at::has_attributes),
    ms(&[Element], "getAttributeNode", 1, at::get_attribute_node),
    ms(&[Element], "getAttributeNodeNS", 2, at::get_attribute_node_ns),
    ms(&[Element], "setAttributeNode", 1, at::set_attribute_node),
    ms(&[Element], "setAttributeNodeNS", 1, at::set_attribute_node),
    ms(&[Element], "removeAttributeNode", 1, at::remove_attribute_node),
    ms(&[Element], "getAttributeNS", 2, ns::get_attribute_ns),
    ms(&[Element], "setAttributeNS", 3, ns::set_attribute_ns),
    ms(&[Element], "hasAttributeNS", 2, ns::has_attribute_ns),
    ms(&[Element], "removeAttributeNS", 2, ns::remove_attribute_ns),
    ms(&[Element], "attachShadow", 1, sh::attach_shadow),
    gs(&[Element], "shadowRoot", sh::shadow_root),
    gs(&[Shadow], "mode", sh::mode),
    gs(&[Shadow], "delegatesFocus", sh::flag_bit::<1, _>),
    gs(&[Shadow], "slotAssignment", sh::slot_assignment),
    gs(&[Shadow], "clonable", sh::flag_bit::<2, _>),
    gs(&[Shadow], "serializable", sh::flag_bit::<4, _>),
    gs(&[Shadow], "host", sh::host),
    ms(&[Element], "matches", 1, nd::matches_selector_s),
    ms(&[Element], "closest", 1, nd::closest),
    ms(CHILD, "remove", 0, nd::remove),
    asr(&[CharData], "data", nd::node_value_text, nd::set_node_value_text),
    gs(&[CharData], "length", nd::data_length),
    ms(&[CharData], "substringData", 2, crate::browser_chardata::substring_data),
    ms(&[CharData], "appendData", 1, crate::browser_chardata::append_data),
    ms(&[CharData], "insertData", 2, crate::browser_chardata::insert_data),
    ms(&[CharData], "deleteData", 2, crate::browser_chardata::delete_data),
    ms(&[CharData], "replaceData", 3, crate::browser_chardata::replace_data),
    ms(&[Text], "splitText", 1, crate::browser_chardata::split_text),
    ms(&[Node], "normalize", 0, crate::browser_chardata::normalize),
    ms(&[Document], "createElement", 1, nd::create_element_s),
    ms(&[Document], "createEvent", 1, ev::create_event),
    ms(&[Document], "createElementNS", 2, ns::create_element_ns),
    ms(&[Document], "getElementsByName", 1, crate::browser_nodelist::get_elements_by_name),
    ms(&[Document], "createAttribute", 1, at::create_attribute),
    ms(&[Document], "createAttributeNS", 2, at::create_attribute_ns),
    ms(&[Document], "createTextNode", 1, nd::create_text_node),
    ms(&[Document], "createComment", 1, nd::create_comment),
    ms(&[Document, Fragment], "getElementById", 1, nd::get_element_by_id),
    ms(&[Document, Shadow], "elementFromPoint", 2, crate::browser_point::element_from_point),
    ms(&[Document], "caretRangeFromPoint", 2, crate::browser_point::caret_range_from_point),
    ms(&[Document, Shadow], "elementsFromPoint", 2, crate::browser_point::elements_from_point),
    gs(&[Document], "documentElement", nd::document_element_s),
    gs(&[Document], "head", nd::head),
    gs(&[Document], "body", nd::body_s),
    asr(&[Document], "title", nd::document_title, nd::set_document_title),
    gs(&[Document], "doctype", nd::doctype),
    gs(&[Document], "defaultView", nd::default_view),
    asr(&[Input], "value", nd::input_value, nd::set_input_value),
    gs(&[Input], "files", nd::input_files),
    asr(&[Input], "checked", nd::input_checked, nd::set_input_checked),
    asr(&[Input], "indeterminate", nd::input_indeterminate, nd::set_input_indeterminate),
    asr(&[Input], "selectionStart", sel::selection_start, sel::set_selection_start),
    asr(&[Input], "selectionEnd", sel::selection_end, sel::set_selection_end),
    asr(&[Input], "selectionDirection", sel::selection_direction, sel::set_selection_direction),
    ms(&[Input], "setSelectionRange", 2, sel::set_selection_range_method),
    ms(&[Input], "select", 0, sel::select_method),
    ms(&[Input], "setRangeText", 1, sel::set_range_text_input),
    asr(&[Input], "type", nd::input_type, nd::set_input_type),
    asr(&[Input], "defaultValue", nd::default_value, nd::set_default_value),
    asr(&[Input], "defaultChecked", nd::default_checked, nd::set_default_checked),
    asr(&[Link], "rel", nd::link_rel, nd::set_link_rel),
    asr(&[Link], "href", crate::browser_loader::link_href, crate::browser_loader::set_link_href),
    asr(&[Link], "as", nd::link_as, nd::set_link_as),
    asr(&[Link], "crossOrigin", nd::link_cross_origin, nd::set_link_cross_origin),
    gs(&[Document], "scripts", crate::browser_platform_query::scripts),
    gs(&[Document], "forms", crate::browser_platform_query::forms),
    gs(&[Document], "images", crate::browser_platform_query::images),
    gs(&[Document], "embeds", crate::browser_platform_query::embeds),
    gs(&[Document], "links", crate::browser_platform_query::links),
    gs(&[Document], "anchors", crate::browser_platform_query::anchors),
    asr(&[Input], "name", fm::input_name, fm::set_input_name),
    asr(&[Input], "disabled", fm::input_disabled, fm::set_input_disabled),
    asr(&[Input], "readOnly", fm::input_read_only, fm::set_input_read_only),
    gs(&[Input], "form", fm::input_form),
    asr(&[Input], "formNoValidate", fm::input_form_no_validate, fm::set_input_form_no_validate),
    asr(&[Button], "name", fm::button_name, fm::set_button_name),
    asr(&[Button], "type", fm::button_type, fm::set_button_type),
    asr(&[Button], "disabled", fm::button_disabled, fm::set_button_disabled),
    gs(&[Button], "form", fm::button_form),
    asr(&[Button], "formNoValidate", fm::button_form_no_validate, fm::set_button_form_no_validate),
    asr(&[Form], "noValidate", fm::form_no_validate, fm::set_form_no_validate),
    ms(&[Form], "requestSubmit", 0, crate::browser_submit::request_submit),
    ms(&[Form], "submit", 0, crate::browser_submit::submit_method),
    ms(&[Form], "checkValidity", 0, crate::browser_submit::form_check_validity),
    ms(&[Form], "reportValidity", 0, crate::browser_submit::form_report_validity),
    ms(&[Input, Button], "checkValidity", 0, crate::browser_submit::control_check_validity),
    ms(&[Input, Button], "reportValidity", 0, crate::browser_submit::control_report_validity),
    gs(&[Submit], "submitter", fm::submit_event_submitter),
    gs(&[Toggle], "oldState", crate::browser_details::old_state),
    gs(&[Toggle], "newState", crate::browser_details::new_state),
    gs(&[Event], "type", ev::event_type),
    gs(&[Event], "target", ev::event_target),
    gs(&[Event], "currentTarget", ev::event_current_target),
    gs(&[Event], "eventPhase", ev::event_phase),
    gs(&[Event], "bubbles", ev::event_bubbles),
    gs(&[Event], "cancelable", ev::event_cancelable),
    gs(&[Event], "composed", ev::event_composed),
    gs(&[Event], "defaultPrevented", ev::event_default_prevented),
    gs(&[Event], "isTrusted", ev::event_is_trusted),
    gs(&[Event], "timeStamp", ev::event_time_stamp),
    gs(&[Event], "srcElement", ev::event_src_element),
    asr(&[Event], "returnValue", ev::event_return_value, ev::set_event_return_value),
    asr(&[Event], "cancelBubble", ev::event_cancel_bubble, ev::set_event_cancel_bubble),
    ms(&[Event], "preventDefault", 0, ev::prevent_default),
    ms(&[Event], "stopPropagation", 0, ev::stop_propagation),
    ms(&[Event], "stopImmediatePropagation", 0, ev::stop_immediate_propagation),
    ms(&[Event], "composedPath", 0, ev::composed_path),
    ms(&[Event], "initEvent", 1, ev::init_event),
    ms(&[Custom], "initCustomEvent", 1, ev::init_custom_event),
    gs(&[Custom], "detail", ev::custom_detail),
    ms(&[Html], "focus", 0, crate::browser_focus::focus),
    ms(&[Html], "blur", 0, crate::browser_focus::blur),
    gs(&[Document], "activeElement", crate::browser_focus::active_element),
    gs(&[Focus], "relatedTarget", crate::browser_focus::related_target),
    gs(&[Mouse], "clientX", ui::client_x),
    gs(&[Mouse], "clientY", ui::client_y),
    gs(&[Mouse], "screenX", ui::screen_x),
    gs(&[Mouse], "screenY", ui::screen_y),
    gs(&[Mouse], "pageX", ui::page_x),
    gs(&[Mouse], "pageY", ui::page_y),
    gs(&[Mouse], "x", ui::client_x),
    gs(&[Mouse], "y", ui::client_y),
    gs(&[Mouse], "button", ui::button),
    gs(&[Mouse], "buttons", ui::buttons),
    gs(&[Mouse, Keyboard, InputEv], "detail", ui::detail),
    gs(&[Mouse, Keyboard], "shiftKey", ui::shift),
    gs(&[Mouse, Keyboard], "ctrlKey", ui::ctrl),
    gs(&[Mouse, Keyboard], "altKey", ui::alt),
    gs(&[Mouse, Keyboard], "metaKey", ui::meta),
    gs(&[Mouse], "relatedTarget", ui::related),
    gs(&[Pointer], "pointerType", ui::pointer_type),
    gs(&[Pointer], "isPrimary", ui::is_primary),
    gs(&[Pointer], "pointerId", ui::pointer_id),
    gs(&[Keyboard], "key", ui::key),
    gs(&[Keyboard], "code", ui::code),
    gs(&[Keyboard], "repeat", ui::repeat),
    gs(&[Keyboard], "keyCode", ui::key_code),
    gs(&[Keyboard], "which", ui::key_code),
    gs(&[Keyboard], "location", ui::location),
    gs(&[Keyboard, InputEv], "isComposing", ui::is_composing),
    gs(&[InputEv], "data", ui::data),
    gs(&[InputEv], "inputType", ui::input_type),
]);

// ---------------------------------------------------------------------------------------------- Browser

/// One realm with a DOM. This is the type tests and tools use.
pub struct Browser<E: Eng = New> {
    pub(crate) observe_document: Option<u64>,
    pub it: E::Vm,
    pub(crate) dom: Rc<Dom<E>>,
}

/// What a dispatch from the test driver did.
#[derive(Debug)]
pub struct Dispatched {
    /// `dispatchEvent`'s result: false when a listener canceled a cancelable event.
    pub not_canceled: bool,
    /// Listener exceptions of this dispatch (also kept for `take_reports`).
    pub reports: Vec<Report>,
}

/// Counters for tests and the load tool.
#[derive(Clone, Copy, Debug, Default)]
pub struct BrowserStats {
    pub wrappers: usize,
    pub listeners: usize,
    pub events: usize,
    pub reports: usize,
    pub dropped_reports: u64,
    pub nodes: usize,
    pub text_bytes: usize,
    pub attr_slots: usize,
}

impl Browser<New> {
    /// A realm over `doc` with `window`, `document` and the DOM interfaces installed.
    pub fn new(doc: Document, limits: BrowserLimits, eval: EvalLimits, heap: HeapLimits) -> R<New, Browser<New>> {
        Browser::make(doc, limits, eval, heap)
    }
}

impl<E: Eng> Browser<E> {
    /// `new` on the engine `E`.
    pub fn make(doc: Document, limits: BrowserLimits, eval: EvalLimits, heap: HeapLimits) -> R<E, Browser<E>> {
        Browser::with_vm(E::realm(eval, heap)?, doc, limits)
    }

    /// A realm over `doc` with `window`, `document` and the DOM interfaces installed, on the engine `it`.
    pub fn with_vm(mut it: E::Vm, doc: Document, limits: BrowserLimits) -> R<E, Browser<E>> {
        for (used, cap, name) in [
            (doc.len(), limits.max_nodes, "dom-nodes"),
            (doc.text_bytes(), limits.max_text_bytes, "dom-text"),
            (doc.attr_slots(), limits.max_attr_slots, "dom-attrs"),
            (doc.name_count(), limits.max_names, "dom-names"),
        ] {
            if used > cap { return Err(Fault::Limit(LimitKind::Host(name))); }
        }
        bare_css::rule_cache::new_document();
        let dom = Rc::new(Dom {
            doc: Rc::new(RefCell::new(doc)),
            st: RefCell::default(),
            lim: limits,
            p: OnceCell::new(),
            global: OnceCell::new(),
            selector_scope: Default::default(),
            doc_wrapper: OnceCell::new(),
            wrapper_edges: Cell::new(false),
            scoped: RefCell::default(),
            fixed: RefCell::default(),
            start: Instant::now(),
            vorigin: if bare_net::vclock::on() { bare_net::vclock::now() } else { 0.0 },
            sched: RefCell::default(),
            fetch: Default::default(),
            xhr: Default::default(),
            streams: Default::default(),
            console: Default::default(),
            aborted: Default::default(),
            nav: Default::default(),
            scripts: Default::default(),
            plat: Default::default(),
            style: Default::default(),
            computed: Default::default(),
            cssom: Default::default(),
            geometry: Default::default(),
            implementation: Default::default(),
            parsing_protos: Default::default(),
            perf_observer: Default::default(),
            perf_observer_ctor: Default::default(),
            perf_ifaces: Default::default(),
            screen_iface: Default::default(),
            frames: Default::default(),
            kids: Default::default(),
            perf_entries: Default::default(),
            load_times: Default::default(),
            html_ifaces: Default::default(),
            html_on: Default::default(),
            dialogs: Default::default(),
            link_done: Default::default(),
            scrolling: Default::default(),
            resize: Default::default(),
            motion: Default::default(),
            frame: Default::default(),
            idle: Default::default(),
            basics: Default::default(),
            mo: Default::default(),
            io: Default::default(),
            walk: Default::default(),
            range: Default::default(),
            clicking: Default::default(),
            attr: Default::default(),
            nl: Default::default(),
            sel: Default::default(),
            dataset: Default::default(),
            abort: Default::default(),
            web: Default::default(),
            els: Default::default(),
            media: Default::default(),
            img: Default::default(),
            hh: Default::default(),
            ctl: Default::default(),
            ce: Default::default(),
            geom: Default::default(),
            worker: Default::default(),
            broadcast: Default::default(),
            reclaim: Default::default(),
        });
        E::install_dom(&mut it, &dom)?;
        #[cfg(test)]
        if let Some(mode) = std::env::var_os("BARE_RECLAIM_STRESS") {
            dom.reclaim.set_each(true);
            // `1`: poison, so a stale node panics. `lazy`: no poison, so a slot is reused and the dead trees wait for slots. `collect`: collect only.
            if mode != "collect" {
                dom.doc.borrow_mut().set_poison(mode != "lazy");
                dom.reclaim.set_always(true);
            }
        }
        Ok(Browser { it, dom, observe_document: bare_observe::process::next_id() })
    }

    /// The native document. Do not hold a mutable borrow across a call into the browser.
    pub fn document(&self) -> Rc<RefCell<Document>> {
        self.dom.doc.clone()
    }

    /// A task ended with `e`. A script budget (steps, call depth, nesting depth, statement frames, jobs of one checkpoint) aborts
    /// only that task, as Chrome does for a runaway script: the console gets one error, the sticky stop states of the queues are
    /// cleared, and the page goes on with its DOM as it is. Every other error, and the abort after `max_task_aborts`, is fatal.
    /// The DOM stays consistent because each native DOM operation completes before a budget is checked, and a budget is only
    /// charged before the work that it bounds. Host caps (`Limit(Host(..))`), heap and memory caps stay fatal: they can come from
    /// garbage that stays, or from a refusal that a retry would repeat.
    pub fn survive(&mut self, e: Fault<E>, what: &str) -> Survive {
        let Fault::Limit(k) = e else { return Survive::Fatal };
        if !is_budget(&e) {
            return Survive::Fatal;
        }
        let n = self.dom.aborted.get() + 1;
        if n > self.dom.lim.max_task_aborts {
            return Survive::Exhausted(format!("{} tasks were aborted by script budgets, the next one ({k:?} in {what}) is too many", n - 1));
        }
        self.dom.aborted.set(n);
        self.it.clear_failed_job();
        self.dom.clear_budget_halt_sched();
        self.dom.clear_budget_halt_nav();
        self.dom.clear_budget_halt_plat();
        self.dom.clear_budget_halt_fetch();
        // A new budget window: the steps of the aborted task must not make the next host call fail.
        let _ = self.it.host_scope_o(|_| Ok(()));
        crate::browser_console::push_text(&self.dom, &mut self.it, crate::browser_console::ConsoleLevel::Error, &format!("Script aborted: the {k:?} budget ended the {what}. The page goes on ({n} of {} aborts used)", self.dom.lim.max_task_aborts));
        Survive::Aborted
    }

    pub fn run_script(&mut self, source: &str) -> R<E, E::Raw> {
        let mut ids = bare_observe::process::context();
        ids.document = self.observe_document;
        let _context = bare_observe::process::enter(ids);
        let r = self.it.run_script_o(source);
        self.observe_stats();
        // `BARE_RECLAIM_STRESS=1 cargo test`: collect and reclaim after each script, freed nodes poisoned (`browser_reclaim.rs`).
        #[cfg(test)]
        if self.dom.reclaim.each() {
            let keep = match &r {
                Ok(v) | Err(Fault::Throw(v)) => Some(*v),
                _ => None,
            };
            self.collect(keep.as_slice())?;
        }
        r
    }

    /// The microtask checkpoint (Promise jobs).
    pub fn drain(&mut self) -> R<E, usize> {
        let mut ids = bare_observe::process::context();
        ids.document = self.observe_document;
        let _context = bare_observe::process::enter(ids);
        let result = self.it.drain_microtasks_o();
        self.observe_stats();
        result
    }

    fn observe_stats(&self) {
        bare_observe::process::emit("engine", "browser_stats", || {
            use bare_observe::{Field, Scalar::U64};
            let s = self.stats();
            let h = self.it.h_stats();
            [Field("heap_accounted_bytes", U64(h.bytes as u64)), Field("live_objects", U64(h.live_objects as u64)), Field("collections", U64(h.collections)), Field("heap_slab_capacity_bytes", U64(h.slab_capacity_bytes as u64)), Field("text_bytes", U64(s.text_bytes as u64)), Field("attr_slots", U64(s.attr_slots as u64)), Field("nodes", U64(s.nodes as u64)), Field("wrappers", U64(s.wrappers as u64)), Field("listeners", U64(s.listeners as u64)), Field("events", U64(s.events as u64))]
        });
    }

    /// Is a collect due at a safe point? The heap asks (`Interp::gc_due`), or the node arena has grown by the same rule since the last collect:
    /// a page that replaces its lists makes dead nodes and little script heap, and only a collect and reclaim free them.
    pub fn collect_due(&self) -> bool {
        let live = self.it.external_live();
        // Events are small in the heap, so their cap can fill before the heap asks: half the cap asks too.
        self.it.gc_due()
            || self.dom.st.borrow().events.len() > self.dom.lim.max_events / 2
            || self.dom.doc.try_borrow().is_ok_and(|d| d.arena_bytes() > live + (2usize << 20).max(live / 4))
    }

    /// Collect the heap. Attached wrappers and their listeners live. `extra` are values the caller still holds.
    pub fn collect(&mut self, extra: &[E::Raw]) -> R<E, crate::js_limits::Collected> {
        let mut ids = bare_observe::process::context();
        ids.document = self.observe_document;
        let _context = bare_observe::process::enter(ids);
        let c = self.it.collect(extra)?;
        { static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0); let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed); if n % 20 == 0 { eprintln!("TMP-COLLECT #{n} arena={}", self.dom.doc.try_borrow().map_or(0, |d| d.arena_bytes())); } }
        // Every collection may leave trees that no wrapper names.
        self.dom.reclaim_wanted();
        // The caller is at a safe point (no script runs, no host call is on the stack): the trees that lost their wrappers can go.
        self.dom.reclaim(true);
        let native = self.dom.doc.try_borrow().map_or(0, |d| d.arena_bytes());
        self.it.set_external_live(native);
        self.observe_stats();
        Ok(c)
    }

    /// Listener exceptions so far, oldest first. Clears the list.
    pub fn take_reports(&mut self) -> Vec<Report> {
        self.dom.st.borrow_mut().reports.drain(..).collect()
    }

    pub fn stats(&self) -> BrowserStats {
        let (st, doc) = (self.dom.st.borrow(), self.dom.doc.borrow());
        BrowserStats {
            wrappers: st.by_obj.len(),
            listeners: st.n_listeners,
            events: st.events.len(),
            reports: st.reports.len(),
            dropped_reports: st.dropped_reports,
            nodes: doc.len(),
            text_bytes: doc.text_bytes(),
            attr_slots: doc.attr_slots(),
        }
    }

    /// The JS wrapper of a native node (made if needed). Keep it in `extra` of `collect` to keep a detached node.
    pub fn wrapper(&mut self, node: NodeId) -> R<E, E::Raw> {
        let dom = self.dom.clone();
        self.it.host_scope_o(|it| dom.wrap(it, node))
    }

    /// The native node of a wrapper value.
    pub fn node(&self, v: E::Raw) -> Option<NodeId> {
        self.dom.node_of(v)
    }

    /// First element in tree order that matches `selector` (native lookup; JS does not run).
    pub fn query(&mut self, selector: &str) -> R<E, Option<NodeId>> {
        let dom = self.dom.clone();
        self.it.host_scope_o(|it| {
            let root = dom.doc.borrow().root();
            Ok(dom.query(it, root, selector, false)?.into_iter().next())
        })
    }

    // ----- the test driver: set native state, then dispatch a normal event -----

    /// Dispatch a new event of type `ty` at `node`. `isTrusted` is false. After each listener that the driver
    /// started, the microtask queue is drained (a checkpoint with an empty JS stack).
    pub fn dispatch(&mut self, node: NodeId, ty: &str, bubbles: bool, cancelable: bool) -> R<E, Dispatched> {
        let dom = self.dom.clone();
        let mark = {
            let st = dom.st.borrow();
            st.dropped_reports + st.reports.len() as u64
        };
        dom.st.borrow_mut().driver = true;
        let r = self.it.host_scope_o(|it| {
            let e = dom.new_event(it, ty, bubbles, cancelable, true, None)?;
            dom.dispatch(it, Target::Node(node), e)
        });
        dom.st.borrow_mut().driver = false;
        let not_canceled = r?;
        let st = dom.st.borrow();
        let total = st.dropped_reports + st.reports.len() as u64;
        let new = ((total - mark) as usize).min(st.reports.len());
        let reports = st.reports.iter().skip(st.reports.len() - new).cloned().collect();
        Ok(Dispatched { not_canceled, reports })
    }

    /// The tab closes: the document is hidden for good (`document.visibilityState` "hidden"), then `visibilitychange` fires at the
    /// document and `pagehide` (`persisted` false: bare keeps no back/forward cache) at the window, as in Chrome. Pages send their
    /// pending changes from these handlers. A second call does nothing.
    pub fn hide(&mut self) -> R<E, ()> {
        let dom = self.dom.clone();
        if dom.basics.hidden.replace(true) {
            return Ok(());
        }
        let root = dom.doc.borrow().root();
        self.it.host_scope_o(|it| {
            let e = dom.new_event(it, "visibilitychange", true, false, false, None)?;
            dom.dispatch(it, Target::Node(root), e)?;
            let e = dom.new_event(it, "pagehide", false, false, false, None)?;
            let k = dom.key(it, "persisted")?;
            it.define_data(e, k, E::bool_raw(false), false, false, true)?;
            dom.dispatch(it, Target::Window, e)?;
            Ok(())
        })
    }

    /// `submit` at a form (bubbles, cancelable). No validation, no navigation.
    pub fn submit(&mut self, form: NodeId) -> R<E, Dispatched> {
        self.dispatch(form, "submit", true, true)
    }

    /// Like a user typing: set the native value of a text-like `<input>`, then dispatch `input` (bubbles).
    pub fn type_text(&mut self, input: NodeId, text: &str) -> R<E, Dispatched> {
        self.dom.driver_input(input, Some(text), None)?;
        self.dispatch(input, "input", true, false)
    }

    /// Like leaving a field: dispatch `change` (bubbles). The value was set by `type_text`.
    pub fn commit_change(&mut self, input: NodeId) -> R<E, Dispatched> {
        self.dom.driver_input(input, None, None)?;
        self.dispatch(input, "change", true, false)
    }

    /// Set the native checkedness of a checkbox or radio, then dispatch `input` and `change`.
    /// Radio groups are not modeled: other radios stay as they are.
    pub fn set_checked(&mut self, input: NodeId, checked: bool) -> R<E, Dispatched> {
        self.dom.driver_input(input, None, Some(checked))?;
        let mut a = self.dispatch(input, "input", true, false)?;
        let b = self.dispatch(input, "change", true, false)?;
        a.not_canceled &= b.not_canceled;
        a.reports.extend(b.reports);
        Ok(a)
    }

    /// The native value of an `<input>` (what JS `input.value` reads).
    pub fn input_value(&self, input: NodeId) -> Option<String> {
        self.dom.input_value_of(input)
    }

    /// The checkedness of a checkbox or a radio (what JS `input.checked` reads).
    pub fn input_checked(&self, input: NodeId) -> bool {
        self.dom.input_checked(input)
    }
}
