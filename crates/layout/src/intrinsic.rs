//! Intrinsic widths: how wide a box wants to be, from its content alone. An inline-block with `width: auto` needs them
//! ("shrink to fit", CSS 2.1 10.3.9): it is as wide as its content on one line (the max-content width), but not wider than
//! the line it sits in, and never narrower than its longest word (the min-content width).
//!
//! Chrome computes them in a separate pass over the subtree (`ComputeMinMaxSizes`) and keeps them on each `LayoutBox`
//! until the content changes. We keep them for the layout that runs, in `Cx::sizes`, only for an inline-block that is
//! measured inside another one: layout asks for its sizes again when it lays it out. So an inline-block inside an
//! inline-block inside ... is measured once, not once for each level above it. An inline-block that is not inside
//! another one is measured once anyway, and is not kept: a page with 2,000,000 of them took 1,105 ms with all of them
//! kept, and 806 ms without (and 298 MB against 216 MB at the peak).

use crate::Cx;
use crate::block::{MAX_DEPTH, border_padding, resolve};
use crate::boxes::Child;
use crate::font::{LETTER, Metrics};
use crate::inline::{Event, Events};
use crate::text::{self, Flow, Piece, Transform, WhiteSpace};
use bare_css::values::{Keyword, Property, Value};
use bare_html::dom::{NodeData, NodeId};

/// The min-content and max-content widths of the content box of a block container.
pub fn sizes(cx: &mut Cx, node: NodeId) -> (f32, f32) {
    if let Some(&known) = cx.sizes.get(&(node.index() as u32)) {
        return known;
    }
    if cx.depth >= MAX_DEPTH {
        return (0.0, 0.0);
    }
    cx.depth += 1;
    cx.measuring += 1;
    let tree = cx.tree;
    let (mut min, mut max) = (0.0f32, 0.0f32);
    for child in tree.children(node) {
        let (a, b) = match child {
            Child::Block(b) => outer(cx, b),
            Child::Lines(first, last) => lines(cx, first, last),
        };
        min = min.max(a);
        max = max.max(b);
    }
    cx.depth -= 1;
    cx.measuring -= 1;
    // A scrollbar that is always there takes room in the content box. An `auto` one is not known yet: layout adds it.
    if tree.overflow(node).1 == Keyword::Scroll {
        min += crate::block::SCROLLBAR;
        max += crate::block::SCROLLBAR;
    }
    if cx.measuring > 0 && matches!(tree.kind(node), crate::boxes::Kind::Atomic | crate::boxes::Kind::Float) {
        cx.sizes.insert(node.index() as u32, (min, max));
    }
    (min, max)
}

/// The min-content and max-content widths of the margin box of a block or an inline-block. A percent counts as 0: it
/// depends on the width that we are trying to find.
fn outer(cx: &mut Cx, node: NodeId) -> (f32, f32) {
    let style = cx.tree.style(node);
    let (border, padding) = border_padding(cx, style, 0.0);
    let bp = border.horizontal() + padding.horizontal();
    let margin = |p| resolve(cx, style.get(p), Some(0.0)).unwrap_or(0.0);
    let margins = margin(Property::MarginLeft) + margin(Property::MarginRight);
    let border_box = style.get(Property::BoxSizing) == Value::Keyword(Keyword::BorderBox);
    let content = |v: f32| if border_box { (v - bp).max(0.0) } else { v };
    let (mut min, mut max) = match resolve(cx, style.get(Property::Width), None).map(content) {
        Some(w) => (w, w),
        None => sizes(cx, node),
    };
    if let Some(hi) = resolve(cx, style.get(Property::MaxWidth), None).map(content) {
        (min, max) = (min.min(hi), max.min(hi));
    }
    if let Some(lo) = resolve(cx, style.get(Property::MinWidth), None).map(content) {
        (min, max) = (min.max(lo), max.max(lo));
    }
    (min + bp + margins, max + bp + margins)
}

/// The min-content and max-content widths of a run of inline content: its longest unit that cannot break, and its
/// longest line if it only breaks where it must (`<br>`, a kept newline).
fn lines(cx: &mut Cx, first: NodeId, last: NodeId) -> (f32, f32) {
    let tree = cx.tree;
    let (mut min, mut max) = (0.0f32, 0.0f32);
    let (mut unit, mut line, mut hang) = (0.0f32, 0.0f32, 0.0f32);
    let mut flow = Flow::new();
    for event in Events::new(&tree, first, last) {
        match event {
            Event::Text(node) => {
                let NodeData::Text(span) = *tree.doc.data(node) else { continue };
                let style = tree.style(node);
                let (ws, transform, m) = (WhiteSpace::of(style), Transform::of(style), Metrics::of(style));
                for piece in text::pieces(tree.doc.text(span), ws, transform, &mut flow) {
                    match piece {
                        Piece::Word { width, .. } => {
                            unit += m.px(width);
                            line += m.px(width);
                            hang = 0.0;
                        }
                        Piece::Space { width, .. } if ws.wrap => {
                            min = min.max(unit);
                            unit = 0.0;
                            line += m.px(width);
                            hang = m.px(width);
                        }
                        Piece::Space { width, .. } => {
                            unit += m.px(width);
                            line += m.px(width);
                        }
                        Piece::Tab { .. } => {
                            unit += m.px(8 * LETTER);
                            line += m.px(8 * LETTER);
                        }
                        Piece::Newline { .. } => {
                            min = min.max(unit);
                            max = max.max(line - hang);
                            (unit, line, hang) = (0.0, 0.0, 0.0);
                        }
                    }
                }
            }
            Event::Open(node) | Event::Close(node) => {
                let style = tree.style(node);
                let (border, padding) = border_padding(cx, style, 0.0);
                let (m, b, p) = if matches!(event, Event::Open(_)) {
                    (Property::MarginLeft, border.left, padding.left)
                } else {
                    (Property::MarginRight, border.right, padding.right)
                };
                let edge = resolve(cx, style.get(m), Some(0.0)).unwrap_or(0.0) + b + p;
                unit += edge;
                line += edge;
            }
            // Out of the flow: it adds nothing to the widths around it.
            Event::OutOfFlow(_) => {}
            Event::Float(node) => {
                // A float is not a break opportunity. On one line it adds its width; it never fits in less than its own min.
                let (a, b) = outer(cx, node);
                min = min.max(a);
                line += b;
            }
            Event::Atomic(node) => {
                min = min.max(unit);
                unit = 0.0;
                let (a, b) = outer(cx, node);
                min = min.max(a);
                line += b;
                hang = 0.0;
                flow.after_space = false;
            }
            Event::Break(_) | Event::Block(_) => {
                min = min.max(unit);
                max = max.max(line - hang);
                (unit, line, hang) = (0.0, 0.0, 0.0);
                flow = Flow::new();
                if let Event::Block(node) = event {
                    let (a, b) = outer(cx, node);
                    min = min.max(a);
                    max = max.max(b);
                }
            }
        }
    }
    (min.max(unit), max.max(line - hang))
}
