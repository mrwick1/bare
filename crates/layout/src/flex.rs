//! Flex layout (CSS Flexible Box Layout 1, section 9). A flex container puts its children (the "items") in a row or a
//! column, the "main axis", and the other axis is the "cross axis". The steps:
//!
//! 1. Items: each child box, and each run of text (an anonymous item). `order` sorts them.
//! 2. The flex base size of each item: its `flex-basis`, else its `width` (or `height` in a column), else its content.
//!    Its "hypothetical main size" is that size between its min and max sizes.
//! 3. Lines: with `flex-wrap: wrap`, the items go on a new line when the line is full.
//! 4. Flexible lengths: on each line, the space that is left goes to the items by `flex-grow`, or the space that is
//!    missing comes from them by `flex-shrink` times their base size. An item that hits its min or max size stops there
//!    ("frozen"), and the others share the rest again.
//! 5. Cross sizes: each item is laid out at its main size. A line is as tall as its tallest item. `align-items:
//!    stretch` (the default) makes the items as tall as the line.
//! 6. Alignment: `justify-content` and `margin: auto` on the main axis, `align-items` / `align-self` on the cross axis,
//!    `align-content` for the lines.
//!
//! How the engines do it: Chrome (`FlexLayoutAlgorithm` in LayoutNG) lays out an item once to measure it, and once more
//! when `stretch` gives it a new height, unless its cache says that the result does not change. Firefox
//! (`nsFlexContainerFrame`) does the same. Here the items lay out straight into the list of fragments, and each keeps
//! its range. To stretch an item, a second layout runs only if its content can depend on its height: a percent that met
//! an unknown base (`Cx::unknown`), a scroll container, a positioned item, an item with a `height`, or one that is a
//! flex, grid or table container itself. Else only the height of its box changes. A row of cards with
//! `align-items: stretch` is laid out once.

use crate::block::{self, Kids, Sides, Space, border_padding, resolve};
use crate::boxes::{Child, Kind};
use crate::fragment::{self, Fragment};
use crate::{Cx, inline, intrinsic, memo};
use bare_css::cascade::ComputedStyle;
use bare_css::values::{Keyword as K, Property as P, Value};
use bare_html::dom::NodeId;

/// One flex item. The sizes are of the content box. "Main" and "cross" are the axes: for a row, main is the width.
struct Item {
    node: NodeId,
    /// An anonymous item: the run of text from `node` to this node.
    last: Option<NodeId>,
    order: i32,
    /// The number of a list item, for its marker.
    number: i32,
    grow: f32,
    shrink: f32,
    base: f32,
    hypo: f32,
    min: f32,
    max: f32,
    /// The size that the flexible lengths give it (step 4).
    main: f32,
    frozen: bool,
    /// The borders and padding on the main and on the cross axis.
    bp_main: f32,
    bp_cross: f32,
    /// The margins at the left (or top) and right (or bottom) of the main axis, then of the cross axis. `None` for `auto`.
    margins: [Option<f32>; 4],
    /// The cross size of the border box, once laid out.
    cross: f32,
    /// The baseline of its last line, and of its first line (only for an item aligned by baseline), from the top of the border box.
    baseline: Option<f32>,
    first: Option<f32>,
    /// The height of its content alone, once laid out (`Done::content`).
    content: f32,
    align: K,
    /// The content box size in the cross axis of a column item (its width).
    width: f32,
    /// Its layout does not depend on its height: a new height changes only its box.
    rigid: bool,
    /// Its fragments: `cx.out[from..to]`, or `moved` if it was laid out again.
    from: usize,
    to: usize,
    moved: Option<Vec<Fragment>>,
    /// How many static positions its layout left waiting for a containing block outside it.
    waits: u32,
    /// The place of its margin box from the content box of the container: main, cross.
    at: (f32, f32),
    /// Its last layout, for the map of layouts that a run again throws away (`memo.rs`). `None` once the fragments changed.
    laid: Option<Box<memo::Laid>>,
}

impl Item {
    fn margin(&self, i: usize) -> f32 {
        self.margins[i].unwrap_or(0.0)
    }

    /// The size of the margin box on the main axis, for a content size.
    fn outer(&self, size: f32) -> f32 {
        size + self.bp_main + self.margin(0) + self.margin(1)
    }

    fn outer_cross(&self) -> f32 {
        self.cross + self.margin(2) + self.margin(3)
    }

    /// An item aligned by baseline, with no auto margin on the cross axis, shares a baseline with the others of its kind: gives the
    /// kind (0 first, 1 last) and the distance from its cross-start margin edge to the baseline. A column item has its baseline on
    /// its cross-start edge, as in Chrome.
    fn shares(&self, row: bool) -> Option<(usize, f32)> {
        if !matches!(self.align, K::Baseline | K::LastBaseline) || self.margins[2].is_none() || self.margins[3].is_none() {
            return None;
        }
        let last = self.align == K::LastBaseline;
        let b = if !row { 0.0 } else { (if last { self.baseline } else { self.first }).unwrap_or(self.cross) };
        Some((last as usize, self.margin(2) + b))
    }
}

/// One line of items: the items `start..end` (in `order`), and its cross size and place.
struct Line {
    start: usize,
    end: usize,
    cross: f32,
    at: f32,
    /// The items aligned by their first baseline (0) and by their last one (1) share a baseline each: the biggest distance from the
    /// cross-start margin edge to it, and from it to the cross-end margin edge.
    above: [f32; 2],
    below: [f32; 2],
}

