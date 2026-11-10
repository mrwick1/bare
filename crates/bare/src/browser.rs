//! The first real DOM/JS bridge: `window`, `document`, nodes and events for the bounded JS evaluator.
//!
//! How it fits together
//! - The DOM is the native `bare_html::dom::Document` behind `Rc<RefCell<Document>>`. JS never sees a copy.
//! - `Dom` implements `bare_js::eval::Host`. JS calls reach it as numbered methods (`handlers`). Getters and setters
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
//! `Error::Limit(LimitKind::Host(name))` (never a catchable JS error): nodes, text bytes, attribute slots, names,
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
//!   properties through the host named-property hook of `bare_js::heap` (`heap_named.rs`). MathML elements have no `dataset` (no `MathMLElement` brand).
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
//! - `<input>` has no value sanitization (newline stripping, number/date parsing). `textarea` has a `value` set by script (no
//!   events, no selection API, not painted or submitted).
//! - Microtasks: the test driver runs a checkpoint after each listener it started from the host. Dispatch from JS does not.
//! - `matchMedia`, `classList` and `getElementsBy*` are in `browser_platform.rs` (read its header).
//! - `getComputedStyle` and the read-only computed declaration are in `browser_computed.rs` (read its header: external sheets are supplied by the embedder, used values are gated).
//! - Subclassing host constructors with `class X extends Event` needs `new.target` plumbing in the evaluator (not done).
//!
//! Not browser-verified: nothing here was compared with Chrome. DOM behavior follows the DOM Standard by reading, and
//! is tested against structural expectations written by hand. Only plain `EventTarget`/`Event` semantics have a Node
//! oracle (`tools/js/browser_oracle.mjs`).

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
use bare_js::eval::{Error, EvalLimits, Host, Interp, LimitKind};
use bare_js::heap::{Heap, HeapLimits, PropertyDescriptor};
use bare_js::value::{ObjId, PropertyKey, Value};
use std::cell::{OnceCell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::hash::{BuildHasherDefault, Hasher};
use std::rc::Rc;
use std::time::Instant;

pub(crate) type R<T> = Result<T, Error>;

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
/// or `NodeId` (the document never reuses one).
pub(crate) trait SlotKey: Copy + Eq {
    fn slot(self) -> usize;
}
impl SlotKey for ObjId {
    fn slot(self) -> usize {
        ObjId::slot(self)
    }
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
}
impl<K, V> Default for IdTab<K, V> {
    fn default() -> Self {
        IdTab { pages: Vec::new(), len: 0 }
    }
}
impl<K: SlotKey, V> IdTab<K, V> {
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    /// The length of the directory and the number of pages that exist.
    #[cfg(test)]
    pub fn size(&self) -> (usize, usize) {
        (self.pages.len(), self.pages.iter().flatten().count())
    }
    pub fn get(&self, k: &K) -> Option<&V> {
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
        self.pages.iter().flatten().flat_map(|p| p.iter().flatten().map(|(k, v)| (k, v)))
    }
    /// The entry of `k` is `v` from now on. The old value comes back if `k` had one.
    pub fn insert(&mut self, k: K, v: V) -> Option<V> {
        let s = k.slot();
        if self.pages.len() <= s >> PAGE_BITS {
            self.pages.resize_with((s >> PAGE_BITS) + 1, || None);
        }
        let page = self.pages[s >> PAGE_BITS].get_or_insert_with(|| Box::new([const { None }; 1 << PAGE_BITS]));
        let old = page[s & ((1 << PAGE_BITS) - 1)].replace((k, v));
        self.len += old.is_none() as usize;
        old.filter(|(kk, _)| *kk == k).map(|(_, v)| v)
    }
    /// The value of `k`; `v` is stored first if there is none.
    pub fn get_or_insert(&mut self, k: K, v: V) -> &mut V {
        if !self.contains_key(&k) {
            self.insert(k, v);
        }
        self.get_mut(&k).expect("just stored")
    }
    pub fn retain(&mut self, mut keep: impl FnMut(&K, &mut V) -> bool) {
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
pub(crate) type H = fn(&Dom, &mut Interp<'_>, Value, &[Value]) -> R<Value>;

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
    pub max_selector_len: usize,
    /// Bytes of one string that crosses between JS and the document.
    pub max_string_bytes: usize,
    /// Kept listener exception reports. The oldest is dropped (and counted) after this.
    pub max_reports: usize,
    /// Tasks that a script budget may abort (`Browser::survive`). The next one is fatal.
    pub max_task_aborts: usize,
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
            max_selector_len: 1024,
            max_string_bytes: 16 << 20,
            max_reports: 256,
            max_task_aborts: 5,
        }
    }
}

/// What a JS event target is on the Rust side.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Target {
    Window,
    Node(NodeId),
    /// A plain `new EventTarget()`.
    Obj(ObjId),
}

#[derive(Clone)]
pub(crate) struct Listener {
    pub id: u64,
    pub ty: Rc<str>,
    pub cb: ObjId,
    pub capture: bool,
    pub once: bool,
    pub passive: bool,
    /// The listener of an event handler property (`oninput`): `cb` is the handler's current value (`browser_handlers.rs`).
    pub handler: bool,
}

pub(crate) struct EventState {
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
    pub target: Value,
    pub current: Value,
    pub detail: Value,
    pub path: Vec<Target>,
    pub time: f64,
    /// `FocusEvent.relatedTarget` (null for other events). Traced, so a saved event keeps the node.
    pub related: Value,
    pub is_focus: bool,
    /// `SubmitEvent.submitter` is in `related` (traced). Only a `SubmitEvent` has this set.
    pub is_submit: bool,
    /// The fields of a mouse, pointer, keyboard or input event that the window made (`browser_ui.rs`). `related` holds `relatedTarget`.
    pub ui: Option<Box<crate::browser_ui::Ui>>,
    /// `isTrusted`: set for the events of `browser_ui.rs`.
    pub trusted: bool,
}

/// Is `e` a task budget that aborts a task and not the page (see [`Browser::survive`])?
pub(crate) fn is_budget(e: &Error) -> bool {
    matches!(e, Error::Limit(LimitKind::Steps | LimitKind::CallDepth | LimitKind::TraversalDepth | LimitKind::StatementFrames | LimitKind::DrainJobs | LimitKind::Host("dom-selector")))
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
}

#[derive(Default)]
pub(crate) struct State {
    pub by_node: IdTab<NodeId, ObjId>,
    pub by_obj: IdTab<ObjId, Target>,
    pub listeners: IdMap<Target, Vec<Listener>>,
    pub n_listeners: usize,
    pub next_id: u64,
    pub events: IdMap<ObjId, EventState>,
    pub inputs: IdMap<NodeId, Input>,
    /// Bytes of the dirty values in `inputs`.
    pub input_bytes: usize,
    pub reports: VecDeque<Report>,
    pub dropped_reports: u64,
    pub sel_cache: HashMap<String, Rc<Vec<Selector>>>,
    /// True while the test driver runs a dispatch from the host (a microtask checkpoint follows each listener).
    pub driver: bool,
    /// The focused element and the transition guards (`browser_focus.rs`).
    pub focus: crate::browser_focus::FocusState,
    /// Pointer state and counters of the window input (`browser_ui.rs`).
    pub ui: crate::browser_ui::UiState,
    /// Event handler properties and attributes (`browser_handlers.rs`).
    pub hd: crate::browser_handlers::Hd,
}

