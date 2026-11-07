//! Layout results kept for the boxes that layout runs again.
//!
//! A flex or grid container lays an item out, learns its height, and lays it out again at the height that `stretch` gives
//! it (`flex.rs`, `grid.rs`). The item is a container too, so its own items are laid out twice in each of the two runs,
//! and so on: the work doubled with each level (measured: 20 nested stretched rows, 43 boxes, 2.6 million layouts).
//!
//! Chrome (`NGLayoutResult` cache), Servo and Taffy keep the result of a box for the space it was laid out in. We do the
//! same for one layout, with two changes that cost nothing for a page that does not need them. The map works only
//! after layout has done `start` units of work again (`Cx::work`: the fragments that a container threw away, and the
//! fragments that the map put back), and then only for an item that has a flex or grid item inside. And a result is
//! kept only when its container is about to throw it away (`keep`), or when the map is not empty. With the first run
//! kept, the second run of the parent finds its children in the map, so each box is laid out once for each distinct
//! height that reaches it, not once for each path down to it. The map holds at most `LIMIT` fragments: past it, the map
//! is emptied and fills again. That costs time and never a wrong result. The results of the boxes of one chain of items
//! need far less (40 stretched rows: 90,000 fragments), and the boxes after the chain do not look at them.
//!
//! A result is a function of the box, its space, its forced width, its height, the call depth, and the list number. It
//! also changes four things outside the fragments, and all four are kept with it and put back on a hit: the boxes
//! out of the flow that wait for a containing block outside the box (`Cx::waiting`), the percents that met an unknown
//! base (`Cx::unknown`), and the offsets and scrollbars that layout wrote by node (`Cx::rel`, `Cx::bars`, in `Cx::fx`).
//! A box with no flex or grid item inside is cheap to run again, so it is not kept: its container's own result is. A box that starts with a list marker waiting reads more state, so it is never kept and never found. A box that has a
//! scroll container inside (`overflow: auto` can run its content again, `block::sized`) reads the scrollbars that the
//! first pass found when it runs inside a scrollbar pass (`Cx::again`): it is kept and found only outside such a pass.

use crate::Cx;
use crate::block::{Done, Space};
use crate::fragment::Fragment;
use bare_html::dom::{NodeData, NodeId};
use std::collections::HashMap;
use std::ops::Range;

/// A thing that layout wrote by node, in the order it wrote it.
#[derive(Clone, Copy)]
pub(crate) enum Effect {
    Rel(u32, (f32, f32)),
    Bars(u32, (bool, bool)),
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct Key {
    node: u32,
    width: u32,
    height: Option<u32>,
    forced: [u32; 3],
    tall: Option<u32>,
    depth: u32,
    number: i32,
}

/// The result of one layout of a box, as `lay_item` made it.
#[derive(Clone)]
pub(crate) struct Laid {
    key: Key,
    done: Done,
    waits: u32,
    unknown: u32,
    fx: Range<usize>,
    /// No box inside could run again for a scrollbar: the result does not depend on `Cx::again` or on `Cx::bars`.
    pure: bool,
}

#[derive(Default)]
pub(crate) struct Memo(HashMap<Key, (Vec<Fragment>, Laid), std::hash::BuildHasherDefault<bare_html::dom::Fnv>>, usize);

/// The most fragments that the map holds: about 32 MB.
const LIMIT: usize = 1 << 20;

/// Keep the result of a layout that its container throws away. `frags` are its fragments, from `laid.done.fragment`.
pub(crate) fn keep(memo: &mut Memo, laid: &Laid, frags: &[Fragment]) {
    let from = laid.done.fragment as u32;
    if memo.1 >= LIMIT {
        memo.0.clear();
        memo.1 = 0;
    }
    memo.1 += frags.len();
    memo.0.entry(laid.key).or_insert_with(|| (frags.iter().map(|f| Fragment { end: f.end - from, ..*f }).collect(), laid.clone()));
}

/// The map starts to work once layout has done this much work again (`Cx::work`). A page that lays a few boxes out twice never
/// pays for it: the map costs about as much as one more layout of a small item. A page that doubles the work at each level
/// reaches the number in less than a millisecond. Tests change it, to compare the map with no map.
#[cfg(not(test))]
pub(crate) const fn start() -> u64 {
    1 << 12
}
#[cfg(test)]
pub(crate) use tests::start;

impl Cx<'_> {
    #[inline(never)]
    pub(crate) fn set_rel(&mut self, node: NodeId, offset: (f32, f32)) {
        self.rel.insert(node.index() as u32, offset);
        self.sets += 1;
        if self.open > 0 {
            self.fx.push(Effect::Rel(node.index() as u32, offset));
        }
    }

    #[inline(always)]
    pub(crate) fn set_bars(&mut self, node: NodeId, need: (bool, bool)) {
        self.bars.insert(node.index() as u32, need);
        self.sets += 1;
        if self.open > 0 {
            self.fx.push(Effect::Bars(node.index() as u32, need));
        }
    }

