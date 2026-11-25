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

use crate::eng::{Eng, Fault, Ops, Val};
use crate::browser_sources::Roots;
use crate::browser::*;
use crate::browser_scoped::*;
use bare_js_api::{Args, Engine, Local};
use bare_html::dom::NodeId;
use std::cell::{Cell, OnceCell, RefCell};
use std::rc::Rc;

pub(crate) type V<E: Eng> = R<E, E::Raw>;

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

pub(crate) struct TokIter<E: Eng> {
    pub list: E::Obj,
    pub idx: usize,
    /// 0 values, 1 keys, 2 entries.
    pub kind: u8,
    pub done: bool,
}

pub(crate) struct PState<E: Eng> {
    pub viewport: (f32, f32),
    pub mql: IdMap<E::Obj, Mql>,
    pub seq: u64,
    /// `DOMTokenList` object -> its element and backing attribute.
    pub lists: IdMap<E::Obj, (NodeId, &'static str)>,
    /// Element and attribute -> its token list (`SameObject`).
    pub by_el: IdMap<(NodeId, &'static str), E::Obj>,
    pub iters: IdMap<E::Obj, TokIter<E>>,
    /// The indexes of each token list (`browser_platform_class.rs`): the attribute text they were made from, and the strings.
    pub toks: IdMap<E::Obj, crate::browser_platform_class::Toks<E>>,
    pub halted: Option<Fault<E>>,
}

impl<E: Eng> Default for PState<E> {
    fn default() -> Self {
        PState { viewport: (1280.0, 800.0), mql: IdMap::default(), seq: 0, lists: IdMap::default(), by_el: IdMap::default(), iters: IdMap::default(), toks: IdMap::default(), halted: None }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct PProtos<E: Eng> {
    pub mql: E::Obj,
    pub list: E::Obj,
    pub iter: E::Obj,
}

#[derive(Default)]
pub(crate) struct Platform<E: Eng> {
    pub lim: Cell<PlatformLimits>,
    pub s: RefCell<PState<E>>,
    pub p: OnceCell<PProtos<E>>,
    pub fixed: RefCell<Vec<<E as Eng>::Obj>>,
    /// `Symbol.iterator` of this realm (the one scripts see).
    pub sym_iter: OnceCell<<E as Eng>::Key>,
    /// The prototype of the snapshot arrays of `children`, `childNodes` and `getElementsBy*` (`browser_dom::wrap_coll`). Also in `fixed`.
    pub coll: OnceCell<<E as Eng>::Obj>,
}

pub(crate) fn arg<E: Eng>(a: &[E::Raw], i: usize) -> E::Raw {
    a.get(i).copied().unwrap_or(E::undef())
}

// ------------------------------------------------------------------------------------------ GC hooks

pub(crate) fn roots<E: Eng>(d: &Dom<E>, out: &mut Roots<'_, '_, E>) {
    out.extend(d.plat.fixed.borrow().iter().map(|&o| E::obj_raw(o)));
    let s = d.plat.s.borrow();
    if s.mql.is_empty() {
        return;
    }
    let st = d.st.borrow();
    out.extend(s.mql.keys().filter(|&&o| st.listeners.get(&Target::Obj(o)).is_some_and(|l| !l.is_empty())).map(|&o| E::obj_raw(o)));
}

/// The references of the iterator `o` of a token list: the list. The token lists have owner entries (`trace_list`).
pub(crate) fn trace_iter<E: Eng>(d: &Dom<E>, o: E::Obj, t: &mut bare_js_api::Tracer<'_, E::Raw>) {
    if let Some(i) = d.plat.s.borrow().iters.get(&o) {
        crate::browser_sources::visit::<E>(t, E::obj_raw(i.list));
    }
}

/// The references of the token list `o` (its owner entry, `browser_sources.rs`): the wrapper of its element and its strings.
pub(crate) fn trace_list<E: Eng>(d: &Dom<E>, o: E::Obj, t: &mut bare_js_api::Tracer<'_, E::Raw>) {
    let s = d.plat.s.borrow();
    if let Some(&(el, _)) = s.lists.get(&o) {
        if let Some(w) = d.st.borrow().by_node.get(&el) {
            t.visit(w);
        }
        for &v in s.toks.get(&o).into_iter().flat_map(|t| &t.vals) {
            crate::browser_sources::visit::<E>(t, E::str_raw(v));
        }
    }
}

/// The references of the wrapper of the element `n`: its token lists.
pub(crate) fn trace_node<E: Eng>(d: &Dom<E>, n: NodeId, t: &mut bare_js_api::Tracer<'_, E::Raw>) {
    let s = d.plat.s.borrow();
    if s.by_el.is_empty() {
        return;
    }
    for attr in ["class", "rel"] {
        if let Some(&l) = s.by_el.get(&(n, attr)) {
            crate::browser_sources::visit::<E>(t, E::obj_raw(l));
        }
    }
}

/// Drop the entries of the token list `o`, which the collector freed.
pub(crate) fn prune_list<E: Eng>(d: &Dom<E>, o: E::Obj) {
    let mut s = d.plat.s.borrow_mut();
    s.toks.remove(&o);
    if let Some(key) = s.lists.remove(&o)
        && s.by_el.get(&key) == Some(&o)
    {
        s.by_el.remove(&key);
    }
}

/// The media query list `o` is dead: drop its entry.
pub(crate) fn forget_mql<E: Eng>(d: &Dom<E>, o: E::Obj) {
    d.plat.s.borrow_mut().mql.remove(&o);
}

/// The token list iterator `o` is dead: drop its entry.
pub(crate) fn forget_iter<E: Eng>(d: &Dom<E>, o: E::Obj) {
    d.plat.s.borrow_mut().iters.remove(&o);
}

impl<E: Eng> Dom<E> {
    /// `Browser::survive`: the viewport callbacks stopped on a script budget; let them run again.
    pub(crate) fn clear_budget_halt_plat(&self) {
        let mut s = self.plat.s.borrow_mut();
        if s.halted.as_ref().is_some_and(crate::browser::is_budget::<E>) {
            s.halted = None;
        }
    }

    /// Is `o` one of this module's objects (so not plain data for structured clone)?
    pub(crate) fn is_platform_object(&self, o: E::Obj) -> bool {
        let s = self.plat.s.borrow();
        s.mql.contains_key(&o) || s.lists.contains_key(&o) || s.iters.contains_key(&o) || self.is_style_object(o) || self.is_attr_object(o) || self.is_nodelist_object(o) || self.is_dataset_object(o) || self.is_geom_object(o)
    }

    pub(crate) fn pl_limits(&self) -> PlatformLimits {
        self.plat.lim.get()
    }

    // ------------------------------------------------------------------------------------ install helpers

    /// `%IteratorPrototype%`, found from `[][Symbol.iterator]()` so that it is the realm's own object.
    fn iterator_prototype_s<'s>(&self, s: &mut Sc<'s, E>, sym: Local<'s>) -> Res<'s, E::Obj> {
        let arr = s.new_array(&[])?;
        let ap = s.intrinsic(Intrinsic::ArrayPrototype);
        let f = s.get(ap, sym)?;
        let iter = s.call(f, arr, &[])?;
        let Some(p1) = s.get_prototype_of(iter)? else { return Err(unsupp("array iterator has no prototype")) };
        let Some(p2) = s.get_prototype_of(p1)? else { return Err(unsupp("array iterator has no prototype")) };
        Ok(obj_id::<E>(s, p2).expect("an object"))
    }

    pub(crate) fn install_platform<'s>(&self, s: &mut Sc<'s, E>) -> Res<'s, ()> {
        let p = self.p();
        let sym = s.intrinsic(Intrinsic::SymbolIterator);
        let Val::Symbol(y) = E::kind(s.engine().raw(sym)) else { unreachable!("Symbol.iterator is a symbol") };
        let _ = self.plat.sym_iter.set(E::sym_key(y));
        let iter_base = self.iterator_prototype_s(s, sym)?;
        let (mql, list, iter) = (self.obj_s(s, p.et)?, self.plain_s(s)?, self.obj_s(s, iter_base)?);
        let _ = self.plat.p.set(PProtos { mql, list, iter });
        let mut fixed = vec![mql, list, iter];

        // MediaQueryList
        table!(rows1 = [
            Row::get(1, "media", media_get, None),
            Row::get(1, "matches", matches_get, None),
            Row::get(1, "onchange", onchange_get, Some(onchange_set)),
            Row::method(1, "addListener", 1, add_listener),
            Row::method(1, "removeListener", 1, remove_listener),
        ]);
        let targets = [self.obj_local(s, mql)];
        self.install_rows(s, &targets, &rows1::T::<E>::V)?;

        // DOMTokenList and its iterator, then Element.classList
        crate::browser_platform_class::install(self, s, list, iter, sym)?;
        table!(rows2 = [
            Row::get(1, "classList", crate::browser_platform_class::class_list, Some(crate::browser_platform_class::set_class_list)),
            Row::get(2, "relList", crate::browser_platform_class::rel_list::<0, _>, Some(crate::browser_platform_class::set_rel_list::<0, _>)),
            Row::get(4, "sandbox", crate::browser_platform_class::sandbox_list, Some(crate::browser_platform_class::set_sandbox_list)),
        ]);
        let targets = [self.obj_local(s, p.element), self.obj_local(s, p.link), self.obj_local(s, p.iframe)];
        self.install_rows(s, &targets, &rows2::T::<E>::V)?;
        crate::browser_platform_query::install(self, s)?;

        // interface objects and `matchMedia`
        for (name, proto) in [("MediaQueryList", mql), ("DOMTokenList", list)] {
            fixed.push(self.interface_s(s, proto, name, illegal_constructor, 0, HostFn::Method)?);
        }
        let g = s.global();
        // Window operations are enumerable.
        let mm = self.def_method_l(s, g, "matchMedia", 1, match_media, true)?;
        fixed.push(obj_id::<E>(s, mm).expect("a function"));
        *self.plat.fixed.borrow_mut() = fixed;
        Ok(())
    }

    // ------------------------------------------------------------------------------------ media

    fn media_eval(&self, it: &mut E::Vm, q: &str) -> R<E, bool> {
        it.charge_o(1 + (q.len() / 4) as u64)?;
        let vp = self.plat.s.borrow().viewport;
        Ok(bare_css::at_rules::media_matches(q, vp))
    }

    fn mql_of<'s>(&self, s: &mut Sc<'s, E>, this: Local<'s>) -> Res<'s, E::Obj> {
        match obj_id::<E>(s, this) {
            Some(o) if self.plat.s.borrow().mql.contains_key(&o) => Ok(o),
            _ => Err(s.type_error("Illegal invocation")),
        }
    }

    /// The listener of `onchange`, when it is still registered.
    fn handler_cb(&self, it: &mut E::Vm, o: E::Obj) -> Option<E::Raw> {
        let id = self.plat.s.borrow().mql.get(&o)?.handler?;
        let st = self.st.borrow();
        st.listeners.get(&Target::Obj(o))?.iter().find(|l| l.id == id).map(|l| it.traced_value(&l.cb))
    }
}

fn match_media<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    if a.is_empty() {
        return Err(s.type_error("Failed to execute 'matchMedia' on 'Window': 1 argument required, but only 0 present."));
    }
    let q = d.arg_str_s(s, a, 0)?;
    let lim = d.pl_limits();
    if q.len() > lim.max_media_query_bytes {
        return Err(d.lim_s(s, "media-query"));
    }
    let q: Rc<str> = Rc::from(q.trim_matches(|c: char| c.is_ascii_whitespace()));
    if d.plat.s.borrow().mql.len() >= lim.max_media_lists {
        return Err(d.lim_s(s, "media-lists"));
    }
    if d.st.borrow().by_obj.len() >= d.lim.max_wrappers {
        return Err(d.lim_s(s, "dom-wrappers"));
    }
    let matches = old::<E, _>(s, |it| d.media_eval(it, &q))?;
    let o = d.obj_s(s, d.plat.p.get().expect("installed").mql)?;
    d.st.borrow_mut().add_target(s.engine(), o, crate::browser::Owner::Mql(o));
    let mut m = d.plat.s.borrow_mut();
    m.seq += 1;
    let seq = m.seq;
    m.mql.insert(o, Mql { q, matches, seq, handler: None });
    drop(m);
    Ok(d.obj_local(s, o))
}

fn media_get<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let o = d.mql_of(s, this)?;
    let q = d.plat.s.borrow().mql[&o].q.clone();
    d.js_str_s(s, &q)
}

