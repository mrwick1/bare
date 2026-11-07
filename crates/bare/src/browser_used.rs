//! The resolved values of `getComputedStyle` that CSSOM defines as the used value, and the text of the grid and calc values.
//!
//! How it works
//! - Chrome 155 (`ComputedStyleUtils`) gives the used value only where the computed value is not a fixed length. So a `width` or `height`
//!   of a box that is rendered is always read from layout, but a `margin`, a `padding` or an inset is read from layout only when it is
//!   `auto`, a percent or a `calc()` with a percent. A fixed length stays the computed one (`margin-left: 10px` is `10px`, also when a
//!   `min-width` or `max-width` changed the width). An inline box that is not atomic has no box in Chrome: all its values are computed.
//!   So are the values of an element that is not rendered (`display: none` on it or on an ancestor): `Snap::hidden`.
//! - One read runs the same bounded layout as `offsetWidth` (`Dom::layout_areas`, so `geometry-*` limits apply). The fragments give
//!   the border box. Layout does not store the margins or the padding, so they are found again from the style: a percent is of the width of
//!   the containing block (CSS Box 3, 7.1; for `top` and `bottom` of an inset, its height), and an `auto` margin from the free space.
//! - `width` and `height`: the content box (the border box less border, padding and the scrollbars of the box), or the border box when
//!   `box-sizing` is `border-box`.
//! - Margins: `auto` is 0 for a float, an inline-block, and a vertical margin of a block. A horizontal one of a block takes the free space
//!   (half each when both are `auto`), only when it is more than 0. An absolute box does the same when `left` and `right` (or `top` and
//!   `bottom`) are both set. An item of a flex container with one line: in the main axis the free space of the line goes in equal parts to
//!   all the auto margins of the line, in the cross axis it goes to the auto margins of the item (`Ctx::flex_margin`).
//! - Insets (`top`, `right`, `bottom`, `left`): `relative` and `sticky` give a percent in pixels; `relative` gives `auto` as 0 or as the minus of the opposite
//!   side, `sticky` keeps `auto`. `absolute` and `fixed` give `auto` as the distance from the edge of the containing block (its padding
//!   box less the scrollbars; the window for `fixed` and for an absolute box with no positioned ancestor).
//! - Lengths from layout are cut to 1/64 px (Chrome's `LayoutUnit`) and printed with 6 significant digits, as Chrome does.
//! - Grid containers: `grid-template-columns` and `-rows` are the sizes of all the tracks after layout, with the names of the lines
//!   (`bare_layout::layout_probing`). A grid with no track is `none`. Every other element keeps the computed list.
//!
//! Gates (`Error::Unsupported`)
//! - A percent padding or margin of a grid item (the grid area is not kept), an `auto` margin of a grid item or of an item of a wrapping flex
//!   container (the free space of the line is not kept), and a containing block that is an inline box.
//!
//! Known differences from Chrome (not gated, the engine does not keep the data)
//! - `text-decoration-style`, `-color` and `-thickness`, and a second line of `text-decoration`, are dropped by the parser. A read gives the
//!   line only, as Chrome does for the default style and color.
//! - `safe` and `unsafe` in `justify-*` and `align-*` are dropped by the parser.
//! - Chrome keeps layout in 1/64 px at each step. We round the result once, so a long chain of fractions can differ by 1/64 px.

use crate::browser::*;
use crate::browser_computed::{Snap, num};
use bare_js::eval::Interp;
use bare_css::cascade::Styles;
use bare_css::grid::{Areas as CellAreas, Bound, Count, Grid, Line, Part, Track};
use bare_css::values::{Keyword, Length, Property, Value as Cv};
use bare_html::dom::{Document, NodeData, NodeId};
use bare_layout::boxes::{Kind, Tree};
use bare_layout::fragment::{Layout, Rect};
use bare_layout::scroll::Areas;

const GRID_ITEM: &str = "a percent padding or margin of a grid item (the grid area is not kept)";
const FLEX_MARGIN: &str = "an auto margin of a grid item or of an item of a wrapping flex container (the free space of its line is not kept)";
const INLINE_CB: &str = "the containing block is an inline box";

