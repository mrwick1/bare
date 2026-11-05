//! Browser platform APIs the staging login page needs before it can hydrate: `matchMedia`/`MediaQueryList`,
//! `Element.classList`/`DOMTokenList`, `getElementsByTagName`, `getElementsByClassName`. Files: this one (state,
//! GC hooks, install, media), `browser_platform_class.rs` (`classList`), `browser_platform_query.rs` (`getElementsBy*`),
//! `browser_platform_tests.rs`.
//!
//! Media environment (what `matches` is computed from)
//! - A viewport in CSS pixels. The default is 1280 x 800, the window `main.rs` lays out with (the realm has no window of
//!   its own yet). The driver changes it with `Browser::set_viewport`. `window.innerWidth` and friends do not exist.
//! - The colour scheme is light. This is what `bare_css::at_rules::media_matches` answers for `prefers-color-scheme`
//!   (`light` is true, `dark` is false), the same function the cascade uses for `@media`. It does not read any OS
//!   setting and it is not configurable. `Browser::media_environment()` reports both values.
//! - Subset: exactly the queries `media_matches` understands (`screen`/`all`, `not`/`only`, `and`, commas, `width`,
//!   `height`, `orientation`, `resolution`, `prefers-color-scheme` (light only), `prefers-reduced-motion` (no-preference),
//!   `hover`, `pointer`, range syntax). A query it does not understand is false, as a media type the engine does not know.
//!   Not done: the "not all" rewrite of a query with a syntax error, and the canonical serialization. `media` is the
//!   query text with ASCII white space trimmed at both ends. The empty query matches.
//!
//! `MediaQueryList`
//! - A real `EventTarget`: it is a `Target::Obj` of the existing event engine (`addEventListener`, `removeEventListener`,
//!   `dispatchEvent`, listener lists, capture/once flags, reports for a throwing listener). Legacy `addListener` and
//!   `removeListener` call those. `onchange` is one listener of type `change` that the setter keeps in place (it keeps
//!   its position when the function is replaced). Gate: `removeEventListener("change", fn)` with the same function also
//!   removes it, and `addEventListener("change", fn)` with the same function is a duplicate (event handlers are not
//!   separate from listeners here).
//! - `Browser::set_viewport(w, h)` evaluates every live list in creation order, stores the new `matches` of all of them,
//!   then fires `change` at each list that changed, in that order. The event is a plain `Event` (not a
//!   `MediaQueryListEvent`) with own, read-only `matches` and `media`. A microtask checkpoint follows each listener, as
//!   for other driver events. A listener that throws is reported. A Limit, an Unsupported or a heap fault halts this
//!   driver API for good: later `set_viewport` calls return the same error and run no JavaScript.
//! - Lifetime: a list with at least one `change` listener is a root. It lives, with its callbacks, until the last
//!   listener goes. A list without listeners lives while JavaScript can reach it. All lists count toward
//!   `max_media_lists`, and the rooted ones are never freed, so that cap bounds them. Objects are pruned in `prune`.
//!
//! `classList` (see `browser_platform_class.rs`) and `getElementsBy*` (see `browser_platform_query.rs`) have their own
//! headers. Nothing here is compared with Chrome.

use crate::browser::*;
use bare_js::eval::{Error, Interp};
use bare_js::value::{ObjId, PropertyKey, Value};
use bare_html::dom::NodeId;
use std::cell::{Cell, OnceCell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

pub(crate) type V = R<Value>;

/// Bounds of the platform module. Each is checked before the thing grows (`Limit(Host(name))`).
#[derive(Clone, Copy, Debug)]
pub struct PlatformLimits {
    /// Live `MediaQueryList` objects (listeners keep them alive, so this bounds them too). `media-lists`.
    pub max_media_lists: usize,
    /// Bytes of one media query. `media-query`.
    pub max_media_query_bytes: usize,
    /// Live `DOMTokenList` objects (one for each element that used `classList`). `token-lists`.
    pub max_token_lists: usize,
    /// Live `DOMTokenList` iterators. `token-iterators`.
    pub max_token_iters: usize,
    /// Tokens in one `add`/`remove` call, or class names in one `getElementsByClassName` call. `class-tokens`.
    pub max_class_tokens: usize,
}

impl Default for PlatformLimits {
    fn default() -> Self {
        PlatformLimits { max_media_lists: 1024, max_media_query_bytes: 1024, max_token_lists: 1 << 16, max_token_iters: 1 << 16, max_class_tokens: 4096 }
    }
}

/// What `matches` is computed from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MediaEnvironment {
    pub viewport: (f32, f32),
    /// Always `"light"` (see the header).
    pub prefers_color_scheme: &'static str,
}

