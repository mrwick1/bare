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
//!   box: the border box minus borders and padding. `contentRect` is the content box at (padding-left, padding-top). An element with
//!   no box (not connected, inside `display: none`) and a non-replaced inline element report 0 x 0, as the standard says for inline.
//!   Only horizontal writing is modelled: inline size is the width, block size the height.
//! - Callbacks run in one host scope for the frame, each with `(entries, observer)` and `this` the observer, then a microtask
//!   checkpoint. A throw is reported (`Browser::take_reports`) and the frame goes on. A limit, `Unsupported` or heap fault ends it.
//!
//! Gates (`Unsupported`, never a guess): `box: 'device-pixel-content-box'` and the entry's `devicePixelContentBoxSize` (no device pixel
//! ratio), a padding that is not an absolute length (a percent needs the containing block), and every gate of `layout_now`.
//!
//! Deviations: entry and size fields are own read-only data properties, not prototype getters. The size arrays are ordinary arrays,
//! not frozen. `DOMRectReadOnly` is a plain object with the eight fields, not that interface. Subclassing `ResizeObserver` keeps the
//! base prototype. An observer that observes anything is a GC root until `unobserve` or `disconnect` (Blink keeps it alive through
//! its targets), so it also keeps detached targets alive.
//!
//! Bounds (`Limit(Host(name))`): `resize-observers` (live observers), `resize-observations` (all observations together).

use crate::browser::*;
use bare_css::cascade::Styles;
use bare_css::values::{Length, Property, Value as Cv};
use bare_layout::boxes::{Kind, Tree};
use bare_layout::fragment::{Kind as FragKind, Rect};
use bare_html::dom::{Document, NodeId};
use bare_js::eval::{Error, Interp};
use bare_js::value::{ObjId, Value};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};

type V = R<Value>;

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
}

struct Observation {
    target: NodeId,
    /// The target's wrapper: the entry gives it back, and the observer keeps it alive.
    wrapper: ObjId,
    observed: Boxes,
    last: (f32, f32),
}

struct Observer {
    cb: Value,
    obs: Vec<Observation>,
    /// The targets of `obs`: `observe` of a new target skips the scan of `obs`.
    targets: HashSet<NodeId>,
    /// Indexes in `obs` of the observations to deliver now.
    active: Vec<usize>,
    skipped: bool,
}

#[derive(Clone, Copy)]
struct Protos {
    observer: ObjId,
    entry: ObjId,
    size: ObjId,
}

/// What a frame looks at: document tree and attributes, text, viewport, supplied sheets.
type Key = (u64, u64, (u32, u32), u64);

#[derive(Default)]
struct State {
    protos: Option<Protos>,
    /// Live observers in creation order (the order of delivery).
    order: Vec<ObjId>,
    observers: HashMap<ObjId, Observer>,
    observations: usize,
    pending: bool,
    last_key: Option<Key>,
}

#[derive(Default)]
pub(crate) struct Resize {
    lim: Cell<ResizeLimits>,
    s: RefCell<State>,
}

#[derive(Clone, Copy, Default)]
struct Sizes {
    border: (f32, f32),
    content: (f32, f32),
    /// padding-left and padding-top: the place of `contentRect`.
    at: (f32, f32),
}

const DEVICE: &str = "ResizeObserver device-pixel-content-box (the device pixel ratio is not modelled)";
const PADDING: &str = "ResizeObserver content box with a padding that is not an absolute length (it needs the containing block)";
const LOOP: &str = "ResizeObserver loop completed with undelivered notifications.";

pub(crate) fn install(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    let op = it.object_prototype();
    let (observer, entry, size) = (d.obj(it, op)?, d.obj(it, op)?, d.obj(it, op)?);
    d.resize.s.borrow_mut().protos = Some(Protos { observer, entry, size });
    let g = it.global_object();
    let (kc, kp) = (d.key(it, "constructor")?, d.key(it, "prototype")?);
    for (name, proto, ctor, len) in [
        ("ResizeObserver", observer, Some(construct as H), 1),
        ("ResizeObserverEntry", entry, None, 0),
        ("ResizeObserverSize", size, None, 0),
    ] {
        let f = d.add_fn(it, ctor.unwrap_or(illegal_constructor), name, len, true)?;
        it.define_data(f, kp, Value::Object(proto), false, false, false)?;
        it.define_data(proto, kc, Value::Object(f), true, false, true)?;
        let k = d.key(it, name)?;
        it.define_data(g, k, Value::Object(f), true, false, true)?;
    }
    d.def_method(it, observer, "observe", 1, observe)?;
    d.def_method(it, observer, "unobserve", 1, unobserve)?;
    d.def_method(it, observer, "disconnect", 0, disconnect)?;
    d.def_get(it, entry, "devicePixelContentBoxSize", device_sizes, None)
}

