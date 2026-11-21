//! `document.elementFromPoint(x, y)` and `elementsFromPoint(x, y)`, on `Document` and on `ShadowRoot` (CSSOM View, DocumentOrShadowRoot).
//!
//! The point is in client coordinates (the window, the page bars outside). The answer comes from the hit test of `bare_paint::hit` over the
//! bounded layout that `getBoundingClientRect` uses (`with_geo`: the `geometry-*` limits apply, and a gate of the cascade or the layout is
//! `Unsupported`, as there). So it follows the order of painting (`z-index`, stacking contexts, floats, positioned boxes), the scroll offsets of
//! the page and of the containers, `overflow` clips, fixed and sticky boxes, `visibility`, `display: none` (no box) and `pointer-events`.
//!
//! What Chrome 155 does, and this file does the same (`tools/fixtures/ui/point-cases.txt`, 40 cases):
//! - Both arguments are converted as WebIDL `double`: fewer than two is a `TypeError`, a non-finite value is a `TypeError`, a string or an
//!   object is converted, `null` is 0.
//! - A point outside the window gives `null` and `[]`. The test is made on the point rounded to a whole pixel (399.5 in a 400 px window is
//!   outside, -0.01 is inside); the hit test uses the point cut to 1/64 px toward zero (-0.01 is 0). The page bars are not part of the window.
//! - `elementsFromPoint` lists every element under the point, the topmost first. The root element is the last one, also when its
//!   `pointer-events` is `none`. A page that no box covers gives the root.
//! - Shadow trees: each element is retargeted against the context object (DOM Standard, "retarget"), and the list has no duplicates. The
//!   document gives the host of a shadow tree and not what is inside it; a shadow root gives what is inside it, and the outside elements
//!   as they are. A document that is not the one of the window (`createHTMLDocument`) and a shadow root that is not connected give `null` and `[]`.
//!
//! Gaps: no transforms (the engine has none), no border radius or `clip-path`; a page that has a gate in layout cannot answer.

use crate::eng::Eng;
use crate::browser::*;
use crate::browser_dom::wrap_all_s;
use crate::browser_scoped::*;
use bare_html::dom::{Document, NodeData, NodeId};
use bare_js_api::{Args, Local};

/// The context object, and the two numbers. The error text names the interface that the call is on.
fn args<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>, name: &str) -> Res<'s, (NodeId, f64, f64)> {
    let scope = d.nd_s(s, this)?;
    let iface = match d.data(scope) {
        NodeData::Document => "Document",
        NodeData::ShadowRoot => "ShadowRoot",
        _ => return Err(s.type_error("Illegal invocation")),
    };
    if a.len() < 2 {
        return Err(s.type_error(&format!("Failed to execute '{name}' on '{iface}': 2 arguments required, but only {} present.", a.len())));
    }
    let (a0, a1) = (arg_s::<E>(s, a, 0), arg_s::<E>(s, a, 1));
    let (x, y) = (s.to_number(a0)?, s.to_number(a1)?);
    if !x.is_finite() || !y.is_finite() {
        return Err(s.type_error(&format!("Failed to execute '{name}' on '{iface}': The provided double value is non-finite.")));
    }
    Ok((scope, x, y))
}

/// The roots from the tree of `scope` up to the top, through the hosts: the document is last when `scope` is connected.
fn roots_of(doc: &Document, scope: NodeId) -> Vec<NodeId> {
    let mut roots = Vec::new();
    let mut cur = scope;
    loop {
        let mut top = cur;
        while let Some(p) = doc.parent(top) {
            top = p;
        }
        roots.push(top);
        match doc.shadow_host(top) {
            Some(h) => cur = h,
            None => return roots,
        }
    }
}

/// `n` retargeted against a context object whose roots are `roots`: while the tree of `n` is a shadow tree that is not one of them, `n` is its host.
pub(crate) fn retarget(doc: &Document, mut n: NodeId, roots: &[NodeId]) -> NodeId {
    loop {
        let mut top = n;
        while let Some(p) = doc.parent(top) {
            top = p;
        }
        match doc.shadow_host(top) {
            Some(h) if !roots.contains(&top) => n = h,
            _ => return n,
        }
    }
}

/// The elements under the point, the topmost first, retargeted against `scope`.
fn under<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, scope: NodeId, x: f64, y: f64) -> Res<'s, Vec<NodeId>> {
    let _caller = crate::force::tag("elementFromPoint");
    let roots = roots_of(&d.doc.borrow(), scope);
    s.charge(roots.len() as u64)?;
    if roots.last() != Some(&d.doc.borrow().root()) {
        return Ok(Vec::new());
    }
    // Blink keeps a point in 1/64 px and cuts the rest toward zero: -0.01 is 0.
    let q = |v: f64| ((v * 64.0).trunc() / 64.0) as f32;
    let (px, py) = (q(x), q(y));
    let hits = old::<E, _>(s, |it| {
        d.with_geo(it, |g, s| {
            let (w, h) = g.view();
            let (rx, ry) = (px.round(), py.round());
            if rx >= 0.0 && ry >= 0.0 && rx < w && ry < h { bare_paint::hit::hits_scrolled(g.doc, g.styles, g.layout, g.areas, s, px, py) } else { Vec::new() }
        })
    })?;
    s.charge(hits.len() as u64)?;
    let doc = d.doc.borrow();
    let mut out: Vec<NodeId> = Vec::with_capacity(hits.len());
    for h in hits {
        let n = retarget(&doc, h, &roots);
        if !out.contains(&n) {
            out.push(n);
        }
    }
    Ok(out)
}

pub(crate) fn element_from_point<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let (scope, x, y) = args(d, s, this, a, "elementFromPoint")?;
    let top = under(d, s, scope, x, y)?.first().copied();
    d.wrap_opt_s(s, top)
}

pub(crate) fn elements_from_point<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let (scope, x, y) = args(d, s, this, a, "elementsFromPoint")?;
    let all = under(d, s, scope, x, y)?;
    wrap_all_s(d, s, &all)
}

/// `document.caretRangeFromPoint(x, y)`: a collapsed range at the text position under the point; `null` outside the window or over no text.
pub(crate) fn caret_range_from_point<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    let (_, x, y) = args(d, s, this, a, "caretRangeFromPoint")?;
    let at = old::<E, _>(s, |it| {
        d.with_geo(it, |g, s| {
            let (w, h) = g.view();
            let (px, py) = (x as f32, y as f32);
            if px.round() >= 0.0 && py.round() >= 0.0 && px.round() < w && py.round() < h { crate::browser_rects::text_at(g, s, px, py) } else { None }
        })
    })?;
    match at {
        Some((n, o)) => old_v::<E>(s, |it| d.caret_range(it, n, o)),
        None => Ok(s.null()),
    }
}
