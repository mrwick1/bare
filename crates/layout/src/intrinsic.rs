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
use crate::breaks::{self, Point, Run};
use crate::text::Wrap;
use crate::block::{Context, MAX_DEPTH, border_padding, resolve};
use crate::boxes::Child;
use crate::font::{self, LETTER, Metrics};
use crate::inline::{Event, Events};
use crate::text::{self, Flow, Piece, Transform, WhiteSpace};
use bare_css::values::{Keyword, Property, Value};
use bare_html::dom::{NodeData, NodeId};

/// The min-content and max-content widths of the content box of a block container.
pub fn sizes(cx: &mut Cx, node: NodeId) -> (f32, f32) {
    // A form control with a text line has a width of its own (`controls.rs`).
    if let Some((w, _)) = crate::controls::of(&cx.tree, node).and_then(|c| crate::controls::size(&cx.tree, node, c)) {
        return (w, w);
    }
    if let Some(&(w, h, _)) = cx.tree.doc.image(node) {
        let height = resolve(cx, cx.tree.style(node).get(Property::Height), None);
        let width = height.filter(|_| h > 0).map_or(w as f32, |height| height * w as f32 / h as f32);
        return (width, width);
    }
    if let Some(&known) = cx.sizes.get(&(node.index() as u32)) {
        return known;
    }
    if cx.depth >= MAX_DEPTH {
        return (0.0, 0.0);
    }
    cx.depth += 1;
    cx.measuring += 1;
    let tree = cx.tree;
    let (mut min, mut max) = match crate::block::context(cx, node) {
        Context::Flow => flow(cx, node),
        Context::Flex => crate::flex::sizes(cx, node),
        Context::Grid => crate::grid::sizes(cx, node),
        Context::Table => crate::table::sizes(cx, node),
    };
    cx.depth -= 1;
    cx.measuring -= 1;
    // A scrollbar that is always there takes room in the content box. An `auto` one is not known yet: layout adds it.
    if tree.overflow(node).1 == Keyword::Scroll {
        min += crate::block::SCROLLBAR;
        max += crate::block::SCROLLBAR;
    }
    // A flex item too: each flex container measures its items, so nested rows would measure the same content once for
    // each level above it.
    if (cx.measuring > 0 && (matches!(tree.kind(node), crate::boxes::Kind::Atomic | crate::boxes::Kind::Float) || tree.is_flex_or_grid(tree.doc.flat_parent(node))))
        || crate::block::context(cx, node) == Context::Table
    {
        // A table is kept always: its own layout asks for the sizes again, and so do the cells that hold it.
        cx.sizes.insert(node.index() as u32, (min, max));
    }
    (min, max)
}

/// The min-content and max-content widths of the content of a block container laid out as blocks and lines: the widest
/// of its children.
pub(crate) fn flow(cx: &mut Cx, node: NodeId) -> (f32, f32) {
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
    (min, max)
}

