//! Event handlers (HTML Standard, "event handlers"): the IDL attributes `onclick` and the rest of `GlobalEventHandlers` and
//! `WindowEventHandlers`, and the content attributes (`<button onclick="...">`).
//!
//! - Where: `Window` (own accessors of the global object), `Document.prototype`, `HTMLElement.prototype`, `SVGElement.prototype`,
//!   and `HTMLBodyElement.prototype` / `HTMLFrameSetElement.prototype` (the window handlers, which read and write the handler of
//!   `window`). The names are Chrome 155's (102 for an element, 117 for a document, 125 for the window, 24 for a body). `onabort` is
//!   also on `AbortSignal.prototype` (`install_onabort`). Gaps: no `MathMLElement` (`browser_namespace.rs`), the 8 names of
//!   `Element.prototype` (`onsearch`, `onfullscreenchange`, ...), and no `FileReader`, `WebSocket`, `Animation`, `HTMLMediaElement`
//!   handlers (those interfaces do not exist here). `MediaQueryList.onchange` and `MessagePort.onmessage` keep their own code.
//! - Lazy accessors: a page that never reads a handler pays nothing for the ~430 accessors. A lookup, `in` or write of an `on<name>`
//!   string key that finds nothing reaches `EventTarget.prototype` (`Heap::set_lazy_chain`) and asks the host once per name (`lazy`).
//!   The host defines that name on every owner that has it (window, document, element, svg, body). The owners themselves are marked
//!   too (`Heap::set_lazy_on`), so `hasOwnProperty` and `getOwnPropertyDescriptor` on them ask. `Object.keys` and the like ask once per
//!   owner for all names. Each accessor has its own getter and setter function. All getters share one host function and all setters
//!   another; the call reads its name from `style.cur`, the id of the running host function (`Hd::fns`).
//! - Storage: the handler is one `Listener` of the target with `handler: true` and `cb` = the current value. Setting an existing
//!   handler changes `cb` in place (the slot keeps its order). Null removes the listener; a later non-null value appends a new one.
//!   A non-callable object is kept and skipped when the event runs; a non-object means null. The handler is never the same listener
//!   as `addEventListener(type, sameFunction)`.
//! - Content attributes: no hook on attribute changes. The document name table only grows, so `sync` scans the names added since the last
//!   scan for `on<known name>` and keeps their atoms (`Hd::atoms`). A document with none pays one compare per dispatch (`sync_path_of`). When one
//!   exists, `sync(target, type)` reads the attribute of the element and compares it with `Hd::seen`, the value the slot was last
//!   made from: a new or changed value activates the handler in place (a raw slot: `Hd::raw`, `cb` = the global object as a
//!   placeholder), a removed attribute deactivates it, an unchanged value leaves the slot alone (so `el.onclick = null` and a property
//!   value stay). `sync` runs before an event turn, before a handler property is read or written, and before `addEventListener` (all
//!   names of that target), so the handler has the position that the attribute had in the listener list when no one looked in between.
//!   Deviations: a re-set of the same value does not replace a property value; remove then set again keeps the old position.
//! - Compile: a raw slot is compiled when it is read or when its turn comes (`Interp::compile_function`): `function onclick(event)`,
//!   with `evt` on an SVG element and five parameters on the `error` handler of the window. The body of a window handler on `<body>` or
//!   `<frameset>` is the attribute of the first `body` or `frameset` child of the root element. A syntax error is reported
//!   (`Dom::report`: a window `error` event) once and leaves the handler null.
//! - Scope: the code of an attribute handler looks names up in the element, its form owner, and the document, then the globals
//!   (`Interp::call_scoped`). A window handler has no scope object. Deviations: a global `let` or `const` wins over the scope; a
//!   function that runs after the handler returned does not see the scope; a handler function moved to another element runs with
//!   the scope of the element that dispatches.
//! - Dispatch (`browser_events.rs`, `call_handler`): `this` is the current target, `false` cancels a cancelable event, a throw is reported,
//!   a Limit/Unsupported/heap fault ends the dispatch. The `error` handler of the window gets `(message, filename, lineno, colno, error)`
//!   for an `ErrorEvent`, and `true` cancels (`false` does not).
//! - Caps: a new slot counts against `max_listeners` and `max_listeners_per_target` before anything changes.
//! - Not done: the special return value of `beforeunload` (no page leaves here), line and column of an `ErrorEvent` (0).