fn keyword(style: &ComputedStyle, p: P) -> K {
    match style.get(p) {
        Value::Keyword(k) => k,
        _ => K::Normal,
    }
}

fn number(style: &ComputedStyle, p: P, default: f32) -> f32 {
    match style.get(p) {
        Value::Number(n) => n,
        _ => default,
    }
}

/// The children of a flex container that are items: (the node, the last node of a text run). A run of text that is
/// only spaces is not an item. `statics`: a run that holds only spaces and boxes out of the flow.
fn children(cx: &Cx, node: NodeId, mut statics: impl FnMut(NodeId, NodeId)) -> Vec<(NodeId, Option<NodeId>)> {
    let tree = cx.tree;
    let mut out = Vec::new();
    for child in tree.children(node) {
        match child {
            Child::Block(b) => out.push((b, None)),
            Child::Lines(first, last) => {
                let mut n = Some(first);
                let (mut text, mut oof) = (false, false);
                while let Some(c) = n {
                    match tree.kind(c) {
                        Kind::Text => text |= !tree.is_collapsible_space(c),
                        Kind::OutOfFlow => oof = true,
                        _ => {}
                    }
                    n = if c == last { None } else { tree.doc.next_sibling(c) };
                }
                if text {
                    out.push((first, Some(last)));
                } else if oof {
                    statics(first, last);
                }
            }
        }
    }
    out
}

