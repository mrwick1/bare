//! `ResizeObserver` (CSSWG Resize Observer) over the native cascade and layout of `browser_geometry.rs`.
//!
//! How it works
//! - Observers are delivered in a rendering step, a scheduler task (`Task::Render`, `browser_scheduler.rs`), as HTML "update the
//!   rendering" does. No paint loop exists, so a frame runs only when it can change something: the scheduler queues one, 16 ms
//!   later, when an observation exists and the document, its text, the viewport or a supplied sheet changed since the last frame
//!   (`Dom::resize_wants_frame`). `observe()` asks for a frame too. An idle page queues no frame, so a session can settle.
//! - A frame follows the standard: gather the active observations at depth 0, then broadcast and gather again at the depth of the
//!   shallowest delivered target, until no observation is active. An observation is active when its size is not the last reported
//!   size, which starts at -1 x -1 (so the first frame always reports). Depth is the number of nodes on the flat-tree path to the root.
//!   An active observation that is not deeper than the depth is skipped. If one is skipped, the frame reports
//!   `Error: ResizeObserver loop completed with undelivered notifications.` and asks for another frame.
//! - Each gather measures the page once: one cascade and one bounded layout (`Dom::layout_now`), for all targets. A broadcast uses
//!   these sizes, so a callback that changes the page is seen by the next gather, not by a later observer in the same broadcast.
//! - Sizes are in CSS pixels, not rounded (`f32` layout units). Border box: the union of the element's border-box fragments. Content
//!   box: the border box minus borders and padding. Device pixel content box: the content box with its edges rounded to whole device
//!   pixels (`devicePixelRatio` is 1, so a device pixel is a CSS pixel), as Chrome snaps it. Every entry has all three sizes. `contentRect` is the content box at (padding-left, padding-top). An element with
//!   no box (not connected, inside `display: none`) and a non-replaced inline element report 0 x 0, as the standard says for inline.
//!   Only horizontal writing is modelled: inline size is the width, block size the height.
//! - Callbacks run in one host scope for the frame, each with `(entries, observer)` and `this` the observer, then a microtask
//!   checkpoint. A throw is reported (`Browser::take_reports`) and the frame goes on. A limit, `Unsupported` or heap fault ends it.
//!
//! Gates (`Unsupported`, never a guess): a padding that is not an absolute length (a percent needs the containing block), and every gate of `layout_now`.
//!
//! Deviations: entry and size fields are own read-only data properties, not prototype getters. The size arrays are ordinary arrays,
//! not frozen. `DOMRectReadOnly` is a plain object with the eight fields, not that interface. Subclassing `ResizeObserver` keeps the
//! base prototype. An observer that observes anything is a GC root until `unobserve` or `disconnect` (Blink keeps it alive through
//! its targets), so it also keeps detached targets alive.
//!
//! Bounds (`Limit(Host(name))`): `resize-observers` (live observers), `resize-observations` (all observations together).

use crate::eng::{Eng, Fault, Ops};
use crate::browser_sources::Roots;
use crate::browser::*;
use bare_css::cascade::Styles;
use bare_css::values::{Length, Property, Value as Cv};
use bare_layout::boxes::{Kind, Tree};
use bare_layout::fragment::{Kind as FragKind, Rect};
use bare_html::dom::{Document, NodeId};
use bare_js_api::{Args, Engine, HostFn, Local, Type};
use crate::browser_scoped::*;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};

/// Bounds of the module. Each is checked before the thing grows.
#[derive(Clone, Copy, Debug)]
pub struct ResizeLimits {
    pub max_observers: usize,
    pub max_observations: usize,
}