/// Counters for tests and the load tool.
#[derive(Clone, Copy, Debug, Default)]
pub struct PlatformStats {
    pub media_lists: usize,
    /// Media lists with a `change` listener (roots).
    pub rooted_media_lists: usize,
    pub token_lists: usize,
    pub token_iters: usize,
}

pub(crate) struct Mql {
    pub q: Rc<str>,
    pub matches: bool,
    pub seq: u64,
    /// The listener id of `onchange`.
    pub handler: Option<u64>,
}

pub(crate) struct TokIter {
    pub list: ObjId,
    pub idx: usize,
    /// 0 values, 1 keys, 2 entries.
    pub kind: u8,
    pub done: bool,
}

pub(crate) struct PState {
    pub viewport: (f32, f32),
    pub mql: HashMap<ObjId, Mql>,
    pub seq: u64,
    /// `DOMTokenList` object -> its element.
    pub lists: HashMap<ObjId, NodeId>,
    /// element -> its `classList` object (`SameObject`).
    pub by_el: HashMap<NodeId, ObjId>,
    pub iters: HashMap<ObjId, TokIter>,
    pub halted: Option<Error>,
}

impl Default for PState {
    fn default() -> Self {
        PState { viewport: (1280.0, 800.0), mql: HashMap::new(), seq: 0, lists: HashMap::new(), by_el: HashMap::new(), iters: HashMap::new(), halted: None }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct PProtos {
    pub mql: ObjId,
    pub list: ObjId,
    pub iter: ObjId,
}

#[derive(Default)]
pub(crate) struct Platform {
    pub lim: Cell<PlatformLimits>,
    pub s: RefCell<PState>,
    pub p: OnceCell<PProtos>,
    pub fixed: RefCell<Vec<ObjId>>,
    /// `Symbol.iterator` of this realm (the one scripts see).
    pub sym_iter: OnceCell<PropertyKey>,
}

pub(crate) fn arg(a: &[Value], i: usize) -> Value {
    a.get(i).copied().unwrap_or(Value::Undefined)
}

// ------------------------------------------------------------------------------------------ GC hooks

pub(crate) fn roots(d: &Dom, out: &mut Vec<Value>) {
    out.extend(d.plat.fixed.borrow().iter().map(|&o| Value::Object(o)));
    let s = d.plat.s.borrow();
    if s.mql.is_empty() {
        return;
    }
    let st = d.st.borrow();
    out.extend(s.mql.keys().filter(|&&o| st.listeners.contains_key(&Target::Obj(o))).map(|&o| Value::Object(o)));
}

pub(crate) fn trace(d: &Dom, owner: ObjId, out: &mut Vec<Value>) {
    let s = d.plat.s.borrow();
    if let Some(&el) = s.lists.get(&owner) {
        out.extend(d.st.borrow().by_node.get(&el).map(|&o| Value::Object(o)));
    } else if let Some(i) = s.iters.get(&owner) {
        out.push(Value::Object(i.list));
    } else if !s.by_el.is_empty()
        && let Some(&Target::Node(n)) = d.st.borrow().by_obj.get(&owner)
        && let Some(&l) = s.by_el.get(&n)
    {
        out.push(Value::Object(l));
    }
}

pub(crate) fn prune(d: &Dom, live: &dyn Fn(ObjId) -> bool) {
    let mut s = d.plat.s.borrow_mut();
    let s = &mut *s;
    s.mql.retain(|&o, _| live(o));
    s.lists.retain(|&o, _| live(o));
    s.iters.retain(|&o, _| live(o));
    let lists = &s.lists;
    s.by_el.retain(|_, o| lists.contains_key(o));
}

impl Dom {
    /// Is `o` one of this module's objects (so not plain data for structured clone)?
    pub(crate) fn is_platform_object(&self, o: ObjId) -> bool {
        let s = self.plat.s.borrow();
        s.mql.contains_key(&o) || s.lists.contains_key(&o) || s.iters.contains_key(&o) || self.is_style_object(o) || self.is_attr_object(o) || self.is_nodelist_object(o) || self.is_dataset_object(o)
    }

    pub(crate) fn pl_limits(&self) -> PlatformLimits {
        self.plat.lim.get()
    }

    // ------------------------------------------------------------------------------------ install helpers

    pub(crate) fn def_method(&self, it: &mut Interp<'_>, target: ObjId, name: &str, len: u32, h: H) -> R<ObjId> {
        let key = self.key(it, name)?;
        let f = self.add_fn(it, h, name, len, false)?;
        it.define_data(target, key, Value::Object(f), true, false, true)?;
        Ok(f)
    }

    pub(crate) fn def_get(&self, it: &mut Interp<'_>, target: ObjId, name: &str, get: H, set: Option<H>) -> R<()> {
        let key = self.key(it, name)?;
        let g = self.add_fn(it, get, &format!("get {name}"), 0, false)?;
        let s = match set {
            Some(h) => Some(self.add_fn(it, h, &format!("set {name}"), 1, false)?),
            None => None,
        };
        self.define_accessor(it, target, key, g, s)
    }

    fn symbol_iterator(&self, it: &mut Interp<'_>) -> R<PropertyKey> {
        let g = *self.global.get().expect("installed");
        let (ks, ki) = (self.key(it, "Symbol")?, self.key(it, "iterator")?);
        let sym = it.get_value(Value::Object(g), ks)?;
        match it.get_value(sym, ki)? {
            Value::Symbol(s) => Ok(PropertyKey::Symbol(s)),
            _ => Err(Error::Unsupported(unsupported("Symbol.iterator is missing in this realm"))),
        }
    }

    /// `%IteratorPrototype%`, found from `[][Symbol.iterator]()` so that it is the realm's own object.
    fn iterator_prototype(&self, it: &mut Interp<'_>, sym: PropertyKey) -> R<ObjId> {
        let arr = it.array_from(&[])?;
        let ap = it.array_prototype();
        let f = it.get_value(Value::Object(ap), sym)?;
        let iter = it.call(f, arr, &[])?;
        let step = |it: &mut Interp<'_>, v: Value| -> R<ObjId> {
            let Value::Object(o) = v else { return Err(Error::Unsupported(unsupported("array iterator is not an object"))) };
            match it.heap.get_prototype_of(o) {
                Ok(Some(p)) => Ok(p),
                Ok(None) => Err(Error::Unsupported(unsupported("array iterator has no prototype"))),
                Err(e) => Err(it.heap_error(e)),
            }
        };
        let p1 = step(it, iter)?;
        step(it, Value::Object(p1))
    }

    pub(crate) fn install_platform(&self, it: &mut Interp<'_>) -> R<()> {
        let g = *self.global.get().expect("installed");
        let p = self.p();
        let op = it.object_prototype();
        let sym = self.symbol_iterator(it)?;
        let _ = self.plat.sym_iter.set(sym);
        let iter_base = self.iterator_prototype(it, sym)?;
        let (mql, list, iter) = (self.obj(it, p.et)?, self.obj(it, op)?, self.obj(it, iter_base)?);
        let _ = self.plat.p.set(PProtos { mql, list, iter });
        let mut fixed = vec![mql, list, iter];

        // MediaQueryList
        self.def_get(it, mql, "media", media_get, None)?;
        self.def_get(it, mql, "matches", matches_get, None)?;
        self.def_get(it, mql, "onchange", onchange_get, Some(onchange_set))?;
        self.def_method(it, mql, "addListener", 1, add_listener)?;
        self.def_method(it, mql, "removeListener", 1, remove_listener)?;

        // DOMTokenList and its iterator, then Element.classList
        crate::browser_platform_class::install(self, it, list, iter, sym)?;
        self.def_get(it, p.element, "classList", crate::browser_platform_class::class_list, Some(crate::browser_platform_class::set_class_list))?;
        crate::browser_platform_query::install(self, it)?;

        // interface objects and `matchMedia`
        let (kp, kc) = (self.key(it, "prototype")?, self.key(it, "constructor")?);
        for (name, proto) in [("MediaQueryList", mql), ("DOMTokenList", list)] {
            let f = self.add_fn(it, illegal_constructor, name, 0, false)?;
            it.define_data(f, kp, Value::Object(proto), false, false, false)?;
            it.define_data(proto, kc, Value::Object(f), true, false, true)?;
            let k = self.key(it, name)?;
            it.define_data(g, k, Value::Object(f), true, false, true)?;
            fixed.push(f);
        }
        let mm = self.def_method(it, g, "matchMedia", 1, match_media)?;
        // Window operations are enumerable.
        let k = self.key(it, "matchMedia")?;
        it.define_data(g, k, Value::Object(mm), true, true, true)?;
        fixed.push(mm);
        *self.plat.fixed.borrow_mut() = fixed;
        Ok(())
    }

    // ------------------------------------------------------------------------------------ media

    fn media_eval(&self, it: &mut Interp<'_>, q: &str) -> R<bool> {
        it.charge(1 + (q.len() / 4) as u64)?;
        let vp = self.plat.s.borrow().viewport;
        Ok(bare_css::at_rules::media_matches(q, vp))
    }

    fn mql_of(&self, it: &mut Interp<'_>, this: Value) -> R<ObjId> {
        match this {
            Value::Object(o) if self.plat.s.borrow().mql.contains_key(&o) => Ok(o),
            _ => Err(it.type_error("Illegal invocation")),
        }
    }

    /// The listener of `onchange`, when it is still registered.
    fn handler_cb(&self, o: ObjId) -> Option<ObjId> {
        let id = self.plat.s.borrow().mql.get(&o)?.handler?;
        let st = self.st.borrow();
        st.listeners.get(&Target::Obj(o))?.iter().find(|l| l.id == id).map(|l| l.cb)
    }
}

fn match_media(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> V {
    if a.is_empty() {
        return Err(it.type_error("Failed to execute 'matchMedia' on 'Window': 1 argument required, but only 0 present."));
    }
    let q = d.string_arg(it, a[0])?;
    let lim = d.pl_limits();
    if q.len() > lim.max_media_query_bytes {
        return Err(d.lim("media-query"));
    }
    let q: Rc<str> = Rc::from(q.trim_matches(|c: char| c.is_ascii_whitespace()));
    if d.plat.s.borrow().mql.len() >= lim.max_media_lists {
        return Err(d.lim("media-lists"));
    }
    if d.st.borrow().by_obj.len() >= d.lim.max_wrappers {
        return Err(d.lim("dom-wrappers"));
    }
    let matches = d.media_eval(it, &q)?;
    let o = d.obj(it, d.plat.p.get().expect("installed").mql)?;
    d.st.borrow_mut().by_obj.insert(o, Target::Obj(o));
    let mut s = d.plat.s.borrow_mut();
    s.seq += 1;
    let seq = s.seq;
    s.mql.insert(o, Mql { q, matches, seq, handler: None });
    Ok(Value::Object(o))
}

fn media_get(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let o = d.mql_of(it, this)?;
    let q = d.plat.s.borrow().mql[&o].q.clone();
    d.js_str(it, &q)
}

fn matches_get(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let o = d.mql_of(it, this)?;
    Ok(Value::Bool(d.plat.s.borrow().mql[&o].matches))
}

fn legacy(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value], add: bool) -> V {
    d.mql_of(it, this)?;
    let ty = d.js_str(it, "change")?;
    let cb = arg(a, 0);
    if add {
        crate::browser_events::add_event_listener(d, it, this, &[ty, cb])
    } else {
        crate::browser_events::remove_event_listener(d, it, this, &[ty, cb])
    }
}

fn add_listener(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    legacy(d, it, this, a, true)
}

fn remove_listener(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    legacy(d, it, this, a, false)
}

fn onchange_get(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let o = d.mql_of(it, this)?;
    Ok(d.handler_cb(o).map_or(Value::Null, Value::Object))
}

fn onchange_set(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let o = d.mql_of(it, this)?;
    let v = arg(a, 0);
    let cb = match v {
        Value::Object(f) if it.is_callable_value(v) => Some(f),
        _ => None,
    };
    let t = Target::Obj(o);
    let old = d.plat.s.borrow().mql[&o].handler;
    let mut st = d.st.borrow_mut();
    let present = old.and_then(|id| st.listeners.get(&t).and_then(|l| l.iter().position(|x| x.id == id)));
    match (cb, present) {
        (Some(f), Some(i)) => st.listeners.get_mut(&t).expect("listed")[i].cb = f,
        (Some(f), None) => {
            if st.n_listeners >= d.lim.max_listeners {
                return Err(d.lim("dom-listeners"));
            }
            if st.listeners.get(&t).map_or(0, Vec::len) >= d.lim.max_listeners_per_target {
                return Err(d.lim("dom-listeners-per-target"));
            }
            st.next_id += 1;
            let id = st.next_id;
            st.listeners.entry(t).or_default().push(Listener { id, ty: Rc::from("change"), cb: f, capture: false, once: false, passive: false, handler: false });
            st.n_listeners += 1;
            drop(st);
            d.plat.s.borrow_mut().mql.get_mut(&o).expect("checked").handler = Some(id);
        }
        (None, Some(_)) => {
            drop(st);
            d.remove_listener(t, old.expect("present"));
            d.plat.s.borrow_mut().mql.get_mut(&o).expect("checked").handler = None;
        }
        (None, None) => {}
    }
    Ok(Value::Undefined)
}

// ------------------------------------------------------------------------------------------ Browser API

impl Browser {
    /// Replace the bounds of this module. Call it before scripts run.
    pub fn set_platform_limits(&mut self, l: PlatformLimits) {
        self.dom.plat.lim.set(l);
    }

