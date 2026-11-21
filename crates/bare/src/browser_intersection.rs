//! `IntersectionObserver` and `IntersectionObserverEntry` (W3C Intersection Observer) over the layout boxes of `browser_geometry.rs` and the scroll
//! offsets of `browser_scroll.rs`. Source: the specification, read by hand, then compared with headless Chrome (`tools/fixtures/ui/intersection-cases.txt`).
//!
//! When it runs
//! - A frame (`browser_frame.rs`) computes every observation once: one cascade and one bounded layout (`Dom::with_geo`), for all observers. A frame
//!   is queued only when an observation exists and the document, its text, the viewport, a supplied sheet or a scroll offset changed since the
//!   last one, so an idle page queues none. `observe()` asks for one too. A page that never reads the global pays nothing: the globals are lazy
//!   (`browser_lazy.rs`) and the state is made on the first read.
//! - An observation reports when its threshold index or its `isIntersecting` differs from the last report. The last report starts at "none", so the
//!   first frame always reports, also for a target that is not shown.
//! - The entries are queued in the frame. A task (`Task::Notify`, due at once) delivers them, one call for each observer with entries, in the order of
//!   creation, each call followed by a microtask checkpoint. A throw is reported (`Browser::take_reports`, type `intersectionobserver`) and the
//!   delivery goes on.
//!
//! The geometry (spec "compute the intersection", as Chrome does it)
//! - The target rectangle is the border box in window coordinates (`Geo::client_rect`: page and container offsets taken off). The root rectangle is
//!   the window without the page bars (root `null` or the document), else the padding box without bars of an element that clips, else the border box
//!   of the element. `rootMargin` grows it: px is floored (as Chrome), a percent is of the width (left, right) or height (top, bottom) of the root.
//! - The target rectangle is cut by the clip of each element on its containing block chain (the parent; for `position:absolute` the nearest positioned
//!   ancestor; for `position:fixed` nothing) that has `overflow` other than `visible` (its padding box without bars), until the root. The root's own
//!   clip does not apply: its margin rectangle does. An explicit root that is not on the chain gives no geometry: all rectangles are 0.
//! - Intersecting means overlap or edge contact, also with an area of 0. The ratio is the area of the intersection over the area of the target (1 or
//!   0 for a target of area 0). `isIntersecting` is true when the rectangles meet and some threshold is not above the ratio (a target at 0.5 with
//!   `threshold: 1` is not intersecting; Chrome does the same). A target with no box (not connected, `display:none`) has all rectangles 0.
//! - Thresholds are kept as `f32`, as Chrome does (`0.1` reads back `0.10000000149011612`).
//!
//! Gates (`Unsupported`, none is faked)
//! - `trackVisibility: true` (it needs occlusion, opacity and transforms). The gates of the cascade and the layout (a missing sheet, a connected shadow tree).
//!
//! Deviations (on purpose)
//! - Entries and rectangles are own read-only data properties and plain objects with the eight fields, not prototype getters and `DOMRectReadOnly`.
//!   `thresholds` is an ordinary array, not frozen. `delay` and `scrollMargin` are not read. Called without `new`, a constructor gives the generic
//!   message of the host. An observer that observes anything is a GC root, like `ResizeObserver`.
//! - A nested same-origin frame is not modelled (the page is one top-level document), so a `rootMargin` of the implicit root reaches outside the
//!   window; Chrome inside an iframe cuts it at the iframe. Chrome could not check that case here.
//!
//! Bounds (`Limit(Host(name))`): `intersection-observers` (live observers), `intersection-observations` (all observations together).

use crate::eng::{Eng, Fault, Ops};
use crate::browser_sources::Roots;
use crate::browser::*;
use crate::browser_lazy::lazy_global;
use crate::browser_scoped::*;
use bare_css::values::{Keyword, Property, Value as Cv};
use bare_html::dom::{NodeData, NodeId};
use bare_js_api::{Args, Engine, HostFn, Local, Type};
use bare_layout::fragment::Rect;
use std::cell::{OnceCell, RefCell};
use std::collections::{HashMap, HashSet};