#[derive(Clone, Copy)]
pub(crate) struct Protos {
    pub et: ObjId,
    pub node: ObjId,
    pub element: ObjId,
    pub html: ObjId,
    pub svg: ObjId,
    pub input: ObjId,
    pub iframe: ObjId,
    pub link: ObjId,
    pub form: ObjId,
    pub button: ObjId,
    pub submit: ObjId,
    pub chardata: ObjId,
    pub text: ObjId,
    pub comment: ObjId,
    pub document: ObjId,
    pub fragment: ObjId,
    pub shadow: ObjId,
    pub event: ObjId,
    pub custom: ObjId,
    pub focus: ObjId,
    pub mouse: ObjId,
    pub pointer: ObjId,
    pub keyboard: ObjId,
    pub inputev: ObjId,
    pub domexc: ObjId,
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

pub(crate) enum Kind {
    Method(H),
    Get(H),
    Accessor(H, H),
}

pub(crate) struct Def {
    pub on: &'static [P],
    pub name: &'static str,
    pub len: u32,
    pub kind: Kind,
}

/// The DOM host of one realm.
pub struct Dom {
    pub(crate) doc: Rc<RefCell<Document>>,
    pub(crate) st: RefCell<State>,
    pub(crate) lim: BrowserLimits,
    pub(crate) p: OnceCell<Protos>,
    pub(crate) global: OnceCell<ObjId>,
    selector_scope: Rc<std::cell::Cell<Option<NodeId>>>,
    pub(crate) doc_wrapper: OnceCell<ObjId>,
    /// `(handler, needs new)` for each host function id.
    pub(crate) handlers: RefCell<Vec<(H, bool)>>,
    /// Prototypes and interface objects: permanent roots.
    pub(crate) fixed: RefCell<Vec<ObjId>>,
    pub(crate) start: Instant,
    pub(crate) sched: RefCell<crate::browser_scheduler::Sched>,
    pub(crate) fetch: crate::browser_fetch::Fetch,
    /// Script-made `ReadableStream`s (`browser_streams.rs`).
    pub(crate) streams: crate::browser_streams::Streams,
    /// `console` and its bounded record sink (`browser_console.rs`).
    pub(crate) console: crate::browser_console::Console,
    /// Tasks that a script budget aborted so far (`Browser::survive`).
    pub(crate) aborted: std::cell::Cell<usize>,
    /// Navigation, history and storage state (`browser_navigation.rs`).
    pub(crate) nav: crate::browser_navigation::Nav,
    pub(crate) scripts: crate::browser_loader::Scripts,
    /// `matchMedia`, `classList`, `getElementsBy*` (`browser_platform.rs`).
    pub(crate) plat: crate::browser_platform::Platform,
    /// `HTMLElement.style` / `CSSStyleDeclaration` (`browser_style.rs`).
    pub(crate) style: crate::browser_style::Style,
    /// `getComputedStyle` and its sheet state (`browser_computed.rs`).
    pub(crate) computed: crate::browser_computed::Computed,
    pub(crate) cssom: crate::browser_cssom::Cssom,
    /// `offsetWidth` and `offsetHeight` (`browser_geometry.rs`).
    pub(crate) geometry: crate::browser_geometry::Geometry,
    /// `document.implementation`, once read (`browser_implementation.rs`). It is also in `fixed`.
    pub(crate) parsing_protos: std::cell::Cell<[Option<ObjId>; 3]>,
    pub(crate) implementation: std::cell::Cell<Option<ObjId>>,
    /// The prototype and the constructor of `PerformanceObserver`, once read (`browser_perf_observer.rs`). Both are also in `fixed`.
    pub(crate) perf_observer: std::cell::Cell<Option<ObjId>>,
    pub(crate) perf_observer_ctor: std::cell::Cell<Option<ObjId>>,
    /// The `mark` and `measure` entries of `performance` (`browser_perf.rs`).
    pub(crate) frames: crate::browser_frames::Frames,
    pub(crate) perf_entries: RefCell<Vec<crate::browser_perf::Entry>>,
    /// The prototype and constructor of each interface of `browser_html.rs`, once made.
    pub(crate) html_ifaces: RefCell<Vec<Option<(ObjId, ObjId)>>>,
    /// A global of `browser_html.rs` was read: new wrappers get their interface prototype.
    pub(crate) html_on: std::cell::Cell<bool>,
    /// Scroll offsets, page bars and `scroll` events (`browser_scroll.rs`).
    pub(crate) scrolling: crate::browser_scroll::Scrolling,
    /// `ResizeObserver` and its rendering step (`browser_resize.rs`).
    pub(crate) resize: crate::browser_resize::Resize,
    /// `requestAnimationFrame` (`browser_frame.rs`).
    pub(crate) frame: crate::browser_frame::Frame,
    /// `requestIdleCallback` (`browser_idle.rs`).
    pub(crate) idle: crate::browser_idle::Idle,
    /// `MutationObserver` (`browser_mutation.rs`).
    pub(crate) mo: crate::browser_mutation::Mo,
    pub(crate) io: crate::browser_intersection::Io,
    pub(crate) walk: crate::browser_walker::Walk,
    pub(crate) range: crate::browser_range::Ranges,
    /// The elements whose `click()` is running (`browser_element.rs`).
    pub(crate) clicking: RefCell<Vec<NodeId>>,
    /// `Element.attributes`, `NamedNodeMap` and `Attr` (`browser_attr.rs`).
    pub(crate) attr: crate::browser_attr::AttrHost,
    /// `Document.getElementsByName` and the live `NodeList` (`browser_nodelist.rs`).
    pub(crate) nl: crate::browser_nodelist::NodeListHost,
    /// `HTMLSelectElement`, `HTMLOptionElement` and the option lists (`browser_select.rs`).
    pub(crate) sel: crate::browser_select::SelHost,
    /// `HTMLElement.dataset` and `DOMStringMap` (`browser_dataset.rs`).
    pub(crate) dataset: crate::browser_dataset::DatasetHost,
    /// `AbortController` and `AbortSignal` (`browser_abort.rs`).
    pub(crate) abort: crate::browser_abort::AbortHost,
    /// `XMLHttpRequest` and `ProgressEvent` (`browser_xhr.rs`).
    pub(crate) xhr: crate::browser_xhr::XhrHost,
    /// `crypto`, `navigator`, `Blob`, `File` and `FormData` (`browser_web.rs`).
    pub(crate) web: crate::browser_web::WebHost,
    /// `HTMLAnchorElement`, `HTMLAreaElement` and `HTMLTemplateElement` (`browser_elements.rs`).
    pub(crate) els: crate::browser_elements::Els,
    /// `HTMLMediaElement` and its kin (`browser_media.rs`).
    pub(crate) media: crate::browser_media::Media,
    /// `Image`, `decode()` and the picture requests of scripts (`browser_img.rs`).
    pub(crate) img: crate::browser_img::Img,
    /// `HTMLBodyElement`, `HTMLFrameSetElement` and `ErrorEvent` (`browser_handlers.rs`).
    pub(crate) hh: crate::browser_handlers::Hh,
    /// Constraint validation, `labels` and `form.reset()` (`browser_controls.rs`).
    pub(crate) ctl: crate::browser_controls::CtlHost,
    /// Custom elements (`browser_custom.rs`).
    pub(crate) ce: crate::browser_custom::Ce,
    /// Rectangles, points, quads and matrices (`browser_geom.rs`, `browser_matrix.rs`).
    pub(crate) geom: crate::browser_geom::Geom,
    /// `Worker` (`browser_worker.rs`).
    pub(crate) worker: crate::browser_worker::WorkerHost,
}

impl Host for Dom {
    fn call(&self, it: &mut Interp<'_>, id: u32, this: Value, args: &[Value], is_new: bool) -> R<Value> {
        let (h, needs_new) = self.handlers.borrow()[id as usize];
        self.style.cur.set(id);
        if needs_new && !is_new {
            return Err(crate::browser_custom::needs_new(it, h));
        }
        let r = if self.ce.on.get() { self.call_ce(it, h, this, args) } else { h(self, it, this, args) };
        if self.mo.armed() {
            self.mo_after(it)?;
        }
        r
    }

    fn roots(&self, out: &mut Vec<Value>) {
        out.extend(self.fixed.borrow().iter().map(|&o| Value::Object(o)));
        out.extend(self.doc_wrapper.get().map(|&o| Value::Object(o)));
        self.sched_roots(out);
        self.fetch.roots(out);
        self.streams.roots(out);
        self.console.roots(out);
        crate::browser_navigation::roots(self, out);
        self.scripts_roots(out);
        crate::browser_platform::roots(self, out);
        crate::browser_style::roots(self, out);
        crate::browser_cssom::roots(self, out);
        crate::browser_computed::roots(self, out);
        at::roots(self, out);
        crate::browser_nodelist::roots(self, out);
        crate::browser_select::roots(self, out);
        crate::browser_dataset::roots(self, out);
        crate::browser_abort::roots(self, out);
        crate::browser_xhr::roots(self, out);
        crate::browser_web::roots(self, out);
        crate::browser_elements::roots(self, out);
        crate::browser_media::roots(self, out);
        crate::browser_img::roots(self, out);
        crate::browser_handlers::roots(self, out);
        crate::browser_controls::roots(self, out);
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
    }