use crate::browser::*;
use bare_html::dom::{Atom, Document, NodeData, NodeId, Ns};
use bare_js::eval::{Error, Interp};
use bare_js::value::{ObjId, Value};
use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

type V = R<Value>;

const WINDOW: u8 = 0;
const DOCUMENT: u8 = 1;
const HTML: u8 = 2;
const SVG: u8 = 3;
const SIGNAL: u8 = 4;
const BODY: u8 = 5;

// The event types of the handlers (the name without `on`). Chrome 155 `Object.getOwnPropertyNames` of the prototypes.
const ELEMENT: &str = "\
    abort beforeinput beforematch beforetoggle blur cancel canplay canplaythrough change click close command contentvisibilityautostatechange contextlost \
    contextmenu contextrestored cuechange dblclick drag dragend dragenter dragleave dragover dragstart drop durationchange emptied ended error focus \
    formdata input invalid keydown keypress keyup load loadeddata loadedmetadata loadstart mousedown mouseenter mouseleave mousemove mouseout mouseover \
    mouseup mousewheel pause play playing progress ratechange reset resize scroll scrollend securitypolicyviolation seeked seeking select slotchange \
    stalled submit suspend timeupdate toggle volumechange waiting webkitanimationend webkitanimationiteration webkitanimationstart webkittransitionend \
    wheel auxclick gotpointercapture lostpointercapture pointerdown pointermove pointerup pointercancel pointerover pointerout pointerenter pointerleave \
    selectstart selectionchange animationcancel animationend animationiteration animationstart transitionrun transitionstart transitionend \
    transitioncancel beforexrselect copy cut paste scrollsnapchange scrollsnapchanging pointerrawupdate";
const DOC_EXTRA: &str = "\
    readystatechange pointerlockchange pointerlockerror beforecopy beforecut beforepaste freeze prerenderingchange resume search visibilitychange \
    fullscreenchange fullscreenerror webkitfullscreenchange webkitfullscreenerror";
const WIN_EXTRA: &str = "\
    pagereveal search appinstalled beforeinstallprompt afterprint beforeprint beforeunload hashchange languagechange message messageerror offline online \
    pagehide pageshow popstate rejectionhandled storage unhandledrejection unload devicemotion deviceorientation deviceorientationabsolute pageswap \
    gamepadconnected gamepaddisconnected";
/// The window handlers that `<body>` and `<frameset>` reflect.
const BODY_NAMES: &str = "\
    blur error focus load resize scroll afterprint beforeprint beforeunload hashchange languagechange message messageerror offline online pagehide \
    pageshow popstate rejectionhandled storage unhandledrejection unload gamepadconnected gamepaddisconnected";

fn listed(list: &'static str, n: &str) -> bool {
    find_in(list, n).is_some()
}

/// The word `n` of `list`, as a slice of `list`.
fn find_in(list: &'static str, n: &str) -> Option<&'static str> {
    let b = list.as_bytes();
    let mut from = 0;
    while !n.is_empty() && let Some(i) = list[from..].find(n) {
        let (s, e) = (from + i, from + i + n.len());
        if (s == 0 || b[s - 1].is_ascii_whitespace()) && (e == b.len() || b[e].is_ascii_whitespace()) {
            return Some(&list[s..e]);
        }
        from = s + 1;
    }
    None
}

/// Does the interface of kind `k` have the handler `on<n>`?
fn has(k: u8, n: &str) -> bool {
    match k {
        HTML | SVG => listed(ELEMENT, n),
        DOCUMENT => listed(ELEMENT, n) || listed(DOC_EXTRA, n),
        WINDOW => (listed(ELEMENT, n) && !matches!(n, "copy" | "cut" | "paste")) || listed(WIN_EXTRA, n),
        BODY => listed(BODY_NAMES, n),
        _ => false,
    }
}

fn names(k: u8) -> Vec<&'static str> {
    let lists: &[&'static str] = match k {
        HTML | SVG => &[ELEMENT],
        DOCUMENT => &[DOC_EXTRA, ELEMENT],
        WINDOW => &[WIN_EXTRA, ELEMENT],
        BODY => &[BODY_NAMES],
        _ => &[],
    };
    lists.iter().flat_map(|l| l.split_whitespace()).filter(|n| has(k, n)).collect()
}