const MAX_OBSERVERS: usize = 1024;
const MAX_OBSERVATIONS: usize = 1 << 14;
const CTOR: &str = "Failed to construct 'IntersectionObserver'";
const INIT: &str = "Failed to read the '{}' property from 'IntersectionObserverInit'";

#[derive(Clone, Copy)]
enum Mar {
    Px(i32),
    Pct(f32),
}

struct Observation<E: Eng> {
    target: NodeId,
    wrapper: E::Obj,
    /// The threshold index and `isIntersecting` of the last report: -1 before the first.
    index: i32,
    inside: bool,
}

struct Observer<E: Eng> {
    cb: E::Raw,
    /// The explicit root and its wrapper. `doc`: the root is the document (the same as no root, but `root` reads back the document).
    root: Option<(NodeId, E::Obj)>,
    doc: bool,
    /// Top, right, bottom, left.
    margin: [Mar; 4],
    margin_text: String,
    thresholds: Vec<f32>,
    thresholds_array: E::Raw,
    obs: Vec<Observation<E>>,
    targets: HashSet<NodeId>,
    queue: Vec<E::Raw>,
}

#[derive(Clone, Copy)]
struct Protos<E: Eng> {
    observer: E::Obj,
    entry: E::Obj,
    ctors: (E::Obj, E::Obj),
}

/// What a frame looks at: document tree and attributes, text, viewport, supplied sheets, scroll offsets.
type Key = (u64, u64, (u32, u32), u64, u64);

#[derive(Default)]
struct State<E: Eng> {
    protos: Option<Protos<E>>,
    /// Live observers in creation order (the order of delivery).
    order: Vec<<E as Eng>::Obj>,
    observers: IdMap<<E as Eng>::Obj, Observer<E>>,
    observations: usize,
    pending: bool,
    last_key: Option<Key>,
    notify_queued: bool,
}

#[derive(Default)]
pub(crate) struct Io<E: Eng> {
    st: OnceCell<RefCell<State<E>>>,
}

fn state<E: Eng>(d: &Dom<E>) -> &RefCell<State<E>> {
    d.io.st.get_or_init(Default::default)
}

// ---------------------------------------------------------------------------------------------- rootMargin

/// One length of `rootMargin`: `px` (floored, like Chrome) or `%`.
fn margin_value(tok: &str) -> Option<Mar> {
    let lower = tok.to_ascii_lowercase();
    let (num, pct) = match (lower.strip_suffix("px"), lower.strip_suffix('%')) {
        (Some(n), _) => (n, false),
        (_, Some(n)) => (n, true),
        _ => return None,
    };
    if !num.starts_with(|c: char| c.is_ascii_digit() || matches!(c, '+' | '-' | '.')) || !num.chars().all(|c| c.is_ascii_digit() || matches!(c, '+' | '-' | '.' | 'e')) {
        return None;
    }
    let v: f64 = num.parse().ok().filter(|v: &f64| v.is_finite())?;
    Some(if pct { Mar::Pct(v as f32) } else { Mar::Px(v.floor().clamp(i32::MIN as f64, i32::MAX as f64) as i32) })
}

fn parse_margin(text: &str) -> Result<[Mar; 4], &'static str> {
    const UNITS: &str = "rootMargin must be specified in absolute length units or percent.";
    let toks: Vec<&str> = text.split_ascii_whitespace().collect();
    let mut v = Vec::with_capacity(4);
    for t in toks.iter().take(4) {
        v.push(margin_value(t).ok_or(UNITS)?);
    }
    if toks.len() > 4 {
        return Err("Extra text found at the end of rootMargin.");
    }
    Ok(match v[..] {
        [] => [Mar::Px(0); 4],
        [a] => [a; 4],
        [a, b] => [a, b, a, b],
        [a, b, c] => [a, b, c, b],
        [a, b, c, e] => [a, b, c, e],
        _ => unreachable!(),
    })
}

