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
//!   `HTMLInputElement`, `CharacterData`, `Text`, `Comment`, `Document`, `EventTarget`, `Event`, `CustomEvent`,
//!   `DOMException`.
//! - Node: `nodeType nodeName nodeValue parentNode parentElement childNodes firstChild lastChild previousSibling
//!   nextSibling ownerDocument isConnected textContent appendChild insertBefore removeChild replaceChild cloneNode
//!   contains hasChildNodes`.
//! - Element: `tagName localName id className getAttribute setAttribute removeAttribute hasAttribute getAttributeNames
//!   children firstElementChild lastElementChild previousElementSibling nextElementSibling childElementCount append
//!   prepend remove matches closest querySelector querySelectorAll` (selectors from `bare_css`).
//! - CharacterData: `data length`. Document: `createElement createElementNS createTextNode createComment getElementById
//!   documentElement head body doctype defaultView querySelector(All)`.
//! - `<input>`: `value checked type defaultValue defaultChecked` with native state (dirty value, checkedness).
//! - Events: `Event`, `CustomEvent` (`detail`), `addEventListener` (capture, once, passive; function or `handleEvent`
//!   object), `removeEventListener`, `dispatchEvent`, capture / target / bubble phases through the document to `window`,
//!   `preventDefault`, `stopPropagation`, `stopImmediatePropagation`, `composedPath`, `new EventTarget()`.
//! - A listener that throws is reported (`Browser::take_reports`) and dispatch goes on. A Limit, an Unsupported or a
//!   heap fault stops the dispatch and is the result of the call.
//!
//! Gates and deviations (on purpose, none is silently faked)
//! - Lists (`childNodes`, `children`, `querySelectorAll`) are plain snapshot arrays, not live `NodeList`/`HTMLCollection`.
//! - A JS string with a lone surrogate cannot be stored in the UTF-8 document: `Error::Unsupported`. Valid pairs round-trip.
//! - `createElementNS` accepts only the HTML namespace. Other namespaces, `namespaceURI`, SVG/MathML and shadow DOM
//!   are `Unsupported` or absent. `new Document()`, `DocumentFragment`, `innerHTML`, `outerHTML`, `attributes`,
//!   `classList`, `dataset`, `style`, `getElementsBy*`, `before/after/replaceWith`, layout APIs, `location` and
//!   network are absent. Timers, `queueMicrotask`, `performance.now` and `MessageChannel` are in `browser_scheduler.rs`.
//! - `addEventListener` option `signal` is `Unsupported`. `isTrusted` is always false. Events made by the test driver
//!   are plain `Event`s (no `MouseEvent`/`InputEvent` fields).
//! - Default actions are NOT implemented: no link navigation, no form submission or validation, no checkbox toggle or
//!   radio-group exclusion on click, no focus. The test driver sets native input state itself, then dispatches events.
//! - `<input>` has no value sanitization (newline stripping, number/date parsing). `select` and `textarea` have no
//!   `value` state.
//! - Microtasks: the test driver runs a checkpoint after each listener it started from the host. Dispatch from JS does not.
//! - Subclassing host constructors with `class X extends Event` needs `new.target` plumbing in the evaluator (not done).
//!
//! Not browser-verified: nothing here was compared with Chrome. DOM behavior follows the DOM Standard by reading, and
//! is tested against structural expectations written by hand. Only plain `EventTarget`/`Event` semantics have a Node
//! oracle (`tools/js/browser_oracle.mjs`).

use crate::browser_dom as nd;
use crate::browser_events as ev;
pub use crate::browser_scheduler::{Pump, SchedLimits, SchedStats};
use bare_css::selector::{Selector, parse_selector_list};
use bare_html::dom::{Document, NodeData, NodeId};
use bare_js::eval::{Error, EvalLimits, Host, Interp, LimitKind};
use bare_js::heap::{Heap, HeapLimits, PropertyDescriptor};
use bare_js::value::{ObjId, PropertyKey, Value};
use std::cell::{OnceCell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::time::Instant;

pub(crate) type R<T> = Result<T, Error>;
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
}

pub(crate) struct EventState {
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
}