/// The event type that the handler `on<n>` listens for.
fn etype(n: &str) -> &str {
    match n {
        "webkitanimationend" => "webkitAnimationEnd",
        "webkitanimationiteration" => "webkitAnimationIteration",
        "webkitanimationstart" => "webkitAnimationStart",
        "webkittransitionend" => "webkitTransitionEnd",
        _ => n,
    }
}

fn known(n: &str) -> bool {
    listed(ELEMENT, n) || listed(DOC_EXTRA, n) || listed(WIN_EXTRA, n)
}

/// The handler state of the host (`State::hd`).
#[derive(Default)]
pub(crate) struct Hd {
    /// Listener id of a handler slot that has an attribute value and no function yet -> the source text.
    pub raw: IdMap<u64, Rc<str>>,
    /// `(target, type)` -> the attribute value that the slot was last made from.
    pub seen: HashMap<(Target, Rc<str>), Rc<str>>,
    /// The attribute names `on<known name>` that the document has had, with their event types.
    pub atoms: Vec<(Atom, Rc<str>)>,
    /// How many names of the document table `atoms` has looked at.
    pub scanned: usize,
    /// Host function id of an accessor function -> (interface kind, event type). Indexed by the id.
    pub fns: Vec<(u8, &'static str)>,
    /// The names `n` of `on<n>` that the host defined on every interface (the `EventTarget.prototype` ask), for a `body` prototype made later.
    pub asked: Vec<&'static str>,
    /// `window.event`: the event of the dispatch that runs (`Dom::dispatch`).
    pub event: Option<ObjId>,
    /// A window `error` event is running: an exception in it is not reported again.
    pub in_error: bool,
}

/// `HTMLBodyElement` and `HTMLFrameSetElement` (prototype, constructor), `ErrorEvent` (`browser_error_event.rs`) and
/// `PromiseRejectionEvent` (`browser_rejection.rs`), once made. All are roots.
#[derive(Default)]
pub(crate) struct Hh {
    pub body: [Cell<Option<[ObjId; 2]>>; 2],
    pub err: Cell<Option<[ObjId; 2]>>,
    pub rej: Cell<Option<[ObjId; 2]>>,
}

pub(crate) fn roots(d: &Dom, out: &mut Vec<Value>) {
    for c in d.hh.body.iter().chain([&d.hh.err, &d.hh.rej]) {
        if let Some(o) = c.get() {
            out.extend(o.map(Value::Object));
        }
    }
}

impl Dom {
    /// The owner of a handler of interface `k`, or `Illegal invocation`. Window also takes `undefined` and `null`. A body or frameset
    /// element gives the window.
    fn handler_owner(&self, it: &mut Interp<'_>, this: Value, k: u8) -> R<Target> {
        let t = match this {
            Value::Undefined | Value::Null if k == WINDOW => Some(Target::Window),
            Value::Object(o) => self.st.borrow().by_obj.get(&o).copied(),
            _ => None,
        };
        let ok = match t {
            Some(Target::Window) => k == WINDOW,
            Some(Target::Obj(o)) => k == SIGNAL && self.is_signal(o),
            Some(Target::Node(n)) => {
                let doc = self.doc.borrow();
                match (doc.data(n), k) {
                    (NodeData::Document, DOCUMENT) => true,
                    (NodeData::Element { .. }, HTML) => doc.element_ns(n) == Ns::Html,
                    (NodeData::Element { .. }, SVG) => doc.element_ns(n) == Ns::Svg,
                    (NodeData::Element { .. }, BODY) => body_like(&doc, n),
                    _ => false,
                }
            }
            _ => false,
        };
        match t {
            Some(Target::Node(_)) if ok && k == BODY => Ok(Target::Window),
            Some(t) if ok => Ok(t),
            _ => Err(it.type_error("Illegal invocation")),
        }
    }

