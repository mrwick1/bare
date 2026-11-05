//! `HTMLElement.offsetWidth` and `offsetHeight` over the native cascade (`bare_css::cascade`) and native layout (`bare_layout::layout`).
//!
//! How it works
//! - Every read runs the whole path again: the cascade of `browser_computed.rs` (`Dom::cascade_now`: the same sheets, gates and caps, so a
//!   linked sheet that is not supplied is `Unsupported`), then `bare_layout::layout` with the current viewport, then the answer from the
//!   fragments. Nothing is cached and nothing is stored on the element or in JS data, so a held element sees the current classes, `id`,
//!   `style` attributes, `<style>` text, supplied sheets, viewport, tree and text. (`Document::revision` ignores text changes, so it cannot key a cache.)
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
//! - `geometry-nesting`: the depth of nested boxes that layout lays out more than once (flex, grid, table, inline-block, float, absolute or fixed,
//!   `overflow: auto|scroll`). Each level can lay its content out again (a stretched flex or grid item, a scroll bar that appears, a shrink-to-fit
//!   measure), so work grows by a factor per level. The factor 4 is an estimate from `flex.rs`, `grid.rs`, `intrinsic.rs` and
//!   `block.rs` (`again`). It is not a proven bound on every layout path.
//! - `geometry-work`: `(nodes + text bytes) * (1 + 4^nesting)`. It is charged as steps before layout. The walk itself is charged first.
//!
//! Not done: `getBoundingClientRect`, `offsetParent`, `offsetLeft`, `offsetTop`, `ResizeObserver`, a cache, slots, shadow sheets, transformed geometry.

use crate::browser::*;
use crate::browser_platform::V;
use bare_css::cascade::Styles;
use bare_css::values::{Keyword, Property, Value as Cv};
use bare_html::dom::{Children, Document, NodeData, NodeId};
use bare_js::eval::Interp;
use bare_js::value::Value;
use std::cell::Cell;

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
    /// Nesting of boxes that layout can lay out again. `geometry-nesting`.
    pub max_nesting: u32,
    /// `(nodes + text bytes) * (1 + 4^nesting)`. `geometry-work`.
    pub max_work: u64,
}

impl Default for GeometryLimits {
    fn default() -> Self {
        GeometryLimits { max_nodes: 1 << 14, max_depth: 96, max_text: 1 << 16, max_fragments: 1 << 17, max_nesting: 4, max_work: 1 << 26 }
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
}

#[derive(Default)]
pub(crate) struct Geometry {
    lim: Cell<GeometryLimits>,
    stats: Cell<GeometryStats>,
}

const SLOT: &str = "geometry of a shadow tree with a slot (slot assignment is not built)";
const SHADOW_SHEET: &str = "geometry of a shadow tree with a style sheet (shadow style scoping is not built)";
const CULLED: &str = "geometry of an inline box with no background and no border (native layout makes no fragment for it)";
const NOT_LONG: &str = "geometry that is not a finite number that fits a long";

/// What the walk found.
struct Bound {
    fragments: u64,
    work: u64,
}

struct Frame<'a> {
    kids: Children<'a>,
    units: u64,
    inline: bool,
    nest: u32,
}