/// Lay out the content of a flex container in `space` (its content box). The fragments are relative to the border box
/// of the container: the content box starts at `bp.left`, `bp.top`.
pub(crate) fn layout(cx: &mut Cx, node: NodeId, space: Space, bp: Sides) -> Kids {
    let style = cx.tree.style(node);
    let direction = keyword(style, P::FlexDirection);
    let row = matches!(direction, K::Row | K::RowReverse);
    let reverse = matches!(direction, K::RowReverse | K::ColumnReverse);
    let wrap = keyword(style, P::FlexWrap) != K::Nowrap;
    let wrap_reverse = keyword(style, P::FlexWrap) == K::WrapReverse;
    let gap = |p, base| resolve(cx, style.get(p), base).unwrap_or(0.0).max(0.0);
    let (column_gap, row_gap) = (gap(P::ColumnGap, Some(space.width)), gap(P::RowGap, space.height));
    let (main_gap, cross_gap) = if row { (column_gap, row_gap) } else { (row_gap, column_gap) };
    // The min and max height of the container, for a height that is not known yet.
    let border_box = style.get(P::BoxSizing) == Value::Keyword(K::BorderBox);
    let content = |v: f32| if border_box { (v - bp.vertical()).max(0.0) } else { v };
    let lo = resolve(cx, style.get(P::MinHeight), None).map_or(0.0, content);
    let hi = resolve(cx, style.get(P::MaxHeight), None).map_or(f32::INFINITY, content);
    let align_items = match keyword(style, P::AlignItems) {
        K::Normal => K::Stretch,
        k => k,
    };
    let (justify, align_content) = (keyword(style, P::JustifyContent), keyword(style, P::AlignContent));

    // A marker of a list item around the container does not go inside an item.
    let marker = cx.marker.take();
    let mut found = Vec::new();
    let kids = children(cx, node, |first, last| found.push((first, last)));
    // The boxes out of the flow in a run with no item: their static position is the corner of the content box.
    for (first, last) in found {
        let from = cx.out.len();
        inline::lines(cx, node, first, last, space.width, (bp.left, bp.top));
        fragment::shift(&mut cx.out, from, bp.left, bp.top);
    }
    let mut counter = cx.tree.counter(node);
    let begin = cx.out.len();
    let mut items: Vec<Item> = kids.into_iter().map(|(n, last)| item(cx, node, n, last, space, row, wrap, align_items, &mut counter)).collect();
    if items.iter().any(|i| i.order != 0) {
        items.sort_by_key(|i| i.order);
        // A stable sort: items with the same `order` keep the order of the document.
    }

    // The main size of the container. A column with `height: auto` is as tall as its items, between its min and max.
    let main_size = if row { Some(space.width) } else { space.height };
    let limit = if wrap { main_size.unwrap_or(hi) } else { f32::INFINITY };
    let mut lines = Vec::new();
    let mut start = 0;
    let mut used = 0.0;
    for (i, it) in items.iter().enumerate() {
        let size = it.outer(it.hypo);
        if i > start && used + main_gap + size > limit + 0.01 {
            lines.push(Line { start, end: i, cross: 0.0, at: 0.0, above: [0.0; 2], below: [0.0; 2] });
            (start, used) = (i, size);
        } else {
            used += if i > start { main_gap + size } else { size };
        }
    }
    if start < items.len() {
        lines.push(Line { start, end: items.len(), cross: 0.0, at: 0.0, above: [0.0; 2], below: [0.0; 2] });
    }
    let main_size = main_size.unwrap_or_else(|| {
        let longest = lines.iter().map(|l| sum(&items[l.start..l.end], main_gap, |it| it.outer(it.hypo))).fold(0.0, f32::max);
        longest.min(hi).max(lo)
    });
    for line in &lines {
        flex(&mut items[line.start..line.end], main_size, main_gap);
    }

    // Lay out each item at its main size, for its cross size.
    for it in &mut items {
        if row {
            lay(cx, node, it, space, it.main, None);
        } else if (it.main - (it.cross - it.bp_main)).abs() > 0.01 {
            // A column item was laid out for its content height. Its final height differs.
            resize(cx, node, it, space, it.width, it.main, it.bp_main);
        }
        if !row {
            it.cross = it.width + it.bp_cross;
        }
    }
    let single = !wrap;
    let cross_size = if row { space.height } else { Some(space.width) };
    for line in &mut lines {
        let (mut big, mut above, mut below) = (0.0f32, [0.0f32; 2], [0.0f32; 2]);
        for it in &items[line.start..line.end] {
            if let Some((k, a)) = it.shares(row) {
                above[k] = above[k].max(a);
                below[k] = below[k].max(it.outer_cross() - a);
            } else {
                big = big.max(it.outer_cross());
            }
        }
        (line.above, line.below) = (above, below);
        line.cross = big.max(above[0] + below[0]).max(above[1] + below[1]);
        if single {
            line.cross = match cross_size {
                Some(c) => c,
                None if row => line.cross.min(hi).max(lo),
                None => line.cross,
            };
        }
    }
    // `align-content`: the space that the lines leave in a container with a cross size.
    let n = lines.len();
    let total = sum(&lines, cross_gap, |l| l.cross);
    let free = if single { 0.0 } else { cross_size.map_or(0.0, |c| c - total) };
    let (mut offset, mut between) = (0.0, cross_gap);
    match align_content {
        K::Normal | K::Stretch | K::LastBaseline if free > 0.0 => {
            for line in &mut lines {
                line.cross += free / n as f32;
            }
        }
        k => (offset, between) = distribute(k, free, n, wrap_reverse, cross_gap),
    }
    for line in &mut lines {
        line.at = offset;
        offset += line.cross + between;
    }
    let content_cross = if lines.is_empty() { 0.0 } else { offset - between };
    let container_cross = cross_size.unwrap_or(content_cross);

    // Stretch, then place each item in its line.
    for line in &lines {
        let free = main_size - sum(&items[line.start..line.end], main_gap, |it| it.outer(it.main));
        let autos = items[line.start..line.end].iter().map(|it| it.margins[..2].iter().filter(|m| m.is_none()).count()).sum::<usize>();
        let (mut x, between) = if autos > 0 && free > 0.0 {
            for it in &mut items[line.start..line.end] {
                for m in 0..2 {
                    if it.margins[m].is_none() {
                        it.margins[m] = Some(free / autos as f32);
                    }
                }
            }
            (0.0, main_gap)
        } else {
            distribute(justify, free, line.end - line.start, reverse, main_gap)
        };
        for it in &mut items[line.start..line.end] {
            let size = it.outer(it.main);
            it.at.0 = if reverse { main_size - x - size } else { x };
            x += size + between;
            if row && it.last.is_none() && it.align == K::Stretch && it.margins[2].is_some() && it.margins[3].is_some() && cx.tree.style(it.node).get(P::Height) == Value::Keyword(K::Auto) {
                let style = cx.tree.style(it.node);
                let tall = stretched(cx, style, line.cross - it.margin(2) - it.margin(3) - it.bp_cross, space, border_box_of(style), it.bp_cross);
                if (tall + it.bp_cross - it.cross).abs() > 0.01 {
                    resize(cx, node, it, space, it.main, tall, it.bp_cross);
                }
            } else if !row && wrap && it.last.is_none() && it.align == K::Stretch && it.margins[2].is_some() && it.margins[3].is_some() && cx.tree.style(it.node).get(P::Width) == Value::Keyword(K::Auto) {
                let style = cx.tree.style(it.node);
                let content = |v: f32| if border_box_of(style) { (v - it.bp_cross).max(0.0) } else { v };
                let lo = resolve(cx, style.get(P::MinWidth), Some(space.width)).map_or(0.0, content);
                let hi = resolve(cx, style.get(P::MaxWidth), Some(space.width)).map_or(f32::INFINITY, content);
                let w = (line.cross - it.margin(2) - it.margin(3) - it.bp_cross).min(hi).max(lo).max(0.0);
                if (w - it.width).abs() > 0.01 {
                    it.width = w;
                    relayout(cx, node, it, space, w, Some(it.main));
                    it.cross = w + it.bp_cross;
                }
            }
            let free = line.cross - it.outer_cross();
            let y = if let Some((k, a)) = it.shares(row) {
                // The baselines are shared: the first ones at the cross-start of the line, the last ones at its cross-end. With
                // `wrap-reverse` the start is at the bottom of the page, and the items keep their order from the top.
                let top = if (k == 0) != wrap_reverse { line.above[k] - a } else { line.cross - line.below[k] - a };
                if wrap_reverse { free - top } else { top }
            } else {
                match (it.margins[2], it.margins[3]) {
                    (None, None) => free.max(0.0) / 2.0,
                    (None, Some(_)) => free.max(0.0),
                    (Some(_), None) => 0.0,
                    _ => match it.align {
                        K::Center => free / 2.0,
                        K::FlexEnd => free,
                        K::End | K::SelfEnd if !wrap_reverse => free,
                        K::Start | K::SelfStart if wrap_reverse => free,
                        _ => 0.0,
                    },
                }
            };
            for m in 2..4 {
                it.margins[m] = Some(it.margin(m));
            }
            let size = it.outer_cross();
            it.at.1 = if wrap_reverse { container_cross - line.at - y - size } else { line.at + y };
        }
    }

    // Place the fragments. They are in `cx.out` in the order of the items, unless `order` sorted the items or an item
    // was laid out again: then they are written again, in `order`.
    let mut baseline = None;
    let first = lines.first().map_or(0..0, |l| l.start..l.end);
    let by_baseline = if row { items[first.clone()].iter().position(|it| it.shares(true).is_some()) } else { None };
    let mut at = begin;
    let in_place = items.iter().all(|it| {
        let ok = it.moved.is_none() && it.from == at;
        at = it.to;
        ok
    });
    let old = if in_place { Vec::new() } else { cx.out.split_off(begin) };
    for (i, it) in items.iter_mut().enumerate() {
        let (x, y) = if row { (it.at.0 + it.margin(0), it.at.1 + it.margin(2)) } else { (it.at.1 + it.margin(2), it.at.0 + it.margin(0)) };
        let base = if in_place { it.from } else { cx.out.len() };
        if !in_place {
            let (list, from) = match &it.moved {
                Some(list) => (&list[..], 0),
                None => (&old[it.from - begin..it.to - begin], it.from),
            };
            cx.out.extend(list.iter().map(|f| Fragment { end: f.end - from as u32 + base as u32, ..*f }));
        }
        let end = if in_place { it.to } else { cx.out.len() };
        if first.contains(&i) && (by_baseline.is_none() && i == first.start || by_baseline == Some(i - first.start)) {
            // The shared baseline, if the item is aligned by baseline; else its first one, read from a few fragments. An
            // item with no baseline gives its bottom border edge (CSS Flexbox 1, section 8.5).
            let b = if it.align == K::LastBaseline && by_baseline.is_some() { it.baseline } else { it.first.or_else(|| first_of(cx, it, &cx.out, base, end, block::PEEK)) };
            baseline = Some(y + b.unwrap_or(if row { it.cross } else { it.main + it.bp_main }));
        }
        fragment::shift(&mut cx.out[..end], base, bp.left + x, bp.top + y);
    }
    cx.marker = marker;
    let height = if row { content_cross } else { main_size };
    Kids::of(height, baseline)
}