impl Default for ResizeLimits {
    fn default() -> Self {
        ResizeLimits { max_observers: 1024, max_observations: 4096 }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Boxes {
    Content,
    Border,
    Device,
}

struct Observation<E: Eng> {
    target: NodeId,
    /// The target's wrapper: the entry gives it back, and the observer keeps it alive.
    wrapper: E::Obj,
    observed: Boxes,
    last: (f32, f32),
}

struct Observer<E: Eng> {
    cb: E::Raw,
    obs: Vec<Observation<E>>,
    /// The targets of `obs`: `observe` of a new target skips the scan of `obs`.
    targets: HashSet<NodeId>,
    /// Indexes in `obs` of the observations to deliver now.
    active: Vec<usize>,
    skipped: bool,
}

#[derive(Clone, Copy)]
struct Protos<E: Eng> {
    observer: E::Obj,
    entry: E::Obj,
    size: E::Obj,
}

/// What a frame looks at: document tree and attributes, text, viewport, supplied sheets.
type Key = (u64, u64, (u32, u32), u64);

#[derive(Default)]
struct State<E: Eng> {
    protos: Option<Protos<E>>,
    /// Live observers in creation order (the order of delivery).
    order: Vec<<E as Eng>::Obj>,
    observers: IdMap<<E as Eng>::Obj, Observer<E>>,
    observations: usize,
    pending: bool,
    last_key: Option<Key>,
}

#[derive(Default)]
pub(crate) struct Resize<E: Eng> {
    lim: Cell<ResizeLimits>,
    s: RefCell<State<E>>,
}

#[derive(Clone, Copy, Default)]
struct Sizes {
    border: (f32, f32),
    content: (f32, f32),
    /// The content box snapped to whole device pixels.
    device: (f32, f32),
    /// padding-left and padding-top: the place of `contentRect`.
    at: (f32, f32),
}

const PADDING: &str = "ResizeObserver content box with a padding that is not an absolute length (it needs the containing block)";
const LOOP: &str = "ResizeObserver loop completed with undelivered notifications.";

pub(crate) fn install<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>) -> Res<'s, ()> {
    let (observer, entry, size) = (d.plain_s(s)?, d.plain_s(s)?, d.plain_s(s)?);
    d.resize.s.borrow_mut().protos = Some(Protos { observer, entry, size });
    for (name, proto, ctor, len) in [
        ("ResizeObserver", observer, Some(construct as HS<E>), 1),
        ("ResizeObserverEntry", entry, None, 0),
        ("ResizeObserverSize", size, None, 0),
    ] {
        d.interface_s(s, proto, name, ctor.unwrap_or(illegal_ctor), len, HostFn::Ctor)?;
    }
    table!(rows1 = [
        Row::method(1, "observe", 1, observe),
        Row::method(1, "unobserve", 1, unobserve),
        Row::method(1, "disconnect", 0, disconnect),
    ]);
    let targets = [d.obj_local(s, observer)];
    d.install_rows(s, &targets, &rows1::T::<E>::V)
}

fn illegal_ctor<'s, E: Eng>(_: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    Err(s.type_error("Illegal constructor"))
}

fn construct<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let cb = arg_s::<E>(s, a, 0);
    if !s.is_callable(cb) {
        return Err(s.type_error("Failed to construct 'ResizeObserver': parameter 1 is not of type 'Function'."));
    }
    let cb = s.engine().raw(cb);
    let proto = d.resize.s.borrow().protos.expect("installed").observer;
    if d.resize.s.borrow().observers.len() >= d.resize.lim.get().max_observers {
        return Err(d.lim_s(s, "resize-observers"));
    }
    let o = d.obj_s(s, proto)?;
    let mut m = d.resize.s.borrow_mut();
    m.observers.insert(o, Observer { cb, obs: Vec::new(), targets: HashSet::new(), active: Vec::new(), skipped: false });
    m.order.push(o);
    drop(m);
    d.own_obj(s.engine(), o, crate::browser::Owner::Resize(o));
    Ok(d.obj_local(s, o))
}

fn observer_of<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>) -> Res<'s, E::Obj> {
    match obj_id::<E>(s, this) {
        Some(o) if d.resize.s.borrow().observers.contains_key(&o) => Ok(o),
        _ => Err(s.type_error("Illegal invocation")),
    }
}

fn target_arg<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, a: Args<'s>, method: &str) -> Res<'s, (NodeId, E::Obj)> {
    let v = arg_s::<E>(s, a, 0);
    match (d.node_of_s(s, v), obj_id::<E>(s, v)) {
        (Some(n), Some(o)) if d.is_element(n) => Ok((n, o)),
        _ => Err(s.type_error(&format!("Failed to execute '{method}' on 'ResizeObserver': parameter 1 is not of type 'Element'."))),
    }
}

fn remove<E: Eng>(d: &Dom<E>, o: E::Obj, target: NodeId) {
    let mut s = d.resize.s.borrow_mut();
    let s = &mut *s;
    let ob = s.observers.get_mut(&o).expect("checked");
    if !ob.targets.remove(&target) {
        return;
    }
    let before = ob.obs.len();
    ob.obs.retain(|x| x.target != target);
    ob.active.clear();
    s.observations -= before - ob.obs.len();
}

