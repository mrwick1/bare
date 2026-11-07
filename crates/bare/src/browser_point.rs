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

use crate::browser::*;
use crate::browser_dom::wrap_all;
use bare_html::dom::{Document, NodeData, NodeId};
use bare_js::eval::Interp;
use bare_js::value::Value;

type V = R<Value>;

/// The context object, and the two numbers. The error text names the interface that the call is on.
fn args(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value], name: &str) -> R<(NodeId, f64, f64)> {
    let scope = d.nd(it, this)?;
    let iface = match d.data(scope) {
        NodeData::Document => "Document",
        NodeData::ShadowRoot => "ShadowRoot",
        _ => return Err(it.type_error("Illegal invocation")),
    };
    if a.len() < 2 {
        return Err(it.type_error(&format!("Failed to execute '{name}' on '{iface}': 2 arguments required, but only {} present.", a.len())));
    }
    let (x, y) = (it.to_js_number(a[0])?, it.to_js_number(a[1])?);
    if !x.is_finite() || !y.is_finite() {
        return Err(it.type_error(&format!("Failed to execute '{name}' on '{iface}': The provided double value is non-finite.")));
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
fn under(d: &Dom, it: &mut Interp<'_>, scope: NodeId, x: f64, y: f64) -> R<Vec<NodeId>> {
    let roots = roots_of(&d.doc.borrow(), scope);
    it.charge(roots.len() as u64)?;
    if roots.last() != Some(&d.doc.borrow().root()) {
        return Ok(Vec::new());
    }
    // Blink keeps a point in 1/64 px and cuts the rest toward zero: -0.01 is 0.
    let q = |v: f64| ((v * 64.0).trunc() / 64.0) as f32;
    let (px, py) = (q(x), q(y));
    let hits = d.with_geo(it, |g, s| {
        let (w, h) = g.view();
        let (rx, ry) = (px.round(), py.round());
        if rx >= 0.0 && ry >= 0.0 && rx < w && ry < h { bare_paint::hit::hits_scrolled(g.doc, g.styles, g.layout, g.areas, s, px, py) } else { Vec::new() }
    })?;
    it.charge(hits.len() as u64)?;
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

pub(crate) fn element_from_point(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let (scope, x, y) = args(d, it, this, a, "elementFromPoint")?;
    let top = under(d, it, scope, x, y)?.first().copied();
    d.wrap_opt(it, top)
}

pub(crate) fn elements_from_point(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let (scope, x, y) = args(d, it, this, a, "elementsFromPoint")?;
    let all = under(d, it, scope, x, y)?;
    wrap_all(d, it, &all)
}