/// Does layout lay the content of this box out more than once? (See the header.)
fn multi_pass(style: &bare_css::cascade::ComputedStyle) -> bool {
    let kw = |p| match style.get(p) {
        Cv::Keyword(k) => Some(k),
        _ => None,
    };
    !matches!(kw(Property::Display), Some(Keyword::Block | Keyword::Inline | Keyword::ListItem))
        || matches!(kw(Property::Float), Some(Keyword::Left | Keyword::Right))
        || matches!(kw(Property::Position), Some(Keyword::Absolute | Keyword::Fixed))
        || matches!(kw(Property::OverflowX), Some(Keyword::Auto | Keyword::Scroll))
        || matches!(kw(Property::OverflowY), Some(Keyword::Auto | Keyword::Scroll))
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

/// One walk of the visible tree from the root element, without recursion. Layout reaches the same nodes and no others.
fn walk(doc: &Document, styles: &Styles, l: &GeometryLimits) -> Result<Bound, Refuse> {
    let Some(root) = doc.children(doc.root()).find(|&n| matches!(doc.data(n), NodeData::Element { .. })) else {
        return Ok(Bound { fragments: 0, work: 0 });
    };
    let (mut nodes, mut text, mut frags) = (0usize, 0usize, 0u64);
    let mut max_nest = 0u32;
    let mut stack: Vec<Frame> = Vec::new();
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
                check_shadow(doc, node)?;
                nodes += 1;
                if nodes > l.max_nodes {
                    return Err(Refuse::Limit("geometry-nodes"));
                }
                if stack.len() + 1 > l.max_depth {
                    return Err(Refuse::Limit("geometry-depth"));
                }
                let nest = stack.last().map_or(0, |f| f.nest) + multi_pass(style) as u32;
                if nest > l.max_nesting {
                    return Err(Refuse::Limit("geometry-nesting"));
                }
                max_nest = max_nest.max(nest);
                if style.get(Property::Display) == Cv::Keyword(Keyword::ListItem) {
                    frags += 1;
                }
                let inline = style.get(Property::Display) == Cv::Keyword(Keyword::Inline) || Some(name) == doc.find_atom("br");
                stack.push(Frame { kids: doc.flat_children(node), units: 0, inline, nest });
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
    let factor = 4u64.saturating_pow(max_nest).saturating_add(1);
    let work = ((nodes + text) as u64).saturating_mul(factor);
    if work > l.max_work {
        return Err(Refuse::Limit("geometry-work"));
    }
    Ok(Bound { fragments: frags, work })
}

impl Dom {
    /// The border-box size of `el` in whole CSS pixels (width, height), zero when it has no box.
    fn offset_size(&self, it: &mut Interp<'_>, el: NodeId) -> R<(i32, i32)> {
        if !self.in_document(it, el)? {
            return Ok((0, 0));
        }
        let styles = self.cascade_now(it)?;
        if styles.style(el).is_none_or(|s| s.get(Property::Display) == Cv::Keyword(Keyword::None)) {
            return Ok((0, 0));
        }
        let l = self.geometry.lim.get();
        let vp = self.plat.s.borrow().viewport;
        let doc = self.doc.borrow();
        it.charge(doc.len() as u64)?;
        let bound = match walk(&doc, &styles, &l) {
            Ok(b) => b,
            Err(Refuse::Limit(name)) => return Err(self.lim(name)),
            Err(Refuse::Gate(why)) => return Err(Self::gate(why)),
        };
        // The work is reserved before layout starts: it cannot be interrupted.
        it.charge(bound.work + 1)?;
        let layout = bare_layout::layout(&doc, &styles, vp);
        let mut s = self.geometry.stats.get();
        s = GeometryStats { layouts: s.layouts + 1, last_bound: bound.fragments, last_fragments: layout.fragments.len() as u64 };
        self.geometry.stats.set(s);
        let (mut x0, mut x1, mut y0, mut y1, mut any) = (f32::INFINITY, f32::NEG_INFINITY, f32::INFINITY, f32::NEG_INFINITY, false);
        for f in layout.fragments.iter().filter(|f| f.node == el && f.kind == bare_layout::fragment::Kind::Box) {
            any = true;
            x0 = x0.min(f.rect.x);
            x1 = x1.max(f.rect.x + f.rect.w);
            y0 = y0.min(f.rect.y);
            y1 = y1.max(f.rect.y + f.rect.h);
        }
        if !any {
            // Layout makes no fragment for an inline box with no background and no border (`inline.rs`, "culled"): that is not "no box".
            return match bare_layout::boxes::Tree::new(&doc, &styles).kind(el) {
                bare_layout::boxes::Kind::Inline | bare_layout::boxes::Kind::Break => Err(Self::gate(CULLED)),
                _ => Ok((0, 0)),
            };
        }
        let long = |v: f32| {
            let r = (v as f64 + 0.5).floor();
            (v.is_finite() && (0.0..=i32::MAX as f64).contains(&r)).then_some(r as i32)
        };
        match (long(x1 - x0), long(y1 - y0)) {
            (Some(w), Some(h)) => Ok((w, h)),
            _ => Err(Self::gate(NOT_LONG)),
        }
    }
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