fn observe<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let o = observer_of(d, s, this)?;
    let (target, wrapper) = target_arg(d, s, a, "observe")?;
    // The options dictionary: `box` is read once, after the target check (WebIDL order).
    let opts = arg_s::<E>(s, a, 1);
    let observed = if is_nullish::<E>(s, opts) {
        Boxes::Content
    } else if is_object::<E>(s, opts) {
        let b = s.get_str(opts, "box")?;
        if s.type_of(b) == Type::Undefined {
            Boxes::Content
        } else {
            match d.string_arg_s(s, b)?.as_str() {
                "content-box" => Boxes::Content,
                "border-box" => Boxes::Border,
                "device-pixel-content-box" => Boxes::Device,
                other => {
                    let m = format!("Failed to execute 'observe' on 'ResizeObserver': The provided value '{other}' is not a valid enum value of type ResizeObserverBoxOptions.");
                    return Err(s.type_error(&m));
                }
            }
        }
    } else {
        return Err(s.type_error("Failed to execute 'observe' on 'ResizeObserver': The provided value is not of type 'ResizeObserverOptions'."));
    };
    // Observing a target again replaces its observation (the standard calls `unobserve` first).
    remove(d, o, target);
    let mut m = d.resize.s.borrow_mut();
    if m.observations >= d.resize.lim.get().max_observations {
        drop(m);
        return Err(d.lim_s(s, "resize-observations"));
    }
    m.observations += 1;
    let ob = m.observers.get_mut(&o).expect("checked");
    ob.targets.insert(target);
    ob.obs.push(Observation { target, wrapper, observed, last: (-1.0, -1.0) });
    m.last_key = None;
    drop(m);
    Ok(s.undefined())
}

fn unobserve<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let o = observer_of(d, s, this)?;
    let (target, _) = target_arg(d, s, a, "unobserve")?;
    remove(d, o, target);
    Ok(s.undefined())
}

fn disconnect<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let o = observer_of(d, s, this)?;
    let mut m = d.resize.s.borrow_mut();
    let m = &mut *m;
    let ob = m.observers.get_mut(&o).expect("checked");
    m.observations -= ob.obs.len();
    ob.obs.clear();
    ob.targets.clear();
    ob.active.clear();
    Ok(s.undefined())
}

impl Sizes {
    fn of(&self, b: Boxes) -> (f32, f32) {
        match b {
            Boxes::Content => self.content,
            Boxes::Border => self.border,
            Boxes::Device => self.device,
        }
    }
}

/// A padding side in px. Only an absolute length is known without the containing block.
fn padding<E: Eng>(style: &bare_css::cascade::ComputedStyle, p: Property) -> R<E, f32> {
    match style.get(p) {
        Cv::Length(Length::Px(x)) => Ok(x.max(0.0)),
        _ => Err(Dom::<E>::gate(PADDING)),
    }
}

fn sizes_of<E: Eng>(tree: &Tree, styles: &Styles, border: Option<Rect>, el: NodeId) -> R<E, Sizes> {
    let Some(style) = styles.style(el) else { return Ok(Sizes::default()) };
    // No box (`display: none`) or a non-replaced inline box: 0 x 0 (an ancestor with no box leaves no fragment, below).
    if matches!(tree.kind(el), Kind::Inline | Kind::Break | Kind::None) {
        return Ok(Sizes::default());
    }
    let Some(r) = border else { return Ok(Sizes::default()) };
    let (pl, pr, pt, pb) = (padding::<E>(style, Property::PaddingLeft)?, padding::<E>(style, Property::PaddingRight)?, padding::<E>(style, Property::PaddingTop)?, padding::<E>(style, Property::PaddingBottom)?);
    let b = |p| style.px(p).max(0.0);
    let w = (r.w - b(Property::BorderLeftWidth) - b(Property::BorderRightWidth) - pl - pr).max(0.0);
    let h = (r.h - b(Property::BorderTopWidth) - b(Property::BorderBottomWidth) - pt - pb).max(0.0);
    let (x, y) = (r.x + b(Property::BorderLeftWidth) + pl, r.y + b(Property::BorderTopWidth) + pt);
    let device = ((x + w).round() - x.round(), (y + h).round() - y.round());
    Ok(Sizes { border: (r.w, r.h), content: (w, h), device, at: (pl, pt) })
}

/// The number of nodes on the flat-tree path from `n` to its root.
fn depth(doc: &Document, n: NodeId) -> usize {
    let (mut d, mut cur) = (1, n);
    while let Some(p) = doc.flat_parent(cur) {
        d += 1;
        cur = p;
    }
    d
}