/// The sum of the sizes of `list`, with a gap between each two.
fn sum<T>(list: &[T], gap: f32, size: impl Fn(&T) -> f32) -> f32 {
    list.iter().map(size).sum::<f32>() + gap * list.len().saturating_sub(1) as f32
}

/// `justify-content` or `align-content`: where the first item (or line) starts, and the space between two of them, for
/// `free` space and `n` of them. `reverse`: the main axis goes the other way, so `start` and `end` (of the page) swap.
fn distribute(k: K, free: f32, n: usize, reverse: bool, gap: f32) -> (f32, f32) {
    let n = n.max(1) as f32;
    match k {
        K::FlexEnd => (free, gap),
        K::End | K::Right if !reverse => (free, gap),
        K::Start | K::Left if reverse => (free, gap),
        K::Center => (free / 2.0, gap),
        K::SpaceBetween if free > 0.0 && n > 1.0 => (0.0, gap + free / (n - 1.0)),
        // With no room, or one item, `space-around` and `space-evenly` center.
        K::SpaceAround | K::SpaceEvenly if free < 0.0 || n == 1.0 => (free / 2.0, gap),
        K::SpaceAround => (free / n / 2.0, gap + free / n),
        K::SpaceEvenly => (free / (n + 1.0), gap + free / (n + 1.0)),
        _ => (0.0, gap),
    }
}

/// Step 4: the main size of each item on one line (CSS Flexbox 1, 9.7).
fn flex(items: &mut [Item], size: f32, gap: f32) {
    let gaps = gap * items.len().saturating_sub(1) as f32;
    let grow = items.iter().map(|it| it.outer(it.hypo)).sum::<f32>() + gaps < size;
    for it in items.iter_mut() {
        it.main = it.hypo;
        let factor = if grow { it.grow } else { it.shrink };
        it.frozen = factor == 0.0 || (grow && it.base > it.hypo) || (!grow && it.base < it.hypo);
    }
    let free = |items: &[Item]| size - gaps - items.iter().map(|it| if it.frozen { it.outer(it.main) } else { it.outer(it.base) }).sum::<f32>();
    let initial = free(items);
    let mut violations = vec![0.0f32; items.len()];
    while items.iter().any(|it| !it.frozen) {
        let mut rest = free(items);
        // The sum is an f64: factors near the range of f32 (`flex: 1e39` is 3.4e38) add up past it, as in Chrome.
        let factors: f64 = items.iter().filter(|it| !it.frozen).map(|it| f64::from(if grow { it.grow } else { it.shrink })).sum();
        // Factors that sum to less than 1 take only that part of the space.
        if factors < 1.0 && (f64::from(initial) * factors).abs() < f64::from(rest.abs()) {
            rest = (f64::from(initial) * factors) as f32;
        }
        let scaled: f64 = items.iter().filter(|it| !it.frozen).map(|it| f64::from(it.shrink) * f64::from(it.base)).sum();
        let mut total = 0.0;
        for (it, v) in items.iter_mut().zip(&mut violations) {
            if it.frozen {
                continue;
            }
            let target = if grow {
                it.base + (f64::from(rest) * f64::from(it.grow) / factors) as f32
            } else if scaled > 0.0 {
                it.base - (f64::from(rest.abs()) * f64::from(it.shrink) * f64::from(it.base) / scaled) as f32
            } else {
                it.base
            };
            let clamped = target.min(it.max).max(it.min).max(0.0);
            *v = clamped - target;
            total += *v;
            it.main = clamped;
        }
        for (it, &v) in items.iter_mut().zip(&violations) {
            if !it.frozen && (!(total.abs() >= 0.001) || (total > 0.0 && v > 0.0) || (total < 0.0 && v < 0.0)) {
                it.frozen = true;
            }
        }
    }
}