fn margin_text(m: &[Mar; 4]) -> String {
    let one = |m: &Mar| match m {
        Mar::Px(x) => format!("{x}px"),
        Mar::Pct(x) => format!("{x}%"),
    };
    m.iter().map(one).collect::<Vec<_>>().join(" ")
}

// ---------------------------------------------------------------------------------------------- constructor and methods

fn bad_init<'s, E: Eng>(s: &mut Sc<'s, E>, member: &str, what: &str) -> bare_js_api::Error<'s> {
    s.type_error(&format!("{CTOR}: {}: {what}", INIT.replace("{}", member)))
}

fn threshold_list<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, v: Local<'s>) -> Res<'s, Vec<f32>> {
    let mut items = Vec::new();
    if s.type_of(v) == Type::Undefined {
    } else if s.is_array(v)? {
        let len = s.get_str(v, "length")?;
        let n = s.to_number(len)?;
        if n > MAX_OBSERVATIONS as f64 {
            return Err(d.lim_s(s, "intersection-thresholds"));
        }
        let base = s.engine().stack_len();
        for i in 0..n as usize {
            let x = s.get_str(v, &i.to_string())?;
            items.push(s.to_number(x)?);
            s.engine().stack_truncate(base);
        }
    } else {
        items.push(s.to_number(v)?);
    }
    for &x in &items {
        if !x.is_finite() {
            return Err(bad_init::<E>(s, "threshold", "The provided double value is non-finite."));
        }
    }
    if items.iter().any(|x| !(0.0..=1.0).contains(x)) {
        return Err(s.range_error(&format!("{CTOR}: Threshold values must be numbers between 0 and 1")));
    }
    let mut out: Vec<f32> = items.into_iter().map(|x| x as f32).collect();
    out.sort_by(f32::total_cmp);
    if out.is_empty() {
        out.push(0.0);
    }
    Ok(out)
}

/// Member `name` of the options object, `undefined` when there is no object.
fn init_member<'s, E: Eng>(s: &mut Sc<'s, E>, opts: Option<Local<'s>>, name: &str) -> Res<'s, Local<'s>> {
    match opts {
        Some(o) => s.get_str(o, name),
        None => Ok(s.undefined()),
    }
}

fn construct<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let Some(cb) = a.get(0) else {
        return Err(s.type_error(&format!("{CTOR}: 1 argument required, but only 0 present.")));
    };
    if !s.is_callable(cb) {
        return Err(s.type_error(&format!("{CTOR}: parameter 1 is not of type 'Function'.")));
    }
    let cb = s.engine().raw(cb);
    let opts = a.get(1);
    if opts.is_some_and(|o| !is_nullish::<E>(s, o) && !is_object::<E>(s, o)) {
        return Err(s.type_error(&format!("{CTOR}: The provided value is not of type 'IntersectionObserverInit'.")));
    }
    let opts = opts.filter(|&o| is_object::<E>(s, o));
    // WebIDL reads the members in alphabetical order.
    let root = init_member::<E>(s, opts, "root")?;
    let node = d.node_of_s(s, root);
    let (root, doc) = match (is_nullish::<E>(s, root), node) {
        (true, _) => (None, false),
        (false, Some(n)) if d.is_element(n) => (Some((n, obj_id::<E>(s, root).expect("a node has a wrapper"))), false),
        (false, Some(n)) if matches!(d.data(n), NodeData::Document) => (Some((n, obj_id::<E>(s, root).expect("a node has a wrapper"))), true),
        _ => return Err(bad_init::<E>(s, "root", "The provided value is not of type '(Document or Element)'.")),
    };
    let m = init_member::<E>(s, opts, "rootMargin")?;
    let margin = if s.type_of(m) == Type::Undefined {
        [Mar::Px(0); 4]
    } else {
        let text = d.string_arg_s(s, m)?;
        match parse_margin(&text) {
            Ok(m) => m,
            Err(m) => return Err(d.dom_err_s(s, "SyntaxError", &format!("{CTOR}: {m}"))),
        }
    };
    let threshold = init_member::<E>(s, opts, "threshold")?;
    let thresholds = threshold_list(d, s, threshold)?;
    let track = init_member::<E>(s, opts, "trackVisibility")?;
    if s.to_boolean(track) {
        return Err(s.engine().api_err(Dom::gate("IntersectionObserver trackVisibility (it needs occlusion, opacity and transforms)")));
    }
    let st = state(d);
    if st.borrow().observers.len() >= MAX_OBSERVERS {
        return Err(d.lim_s(s, "intersection-observers"));
    }
    let list: Vec<Local<'s>> = thresholds.iter().map(|&t| s.number(t as f64)).collect();
    let thresholds_array = s.new_array(&list)?;
    let thresholds_array = s.engine().raw(thresholds_array);
    let proto = st.borrow().protos.expect("made").observer;
    let o = d.obj_s(s, proto)?;
    let margin_text = margin_text(&margin);
    let mut m = st.borrow_mut();
    m.observers.insert(o, Observer { cb, root, doc, margin, margin_text, thresholds, thresholds_array, obs: Vec::new(), targets: HashSet::new(), queue: Vec::new() });
    m.order.push(o);
    drop(m);
    d.own_obj(s.engine(), o, crate::browser::Owner::Io(o));
    Ok(d.obj_local(s, o))
}