impl<E: Eng> Dom<E> {
    fn resize_key(&self) -> Key {
        let doc = self.doc.borrow();
        let vp = self.plat.s.borrow().viewport;
        (doc.revision(), doc.text_revision(), (vp.0.to_bits(), vp.1.to_bits()), self.computed.sheet_gen.get())
    }

    /// Should the scheduler queue a rendering step now? Marks one as pending when it says yes.
    pub(crate) fn resize_wants_frame(&self) -> bool {
        // A page with no observation pays one borrow here, not the key.
        if self.resize.s.borrow().observations == 0 {
            return false;
        }
        let key = self.resize_key();
        let mut s = self.resize.s.borrow_mut();
        if s.pending || s.last_key == Some(key) {
            return false;
        }
        s.pending = true;
        true
    }

    /// Measure every observed target once: one cascade and one bounded layout.
    fn measure(&self, it: &mut E::Vm) -> R<E, HashMap<NodeId, Sizes>> {
        let _caller = crate::force::tag("ResizeObserver");
        let targets: Vec<NodeId> = {
            let s = self.resize.s.borrow();
            s.order.iter().flat_map(|o| s.observers[o].obs.iter().map(|x| x.target)).collect()
        };
        let styles = self.cascade_now(it)?;
        let layout = &self.layout_now(it, &styles)?.layout;
        // The border box of each target: the union of its box fragments, in one pass over the fragments (CSSOM View).
        let mut boxes: HashMap<NodeId, Option<Rect>> = targets.iter().map(|&t| (t, None)).collect();
        it.charge_o(layout.fragments.len() as u64)?;
        for f in layout.fragments.iter().filter(|f| f.kind == FragKind::Box) {
            if let Some(b) = boxes.get_mut(&f.node) {
                let r = f.rect;
                *b = Some(match *b {
                    None => r,
                    Some(u) => {
                        let (x, y) = (u.x.min(r.x), u.y.min(r.y));
                        Rect { x, y, w: (u.x + u.w).max(r.x + r.w) - x, h: (u.y + u.h).max(r.y + r.h) - y }
                    }
                });
            }
        }
        let doc = self.doc.borrow();
        let tree = Tree::new(&doc, &styles);
        let mut out = HashMap::with_capacity(boxes.len());
        for (t, border) in boxes {
            it.charge_o(1)?;
            out.insert(t, sizes_of::<E>(&tree, &styles, border, t)?);
        }
        Ok(out)
    }

    /// Gather the active observations deeper than `at`. Gives (any active, any skipped).
    fn gather(&self, at: usize, sizes: &HashMap<NodeId, Sizes>) -> (bool, bool) {
        let doc = self.doc.borrow();
        let mut s = self.resize.s.borrow_mut();
        let (mut active, mut skipped) = (false, false);
        for ob in s.observers.values_mut() {
            ob.active.clear();
            ob.skipped = false;
            for (i, x) in ob.obs.iter().enumerate() {
                let z = sizes[&x.target];
                let now = z.of(x.observed);
                if now != x.last {
                    if depth(&doc, x.target) > at {
                        ob.active.push(i);
                    } else {
                        ob.skipped = true;
                    }
                }
            }
            active |= !ob.active.is_empty();
            skipped |= ob.skipped;
        }
        (active, skipped)
    }

    fn size_obj(&self, it: &mut E::Vm, proto: E::Obj, (w, h): (f32, f32)) -> R<E, E::Raw> {
        let o = self.obj(it, proto)?;
        for (name, v) in [("inlineSize", w), ("blockSize", h)] {
            let k = self.key(it, name)?;
            it.define_data(o, k, E::num(v as f64), false, true, true)?;
        }
        Ok(E::obj_raw(o))
    }

    fn entry(&self, it: &mut E::Vm, p: Protos<E>, wrapper: E::Obj, z: Sizes) -> R<E, E::Raw> {
        it.charge_o(1)?;
        let e = self.obj(it, p.entry)?;
        let rect = crate::browser_geom::rect_object(self, it, [z.at.0 as f64, z.at.1 as f64, z.content.0 as f64, z.content.1 as f64], false)?;
        let border = self.size_obj(it, p.size, z.border)?;
        let border = it.array_from(&[border])?;
        let content = self.size_obj(it, p.size, z.content)?;
        let content = it.array_from(&[content])?;
        let device = self.size_obj(it, p.size, z.device)?;
        let device = it.array_from(&[device])?;
        for (name, v) in [("target", E::obj_raw(wrapper)), ("contentRect", rect), ("borderBoxSize", border), ("contentBoxSize", content), ("devicePixelContentBoxSize", device)] {
            let k = self.key(it, name)?;
            it.define_data(e, k, v, false, true, true)?;
        }
        Ok(E::obj_raw(e))
    }