fn border_box_of(style: &ComputedStyle) -> bool {
    style.get(P::BoxSizing) == Value::Keyword(K::BorderBox)
}

/// A content height between `min-height` and `max-height`.
fn stretched(cx: &Cx, style: &ComputedStyle, h: f32, space: Space, border_box: bool, bp: f32) -> f32 {
    let content = |v: f32| if border_box { (v - bp).max(0.0) } else { v };
    let lo = resolve(cx, style.get(P::MinHeight), space.height).map_or(0.0, content);
    let hi = resolve(cx, style.get(P::MaxHeight), space.height).map_or(f32::INFINITY, content);
    h.min(hi).max(lo).max(0.0)
}

/// Steps 1 and 2 for one item: its style, its base size and its min and max main sizes. A column item is laid out here,
/// at its width, for its content height.
#[allow(clippy::too_many_arguments)]
fn item(cx: &mut Cx, container: NodeId, node: NodeId, last: Option<NodeId>, space: Space, row: bool, wrap: bool, align_items: K, counter: &mut crate::boxes::Counter) -> Item {
    let mut it = Item {
        node,
        last,
        order: 0,
        number: 0,
        grow: 0.0,
        shrink: 1.0,
        base: 0.0,
        hypo: 0.0,
        min: 0.0,
        max: f32::INFINITY,
        main: 0.0,
        frozen: false,
        bp_main: 0.0,
        bp_cross: 0.0,
        margins: [Some(0.0); 4],
        cross: 0.0,
        baseline: None,
        first: None,
        content: 0.0,
        align: align_items,
        width: space.width,
        rigid: true,
        from: 0,
        to: 0,
        moved: None,
        waits: 0,
        at: (0.0, 0.0),
        laid: None,
    };
    if last.is_some() {
        // An anonymous item: no style of its own, no box. Its sizes are those of its lines.
        let (min, max) = intrinsic::lines(cx, node, last.unwrap_or(node));
        if row {
            (it.base, it.min) = (max, min);
        } else {
            // A column item that is not stretched is as wide as its text, as for an element.
            if it.align != K::Stretch {
                it.width = max.min(min.max(space.width));
            }
            let w = it.width;
            lay(cx, container, &mut it, space, w, None);
            (it.base, it.min) = (it.content, it.content);
        }
        it.hypo = it.base.max(it.min);
        return it;
    }
    let tree = cx.tree;
    let style = tree.style(node);
    if tree.kind(node) == Kind::ListItem {
        it.number = tree.list_number(counter, node);
    }
    it.order = number(style, P::Order, 0.0) as i32;
    it.grow = number(style, P::FlexGrow, 0.0).max(0.0);
    it.shrink = number(style, P::FlexShrink, 1.0).max(0.0);
    it.align = match keyword(style, P::AlignSelf) {
        K::Auto => align_items,
        K::Normal => K::Stretch,
        k => k,
    };
    let (border, padding) = border_padding(cx, style, space.width);
    let margin = |p| resolve(cx, style.get(p), Some(space.width));
    let auto = |p| style.get(p) == Value::Keyword(K::Auto);
    let m = |p| if auto(p) { None } else { Some(margin(p).unwrap_or(0.0)) };
    let (bpx, bpy) = (border.horizontal() + padding.horizontal(), border.vertical() + padding.vertical());
    let border_box = border_box_of(style);
    let (size_p, min_p, max_p, bp_main, main_base) =
        if row { (P::Width, P::MinWidth, P::MaxWidth, bpx, Some(space.width)) } else { (P::Height, P::MinHeight, P::MaxHeight, bpy, space.height) };
    let content = |v: f32| if border_box { (v - bp_main).max(0.0) } else { v };
    if row {
        it.margins = [m(P::MarginLeft), m(P::MarginRight), m(P::MarginTop), m(P::MarginBottom)];
        (it.bp_main, it.bp_cross) = (bpx, bpy);
    } else {
        it.margins = [m(P::MarginTop), m(P::MarginBottom), m(P::MarginLeft), m(P::MarginRight)];
        (it.bp_main, it.bp_cross) = (bpy, bpx);
        // The width of a column item: stretched to the container, or as wide as its content. With more than one line,
        // it stretches to its line, once the lines are known.
        let cross_auto = it.margins[2].is_none() || it.margins[3].is_none();
        let (w, _, _) = block::widths(cx, style, space.width, bpx);
        it.width = if auto(P::Width) && (it.align != K::Stretch || cross_auto || wrap) {
            let (min, max) = intrinsic::sizes(cx, node);
            let room = space.width - it.margin(2) - it.margin(3) - bpx;
            let cb = |v: f32| if border_box { (v - bpx).max(0.0) } else { v };
            let lo = resolve(cx, style.get(P::MinWidth), Some(space.width)).map_or(0.0, cb);
            let hi = resolve(cx, style.get(P::MaxWidth), Some(space.width)).map_or(f32::INFINITY, cb);
            max.min(min.max(room)).min(hi).max(lo)
        } else {
            w
        };
        let w = it.width;
        lay(cx, container, &mut it, space, w, None);
    }
    let size = resolve(cx, style.get(size_p), main_base).map(content);
    it.max = resolve(cx, style.get(max_p), main_base).map_or(f32::INFINITY, content);
    // The content size: the max-content width of a row item, the height of a column item laid out at its width.
    let mut measured = (!row).then_some((it.content, it.content));
    let mut measure = |cx: &mut Cx| *measured.get_or_insert_with(|| intrinsic::sizes(cx, node));
    let basis = match style.get(P::FlexBasis) {
        Value::Keyword(K::Content) => None,
        Value::Keyword(K::Auto) => size,
        v => resolve(cx, v, main_base).map(content).or(size),
    };
    it.base = basis.unwrap_or_else(|| measure(cx).1);
    it.min = match resolve(cx, style.get(min_p), main_base) {
        Some(v) => content(v),
        // The automatic minimum size (4.5): the min-content size, but not more than `width` or `max-width`. A scroll
        // container can be as small as it likes.
        None if !tree.scrolls(node) => size.map_or(f32::INFINITY, |s| s).min(measure(cx).0).min(it.max),
        None => 0.0,
    };
    it.hypo = it.base.min(it.max).max(it.min);
    it
}

