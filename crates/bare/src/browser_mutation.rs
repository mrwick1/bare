//! `MutationObserver` and `MutationRecord` (DOM Standard, "Mutation observers"). Read by hand, then compared with headless Chrome on 52 cases
//! (`tools/fixtures/ui/mutation-cases.txt`, `tools/ui/chrome_topic.mjs`).
//!
//! How it works
//! - The native document reports each change to one watcher (`bare_html::dom::Document::set_watcher`): a node linked or unlinked, an attribute
//!   set or removed, the text of a node replaced. The watcher is set when the first `observe` call registers a node and is removed when the last
//!   registration goes. A page that never calls `observe` pays one `Option` check at a change and one `Cell` read at a host call. Nothing else.
//! - Registrations are in a table by node (`regs`), as in the standard ("registered observer list"). A change walks the inclusive ancestors of its
//!   target, so a change costs the depth of the node (and nothing when no registration exists). An observer gets one record for a change, also
//!   when it is registered on two ancestors (the old value is kept when any registration asks for it).
//! - A record is queued in the observer at the moment of the change. The delivery is one microtask for the realm ("queue a mutation observer
//!   microtask"). The microtask is queued after the host call that changed the node returns (`Dom::mo_after`, called by `Host::call`), because the
//!   watcher cannot reach the interpreter. A change made by the host itself (a loader, not script) is therefore delivered after the next host call.
//! - Delivery ("notify mutation observers"): the observers in the order of creation, each with its records, as `callback(records, observer)` with
//!   `this` the observer. A throw is reported (`Browser::take_reports`, type `mutationobserver`) and the next observer runs. A change in a callback
//!   queues the next delivery.
//! - Transient registered observers: when a node is removed, the observers with `subtree` on its ancestors also watch the removed node until their
//!   next delivery, as the standard says.
//! - Groups (`Dom::mo_group`): `textContent`, `innerHTML` (replace all), `replaceChild` and a many-node `append` or `prepend` make one record, not one
//!   for each link and unlink. Inside a group the records of one target merge, and a replace-all keeps only the changes of its own children.
//!
//! Deviations (recorded, none is silent)
//! - `MutationRecord` fields are own read-only data properties, not prototype getters (like `ResizeObserverEntry`): `Object.keys(record)` lists them.
//!   `addedNodes` and `removedNodes` are plain arrays (snapshots), not `NodeList`.
//! - An observer that has a registration or a queued record is a GC root until `disconnect` (the standard holds the observer weakly from the node).
//! - `replaceChild(new, old)` where `new` has another parent queues the removal record of that parent after the record of `old`, not before. Only a
//!   page that observes both parents can see it.
//! - A text change inside a node that the standard does not model as a change (`insertData`, `Range` edits) is not in the host yet.
//!
//! Bounds (`Limit(Host(name))`): `mutation-observers`, `mutation-registrations`, `mutation-records` (queued records of all observers).