    fn trace(&self, owner: ObjId, out: &mut Vec<Value>) {
        if self.frames.active() { crate::browser_frames::trace(self, owner, out); }
        self.sched_trace(owner, out);
        crate::browser_navigation::trace(self, owner, out);
        crate::browser_platform::trace(self, owner, out);
        crate::browser_style::trace(self, owner, out);
        crate::browser_computed::trace(self, owner, out);
        self.resize_trace(owner, out);
        self.mo_trace(owner, out);
        self.io_trace(owner, out);
        self.walk_trace(owner, out);
        self.range_trace(owner, out);
        at::trace(self, owner, out);
        crate::browser_nodelist::trace(self, owner, out);
        crate::browser_dataset::trace(self, owner, out);
        crate::browser_abort::trace(self, owner, out);
        if self.xhr.active.get() {
            crate::browser_xhr::trace(self, owner, out);
        }
        if self.web.active.get() {
            crate::browser_web::trace(self, owner, out);
        }
        let st = self.st.borrow();
        if let Some(&t) = st.by_obj.get(&owner) {
            for l in st.listeners.get(&t).into_iter().flatten() {
                out.push(Value::Object(l.cb));
            }
            if let Target::Node(n) = t {
                let doc = self.doc.borrow();
                out.extend(doc.parent(n).and_then(|p| st.by_node.get(&p)).map(|&o| Value::Object(o)));
                out.extend(doc.children(n).filter_map(|c| st.by_node.get(&c)).map(|&o| Value::Object(o)));
                // A host and its root (either mode) keep each other's wrapper alive, so a detached pair is one cycle and an expando is not lost.
                out.extend(doc.shadow_host(n).into_iter().chain(doc.shadow_root(n)).chain(doc.template_content(n)).filter_map(|x| st.by_node.get(&x)).map(|&o| Value::Object(o)));
            }
        } else if let Some(e) = st.events.get(&owner) {
            out.extend([e.target, e.current, e.detail, e.related]);
            for &t in &e.path {
                match t {
                    Target::Node(n) => out.extend(st.by_node.get(&n).map(|&o| Value::Object(o))),
                    Target::Obj(o) => out.push(Value::Object(o)),
                    Target::Window => {}
                }
            }
        }
        self.fetch.trace(owner, out);
        self.streams.trace(owner, out);
    }

    fn prune(&self, live: &dyn Fn(ObjId) -> bool) {
        crate::browser_navigation::prune(self, live);
        crate::browser_frames::prune(self, live);
        crate::browser_platform::prune(self, live);
        crate::browser_geom::prune(self, live);
        crate::browser_style::prune(self, live);
        crate::browser_computed::prune(self, live);
        self.resize_prune(live);
        self.mo_prune(live);
        self.io_prune(live);
        self.walk_prune(live);
        self.range_prune(live);
        at::prune(self, live);
        crate::browser_nodelist::prune(self, live);
        crate::browser_select::prune(self, live);
        crate::browser_dataset::prune(self, live);
        crate::browser_web::prune(self, live);
        crate::browser_controls::prune(self, live);
        let mut guard = self.st.borrow_mut();
        let st = &mut *guard;
        st.by_obj.retain(|&o, t| matches!(t, Target::Window) || live(o));
        st.by_node.retain(|_, &mut o| live(o));
        let (by_node, by_obj) = (&st.by_node, &st.by_obj);
        st.listeners.retain(|t, _| match t {
            Target::Window => true,
            Target::Node(n) => by_node.contains_key(n),
            Target::Obj(o) => by_obj.contains_key(o),
        });
        st.n_listeners = st.listeners.values().map(Vec::len).sum();
        st.events.retain(|&o, _| live(o));
        drop(guard);
        self.sched_prune(live);
        self.idle_prune(live);
        self.fetch.prune(live);
        self.streams.prune(live);
        crate::browser_abort::prune(self, live);
        crate::browser_xhr::prune(self, live);
        crate::browser_worker::prune(self, live);
    }

    fn report_exception(&self, it: &mut Interp<'_>, thrown: Value) {
        self.report(it, thrown, "microtask", Target::Window);
    }

    fn promise_rejections(&self, it: &mut Interp<'_>, unhandled: &[Value], handled: &[Value], dropped: u32) -> R<()> {
        crate::browser_rejection::notify(self, it, unhandled, handled, dropped)
    }

    fn fill_indexed(&self, it: &mut Interp<'_>, owner: ObjId, index: u32) -> R<()> {
        if index == bare_js::heap::LAZY_TAG { return crate::browser_handlers::tags(self,it); }
        if index == crate::browser_frames::FILL { crate::browser_frames::fill(self, it, owner, index)?; return Ok(()); }
        if (bare_js::heap::LAZY_ALL..u32::MAX).contains(&index) {
            return crate::browser_handlers::lazy(self, it, owner, index == bare_js::heap::LAZY_ALL);
        }
        if crate::browser_nodelist::fill(self, it, owner, index)? || crate::browser_dataset::fill(self, it, owner)? || crate::browser_cssom::fill(self, it, owner)? {
            return Ok(());
        }
        at::fill(self, it, owner, index)
    }