    /// The viewport and colour scheme that `matchMedia` uses.
    pub fn media_environment(&self) -> MediaEnvironment {
        MediaEnvironment { viewport: self.dom.plat.s.borrow().viewport, prefers_color_scheme: "light" }
    }

    pub fn platform_stats(&self) -> PlatformStats {
        let (s, st) = (self.dom.plat.s.borrow(), self.dom.st.borrow());
        PlatformStats {
            media_lists: s.mql.len(),
            rooted_media_lists: s.mql.keys().filter(|&&o| st.listeners.contains_key(&Target::Obj(o))).count(),
            token_lists: s.lists.len(),
            token_iters: s.iters.len(),
        }
    }

    /// Change the viewport (CSS pixels, finite, 0 to 1,000,000). Every live `MediaQueryList` is evaluated again, in
    /// creation order, and `change` fires at each list whose `matches` changed (see the header). Returns the number of
    /// events fired. An invalid size is `Unsupported` and changes nothing. A fatal error (Limit, Unsupported, heap fault)
    /// halts this API: later calls return it and run no JavaScript.
    pub fn set_viewport(&mut self, width: f32, height: f32) -> R<usize> {
        let ok = |v: f32| v.is_finite() && (0.0..=1_000_000.0).contains(&v);
        if !ok(width) || !ok(height) {
            return Err(Error::Unsupported(unsupported("viewport must be finite and between 0 and 1000000")));
        }
        let dom = self.dom.clone();
        if let Some(e) = dom.plat.s.borrow().halted {
            return Err(e);
        }
        let r = self.set_viewport_inner(&dom, (width, height));
        if let Err(e) = r {
            dom.plat.s.borrow_mut().halted = Some(e);
        }
        r
    }