    /// The kind and event type of the accessor function that is running.
    fn handler_fn(&self) -> (u8, &'static str) {
        let (st, id) = (self.st.borrow(), self.style.cur.get());
        st.hd.fns[id as usize]
    }
}

fn body_like(doc: &Document, n: NodeId) -> bool {
    doc.element_ns(n) == Ns::Html && matches!(doc.data(n), NodeData::Element { name, .. } if matches!(doc.atom_str(*name), "body" | "frameset"))
}

/// The `body` or `frameset` child of the root element: the element whose attributes are the window handlers.
fn body_element(doc: &Document) -> Option<NodeId> {
    let html = doc.children(doc.root()).find(|&c| matches!(doc.data(c), NodeData::Element { .. }))?;
    doc.children(html).find(|&c| body_like(doc, c))
}

fn hget(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let (k, ty) = d.handler_fn();
    let t = d.handler_owner(it, this, k)?;
    handler_value(d, it, t, &ty)
}

fn hset(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let (k, ty) = d.handler_fn();
    let t = d.handler_owner(it, this, k)?;
    sync(d, t, &ty);
    handler_set(d, t, &ty, a.first().copied())?;
    Ok(Value::Undefined)
}

fn sig_get(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let t = d.handler_owner(it, this, SIGNAL)?;
    Ok(handler_get(d, t, "abort"))
}

fn sig_set(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let t = d.handler_owner(it, this, SIGNAL)?;
    handler_set(d, t, "abort", a.first().copied())?;
    Ok(Value::Undefined)
}

/// The value of the handler `on<ty>` of `t`, or `null`, with no attribute and no compile (for the host-made targets).
pub(crate) fn handler_get(d: &Dom, t: Target, ty: &str) -> Value {
    let st = d.st.borrow();
    st.listeners.get(&t).and_then(|v| v.iter().find(|l| l.handler && *l.ty == *ty)).map_or(Value::Null, |l| Value::Object(l.cb))
}

/// The value of the handler `on<ty>` of `t` (the getter): the attribute is read and compiled if it is new.
fn handler_value(d: &Dom, it: &mut Interp<'_>, t: Target, ty: &str) -> V {
    sync(d, t, ty);
    let found = {
        let st = d.st.borrow();
        st.listeners.get(&t).and_then(|v| v.iter().find(|l| l.handler && *l.ty == *ty)).map(|l| (l.id, l.cb, !st.hd.raw.is_empty() && st.hd.raw.contains_key(&l.id)))
    };
    match found {
        None => Ok(Value::Null),
        Some((id, _, true)) => compile_slot(d, it, t, ty, id),
        Some((_, cb, false)) => Ok(Value::Object(cb)),
    }
}

/// Set the handler `on<ty>` of `t` (see the header for the rules).
pub(crate) fn handler_set(d: &Dom, t: Target, ty: &str, value: Option<Value>) -> R<()> {
    let cb: Option<ObjId> = match value {
        Some(Value::Object(o)) => Some(o),
        _ => None,
    };
    let mut st = d.st.borrow_mut();
    let st = &mut *st;
    let at = st.listeners.get(&t).and_then(|v| v.iter().position(|l| l.handler && *l.ty == *ty));
    match (at, cb) {
        (Some(i), Some(cb)) => {
            let l = &mut st.listeners.get_mut(&t).expect("found")[i];
            l.cb = cb;
            if !st.hd.raw.is_empty() {
                st.hd.raw.remove(&l.id);
            }
        }
        (Some(i), None) => {
            let v = st.listeners.get_mut(&t).expect("found");
            let id = v.remove(i).id;
            if !st.hd.raw.is_empty() {
                st.hd.raw.remove(&id);
            }
            if v.is_empty() {
                st.listeners.remove(&t);
            }
            st.n_listeners -= 1;
        }
        (None, Some(cb)) => {
            if st.n_listeners >= d.lim.max_listeners {
                return Err(d.lim("dom-listeners"));
            }
            if st.listeners.get(&t).map_or(0, Vec::len) >= d.lim.max_listeners_per_target {
                return Err(d.lim("dom-listeners-per-target"));
            }
            st.next_id += 1;
            let id = st.next_id;
            st.listeners.entry(t).or_default().push(Listener { id, ty: Rc::from(ty), cb, capture: false, once: false, passive: false, handler: true });
            st.n_listeners += 1;
        }
        (None, None) => {}
    }
    Ok(())
}

// ------------------------------------------------------------------------------------------ content attributes

/// Look at the names added to the document since the last scan.
fn scan(doc: &Document, hd: &mut Hd) {
    for i in hd.scanned..doc.name_count() {
        let atom = doc.atom_at(i);
        if let Some(n) = doc.atom_str(atom).strip_prefix("on")
            && known(n)
        {
            hd.atoms.push((atom, Rc::from(etype(n))));
        }
    }
    hd.scanned = doc.name_count();
}

/// Does the document have no name that starts with `on`? One flag when it has none.
#[inline]
fn idle(d: &Dom) -> bool {
    !d.doc.borrow().has_on_name()
}

/// Bring the handler `on<ty>` of `t` up to date with its content attribute (see the header). Cheap when the document has no `on*` name.
#[inline]
pub(crate) fn sync(d: &Dom, t: Target, ty: &str) {
    if !matches!(t, Target::Obj(_)) && !idle(d) {
        sync_slow(d, t, ty);
    }
}

fn sync_slow(d: &Dom, t: Target, ty: &str) {
    let doc = d.doc.borrow();
    let mut st = d.st.borrow_mut();
    if doc.name_count() != st.hd.scanned {
        scan(&doc, &mut st.hd);
    }
    if let Some(i) = st.hd.atoms.iter().position(|(_, x)| **x == *ty) {
        let (atom, ty) = st.hd.atoms[i].clone();
        sync_one(d, &doc, &mut st, t, &ty, atom);
    }
}

/// `sync` for every target of the path of a dispatch. One test when the document has no `on*` attribute name.
pub(crate) fn sync_path_of(d: &Dom, path: &[Target], ty: &str) {
    if idle(d) {
        return;
    }
    for &t in path {
        sync(d, t, ty);
    }
}

/// `sync` of the handler `on<ty>` for `n`, its ancestors and the window: before the host looks whether anything listens on the path.
pub(crate) fn sync_path(d: &Dom, n: NodeId, ty: &str) {
    {
        let doc = d.doc.borrow();
        let st = d.st.borrow();
        if doc.name_count() == st.hd.scanned && st.hd.atoms.is_empty() {
            return;
        }
    }
    let mut cur = Some(n);
    while let Some(c) = cur {
        sync(d, Target::Node(c), ty);
        cur = d.doc.borrow().parent(c);
    }
    sync(d, Target::Window, ty);
}

/// `sync` for every handler name of the document: before a listener is added to `t`.
#[inline]
pub(crate) fn sync_all(d: &Dom, t: Target) {
    if !matches!(t, Target::Obj(_)) && !idle(d) {
        sync_all_slow(d, t);
    }
}

fn sync_all_slow(d: &Dom, t: Target) {
    let doc = d.doc.borrow();
    let mut st = d.st.borrow_mut();
    if doc.name_count() != st.hd.scanned {
        scan(&doc, &mut st.hd);
    }
    for i in 0..st.hd.atoms.len() {
        let (atom, ty) = st.hd.atoms[i].clone();
        sync_one(d, &doc, &mut st, t, &ty, atom);
    }
}

fn sync_one(d: &Dom, doc: &Document, st: &mut State, t: Target, ty: &Rc<str>, atom: Atom) {
    let lower = ty.to_ascii_lowercase();
    let el = match t {
        Target::Window if listed(BODY_NAMES, &lower) => body_element(doc),
        Target::Node(n) if matches!(doc.data(n), NodeData::Element { .. }) && !(body_like(doc, n) && listed(BODY_NAMES, &lower)) => Some(n),
        _ => return,
    };
    let attr = el.and_then(|n| doc.attr(n, atom));
    let key = (t, ty.clone());
    match (attr, st.hd.seen.get(&key)) {
        (None, None) => {}
        (None, Some(_)) => {
            st.hd.seen.remove(&key);
            deactivate(st, t, ty);
        }
        (Some(v), Some(s)) if **s == *v => {}
        (Some(v), _) => {
            let v: Rc<str> = Rc::from(v);
            st.hd.seen.insert(key, v.clone());
            activate(d, st, t, ty, v);
        }
    }
}

/// The handler of `t` for `ty` becomes the raw text `src` (in place when it exists, else a new slot).
fn activate(d: &Dom, st: &mut State, t: Target, ty: &Rc<str>, src: Rc<str>) {
    let g = *d.global.get().expect("installed");
    if let Some(l) = st.listeners.get_mut(&t).and_then(|v| v.iter_mut().find(|l| l.handler && *l.ty == **ty)) {
        l.cb = g;
        st.hd.raw.insert(l.id, src);
        return;
    }
    // At a cap the attribute is not made a handler (the property setter reports the cap; a markup attribute has no caller to tell).
    if st.n_listeners >= d.lim.max_listeners || st.listeners.get(&t).map_or(0, Vec::len) >= d.lim.max_listeners_per_target {
        return;
    }
    st.next_id += 1;
    let id = st.next_id;
    st.listeners.entry(t).or_default().push(Listener { id, ty: ty.clone(), cb: g, capture: false, once: false, passive: false, handler: true });
    st.n_listeners += 1;
    st.hd.raw.insert(id, src);
}

fn deactivate(st: &mut State, t: Target, ty: &Rc<str>) {
    let Some(v) = st.listeners.get_mut(&t) else { return };
    let Some(i) = v.iter().position(|l| l.handler && *l.ty == **ty) else { return };
    st.hd.raw.remove(&v.remove(i).id);
    if v.is_empty() {
        st.listeners.remove(&t);
    }
    st.n_listeners -= 1;
}

/// Compile the raw handler `id` of `t`. A syntax error is reported and removes the slot (the handler is null).
pub(crate) fn compile_slot(d: &Dom, it: &mut Interp<'_>, t: Target, ty: &str, id: u64) -> V {
    let Some(src) = d.st.borrow_mut().hd.raw.remove(&id) else { return Ok(Value::Null) };
    let svg = matches!(t, Target::Node(n) if d.doc.borrow().element_ns(n) == Ns::Svg);
    let params = if matches!(t, Target::Window) && ty == "error" {
        "event, source, lineno, colno, error"
    } else if svg {
        "evt"
    } else {
        "event"
    };
    match it.compile_function(&format!("on{}", ty.to_ascii_lowercase()), params, &src) {
        Ok(f) => {
            if let Some(l) = d.st.borrow_mut().listeners.get_mut(&t).and_then(|v| v.iter_mut().find(|l| l.id == id)) {
                l.cb = f;
            }
            Ok(Value::Object(f))
        }
        Err(Error::Throw(v)) => {
            d.remove_listener(t, id);
            d.report(it, v, "error", t);
            Ok(Value::Null)
        }
        Err(e) => Err(e),
    }
}

/// The objects that the names of the handler attribute of `t` are looked up in before the globals: the element, its form owner (and the
/// form's named controls), and the document. A window handler has none. The named controls are a plain object made now: the first
/// listed control of the form for each `name` or `id` (deviation: a radio group is its first button, not a `RadioNodeList`).
pub(crate) fn scope_of(d: &Dom, it: &mut Interp<'_>, t: Target) -> R<Vec<ObjId>> {
    let Target::Node(n) = t else { return Ok(Vec::new()) };
    let mut out = Vec::with_capacity(4);
    let mut push = |v: Value| {
        if let Value::Object(o) = v {
            out.push(o);
        }
    };
    push(d.wrap(it, n)?);
    let form = if d.doc.borrow().element_ns(n) == Ns::Html && matches!(d.data(n), NodeData::Element { name, .. } if d.doc.borrow().atom_str(name) == "form") { Some(n) } else { d.form_owner(it, n)? };
    if let Some(f) = form {
        if f != n {
            push(d.wrap(it, f)?);
        }
        let names = form_names(d, it, f)?;
        push(Value::Object(names));
    }
    let root = d.doc.borrow().root();
    push(d.wrap(it, root)?);
    Ok(out)
}

/// A null-prototype object with the listed controls of `form` by `name` and `id` (tree order, the first wins).
fn form_names(d: &Dom, it: &mut Interp<'_>, form: NodeId) -> R<ObjId> {
    let o = it.heap.new_object(None).map_err(|e| it.heap_error(e))?;
    let mut found: Vec<(NodeId, Vec<String>)> = Vec::new();
    {
        let doc = d.doc.borrow();
        let mut stack = vec![form];
        while let Some(c) = stack.pop() {
            it.charge(1)?;
            let kids: Vec<NodeId> = doc.children(c).collect();
            stack.extend(kids.into_iter().rev());
            if let NodeData::Element { name, .. } = doc.data(c)
                && matches!(doc.atom_str(*name), "button" | "fieldset" | "input" | "object" | "output" | "select" | "textarea")
            {
                let keys: Vec<String> = [doc.find_atom("name"), Some(Atom::ID)].into_iter().flatten().filter_map(|a| doc.attr(c, a)).filter(|v| !v.is_empty()).map(str::to_string).collect();
                if !keys.is_empty() {
                    found.push((c, keys));
                }
            }
        }
    }
    for (c, keys) in found {
        if d.form_owner(it, c)? != Some(form) {
            continue;
        }
        let w = d.wrap(it, c)?;
        for k in keys {
            let key = d.key(it, &k)?;
            if !it.heap.has_own_property(o, key).map_err(|e| it.heap_error(e))? {
                it.define_data(o, key, w, true, true, true)?;
            }
        }
    }
    Ok(o)
}

// ------------------------------------------------------------------------------------------ lazy accessors

/// The kind of interface whose accessors the object `owner` holds.
fn kind_of(d: &Dom, owner: ObjId) -> Option<u8> {
    let p = d.p();
    let g = *d.global.get()?;
    if owner == g {
        Some(WINDOW)
    } else if owner == p.document {
        Some(DOCUMENT)
    } else if owner == p.html {
        Some(HTML)
    } else if owner == p.svg {
        Some(SVG)
    } else if d.hh.body.iter().any(|c| c.get().is_some_and(|o| o[0] == owner)) {
        Some(BODY)
    } else {
        None
    }
}

/// `Dom::fill_indexed` for `LAZY_ON` (the name `it.heap.lazy_on_key()`) and `LAZY_ALL` (every name) of `owner`.
pub(crate) fn lazy(d: &Dom, it: &mut Interp<'_>, owner: ObjId, all: bool) -> R<()> {
    let Some(key) = (!all).then(|| it.heap.lazy_on_key()).flatten() else {
        return match kind_of(d, owner) {
            Some(k) if all => {
                it.heap.clear_lazy_on(owner);
                for n in names(k) {
                    define(d, it, owner, k, n)?;
                }
                Ok(())
            }
            _ => Ok(()),
        };
    };
    if crate::browser::define_window(d,it,&key)? || crate::browser_cssom::define(d,it,&key)? { return Ok(()); }
    if crate::browser_svg::define(d,it,&key)? || crate::browser_shims::define(d,it,&key)? { return Ok(()); }
    match key.as_str() {
        "write" => {d.def_method(it,d.p().document,"write",1,crate::browser_loader::document_write)?; return Ok(());},
        "writeln" => {d.def_method(it,d.p().document,"writeln",1,crate::browser_loader::document_writeln)?; return Ok(());},
        "hasFocus" => {d.def_method(it,d.p().document,"hasFocus",0,crate::browser_gaps::has_focus)?; return Ok(());},
        "open" => {d.def_method(it,d.p().document,"open",0,crate::browser_loader::document_open)?; return Ok(());},
        "close" => {d.def_method(it,d.p().document,"close",0,crate::browser_loader::document_close)?; return Ok(());},
        _=>{}
    }
    if matches!(key.as_str(), "getContext" | "toDataURL") {
        crate::browser_html::define_global(d, it, "HTMLCanvasElement")?;
        return Ok(());
    }
    if key.starts_with("HTML") && crate::browser_html::define_global(d, it, &key)? {
        return Ok(());
    }
    let Some(n) = key.strip_prefix("on").and_then(|n| find_in(ELEMENT, n).or_else(|| find_in(DOC_EXTRA, n)).or_else(|| find_in(WIN_EXTRA, n))) else { return Ok(()) };
    if owner != d.p().et {
        return match kind_of(d, owner) {
            Some(k) if has(k, n) => define(d, it, owner, k, n),
            _ => Ok(()),
        };
    }
    // A chain that reached `EventTarget.prototype` found nothing: the name goes on every interface that has it. A `body` prototype made
    // later takes the names asked so far (`body_proto`).
    let g = *d.global.get().expect("installed");
    let p = d.p();
    let bodies = d.hh.body.iter().filter_map(|c| c.get()).map(|b| (BODY, b[0]));
    for (k, o) in [(WINDOW, g), (DOCUMENT, p.document), (HTML, p.html), (SVG, p.svg)].into_iter().chain(bodies) {
        if has(k, n) {
            define(d, it, o, k, n)?;
        }
    }
    d.st.borrow_mut().hd.asked.push(n);
    Ok(())
}

/// Define the accessor `on<n>` of kind `k` on `owner`, unless it is there.
fn define(d: &Dom, it: &mut Interp<'_>, owner: ObjId, k: u8, n: &'static str) -> R<()> {
    let name = format!("on{n}");
    let key = d.key(it, &name)?;
    if it.heap.has_own_property(owner, key).map_err(|e| it.heap_error(e))? {
        return Ok(());
    }
    let id = d.handlers.borrow().len() as u32;
    let gf = d.add_fn(it, hget, &format!("get {name}"), 0, false)?;
    let sf = d.add_fn(it, hset, &format!("set {name}"), 1, false)?;
    let ty = etype(n);
    let mut st = d.st.borrow_mut();
    st.hd.fns.resize(id as usize + 2, (0, ""));
    st.hd.fns[id as usize..].fill((k, ty));
    drop(st);
    d.define_accessor(it, owner, key, gf, Some(sf))
}

/// `onabort` of `AbortSignal.prototype`, made with the lazy `AbortSignal` (`browser_abort.rs`).
pub(crate) fn install_onabort(d: &Dom, it: &mut Interp<'_>, proto: ObjId) -> R<()> {
    d.def_get(it, proto, "onabort", sig_get, Some(sig_set))
}

pub(crate) fn install(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    let p = d.p();
    let g = *d.global.get().expect("installed");
    for o in [g, p.document, p.html, p.svg] {
        it.heap.set_lazy_on(o).map_err(|e| it.heap_error(e))?;
    }
    it.heap.set_lazy_chain(p.et).map_err(|e| it.heap_error(e))?;
    crate::browser_lazy::install(d, it, &[("HTMLBodyElement", lazy_body as H), ("HTMLFrameSetElement", lazy_frameset)])
}

// ------------------------------------------------------------------------------------------ body and frameset

const BODY_TAGS: [&str; 2] = ["HTMLBodyElement", "HTMLFrameSetElement"];

/// `[prototype, constructor]` of `HTMLBodyElement` (`i` = 0) or `HTMLFrameSetElement` (1), made on first use.
pub(crate) fn body_proto(d: &Dom, it: &mut Interp<'_>, i: usize) -> R<[ObjId; 2]> {
    if let Some(o) = d.hh.body[i].get() {
        return Ok(o);
    }
    let name = BODY_TAGS[i];
    let proto = d.obj(it, d.p().html)?;
    let f = d.add_fn(it, illegal_constructor, name, 0, false)?;
    let (kp, kc) = (d.key(it, "prototype")?, d.key(it, "constructor")?);
    it.define_data(f, kp, Value::Object(proto), false, false, false)?;
    it.define_data(proto, kc, Value::Object(f), true, false, true)?;
    crate::browser_lazy::tag(d, it, proto, name)?;
    it.heap.set_lazy_on(proto).map_err(|e| it.heap_error(e))?;
    d.hh.body[i].set(Some([proto, f]));
    let asked = d.st.borrow().hd.asked.clone();
    for &n in asked.iter().filter(|n| has(BODY, n)) {
        define(d, it, proto, BODY, n)?;
    }
    Ok([proto, f])
}

fn ensure_body(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    body_proto(d, it, 0).map(|_| ())
}

fn ensure_frameset(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    body_proto(d, it, 1).map(|_| ())
}

crate::browser_lazy::lazy_global!(lazy_body, "HTMLBodyElement", ensure_body, |d| d.hh.body[0].get().expect("made")[1]);
crate::browser_lazy::lazy_global!(lazy_frameset, "HTMLFrameSetElement", ensure_frameset, |d| d.hh.body[1].get().expect("made")[1]);


#[cold]
#[inline(never)]
pub(crate) fn tags(d:&Dom,it:&mut Interp<'_>)->R<()> {
    let p=d.p();
    let key=it.symbol_to_string_tag();
    for (proto,name) in [(p.et,"EventTarget"),(p.node,"Node"),(p.element,"Element"),(p.html,"HTMLElement"),(p.svg,"SVGElement"),(p.document,"Document"),(p.text,"Text"),(p.comment,"Comment"),(it.global_object(),"Window")] {
        if it.heap.peek_own_property(proto,key).map_err(|e|it.heap_error(e))?.is_none() {
            let value=Value::String(it.heap.intern_str(name).map_err(|e|it.heap_error(e))?);
            it.define_data(proto,key,value,false,false,true)?;
        }
    }
    Ok(())
}