    fn key(&self, node: NodeId, space: Space, forced: (f32, f32, f32), tall: Option<f32>) -> Key {
        Key {
            node: node.index() as u32,
            width: space.width.to_bits(),
            height: space.height.map(f32::to_bits),
            forced: [forced.0.to_bits(), forced.1.to_bits(), forced.2.to_bits()],
            tall: tall.map(f32::to_bits),
            depth: self.depth as u32,
            number: if self.tree.kind(node) == crate::boxes::Kind::ListItem { self.number } else { 0 },
        }
    }

    /// `block::sized` for a box that is its own formatting context and sits at the origin of its container (a flex or grid
    /// item). The result comes from the map when the box was laid out in this space before. `laid` is what `keep` needs:
    /// `None` for a result that is in the map already, for a box that must not be kept, and for a box with no item inside.
    ///
    /// Until `start`, and for an item with no element inside (it cannot hold another item), this is `block::sized`.
    #[inline(always)]
    pub(crate) fn lay_item(&mut self, node: NodeId, space: Space, forced: (f32, f32, f32), tall: Option<f32>, laid: &mut Option<Box<Laid>>) -> Done {
        if self.work < self.memo_from {
            *laid = None;
            return crate::block::sized(self, node, space, Some(forced), tall, crate::block::Pos::default());
        }
        self.lay_nested(node, space, forced, tall, laid)
    }

    #[inline(never)]
    fn lay_nested(&mut self, node: NodeId, space: Space, forced: (f32, f32, f32), tall: Option<f32>, laid: &mut Option<Box<Laid>>) -> Done {
        let doc = self.tree.doc;
        if !doc.children(node).any(|c| matches!(doc.data(c), NodeData::Element { .. })) {
            *laid = None;
            return crate::block::sized(self, node, space, Some(forced), tall, crate::block::Pos::default());
        }
        let usable = self.marker.is_none();
        if usable && !self.memo.0.is_empty() && let Some(done) = self.replay(node, space, forced, tall) {
            *laid = None;
            return done;
        }
        let (fx, waiting, unknown, scrolls, items) = (self.fx.len(), self.waiting, self.unknown.get(), self.scrolls, self.items);
        self.items += 1;
        self.open += 1;
        // Over the budget, the box is laid out but not its content.
        let depth = self.depth;
        if self.work > self.budget {
            self.depth = crate::block::MAX_DEPTH;
        }
        let done = crate::block::sized(self, node, space, Some(forced), tall, crate::block::Pos::default());
        self.depth = depth;
        self.open -= 1;
        *laid = None;
        if usable && self.items - items > 1 {
            self.record(node, space, forced, tall, done, (fx, waiting, unknown, scrolls), laid);
        }
        done
    }

    /// Put back the result of a box that was laid out in this space before.
    #[inline(never)]
    fn replay(&mut self, node: NodeId, space: Space, forced: (f32, f32, f32), tall: Option<f32>) -> Option<Done> {
        let (frags, laid) = self.memo.0.get(&self.key(node, space, forced, tall))?;
        if !(laid.pure || self.again == 0) {
            return None;
        }
        let from = self.out.len();
        self.out.extend(frags.iter().map(|f| Fragment { end: f.end + from as u32, ..*f }));
        self.work += frags.len() as u64;
        self.waiting += laid.waits;
        self.unknown.set(self.unknown.get() + laid.unknown);
        for i in laid.fx.clone() {
            let effect = self.fx[i];
            match effect {
                Effect::Rel(n, o) => drop(self.rel.insert(n, o)),
                Effect::Bars(n, b) => drop(self.bars.insert(n, b)),
            }
            if self.open > 0 {
                self.fx.push(effect);
            }
        }
        // This item and one inside it count as laid out, so that the box around this one is not taken for a leaf.
        self.items += 2;
        Some(Done { fragment: from, ..laid.done })
    }

