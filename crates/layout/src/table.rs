//! Table layout (CSS 2.1, chapter 17, and CSS Tables 3). Not built yet: the content is laid out as blocks and lines.

use crate::Cx;
use crate::block::{self, Kids, Sides, Space};
use bare_html::dom::NodeId;

/// Lay out the content of a table container in `space` (its content box). The fragments are relative to the border box
/// of the container: the content box starts at `bp.left`, `bp.top`.
pub(crate) fn layout(cx: &mut Cx, node: NodeId, space: Space, bp: Sides) -> Kids {
    block::flow(cx, node, space, bp)
}

/// The min-content and max-content widths of the content box of a table container.
pub(crate) fn sizes(cx: &mut Cx, node: NodeId) -> (f32, f32) {
    crate::intrinsic::flow(cx, node)
}