/// Lay out an item at the end of `cx.out`, with a content width `w`, and a content height `tall` if it is known.
fn lay(cx: &mut Cx, container: NodeId, it: &mut Item, space: Space, w: f32, tall: Option<f32>) {
    it.from = cx.out.len();
    let (unknown, waiting) = (cx.unknown.get(), cx.waiting);
    if let Some(last) = it.last {
        let lines = inline::lines(cx, container, it.node, last, w, (0.0, 0.0));
        (it.cross, it.baseline, it.content) = (lines.height, lines.baseline, lines.height);
        if it.align == K::Baseline {
            it.first = first_of(cx, it, &cx.out, it.from, cx.out.len(), u32::MAX);
        }
    } else {
        cx.number = it.number;
        // The margins are placed by the container: the box starts at 0.
        let done = cx.lay_item(it.node, space, (w, 0.0, 0.0), tall, &mut it.laid);
        cx.out[done.fragment].rect.x = 0.0;
        // A scroll container keeps the baseline of its content, but not below its bottom edge.
        (it.cross, it.baseline, it.content) = (done.height, done.baseline.map(|b| block::clamp_scroller(cx, it.node, true, b, done.height)), done.content);
        if it.align == K::Baseline {
            it.first = first_of(cx, it, &cx.out, it.from, cx.out.len(), u32::MAX);
        }
        let tree = cx.tree;
        it.rigid = cx.unknown.get() == unknown
            && block::context(cx, it.node) == block::Context::Flow
            && !tree.scrolls(it.node)
            && tree.position(it.node) == K::Static
            && tree.style(it.node).get(P::Height) == Value::Keyword(K::Auto);
    }
    it.to = cx.out.len();
    it.waits = cx.waiting - waiting;
}

/// The baseline of the first line of an item laid out in `out[at..end]` (`block::first_baseline`), read from `limit` fragments.
fn first_of(cx: &Cx, it: &Item, out: &[Fragment], at: usize, end: usize, limit: u32) -> Option<f32> {
    block::first_baseline(cx, it.node, it.last.is_none(), out, at, end, it.baseline, it.cross, limit)
}

/// Give a laid-out item a new content height: only its box changes if its content does not depend on it, else it is
/// laid out again. For a column item, `w` is its width and `h` its main size; for a row item, `w` its main size and
/// `h` its stretched height.
/// `bp`: the borders and padding on the top and bottom. Text lines (an anonymous item) do not depend on the height.
#[allow(clippy::too_many_arguments)]
fn resize(cx: &mut Cx, container: NodeId, it: &mut Item, space: Space, w: f32, h: f32, bp: f32) {
    if it.last.is_some() {
        it.cross = h;
        return;
    }
    if it.rigid {
        it.laid = None;
        let f = match &mut it.moved {
            Some(list) => &mut list[0],
            None => &mut cx.out[it.from],
        };
        f.rect.h = h + bp;
        it.cross = h + bp;
        return;
    }
    relayout(cx, container, it, space, w, Some(h));
}

/// Lay out an item again, with a new size, on a list of its own: the items after it are in `cx.out` already.
fn relayout(cx: &mut Cx, container: NodeId, it: &mut Item, space: Space, w: f32, tall: Option<f32>) {
    // The static positions in the old fragments wait no more.
    cx.waiting -= it.waits;
    // The fragments of the first layout are thrown away: that is work (`Cx::work`).
    cx.work += it.moved.as_ref().map_or(it.to - it.from, Vec::len) as u64;
    if let Some(laid) = it.laid.take() {
        memo::keep(&mut cx.memo, &laid, it.moved.as_deref().unwrap_or(&cx.out[it.from..it.to]));
    }
    let mut list = it.moved.take().unwrap_or_default();
    list.clear();
    let saved = std::mem::replace(&mut cx.out, list);
    lay(cx, container, it, space, w, tall);
    it.moved = Some(std::mem::replace(&mut cx.out, saved));
}