/// A layout and its scroll containers, for one read.
pub(crate) struct Used {
    pub(crate) layout: Layout,
    pub(crate) areas: Areas,
}

type G<T> = Result<T, &'static str>;

/// A length cut to 1/64 px toward zero, as `LayoutUnit` does. A tiny excess from `f32` arithmetic is first rounded away.
fn lu(v: f32) -> f32 {
    let q = v * 64.0;
    let r = q.round();
    (if (q - r).abs() < 0.002 { r } else { q.trunc() }) / 64.0
}

fn px_text(v: f32) -> String {
    format!("{}px", num(lu(v)))
}

impl Dom {
    /// Run the bounded layout for a read of `props` of `el`, if one of them needs it (see the header). Not for an element that is not rendered.
    pub(crate) fn used_for(&self, it: &mut Interp<'_>, s: &Snap, el: NodeId, props: &[Property]) -> R<Option<Used>> {
        // Most reads are of other properties: no box tree for them.
        let may = |p: &Property| {
            use Property::*;
            matches!(p, Width | Height | MarginTop | MarginRight | MarginBottom | MarginLeft | PaddingTop | PaddingRight | PaddingBottom | PaddingLeft | Top | Right | Bottom | Left | GridTemplateColumns | GridTemplateRows)
        };
        if s.hidden || !props.iter().any(may) {
            return Ok(None);
        }
        let kind = Tree::new(&self.doc.borrow(), &s.styles).kind(el);
        let tracks = props.iter().any(|&p| wants_tracks(&s.style, p));
        if !tracks && !props.iter().any(|&p| needs_layout(&s.style, kind, p)) {
            return Ok(None);
        }
        self.geometry.probe.set(tracks.then_some(el));
        let r = self.layout_areas(it, &s.styles, None);
        self.geometry.probe.set(None);
        let (layout, areas, _) = r?;
        Ok(Some(Used { layout, areas }))
    }
}

/// What the three kinds of box do for a read.
fn is_box(k: Kind) -> bool {
    !matches!(k, Kind::Inline | Kind::Break | Kind::Text | Kind::None)
}

/// Does the resolved value of `p` need layout? `kind` is the box kind of the element, `rendered` is false inside `display: none`.
pub(crate) fn needs_layout(style: &bare_css::cascade::ComputedStyle, kind: Kind, p: Property) -> bool {
    use Property::*;
    let v = style.get(p);
    let fixed = matches!(v, Cv::Length(Length::Px(_)));
    match p {
        Width | Height => is_box(kind),
        MarginTop | MarginRight | MarginBottom | MarginLeft => is_box(kind) && !fixed,
        PaddingTop | PaddingRight | PaddingBottom | PaddingLeft => is_box(kind) && matches!(v, Cv::Length(Length::Percent(_) | Length::Calc(_))),
        Top | Right | Bottom | Left => {
            let pct = |v| matches!(v, Cv::Length(Length::Percent(_) | Length::Calc(_)));
            match style.get(Position) {
                Cv::Keyword(Keyword::Absolute | Keyword::Fixed) => !fixed,
                Cv::Keyword(Keyword::Relative) => {
                    let opposite = style.get(match p { Top => Bottom, Bottom => Top, Left => Right, _ => Left });
                    pct(v) || (v == Cv::Keyword(Keyword::Auto) && pct(opposite))
                }
                Cv::Keyword(Keyword::Sticky) => pct(v),
                _ => false,
            }
        }
        _ => false,
    }
}

/// The grid container whose used tracks a read needs (`bare_layout::layout_probing`).
pub(crate) fn wants_tracks(style: &bare_css::cascade::ComputedStyle, p: Property) -> bool {
    matches!(p, Property::GridTemplateColumns | Property::GridTemplateRows) && matches!(style.get(Property::Display), Cv::Keyword(Keyword::Grid | Keyword::InlineGrid))
}

/// Where a percent or an `auto` offset is measured from.
enum Cb {
    View,
    Node(NodeId),
}

/// What a read of the used values looks at.
pub(crate) struct Ctx<'a> {
    pub(crate) doc: &'a Document,
    pub(crate) styles: &'a Styles,
    tree: Tree<'a>,
    u: &'a Used,
    el: NodeId,
}