    fn set_viewport_inner(&mut self, dom: &Rc<Dom>, vp: (f32, f32)) -> R<usize> {
        // An empty queue can still hold a sticky failed-job error.
        self.it.host_scope(|it| it.host_checkpoint().map(|_| ()))?;
        let mut order: Vec<(u64, ObjId)> = {
            let mut s = dom.plat.s.borrow_mut();
            s.viewport = vp;
            s.mql.iter().map(|(&o, m)| (m.seq, o)).collect()
        };
        order.sort_unstable_by_key(|x| x.0);
        // Evaluate and store all states first, as the standard does, then fire.
        let changed = self.it.host_scope(|it| {
            let mut changed = Vec::new();
            for &(_, o) in &order {
                let q = dom.plat.s.borrow().mql[&o].q.clone();
                let now = dom.media_eval(it, &q)?;
                let mut s = dom.plat.s.borrow_mut();
                let m = s.mql.get_mut(&o).expect("listed");
                if m.matches != now {
                    m.matches = now;
                    changed.push((o, q));
                }
            }
            Ok(changed)
        })?;
        let was = std::mem::replace(&mut dom.st.borrow_mut().driver, true);
        let mut fired = 0;
        let mut result = Ok(());
        for (o, q) in changed {
            let r = self.it.host_scope(|it| {
                let now = dom.plat.s.borrow().mql[&o].matches;
                let e = dom.new_event(it, "change", false, false, false, None)?;
                let (km, kq) = (dom.key(it, "matches")?, dom.key(it, "media")?);
                let media = dom.js_str(it, &q)?;
                it.define_data(e, km, Value::Bool(now), false, true, true)?;
                it.define_data(e, kq, media, false, true, true)?;
                dom.dispatch(it, Target::Obj(o), e).map(|_| ())
            });
            fired += 1;
            if let Err(e) = r {
                result = Err(e);
                break;
            }
        }
        dom.st.borrow_mut().driver = was;
        result.map(|_| fired)
    }
}