    /// Note the result of a box with an item inside, for `keep`.
    #[inline(never)]
    #[allow(clippy::too_many_arguments)]
    fn record(&mut self, node: NodeId, space: Space, forced: (f32, f32, f32), tall: Option<f32>, done: Done, before: (usize, u32, u32, u32), laid: &mut Option<Box<Laid>>) {
        let pure = self.scrolls == before.3;
        if !(pure || self.again == 0) {
            return;
        }
        let made = Box::new(Laid { key: self.key(node, space, forced, tall), done, waits: self.waiting - before.1, unknown: self.unknown.get() - before.2, fx: before.0..self.fx.len(), pure });
        if !self.memo.0.is_empty() {
            keep(&mut self.memo, &made, &self.out[done.fragment..]);
        } else {
            *laid = Some(made);
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use crate::block::tests::run;
    use std::cell::Cell;

    thread_local! {
        static START: Cell<u64> = const { Cell::new(0) };
    }

    /// The work from which the map works, for the test of this thread: from the start, unless the test says else.
    pub(crate) fn start() -> u64 {
        START.with(Cell::get)
    }

    const CSS: &str = "body { margin: 0 } .r { display: flex } .g { display: grid; grid-template-columns: auto auto } .c { display: flex; flex-direction: column } \
                       .a { overflow: auto; max-height: 50px } .p { position: relative; left: 5%; top: 3px } .q { position: relative } .z { position: absolute; bottom: 0; right: 0 }";

    /// `n` boxes inside one another. Each is `open`, then `wrap` around the next box (or `inner`), then a sibling that is
    /// taller the nearer the box is to the outside, so each box is stretched, and then laid out again.
    fn stretched(open: &str, wrap: (&str, &str), n: usize, inner: &str) -> String {
        let mut s = String::new();
        for _ in 0..n {
            s += open;
            s += wrap.0;
        }
        s += inner;
        for k in (0..n).rev() {
            s += &format!("{}<div style='height:{}px'></div></div>", wrap.1, 10 + 20 * (n - 1 - k));
        }
        s
    }

    fn work(html: &str) -> u64 {
        run(html, CSS).1.work
    }

    /// The same page with the map on and off: the fragments and the scrollbars must be the same.
    fn same(html: &str) {
        let on = run(html, CSS);
        START.with(|s| s.set(u64::MAX));
        let off = run(html, CSS);
        START.with(|s| s.set(0));
        assert_eq!(crate::dump(&on.0, &on.1), crate::dump(&off.0, &off.1), "{html}");
        assert_eq!(on.1.bars, off.1.bars, "{html}");
    }

    #[test]
    fn the_map_does_not_change_a_layout() {
        let none = ("", "");
        for n in [1, 2, 5] {
            same(&stretched("<div class=r>", none, n, "<i id=t>text</i>"));
            same(&stretched("<div class=g>", none, n, "<i id=t>text</i>"));
            same(&stretched("<div class=r>", ("<div class=c>", "</div>"), n, "<i>a</i><i>b</i>"));
            same(&stretched("<div class='r p'>", none, n, "<i id=t>text</i>"));
            same(&stretched("<div class=r style='align-items:center'>", none, n, "<ol><li>x<li>y</ol>"));
            same(&stretched("<div class=r>", ("<div class=a>", "</div>"), n, "<p>a</p><p>b</p><p>c</p><p>d</p>"));
            same(&stretched("<div class=r>", ("<div class=q><b class=z>x</b>", "</div>"), n, "<i>t</i>"));
            same(&stretched("<div class=g>", ("<ul><li>", "</ul>"), n, "<table><tr><td>a<td>b</table>"));
            same(&stretched("<div class=r style='flex-wrap:wrap'>", ("<div class=c style='height:90px;flex-wrap:wrap'>", "</div>"), n, "<i>a</i><i>b</i>"));
        }
        // a flex column that grows its items, in a height
        same(&format!("{}<i>t</i>{}", "<div class=c style='height:300px'><div class=c style='flex:1'>".repeat(4), "</div></div>".repeat(4)));
    }

    #[test]
    fn nested_stretched_boxes_are_laid_out_a_bounded_number_of_times() {
        // Boxes inside one another, a different height at each level. Without the map every level doubles the work: 2^40 layouts
        // at 40 levels. With it each box is laid out once for each height that reaches it, so the work grows with the cube of the
        // levels at most: twice the levels, less than ten times the work (a hit puts back the fragments of its whole subtree, and they count).
        for open in ["<div class=r>", "<div class=g>"] {
            let (half, whole) = (work(&stretched(open, ("", ""), 20, "<i>text</i>")), work(&stretched(open, ("", ""), 40, "<i>text</i>")));
            assert!(half > 0 && whole < 10 * half, "{open} {half} {whole}");
        }
        // one stretched box: one layout again
        assert!(work(&stretched("<div class=r>", ("", ""), 1, "<i>text</i>")) < 10);
    }

    #[test]
    fn the_map_starts_late_and_still_bounds_the_work() {
        // The real start: 4096 units of work again before the map works. 40 nested rows reach it, and then stop doubling.
        START.with(|s| s.set(1 << 12));
        let rows = work(&stretched("<div class=r>", ("", ""), 40, "<i>text</i>"));
        START.with(|s| s.set(0));
        assert!(rows < 1_000_000, "{rows}");
    }

    #[test]
    fn a_budget_stops_layout() {
        let html = stretched("<div class=r>", ("<div class=a style='max-height:300px'>", "</div>"), 30, "<i>text</i>");
        let mut doc = bare_html::tree::parse(&html);
        let mut rules = bare_css::cascade::RuleSet::new((800.0, 600.0));
        rules.add(&mut doc, bare_css::cascade::UA_CSS, bare_css::cascade::Origin::UserAgent);
        rules.add(&mut doc, CSS, bare_css::cascade::Origin::Author);
        let styles = rules.compute(&doc);
        // Nested scroll containers that need a bar run in a pass where the map is not used, so the work doubles for each one.
        let layout = crate::layout_within(&doc, &styles, (800.0, 600.0), 10_000);
        assert!(layout.work > 10_000);
        // It stopped soon after the budget, long before the 2^30 layouts that the page asks for.
        assert!(layout.work < 20_000, "{}", layout.work);
    }
}