fn side(p: Property) -> usize {
    use Property::*;
    match p {
        MarginTop | PaddingTop | Top => 0,
        MarginRight | PaddingRight | Right => 1,
        MarginBottom | PaddingBottom | Bottom => 2,
        _ => 3,
    }
}

const MARGINS: [Property; 4] = [Property::MarginTop, Property::MarginRight, Property::MarginBottom, Property::MarginLeft];
const PADDINGS: [Property; 4] = [Property::PaddingTop, Property::PaddingRight, Property::PaddingBottom, Property::PaddingLeft];
const INSETS: [Property; 4] = [Property::Top, Property::Right, Property::Bottom, Property::Left];

impl<'a> Ctx<'a> {
    pub(crate) fn new(doc: &'a Document, styles: &'a Styles, u: &'a Used, el: NodeId) -> Ctx<'a> {
        Ctx { doc, styles, tree: Tree::new(doc, styles), u, el }
    }

    fn view(&self) -> Rect {
        Rect { x: 0.0, y: 0.0, w: self.u.layout.view.0, h: self.u.layout.view.1 }
    }

    fn border_box(&self, n: NodeId) -> Option<Rect> {
        crate::browser_geometry::border_box(&self.u.layout, n)
    }

    /// A length of the style in pixels, against `base` for a percent. `None` for `auto` and the like.
    fn resolve(&self, v: Cv, base: f32) -> Option<f32> {
        match v {
            Cv::Length(Length::Px(x)) => Some(x),
            Cv::Length(Length::Percent(p)) => Some(lu(base * p / 100.0)),
            Cv::Length(Length::Calc(i)) => {
                let (px, pct) = self.styles.calc(i);
                Some(lu(px + base * pct / 100.0))
            }
            _ => None,
        }
    }

    /// The containing block of `n`: the window for a `fixed` box, the nearest positioned ancestor for an absolute one, else the
    /// nearest ancestor that is not an inline box.
    fn containing(&self, n: NodeId) -> G<Cb> {
        let (out_of_flow, fixed) = (self.tree.kind(n) == Kind::OutOfFlow, self.tree.position(n) == Keyword::Fixed);
        if out_of_flow && fixed {
            return Ok(Cb::View);
        }
        let mut up = self.doc.flat_parent(n);
        while let Some(a) = up.filter(|&a| a != self.doc.root()) {
            if !matches!(self.doc.data(a), NodeData::Element { .. }) {
                up = self.doc.flat_parent(a);
                continue;
            }
            let positioned = self.tree.position(a) != Keyword::Static;
            let inline = self.tree.kind(a) == Kind::Inline;
            if out_of_flow && positioned {
                return if inline { Err(INLINE_CB) } else { Ok(Cb::Node(a)) };
            }
            if !out_of_flow && !inline {
                return Ok(Cb::Node(a));
            }
            up = self.doc.flat_parent(a);
        }
        Ok(Cb::View)
    }

    /// The rectangle that a percent of `n` is of: the padding box less the scrollbars for an absolute box, else the content box.
    fn cb_rect(&self, n: NodeId) -> G<Rect> {
        match self.containing(n)? {
            Cb::View => Ok(self.view()),
            Cb::Node(a) if self.tree.kind(n) == Kind::OutOfFlow => {
                let r = self.border_box(a).ok_or(INLINE_CB)?;
                let b = |p| self.tree.style(a).px(p).max(0.0);
                let (bars_x, bars_y) = self.u.areas.get(a).map_or((0.0, 0.0), |s| (s.pad.w - s.client().0, s.pad.h - s.client().1));
                Ok(Rect { x: r.x + b(Property::BorderLeftWidth), y: r.y + b(Property::BorderTopWidth), w: (r.w - b(Property::BorderLeftWidth) - b(Property::BorderRightWidth) - bars_x).max(0.0), h: (r.h - b(Property::BorderTopWidth) - b(Property::BorderBottomWidth) - bars_y).max(0.0) })
            }
            Cb::Node(a) => {
                if self.tree.is_flex_or_grid(Some(a)) && matches!(self.tree.style(a).get(Property::Display), Cv::Keyword(Keyword::Grid | Keyword::InlineGrid)) {
                    return Err(GRID_ITEM);
                }
                self.content_box(a)
            }
        }
    }

    /// The four paddings of `n` in pixels. A percent is of the width of the containing block.
    fn paddings(&self, n: NodeId) -> G<[f32; 4]> {
        let style = self.tree.style(n);
        let vals = PADDINGS.map(|p| style.get(p));
        let mut base = None;
        let mut out = [0.0; 4];
        for (o, v) in out.iter_mut().zip(vals) {
            *o = match v {
                Cv::Length(Length::Px(x)) => x,
                Cv::Length(Length::Percent(_) | Length::Calc(_)) => {
                    let w = match base {
                        Some(w) => w,
                        None => *base.insert(self.cb_rect(n)?.w),
                    };
                    self.resolve(v, w).unwrap_or(0.0)
                }
                _ => 0.0,
            }
            .max(0.0);
        }
        Ok(out)
    }

    /// The content box of `n`: its border box less border, padding and scrollbars.
    fn content_box(&self, n: NodeId) -> G<Rect> {
        let r = self.border_box(n).ok_or(INLINE_CB)?;
        let style = self.tree.style(n);
        let b = |p| style.px(p).max(0.0);
        let pad = self.paddings(n)?;
        let (bx, by) = self.u.areas.get(n).map_or((0.0, 0.0), |s| (s.pad.w - s.client().0, s.pad.h - s.client().1));
        let (l, t) = (b(Property::BorderLeftWidth) + pad[3], b(Property::BorderTopWidth) + pad[0]);
        let (w, h) = (b(Property::BorderRightWidth) + pad[1] + bx, b(Property::BorderBottomWidth) + pad[2] + by);
        Ok(Rect { x: r.x + l, y: r.y + t, w: (r.w - l - w).max(0.0), h: (r.h - t - h).max(0.0) })
    }

    /// The margin of side `i` as the style says it, or `None` for `auto`.
    fn margin_spec(&self, n: NodeId, i: usize) -> G<Option<f32>> {
        let v = self.tree.style(n).get(MARGINS[i]);
        Ok(match v {
            Cv::Length(Length::Percent(_) | Length::Calc(_)) => self.resolve(v, self.cb_rect(n)?.w),
            _ => self.resolve(v, 0.0),
        })
    }

    /// The inset of side `i` as the style says it (against the containing block), or `None` for `auto`.
    fn inset_spec(&self, n: NodeId, i: usize) -> G<Option<f32>> {
        let v = self.tree.style(n).get(INSETS[i]);
        if !matches!(v, Cv::Length(_)) {
            return Ok(None);
        }
        let cb = if matches!(v, Cv::Length(Length::Px(_))) { Rect::default() } else { self.cb_rect(n)? };
        Ok(self.resolve(v, if i % 2 == 1 { cb.w } else { cb.h }))
    }

    /// The used margin of side `i` (0 top, 1 right, 2 bottom, 3 left).
    fn margin(&self, n: NodeId, i: usize) -> G<f32> {
        if let Some(m) = self.margin_spec(n, i)? {
            return Ok(m);
        }
        let kind = self.tree.kind(n);
        if matches!(kind, Kind::Atomic | Kind::Float) || !is_box(kind) {
            return Ok(0.0);
        }
        let Some(r) = self.border_box(n) else { return Ok(0.0) };
        let (lo, hi) = if i % 2 == 1 { (3, 1) } else { (0, 2) };
        if kind == Kind::OutOfFlow {
            // CSS 2.1, 10.3.7 and 10.6.4: auto margins share the room that is left between two offsets that are both set.
            let (a, b) = (self.inset_spec(n, lo)?, self.inset_spec(n, hi)?);
            let (Some(a), Some(b)) = (a, b) else { return Ok(0.0) };
            let cb = self.cb_rect(n)?;
            let (room, size) = if i % 2 == 1 { (cb.w, r.w) } else { (cb.h, r.h) };
            return self.share(n, i, lo, hi, room - a - b - size);
        }
        let parent = self.doc.flat_parent(n);
        if let Some(p) = parent.filter(|&p| self.tree.is_flex_or_grid(Some(p))) {
            return self.flex_margin(n, i, p);
        }
        let block = matches!(self.tree.style(n).get(Property::Display), Cv::Keyword(Keyword::Block | Keyword::ListItem | Keyword::Table | Keyword::FlowRoot | Keyword::Flex | Keyword::Grid));
        if i.is_multiple_of(2) || !block {
            return Ok(0.0);
        }
        let room = self.cb_rect(n)?.w - r.w;
        self.share(n, i, lo, hi, room)
    }

    /// The used auto margin `i` of `n`, an item of the flex container `p` (CSS Flexbox 9.5, 8.1). A single line only: in the main axis the
    /// free space of the line goes in equal parts to all the auto margins of the line, in the cross axis the free space of the line goes
    /// to the margins of the item (the line is as big as the content box of the container).
    fn flex_margin(&self, n: NodeId, i: usize, p: NodeId) -> G<f32> {
        let ps = self.tree.style(p);
        let flex = matches!(ps.get(Property::Display), Cv::Keyword(Keyword::Flex | Keyword::InlineFlex));
        if !flex || ps.get(Property::FlexWrap) != Cv::Keyword(Keyword::Nowrap) {
            return Err(FLEX_MARGIN);
        }
        let row = !matches!(ps.get(Property::FlexDirection), Cv::Keyword(Keyword::Column | Keyword::ColumnReverse));
        let wide = i % 2 == 1;
        let (lo, hi) = if wide { (3, 1) } else { (0, 2) };
        let c = self.content_box(p)?;
        let room = if wide { c.w } else { c.h };
        // The size of an item on this axis with its margins that are set, and how many of its margins are auto.
        let outer = |m: NodeId| -> G<(f32, usize)> {
            let r = self.border_box(m).ok_or(FLEX_MARGIN)?;
            let (a, b) = (self.margin_spec(m, lo)?, self.margin_spec(m, hi)?);
            Ok(((if wide { r.w } else { r.h }) + a.unwrap_or(0.0) + b.unwrap_or(0.0), usize::from(a.is_none()) + usize::from(b.is_none())))
        };
        if wide != row {
            let (size, _) = outer(n)?;
            let own = self.margin_spec(n, if i == lo { hi } else { lo })?.unwrap_or(0.0);
            return self.share(n, i, lo, hi, room - (size - own));
        }
        let (mut used, mut autos) = (0.0, 0);
        for k in self.doc.flat_children(p) {
            match self.doc.data(k) {
                NodeData::Text(_) if !self.tree.is_collapsible_space(k) => return Err(FLEX_MARGIN),
                NodeData::Element { .. } if !matches!(self.tree.kind(k), Kind::None | Kind::OutOfFlow) => {
                    let (s, a) = outer(k)?;
                    used += s;
                    autos += a;
                }
                _ => {}
            }
        }
        let free = room - used;
        Ok(if free <= 0.0 || autos == 0 { 0.0 } else { lu(free / autos as f32) })
    }

    /// The part of `room` (the width less the box and the offsets) that the auto margin `i` takes, `lo` and `hi` being the two sides of its axis.
    fn share(&self, n: NodeId, i: usize, lo: usize, hi: usize, room: f32) -> G<f32> {
        let other = if i == lo { hi } else { lo };
        let other_margin = self.margin_spec(n, other)?;
        let free = room - other_margin.unwrap_or(0.0);
        if free <= 0.0 {
            return Ok(0.0);
        }
        Ok(match other_margin {
            Some(_) => free,
            // Both are auto: the start side gets half, cut to 1/64 px, and the end side the rest.
            None if i == lo => lu(free / 2.0),
            None => free - lu(free / 2.0),
        })
    }

    /// The text of the used value of `p`, or `None` when the computed value is the resolved one.
    pub(crate) fn text(&self, s: &Snap, p: Property) -> G<Option<String>> {
        use Property::*;
        let el = self.el;
        let kind = self.tree.kind(el);
        if !needs_layout(&s.style, kind, p) && !wants_tracks(&s.style, p) {
            return Ok(None);
        }
        let Some(r) = self.border_box(el) else { return Ok(None) };
        let style = &s.style;
        Ok(Some(match p {
            Width | Height => {
                let wide = p == Width;
                if style.get(BoxSizing) == Cv::Keyword(Keyword::BorderBox) {
                    px_text(if wide { r.w } else { r.h })
                } else {
                    let c = self.content_box(el)?;
                    px_text(if wide { c.w } else { c.h })
                }
            }
            MarginTop | MarginRight | MarginBottom | MarginLeft => px_text(self.margin(el, side(p))?),
            PaddingTop | PaddingRight | PaddingBottom | PaddingLeft => px_text(self.paddings(el)?[side(p)]),
            Top | Right | Bottom | Left => return self.inset(s, side(p)),
            GridTemplateColumns | GridTemplateRows => return Ok(self.tracks(p)),
            _ => return Ok(None),
        }))
    }

    fn inset(&self, s: &Snap, i: usize) -> G<Option<String>> {
        let n = self.el;
        let style = &s.style;
        let v = style.get(INSETS[i]);
        let auto = v == Cv::Keyword(Keyword::Auto);
        match style.get(Property::Position) {
            Cv::Keyword(Keyword::Relative) if auto => {
                let opposite = style.get(INSETS[(i + 2) % 4]);
                let cb = if matches!(opposite, Cv::Length(Length::Px(_))) { Rect::default() } else { self.cb_rect(n)? };
                Ok(Some(match opposite {
                    Cv::Length(_) => {
                        let o = self.resolve(opposite, if i % 2 == 1 { cb.w } else { cb.h }).unwrap_or(0.0);
                        px_text(-o)
                    }
                    _ => "0px".to_string(),
                }))
            }
            Cv::Keyword(Keyword::Relative | Keyword::Sticky) => Ok(self.inset_spec(n, i)?.map(px_text)),
            Cv::Keyword(Keyword::Absolute | Keyword::Fixed) => {
                if let Some(x) = self.inset_spec(n, i)? {
                    return Ok(Some(px_text(x)));
                }
                let (Some(r), cb) = (self.border_box(n), self.cb_rect(n)?) else { return Ok(None) };
                let (ml, mt) = (self.margin(n, 3)?, self.margin(n, 0)?);
                let (mr, mb) = (self.margin(n, 1)?, self.margin(n, 2)?);
                let v = match i {
                    3 => r.x - cb.x - ml,
                    0 => r.y - cb.y - mt,
                    1 => cb.w - (r.x - cb.x + r.w) - mr,
                    _ => cb.h - (r.y - cb.y + r.h) - mb,
                };
                Ok(Some(px_text(v)))
            }
            _ => Ok(None),
        }
    }

    /// `grid-template-columns` and `-rows` of a grid container after layout.
    fn tracks(&self, p: Property) -> Option<String> {
        let t = self.u.layout.tracks.as_ref()?;
        let a = usize::from(p == Property::GridTemplateColumns);
        let sizes = &t.sizes[a];
        if sizes.is_empty() {
            return Some("none".into());
        }
        let line_names = |line: i32, out: &mut Vec<String>| {
            let names: Vec<&str> = t.names[a].iter().filter(|(_, l)| *l == line).map(|(n, _)| n.as_str()).collect();
            if !names.is_empty() {
                out.push(format!("[{}]", names.join(" ")));
            }
        };
        let mut out = Vec::new();
        for (i, &w) in sizes.iter().enumerate() {
            line_names(i as i32 - t.before[a] as i32 + 1, &mut out);
            out.push(px_text(w));
        }
        line_names(sizes.len() as i32 - t.before[a] as i32 + 1, &mut out);
        Some(out.join(" "))
    }
}

// ------------------------------------------------------------------------------------------ computed text

/// `calc(100% - 20px)`: the percent first, as Chrome prints it.
pub(crate) fn calc_text(styles: &Styles, i: u32) -> String {
    let (px, pct) = styles.calc(i);
    let sign = |x: f32| if x < 0.0 { '-' } else { '+' };
    match (px == 0.0, pct == 0.0) {
        (true, _) => format!("calc({}%)", num(pct)),
        (_, true) => format!("calc({}px)", num(px)),
        _ => format!("calc({}% {} {}px)", num(pct), sign(px), num(px.abs())),
    }
}

fn length_text(styles: &Styles, l: Length) -> String {
    match l {
        Length::Px(x) => format!("{}px", num(x)),
        Length::Percent(x) => format!("{}%", num(x)),
        Length::Calc(i) => calc_text(styles, i),
        // The cascade made every other unit pixels.
        _ => "0px".into(),
    }
}

fn bound_text(styles: &Styles, b: Bound) -> String {
    match b {
        Bound::Auto => "auto".into(),
        Bound::MinContent => "min-content".into(),
        Bound::MaxContent => "max-content".into(),
        Bound::Len(l) => length_text(styles, l),
        Bound::Fr(x) => format!("{}fr", num(x)),
        Bound::Fit(l) => format!("fit-content({})", length_text(styles, l)),
    }
}

fn track_text(styles: &Styles, t: &Track) -> String {
    if t.min == t.max {
        bound_text(styles, t.min)
    } else if t.min == Bound::Auto && matches!(t.max, Bound::Fr(_) | Bound::Fit(_)) {
        bound_text(styles, t.max)
    } else {
        format!("minmax({}, {})", bound_text(styles, t.min), bound_text(styles, t.max))
    }
}

fn parts_text(styles: &Styles, parts: &[Part]) -> String {
    let mut out: Vec<String> = Vec::with_capacity(parts.len());
    for part in parts {
        out.push(match part {
            Part::Names(names) => format!("[{}]", names.join(" ")),
            Part::Track(t) => track_text(styles, t),
            Part::Repeat(count, inner) => {
                let n = match count {
                    Count::Times(n) => n.to_string(),
                    Count::Fill => "auto-fill".into(),
                    Count::Fit => "auto-fit".into(),
                };
                format!("repeat({n}, {})", parts_text(styles, inner))
            }
        });
    }
    out.join(" ")
}

fn line_text(l: &Line) -> String {
    match l {
        Line::Auto => "auto".into(),
        Line::Num(n) => n.to_string(),
        Line::Name(name, 1) => name.to_string(),
        Line::Name(name, n) => format!("{n} {name}"),
        Line::Span(None, n) => format!("span {n}"),
        Line::Span(Some(name), 1) => format!("span {name}"),
        Line::Span(Some(name), n) => format!("span {n} {name}"),
    }
}

/// `"a b" "c d"`: one string for each row, `.` for a cell that no area covers.
fn areas_text(a: &CellAreas) -> String {
    let (rows, cols) = (a.rows as usize, a.columns as usize);
    let mut cells: Vec<Vec<&str>> = vec![vec!["."; cols]; rows];
    for (name, [r0, r1, c0, c1]) in &a.names {
        for row in cells.iter_mut().take(*r1 as usize - 1).skip(*r0 as usize - 1) {
            for cell in row.iter_mut().take(*c1 as usize - 1).skip(*c0 as usize - 1) {
                *cell = name;
            }
        }
    }
    cells.iter().map(|row| format!("\"{}\"", row.join(" "))).collect::<Vec<_>>().join(" ")
}

/// The computed text of a grid value (`Value::Grid`).
pub(crate) fn grid_text(styles: &Styles, g: &Grid) -> String {
    match g {
        Grid::Tracks(parts) => parts_text(styles, parts),
        Grid::Line(l) => line_text(l),
        Grid::Areas(a) => areas_text(a),
    }
}

/// `grid-auto-flow`: `row`, `column`, `dense` (row dense) or `column dense`.
pub(crate) fn flow_text(flow: f32) -> &'static str {
    use bare_css::grid::{FLOW_COLUMN, FLOW_DENSE};
    match (flow as u32 & FLOW_COLUMN as u32 != 0, flow as u32 & FLOW_DENSE as u32 != 0) {
        (false, false) => "row",
        (true, false) => "column",
        (false, true) => "dense",
        (true, true) => "column dense",
    }
}