fn illegal_entry<'s, E: Eng>(_: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    Err(s.type_error("Failed to construct 'IntersectionObserverEntry': Illegal constructor"))
}

fn observer_of<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>) -> Res<'s, E::Obj> {
    match obj_id::<E>(s, this) {
        Some(o) if state(d).borrow().observers.contains_key(&o) => Ok(o),
        _ => Err(s.type_error("Illegal invocation")),
    }
}

fn target_arg<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, a: Args<'s>, method: &str) -> Res<'s, (NodeId, E::Obj)> {
    let Some(v) = a.get(0) else {
        return Err(s.type_error(&format!("Failed to execute '{method}' on 'IntersectionObserver': 1 argument required, but only 0 present.")));
    };
    match (d.node_of_s(s, v), obj_id::<E>(s, v)) {
        (Some(n), Some(o)) if d.is_element(n) => Ok((n, o)),
        _ => Err(s.type_error(&format!("Failed to execute '{method}' on 'IntersectionObserver': parameter 1 is not of type 'Element'."))),
    }
}

fn observe<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let o = observer_of(d, s, this)?;
    let (target, wrapper) = target_arg(d, s, a, "observe")?;
    let mut m = state(d).borrow_mut();
    let m = &mut *m;
    let ob = m.observers.get_mut(&o).expect("checked");
    if ob.targets.contains(&target) {
        return Ok(s.undefined());
    }
    if m.observations >= MAX_OBSERVATIONS {
        return Err(d.lim_s(s, "intersection-observations"));
    }
    m.observations += 1;
    ob.targets.insert(target);
    ob.obs.push(Observation { target, wrapper, index: -1, inside: false });
    m.last_key = None;
    Ok(s.undefined())
}

fn unobserve<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let o = observer_of(d, s, this)?;
    let (target, _) = target_arg(d, s, a, "unobserve")?;
    let mut m = state(d).borrow_mut();
    let m = &mut *m;
    let ob = m.observers.get_mut(&o).expect("checked");
    if ob.targets.remove(&target) {
        ob.obs.retain(|x| x.target != target);
        m.observations -= 1;
    }
    Ok(s.undefined())
}

fn disconnect<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let o = observer_of(d, s, this)?;
    let mut m = state(d).borrow_mut();
    let m = &mut *m;
    let ob = m.observers.get_mut(&o).expect("checked");
    m.observations -= ob.obs.len();
    ob.obs.clear();
    ob.targets.clear();
    Ok(s.undefined())
}