fn matches_get<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let o = d.mql_of(s, this)?;
    let m = d.plat.s.borrow().mql[&o].matches;
    Ok(s.boolean(m))
}

fn legacy<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>, add: bool) -> Res<'s, Local<'s>> {
    d.mql_of(s, this)?;
    // The two arguments of the listener call, in a window of their own.
    let (ty, cb) = (d.js_str_s(s, "change")?, arg_s::<E>(s, a, 0));
    let e = s.engine();
    let (ty, cb) = (e.raw(ty), e.raw(cb));
    let base = e.stack_len();
    e.raw_local(&ty);
    e.raw_local(&cb);
    let args = Args::from_slots(base, 2);
    if add {
        crate::browser_events::add_event_listener(d, s, this, args)
    } else {
        crate::browser_events::remove_event_listener(d, s, this, args)
    }
}

fn add_listener<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    legacy(d, s, this, a, true)
}

fn remove_listener<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    legacy(d, s, this, a, false)
}

fn onchange_get<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let o = d.mql_of(s, this)?;
    Ok(match d.handler_cb(s.engine(), o) {
        Some(v) => s.engine().raw_local(&v),
        None => s.null(),
    })
}

fn onchange_set<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let o = d.mql_of(s, this)?;
    let v = arg_s::<E>(s, a, 0);
    let cb = if s.is_callable(v) { obj_id::<E>(s, v) } else { None };
    old::<E, _>(s, |it| {
        let t = Target::Obj(o);
        let old = d.plat.s.borrow().mql[&o].handler;
        let mut guard = d.st.borrow_mut();
        let st = &mut *guard;
        let present = old.and_then(|id| st.listeners.get(&t).and_then(|l| l.iter().position(|x| x.id == id)));
        match (cb, present) {
            (Some(f), Some(i)) => {
                it.store_value(&mut st.listeners.get_mut(&t).expect("listed")[i].cb, E::obj_raw(f));
                d.own_listeners(st, it, t);
            }
            (Some(f), None) => {
                if st.n_listeners >= d.lim.max_listeners {
                    return Err(d.lim("dom-listeners"));
                }
                st.next_id += 1;
                let id = st.next_id;
                let cb = it.trace_value(E::obj_raw(f));
                st.listeners.entry(t).or_default().push(Listener { id, ty: Rc::from("change"), cb, capture: false, once: false, passive: false, handler: false });
                st.n_listeners += 1;
                d.own_listeners(st, it, t);
                d.plat.s.borrow_mut().mql.get_mut(&o).expect("checked").handler = Some(id);
            }
            (None, Some(_)) => {
                drop(guard);
                d.remove_listener(t, old.expect("present"));
                d.plat.s.borrow_mut().mql.get_mut(&o).expect("checked").handler = None;
            }
            (None, None) => {}
        }
        Ok(())
    })?;
    Ok(s.undefined())
}

