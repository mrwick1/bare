//! `HTMLElement.offsetWidth` and `offsetHeight` over the native cascade (`bare_css::cascade`) and native layout (`bare_layout::layout`).
//!
//! How it works
//! - Every read runs the whole path again: the cascade of `browser_computed.rs` (`Dom::cascade_now`: the same sheets, gates and caps, so a
//!   linked sheet that is not supplied is `Unsupported`), then `bare_layout::layout` with the current viewport, then the answer from the
//!   fragments. The layout is not cached. The cascade is used again only while the document counters and the sheets are the same
//!   (`Dom::cascade_for`). Nothing is stored on the element or in JS data, so a held element sees the current classes, `id`,
//!   `style` attributes, `<style>` text, supplied sheets, viewport, tree and text.
//! - CSSOM View (`offsetWidth`, `offsetHeight`): "if the element does not have any associated box return zero", else the width (height) of the
//!   axis-aligned bounding box of the border boxes of all fragments of the principal box, ignoring transforms. Here: the union of all `Kind::Box`
//!   fragments of the element. An element with no fragment is zero: a detached or not connected element, `display:none` (and everything below it,
//!   where the engine has no style at all), and a box that layout does not make. Native layout has no transforms, so nothing is ignored by hand.
//! - The spec gives no integer rule. The value is a `long`; like Chrome and Firefox it is the nearest integer, halves up. A value that is not
//!   finite or does not fit a `long` is `Unsupported` (layout clamps lengths to 33554431px, so this does not happen for sane pages).
//! - Brand: `this` must be a non-foreign element (`HTMLElement`), else `TypeError: Illegal invocation`. The accessors are read-only.
//!
//! Gates (`Error::Unsupported`, none is faked)
//! - An element in a connected shadow tree (`Dom::in_document`). A visible host is laid out from its flat tree (`Document::flat_children`), but
//!   only when its shadow tree has no `<slot>` (slot assignment is not built) and no `<style>` or `<link>` (shadow sheets are not scoped).
//! - A missing external sheet, as in `browser_computed.rs`.
//! - An inline box (or `<br>`) with no background and no border: native layout "culls" it and makes no fragment, so its size is not known.
//! - `display: contents`, `writing-mode`, vertical text and `direction` positioning are not in the engine, so they are not modelled.
//!
//! Bound (`GeometryLimits`, `Limit(Host(name))`, never catchable). Native layout recurses and cannot be interrupted. So `Dom::preflight` walks the
//! visible light tree once, without recursion, before layout starts, and refuses or reserves:
//! - `geometry-nodes`: visible elements and text nodes. `geometry-depth`: tree depth (layout recurses about 3 native frames per level; the
//!   engine's own ceiling is `block::MAX_DEPTH` = 400, and the default here is far under it: measured with `stack_probe` in `browser_geometry_tests.rs`: depth 96 of every shape runs on 1 MiB in release and 4 MiB in debug).
//!   `geometry-text`: bytes of visible text.
//! - `geometry-fragments`: an upper bound of the fragments layout can allocate. A block-level element makes 1; a list item 1 more (the marker); a
//!   text node at most one per byte (one per line, a line holds a byte at least); an inline element at most one per line, and a line of it holds
//!   at least one unit of its subtree, so at most `1 + units(subtree)` (units: nodes and bytes of text). Layout writes into one `Vec` (capacity
//!   = the node count) and copies finished inline-blocks and flex or grid items once into it, so memory is about `bound * 40` bytes.
//! - `geometry-work`: units of layout work done again, counted by layout itself while it runs (`bare_layout::Cx::work`: one for each
//!   fragment that a flex or grid container or a scrollbar pass threw away to lay a box out again, and one for each fragment that a kept
//!   result put back). A page that lays nothing out twice has none, and its own cost is bounded by the limits above. Layout stops going
//!   deeper when it passes `max_work`, so its time is bounded without a guess of how often a box runs again: `bare_layout::layout_within` returns, the
//!   result is thrown away, and the read stops the page. What layout did is then charged as steps. There is no limit on how deep
//!   boxes that run again are nested: a flex or grid item that stretches is laid out again, but layout keeps the result of each box
//!   for each space it was laid out in (`layout/src/memo.rs`), so the work is about one layout for each box for each distinct height
//!   that reaches it (measured in `docs/resources.md`, "Layout work of geometry reads"), not a factor for each level of nesting.
//!   Only boxes inside a scroll container that needs a scrollbar can still grow by a factor for each level; `max_work` stops those.
//!
//! Not done: `getBoundingClientRect`, `offsetParent`, `offsetLeft`, `offsetTop`, `ResizeObserver`, a cache, slots, shadow sheets, transformed geometry.