fn take_records<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let o = observer_of(d, s, this)?;
    let q = std::mem::take(&mut state(d).borrow_mut().observers.get_mut(&o).expect("checked").queue);
    let items: Vec<Local<'s>> = q.iter().map(|v| s.engine().raw_local(v)).collect();
    s.new_array(&items)
}

fn root_get<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let o = observer_of(d, s, this)?;
    let root = state(d).borrow().observers[&o].root;
    Ok(match root {
        Some(r) => d.obj_local(s, r.1),
        None => s.null(),
    })
}

fn margin_get<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let o = observer_of(d, s, this)?;
    let t = state(d).borrow().observers[&o].margin_text.clone();
    d.js_str_s(s, &t)
}

fn thresholds_get<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let o = observer_of(d, s, this)?;
    let v = state(d).borrow().observers[&o].thresholds_array;
    Ok(s.engine().raw_local(&v))
}

fn ensure<E: Eng>(d: &Dom<E>, it: &mut E::Vm) -> R<E, ()> {
    if state(d).borrow().protos.is_some() {
        return Ok(());
    }
    let op = it.object_prototype();
    let (observer, entry) = (d.obj(it, op)?, d.obj(it, op)?);
    let (kc, kp) = (d.key(it, "constructor")?, d.key(it, "prototype")?);
    let mut ctors = Vec::new();
    for (name, proto, ctor, len) in [("IntersectionObserver", observer, construct as HS<E>, 1), ("IntersectionObserverEntry", entry, illegal_entry, 0)] {
        let f = d.add_fn_s_obj(it, ctor, name, len, HostFn::Ctor)?;
        it.define_data(f, kp, E::obj_raw(proto), false, false, false)?;
        it.define_data(proto, kc, E::obj_raw(f), true, false, true)?;
        crate::browser_lazy::tag(d, it, proto, name)?;
        ctors.push(f);
    }
    d.def_method_s(it, observer, "observe", 1, observe)?;
    d.def_method_s(it, observer, "unobserve", 1, unobserve)?;
    d.def_method_s(it, observer, "disconnect", 0, disconnect)?;
    d.def_method_s(it, observer, "takeRecords", 0, take_records)?;
    d.def_get_s(it, observer, "root", root_get, None)?;
    d.def_get_s(it, observer, "rootMargin", margin_get, None)?;
    d.def_get_s(it, observer, "thresholds", thresholds_get, None)?;
    state(d).borrow_mut().protos = Some(Protos { observer, entry, ctors: (ctors[0], ctors[1]) });
    Ok(())
}

lazy_global!(lazy_observer, "IntersectionObserver", ensure, |d| state(d).borrow().protos.expect("made").ctors.0);
lazy_global!(lazy_entry, "IntersectionObserverEntry", ensure, |d| state(d).borrow().protos.expect("made").ctors.1);

pub(crate) fn install<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>) -> Res<'s, ()> {
    table!(lazy = [
        Row::both(1, "IntersectionObserver", lazy_observer),
        Row::both(1, "IntersectionObserverEntry", lazy_entry),
    ]);
    let g = s.global();
    d.install_rows(s, &[g], &lazy::T::<E>::V)
}

// ---------------------------------------------------------------------------------------------- geometry

/// A rectangle as (left, top, right, bottom).
type Box4 = (f32, f32, f32, f32);

fn of_rect(r: Rect) -> Box4 {
    (r.x, r.y, r.x + r.w, r.y + r.h)
}

/// The overlap of two rectangles: edge contact counts, so a rectangle of area 0 can come out. `None`: they do not meet.
fn meet(a: Box4, b: Box4) -> Option<Box4> {
    let r = (a.0.max(b.0), a.1.max(b.1), a.2.min(b.2), a.3.min(b.3));
    (r.0 <= r.2 && r.1 <= r.3).then_some(r)
}

fn area(b: Box4) -> f32 {
    (b.2 - b.0) * (b.3 - b.1)
}

/// What one observation looks like at one frame.
struct Shot {
    bounding: Box4,
    inter: Box4,
    root: Box4,
    inside: bool,
    ratio: f32,
}