#[derive(Default)]
pub(crate) struct State {
    pub by_node: HashMap<NodeId, ObjId>,
    pub by_obj: HashMap<ObjId, Target>,
    pub listeners: HashMap<Target, Vec<Listener>>,
    pub n_listeners: usize,
    pub next_id: u64,
    pub events: HashMap<ObjId, EventState>,
    pub inputs: HashMap<NodeId, Input>,
    /// Bytes of the dirty values in `inputs`.
    pub input_bytes: usize,
    pub reports: VecDeque<Report>,
    pub dropped_reports: u64,
    pub sel_cache: HashMap<String, Rc<Vec<Selector>>>,
    /// True while the test driver runs a dispatch from the host (a microtask checkpoint follows each listener).
    pub driver: bool,
}

#[derive(Clone, Copy)]
pub(crate) struct Protos {
    pub et: ObjId,
    pub node: ObjId,
    pub element: ObjId,
    pub html: ObjId,
    pub input: ObjId,
    pub chardata: ObjId,
    pub text: ObjId,
    pub comment: ObjId,
    pub document: ObjId,
    pub event: ObjId,
    pub custom: ObjId,
    pub domexc: ObjId,
}

#[derive(Clone, Copy)]
pub(crate) enum P {
    Et,
    Node,
    Element,
    Input,
    CharData,
    Document,
    Event,
    Custom,
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
    pub(crate) doc_wrapper: OnceCell<ObjId>,
    /// `(handler, needs new)` for each host function id.
    pub(crate) handlers: RefCell<Vec<(H, bool)>>,
    /// Prototypes and interface objects: permanent roots.
    pub(crate) fixed: RefCell<Vec<ObjId>>,
    pub(crate) start: Instant,
    pub(crate) sched: RefCell<crate::browser_scheduler::Sched>,
    pub(crate) fetch: crate::browser_fetch::Fetch,
}

impl Host for Dom {
    fn call(&self, it: &mut Interp<'_>, id: u32, this: Value, args: &[Value], is_new: bool) -> R<Value> {
        let (h, needs_new) = self.handlers.borrow()[id as usize];
        if needs_new && !is_new {
            return Err(it.type_error("Failed to construct: please use the 'new' operator"));
        }
        h(self, it, this, args)
    }

    fn roots(&self, out: &mut Vec<Value>) {
        out.extend(self.fixed.borrow().iter().map(|&o| Value::Object(o)));
        out.extend(self.doc_wrapper.get().map(|&o| Value::Object(o)));
        self.sched_roots(out);
        self.fetch.roots(out);
    }

    fn trace(&self, owner: ObjId, out: &mut Vec<Value>) {
        self.sched_trace(owner, out);
        let st = self.st.borrow();
        if let Some(&t) = st.by_obj.get(&owner) {
            for l in st.listeners.get(&t).into_iter().flatten() {
                out.push(Value::Object(l.cb));
            }
            if let Target::Node(n) = t {
                let doc = self.doc.borrow();
                out.extend(doc.parent(n).and_then(|p| st.by_node.get(&p)).map(|&o| Value::Object(o)));
                out.extend(doc.children(n).filter_map(|c| st.by_node.get(&c)).map(|&o| Value::Object(o)));
            }
        } else if let Some(e) = st.events.get(&owner) {
            out.extend([e.target, e.current, e.detail]);
            for &t in &e.path {
                match t {
                    Target::Node(n) => out.extend(st.by_node.get(&n).map(|&o| Value::Object(o))),
                    Target::Obj(o) => out.push(Value::Object(o)),
                    Target::Window => {}
                }
            }
        }
        self.fetch.trace(owner, out);
    }

    fn prune(&self, live: &dyn Fn(ObjId) -> bool) {
        let mut st = self.st.borrow_mut();
        let st = &mut *st;
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
        self.sched_prune(live);
        self.fetch.prune(live);
    }

    fn report_exception(&self, it: &mut Interp<'_>, thrown: Value) {
        self.report(it, thrown, "microtask", Target::Window);
    }
}