/// The min-content and max-content widths of the margin box of a block or an inline-block. A percent counts as 0: it
/// depends on the width that we are trying to find.
pub(crate) fn outer(cx: &mut Cx, node: NodeId) -> (f32, f32) {
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
pub(crate) fn lines(cx: &mut Cx, first: NodeId, last: NodeId) -> (f32, f32) {
    let tree = cx.tree;
    if tree.styles.wrapping() && Events::new(&tree, first, last).any(|event| match event {
        Event::Text(node) => { let style = tree.style(node); Wrap::of(style, WhiteSpace::of(style)).min_content() }
        _ => false,
    }) { lines_with::<true>(cx, first, last) } else { lines_with::<false>(cx, first, last) }
}

fn lines_with<const BREAKS: bool>(cx: &mut Cx, first: NodeId, last: NodeId) -> (f32, f32) {
    let tree = cx.tree;
    let (mut min, mut max) = (0.0f32, 0.0f32);
    let (mut unit, mut line, mut hang) = (0.0f32, 0.0f32, 0.0f32);
    let mut flow = Flow::new();
    let mut runs = Vec::<Run<'_>>::new();
    let mut before = None;
    let mut word_start = true;
    let minimum = |runs: &[Run<'_>], total: f32, start: bool| {
        if BREAKS && runs.iter().any(|run| run.policy.min_content()) { breaks::minimum(runs, total, start) } else { total }
    };
    for event in Events::new(&tree, first, last) {
        match event {
            Event::Text(node) => {
                let NodeData::Text(span) = *tree.doc.data(node) else { continue };
                let style = tree.style(node);
                let (ws, transform, m) = (WhiteSpace::of(style), Transform::of(style), Metrics::of(style));
                let text = tree.doc.text(span);
                let sp = font::spacing(tree.styles.spaced(), style);
                let spaced = sp != (0.0, 0.0);
                if BREAKS && runs.is_empty() { word_start = flow.after_space; }
                let template = Run { item:0, start:0, text:"", x:0.0, before:Point {item:0, byte:None, x:0.0}, metrics:m, transform, spacing:sp,
                    policy:if BREAKS { Wrap::of(style, ws) } else { Wrap::Normal } };
                for piece in text::pieces(text, ws, transform, &mut flow) {
                    match piece {
                        Piece::Word { start, end, width } => {
                            let w = m.px(width) + if spaced { font::extra(&text[start as usize..end as usize], sp) } else { 0.0 };
                            if BREAKS {
                                runs.push(Run {item:runs.len(), start, text:&text[start as usize..end as usize], x:unit,
                                    before:before.take().unwrap_or(Point {item:runs.len(), byte:Some(start), x:unit}), ..template});
                            }
                            unit += w;
                            line += w;
                            hang = 0.0;
                        }
                        Piece::Space { width, .. } if ws.wrap => {
                            let w = m.px(width) + if spaced { font::extra_spaces(width, sp) } else { 0.0 };
                            min = min.max(minimum(&runs, unit, word_start));
                            unit = 0.0;
                            if BREAKS { runs.clear(); before = None; word_start = true; }
                            line += w;
                            hang = w;
                        }
                        Piece::Space { start, end, width } => {
                            let w = m.px(width) + if spaced { font::extra_spaces(width, sp) } else { 0.0 };
                            if BREAKS {
                                runs.push(Run {item:runs.len(), start, text:&text[start as usize..end as usize], x:unit,
                                    before:before.take().unwrap_or(Point {item:runs.len(), byte:Some(start), x:unit}), ..template});
                            }
                            unit += w;
                            line += w;
                        }
                        Piece::Tab { at } => {
                            let w = m.px(8 * LETTER) + 8.0 * (sp.0 + sp.1);
                            if BREAKS {
                                runs.push(Run {item:runs.len(), start:at, text:&text[at as usize..at as usize + 1], x:unit,
                                    before:before.take().unwrap_or(Point {item:runs.len(), byte:Some(at), x:unit}), ..template});
                            }
                            unit += w;
                            line += w;
                        }
                        Piece::Newline { .. } => {
                            min = min.max(minimum(&runs, unit, word_start));
                            max = max.max(line - hang);
                            (unit, line, hang) = (0.0, 0.0, 0.0);
                            if BREAKS { runs.clear(); before = None; word_start = true; }
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
                if BREAKS && matches!(event, Event::Open(_)) && before.is_none() {
                    before = Some(Point {item:runs.len(), byte:None, x:unit});
                }
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
                min = min.max(minimum(&runs, unit, word_start));
                unit = 0.0;
                if BREAKS { runs.clear(); before = None; word_start = false; }
                let (a, b) = outer(cx, node);
                min = min.max(a);
                line += b;
                hang = 0.0;
                flow.after_space = false;
            }
            Event::Break(_) | Event::Block(_) => {
                min = min.max(minimum(&runs, unit, word_start));
                max = max.max(line - hang);
                (unit, line, hang) = (0.0, 0.0, 0.0);
                flow = Flow::new();
                if BREAKS { runs.clear(); before = None; word_start = true; }
                if let Event::Block(node) = event {
                    let (a, b) = outer(cx, node);
                    min = min.max(a);
                    max = max.max(b);
                }
            }
        }
    }
    (min.max(minimum(&runs, unit, word_start)), max.max(line - hang))
}
