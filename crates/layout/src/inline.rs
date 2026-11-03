//! Inline layout: lines of text and inline boxes. (A first version: one line for each run, with no text fragments.
//! Topic 4 of module 4 replaces it.)

use crate::Cx;
use crate::block::Space;
use crate::boxes::Kind;
use crate::font::{self, Metrics};
use crate::fragment::{Fragment, Kind as FragmentKind, Rect};
use bare_html::dom::NodeId;

/// The result of the lines of one run of inline content.
pub struct Lines {
    /// The height of all the lines.
    pub height: f32,
    /// There is at least one line with something on it. A run of empty inline boxes makes no line.
    pub any: bool,
}

/// Lay out the run `first..=last` of the children of `block` in lines. The fragments are relative to the top-left of
/// the lines: the caller moves them.
pub fn lines(cx: &mut Cx, block: NodeId, first: NodeId, last: NodeId, _space: Space) -> Lines {
    let tree = &cx.tree;
    let mut any = false;
    let mut n = Some(first);
    while let Some(node) = n {
        any |= match tree.kind(node) {
            Kind::Text => !tree.is_collapsible_space(node),
            Kind::None => false,
            _ => true,
        };
        n = if node == last { None } else { tree.doc.next_sibling(node) };
    }
    let style = tree.style(block);
    let height = if any { Metrics::of(style).line_height(style) } else { 0.0 };
    Lines { height, any }
}

/// The marker of a list item with no line inside: at the top, outside the content box. `dx`, `dy`: the border and padding.
pub fn lone_marker(cx: &mut Cx, item: NodeId, dx: f32, dy: f32) {
    let style = cx.tree.style(item);
    let Some(text) = crate::boxes::marker_text(style, cx.number) else { return };
    let metrics = Metrics::of(style);
    let w = metrics.px(font::width(&text));
    let at = cx.out.len() as u32;
    cx.out.push(Fragment {
        rect: Rect { x: dx - w, y: dy, w, h: metrics.ascent + metrics.descent },
        node: item,
        kind: FragmentKind::Marker { number: cx.number },
        end: at + 1,
    });
}