use crate::browser::*;
use bare_html::dom::{Atom, Change, Document, NodeId};
use bare_js::eval::{Error, Interp};
use bare_js::value::{ObjId, Value};
use std::cell::{Cell, OnceCell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

type V = R<Value>;

const MAX_OBSERVERS: usize = 1024;
const MAX_REGS: usize = 1 << 14;
const MAX_RECORDS: usize = 1 << 16;

struct Opts {
    child: bool,
    attrs: bool,
    chars: bool,
    subtree: bool,
    attr_old: bool,
    char_old: bool,
    filter: Option<Vec<Box<str>>>,
}

struct Reg {
    obs: ObjId,
    o: Rc<Opts>,
    /// `Some(node)`: a transient registration made because of the registration on `node`.
    source: Option<NodeId>,
}

#[derive(Clone, Copy, PartialEq)]
enum Ty {
    Child,
    Attrs,
    Chars,
}

struct Rec {
    ty: Ty,
    target: NodeId,
    added: Vec<NodeId>,
    removed: Vec<NodeId>,
    prev: Option<NodeId>,
    next: Option<NodeId>,
    name: Option<(Box<str>, Option<&'static str>)>,
    old: Option<Box<str>>,
    group: u32,
}

struct Obs {
    cb: Value,
    recs: Vec<Rec>,
    /// Registrations, transient ones too.
    regs: usize,
}

#[derive(Clone, Copy)]
struct Grp {
    id: u32,
    /// Replace all: only the changes of the children of this node count, and the siblings of the record are null.
    all: Option<NodeId>,
}

#[derive(Default)]
struct St {
    observer: Option<ObjId>,
    record: Option<ObjId>,
    ctors: Option<(ObjId, ObjId)>,
    /// The document has our watcher (a registration exists).
    watching: bool,
    notify: Option<ObjId>,
    obs: HashMap<ObjId, Obs>,
    order: Vec<ObjId>,
    regs: HashMap<NodeId, Vec<Reg>>,
    n_regs: usize,
    n_recs: usize,
    transient: usize,
    grp: Option<Grp>,
    next_grp: u32,
    /// A record was queued and no delivery microtask is queued.
    want: bool,
    queued: bool,
    overflow: bool,
}

#[derive(Default)]
pub(crate) struct Mo {
    /// True from the first `observe`: `Host::call` then looks at `St::want` after each call.
    armed: Cell<bool>,
    st: OnceCell<Rc<RefCell<St>>>,
}

impl Mo {
    #[inline]
    pub(crate) fn armed(&self) -> bool {
        self.armed.get()
    }
}

impl St {
    /// The observers that want a change of kind `ty` at `target` (and whether any registration of theirs asks for the old value).
    fn interested(&self, doc: &Document, target: NodeId, ty: Ty, attr: Option<Atom>) -> Vec<(ObjId, bool)> {
        let mut out: Vec<(ObjId, bool)> = Vec::new();
        let mut cur = Some(target);
        while let Some(n) = cur {
            for r in self.regs.get(&n).map_or(&[][..], |v| v) {
                let o = &*r.o;
                if (n != target && !o.subtree) || !(match ty { Ty::Child => o.child, Ty::Attrs => o.attrs, Ty::Chars => o.chars }) {
                    continue;
                }
                if let (Ty::Attrs, Some(a), Some(f)) = (ty, attr, &o.filter)
                    && (doc.attr_ns(a).is_some() || !f.iter().any(|x| &**x == doc.attr_local(a)))
                {
                    continue;
                }
                let old = match ty {
                    Ty::Attrs => o.attr_old,
                    Ty::Chars => o.char_old,
                    Ty::Child => false,
                };
                match out.iter_mut().find(|x| x.0 == r.obs) {
                    Some(x) => x.1 |= old,
                    None => out.push((r.obs, old)),
                }
            }
            cur = doc.parent(n);
        }
        out
    }

    fn push(&mut self, o: ObjId, mut rec: Rec) {
        if self.n_recs >= MAX_RECORDS {
            self.overflow = true;
            return;
        }
        let grp = self.grp;
        if let Some(g) = grp
            && rec.ty == Ty::Child
        {
            rec.group = g.id;
            if let Some(ob) = self.obs.get_mut(&o)
                && let Some(last) = ob.recs.iter_mut().rev().take_while(|r| r.group == g.id).find(|r| r.ty == Ty::Child && r.target == rec.target)
            {
                last.added.append(&mut rec.added);
                last.removed.append(&mut rec.removed);
                if g.all.is_none() {
                    last.next = rec.next;
                }
                return;
            }
            if g.all.is_some() {
                (rec.prev, rec.next) = (None, None);
            }
        }
        if let Some(ob) = self.obs.get_mut(&o) {
            ob.recs.push(rec);
            self.n_recs += 1;
            self.want = true;
        }
    }

    fn child(&mut self, doc: &Document, parent: NodeId, node: NodeId, link: bool, prev: Option<NodeId>, next: Option<NodeId>) {
        if let Some(g) = self.grp
            && g.all.is_some_and(|r| r != parent)
        {
            return;
        }
        for (o, _) in self.interested(doc, parent, Ty::Child, None) {
            let (added, removed) = if link { (vec![node], vec![]) } else { (vec![], vec![node]) };
            self.push(o, Rec { ty: Ty::Child, target: parent, added, removed, prev, next, name: None, old: None, group: 0 });
        }
    }

    /// Transient registered observers (standard "remove", step 15): the observers with `subtree` on the ancestors of `parent` watch `node` too.
    fn make_transient(&mut self, doc: &Document, parent: NodeId, node: NodeId) {
        let mut add = Vec::new();
        let mut cur = Some(parent);
        while let Some(n) = cur {
            for r in self.regs.get(&n).map_or(&[][..], |v| v) {
                if r.o.subtree {
                    add.push(Reg { obs: r.obs, o: r.o.clone(), source: Some(r.source.unwrap_or(n)) });
                }
            }
            cur = doc.parent(n);
        }
        if add.is_empty() || self.n_regs + add.len() > MAX_REGS {
            return;
        }
        for r in &add {
            if let Some(ob) = self.obs.get_mut(&r.obs) {
                ob.regs += 1;
            }
        }
        self.n_regs += add.len();
        self.transient += add.len();
        // As in Chrome, a transient registration queues a delivery too: it is the delivery that ends it.
        self.want = true;
        self.regs.entry(node).or_default().extend(add);
    }

    fn on_change(&mut self, doc: &Document, ch: Change) {
        if self.regs.is_empty() {
            return;
        }
        match ch {
            Change::Link { parent, node, prev, next } => self.child(doc, parent, node, true, prev, next),
            Change::Unlink { parent, node, prev, next } => {
                self.child(doc, parent, node, false, prev, next);
                if self.grp.is_none_or(|g| g.all.is_none_or(|r| r == parent)) {
                    self.make_transient(doc, parent, node);
                }
            }
            Change::Attr { el, name, old } => {
                for (o, want_old) in self.interested(doc, el, Ty::Attrs, Some(name)) {
                    let ns = doc.attr_ns(name).map(|n| n.uri());
                    let rec = Rec {
                        ty: Ty::Attrs,
                        target: el,
                        added: vec![],
                        removed: vec![],
                        prev: None,
                        next: None,
                        name: Some((doc.attr_local(name).into(), ns)),
                        old: old.filter(|_| want_old).map(|s| doc.text(s).into()),
                        group: 0,
                    };
                    self.push(o, rec);
                }
            }
            Change::Text { node, old } => {
                for (o, want_old) in self.interested(doc, node, Ty::Chars, None) {
                    let rec = Rec { ty: Ty::Chars, target: node, added: vec![], removed: vec![], prev: None, next: None, name: None, old: want_old.then(|| doc.text(old).into()), group: 0 };
                    self.push(o, rec);
                }
            }
        }
    }

    /// Remove every registration of `o` (`transient_only`: only the transient ones).
    fn drop_regs(&mut self, o: ObjId, transient_only: bool) {
        let mut gone = 0;
        self.regs.retain(|_, v| {
            let before = v.len();
            v.retain(|r| r.obs != o || (transient_only && r.source.is_none()));
            gone += before - v.len();
            !v.is_empty()
        });
        if let Some(ob) = self.obs.get_mut(&o) {
            ob.regs -= gone;
        }
        self.n_regs -= gone;
        if transient_only {
            self.transient -= gone;
        } else {
            self.transient = self.regs.values().flatten().filter(|r| r.source.is_some()).count();
        }
    }
}

// ------------------------------------------------------------------------------------ interface

fn state(d: &Dom) -> &Rc<RefCell<St>> {
    d.mo.st.get_or_init(Default::default)
}

fn observer_of(d: &Dom, it: &mut Interp<'_>, this: Value) -> R<ObjId> {
    match this {
        Value::Object(o) if state(d).borrow().obs.contains_key(&o) => Ok(o),
        _ => Err(it.type_error("Illegal invocation")),
    }
}

fn illegal(_: &Dom, it: &mut Interp<'_>, _: Value, _: &[Value]) -> V {
    Err(it.type_error("Failed to construct 'MutationRecord': Illegal constructor"))
}

fn construct(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> V {
    let Some(&cb) = a.first() else {
        return Err(it.type_error("Failed to construct 'MutationObserver': 1 argument required, but only 0 present."));
    };
    if !it.is_callable_value(cb) {
        return Err(it.type_error("Failed to construct 'MutationObserver': parameter 1 is not of type 'Function'."));
    }
    let st = state(d);
    if st.borrow().obs.len() >= MAX_OBSERVERS {
        return Err(d.lim("mutation-observers"));
    }
    let proto = st.borrow().observer.expect("made");
    let o = d.obj(it, proto)?;
    let mut s = st.borrow_mut();
    s.obs.insert(o, Obs { cb, recs: Vec::new(), regs: 0 });
    s.order.push(o);
    Ok(Value::Object(o))
}

const NEED: &str = "Failed to execute 'observe' on 'MutationObserver': The options object must set at least one of 'attributes', 'characterData', or 'childList' to true.";

fn parse_opts(d: &Dom, it: &mut Interp<'_>, v: Value) -> R<Opts> {
    // WebIDL dictionary members are read in alphabetical order.
    let get = |it: &mut Interp<'_>, name: &str| -> R<Option<Value>> {
        if !matches!(v, Value::Object(_)) {
            return Ok(None);
        }
        let k = d.key(it, name)?;
        Ok(Some(it.get_value(v, k)?).filter(|x| !matches!(x, Value::Undefined)))
    };
    let filter = get(it, "attributeFilter")?;
    let attr_old = get(it, "attributeOldValue")?;
    let attrs = get(it, "attributes")?;
    let chars = get(it, "characterData")?;
    let char_old = get(it, "characterDataOldValue")?;
    let child = get(it, "childList")?;
    let subtree = get(it, "subtree")?;
    let b = |it: &mut Interp<'_>, x: Option<Value>| -> R<Option<bool>> { x.map(|x| it.to_js_bool(x)).transpose() };
    let (attr_old, attrs, chars, char_old, child, subtree) = (b(it, attr_old)?, b(it, attrs)?, b(it, chars)?, b(it, char_old)?, b(it, child)?, b(it, subtree)?);
    let filter = match filter {
        None => None,
        Some(f) => {
            let kl = d.key(it, "length")?;
            let len = it.get_value(f, kl)?;
            let n = it.to_js_number(len)?;
            let n = if n.is_finite() && n > 0.0 { n as usize } else { 0 };
            if n > 1 << 12 {
                return Err(d.lim("mutation-filter"));
            }
            let mut names = Vec::with_capacity(n);
            for i in 0..n {
                let k = d.key(it, &i.to_string())?;
                let x = it.get_value(f, k)?;
                names.push(d.string_arg(it, x)?.into_boxed_str());
            }
            Some(names)
        }
    };
    let attrs = attrs.or((attr_old.is_some() || filter.is_some()).then_some(true));
    let chars = chars.or(char_old.is_some().then_some(true));
    let o = Opts { child: child.unwrap_or(false), attrs: attrs.unwrap_or(false), chars: chars.unwrap_or(false), subtree: subtree.unwrap_or(false), attr_old: attr_old.unwrap_or(false), char_old: char_old.unwrap_or(false), filter };
    let m = |what: &str| format!("Failed to execute 'observe' on 'MutationObserver': The options object may only set '{what}' when '{}' is true or not present.", if what.starts_with("attr") { "attributes" } else { "characterData" });
    if o.attr_old && !o.attrs {
        return Err(it.type_error(&m("attributeOldValue").replace("'attributeOldValue'", "'attributeOldValue' to true")));
    }
    if o.filter.is_some() && !o.attrs {
        return Err(it.type_error(&m("attributeFilter")));
    }
    if o.char_old && !o.chars {
        return Err(it.type_error(&m("characterDataOldValue").replace("'characterDataOldValue'", "'characterDataOldValue' to true")));
    }
    // Chrome checks the old-value and filter members before the "at least one" rule (the standard has them the other way round).
    if !(o.child || o.attrs || o.chars) {
        return Err(it.type_error(NEED));
    }
    Ok(o)
}

fn observe(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let o = observer_of(d, it, this)?;
    if a.is_empty() {
        return Err(it.type_error("Failed to execute 'observe' on 'MutationObserver': 1 argument required, but only 0 present."));
    }
    let Some(target) = d.node_of(a[0]) else {
        return Err(it.type_error("Failed to execute 'observe' on 'MutationObserver': parameter 1 is not of type 'Node'."));
    };
    let opts = Rc::new(parse_opts(d, it, a.get(1).copied().unwrap_or(Value::Undefined))?);
    let st = state(d);
    let watch = {
        let mut s = st.borrow_mut();
        let s = &mut *s;
        if let Some(r) = s.regs.get_mut(&target).and_then(|v| v.iter_mut().find(|r| r.obs == o && r.source.is_none())) {
            // The standard replaces the options and drops the transient registrations that came from this one.
            r.o = opts;
            let mut gone = 0;
            for v in s.regs.values_mut() {
                let n = v.len();
                v.retain(|r| !(r.obs == o && r.source == Some(target)));
                gone += n - v.len();
            }
            s.n_regs -= gone;
            s.transient -= gone;
            s.obs.get_mut(&o).expect("checked").regs -= gone;
        } else {
            if s.n_regs >= MAX_REGS {
                return Err(d.lim("mutation-registrations"));
            }
            s.regs.entry(target).or_default().push(Reg { obs: o, o: opts, source: None });
            s.n_regs += 1;
            s.obs.get_mut(&o).expect("checked").regs += 1;
        }
        !std::mem::replace(&mut s.watching, true)
    };
    d.mo.armed.set(true);
    d.ensure_notify(it)?;
    if watch {
        let sh = st.clone();
        d.doc.borrow_mut().set_watcher(Some(Box::new(move |doc, ch| sh.borrow_mut().on_change(doc, ch))));
    }
    Ok(Value::Undefined)
}

fn disconnect(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let o = observer_of(d, it, this)?;
    let mut s = state(d).borrow_mut();
    s.drop_regs(o, false);
    let ob = s.obs.get_mut(&o).expect("checked");
    let n = std::mem::take(&mut ob.recs).len();
    s.n_recs -= n;
    drop(s);
    d.mo_unwatch_if_idle();
    Ok(Value::Undefined)
}

fn take_records(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let o = observer_of(d, it, this)?;
    let recs = {
        let mut s = state(d).borrow_mut();
        let r = std::mem::take(&mut s.obs.get_mut(&o).expect("checked").recs);
        s.n_recs -= r.len();
        r
    };
    d.record_array(it, recs)
}

impl Dom {
    fn ensure_notify(&self, it: &mut Interp<'_>) -> R<()> {
        if state(self).borrow().notify.is_none() {
            let f = self.add_fn(it, notify, "notify", 0, false)?;
            state(self).borrow_mut().notify = Some(f);
        }
        Ok(())
    }

    fn record_array(&self, it: &mut Interp<'_>, recs: Vec<Rec>) -> V {
        let proto = state(self).borrow().record.expect("made");
        let mut items = Vec::with_capacity(recs.len());
        for r in recs {
            it.charge(1)?;
            let o = self.obj(it, proto)?;
            let ty = self.js_str(it, match r.ty { Ty::Child => "childList", Ty::Attrs => "attributes", Ty::Chars => "characterData" })?;
            let target = self.wrap(it, r.target)?;
            let list = |it: &mut Interp<'_>, ids: &[NodeId]| -> V {
                let mut v = Vec::with_capacity(ids.len());
                for &n in ids {
                    v.push(self.wrap(it, n)?);
                }
                it.array_from(&v)
            };
            let (added, removed) = (list(it, &r.added)?, list(it, &r.removed)?);
            let (prev, next) = (self.wrap_opt(it, r.prev)?, self.wrap_opt(it, r.next)?);
            let (name, ns) = match &r.name {
                Some((n, ns)) => (self.js_str(it, n)?, match ns { Some(u) => self.js_str(it, u)?, None => Value::Null }),
                None => (Value::Null, Value::Null),
            };
            let old = match &r.old {
                Some(s) => self.js_str(it, s)?,
                None => Value::Null,
            };
            for (k, v) in [("type", ty), ("target", target), ("addedNodes", added), ("removedNodes", removed), ("previousSibling", prev), ("nextSibling", next), ("attributeName", name), ("attributeNamespace", ns), ("oldValue", old)] {
                let k = self.key(it, k)?;
                it.define_data(o, k, v, false, true, true)?;
            }
            items.push(Value::Object(o));
        }
        it.array_from(&items)
    }

    /// Run `f` as one group of changes (see the header). A page with no observer pays one `Cell` read. `all`: a replace-all of the children of that node.
    pub(crate) fn mo_group<T>(&self, all: Option<NodeId>, f: impl FnOnce() -> R<T>) -> R<T> {
        if !self.mo.armed.get() {
            return f();
        }
        let mut s = state(self).borrow_mut();
        s.next_grp = s.next_grp.wrapping_add(1).max(1);
        let id = s.next_grp;
        let prev = s.grp.replace(Grp { id, all });
        drop(s);
        let r = f();
        state(self).borrow_mut().grp = prev;
        r
    }

    /// No registration is left: the document needs no watcher.
    fn mo_unwatch_if_idle(&self) {
        let mut s = state(self).borrow_mut();
        if s.watching && s.n_regs == 0 {
            s.watching = false;
            self.doc.borrow_mut().set_watcher(None);
        }
    }

    /// Called by `Host::call` after each host call once an observer exists: queue the delivery microtask when a record is waiting.
    pub(crate) fn mo_after(&self, it: &mut Interp<'_>) -> R<()> {
        let mut s = state(self).borrow_mut();
        if s.overflow {
            return Err(self.lim("mutation-records"));
        }
        if s.want && !s.queued {
            s.want = false;
            s.queued = true;
            let f = s.notify.expect("made by observe");
            drop(s);
            it.queue_microtask(Value::Object(f))?;
        }
        Ok(())
    }

    pub(crate) fn mo_roots(&self, out: &mut Vec<Value>) {
        let Some(st) = self.mo.st.get() else { return };
        let s = st.borrow();
        out.extend([s.observer, s.record, s.notify, s.ctors.map(|c| c.0), s.ctors.map(|c| c.1)].into_iter().flatten().map(Value::Object));
        out.extend(s.obs.iter().filter(|(_, o)| o.regs > 0 || !o.recs.is_empty()).map(|(&o, _)| Value::Object(o)));
    }

    pub(crate) fn mo_trace(&self, owner: ObjId, out: &mut Vec<Value>) {
        if let Some(st) = self.mo.st.get()
            && let Some(o) = st.borrow().obs.get(&owner)
        {
            out.push(o.cb);
        }
    }

    pub(crate) fn mo_prune(&self, live: &dyn Fn(ObjId) -> bool) {
        if let Some(st) = self.mo.st.get() {
            let mut s = st.borrow_mut();
            s.obs.retain(|&o, _| live(o));
            let s = &mut *s;
            let obs = &s.obs;
            s.order.retain(|o| obs.contains_key(o));
        }
    }
}

/// "Notify mutation observers": the delivery microtask.
fn notify(d: &Dom, it: &mut Interp<'_>, _: Value, _: &[Value]) -> V {
    let order = {
        let mut s = state(d).borrow_mut();
        s.queued = false;
        s.order.clone()
    };
    for o in order {
        let (cb, recs) = {
            let mut s = state(d).borrow_mut();
            let Some(ob) = s.obs.get_mut(&o) else { continue };
            let cb = ob.cb;
            let recs = std::mem::take(&mut ob.recs);
            s.n_recs -= recs.len();
            if s.transient > 0 {
                s.drop_regs(o, true);
            }
            if recs.is_empty() {
                continue;
            }
            (cb, recs)
        };
        let list = d.record_array(it, recs)?;
        d.mo_unwatch_if_idle();
        match it.call(cb, Value::Object(o), &[list, Value::Object(o)]) {
            Ok(_) => {}
            Err(Error::Throw(v)) => d.report(it, v, "mutationobserver", Target::Window),
            Err(e) => return Err(e),
        }
    }
    Ok(Value::Undefined)
}

fn ensure(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    if state(d).borrow().observer.is_some() {
        return Ok(());
    }
    let op = it.object_prototype();
    let (observer, record) = (d.obj(it, op)?, d.obj(it, op)?);
    let (kc, kp) = (d.key(it, "constructor")?, d.key(it, "prototype")?);
    let (co, cr) = (d.add_fn(it, construct, "MutationObserver", 1, true)?, d.add_fn(it, illegal, "MutationRecord", 0, true)?);
    for (f, proto) in [(co, observer), (cr, record)] {
        it.define_data(f, kp, Value::Object(proto), false, false, false)?;
        it.define_data(proto, kc, Value::Object(f), true, false, true)?;
    }
    d.def_method(it, observer, "observe", 1, observe)?;
    d.def_method(it, observer, "disconnect", 0, disconnect)?;
    d.def_method(it, observer, "takeRecords", 0, take_records)?;
    let mut s = state(d).borrow_mut();
    s.observer = Some(observer);
    s.record = Some(record);
    s.ctors = Some((co, cr));
    Ok(())
}

lazy_global!(lazy_observer, "MutationObserver", ensure, |d| state(d).borrow().ctors.expect("made").0);
lazy_global!(lazy_record, "MutationRecord", ensure, |d| state(d).borrow().ctors.expect("made").1);
use crate::browser_lazy::lazy_global;

pub(crate) fn install(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    crate::browser_lazy::install(d, it, &[("MutationObserver", lazy_observer as H), ("MutationRecord", lazy_record as H)])
}