// ------------------------------------------------------------------------------------------ Browser API

impl<E: Eng> Browser<E> {
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
            rooted_media_lists: s.mql.keys().filter(|&&o| st.listeners.get(&Target::Obj(o)).is_some_and(|l| !l.is_empty())).count(),
            token_lists: s.lists.len(),
            token_iters: s.iters.len(),
        }
    }

    /// Change the viewport (CSS pixels, finite, 0 to 1,000,000). Every live `MediaQueryList` is evaluated again, in
    /// creation order, and `change` fires at each list whose `matches` changed (see the header). Returns the number of
    /// events fired. An invalid size is `Unsupported` and changes nothing. A fatal error (Limit, Unsupported, heap fault)
    /// halts this API: later calls return it and run no JavaScript.
    pub fn set_viewport(&mut self, width: f32, height: f32) -> R<E, usize> {
        let ok = |v: f32| v.is_finite() && (0.0..=1_000_000.0).contains(&v);
        if !ok(width) || !ok(height) {
            return Err(Fault::Unsupported("viewport must be finite and between 0 and 1000000"));
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

    fn set_viewport_inner(&mut self, dom: &Rc<Dom<E>>, vp: (f32, f32)) -> R<E, usize> {
        // An empty queue can still hold a sticky failed-job error.
        self.it.host_scope_o(|it| it.host_checkpoint().map(|_| ()))?;
        let mut order: Vec<(u64, E::Obj)> = {
            let mut s = dom.plat.s.borrow_mut();
            s.viewport = vp;
            s.mql.iter().map(|(&o, m)| (m.seq, o)).collect()
        };
        order.sort_unstable_by_key(|x| x.0);
        // Evaluate and store all states first, as the standard does, then fire.
        let changed = self.it.host_scope_o(|it| {
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
            let r = self.it.host_scope_o(|it| {
                let now = dom.plat.s.borrow().mql[&o].matches;
                let e = dom.new_event(it, "change", false, false, false, None)?;
                let (km, kq) = (dom.key(it, "matches")?, dom.key(it, "media")?);
                let media = dom.js_str(it, &q)?;
                it.define_data(e, km, E::bool_raw(now), false, true, true)?;
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