fn domexc_codes(name: &str) -> u32 {
    match name {
        "HierarchyRequestError" => 3,
        "InvalidCharacterError" => 5,
        "NotFoundError" => 8,
        "NotSupportedError" => 9,
        "InvalidStateError" => 11,
        "SyntaxError" => 12,
        "DataCloneError" => 25,
        _ => 0,
    }
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
            if st.by_obj.len() + chain.len() > self.lim.max_wrappers {
                return Err(self.lim("dom-wrappers"));
            }
        }
        let p = self.p();
        let mut first = None;
        for &n in chain.iter() {
            let proto = {
                let doc = self.doc.borrow();
                match *doc.data(n) {
                    NodeData::Element { name, .. } => match doc.atom_str(name) {
                        "input" => p.input,
                        "svg" | "math" => p.element,
                        _ => p.html,
                    },
                    NodeData::Text(_) => p.text,
                    NodeData::Comment(_) => p.comment,
                    NodeData::Document => p.document,
                    NodeData::Doctype(_) => p.node,
                }
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
            parse_selector_list(text, &mut doc)
        };
        let Some(list) = parsed else { return Err(self.dom_err(it, "SyntaxError", &format!("'{text}' is not a valid selector"))) };
        // `a b c d` can backtrack a lot: refuse more than two descendant or later-sibling combinators.
        if list.iter().any(|s| nd::backtracking(s) > 2) {
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

    // ------------------------------------------------------------ install

    fn pick(&self, p: P) -> ObjId {
        let ps = self.p();
        match p {
            P::Et => ps.et,
            P::Node => ps.node,
            P::Element => ps.element,
            P::Input => ps.input,
            P::CharData => ps.chardata,
            P::Document => ps.document,
            P::Event => ps.event,
            P::Custom => ps.custom,
        }
    }

    pub(crate) fn add_fn(&self, it: &mut Interp<'_>, h: H, name: &str, len: u32, ctor: bool) -> R<ObjId> {
        let id = self.handlers.borrow().len() as u32;
        self.handlers.borrow_mut().push((h, ctor));
        it.host_function(id, name, len, ctor)
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
        let input = mk(it, html)?;
        let chardata = mk(it, node)?;
        let text = mk(it, chardata)?;
        let comment = mk(it, chardata)?;
        let document = mk(it, node)?;
        let event = mk(it, op)?;
        let custom = mk(it, event)?;
        let domexc = mk(it, ep)?;
        let _ = self.p.set(Protos { et, node, element, html, input, chardata, text, comment, document, event, custom, domexc });
        // The global object is `window`: it is an EventTarget.
        if let Err(e) = it.heap.set_prototype_of(g, Some(et)) {
            return Err(it.heap_error(e));
        }
        self.st.borrow_mut().by_obj.insert(g, Target::Window);
        let mut fixed = vec![et, node, element, html, input, chardata, text, comment, document, event, custom, domexc];

        // methods and accessors
        for d in DEFS {
            let key = self.key(it, d.name)?;
            for &on in d.on {
                let target = self.pick(on);
                match d.kind {
                    Kind::Method(h) => {
                        let f = self.add_fn(it, h, d.name, d.len, false)?;
                        it.define_data(target, key, Value::Object(f), true, false, true)?;
                    }
                    Kind::Get(h) => {
                        let f = self.add_fn(it, h, &format!("get {}", d.name), 0, false)?;
                        self.define_accessor(it, target, key, f, None)?;
                    }
                    Kind::Accessor(gh, sh) => {
                        let f = self.add_fn(it, gh, &format!("get {}", d.name), 0, false)?;
                        let s = self.add_fn(it, sh, &format!("set {}", d.name), 1, false)?;
                        self.define_accessor(it, target, key, f, Some(s))?;
                    }
                }
            }
        }

        // interface objects
        let ifaces: [(&str, ObjId, Option<H>); 12] = [
            ("EventTarget", et, Some(ev::event_target_ctor as H)),
            ("Node", node, None),
            ("Element", element, None),
            ("HTMLElement", html, None),
            ("HTMLInputElement", input, None),
            ("CharacterData", chardata, None),
            ("Text", text, None),
            ("Comment", comment, None),
            ("Document", document, None),
            ("Event", event, Some(ev::event_ctor as H)),
            ("CustomEvent", custom, Some(ev::custom_event_ctor as H)),
            ("DOMException", domexc, None),
        ];
        let (kp, kc) = (self.key(it, "prototype")?, self.key(it, "constructor")?);
        let mut ctors = Vec::new();
        for (name, proto, ctor) in ifaces {
            let f = self.add_fn(it, ctor.unwrap_or(illegal_constructor), name, 0, ctor.is_some())?;
            it.define_data(f, kp, Value::Object(proto), false, false, false)?;
            it.define_data(proto, kc, Value::Object(f), true, false, true)?;
            let k = self.key(it, name)?;
            it.define_data(g, k, Value::Object(f), true, false, true)?;
            fixed.push(f);
            ctors.push((name, f));
        }
        // constants
        for (iface, consts) in [
            ("Node", &[("ELEMENT_NODE", 1.0), ("TEXT_NODE", 3.0), ("COMMENT_NODE", 8.0), ("DOCUMENT_NODE", 9.0), ("DOCUMENT_TYPE_NODE", 10.0)][..]),
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
        crate::browser_fetch::install(self, it)
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

fn illegal_constructor(_: &Dom, it: &mut Interp<'_>, _: Value, _: &[Value]) -> R<Value> {
    Err(it.type_error("Illegal constructor"))
}

use Kind::{Accessor as A, Get as G, Method as M};
use P::*;

const PARENT: &[P] = &[Element, Document];
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
    Def { on: &[Node], name: "contains", len: 1, kind: M(nd::contains) },
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
    Def { on: &[Element], name: "previousElementSibling", len: 0, kind: G(nd::previous_element_sibling) },
    Def { on: &[Element], name: "nextElementSibling", len: 0, kind: G(nd::next_element_sibling) },
    Def { on: &[Element], name: "id", len: 0, kind: A(nd::id, nd::set_id) },
    Def { on: &[Element], name: "className", len: 0, kind: A(nd::class_name, nd::set_class_name) },
    Def { on: &[Element], name: "getAttribute", len: 1, kind: M(nd::get_attribute) },
    Def { on: &[Element], name: "setAttribute", len: 2, kind: M(nd::set_attribute) },
    Def { on: &[Element], name: "removeAttribute", len: 1, kind: M(nd::remove_attribute) },
    Def { on: &[Element], name: "hasAttribute", len: 1, kind: M(nd::has_attribute) },
    Def { on: &[Element], name: "getAttributeNames", len: 0, kind: M(nd::get_attribute_names) },
    Def { on: &[Element], name: "matches", len: 1, kind: M(nd::matches_selector) },
    Def { on: &[Element], name: "closest", len: 1, kind: M(nd::closest) },
    Def { on: CHILD, name: "remove", len: 0, kind: M(nd::remove) },
    Def { on: &[CharData], name: "data", len: 0, kind: A(nd::node_value_text, nd::set_node_value_text) },
    Def { on: &[CharData], name: "length", len: 0, kind: G(nd::data_length) },
    Def { on: &[Document], name: "createElement", len: 1, kind: M(nd::create_element) },
    Def { on: &[Document], name: "createElementNS", len: 2, kind: M(nd::create_element_ns) },
    Def { on: &[Document], name: "createTextNode", len: 1, kind: M(nd::create_text_node) },
    Def { on: &[Document], name: "createComment", len: 1, kind: M(nd::create_comment) },
    Def { on: &[Document], name: "getElementById", len: 1, kind: M(nd::get_element_by_id) },
    Def { on: &[Document], name: "documentElement", len: 0, kind: G(nd::document_element) },
    Def { on: &[Document], name: "head", len: 0, kind: G(nd::head) },
    Def { on: &[Document], name: "body", len: 0, kind: G(nd::body) },
    Def { on: &[Document], name: "doctype", len: 0, kind: G(nd::doctype) },
    Def { on: &[Document], name: "defaultView", len: 0, kind: G(nd::default_view) },
    Def { on: &[Input], name: "value", len: 0, kind: A(nd::input_value, nd::set_input_value) },
    Def { on: &[Input], name: "checked", len: 0, kind: A(nd::input_checked, nd::set_input_checked) },
    Def { on: &[Input], name: "type", len: 0, kind: A(nd::input_type, nd::set_input_type) },
    Def { on: &[Input], name: "defaultValue", len: 0, kind: A(nd::default_value, nd::set_default_value) },
    Def { on: &[Input], name: "defaultChecked", len: 0, kind: A(nd::default_checked, nd::set_default_checked) },
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
    Def { on: &[Custom], name: "detail", len: 0, kind: G(ev::custom_detail) },
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
            doc_wrapper: OnceCell::new(),
            handlers: RefCell::default(),
            fixed: RefCell::default(),
            start: Instant::now(),
            sched: RefCell::default(),
            fetch: Default::default(),
        });
        it.set_host(dom.clone())?;
        it.host_scope(|it| {
            dom.install(it)?;
            dom.install_scheduler(it)
        })?;
        Ok(Browser { it, dom })
    }

    /// The native document. Do not hold a mutable borrow across a call into the browser.
    pub fn document(&self) -> Rc<RefCell<Document>> {
        self.dom.doc.clone()
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

    /// `click` (bubbles, cancelable). No default action runs (see the header).
    pub fn click(&mut self, node: NodeId) -> R<Dispatched> {
        self.dispatch(node, "click", true, true)
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
}