fn construct(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> V {
    let cb = a.first().copied().unwrap_or(Value::Undefined);
    if !it.is_callable_value(cb) {
        return Err(it.type_error("Failed to construct 'ResizeObserver': parameter 1 is not of type 'Function'."));
    }
    let proto = d.resize.s.borrow().protos.expect("installed").observer;
    if d.resize.s.borrow().observers.len() >= d.resize.lim.get().max_observers {
        return Err(d.lim("resize-observers"));
    }
    let o = d.obj(it, proto)?;
    let mut s = d.resize.s.borrow_mut();
    s.observers.insert(o, Observer { cb, obs: Vec::new(), targets: HashSet::new(), active: Vec::new(), skipped: false });
    s.order.push(o);
    Ok(Value::Object(o))
}

fn observer_of(d: &Dom, it: &mut Interp<'_>, this: Value) -> R<ObjId> {
    match this {
        Value::Object(o) if d.resize.s.borrow().observers.contains_key(&o) => Ok(o),
        _ => Err(it.type_error("Illegal invocation")),
    }
}

fn target_arg(d: &Dom, it: &mut Interp<'_>, a: &[Value], method: &str) -> R<(NodeId, ObjId)> {
    let v = a.first().copied().unwrap_or(Value::Undefined);
    match (d.node_of(v), v) {
        (Some(n), Value::Object(o)) if d.is_element(n) => Ok((n, o)),
        _ => Err(it.type_error(&format!("Failed to execute '{method}' on 'ResizeObserver': parameter 1 is not of type 'Element'."))),
    }
}

fn remove(d: &Dom, o: ObjId, target: NodeId) {
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

fn observe(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let o = observer_of(d, it, this)?;
    let (target, wrapper) = target_arg(d, it, a, "observe")?;
    // The options dictionary: `box` is read once, after the target check (WebIDL order).
    let observed = match a.get(1).copied().unwrap_or(Value::Undefined) {
        Value::Undefined | Value::Null => Boxes::Content,
        Value::Object(_) => {
            let k = d.key(it, "box")?;
            match it.get_value(a[1], k)? {
                Value::Undefined => Boxes::Content,
                v => match d.string_arg(it, v)?.as_str() {
                    "content-box" => Boxes::Content,
                    "border-box" => Boxes::Border,
                    "device-pixel-content-box" => return Err(Dom::gate(DEVICE)),
                    other => {
                        let m = format!("Failed to execute 'observe' on 'ResizeObserver': The provided value '{other}' is not a valid enum value of type ResizeObserverBoxOptions.");
                        return Err(it.type_error(&m));
                    }
                },
            }
        }
        _ => return Err(it.type_error("Failed to execute 'observe' on 'ResizeObserver': The provided value is not of type 'ResizeObserverOptions'.")),
    };
    // Observing a target again replaces its observation (the standard calls `unobserve` first).
    remove(d, o, target);
    let mut s = d.resize.s.borrow_mut();
    if s.observations >= d.resize.lim.get().max_observations {
        return Err(d.lim("resize-observations"));
    }
    s.observations += 1;
    let ob = s.observers.get_mut(&o).expect("checked");
    ob.targets.insert(target);
    ob.obs.push(Observation { target, wrapper, observed, last: (-1.0, -1.0) });
    s.last_key = None;
    Ok(Value::Undefined)
}

fn unobserve(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let o = observer_of(d, it, this)?;
    let (target, _) = target_arg(d, it, a, "unobserve")?;
    remove(d, o, target);
    Ok(Value::Undefined)
}

fn disconnect(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let o = observer_of(d, it, this)?;
    let mut s = d.resize.s.borrow_mut();
    let s = &mut *s;
    let ob = s.observers.get_mut(&o).expect("checked");
    s.observations -= ob.obs.len();
    ob.obs.clear();
    ob.targets.clear();
    ob.active.clear();
    Ok(Value::Undefined)
}

fn device_sizes(_: &Dom, _: &mut Interp<'_>, _: Value, _: &[Value]) -> V {
    Err(Dom::gate(DEVICE))
}

/// A padding side in px. Only an absolute length is known without the containing block.
fn padding(style: &bare_css::cascade::ComputedStyle, p: Property) -> R<f32> {
    match style.get(p) {
        Cv::Length(Length::Px(x)) => Ok(x.max(0.0)),
        _ => Err(Dom::gate(PADDING)),
    }
}

fn sizes_of(tree: &Tree, styles: &Styles, border: Option<Rect>, el: NodeId) -> R<Sizes> {
    let Some(style) = styles.style(el) else { return Ok(Sizes::default()) };
    // No box (`display: none`) or a non-replaced inline box: 0 x 0 (an ancestor with no box leaves no fragment, below).
    if matches!(tree.kind(el), Kind::Inline | Kind::Break | Kind::None) {
        return Ok(Sizes::default());
    }
    let Some(r) = border else { return Ok(Sizes::default()) };
    let (pl, pr, pt, pb) = (padding(style, Property::PaddingLeft)?, padding(style, Property::PaddingRight)?, padding(style, Property::PaddingTop)?, padding(style, Property::PaddingBottom)?);
    let b = |p| style.px(p).max(0.0);
    let w = (r.w - b(Property::BorderLeftWidth) - b(Property::BorderRightWidth) - pl - pr).max(0.0);
    let h = (r.h - b(Property::BorderTopWidth) - b(Property::BorderBottomWidth) - pt - pb).max(0.0);
    Ok(Sizes { border: (r.w, r.h), content: (w, h), at: (pl, pt) })
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

impl Dom {
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
    fn measure(&self, it: &mut Interp<'_>) -> R<HashMap<NodeId, Sizes>> {
        let targets: Vec<NodeId> = {
            let s = self.resize.s.borrow();
            s.order.iter().flat_map(|o| s.observers[o].obs.iter().map(|x| x.target)).collect()
        };
        let styles = self.cascade_now(it)?;
        let layout = self.layout_now(it, &styles)?;
        // The border box of each target: the union of its box fragments, in one pass over the fragments (CSSOM View).
        let mut boxes: HashMap<NodeId, Option<Rect>> = targets.iter().map(|&t| (t, None)).collect();
        it.charge(layout.fragments.len() as u64)?;
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
            it.charge(1)?;
            out.insert(t, sizes_of(&tree, &styles, border, t)?);
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
                let now = if x.observed == Boxes::Border { z.border } else { z.content };
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

    fn size_obj(&self, it: &mut Interp<'_>, proto: ObjId, (w, h): (f32, f32)) -> R<Value> {
        let o = self.obj(it, proto)?;
        for (name, v) in [("inlineSize", w), ("blockSize", h)] {
            let k = self.key(it, name)?;
            it.define_data(o, k, Value::Number(v as f64), false, true, true)?;
        }
        Ok(Value::Object(o))
    }

    fn entry(&self, it: &mut Interp<'_>, p: Protos, wrapper: ObjId, z: Sizes) -> R<Value> {
        it.charge(1)?;
        let e = self.obj(it, p.entry)?;
        let rect = crate::browser_geom::rect_object(self, it, [z.at.0 as f64, z.at.1 as f64, z.content.0 as f64, z.content.1 as f64], false)?;
        let border = self.size_obj(it, p.size, z.border)?;
        let border = it.array_from(&[border])?;
        let content = self.size_obj(it, p.size, z.content)?;
        let content = it.array_from(&[content])?;
        for (name, v) in [("target", Value::Object(wrapper)), ("contentRect", rect), ("borderBoxSize", border), ("contentBoxSize", content)] {
            let k = self.key(it, name)?;
            it.define_data(e, k, v, false, true, true)?;
        }
        Ok(Value::Object(e))
    }

    /// Broadcast: each observer with active observations, in creation order, gets its entries. Gives the shallowest delivered depth.
    fn broadcast(&self, it: &mut Interp<'_>, sizes: &HashMap<NodeId, Sizes>) -> R<usize> {
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
                    x.last = if x.observed == Boxes::Border { z.border } else { z.content };
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
            match it.call(cb, Value::Object(o), &[list, Value::Object(o)]) {
                Ok(_) => {}
                Err(Error::Throw(v)) => self.report(it, v, "resizeobserver", Target::Window),
                Err(e) => return Err(e),
            }
            it.host_checkpoint()?;
        }
        Ok(shallowest)
    }

    /// The rendering step: gather and broadcast until no observation is active (see the header).
    pub(crate) fn resize_frame(&self, it: &mut Interp<'_>) -> R<()> {
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
            let e = it.new_error(it.error_prototype(), LOOP)?;
            self.report(it, Value::Object(e), "error", Target::Window);
        }
        Ok(())
    }

    pub(crate) fn resize_roots(&self, out: &mut Vec<Value>) {
        let s = self.resize.s.borrow();
        if let Some(p) = s.protos {
            out.extend([p.observer, p.entry, p.size].map(Value::Object));
        }
        out.extend(s.observers.iter().filter(|(_, ob)| !ob.obs.is_empty()).map(|(&o, _)| Value::Object(o)));
    }

    pub(crate) fn resize_trace(&self, owner: ObjId, out: &mut Vec<Value>) {
        if let Some(ob) = self.resize.s.borrow().observers.get(&owner) {
            out.push(ob.cb);
            out.extend(ob.obs.iter().map(|x| Value::Object(x.wrapper)));
        }
    }

    pub(crate) fn resize_prune(&self, live: &dyn Fn(ObjId) -> bool) {
        let mut s = self.resize.s.borrow_mut();
        s.observers.retain(|&o, _| live(o));
        let s = &mut *s;
        let observers = &s.observers;
        s.order.retain(|o| observers.contains_key(o));
    }
}

impl Browser {
    /// Replace the bounds of this module. Call it before scripts run.
    pub fn set_resize_limits(&mut self, l: ResizeLimits) {
        self.dom.resize.lim.set(l);
    }
}