/// The min-content and max-content widths of the content box of a flex container: for a row, the items side by side
/// (with `wrap`, the min is the widest item alone); for a column, the widest item.
pub(crate) fn sizes(cx: &mut Cx, node: NodeId) -> (f32, f32) {
    let style = cx.tree.style(node);
    let row = matches!(keyword(style, P::FlexDirection), K::Row | K::RowReverse);
    let wrap = keyword(style, P::FlexWrap) != K::Nowrap;
    let gap = resolve(cx, style.get(P::ColumnGap), Some(0.0)).unwrap_or(0.0).max(0.0);
    let (mut min, mut max, mut n) = (0.0f32, 0.0f32, 0);
    for (child, last) in children(cx, node, |_, _| {}) {
        let (a, b) = match last {
            Some(last) => intrinsic::lines(cx, child, last),
            None => intrinsic::outer(cx, child),
        };
        n += 1;
        if row {
            min = if wrap { min.max(a) } else { min + a };
            max += b;
        } else {
            (min, max) = (min.max(a), max.max(b));
        }
    }
    if row && n > 1 {
        let gaps = gap * (n - 1) as f32;
        max += gaps;
        if !wrap {
            min += gaps;
        }
    }
    (min, max)
}

#[cfg(test)]
mod tests {
    use crate::block::tests::{dump, rect};

    const CSS: &str = "body { line-height: 20px } .f { display: flex; width: 400px }";

    #[test]
    fn a_column_item_shrinks_to_its_content_not_to_its_height() {
        // Both are 100 tall in 60: they shrink alike to 30. The automatic minimum is the content (20), not `height`.
        let html = "<div class=f style='flex-direction:column;height:60px'><div id=a style='height:100px'>a</div><div id=b style='height:100px'>b</div></div>";
        assert_eq!(rect(html, CSS, "a").h, 30.0);
        assert_eq!(rect(html, CSS, "b").y, 8.0 + 30.0);
    }

    #[test]
    fn items_of_a_wrapped_column_stretch_to_their_line() {
        // Two lines of columns: each line is as wide as its widest item ("a" is 9.6), not the container.
        let html = "<div class=f style='flex-flow:column wrap;height:50px;align-content:flex-start'><div id=a style='height:30px'>a</div><div id=b style='height:30px'>bb</div></div>";
        assert_eq!(rect(html, CSS, "a").w, 9.6);
        assert_eq!(rect(html, CSS, "b").x, 8.0 + 9.6);
        // `align-content: normal` shares the 371.2 that is left between the two lines.
        let html = html.replace("align-content:flex-start", "");
        assert!((rect(&html, CSS, "a").w - (9.6 + 185.6)).abs() < 0.01);
    }
    #[test]
    fn a_positioned_item_that_stretches_is_laid_out_again() {
        // The first item is a containing block, so it is laid out again at the height of the line. Its absolute box
        // was placed in the first layout: the second one must not count it twice.
        let html = "<div class=f><p id=p style='position:relative;margin:0'><i id=a style='position:absolute;bottom:0'>x</i>a</p><p style='margin:0'>b<br>b</p></div>";
        assert_eq!(rect(html, CSS, "p").h, 40.0);
        assert_eq!(rect(html, CSS, "a").y, 8.0 + 40.0 - 20.0);
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.01
    }

    #[test]
    fn grow_shares_the_free_space() {
        // `flex: 1` and `flex: 2` start at 0 and share the 350 that `w50` leaves: 116.67 and 233.33.
        let html = "<div class=f><p id=a style='flex:1;margin:0'>a</p><p id=b style='flex:2;margin:0'>b</p><p id=c style='width:50px;margin:0'>c</p></div>";
        assert!(close(rect(html, CSS, "a").w, 350.0 / 3.0));
        assert!(close(rect(html, CSS, "b").x, 8.0 + 350.0 / 3.0));
        assert_eq!(rect(html, CSS, "c").x, 8.0 + 350.0);
    }