use crate::browser::*;
use crate::browser_platform::V;
use bare_css::cascade::Styles;
use bare_css::values::{Keyword, Property, Value as Cv};
use bare_html::dom::{Children, Document, NodeData, NodeId};
use bare_js::eval::Interp;
use bare_js::value::Value;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;

/// Bounds of the geometry module (see the header). Each is checked before native layout starts.
#[derive(Clone, Copy, Debug)]
pub struct GeometryLimits {
    /// Visible elements and text nodes. `geometry-nodes`.
    pub max_nodes: usize,
    /// Depth of the visible tree. `geometry-depth`.
    pub max_depth: usize,
    /// Bytes of visible text. `geometry-text`.
    pub max_text: usize,
    /// Upper bound of the fragments of one layout. `geometry-fragments`.
    pub max_fragments: u64,
    /// Units of layout work (see the header). `geometry-work`.
    pub max_work: u64,
}

impl Default for GeometryLimits {
    fn default() -> Self {
        GeometryLimits { max_nodes: 1 << 14, max_depth: 96, max_text: 1 << 16, max_fragments: 1 << 17, max_work: 1 << 20 }
    }
}

/// Counters for tests and the load tool.
#[derive(Clone, Copy, Debug, Default)]
pub struct GeometryStats {
    /// Native layouts run.
    pub layouts: u64,
    /// The fragment bound of the last layout, and the fragments it made.
    pub last_bound: u64,
    pub last_fragments: u64,
    /// The work of the last layout, in units (see the header).
    pub last_work: u64,
}

#[derive(Default)]
pub(crate) struct Geometry {
    lim: Cell<GeometryLimits>,
    stats: Cell<GeometryStats>,
    /// The grid container whose used tracks the next layout keeps (`getComputedStyle`, `browser_used.rs`).
    pub(crate) probe: Cell<Option<NodeId>>,
    /// The next layout gives each inline box a fragment (`getClientRects` of an empty `span` and of a `br`, `browser_rects.rs`).
    pub(crate) uncull: Cell<bool>,
    /// The counts of the last window walk.
    sums: RefCell<Sums>,
}

const SLOT: &str = "geometry of a shadow tree with a slot (slot assignment is not built)";
const SHADOW_SHEET: &str = "geometry of a shadow tree with a style sheet (shadow style scoping is not built)";
const CULLED: &str = "geometry of an inline box with no background and no border (native layout makes no fragment for it)";
const NOT_LONG: &str = "geometry that is not a finite number that fits a long";

/// What the walk found.
#[derive(Debug, PartialEq)]
struct Bound {
    fragments: u64,
    /// Nodes and bytes of text: what the walk looked at.
    seen: u64,
}

struct Frame<'a> {
    kids: Children<'a>,
    units: u64,
    inline: bool,
}