const NONE: Box4 = (0.0, 0.0, 0.0, 0.0);

fn keyword(styles: &bare_css::cascade::Styles, n: NodeId, p: Property) -> Keyword {
    match styles.style(n).map(|s| s.get(p)) {
        Some(Cv::Keyword(k)) => k,
        _ => Keyword::Static,
    }
}

/// The element that clips and contains `cur`: its containing block (`None`: the window).
fn container<E: Eng>(g: &crate::browser_scroll::Geo, cur: NodeId) -> R<E, Option<NodeId>> {
    let mut up = g.doc.parent(cur);
    match keyword(g.styles, cur, Property::Position) {
        Keyword::Fixed => return Ok(None),
        Keyword::Absolute => {
            while let Some(a) = up.filter(|&a| matches!(g.doc.data(a), NodeData::Element { .. })) {
                if keyword(g.styles, a, Property::Position) != Keyword::Static {
                    return Ok(Some(a));
                }
                up = g.doc.parent(a);
            }
            return Ok(None);
        }
        _ => {}
    }
    Ok(up.filter(|&a| matches!(g.doc.data(a), NodeData::Element { .. })))
}

fn expand(root: Box4, m: &[Mar; 4]) -> Box4 {
    let (w, h) = (root.2 - root.0, root.3 - root.1);
    let v = |m: Mar, of: f32| match m {
        Mar::Px(x) => x as f32,
        Mar::Pct(p) => p * of / 100.0,
    };
    (root.0 - v(m[3], w), root.1 - v(m[0], h), root.2 + v(m[1], w), root.3 + v(m[2], h))
}

fn shoot<E: Eng>(g: &crate::browser_scroll::Geo, s: &bare_paint::Scroll, ob: &Observer<E>, root_ok: bool, target: NodeId, connected: bool) -> R<E, Shot> {
    let none = Shot { bounding: NONE, inter: NONE, root: NONE, inside: false, ratio: 0.0 };
    let Some(rect) = connected.then(|| g.client_rect(s, target)).flatten() else { return Ok(none) };
    let bounding = of_rect(rect);
    // The root rectangle: the window, the clip of an element that clips, or the border box.
    let base = match ob.root.filter(|_| !ob.doc) {
        None => {
            let v = g.view();
            (0.0, 0.0, v.0, v.1)
        }
        Some((r, _)) => {
            let Some(cr) = root_ok.then(|| g.client_rect(s, r)).flatten() else { return Ok(none) };
            match g.areas.get(r) {
                Some(a) => {
                    let bb = crate::browser_geometry::border_box(g.layout, r).expect("a box");
                    let c = a.client();
                    (cr.x + a.pad.x - bb.x, cr.y + a.pad.y - bb.y, cr.x + a.pad.x - bb.x + c.0, cr.y + a.pad.y - bb.y + c.1)
                }
                None => of_rect(cr),
            }
        }
    };
    let root = expand(base, &ob.margin);
    // Cut the target by the clips on its chain, up to the root.
    let (mut r, mut cur, mut found) = (Some(bounding), target, ob.root.is_none() || ob.doc);
    while let Some(a) = container::<E>(g, cur)? {
        if !ob.doc && Some(a) == ob.root.map(|x| x.0) {
            r = r.and_then(|r| meet(r, root));
            found = true;
            break;
        }
        if let Some(area) = g.areas.get(a) {
            let (cr, bb) = (g.client_rect(s, a), crate::browser_geometry::border_box(g.layout, a));
            if let (Some(cr), Some(bb)) = (cr, bb) {
                let c = area.client();
                let (x, y) = (cr.x + area.pad.x - bb.x, cr.y + area.pad.y - bb.y);
                r = r.and_then(|r| meet(r, (x, y, x + c.0, y + c.1)));
            }
        }
        cur = a;
    }
    if !found {
        return Ok(none);
    }
    if ob.root.is_none() || ob.doc {
        r = r.and_then(|r| meet(r, root));
    }
    let target_area = area(bounding);
    let ratio = match r {
        Some(i) if target_area > 0.0 => area(i) / target_area,
        Some(_) => 1.0,
        None => 0.0,
    };
    let index = ob.thresholds.iter().filter(|&&t| t <= ratio).count();
    Ok(Shot { bounding, inter: r.unwrap_or(NONE), root, inside: r.is_some() && index > 0, ratio })
}