    #[test]
    fn a_flex_factor_over_the_range_of_f32_ends_and_shares_as_chrome() {
        // `1e39` is 3.4e38 (the largest f32). Their sum is over the range: the shares must still be exact, and the loop must end.
        // Chrome 155 (`tools/fixtures/ui/flexhuge-cases.txt`): 150 and 150; 280.8 and 19.2 (the minimum width of "bb"); 100 each.
        let run = |html: &'static str, id: &'static str| {
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || tx.send(rect(html, CSS, id)).ok());
            rx.recv_timeout(std::time::Duration::from_secs(5)).expect("layout did not end")
        };
        let shrink = "<div class=f style='width:300px'><p id=a style='flex:0 1e39 100px;margin:0'>aa</p><p id=b style='flex:0 1e39 400px;margin:0'>bb</p></div>";
        assert!(close(run(shrink, "a").w, 60.0) && close(run(shrink, "b").w, 240.0));
        let two = "<div class=f style='width:300px'><p id=a style='flex:1e39 0 0;margin:0'>aa</p><p id=b style='flex:1e39 0 0;margin:0'>bb</p></div>";
        assert!(close(run(two, "a").w, 150.0) && close(run(two, "b").w, 150.0));
        let one = "<div class=f style='width:300px'><p id=a style='flex:1e39 0 0;margin:0'>aa</p><p id=b style='flex:1 0 0;margin:0'>bb</p></div>";
        assert!(close(run(one, "b").w, 19.2) && close(run(one, "a").w, 300.0 - 19.2));
        let three = "<div class=f style='width:300px'><p id=a style='flex:3.4e38 0 0;margin:0'>a</p><p id=b style='flex:3.4e38 0 0;margin:0'>b</p><p id=c style='flex:3.4e38 0 0;margin:0'>c</p></div>";
        assert!(close(run(three, "a").w, 100.0) && close(run(three, "c").x, 8.0 + 200.0));
    }

    #[test]
    fn a_column_run_of_text_is_as_wide_as_its_text_unless_it_stretches() {
        // Chrome 155 (`tools/fixtures/ui/anonalign-cases.txt`): the letter (9.6 wide) of a 200 wide column is centered, or at the end.
        let at = |css: &str| {
            let out = dump("<div id=g>S</div>", &format!("#g {{ display: flex; flex-direction: column; width: 200px; height: 100px; {css} }}"));
            out.lines().find(|l| l.contains('"')).unwrap().trim().to_string()
        };
        assert_eq!(at("align-items: center"), "\"S\" 103.2,8 9.6x22");
        assert_eq!(at("align-items: flex-end"), "\"S\" 198.4,8 9.6x22");
        assert_eq!(at("align-items: flex-start; text-align: right"), "\"S\" 8,8 9.6x22");
        assert_eq!(at("text-align: right"), "\"S\" 198.4,8 9.6x22");
    }

    #[test]
    fn shrink_is_by_the_base_size() {
        // 200 too much: 300 gives 120 and 200 gives 80. `flex-shrink: 0` keeps its 100.
        let html = "<div class=f><p id=a style='width:300px;margin:0'>a</p><p id=b style='width:200px;margin:0'>b</p><p id=c style='width:100px;flex-shrink:0;margin:0'>c</p></div>";
        assert!(close(rect(html, CSS, "a").w, 180.0));
        assert!(close(rect(html, CSS, "b").w, 120.0));
        assert!(close(rect(html, CSS, "c").w, 100.0));
    }

    #[test]
    fn an_item_does_not_shrink_below_its_longest_word() {
        // A word of 50 letters is 480 wide: wider than the container. `min-width: 0` lets it shrink.
        let word = "a".repeat(50);
        let html = format!("<div class=f><p id=a style='flex:1;margin:0'>{word}</p><p style='flex:1;margin:0'>b</p></div>");
        assert!(close(rect(&html, CSS, "a").w, 480.0));
        let html = html.replace("flex:1;margin:0'>a", "flex:1;min-width:0;margin:0'>a");
        assert!(close(rect(&html, CSS, "a").w, 200.0));
    }

    #[test]
    fn justify_content_and_auto_margins() {
        let items = "<p id=a style='margin:0'>a</p><p style='margin:0'>b</p><p id=c style='margin:0'>c</p>";
        let html = format!("<div class=f style='justify-content:space-between'>{items}</div>");
        assert!(close(rect(&html, CSS, "c").x, 8.0 + 400.0 - 9.6));
        let html = format!("<div class=f style='justify-content:center'>{items}</div>");
        assert!(close(rect(&html, CSS, "a").x, 8.0 + (400.0 - 28.8) / 2.0));
        // An auto margin takes the free space before `justify-content` does.
        let html = "<div class=f style='justify-content:center'><p style='margin:0'>a</p><p id=c style='margin:0 0 0 auto'>c</p></div>";
        assert!(close(rect(html, CSS, "c").x, 8.0 + 400.0 - 9.6));
        // `row-reverse` starts at the right.
        let html = format!("<div class=f style='flex-direction:row-reverse'>{items}</div>");
        assert!(close(rect(&html, CSS, "a").x, 8.0 + 400.0 - 9.6));
    }

    #[test]
    fn items_stretch_to_the_tallest_one() {
        // The line is 40 tall. The first item stretches, the centered one does not.
        let html = "<div class=f><p id=a style='margin:0'>a</p><p style='margin:0'>b<br>b</p><p id=c style='margin:0;align-self:center'>c</p></div>";
        assert_eq!(rect(html, CSS, "a").h, 40.0);
        assert_eq!((rect(html, CSS, "c").y, rect(html, CSS, "c").h), (8.0 + 10.0, 20.0));
    }

    #[test]
    fn wrap_makes_lines() {
        // Two of 150 fit in 400. The last one goes on a second line, and grows to fill it.
        let html = "<div class=f style='flex-wrap:wrap'><p style='width:150px;margin:0'>a</p><p style='width:150px;margin:0'>b</p><p id=c style='width:150px;flex-grow:1;margin:0'>c</p></div>";
        assert_eq!(rect(html, CSS, "c"), crate::fragment::Rect { x: 8.0, y: 8.0 + 20.0, w: 400.0, h: 20.0 });
    }

    #[test]
    fn a_column_grows_into_its_height() {
        let html = "<div class=f style='flex-direction:column;height:150px'><p id=a style='flex:1;margin:0'>a</p><p id=b style='flex:2;margin:0'>b</p><p style='margin:0'>c</p></div>";
        // 150 - 20 for the last one: 43.33 and 86.67.
        assert!(close(rect(html, CSS, "a").h, 130.0 / 3.0));
        assert!(close(rect(html, CSS, "b").y, 8.0 + 130.0 / 3.0));
    }

    #[test]
    fn order_and_anonymous_items() {
        let html = "<div class=f><p id=a style='order:1;margin:0'>a</p><p id=b style='margin:0'>b</p></div>";
        assert_eq!(rect(html, CSS, "a").x, 8.0 + 9.6);
        // The text is an item of its own: "text " is 5 letters, its space at the end hangs.
        let html = "<div class=f>text <b id=b>b</b></div>";
        assert!(close(rect(html, CSS, "b").x, 8.0 + 4.0 * 9.6));
    }

    #[test]
    fn an_inline_flex_container_is_as_wide_as_its_items() {
        let html = "<div><span id=s style='display:inline-flex'><b>abc</b><b>de</b></span></div>";
        assert!(close(rect(html, CSS, "s").w, 5.0 * 9.6));
    }
}