enum Refuse {
    Limit(&'static str),
    Gate(&'static str),
}

/// A visible host is laid out from its flat tree: the children of its shadow root. Only the shadow trees that need nothing more are
/// supported: one with a `<slot>` needs slot assignment, and one with a `<style>` or `<link>` needs scoped shadow sheets. Every node of
/// the shadow tree is looked at, hidden ones too (a hidden `<style>` still applies). The caller charged `doc.len()`, which covers this.
fn check_shadow(doc: &Document, host: NodeId) -> Result<(), Refuse> {
    let Some(root) = doc.shadow_root(host) else { return Ok(()) };
    let names = |n: &str| doc.find_atom(n);
    let (slot, style, link) = (names("slot"), names("style"), names("link"));
    let mut stack: Vec<NodeId> = doc.children(root).collect();
    while let Some(n) = stack.pop() {
        if let NodeData::Element { name, .. } = *doc.data(n) {
            if Some(name) == slot {
                return Err(Refuse::Gate(SLOT));
            }
            if Some(name) == style || Some(name) == link {
                return Err(Refuse::Gate(SHADOW_SHEET));
            }
        }
        // A nested host is checked when the walk reaches it.
        stack.extend(doc.children(n));
    }
    Ok(())
}

/// What the walk counted in the subtree of one element: what it adds to the totals, the units it gives its parent, and how deep it is.
#[derive(Clone, Copy)]
struct Sum {
    /// The window walk that counted it or used it last.
    walk: u64,
    nodes: usize,
    text: usize,
    frags: u64,
    units: u64,
    depth: usize,
}

/// The counts of the big subtrees (`BIG` nodes or more) of the last window walk, by `NodeId::index`. The next window walk is told
/// which nodes changed (the changed nodes and their ancestors): it takes the counts of a big subtree that did not change from
/// here, and does not walk it. A count is good for the next walk only, so a subtree that was hidden or out of the tree is walked again.
#[derive(Default)]
pub(crate) struct Sums {
    walk: u64,
    map: HashMap<u32, Sum>,
    /// The subtrees that were not walked, for tests.
    used: u64,
}

const BIG: usize = 64;

/// One walk of the visible tree from the root element, without recursion. Layout reaches the same nodes and no others.
/// `KEEP`: a window walk, that keeps the counts of the big subtrees in `sums`, and uses them where `dirty` (if any) says no node changed.
fn walk<const KEEP: bool>(doc: &Document, styles: &Styles, l: &GeometryLimits, sums: Option<&mut Sums>, dirty: Option<&[bool]>) -> Result<Bound, Refuse> {
    let Some(root) = doc.children(doc.root()).find(|&n| matches!(doc.data(n), NodeData::Element { .. })) else {
        return Ok(Bound { fragments: 0, seen: 0 });
    };
    let (mut nodes, mut text, mut frags) = (0usize, 0usize, 0u64);
    let br = doc.find_atom("br");
    let mut stack: Vec<Frame> = Vec::new();
    // With `KEEP`, beside `stack`: the element, the totals before it, and the depth of the stack before it and deepest under it.
    let mut opens: Vec<(NodeId, usize, usize, u64, usize, usize)> = Vec::new();
    let mut sums = sums.filter(|_| KEEP);
    if let Some(s) = sums.as_deref_mut() {
        s.walk += 1;
        if dirty.is_none() {
            s.map.clear();
        }
    }
    let mut next = Some(root);
    loop {
        // Open `next` (an element), or look at the next child of the open element.
        let node = match next.take() {
            Some(n) => n,
            None => match stack.last_mut() {
                None => break,
                Some(top) => match top.kids.next() {
                    Some(c) => c,
                    None => {
                        let f = stack.pop().expect("open");
                        let units = f.units + 1;
                        frags += if f.inline { units } else { 1 };
                        if let Some(parent) = stack.last_mut() {
                            parent.units += units;
                        }
                        if frags > l.max_fragments {
                            return Err(Refuse::Limit("geometry-fragments"));
                        }
                        if KEEP && let Some((el, n0, t0, f0, d0, deep)) = opens.pop() {
                            if let Some(up) = opens.last_mut() {
                                up.5 = up.5.max(deep);
                            }
                            if nodes - n0 >= BIG && let Some(s) = sums.as_deref_mut() {
                                let walk = s.walk;
                                s.map.insert(el.index() as u32, Sum { walk, nodes: nodes - n0, text: text - t0, frags: frags - f0, units, depth: deep - d0 });
                            }
                        }
                        continue;
                    }
                },
            },
        };
        match *doc.data(node) {
            NodeData::Element { name, .. } => {
                let Some(style) = styles.style(node) else { continue };
                if style.get(Property::Display) == Cv::Keyword(Keyword::None) {
                    continue;
                }
                // A big subtree with no change since the last window walk: its counts, not a walk.
                if KEEP
                    && let (Some(d), Some(s)) = (dirty, sums.as_deref_mut())
                    && !d.get(node.index()).copied().unwrap_or(true)
                    && let Some(sum) = s.map.get_mut(&(node.index() as u32)).filter(|c| c.walk + 1 == s.walk)
                {
                    sum.walk = s.walk;
                    s.used += 1;
                    (nodes, text, frags) = (nodes + sum.nodes, text + sum.text, frags + sum.frags);
                    if let Some(parent) = stack.last_mut() {
                        parent.units += sum.units;
                    }
                    if let Some(up) = opens.last_mut() {
                        up.5 = up.5.max(stack.len() + sum.depth);
                    }
                    if stack.len() + sum.depth > l.max_depth {
                        return Err(Refuse::Limit("geometry-depth"));
                    }
                    if nodes > l.max_nodes {
                        return Err(Refuse::Limit("geometry-nodes"));
                    }
                    if text > l.max_text {
                        return Err(Refuse::Limit("geometry-text"));
                    }
                    if frags > l.max_fragments {
                        return Err(Refuse::Limit("geometry-fragments"));
                    }
                    continue;
                }
                check_shadow(doc, node)?;
                nodes += 1;
                if nodes > l.max_nodes {
                    return Err(Refuse::Limit("geometry-nodes"));
                }
                if stack.len() + 1 > l.max_depth {
                    return Err(Refuse::Limit("geometry-depth"));
                }
                if style.get(Property::Display) == Cv::Keyword(Keyword::ListItem) {
                    frags += 1;
                }
                let inline = style.get(Property::Display) == Cv::Keyword(Keyword::Inline) || Some(name) == br;
                if KEEP {
                    // The counts before this element: its own node and list marker are in its subtree.
                    let marker = (style.get(Property::Display) == Cv::Keyword(Keyword::ListItem)) as u64;
                    opens.push((node, nodes - 1, text, frags - marker, stack.len(), stack.len() + 1));
                }
                stack.push(Frame { kids: doc.flat_children(node), units: 0, inline });
            }
            NodeData::Text(span) => {
                let Some(top) = stack.last_mut() else { continue };
                let bytes = doc.text(span).len();
                nodes += 1;
                text += bytes;
                if nodes > l.max_nodes {
                    return Err(Refuse::Limit("geometry-nodes"));
                }
                if text > l.max_text {
                    return Err(Refuse::Limit("geometry-text"));
                }
                frags += bytes as u64;
                top.units += bytes as u64 + 1;
            }
            _ => {}
        }
        if frags > l.max_fragments {
            return Err(Refuse::Limit("geometry-fragments"));
        }
    }
    Ok(Bound { fragments: frags, seen: (nodes + text) as u64 })
}

impl Dom {
    /// One bounded native layout of the whole page with these styles: the preflight walk refuses or reserves the work first (see the header).
    /// `offsetWidth`, `offsetHeight` and `ResizeObserver` share it.
    pub(crate) fn layout_now(&self, it: &mut Interp<'_>, styles: &Styles) -> R<bare_layout::fragment::Layout> {
        self.layout_areas(it, styles, None).map(|(l, _, _)| l)
    }

    /// One bounded native layout at the size `vp` (the window minus the page bars, see `browser_scroll.rs`).
    pub(crate) fn layout_at(&self, it: &mut Interp<'_>, styles: &Styles, vp: (f32, f32), keep: Option<(Option<bare_layout::keep::Kept>, Option<&[bool]>)>) -> R<bare_layout::fragment::Layout> {
        let l = self.geometry.lim.get();
        let doc = self.doc.borrow();
        it.charge(doc.len() as u64)?;
        let walked = match &keep {
            Some((_, dirty)) => walk::<true>(&doc, styles, &l, Some(&mut self.geometry.sums.borrow_mut()), *dirty),
            None => walk::<false>(&doc, styles, &l, None, None),
        };
        let bound = match walked {
            Ok(b) => b,
            Err(Refuse::Limit(name)) => return Err(self.lim(name)),
            Err(Refuse::Gate(why)) => return Err(Self::gate(why)),
        };
        // Layout cannot be interrupted, so it counts its own work and stops going deeper at `max_work`.
        it.charge(bound.seen + 1)?;
        let probe = self.geometry.probe.get();
        let layout = if self.geometry.uncull.get() {
            bare_layout::layout_uncull(&doc, styles, vp, l.max_work)
        } else if let Some((kept, dirty)) = keep.filter(|_| probe.is_none()) {
            bare_layout::layout_kept(&doc, styles, vp, l.max_work, kept, dirty)
        } else {
            bare_layout::layout_probing(&doc, styles, vp, l.max_work, probe)
        };
        if layout.work > l.max_work {
            return Err(self.lim("geometry-work"));
        }
        it.charge(layout.work)?;
        let mut s = self.geometry.stats.get();
        s = GeometryStats { layouts: s.layouts + 1, last_bound: bound.fragments, last_fragments: layout.fragments.len() as u64, last_work: layout.work };
        self.geometry.stats.set(s);
        Ok(layout)
    }

    /// The border-box size of `el` in whole CSS pixels (width, height), zero when it has no box.
    fn offset_size(&self, it: &mut Interp<'_>, el: NodeId) -> R<(i32, i32)> {
        if !self.in_document(it, el)? {
            return Ok((0, 0));
        }
        let styles = self.cascade_now(it)?;
        if styles.style(el).is_none_or(|s| s.get(Property::Display) == Cv::Keyword(Keyword::None)) {
            return Ok((0, 0));
        }
        let layout = self.layout_now(it, &styles)?;
        let Some(r) = border_box(&layout, el) else {
            // Layout makes no fragment for an inline box with no background and no border (`inline.rs`, "culled"): that is not "no box".
            let doc = self.doc.borrow();
            return match bare_layout::boxes::Tree::new(&doc, &styles).kind(el) {
                bare_layout::boxes::Kind::Inline | bare_layout::boxes::Kind::Break => Err(Self::gate(CULLED)),
                _ => Ok((0, 0)),
            };
        };
        let long = |v: f32| {
            let r = (v as f64 + 0.5).floor();
            (v.is_finite() && (0.0..=i32::MAX as f64).contains(&r)).then_some(r as i32)
        };
        match (long(r.w), long(r.h)) {
            (Some(w), Some(h)) => Ok((w, h)),
            _ => Err(Self::gate(NOT_LONG)),
        }
    }
}

/// The union of the border-box fragments of `el`: its border box (CSSOM View). `None` when layout made no box fragment for it.
pub(crate) fn border_box(layout: &bare_layout::fragment::Layout, el: NodeId) -> Option<bare_layout::fragment::Rect> {
    let (mut x0, mut x1, mut y0, mut y1, mut any) = (f32::INFINITY, f32::NEG_INFINITY, f32::INFINITY, f32::NEG_INFINITY, false);
    for f in layout.fragments.iter().filter(|f| f.node == el && f.kind == bare_layout::fragment::Kind::Box) {
        any = true;
        x0 = x0.min(f.rect.x);
        x1 = x1.max(f.rect.x + f.rect.w);
        y0 = y0.min(f.rect.y);
        y1 = y1.max(f.rect.y + f.rect.h);
    }
    any.then_some(bare_layout::fragment::Rect { x: x0, y: y0, w: x1 - x0, h: y1 - y0 })
}

fn offset_width(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let el = d.style_html_el(it, this)?;
    Ok(Value::Number(d.offset_size(it, el)?.0 as f64))
}

fn offset_height(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let el = d.style_html_el(it, this)?;
    Ok(Value::Number(d.offset_size(it, el)?.1 as f64))
}

pub(crate) fn install(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    let html = d.p().html;
    d.def_get(it, html, "offsetWidth", offset_width, None)?;
    d.def_get(it, html, "offsetHeight", offset_height, None)
}

impl Browser {
    /// Replace the bounds of this module. Call it before scripts run.
    pub fn set_geometry_limits(&mut self, l: GeometryLimits) {
        self.dom.geometry.lim.set(l);
    }

    pub fn geometry_stats(&self) -> GeometryStats {
        self.dom.geometry.stats.get()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bare_css::cascade::{Origin, RuleSet, UA_CSS};

    /// Random changes of the tree, the text and `display`, each followed by a window walk that is told the changed nodes and their
    /// ancestors: its counts are those of a walk of the whole tree, and it skips big subtrees.
    #[test]
    fn a_walk_with_the_kept_counts_equals_a_full_walk() {
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        let mut rnd = |n: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % n as u64) as usize
        };
        let l = GeometryLimits { max_nodes: 1 << 20, max_depth: 400, max_text: 1 << 24, max_fragments: 1 << 24, max_work: 1 << 30 };
        let (mut skipped, mut steps) = (0, 0);
        for _ in 0..6 {
            let mut doc = bare_html::tree::parse("<!doctype html><body></body>");
            let all = |doc: &Document| {
                let mut out = vec![doc.root()];
                let mut k = 0;
                while k < out.len() {
                    let kids: Vec<_> = doc.children(out[k]).collect();
                    out.extend(kids);
                    k += 1;
                }
                out.retain(|&n| matches!(doc.data(n), NodeData::Element { .. }));
                out
            };
            for _ in 0..400 {
                let els = all(&doc);
                let parent = els[2 + rnd(els.len() - 2)];
                let el = doc.insert_element(parent, None, ["div", "span", "li", "br"][rnd(4)], &[("class", ["a", "b", "h"][rnd(3)])]);
                doc.append_text(el, "text");
            }
            let mut rules = RuleSet::new((800.0, 600.0));
            rules.add(&mut doc, UA_CSS, Origin::UserAgent);
            rules.add(&mut doc, ".h{display:none}.b{display:inline}li{display:list-item}", Origin::Author);
            let mut styles = rules.compute(&doc);
            let mut sums = Sums::default();
            walk::<true>(&doc, &styles, &l, Some(&mut sums), None).ok().expect("a walk");
            for _ in 0..120 {
                let els = all(&doc);
                let pick = els[2 + rnd(els.len() - 2)];
                let mut touched = vec![pick];
                match rnd(4) {
                    0 => doc.set_attr(pick, "class", ["a", "b", "h"][rnd(3)]),
                    1 => {
                        let el = doc.insert_element(pick, None, "div", &[("class", "a")]);
                        doc.append_text(el, "more text");
                        touched.push(el);
                    }
                    2 if doc.parent(pick) != Some(els[0]) => {
                        touched[0] = doc.parent(pick).expect("a parent");
                        doc.detach(pick);
                    }
                    _ => doc.append_text(pick, "x"),
                }
                let new = rules.compute(&doc);
                let mut dirty = vec![false; doc.len()];
                let display = |s: &Styles, n: NodeId| (n.index() < s.nodes()).then(|| s.style(n).map(|c| c.get(Property::Display))).flatten();
                touched.extend(all(&doc).into_iter().filter(|&n| display(&styles, n) != display(&new, n)));
                for n in touched {
                    let mut cur = Some(n);
                    while let Some(c) = cur.filter(|c| !dirty[c.index()]) {
                        dirty[c.index()] = true;
                        cur = doc.parent(c);
                    }
                }
                styles = new;
                let before = sums.used;
                let kept = walk::<true>(&doc, &styles, &l, Some(&mut sums), Some(&dirty)).ok().expect("a walk");
                let full = walk::<false>(&doc, &styles, &l, None, None).ok().expect("a walk");
                assert_eq!(kept, full, "step {steps}");
                skipped += (sums.used > before) as usize;
                steps += 1;
            }
        }
        assert!(skipped > steps / 5, "the kept counts were used in {skipped} of {steps} walks");
    }
}