    /// Broadcast: each observer with active observations, in creation order, gets its entries. Gives the shallowest delivered depth.
    fn broadcast(&self, it: &mut E::Vm, sizes: &HashMap<NodeId, Sizes>) -> R<E, usize> {
        let order = self.resize.s.borrow().order.clone();
        let mut shallowest = usize::MAX;
        for o in order {
            // Read the observer again: an earlier callback may have disconnected it or changed its observations.
            let (cb, picked, p) = {
                let mut s = self.resize.s.borrow_mut();
                let p = s.protos.expect("installed");
                let Some(ob) = s.observers.get_mut(&o) else { continue };
                if ob.active.is_empty() {
                    continue;
                }
                let doc = self.doc.borrow();
                let mut picked = Vec::new();
                for &i in &std::mem::take(&mut ob.active) {
                    let x = &mut ob.obs[i];
                    let z = sizes[&x.target];
                    x.last = z.of(x.observed);
                    shallowest = shallowest.min(depth(&doc, x.target));
                    picked.push((x.wrapper, z));
                }
                (ob.cb, picked, p)
            };
            let mut entries = Vec::with_capacity(picked.len());
            for (w, z) in picked {
                entries.push(self.entry(it, p, w, z)?);
            }
            let list = it.array_from(&entries)?;
            match it.call_o(cb, E::obj_raw(o), &[list, E::obj_raw(o)]) {
                Ok(_) => {}
                Err(Fault::Throw(v)) => self.report(it, v, "resizeobserver", Target::Window),
                Err(e) => return Err(e),
            }
            it.host_checkpoint()?;
        }
        Ok(shallowest)
    }

    /// The rendering step: gather and broadcast until no observation is active (see the header).
    pub(crate) fn resize_frame(&self, it: &mut E::Vm) -> R<E, ()> {
        self.resize.s.borrow_mut().pending = false;
        if self.resize.s.borrow().observations == 0 {
            return Ok(());
        }
        let mut at = 0;
        let skipped = loop {
            let sizes = self.measure(it)?;
            let (active, skipped) = self.gather(at, &sizes);
            if !active {
                break skipped;
            }
            at = self.broadcast(it, &sizes)?;
        };
        let key = self.resize_key();
        let mut s = self.resize.s.borrow_mut();
        // Skipped observations are delivered by the next frame.
        s.last_key = if skipped { None } else { Some(key) };
        drop(s);
        if skipped {
            let e = it.new_error_o(it.error_prototype(), LOOP)?;
            self.report(it, E::obj_raw(e), "error", Target::Window);
        }
        Ok(())
    }

    pub(crate) fn resize_roots(&self, out: &mut Roots<'_, '_, E>) {
        let s = self.resize.s.borrow();
        if let Some(p) = s.protos {
            out.extend([p.observer, p.entry, p.size].map(E::obj_raw));
        }
        out.extend(s.observers.iter().filter(|(_, ob)| !ob.obs.is_empty()).map(|(&o, _)| E::obj_raw(o)));
    }

    pub(crate) fn resize_trace(&self, owner: E::Obj, t: &mut bare_js_api::Tracer<'_, E::Raw>) {
        let out = &mut Roots::<E>(t);
        if let Some(ob) = self.resize.s.borrow().observers.get(&owner) {
            out.push(ob.cb);
            out.extend(ob.obs.iter().map(|x| E::obj_raw(x.wrapper)));
        }
    }

    /// The entries of the observer table (tests).
    #[cfg(test)]
    pub(crate) fn resize_entries(&self) -> usize {
        self.resize.s.borrow().observers.len()
    }

    /// The observers `dead` were collected: drop their entries.
    pub(crate) fn resize_forget(&self, dead: &[E::Obj]) {
        let mut s = self.resize.s.borrow_mut();
        let s = &mut *s;
        for o in dead {
            s.observers.remove(o);
        }
        let observers = &s.observers;
        s.order.retain(|o| observers.contains_key(o));
    }
}

impl<E: Eng> Browser<E> {
    /// Replace the bounds of this module. Call it before scripts run.
    pub fn set_resize_limits(&mut self, l: ResizeLimits) {
        self.dom.resize.lim.set(l);
    }
}