// ---------------------------------------------------------------------------------------------- frames

impl<E: Eng> Dom<E> {
    fn io_key(&self) -> Key {
        let doc = self.doc.borrow();
        let vp = self.plat.s.borrow().viewport;
        (doc.revision(), doc.text_revision(), (vp.0.to_bits(), vp.1.to_bits()), self.computed.sheet_gen.get(), self.scroll_epoch())
    }

    /// Should the scheduler queue a rendering step now? Marks one as pending when it says yes. A page with no observer pays one `get`.
    pub(crate) fn io_wants_frame(&self) -> bool {
        let Some(st) = self.io.st.get() else { return false };
        if st.borrow().observations == 0 {
            return false;
        }
        let key = self.io_key();
        let mut s = st.borrow_mut();
        if s.pending || s.last_key == Some(key) {
            return false;
        }
        s.pending = true;
        true
    }

    fn rect_obj(&self, it: &mut E::Vm, b: Box4) -> R<E, E::Raw> {
        crate::browser_geom::rect_object(self, it, [b.0 as f64, b.1 as f64, (b.2 - b.0) as f64, (b.3 - b.1) as f64], false)
    }

    /// Compute every observation once, queue the entries of those that changed, and ask for their delivery.
    pub(crate) fn io_frame(&self, it: &mut E::Vm) -> R<E, ()> {
        let _caller = crate::force::tag("IntersectionObserver");
        let Some(st) = self.io.st.get() else { return Ok(()) };
        {
            let mut s = st.borrow_mut();
            s.pending = false;
            if s.observations == 0 {
                return Ok(());
            }
        }
        // Which targets and roots are connected: this walks the tree with the step charge, so it is done before the geometry.
        type Roots<E: Eng> = Vec<(E::Obj, Option<NodeId>)>;
        let (list, roots): (Vec<(E::Obj, NodeId)>, Roots<E>) = {
            let s = st.borrow();
            let pairs = s.order.iter().flat_map(|&o| s.observers[&o].obs.iter().map(move |x| (o, x.target))).collect();
            (pairs, s.order.iter().map(|&o| (o, s.observers[&o].root.filter(|_| !s.observers[&o].doc).map(|r| r.0))).collect())
        };
        let mut connected = HashMap::new();
        for &(_, t) in &list {
            if let std::collections::hash_map::Entry::Vacant(e) = connected.entry(t) {
                e.insert(self.in_document(it, t)?);
            }
        }
        let root_ok: HashMap<E::Obj, bool> = roots.into_iter().map(|(o, r)| Ok((o, r.map_or(Ok(true), |r| self.in_document(it, r))?))).collect::<R<E, _>>()?;
        it.charge_o(list.len() as u64)?;
        // One cascade and one layout for all of them.
        let shots = self.with_geo(it, |g, s| -> R<E, Vec<(E::Obj, usize, Shot)>> {
            let st = state(self).borrow();
            let mut out = Vec::new();
            for &o in &st.order {
                let ob = &st.observers[&o];
                for (i, x) in ob.obs.iter().enumerate() {
                    out.push((o, i, shoot(g, s, ob, root_ok[&o], x.target, connected[&x.target])?));
                }
            }
            Ok(out)
        })??;
        let now = E::num(self.now_ms());
        let (mut wake, mut made) = (false, Vec::new());
        {
            let mut s = st.borrow_mut();
            for (o, i, shot) in shots {
                let ob = s.observers.get_mut(&o).expect("listed");
                let index = ob.thresholds.iter().filter(|&&t| t <= shot.ratio).count() as i32;
                let x = &mut ob.obs[i];
                if (x.index, x.inside) != (index, shot.inside) {
                    (x.index, x.inside) = (index, shot.inside);
                    made.push((o, x.wrapper, shot));
                }
            }
        }
        for (o, wrapper, shot) in made {
            it.charge_o(1)?;
            let e = self.obj(it, st.borrow().protos.expect("made").entry)?;
            let fields = [
                ("time", now),
                ("rootBounds", self.rect_obj(it, shot.root)?),
                ("boundingClientRect", self.rect_obj(it, shot.bounding)?),
                ("intersectionRect", self.rect_obj(it, shot.inter)?),
                ("isIntersecting", E::bool_raw(shot.inside)),
                ("intersectionRatio", E::num(shot.ratio as f64)),
                ("target", E::obj_raw(wrapper)),
            ];
            for (name, v) in fields {
                let k = self.key(it, name)?;
                it.define_data(e, k, v, false, true, true)?;
            }
            st.borrow_mut().observers.get_mut(&o).expect("listed").queue.push(E::obj_raw(e));
            wake = true;
        }
        let key = self.io_key();
        let mut s = st.borrow_mut();
        s.last_key = Some(key);
        if wake && !s.notify_queued {
            s.notify_queued = true;
            drop(s);
            self.want_notify()?;
        }
        Ok(())
    }