    fn named_write(&self, it: &mut Interp<'_>, req: bare_js::heap::NamedRequest) -> R<bool> {
        if self.storage_area(req.owner).is_some() {
            return crate::browser_navigation_storage::write(self, it, req);
        }
        crate::browser_dataset::write(self, it, req)
    }
}

fn domexc_codes(name: &str) -> u32 {
    match name {
        "IndexSizeError" => 1,
        "HierarchyRequestError" => 3,
        "InvalidCharacterError" => 5,
        "NoModificationAllowedError" => 7,
        "NotFoundError" => 8,
        "NotSupportedError" => 9,
        "InUseAttributeError" => 10,
        "InvalidStateError" => 11,
        "TypeMismatchError" => 17,
        "SyntaxError" => 12,
        "SecurityError" => 18,
        "QuotaExceededError" => 22,
        "DataCloneError" => 25,
        "AbortError" => 20,
        "TimeoutError" => 23,
        _ => 0,
    }
}

#[cold]
pub(crate) fn define_window(d: &Dom, it: &mut Interp<'_>, name: &str) -> R<bool> {
    if name == "eval" {
        d.def_method(it,it.global_object(),"eval",1,|_,it,_,a|it.eval_global(a.first().copied().unwrap_or(Value::Undefined)))?;
        return Ok(true);
    }
    if name == "DocumentType" { crate::browser_implementation::doctype_global(d, it, Value::Undefined, &[])?; return Ok(true); }
    if !matches!(name,"frames"|"parent"|"top"|"length"|"name"|"screen"){return Ok(false);}
    let g = it.global_object();
    let key=d.key(it,name)?;
    if it.heap.peek_own_property(g,key).map_err(|e|it.heap_error(e))?.is_some(){return Ok(true);}
    if name == "length" {
        let f = d.add_fn(it, crate::browser_frames::length, "get length", 0, false)?;
        d.define_accessor(it, g, key, f, None)?;
        return Ok(true);
    }
    let value = match name {
        "frames" | "parent" | "top" => Value::Object(g),
        "length" => Value::Number(0.0),
        "name" => d.js_str(it, "")?,
        "screen" => { window_screen(d,it,Value::Undefined,&[])?; return Ok(true); },
        _ => unreachable!(),
    };
    it.define_data(g,key,value,!matches!(name,"top"|"length"),true,name!="top")?;
    Ok(true)
}

// The virtual display has the default headless dimensions. Viewport resizing does not resize it.
#[cold]
fn window_screen(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> R<Value> {
    let value = if let Some(&v) = a.first() {
        v
    } else {
        let o = d.obj(it, it.object_prototype())?;
        crate::browser_lazy::tag(d, it, o, "Screen")?;
        for (name, value) in [("width", 1280.0), ("height", 800.0), ("availWidth", 1280.0), ("availHeight", 800.0), ("availLeft", 0.0), ("availTop", 0.0), ("colorDepth", 24.0), ("pixelDepth", 24.0)] {
            let k = d.key(it, name)?;
            it.define_data(o, k, Value::Number(value), false, true, true)?;
        }
        Value::Object(o)
    };
    let k = d.key(it, "screen")?;
    it.define_data(it.global_object(), k, value, true, false, true)?;
    Ok(value)
}

impl Dom {
    pub(crate) fn p(&self) -> Protos {
        *self.p.get().expect("installed")
    }

    pub(crate) fn lim(&self, what: &'static str) -> Error {
        Error::Limit(LimitKind::Host(what))
    }

    pub(crate) fn obj(&self, it: &mut Interp<'_>, proto: ObjId) -> R<ObjId> {
        match it.heap.new_object(Some(proto)) {
            Ok(o) => Ok(o),
            Err(e) => Err(it.heap_error(e)),
        }
    }

    pub(crate) fn key(&self, it: &mut Interp<'_>, name: &str) -> R<PropertyKey> {
        match it.heap.key_from_str(name) {
            Ok(k) => Ok(k),
            Err(e) => Err(it.heap_error(e)),
        }
    }

    /// A string value (the JS string has the same UTF-16 as `s`).
    pub(crate) fn js_str(&self, it: &mut Interp<'_>, s: &str) -> R<Value> {
        if s.len() > self.lim.max_string_bytes { return Err(self.lim("dom-string")); }
        it.charge((s.len() / 32) as u64)?;
        match it.heap.intern_str(s) {
            Ok(s) => Ok(Value::String(s)),
            Err(e) => Err(it.heap_error(e)),
        }
    }

    /// `ToString(v)` as UTF-8. A lone surrogate cannot be stored: `Unsupported`, never a lossy copy.
    pub(crate) fn string_arg(&self, it: &mut Interp<'_>, v: Value) -> R<String> {
        let s = it.to_js_string(v)?;
        let len = it.heap.str_len(s).map_err(Error::Heap)?;
        if len > self.lim.max_string_bytes { return Err(self.lim("dom-string")); }
        it.charge((len / 32) as u64)?;
        let units = match it.heap.str_units(s) {
            Ok(u) => u,
            Err(e) => return Err(it.heap_error(e)),
        };
        if units.len() > self.lim.max_string_bytes {
            return Err(self.lim("dom-string"));
        }
        let bytes = char::decode_utf16(units.iter().copied()).try_fold(0usize, |n, c| {
            c.map(|c| n.saturating_add(c.len_utf8()))
        }).map_err(|_| Error::Unsupported(unsupported("lone surrogate in a DOM string (the native document is UTF-8)")))?;
        if bytes > self.lim.max_string_bytes {
            return Err(self.lim("dom-string"));
        }
        String::from_utf16(units).map_err(|_| Error::Unsupported(unsupported("lone surrogate in a DOM string (the native document is UTF-8)")))
    }

    /// A DOMException-like error to throw: an object that inherits from `Error.prototype`, with `name`, `message`, `code`.
    pub(crate) fn dom_err(&self, it: &mut Interp<'_>, name: &str, msg: &str) -> Error {
        let r = (|| -> R<Value> {
            let o = it.new_error(self.p().domexc, msg)?;
            let n = self.js_str(it, name)?;
            let (kn, kc) = (self.key(it, "name")?, self.key(it, "code")?);
            it.define_data(o, kn, n, true, false, true)?;
            it.define_data(o, kc, Value::Number(domexc_codes(name) as f64), true, false, true)?;
            Ok(Value::Object(o))
        })();
        match r {
            Ok(v) => Error::Throw(v),
            Err(e) => e,
        }
    }

    // ------------------------------------------------------------ wrappers

    /// The wrapper of `node`. It is made with the wrappers of all its ancestors (see the header).
    pub(crate) fn wrap(&self, it: &mut Interp<'_>, node: NodeId) -> R<Value> {
        if let Some(&o) = self.st.borrow().by_node.get(&node) {
            return Ok(Value::Object(o));
        }
        let mut chain = Vec::new();
        {
            let (doc, st) = (self.doc.borrow(), self.st.borrow());
            let mut cur = Some(node);
            while let Some(n) = cur {
                if st.by_node.contains_key(&n) {
                    break;
                }
                it.charge(1)?;
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
                            "link" => p.link,
                            "style" => crate::browser_cssom::wrapper_proto(self,it)?,
                            "a" | "area" | "template" | "img" | "slot" => crate::browser_elements::proto(self, it, doc.atom_str(name))?,
                            t @ ("audio" | "video") => crate::browser_media::proto(self, it, n, t)?,
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
            let mut st = self.st.borrow_mut();
            st.by_node.insert(n, o);
            st.by_obj.insert(o, Target::Node(n));
            first.get_or_insert(o);
        }
        Ok(Value::Object(first.expect("chain has the node")))
    }

    pub(crate) fn wrap_opt(&self, it: &mut Interp<'_>, n: Option<NodeId>) -> R<Value> {
        match n {
            Some(n) => self.wrap(it, n),
            None => Ok(Value::Null),
        }
    }

    pub(crate) fn node_of(&self, v: Value) -> Option<NodeId> {
        let Value::Object(o) = v else { return None };
        match self.st.borrow().by_obj.get(&o) {
            Some(&Target::Node(n)) => Some(n),
            _ => None,
        }
    }

    pub(crate) fn nd(&self, it: &mut Interp<'_>, this: Value) -> R<NodeId> {
        match self.node_of(this) {
            Some(n) => Ok(n),
            None => Err(it.type_error("Illegal invocation")),
        }
    }

    pub(crate) fn arg_node(&self, it: &mut Interp<'_>, args: &[Value], i: usize) -> R<NodeId> {
        match self.node_of(args.get(i).copied().unwrap_or(Value::Undefined)) {
            Some(n) => Ok(n),
            None => Err(it.type_error(&format!("parameter {} is not of type 'Node'", i + 1))),
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
    pub(crate) fn selectors(&self, it: &mut Interp<'_>, text: &str) -> R<Rc<Vec<Selector>>> {
        if let Some(s) = self.st.borrow().sel_cache.get(text) {
            return Ok(s.clone());
        }
        if text.len() > self.lim.max_selector_len {
            return Err(self.lim("dom-selector"));
        }
        let parsed = {
            let mut doc = self.doc.borrow_mut();
            if doc.name_count() + text.len() > self.lim.max_names {
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
        if st.sel_cache.len() >= 256 {
            st.sel_cache.clear();
        }
        st.sel_cache.insert(text.to_string(), list.clone());
        Ok(list)
    }

    pub(crate) fn selectors_at(&self, it: &mut Interp<'_>, text: &str, scope: NodeId) -> R<Rc<Vec<Selector>>> {
        self.selector_scope.set(Some(scope));
        self.selectors(it, text)
    }

    // ------------------------------------------------------------ install

    fn pick(&self, p: P) -> ObjId {
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

    pub(crate) fn add_fn(&self, it: &mut Interp<'_>, h: H, name: &str, len: u32, ctor: bool) -> R<ObjId> {
        let id = self.handlers.borrow().len() as u32;
        self.handlers.borrow_mut().push((h, ctor));
        it.host_function(id, name, len, ctor)
    }

    /// A constructor that takes a different `new.target`: it reads it with `host_proto_from_new_target`.
    pub(crate) fn add_ctor_nt(&self, it: &mut Interp<'_>, h: H, name: &str, len: u32) -> R<ObjId> {
        let id = self.handlers.borrow().len() as u32;
        self.handlers.borrow_mut().push((h, true));
        it.host_ctor_with_new_target(id, name, len)
    }

    pub(crate) fn install(&self, it: &mut Interp<'_>) -> R<()> {
        let g = it.global_object();
        let _ = self.global.set(g);
        let op = it.object_prototype();
        let ep = it.error_prototype();
        let mk = |it: &mut Interp<'_>, parent: ObjId| self.obj(it, parent);
        let et = mk(it, op)?;
        let node = mk(it, et)?;
        let element = mk(it, node)?;
        let html = mk(it, element)?;
        let svg = mk(it, element)?;
        let input = mk(it, html)?;
        let iframe = mk(it, html)?;
        let link = mk(it, html)?;
        let form = mk(it, html)?;
        let button = mk(it, html)?;
        let chardata = mk(it, node)?;
        let text = mk(it, chardata)?;
        let comment = mk(it, chardata)?;
        let document = mk(it, node)?;
        let fragment = mk(it, node)?;
        let shadow = mk(it, fragment)?;
        let event = mk(it, op)?;
        let custom = mk(it, event)?;
        let focus = mk(it, event)?;
        let submit = mk(it, event)?;
        let mouse = mk(it, event)?;
        let pointer = mk(it, mouse)?;
        let keyboard = mk(it, event)?;
        let inputev = mk(it, event)?;
        let domexc = mk(it, ep)?;
        let _ = self.p.set(Protos { et, node, element, html, svg, input, iframe, link, form, button, submit, chardata, text, comment, document, fragment, shadow, event, custom, focus, mouse, pointer, keyboard, inputev, domexc });
        // The global object is `window`: it is an EventTarget.
        if let Err(e) = it.heap.set_prototype_of(g, Some(et)) {
            return Err(it.heap_error(e));
        }
        self.st.borrow_mut().by_obj.insert(g, Target::Window);
        let mut fixed = vec![et, node, element, html, svg, input, iframe, link, form, button, submit, chardata, text, comment, document, fragment, shadow, event, custom, focus, mouse, pointer, keyboard, inputev, domexc];

        // methods and accessors
        // One buffer for the names "get x" and "set x" (a `format!` for each of the 130 accessors cost 1.5% of the start).
        let mut nm = String::new();
        let named = |nm: &mut String, p: &str, n: &str| {
            nm.clear();
            nm.push_str(p);
            nm.push_str(n);
        };
        // One function object serves every target of a row (`Element.prototype.append === DocumentFragment.prototype.append`, unlike Chrome): a realm
        // pays for each function it makes, and most rows of the table have 2 or 3 targets.
        for d in DEFS {
            let key = self.key(it, d.name)?;
            let (f, s) = match d.kind {
                Kind::Method(h) => {
                    let id = self.handlers.borrow().len() as u32;
                    self.handlers.borrow_mut().push((h, false));
                    let PropertyKey::String(name) = key else { unreachable!("a method name is not an index") };
                    (it.host_function_interned(id, name, d.len, false)?, None)
                }
                Kind::Get(h) => {
                    named(&mut nm, "get ", d.name);
                    (self.add_fn(it, h, &nm, 0, false)?, None)
                }
                Kind::Accessor(gh, sh) => {
                    named(&mut nm, "get ", d.name);
                    let f = self.add_fn(it, gh, &nm, 0, false)?;
                    named(&mut nm, "set ", d.name);
                    (f, Some(self.add_fn(it, sh, &nm, 1, false)?))
                }
            };
            for &on in d.on {
                let target = self.pick(on);
                match d.kind {
                    Kind::Method(_) => it.define_data(target, key, Value::Object(f), true, false, true)?,
                    _ => self.define_accessor(it, target, key, f, s)?,
                }
            }
        }

        // interface objects
        let ifaces: [(&str, ObjId, Option<H>); 25] = [
            ("EventTarget", et, Some(ev::event_target_ctor as H)),
            ("Node", node, None),
            ("Element", element, None),
            ("HTMLElement", html, Some(ce::HANDLERS[0])),
            ("SVGElement", svg, None),
            ("HTMLInputElement", input, Some(ce::HANDLERS[6])),
            ("HTMLIFrameElement", iframe, Some(ce::HANDLERS[5])),
            ("HTMLLinkElement", link, Some(ce::HANDLERS[7])),
            ("HTMLFormElement", form, Some(ce::HANDLERS[4])),
            ("HTMLButtonElement", button, Some(ce::HANDLERS[3])),
            ("CharacterData", chardata, None),
            ("Text", text, None),
            ("Comment", comment, None),
            ("Document", document, None),
            ("DocumentFragment", fragment, None),
            ("ShadowRoot", shadow, None),
            ("Event", event, Some(ev::event_ctor as H)),
            ("CustomEvent", custom, Some(ev::custom_event_ctor as H)),
            ("FocusEvent", focus, Some(crate::browser_focus::focus_event_ctor as H)),
            ("SubmitEvent", submit, Some(crate::browser_form::submit_event_ctor as H)),
            ("MouseEvent", mouse, Some(crate::browser_uievent::ctor::<0> as H)),
            ("PointerEvent", pointer, Some(crate::browser_uievent::ctor::<1> as H)),
            ("KeyboardEvent", keyboard, Some(crate::browser_uievent::ctor::<2> as H)),
            ("InputEvent", inputev, Some(crate::browser_uievent::ctor::<3> as H)),
            ("DOMException", domexc, None),
        ];
        let (kp, kc) = (self.key(it, "prototype")?, self.key(it, "constructor")?);
        let mut ctors = Vec::new();
        for (name, proto, ctor) in ifaces {
            let f = match if name == "DocumentFragment" { Some(nd::fragment_ctor as H) } else { ctor } {
                Some(h) => self.add_ctor_nt(it, h, name, u32::from(name != "EventTarget" && name != "DocumentFragment" && !name.starts_with("HTML")))?,
                None => self.add_fn(it, illegal_constructor, name, 0, false)?,
            };
            it.define_data(f, kp, Value::Object(proto), false, false, false)?;
            it.define_data(proto, kc, Value::Object(f), true, false, true)?;
            let k = self.key(it, name)?;
            it.define_data(g, k, Value::Object(f), true, false, true)?;
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
            for &(n, v) in consts {
                let k = self.key(it, n)?;
                it.define_data(f, k, Value::Number(v), false, true, false)?;
                it.define_data(proto, k, Value::Number(v), false, true, false)?;
            }
        }
        *self.fixed.borrow_mut() = fixed;

        // globals: window, self, document
        let docw = self.wrap(it, self.doc.borrow().root())?;
        let Value::Object(docw) = docw else { unreachable!() };
        let _ = self.doc_wrapper.set(docw);
        for (name, v) in [("window", Value::Object(g)), ("self", Value::Object(g)), ("document", Value::Object(docw))] {
            let k = self.key(it, name)?;
            it.define_data(g, k, v, true, true, true)?;
        }
        crate::browser_navigation::install(self, it)?;
        crate::browser_fetch::install(self, it)?;
        crate::browser_loader::install(self, it)?;
        self.install_platform(it)?;
        at::install(self, it)?;
        crate::browser_nodelist::install(self, it)?;
        crate::browser_dataset::install(self, it)?;
        crate::browser_abort::install(self, it)?;
        crate::browser_worker::install(self, it)?;
        crate::browser_xhr::install(self, it)?;
        crate::browser_encoding::install(self, it)?;
        crate::browser_web::install(self, it)?;
        crate::browser_console::install(self, it)?;
        crate::browser_css_ns::install(self, it)?;
        crate::browser_style::install(self, it)?;
        crate::browser_computed::install(self, it)?;
        crate::browser_geometry::install(self, it)?;
        crate::browser_element::install(self, it)?;
        crate::browser_scroll::install(self, it)?;
        crate::browser_rects::install(self, it)?;
        crate::browser_resize::install(self, it)?;
        crate::browser_frame::install(self, it)?;
        crate::browser_idle::install(self, it)?;
        crate::browser_mutation::install(self, it)?;
        crate::browser_intersection::install(self, it)?;
        crate::browser_walker::install(self, it)?;
        crate::browser_range::install(self, it)?;
        crate::browser_handlers::install(self, it)?;
        crate::browser_error_event::install(self, it)?;
        crate::browser_canvas::install(self, it)?;
        crate::browser_rejection::install(self, it)?;
        crate::browser_perf_observer::install(self, it)?;
        crate::browser_select::install(self, it)?;
        // Last: a new host function earlier in this list moves the id of every one after it.
        crate::browser_elements::install(self, it)?;
        nd::install_late(self, it)?;
        crate::browser_controls::install(self, it)?;
        crate::browser_media::install(self, it)?;
        crate::browser_img::install(self, it)?;
        crate::browser_custom::install(self, it)?;
        crate::browser_geom::install(self, it)
    }

    pub(crate) fn define_accessor(&self, it: &mut Interp<'_>, o: ObjId, key: PropertyKey, get: ObjId, set: Option<ObjId>) -> R<()> {
        let d = PropertyDescriptor { get: Some(Some(get)), set: Some(set), enumerable: Some(true), configurable: Some(true), ..Default::default() };
        match it.heap.define_own_property(o, key, &d) {
            Ok(_) => Ok(()),
            Err(e) => Err(it.heap_error(e)),
        }
    }
}

pub(crate) fn unsupported(what: &'static str) -> bare_js::eval::Unsupported {
    bare_js::eval::Unsupported { what, span: Default::default() }
}

pub(crate) fn illegal_constructor(_: &Dom, it: &mut Interp<'_>, _: Value, _: &[Value]) -> R<Value> {
    Err(it.type_error("Illegal constructor"))
}

use Kind::{Accessor as A, Get as G, Method as M};
use P::*;

const PARENT: &[P] = &[Element, Document, Fragment];
const CHILD: &[P] = &[Element, CharData];

/// Every host method. The order is the order of function ids, but nothing depends on it.
pub(crate) static DEFS: &[Def] = &[
    Def { on: &[Et], name: "addEventListener", len: 2, kind: M(ev::add_event_listener) },
    Def { on: &[Et], name: "removeEventListener", len: 2, kind: M(ev::remove_event_listener) },
    Def { on: &[Et], name: "dispatchEvent", len: 1, kind: M(ev::dispatch_event) },
    Def { on: &[Node], name: "nodeType", len: 0, kind: G(nd::node_type) },
    Def { on: &[Node], name: "nodeName", len: 0, kind: G(nd::node_name) },
    Def { on: &[Node], name: "nodeValue", len: 0, kind: A(nd::node_value, nd::set_node_value) },
    Def { on: &[Node], name: "parentNode", len: 0, kind: G(nd::parent_node) },
    Def { on: &[Node], name: "parentElement", len: 0, kind: G(nd::parent_element) },
    Def { on: &[Node], name: "childNodes", len: 0, kind: G(nd::child_nodes) },
    Def { on: &[Node], name: "firstChild", len: 0, kind: G(nd::first_child) },
    Def { on: &[Node], name: "lastChild", len: 0, kind: G(nd::last_child) },
    Def { on: &[Node], name: "previousSibling", len: 0, kind: G(nd::previous_sibling) },
    Def { on: &[Node], name: "nextSibling", len: 0, kind: G(nd::next_sibling) },
    Def { on: &[Node], name: "ownerDocument", len: 0, kind: G(nd::owner_document) },
    Def { on: &[Node], name: "isConnected", len: 0, kind: G(nd::is_connected) },
    Def { on: &[Node], name: "textContent", len: 0, kind: A(nd::text_content, nd::set_text_content) },
    Def { on: &[Node], name: "appendChild", len: 1, kind: M(nd::append_child) },
    Def { on: &[Node], name: "insertBefore", len: 2, kind: M(nd::insert_before) },
    Def { on: &[Node], name: "removeChild", len: 1, kind: M(nd::remove_child) },
    Def { on: &[Node], name: "replaceChild", len: 2, kind: M(nd::replace_child) },
    Def { on: &[Node], name: "cloneNode", len: 0, kind: M(nd::clone_node) },
    Def { on: &[Node], name: "contains", len: 1, kind: M(crate::browser_nodecmp::contains) },
    Def { on: &[Node], name: "compareDocumentPosition", len: 1, kind: M(crate::browser_nodecmp::compare_document_position) },
    Def { on: &[Node], name: "isSameNode", len: 1, kind: M(crate::browser_nodecmp::is_same_node) },
    Def { on: &[Node], name: "isEqualNode", len: 1, kind: M(crate::browser_nodecmp::is_equal_node) },
    Def { on: &[Node], name: "lookupNamespaceURI", len: 1, kind: M(crate::browser_nodecmp::lookup_namespace_uri) },
    Def { on: &[Node], name: "lookupPrefix", len: 1, kind: M(crate::browser_nodecmp::lookup_prefix) },
    Def { on: &[Node], name: "isDefaultNamespace", len: 1, kind: M(crate::browser_nodecmp::is_default_namespace) },
    Def { on: &[Node], name: "getRootNode", len: 0, kind: M(nd::get_root_node) },
    Def { on: &[Node], name: "hasChildNodes", len: 0, kind: M(nd::has_child_nodes) },
    Def { on: PARENT, name: "children", len: 0, kind: G(nd::children) },
    Def { on: PARENT, name: "firstElementChild", len: 0, kind: G(nd::first_element_child) },
    Def { on: PARENT, name: "lastElementChild", len: 0, kind: G(nd::last_element_child) },
    Def { on: PARENT, name: "childElementCount", len: 0, kind: G(nd::child_element_count) },
    Def { on: PARENT, name: "querySelector", len: 1, kind: M(nd::query_selector) },
    Def { on: PARENT, name: "querySelectorAll", len: 1, kind: M(nd::query_selector_all) },
    Def { on: PARENT, name: "append", len: 0, kind: M(nd::append) },
    Def { on: PARENT, name: "prepend", len: 0, kind: M(nd::prepend) },
    Def { on: &[Element], name: "tagName", len: 0, kind: G(nd::tag_name) },
    Def { on: &[Element], name: "localName", len: 0, kind: G(nd::local_name) },
    Def { on: &[Element], name: "namespaceURI", len: 0, kind: G(ns::namespace_uri) },
    Def { on: &[Element], name: "prefix", len: 0, kind: G(ns::prefix) },
    Def { on: &[Element], name: "previousElementSibling", len: 0, kind: G(nd::previous_element_sibling) },
    Def { on: &[Element, Text], name: "assignedSlot", len: 0, kind: G(crate::browser_slot::assigned_slot) },
    Def { on: &[Element], name: "nextElementSibling", len: 0, kind: G(nd::next_element_sibling) },
    Def { on: &[Element, Shadow], name: "innerHTML", len: 0, kind: A(frag::inner_html, frag::set_inner_html) },
    Def { on: &[Element], name: "outerHTML", len: 0, kind: A(frag::outer_html, frag::set_outer_html) },
    Def { on: &[Element], name: "id", len: 0, kind: A(nd::id, nd::set_id) },
    Def { on: &[Element], name: "className", len: 0, kind: A(nd::class_name, nd::set_class_name) },
    Def { on: &[Element], name: "lang", len: 0, kind: A(nd::lang, nd::set_lang) },
    Def { on: &[Element], name: "getAttribute", len: 1, kind: M(nd::get_attribute) },
    Def { on: &[Element], name: "setAttribute", len: 2, kind: M(nd::set_attribute) },
    Def { on: &[Element], name: "removeAttribute", len: 1, kind: M(nd::remove_attribute) },
    Def { on: &[Element], name: "hasAttribute", len: 1, kind: M(nd::has_attribute) },
    Def { on: &[Element], name: "getAttributeNames", len: 0, kind: M(nd::get_attribute_names) },
    Def { on: &[Element], name: "attributes", len: 0, kind: G(at::attributes) },
    Def { on: &[Element], name: "hasAttributes", len: 0, kind: M(at::has_attributes) },
    Def { on: &[Element], name: "getAttributeNode", len: 1, kind: M(at::get_attribute_node) },
    Def { on: &[Element], name: "getAttributeNodeNS", len: 2, kind: M(at::get_attribute_node_ns) },
    Def { on: &[Element], name: "setAttributeNode", len: 1, kind: M(at::set_attribute_node) },
    Def { on: &[Element], name: "setAttributeNodeNS", len: 1, kind: M(at::set_attribute_node) },
    Def { on: &[Element], name: "removeAttributeNode", len: 1, kind: M(at::remove_attribute_node) },
    Def { on: &[Element], name: "getAttributeNS", len: 2, kind: M(ns::get_attribute_ns) },
    Def { on: &[Element], name: "setAttributeNS", len: 3, kind: M(ns::set_attribute_ns) },
    Def { on: &[Element], name: "hasAttributeNS", len: 2, kind: M(ns::has_attribute_ns) },
    Def { on: &[Element], name: "removeAttributeNS", len: 2, kind: M(ns::remove_attribute_ns) },
    Def { on: &[Element], name: "attachShadow", len: 1, kind: M(sh::attach_shadow) },
    Def { on: &[Element], name: "shadowRoot", len: 0, kind: G(sh::shadow_root) },
    Def { on: &[Shadow], name: "mode", len: 0, kind: G(sh::mode) },
    Def { on: &[Shadow], name: "delegatesFocus", len: 0, kind: G(sh::flag_bit::<1>) },
    Def { on: &[Shadow], name: "slotAssignment", len: 0, kind: G(sh::slot_assignment) },
    Def { on: &[Shadow], name: "clonable", len: 0, kind: G(sh::flag_bit::<2>) },
    Def { on: &[Shadow], name: "serializable", len: 0, kind: G(sh::flag_bit::<4>) },
    Def { on: &[Shadow], name: "host", len: 0, kind: G(sh::host) },
    Def { on: &[Element], name: "matches", len: 1, kind: M(nd::matches_selector) },
    Def { on: &[Element], name: "closest", len: 1, kind: M(nd::closest) },
    Def { on: CHILD, name: "remove", len: 0, kind: M(nd::remove) },
    Def { on: &[CharData], name: "data", len: 0, kind: A(nd::node_value_text, nd::set_node_value_text) },
    Def { on: &[CharData], name: "length", len: 0, kind: G(nd::data_length) },
    Def { on: &[CharData], name: "substringData", len: 2, kind: M(crate::browser_chardata::substring_data) },
    Def { on: &[CharData], name: "appendData", len: 1, kind: M(crate::browser_chardata::append_data) },
    Def { on: &[CharData], name: "insertData", len: 2, kind: M(crate::browser_chardata::insert_data) },
    Def { on: &[CharData], name: "deleteData", len: 2, kind: M(crate::browser_chardata::delete_data) },
    Def { on: &[CharData], name: "replaceData", len: 3, kind: M(crate::browser_chardata::replace_data) },
    Def { on: &[Text], name: "splitText", len: 1, kind: M(crate::browser_chardata::split_text) },
    Def { on: &[Node], name: "normalize", len: 0, kind: M(crate::browser_chardata::normalize) },
    Def { on: &[Document], name: "createElement", len: 1, kind: M(nd::create_element) },
    Def { on: &[Document], name: "createEvent", len: 1, kind: M(ev::create_event) },
    Def { on: &[Document], name: "createElementNS", len: 2, kind: M(ns::create_element_ns) },
    Def { on: &[Document], name: "getElementsByName", len: 1, kind: M(crate::browser_nodelist::get_elements_by_name) },
    Def { on: &[Document], name: "createAttribute", len: 1, kind: M(at::create_attribute) },
    Def { on: &[Document], name: "createAttributeNS", len: 2, kind: M(at::create_attribute_ns) },
    Def { on: &[Document], name: "createTextNode", len: 1, kind: M(nd::create_text_node) },
    Def { on: &[Document], name: "createComment", len: 1, kind: M(nd::create_comment) },
    Def { on: &[Document, Fragment], name: "getElementById", len: 1, kind: M(nd::get_element_by_id) },
    Def { on: &[Document, Shadow], name: "elementFromPoint", len: 2, kind: M(crate::browser_point::element_from_point) },
    Def { on: &[Document, Shadow], name: "elementsFromPoint", len: 2, kind: M(crate::browser_point::elements_from_point) },
    Def { on: &[Document], name: "documentElement", len: 0, kind: G(nd::document_element) },
    Def { on: &[Document], name: "head", len: 0, kind: G(nd::head) },
    Def { on: &[Document], name: "body", len: 0, kind: G(nd::body) },
    Def { on: &[Document], name: "title", len: 0, kind: A(nd::document_title, nd::set_document_title) },
    Def { on: &[Document], name: "doctype", len: 0, kind: G(nd::doctype) },
    Def { on: &[Document], name: "defaultView", len: 0, kind: G(nd::default_view) },
    Def { on: &[Input], name: "value", len: 0, kind: A(nd::input_value, nd::set_input_value) },
    Def { on: &[Input], name: "checked", len: 0, kind: A(nd::input_checked, nd::set_input_checked) },
    Def { on: &[Input], name: "indeterminate", len: 0, kind: A(nd::input_indeterminate, nd::set_input_indeterminate) },
    Def { on: &[Input], name: "selectionStart", len: 0, kind: A(sel::selection_start, sel::set_selection_start) },
    Def { on: &[Input], name: "selectionEnd", len: 0, kind: A(sel::selection_end, sel::set_selection_end) },
    Def { on: &[Input], name: "selectionDirection", len: 0, kind: A(sel::selection_direction, sel::set_selection_direction) },
    Def { on: &[Input], name: "setSelectionRange", len: 2, kind: M(sel::set_selection_range_method) },
    Def { on: &[Input], name: "select", len: 0, kind: M(sel::select_method) },
    Def { on: &[Input], name: "setRangeText", len: 1, kind: M(sel::set_range_text_input) },
    Def { on: &[Input], name: "type", len: 0, kind: A(nd::input_type, nd::set_input_type) },
    Def { on: &[Input], name: "defaultValue", len: 0, kind: A(nd::default_value, nd::set_default_value) },
    Def { on: &[Input], name: "defaultChecked", len: 0, kind: A(nd::default_checked, nd::set_default_checked) },
    Def { on: &[Link], name: "rel", len: 0, kind: A(nd::link_rel, nd::set_link_rel) },
    Def { on: &[Link], name: "href", len: 0, kind: A(crate::browser_loader::link_href, crate::browser_loader::set_link_href) },
    Def { on: &[Link], name: "as", len: 0, kind: A(nd::link_as, nd::set_link_as) },
    Def { on: &[Link], name: "crossOrigin", len: 0, kind: A(nd::link_cross_origin, nd::set_link_cross_origin) },
    Def { on: &[Document], name: "scripts", len: 0, kind: G(crate::browser_platform_query::scripts) },
    Def { on: &[Document], name: "forms", len: 0, kind: G(crate::browser_platform_query::forms) },
    Def { on: &[Document], name: "images", len: 0, kind: G(crate::browser_platform_query::images) },
    Def { on: &[Document], name: "embeds", len: 0, kind: G(crate::browser_platform_query::embeds) },
    Def { on: &[Document], name: "links", len: 0, kind: G(crate::browser_platform_query::links) },
    Def { on: &[Document], name: "anchors", len: 0, kind: G(crate::browser_platform_query::anchors) },
    Def { on: &[Input], name: "name", len: 0, kind: A(fm::input_name, fm::set_input_name) },
    Def { on: &[Input], name: "disabled", len: 0, kind: A(fm::input_disabled, fm::set_input_disabled) },
    Def { on: &[Input], name: "readOnly", len: 0, kind: A(fm::input_read_only, fm::set_input_read_only) },
    Def { on: &[Input], name: "form", len: 0, kind: G(fm::input_form) },
    Def { on: &[Input], name: "formNoValidate", len: 0, kind: A(fm::input_form_no_validate, fm::set_input_form_no_validate) },
    Def { on: &[Button], name: "name", len: 0, kind: A(fm::button_name, fm::set_button_name) },
    Def { on: &[Button], name: "type", len: 0, kind: A(fm::button_type, fm::set_button_type) },
    Def { on: &[Button], name: "disabled", len: 0, kind: A(fm::button_disabled, fm::set_button_disabled) },
    Def { on: &[Button], name: "form", len: 0, kind: G(fm::button_form) },
    Def { on: &[Button], name: "formNoValidate", len: 0, kind: A(fm::button_form_no_validate, fm::set_button_form_no_validate) },
    Def { on: &[Form], name: "noValidate", len: 0, kind: A(fm::form_no_validate, fm::set_form_no_validate) },
    Def { on: &[Form], name: "requestSubmit", len: 0, kind: M(crate::browser_submit::request_submit) },
    Def { on: &[Form], name: "submit", len: 0, kind: M(crate::browser_submit::submit_method) },
    Def { on: &[Form], name: "checkValidity", len: 0, kind: M(crate::browser_submit::form_check_validity) },
    Def { on: &[Form], name: "reportValidity", len: 0, kind: M(crate::browser_submit::form_report_validity) },
    Def { on: &[Input, Button], name: "checkValidity", len: 0, kind: M(crate::browser_submit::control_check_validity) },
    Def { on: &[Input, Button], name: "reportValidity", len: 0, kind: M(crate::browser_submit::control_report_validity) },
    Def { on: &[Submit], name: "submitter", len: 0, kind: G(fm::submit_event_submitter) },
    Def { on: &[Event], name: "type", len: 0, kind: G(ev::event_type) },
    Def { on: &[Event], name: "target", len: 0, kind: G(ev::event_target) },
    Def { on: &[Event], name: "currentTarget", len: 0, kind: G(ev::event_current_target) },
    Def { on: &[Event], name: "eventPhase", len: 0, kind: G(ev::event_phase) },
    Def { on: &[Event], name: "bubbles", len: 0, kind: G(ev::event_bubbles) },
    Def { on: &[Event], name: "cancelable", len: 0, kind: G(ev::event_cancelable) },
    Def { on: &[Event], name: "composed", len: 0, kind: G(ev::event_composed) },
    Def { on: &[Event], name: "defaultPrevented", len: 0, kind: G(ev::event_default_prevented) },
    Def { on: &[Event], name: "isTrusted", len: 0, kind: G(ev::event_is_trusted) },
    Def { on: &[Event], name: "timeStamp", len: 0, kind: G(ev::event_time_stamp) },
    Def { on: &[Event], name: "preventDefault", len: 0, kind: M(ev::prevent_default) },
    Def { on: &[Event], name: "stopPropagation", len: 0, kind: M(ev::stop_propagation) },
    Def { on: &[Event], name: "stopImmediatePropagation", len: 0, kind: M(ev::stop_immediate_propagation) },
    Def { on: &[Event], name: "composedPath", len: 0, kind: M(ev::composed_path) },
    Def { on: &[Event], name: "initEvent", len: 1, kind: M(ev::init_event) },
    Def { on: &[Custom], name: "initCustomEvent", len: 1, kind: M(ev::init_custom_event) },
    Def { on: &[Custom], name: "detail", len: 0, kind: G(ev::custom_detail) },
    Def { on: &[Html], name: "focus", len: 0, kind: M(crate::browser_focus::focus) },
    Def { on: &[Html], name: "blur", len: 0, kind: M(crate::browser_focus::blur) },
    Def { on: &[Document], name: "activeElement", len: 0, kind: G(crate::browser_focus::active_element) },
    Def { on: &[Focus], name: "relatedTarget", len: 0, kind: G(crate::browser_focus::related_target) },
    Def { on: &[Mouse], name: "clientX", len: 0, kind: G(ui::client_x) },
    Def { on: &[Mouse], name: "clientY", len: 0, kind: G(ui::client_y) },
    Def { on: &[Mouse], name: "screenX", len: 0, kind: G(ui::screen_x) },
    Def { on: &[Mouse], name: "screenY", len: 0, kind: G(ui::screen_y) },
    Def { on: &[Mouse], name: "pageX", len: 0, kind: G(ui::page_x) },
    Def { on: &[Mouse], name: "pageY", len: 0, kind: G(ui::page_y) },
    Def { on: &[Mouse], name: "x", len: 0, kind: G(ui::client_x) },
    Def { on: &[Mouse], name: "y", len: 0, kind: G(ui::client_y) },
    Def { on: &[Mouse], name: "button", len: 0, kind: G(ui::button) },
    Def { on: &[Mouse], name: "buttons", len: 0, kind: G(ui::buttons) },
    Def { on: &[Mouse, Keyboard, InputEv], name: "detail", len: 0, kind: G(ui::detail) },
    Def { on: &[Mouse, Keyboard], name: "shiftKey", len: 0, kind: G(ui::shift) },
    Def { on: &[Mouse, Keyboard], name: "ctrlKey", len: 0, kind: G(ui::ctrl) },
    Def { on: &[Mouse, Keyboard], name: "altKey", len: 0, kind: G(ui::alt) },
    Def { on: &[Mouse, Keyboard], name: "metaKey", len: 0, kind: G(ui::meta) },
    Def { on: &[Mouse], name: "relatedTarget", len: 0, kind: G(ui::related) },
    Def { on: &[Pointer], name: "pointerType", len: 0, kind: G(ui::pointer_type) },
    Def { on: &[Pointer], name: "isPrimary", len: 0, kind: G(ui::is_primary) },
    Def { on: &[Pointer], name: "pointerId", len: 0, kind: G(ui::pointer_id) },
    Def { on: &[Keyboard], name: "key", len: 0, kind: G(ui::key) },
    Def { on: &[Keyboard], name: "code", len: 0, kind: G(ui::code) },
    Def { on: &[Keyboard], name: "repeat", len: 0, kind: G(ui::repeat) },
    Def { on: &[Keyboard], name: "keyCode", len: 0, kind: G(ui::key_code) },
    Def { on: &[Keyboard], name: "which", len: 0, kind: G(ui::key_code) },
    Def { on: &[Keyboard], name: "location", len: 0, kind: G(ui::location) },
    Def { on: &[Keyboard, InputEv], name: "isComposing", len: 0, kind: G(ui::is_composing) },
    Def { on: &[InputEv], name: "data", len: 0, kind: G(ui::data) },
    Def { on: &[InputEv], name: "inputType", len: 0, kind: G(ui::input_type) },
];

// ---------------------------------------------------------------------------------------------- Browser

/// One realm with a DOM. This is the type tests and tools use.
pub struct Browser {
    pub it: Interp<'static>,
    pub(crate) dom: Rc<Dom>,
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

impl Browser {
    /// A realm over `doc` with `window`, `document` and the DOM interfaces installed.
    pub fn new(doc: Document, limits: BrowserLimits, eval: EvalLimits, heap: HeapLimits) -> R<Browser> {
        for (used, cap, name) in [
            (doc.len(), limits.max_nodes, "dom-nodes"),
            (doc.text_bytes(), limits.max_text_bytes, "dom-text"),
            (doc.attr_slots(), limits.max_attr_slots, "dom-attrs"),
            (doc.name_count(), limits.max_names, "dom-names"),
        ] {
            if used > cap { return Err(Error::Limit(LimitKind::Host(name))); }
        }
        let heap = Heap::new(heap).map_err(Error::Heap)?;
        let mut it = Interp::realm(heap, eval)?;
        let dom = Rc::new(Dom {
            doc: Rc::new(RefCell::new(doc)),
            st: RefCell::default(),
            lim: limits,
            p: OnceCell::new(),
            global: OnceCell::new(),
            selector_scope: Default::default(),
            doc_wrapper: OnceCell::new(),
            handlers: RefCell::default(),
            fixed: RefCell::default(),
            start: Instant::now(),
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
            frames: Default::default(),
            perf_entries: Default::default(),
            html_ifaces: Default::default(),
            html_on: Default::default(),
            scrolling: Default::default(),
            resize: Default::default(),
            frame: Default::default(),
            idle: Default::default(),
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
        });
        it.set_host(dom.clone())?;
        it.heap.set_indexed_source(dom.clone());
        it.heap.set_named_source(dom.clone());
        it.host_scope(|it| {
            dom.install(it)?;
            dom.install_scheduler(it)?;
            crate::browser_frames::activate(&dom, it)
        })?;
        Ok(Browser { it, dom })
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
    pub fn survive(&mut self, e: Error, what: &str) -> Survive {
        let Error::Limit(k) = e else { return Survive::Fatal };
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
        let _ = self.it.host_scope(|_| Ok(()));
        crate::browser_console::push_text(&self.dom, &mut self.it, crate::browser_console::ConsoleLevel::Error, &format!("Script aborted: the {k:?} budget ended the {what}. The page goes on ({n} of {} aborts used)", self.dom.lim.max_task_aborts));
        Survive::Aborted
    }

    pub fn run_script(&mut self, source: &str) -> R<Value> {
        self.it.run_script(source)
    }

    /// The microtask checkpoint (Promise jobs).
    pub fn drain(&mut self) -> R<usize> {
        self.it.drain_microtasks()
    }

    /// Collect the heap. Attached wrappers and their listeners live. `extra` are values the caller still holds.
    pub fn collect(&mut self, extra: &[Value]) -> R<bare_js::heap::Collected> {
        self.it.collect(extra)
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
    pub fn wrapper(&mut self, node: NodeId) -> R<Value> {
        let dom = self.dom.clone();
        self.it.host_scope(|it| dom.wrap(it, node))
    }

    /// The native node of a wrapper value.
    pub fn node(&self, v: Value) -> Option<NodeId> {
        self.dom.node_of(v)
    }

    /// First element in tree order that matches `selector` (native lookup; JS does not run).
    pub fn query(&mut self, selector: &str) -> R<Option<NodeId>> {
        let dom = self.dom.clone();
        self.it.host_scope(|it| {
            let root = dom.doc.borrow().root();
            Ok(dom.query(it, root, selector, false)?.into_iter().next())
        })
    }

    // ----- the test driver: set native state, then dispatch a normal event -----

    /// Dispatch a new event of type `ty` at `node`. `isTrusted` is false. After each listener that the driver
    /// started, the microtask queue is drained (a checkpoint with an empty JS stack).
    pub fn dispatch(&mut self, node: NodeId, ty: &str, bubbles: bool, cancelable: bool) -> R<Dispatched> {
        let dom = self.dom.clone();
        let mark = {
            let st = dom.st.borrow();
            st.dropped_reports + st.reports.len() as u64
        };
        dom.st.borrow_mut().driver = true;
        let r = self.it.host_scope(|it| {
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

    /// `submit` at a form (bubbles, cancelable). No validation, no navigation.
    pub fn submit(&mut self, form: NodeId) -> R<Dispatched> {
        self.dispatch(form, "submit", true, true)
    }

    /// Like a user typing: set the native value of a text-like `<input>`, then dispatch `input` (bubbles).
    pub fn type_text(&mut self, input: NodeId, text: &str) -> R<Dispatched> {
        self.dom.driver_input(input, Some(text), None)?;
        self.dispatch(input, "input", true, false)
    }

    /// Like leaving a field: dispatch `change` (bubbles). The value was set by `type_text`.
    pub fn commit_change(&mut self, input: NodeId) -> R<Dispatched> {
        self.dom.driver_input(input, None, None)?;
        self.dispatch(input, "change", true, false)
    }

    /// Set the native checkedness of a checkbox or radio, then dispatch `input` and `change`.
    /// Radio groups are not modeled: other radios stay as they are.
    pub fn set_checked(&mut self, input: NodeId, checked: bool) -> R<Dispatched> {
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