    /// The delivery task: each observer with entries gets one call, in the order of creation.
    pub(crate) fn io_notify(&self, it: &mut E::Vm) -> R<E, ()> {
        let Some(st) = self.io.st.get() else { return Ok(()) };
        st.borrow_mut().notify_queued = false;
        let order = st.borrow().order.clone();
        for o in order {
            let (cb, queue) = {
                let mut s = st.borrow_mut();
                let Some(ob) = s.observers.get_mut(&o) else { continue };
                if ob.queue.is_empty() {
                    continue;
                }
                (ob.cb, std::mem::take(&mut ob.queue))
            };
            let list = it.array_from(&queue)?;
            match it.call_o(cb, E::obj_raw(o), &[list, E::obj_raw(o)]) {
                Ok(_) => {}
                Err(Fault::Throw(v)) => self.report(it, v, "intersectionobserver", Target::Window),
                Err(e) => return Err(e),
            }
            it.host_checkpoint()?;
        }
        Ok(())
    }

    pub(crate) fn io_roots(&self, out: &mut Roots<'_, '_, E>) {
        let Some(st) = self.io.st.get() else { return };
        let s = st.borrow();
        if let Some(p) = s.protos {
            out.extend([p.observer, p.entry, p.ctors.0, p.ctors.1].map(E::obj_raw));
        }
        out.extend(s.observers.iter().filter(|(_, ob)| !ob.obs.is_empty() || !ob.queue.is_empty()).map(|(&o, _)| E::obj_raw(o)));
    }

    pub(crate) fn io_trace(&self, owner: E::Obj, t: &mut bare_js_api::Tracer<'_, E::Raw>) {
        let out = &mut Roots::<E>(t);
        if let Some(st) = self.io.st.get()
            && let Some(ob) = st.borrow().observers.get(&owner)
        {
            out.extend([ob.cb, ob.thresholds_array]);
            out.extend(ob.root.map(|r| E::obj_raw(r.1)));
            out.extend(ob.obs.iter().map(|x| E::obj_raw(x.wrapper)));
            out.extend(ob.queue.iter().copied());
        }
    }

    /// The entries of the observer table (tests).
    #[cfg(test)]
    pub(crate) fn io_entries(&self) -> usize {
        self.io.st.get().map_or(0, |s| s.borrow().observers.len())
    }

    /// The observers `dead` were collected: drop their entries.
    pub(crate) fn io_forget(&self, dead: &[E::Obj]) {
        if let Some(st) = self.io.st.get() {
            let mut s = st.borrow_mut();
            let s = &mut *s;
            for o in dead {
                s.observers.remove(o);
            }
            let observers = &s.observers;
            s.order.retain(|o| observers.contains_key(o));
        }
    }
}
