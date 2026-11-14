//! Inline layout: put text and inline boxes in lines, and break the lines (CSS 2.1 section 10.8, CSS Text 3).
//!
//! The content of a run (`boxes::Child::Lines`) is a flat stream of events: text, the start and the end of an inline
//! box (`<b>`, `<a>`), an atomic inline (`inline-block`, `img`), a `<br>`, a block inside an inline. `Events` walks the
//! DOM and gives them one at a time, with no list in memory. (Chrome builds the same stream once and keeps it: a list
//! of `NGInlineItem` with the text of the paragraph copied into one string.)
//!
//! Line breaking is greedy, as in every browser: put words on the line while they fit, and start a new line when the
//! next one does not. A line may only break at a "break opportunity": after a space (if `white-space` wraps), and before
//! and after an atomic inline. Between two opportunities the content is one unit, even across elements: `a<b>b</b>c`
//! never breaks. The unit waits in `pending` until the next opportunity, and then it goes on this line or the next.
//! (The other way, used by TeX, looks at the whole paragraph to make the lines even. It costs much more. Chrome uses
//! it only for `text-wrap: pretty`.)
//!
//! The height of a line: each inline box on the line (and the block itself, the "strut") has a font and a
//! `line-height`. The extra space of the line height, beyond the font's ascent and descent, is split in half above and
//! below the letters ("half-leading"). The boxes sit on one baseline, and the line is as tall as what sticks out the
//! most above and below it. A `<span>` with a bigger font makes its line taller.
//!
//! `vertical-align` moves a box off the baseline (`Align`): its content goes up or down by a shift, and the shifts of
//! boxes inside each other add up. A box with `top` or `bottom` (with what is inside it) waits until the height of the
//! line is known, and then it goes to the top or the bottom of the line (CSS 2.1, 10.8.1). The text keeps no shift of its
//! own: an `Item::Place` in the line at each change of place is enough, and a line with no `vertical-align`
//! has none.
//!
//! A line is kept in `Line` until it is full, and only then its fragments are written: their heights and their place
//! for `text-align` are known only at the end. An inline box that goes over two lines makes one fragment on each line.
//! Its left border and padding are on the first one, its right border and padding on the last one (like
//! `box-decoration-break: slice`).
//!
//! Floats (`floats.rs`) make lines shorter: each line asks the floats for the room at its height (`Breaker::band`),
//! with the height of the strut, as the line is not done yet. Chrome asks again when a line turns out taller and
//! meets a float lower down; we do not. If a unit does not fit next to the floats on an empty line, the line goes down
//! below the next float that ends (CSS 2.1, 9.5). A float in the middle of a line goes on that line if it fits next to
//! what is already there, and the text on the line moves aside. If it does not fit, it waits for the next line.

use crate::breaks::{self, Point, Run};
use crate::text::Wrap;
use crate::Cx;
use crate::block::{self, MAX_DEPTH, Pos, Space, border_padding, clear_sides, resolve};
use crate::boxes::Kind;
use crate::font::{self, LETTER, Metrics};
use crate::fragment::{Fragment, Kind as FragmentKind, Rect};
use crate::intrinsic;
use crate::text::{self, Flow, Piece, Transform, WhiteSpace};
use bare_css::values::{Keyword, Property, Value};
use bare_html::dom::{NodeData, NodeId};

/// One thing in the content of a run, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// A text node.
    Text(NodeId),
    /// The start of an inline box.
    Open(NodeId),
    /// The end of an inline box.
    Close(NodeId),
    /// An atomic inline: one unit on the line.
    Atomic(NodeId),
    /// A float: out of the line, at the side.
    Float(NodeId),
    /// A box with `position: absolute` or `fixed`: out of the line. It leaves its static position.
    OutOfFlow(NodeId),
    /// `<br>`.
    Break(NodeId),
    /// A block inside an inline box: it takes a line of its own.
    Block(NodeId),
}

/// The events of the run `first..=last`. JS: a generator over the DOM, with a stack of the open inline boxes.
pub struct Events<'t, 'a> {
    tree: &'t crate::boxes::Tree<'a>,
    next: Option<NodeId>,
    last: NodeId,
    open: Vec<NodeId>,
}

impl<'t, 'a> Events<'t, 'a> {
    pub fn new(tree: &'t crate::boxes::Tree<'a>, first: NodeId, last: NodeId) -> Events<'t, 'a> {
        Events { tree, next: Some(first), last, open: Vec::new() }
    }

    /// The node after `node`: its next sibling, or nothing at the end of the run or of an inline box.
    fn after(&self, node: NodeId) -> Option<NodeId> {
        if self.open.is_empty() && node == self.last { None } else { self.tree.doc.flat_next(node) }
    }
}

impl Iterator for Events<'_, '_> {
    type Item = Event;

    fn next(&mut self) -> Option<Event> {
        loop {
            let Some(node) = self.next else {
                // The end of the children of the innermost open box: close it.
                let node = self.open.pop()?;
                self.next = self.after(node);
                return Some(Event::Close(node));
            };
            let kind = self.tree.kind(node);
            if kind == Kind::Inline {
                self.open.push(node);
                self.next = self.tree.doc.flat_children(node).next();
                return Some(Event::Open(node));
            }
            self.next = self.after(node);
            return Some(match kind {
                Kind::Text => Event::Text(node),
                Kind::Atomic => Event::Atomic(node),
                Kind::Float => Event::Float(node),
                Kind::OutOfFlow => Event::OutOfFlow(node),
                Kind::Break => Event::Break(node),
                Kind::Block | Kind::ListItem => Event::Block(node),
                Kind::None | Kind::Inline => continue,
            });
        }
    }
}

/// The result of the lines of one run.
pub struct Lines {
    /// The height of all the lines.
    pub height: f32,
    /// At least one line has something on it. A run of empty inline boxes makes no line, and margins go through it.
    pub any: bool,
    /// The baseline of the last line, from the top of the first one.
    pub baseline: Option<f32>,
}

/// Something on a line, before its fragments are written. `x` is from the start of the line (or of `pending`).
/// 24 bytes in the measured build. Extra wrapping state stays outside each item.
enum Item {
    /// Text of one node: the bytes `start..end`, `w` pixels wide.
    Text { node: NodeId, start: u32, end: u32, x: f32, w: f32 },
    /// The start of an inline box that is not culled (see `Inline`). `x` is where its border box starts.
    Open { node: NodeId, x: f32 },
    /// The end of an inline box that is not culled. `x` is where its border box ends.
    Close { x: f32 },
    /// An atomic inline, already laid out. In a `Box`, so that the other items stay small.
    Atomic(Box<Atomic>),
    /// The text after it sits at this place (`vertical-align`), as for `Open`. Only on a line that has a change of place.
    Place { shift: f32, root: u32 },
}

/// An atomic inline: its fragments (relative to it), the size of its margin box, and its baseline from the top of the
/// margin box. `align`: its `vertical-align`. `shift` and `root`: where its baseline is, as for `Open`.
struct Atomic {
    fragments: Vec<Fragment>,
    x: f32,
    w: f32,
    h: f32,
    baseline: f32,
    margin_left: f32,
    margin_top: f32,
    align: Align,
    shift: f32,
    root: u32,
}

impl Item {
    fn shift(&mut self, dx: f32) {
        match self {
            Item::Text { x, .. } | Item::Open { x, .. } | Item::Close { x, .. } => *x += dx,
            Item::Atomic(a) => a.x += dx,
            Item::Place { .. } => {}
        }
    }
}

/// What an inline box needs on a line.
#[derive(Debug, Clone, Copy)]
struct Inline {
    /// The left and right margins, and the left and right border and padding.
    margins: (f32, f32),
    edges: (f32, f32),
    /// How far its font and `line-height` reach above and below the baseline (0 for a culled box).
    above: f32,
    below: f32,
    /// It has a background or a border: it needs a fragment.
    paint: bool,
    /// It has a `position`: it needs a fragment too, to be a place that `top` and `left` move.
    positioned: bool,
    /// It has a `vertical-align` other than `baseline`.
    raised: bool,
    /// Culled: no fragment, no item, nothing. Most `<b>`, `<i>` and `<a>` are culled: they have nothing to paint, no
    /// margin, border or padding, and the same font as their parent, so they change nothing on the line. Chrome does
    /// the same ("culled inlines", `LayoutInline::ShouldCreateBoxFragment`). It saves a fragment for each line that
    /// the box is on: 10,000 nested `<span>` over 10,000 lines would make 100,000,000 fragments.
    culled: bool,
}

/// An inline box that is open on the line.
struct Open {
    node: NodeId,
    /// Where its border box starts on this line.
    x0: f32,
    paint: bool,
    /// The order in which the boxes opened: the fragments of the outer ones come first, so paint draws them first.
    order: u32,
    /// The most that this box and the open boxes around it reach above and below the baseline (of the line, or of the
    /// `top` or `bottom` box around it: `root`).
    above: f32,
    below: f32,
    /// `vertical-align`. `shift`: how far its baseline is above the baseline of its root. `root`: the `top` or `bottom` box
    /// that it is in (an index in `Breaker::aligned`: this box itself if it is one), or `NONE` for the line.
    align: Align,
    shift: f32,
    root: u32,
}

/// A `top` or `bottom` box on the line: how far it reaches above and below its own baseline. It is placed when the height
/// of the line is known.
struct Aligned {
    bottom: bool,
    above: f32,
    below: f32,
    /// How far the shifted box that it is in lifts it (0 for a box that is not in one).
    shift: f32,
}

/// The opted-in path for spacing or extra break opportunities. Keep the ordinary
/// piece loop small; joined grapheme work runs only if the pending unit overflows.
#[cold]
#[inline(never)]
fn spaced_pieces<const WRAPS: bool>(b: &mut Breaker<'_, '_, WRAPS>, node: NodeId, text: &str, (ws, transform, metrics): (WhiteSpace, Transform, Metrics), (sp, wrap): ((f32, f32), Wrap), flow: &mut Flow) {
    for piece in text::pieces(text, ws, transform, flow) {
        match piece {
            Piece::Word { start, end, width } => {
                if WRAPS { b.pending_wrap |= wrap != Wrap::Normal; }
                b.add_text(node, start, end, metrics.px(width) + font::extra(&text[start as usize..end as usize], sp));
            }
            Piece::Space { start, end, width } if ws.wrap => {
                b.commit();
                b.add_space(node, start, end, metrics.px(width) + font::extra_spaces(width, sp));
            }
            Piece::Space { start, end, width } => b.add_text(node, start, end, metrics.px(width) + font::extra_spaces(width, sp)),
            Piece::Tab { at } => {
                if WRAPS { b.pending_wrap |= wrap != Wrap::Normal; }
                let stop = metrics.px(8 * LETTER) + 8.0 * (sp.0 + sp.1);
                let x = b.x + b.pending_w;
                b.add_text(node, at, at + 1, if stop > 0.0 { ((x / stop).floor() + 1.0) * stop - x } else { 0.0 });
            }
            Piece::Newline { at } => {
                b.add_text(node, at, at, 0.0);
                b.commit();
                b.content = true;
                b.finish();
                if WRAPS { b.pending_word_start = true; }
            }
        }
    }
}

/// The line breaker. JS: an object with the state of the line that is being filled.
struct Breaker<'c, 'a, const WRAPS: bool> {
    cx: &'c mut Cx<'a>,
    block: NodeId,
    /// `text-align` of the block, with `start` and `end` turned into `left` and `right` by its direction.
    align: Keyword,
    width: f32,
    /// The line so far, and where it ends.
    line: Vec<Item>,
    x: f32,
    /// The line has something that makes it a real line: text, an atomic inline, a `<br>`, or an inline box with a
    /// border, padding or margin.
    content: bool,
    /// The last space on the line, which hangs if the line ends after it: the item, the byte where the space starts,
    /// and its width.
    trail: Option<(usize, u32, f32)>,
    /// The unit that waits for the next break opportunity, and its width.
    pending: Vec<Item>,
    pending_w: f32,
    pending_wrap: bool,
    pending_word_start: bool,
    pending_content: bool,
    /// The inline boxes that are open now (not culled), outer first. A stack: a box closes before the box around it.
    open: Vec<Open>,
    opened: u32,
    /// The pieces of inline boxes that closed on this line and need a fragment: (order, node, x0, x1), and (`shift`, `root`).
    segments: Vec<(u32, NodeId, f32, f32, f32, u32)>,
    /// How far the boxes on this line reach above and below the baseline.
    above: f32,
    below: f32,
    /// The `top` and `bottom` boxes on this line, how many of them are open now, and where the text that was added last sits:
    /// (`shift`, `root`). A change of it goes in `line` as an `Item::Place`.
    aligned: Vec<Aligned>,
    rooted: u32,
    place: (f32, u32),
    /// The same for the block itself, the "strut": as if each line started with a letter in the block's font.
    strut: (f32, f32),
    /// The top of the line, from the top of the first line.
    y: f32,
    any: bool,
    baseline: Option<f32>,
    /// Where the first line starts in the BFC, for the floats.
    bfc: (f32, f32),
    /// The room for this line next to the floats: where it starts (from the left of the block), and how wide it is.
    left: f32,
    avail: f32,
    /// Floats that did not fit on the line where they are in the source. They go at the top of the next line.
    deferred: Vec<(NodeId, bool, Atomic)>,
    /// Out-of-flow boxes that were blocks before: their static position is at the start of the next line.
    statics: Vec<NodeId>,
    /// The static positions on this line of out-of-flow boxes that were inline: `text-align` moves them with the line.
    line_statics: Vec<usize>,
}

/// How far the font and the `line-height` of a box reach above and below the baseline. The extra space of the line
/// height beyond the font's ascent and descent is split in half above and below ("half-leading"). As in Chrome, the line
/// height is cut to 1/64 px and the half above is a whole number of pixels (rounded down), so the rest goes below.
fn reach(cx: &Cx, node: NodeId) -> (f32, f32) {
    let style = cx.tree.style(node);
    let m = Metrics::of(style);
    reach_of(&m, m.line_height(style))
}

fn reach_of(m: &Metrics, line_height: f32) -> (f32, f32) {
    let leading = font::lu(line_height) - (m.ascent + m.descent);
    let up = font::floor(leading / 2.0);
    (m.ascent + up, m.descent + leading - up)
}

/// No `top` or `bottom` box.
const NONE: u32 = u32::MAX;

/// Where a box sits on its line (`vertical-align`): `Shift` is how far its baseline is above the baseline of its parent.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Align {
    Shift(f32),
    Top,
    Bottom,
}

/// The `vertical-align` of an inline box or an atomic inline. `above` and `below`: how far the box reaches above and below
/// its own baseline, with its half-leading (for an atomic inline: its margin box). The numbers are those of Chrome:
/// `sub` and `super` use the font size of the parent, `middle` its x-height, `text-top` and `text-bottom` its font.
fn align(cx: &Cx, node: NodeId, above: f32, below: f32) -> Align {
    let style = cx.tree.style(node);
    let v = style.get(Property::VerticalAlign);
    if matches!(v, Value::Keyword(Keyword::Baseline)) {
        return Align::Shift(0.0);
    }
    let parent = || Metrics::of(cx.tree.style(cx.tree.doc.flat_parent(node).expect("an inline box has a parent")));
    let size = |m: &Metrics| (m.size * 64.0).floor() as i32;
    Align::Shift(match v {
        Value::Keyword(Keyword::Top) => return Align::Top,
        Value::Keyword(Keyword::Bottom) => return Align::Bottom,
        Value::Keyword(Keyword::Sub) => -((size(&parent()) / 5 + 64) as f32 / 64.0),
        Value::Keyword(Keyword::Super) => (size(&parent()) / 3 + 64) as f32 / 64.0,
        Value::Keyword(Keyword::Middle) => parent().half_x_height() - (above - below) / 2.0,
        Value::Keyword(Keyword::TextTop) => parent().ascent - above,
        Value::Keyword(Keyword::TextBottom) => below - parent().descent,
        // A length, and a percent of the line height of the box itself.
        Value::Length(_) => font::lu(resolve(cx, v, Some(font::lu(Metrics::of(style).line_height(style)))).unwrap_or(0.0)),
        _ => 0.0,
    })
}

/// What an inline box needs on a line. `width`: the width of the block, for percent margins and padding.
fn inline_box(cx: &Cx, node: NodeId, width: f32) -> Inline {
    let style = cx.tree.style(node);
    let (border, padding) = border_padding(cx, style, width);
    let margin = |p| resolve(cx, style.get(p), Some(width)).unwrap_or(0.0);
    let margins = (margin(Property::MarginLeft), margin(Property::MarginRight));
    let edges = (border.left + padding.left, border.right + padding.right);
    let m = Metrics::of(style);
    let line_height = m.line_height(style);
    let paint = style.color(Property::BackgroundColor).a != 0 || border.horizontal() + border.vertical() != 0.0;
    let parent = cx.tree.doc.flat_parent(node).expect("an inline box has a parent");
    let parent_style = cx.tree.style(parent);
    let parent_metrics = Metrics::of(parent_style);
    // The same font and line height as the parent reach the same: no need to work out how far.
    let same_font = m == parent_metrics && line_height == parent_metrics.line_height(parent_style);
    let flat = margins == (0.0, 0.0) && edges == (0.0, 0.0) && padding.vertical() == 0.0;
    let positioned = cx.tree.position(node) != Keyword::Static;
    // A box with `vertical-align` moves its content: it stays in the line.
    let raised = !matches!(style.get(Property::VerticalAlign), Value::Keyword(Keyword::Baseline));
    let culled = !paint && flat && same_font && !positioned && !raised && !cx.uncull;
    // A culled box has no use for the numbers.
    let (above, below) = if culled { (0.0, 0.0) } else { reach_of(&m, line_height) };
    Inline { margins, edges, above, below, paint, positioned, raised, culled }
}

/// Lay out the run `first..=last` of the children of `block` in lines `width` pixels wide. The fragments are relative to
/// the top-left of the first line: the caller moves them. `bfc`: where that top-left is in the BFC, for the floats.
pub fn lines(cx: &mut Cx, block: NodeId, first: NodeId, last: NodeId, width: f32, bfc: (f32, f32)) -> Lines {
    if cx.tree.styles.wrapping() { lines_with::<true>(cx, block, first, last, width, bfc) }
    else { lines_with::<false>(cx, block, first, last, width, bfc) }
}

fn lines_with<const WRAPS: bool>(cx: &mut Cx, block: NodeId, first: NodeId, last: NodeId, width: f32, bfc: (f32, f32)) -> Lines {
    let tree = cx.tree;
    let strut = reach(cx, block);
    let own = tree.style(block);
    let align = match own.get(Property::TextAlign) {
        // `start` is the right edge in an `rtl` block, and `end` the left edge.
        Value::Keyword(side @ (Keyword::Start | Keyword::End)) => {
            if (side == Keyword::End) != (own.get(Property::Direction) == Value::Keyword(Keyword::Rtl)) { Keyword::Right } else { Keyword::Left }
        }
        Value::Keyword(k) => k,
        _ => Keyword::Left,
    };
    let mut b = Breaker::<WRAPS> {
        cx,
        block,
        align,
        width,
        line: Vec::new(),
        x: 0.0,
        content: false,
        trail: None,
        pending: Vec::new(),
        pending_w: 0.0,
        pending_wrap: false,
        pending_word_start: true,
        pending_content: false,
        open: Vec::new(),
        opened: 0,
        segments: Vec::new(),
        above: strut.0,
        below: strut.1,
        aligned: Vec::new(),
        rooted: 0,
        place: (0.0, NONE),
        strut,
        y: 0.0,
        any: false,
        baseline: None,
        bfc,
        left: 0.0,
        avail: width,
        deferred: Vec::new(),
        statics: Vec::new(),
        line_statics: Vec::new(),
    };
    b.band();
    let mut flow = Flow::new();
    for event in Events::new(&tree, first, last) {
        match event {
            Event::Text(node) => {
                let NodeData::Text(span) = *tree.doc.data(node) else { continue };
                let style = tree.style(node);
                let (ws, transform, metrics) = (WhiteSpace::of(style), Transform::of(style), Metrics::of(style));
                let text = tree.doc.text(span);
                let wrap = if WRAPS { Wrap::of(style, ws) } else { Wrap::Normal };
                if tree.styles.spaced() {
                    let sp = font::spacing(true, style);
                    if sp != (0.0, 0.0) {
                        spaced_pieces(&mut b, node, text, (ws, transform, metrics), (sp, wrap), &mut flow);
                        continue;
                    }
                }
                if wrap != Wrap::Normal {
                    spaced_pieces(&mut b, node, text, (ws, transform, metrics), ((0.0, 0.0), wrap), &mut flow);
                    continue;
                }
                for piece in text::pieces(text, ws, transform, &mut flow) {
                    match piece {
                        Piece::Word { start, end, width } => b.add_text(node, start, end, metrics.px(width)),
                        Piece::Space { start, end, width } if ws.wrap => {
                            b.commit();
                            b.add_space(node, start, end, metrics.px(width));
                        }
                        Piece::Space { start, end, width } => b.add_text(node, start, end, metrics.px(width)),
                        Piece::Tab { at } => {
                            // The next tab stop: every 8 letters.
                            let stop = metrics.px(8 * LETTER);
                            let x = b.x + b.pending_w;
                            let w = if stop > 0.0 { ((x / stop).floor() + 1.0) * stop - x } else { 0.0 };
                            // A font size of 0 puts the tab stops 0 pixels apart: no width, and no division by 0.
                            b.add_text(node, at, at + 1, w);
                        }
                        Piece::Newline { at } => {
                            b.add_text(node, at, at, 0.0);
                            b.commit();
                            b.content = true;
                            b.finish();
                            if WRAPS { b.pending_word_start = true; }
                        }
                    }
                }
            }
            Event::Open(node) | Event::Close(node) => {
                let info = inline_box(b.cx, node, b.width);
                if info.culled {
                    continue;
                }
                if matches!(event, Event::Open(_)) && b.cx.tree.position(node) == Keyword::Relative {
                    let offset = block::relative_offset(b.cx, tree.style(node), Space { width: b.width, height: None });
                    if offset != (0.0, 0.0) {
                        b.cx.set_rel(node, offset);
                    }
                }
                if info.paint || info.edges != (0.0, 0.0) || info.margins != (0.0, 0.0) {
                    b.pending_content = true;
                }
                if matches!(event, Event::Open(_)) {
                    b.pending.push(Item::Open { node, x: b.pending_w + info.margins.0 });
                    b.pending_w += info.margins.0 + info.edges.0;
                } else {
                    b.pending.push(Item::Close { x: b.pending_w + info.edges.1 });
                    b.pending_w += info.edges.1 + info.margins.1;
                }
            }
            Event::Atomic(node) => {
                b.commit();
                let item = atomic(b.cx, node, b.width);
                b.pending_w += item.w;
                b.pending.push(Item::Atomic(Box::new(item)));
                b.pending_content = true;
                b.commit();
                flow.after_space = false;
                if WRAPS { b.pending_word_start = false; }
            }
            Event::Float(node) => {
                // Not a break opportunity: the unit that waits stays as it is.
                let item = atomic(b.cx, node, b.width);
                let left = tree.style(node).get(Property::Float) == Value::Keyword(Keyword::Left);
                let used = b.x + b.pending_w;
                if used == 0.0 || item.w <= b.avail - used + 0.01 {
                    b.float(node, left, item);
                } else {
                    b.deferred.push((node, left, item));
                }
            }
            Event::OutOfFlow(node) => {
                // Its static position (CSS 2.1, 10.3.7). A box that was inline before `position` made it a block sits
                // where the line is now. One that was a block starts a line of its own: this one if it is still empty.
                let was_inline = matches!(tree.style(node).get(Property::Display), Value::Keyword(Keyword::Inline | Keyword::InlineBlock | Keyword::InlineFlex | Keyword::InlineGrid));
                let used = b.x + b.pending_w;
                if was_inline || (!b.content && used == 0.0) {
                    if was_inline {
                        b.line_statics.push(b.cx.out.len());
                    }
                    b.place_static(node, b.left + used);
                } else {
                    b.statics.push(node);
                }
            }
            Event::Break(node) => {
                if b.cx.uncull {
                    b.add_text(node, 0, 1, 0.0);
                }
                b.commit();
                b.content = true;
                b.finish();
                flow = Flow::new();
                if WRAPS { b.pending_word_start = true; }
            }
            Event::Block(node) => {
                b.commit();
                b.finish();
                b.block_in_inline(node);
                flow = Flow::new();
                if WRAPS { b.pending_word_start = true; }
            }
        }
    }
    b.commit();
    b.finish();
    let any = b.any;
    Lines { height: if any { b.y } else { 0.0 }, any, baseline: b.baseline }
}

impl<const WRAPS: bool> Breaker<'_, '_, WRAPS> {
    /// Add text to the unit that waits. Text of the same node right after it joins the same item.
    #[inline(always)]
    fn add_text(&mut self, node: NodeId, start: u32, end: u32, w: f32) {
        self.pending_content = true;
        if let Some(Item::Text { node: n, end: e, w: pw, .. }) = self.pending.last_mut()
            && *n == node
            && *e == start
        {
            *e = end;
            *pw += w;
        } else {
            self.pending.push(Item::Text { node, start, end, x: self.pending_w, w });
        }
        self.pending_w += w;
    }

    /// A space that wraps: it goes on the line right away (the unit before it is already there), and a line may break
    /// after it. If the line ends here, the space hangs: it does not count.
    #[inline(always)]
    fn add_space(&mut self, node: NodeId, start: u32, end: u32, w: f32) {
        if WRAPS { self.pending_word_start = true; }
        if let Some(Item::Text { node: n, end: e, w: pw, .. }) = self.line.last_mut()
            && *n == node
            && *e == start
        {
            *e = end;
            *pw += w;
        } else {
            self.mark();
            self.line.push(Item::Text { node, start, end, x: self.x, w });
        }
        self.trail = Some((self.line.len() - 1, start, w));
        self.x += w;
    }

    /// A box reaches `above` and `below` its baseline, which is `root`'s: the line (`NONE`) or a `top` or `bottom` box.
    fn reach(&mut self, root: u32, above: f32, below: f32) {
        let (a, b) = match self.aligned.get_mut(root as usize) {
            Some(r) => (&mut r.above, &mut r.below),
            None => (&mut self.above, &mut self.below),
        };
        *a = a.max(above);
        *b = b.max(below);
    }

    /// A box reaches `above` and `below` its baseline, which is `shift` above the baseline of the line. In a `top` or `bottom`
    /// box (`root`) the reach counts in the box and, as in Chrome, on the line too: the text sits lower under a tall or
    /// raised inline box in it. Not for a culled box or an atomic inline (`line` is false): they do not.
    #[inline(always)]
    fn reach_in(&mut self, root: u32, above: f32, below: f32, shift: f32, line: bool) {
        let Some(r) = self.aligned.get(root as usize) else { return self.reach(NONE, above + shift, below - shift) };
        let rel = shift - r.shift;
        self.reach(root, above + rel, below - rel);
        if line {
            self.reach(NONE, above + shift, below - shift);
        }
    }

    /// Text goes on the line next, at the place of the innermost open box. Only a change of place is kept: text with no
    /// `vertical-align` around it keeps no list.
    fn mark(&mut self) {
        let place = self.open.last().map_or((0.0, NONE), |o| (o.shift, o.root));
        if place != self.place {
            self.line.push(Item::Place { shift: place.0, root: place.1 });
            self.place = place;
        }
    }

    /// An inline box opens on this line: where it sits, and how far it and the boxes in it reach.
    #[inline(always)]
    fn open_box(&mut self, node: NodeId, x: f32) {
        let info = inline_box(self.cx, node, self.width);
        let al = if info.raised { align(self.cx, node, info.above, info.below) } else { Align::Shift(0.0) };
        let (shift, root) = self.open.last().map_or((0.0, NONE), |o| (o.shift, o.root));
        let (shift, root, above, below) = match al {
            Align::Shift(s) => {
                let s = shift + s;
                self.reach_in(root, info.above, info.below, s, !info.culled);
                // What is in a `top` or `bottom` box starts afresh: the box itself reaches in its own entry.
                let (above, below) = match self.open.last() {
                    Some(o) if !matches!(o.align, Align::Top | Align::Bottom) => (o.above.max(info.above + s), o.below.max(info.below - s)),
                    _ => (info.above + s, info.below - s),
                };
                (s, root, above, below)
            }
            _ => {
                self.aligned.push(Aligned { bottom: al == Align::Bottom, above: info.above, below: info.below, shift });
                self.rooted += 1;
                (shift, self.aligned.len() as u32 - 1, info.above, info.below)
            }
        };
        self.open.push(Open { node, x0: x, paint: info.paint || info.positioned || self.cx.uncull, order: self.opened, above, below, align: al, shift, root });
        self.opened += 1;
    }

    /// An atomic inline goes on the line: where it sits, and how far it reaches.
    #[inline(never)]
    fn place_atomic(&mut self, a: &mut Atomic) {
        let (above, below) = (a.baseline, a.h - a.baseline);
        match a.align {
            Align::Shift(s) => {
                let (shift, root) = self.open.last().map_or((0.0, NONE), |o| (o.shift, o.root));
                (a.shift, a.root) = (shift + s, root);
                self.reach_in(root, above, below, a.shift, false);
            }
            al => {
                self.aligned.push(Aligned { bottom: al == Align::Bottom, above, below, shift: 0.0 });
                a.root = self.aligned.len() as u32 - 1;
            }
        }
    }

    /// A break opportunity: put the unit that waits on this line, or on a new line if it does not fit.
    #[inline(always)]
    fn commit(&mut self) {
        if WRAPS && self.pending_wrap && self.x + self.pending_w > self.avail + 0.01 && self.wrap_pending() { return; }
        self.commit_ready();
    }

    /// Only an overflowing opted-in unit pays for joined text and ICU. Each source
    /// item and grapheme is visited once. The ordinary Item representation stays small.
    #[cold]
    fn wrap_pending(&mut self) -> bool {
        let tree = self.cx.tree;
        let mut runs = Vec::new();
        let mut before = None;
        for (i, item) in self.pending.iter().enumerate() {
            match *item {
                Item::Open {node, x} => {
                    if before.is_none() {
                        before = Some(Point {item:i, byte:None, x:x - inline_box(self.cx, node, self.width).margins.0});
                    }
                }
                Item::Text {node, start, end, x, ..} => {
                    let NodeData::Text(span) = *tree.doc.data(node) else { continue };
                    let style = tree.style(node);
                    runs.push(Run {
                        item:i, start, text:&tree.doc.text(span)[start as usize..end as usize], x,
                        before:before.take().unwrap_or(Point {item:i, byte:Some(start), x}),
                        metrics:Metrics::of(style), transform:Transform::of(style),
                        spacing:font::spacing(tree.styles.spaced(), style),
                        policy:Wrap::of(style, WhiteSpace::of(style)),
                    });
                }
                _ => {}
            }
        }
        let emergency = self.pending_w > self.width + 0.01;
        let mask = breaks::ORDINARY | if emergency { breaks::EMERGENCY } else { 0 };
        let mut cuts: Vec<_> = breaks::opportunities(&runs, self.pending_word_start, self.x).into_iter()
            .filter(|op| op.kinds & mask != 0).map(|op| op.point).collect();
        if cuts.is_empty() { return false; }
        let ordinary = runs.iter().any(|run| run.policy == Wrap::All);
        if !ordinary && self.content { self.finish(); }
        cuts.push(Point {item:self.pending.len(), byte:None, x:self.pending_w});
        let mut source: std::collections::VecDeque<_> = std::mem::take(&mut self.pending).into_iter().enumerate().collect();
        let mut from = 0.0;
        let mut next = 0;
        while next < cuts.len() {
            let room = self.avail - self.x;
            if cuts[next].x - from > room + 0.01 && self.content {
                self.finish();
                continue;
            }
            let mut last = next;
            while last + 1 < cuts.len() && cuts[last + 1].x - from <= room + 0.01 { last += 1; }
            let cut = cuts[last];
            while source.front().is_some_and(|(i, _)| *i < cut.item) {
                let (_, mut item) = source.pop_front().unwrap();
                item.shift(-from);
                self.pending.push(item);
            }
            if let Some(stop) = cut.byte
                && source.front().is_some_and(|(i, _)| *i == cut.item)
            {
                let (i, item) = source.pop_front().unwrap();
                let Item::Text {node, start, end, x, w} = item else { unreachable!("a text cut names text") };
                let width = cut.x - x;
                if stop > start { self.pending.push(Item::Text {node, start, end:stop, x:x - from, w:width}); }
                if stop < end { source.push_front((i, Item::Text {node, start:stop, end, x:cut.x, w:w - width})); }
            }
            self.pending_w = cut.x - from;
            self.pending_content = true;
            self.pending_wrap = false;
            self.commit_ready();
            next = last + 1;
            from = cut.x;
            if next < cuts.len() { self.finish(); }
        }
        true
    }

    fn commit_ready(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        if self.content && self.x + self.pending_w > self.avail + 0.01 {
            // Too wide, and the line has something on it: this line is done. (A unit that is too wide for an empty line
            // stays, and sticks out: `overflow-wrap: normal`.) The 0.01 is for rounding: sums of `f32` are not exact.
            self.finish();
        }
        // An empty line next to floats, and the unit does not fit: the line goes down below the floats until it fits.
        while !self.content && self.x + self.pending_w > self.avail + 0.01 && self.avail < self.width {
            let Some(next) = self.cx.floats.next_bottom(self.bfc.1 + self.y, self.strut.0 + self.strut.1) else { break };
            self.y = next - self.bfc.1;
            self.band();
        }
        // Only content ends the hanging space: the ends and starts of inline boxes after it do not.
        if self.pending.iter().any(|i| matches!(i, Item::Text { .. } | Item::Atomic(_))) {
            self.trail = None;
        }
        let dx = self.x;
        let mut pending = std::mem::take(&mut self.pending);
        // `take` leaves an empty list in its place, so the loop can change `self`. The list goes back after, to keep its room.
        for mut item in pending.drain(..) {
            item.shift(dx);
            match item {
                // Text right after text of the same node (a word after its space) joins it: one fragment, not two.
                Item::Text { node, start, end, w, .. } => {
                    if let Some(Item::Text { node: n, end: e, w: pw, .. }) = self.line.last_mut()
                        && *n == node
                        && *e == start
                    {
                        *e = end;
                        *pw += w;
                        continue;
                    }
                }
                Item::Open { node, x } => {
                    self.open_box(node, x);
                    continue;
                }
                Item::Close { x, .. } => {
                    if let Some(o) = self.open.pop() {
                        self.rooted -= matches!(o.align, Align::Top | Align::Bottom) as u32;
                        if o.paint {
                            self.segments.push((o.order, o.node, o.x0, x, o.shift, o.root));
                        }
                    }
                    continue;
                }
                // On the baseline of the line, with no box around it: only the height of the line changes.
                Item::Atomic(ref mut a) if self.open.is_empty() && a.align == Align::Shift(0.0) => {
                    self.above = self.above.max(a.baseline);
                    self.below = self.below.max(a.h - a.baseline);
                }
                Item::Atomic(ref mut a) => self.place_atomic(a),
                Item::Place { .. } => {}
            }
            if matches!(item, Item::Text { .. }) {
                self.mark();
            }
            self.line.push(item);
        }
        self.pending = pending;
        self.x += self.pending_w;
        self.content |= self.pending_content;
        self.pending_w = 0.0;
        self.pending_content = false;
        if WRAPS { self.pending_wrap = false; }
    }

    /// The line is done: find its height and baseline, write its fragments, and start the next line.
    fn finish(&mut self) {
        // The space at the end hangs: it is not shown, and `text-align` does not count it.
        if let Some((i, start, w)) = self.trail.take()
            && let Some(Item::Text { end, w: tw, x, .. }) = self.line.get_mut(i)
        {
            *end = start;
            *tw -= w;
            let x0 = *x + *tw;
            // The boxes that end after the space end before it: with the space at `x0..x0 + w`, an end at `x0 + w` or
            // later moves back by `w`.
            for seg in &mut self.segments {
                if seg.3 >= x0 + w - 0.01 {
                    seg.3 -= w;
                }
            }
            self.x -= w;
        }
        if self.content {
            self.write();
        }
        self.line_statics.clear();
        // The next line. The boxes that are still open go on to it: they start at its left edge, and they reach as far
        // above and below as they did.
        self.line.clear();
        self.segments.clear();
        self.x = 0.0;
        self.content = false;
        for o in &mut self.open {
            o.x0 = 0.0;
        }
        self.aligned.clear();
        self.place = (0.0, NONE);
        if self.rooted == 0 {
            let (a, b) = self.open.last().map_or((0.0, 0.0), |o| (o.above, o.below));
            self.above = self.strut.0.max(a);
            self.below = self.strut.1.max(b);
        } else {
            // A `top` or `bottom` box is open: it is on this line again, and what is inside it with it.
            (self.above, self.below) = self.strut;
            let mut root = NONE;
            for i in 0..self.open.len() {
                let (above, below, align, shift) = {
                    let o = &self.open[i];
                    (o.above, o.below, o.align, o.shift)
                };
                if let Align::Top | Align::Bottom = align {
                    root = self.aligned.len() as u32;
                    self.aligned.push(Aligned { bottom: align == Align::Bottom, above, below, shift });
                } else if root != NONE {
                    self.reach(NONE, above, below);
                }
                self.open[i].root = root;
                self.reach(root, above, below);
            }
        }
        for (node, left, item) in std::mem::take(&mut self.deferred) {
            self.float(node, left, item);
        }
        self.band();
        for node in std::mem::take(&mut self.statics) {
            self.place_static(node, self.left);
        }
    }

    /// The static position of an out-of-flow box, `x` from the left of the line, at the top of the line.
    fn place_static(&mut self, node: NodeId, x: f32) {
        self.cx.fixed |= self.cx.tree.position(node) == Keyword::Fixed;
        self.cx.waiting += 1;
        self.cx.statics += 1;
        let at = self.cx.out.len() as u32;
        self.cx.out.push(Fragment { rect: Rect { x, y: self.y, w: 0.0, h: 0.0 }, node, kind: FragmentKind::Static, end: at + 1 });
    }

    /// The room for the line at `y`, next to the floats.
    fn band(&mut self) {
        let h = self.strut.0 + self.strut.1;
        let (l, r) = self.cx.floats.band(self.bfc.1 + self.y, h, self.bfc.0, self.bfc.0 + self.width);
        self.left = l - self.bfc.0;
        self.avail = r - l;
    }

    /// Place a float, not higher than the top of this line, and write its fragments.
    fn float(&mut self, node: NodeId, left: bool, item: Atomic) {
        let (cl, cr) = clear_sides(self.cx.tree.style(node));
        let top = self.bfc.1 + self.y;
        let top = self.cx.floats.clear(cl, cr).map_or(top, |c| top.max(c));
        let (x, y) = self.cx.floats.place(left, item.w, item.h, top, self.bfc.0, self.bfc.0 + self.width);
        let Atomic { mut fragments, margin_left, margin_top, .. } = item;
        let base = self.cx.out.len() as u32;
        for f in &mut fragments {
            f.end += base;
        }
        fragments[0].rect.x = x - self.bfc.0 + margin_left;
        fragments[0].rect.y = y - self.bfc.1 + margin_top;
        self.cx.out.append(&mut fragments);
        self.cx.spare.push(fragments);
        self.band();
    }

    /// Write the fragments of a line that has content.
    fn write(&mut self) {
        let tree = self.cx.tree;
        // The pieces of the boxes that are still open, up to the end of the line.
        let line_end = self.x;
        for o in self.open.iter().filter(|o| o.paint) {
            self.segments.push((o.order, o.node, o.x0, line_end, o.shift, o.root));
        }
        self.segments.sort_unstable_by_key(|s| s.0);
        let (mut above, mut below) = (self.above, self.below);
        // The `top` and `bottom` boxes go last, the tallest first: the line grows below the baseline for a `top` box and
        // above it for a `bottom` box, as in Chrome. The boxes do not change the baseline for the others.
        if !self.aligned.is_empty() {
            let mut order: Vec<usize> = (0..self.aligned.len()).collect();
            let height = |i: usize| self.aligned[i].above + self.aligned[i].below;
            order.sort_by(|&i, &j| height(j).total_cmp(&height(i)));
            for i in order {
                let h = height(i);
                if self.aligned[i].bottom {
                    above = above.max(h - below);
                } else {
                    below = below.max(h - above);
                }
            }
        }
        let baseline = above;
        // The baseline of a box in `root`: of the line, or of the `top` or `bottom` box.
        let at_root = |root: u32| match self.aligned.get(root as usize) {
            None => baseline,
            Some(a) if a.bottom => above + below - a.below,
            Some(a) => a.above,
        };
        // How far up the text sits from the top of the line, until the next `Item::Place`.
        let mut lift = baseline;

        // `text-align`: move the whole line.
        let free = (self.avail - self.x).max(0.0);
        let dx = self.left
            + match self.align {
                Keyword::Right => free,
                Keyword::Center => free / 2.0,
                _ => 0.0,
                // `left` and `justify` (not built: see `docs/progress.md`).
            };

        for &i in &self.line_statics {
            self.cx.out[i].rect.x += dx - self.left;
        }
        // The fragments: the inline boxes first (paint draws their background under the text), then the content.
        let y = self.y;
        for &(_, node, x0, x1, shift, root) in &self.segments {
            let style = tree.style(node);
            let m = Metrics::of(style);
            let (border, padding) = border_padding(self.cx, style, self.width);
            let top = at_root(root) - shift - m.ascent - border.top - padding.top;
            let h = m.ascent + m.descent + border.vertical() + padding.vertical();
            let at = self.cx.out.len() as u32;
            let (rx, ry) = moved(self.cx, self.block, node);
            self.cx.out.push(Fragment { rect: Rect { x: dx + x0 + rx, y: y + top + ry, w: (x1 - x0).max(0.0), h }, node, kind: FragmentKind::Box, end: at + 1 });
        }
        if let Some(item) = self.cx.marker.take() {
            marker(self.cx, item, dx, y + baseline);
        }
        for item in self.line.drain(..) {
            match item {
                Item::Place { shift, root } => lift = at_root(root) - shift,
                Item::Text { node, start, end, x, w } if end > start => {
                    let m = Metrics::of(tree.style(node));
                    let at = self.cx.out.len() as u32;
                    let (rx, ry) = moved(self.cx, self.block, tree.doc.flat_parent(node).expect("a text node has a parent"));
                    self.cx.out.push(Fragment {
                        rect: Rect { x: dx + x + rx, y: y + lift - m.ascent + ry, w, h: m.ascent + m.descent },
                        node,
                        kind: FragmentKind::Text { start, end },
                        end: at + 1,
                    });
                }
                Item::Atomic(a) => {
                    let Atomic { mut fragments, x, baseline: b, margin_left, margin_top, shift, root, .. } = *a;
                    let base = self.cx.out.len() as u32;
                    for f in &mut fragments {
                        f.end += base;
                    }
                    let (rx, ry) = moved(self.cx, self.block, tree.doc.flat_parent(fragments[0].node).expect("an inline-block has a parent"));
                    fragments[0].rect.x = dx + x + margin_left + rx;
                    fragments[0].rect.y = y + at_root(root) - shift - b + margin_top + ry;
                    self.cx.out.append(&mut fragments);
                    self.cx.spare.push(fragments);
                }
                _ => {}
            }
        }
        self.y += above + below;
        self.baseline = Some(y + baseline);
        self.any = true;
    }

    /// A block inside an inline box: it goes under the line before, as wide as the lines, and the next line goes under it.
    /// (CSS 2.1 splits the inline box in two around it. Chrome since 2022 keeps the block in the line content, as here.)
    fn block_in_inline(&mut self, node: NodeId) {
        if self.cx.depth >= MAX_DEPTH {
            return;
        }
        let from = self.cx.out.len();
        self.cx.depth += 1;
        let pos = Pos { x: self.bfc.0, y: self.bfc.1 + self.y, strut: Default::default() };
        let done = block::block(self.cx, node, Space { width: self.width, height: None }, pos);
        self.cx.depth -= 1;
        let f = &mut self.cx.out[from];
        f.rect.y = self.y + done.margin_top;
        self.y += done.margin_top + done.height + done.margin_bottom;
        if done.height + done.margin_top + done.margin_bottom != 0.0 || done.baseline.is_some() {
            self.any = true;
        }
        if let Some(b) = done.baseline {
            self.baseline = Some(self.y - done.margin_bottom - done.height + b);
        }
        self.band();
    }
}

/// Lay out an atomic inline (`inline-block`) for a line `cb` pixels wide. With `width: auto` it is as wide as its content,
/// but not wider than the line: "shrink to fit" (CSS 2.1, 10.3.9).
fn atomic(cx: &mut Cx, node: NodeId, cb: f32) -> Atomic {
    let style = cx.tree.style(node);
    let (border, padding) = border_padding(cx, style, cb);
    let bp = border.horizontal() + padding.horizontal();
    let margin = |p| resolve(cx, style.get(p), Some(cb)).unwrap_or(0.0);
    let (ml, mr) = (margin(Property::MarginLeft), margin(Property::MarginRight));
    let (w, _, _) = block::widths(cx, style, cb, bp);
    let width = if style.get(Property::Width) == Value::Keyword(Keyword::Auto) {
        let (min, max) = intrinsic::sizes(cx, node);
        let fit = max.min(min.max(cb - ml - mr - bp));
        // `min-width` and `max-width` still apply.
        let border_box = style.get(Property::BoxSizing) == Value::Keyword(Keyword::BorderBox);
        let content = |v: f32| if border_box { (v - bp).max(0.0) } else { v };
        let hi = resolve(cx, style.get(Property::MaxWidth), Some(cb)).map(content).unwrap_or(f32::INFINITY);
        let lo = resolve(cx, style.get(Property::MinWidth), Some(cb)).map(content).unwrap_or(0.0);
        fit.min(hi).max(lo)
    } else {
        w
    };
    // Lay it out on its own list, so that its fragments can go in after the inline boxes of the line.
    let list = cx.spare.pop().unwrap_or_default();
    let saved = std::mem::replace(&mut cx.out, list);
    let marker = cx.marker.take();
    // A marker of a list item around this inline-block goes on the line, not inside the inline-block.
    let done = if cx.depth < MAX_DEPTH {
        cx.depth += 1;
        let done = block::sized(cx, node, Space { width: cb, height: None }, Some((width, ml, mr)), None, Pos::default());
        cx.depth -= 1;
        done
    } else {
        block::sized(cx, node, Space { width: 0.0, height: Some(0.0) }, Some((0.0, 0.0, 0.0)), None, Pos::default())
    };
    cx.marker = marker;
    let fragments = std::mem::replace(&mut cx.out, saved);
    let h = done.margin_top + done.height + done.margin_bottom;
    // The baseline is the last line inside. With no line, or in a scroll container, the bottom of the margin box (CSS 2.1,
    // 10.8.1). An `inline-flex` or `inline-grid` takes its baseline from its items, scroll container or not, as Chrome does.
    let items = || matches!(style.get(Property::Display), Value::Keyword(Keyword::InlineFlex | Keyword::InlineGrid));
    let baseline = done.baseline.filter(|_| !cx.tree.scrolls(node) || items()).map_or(h, |b| done.margin_top + b);
    // The width can grow by a scrollbar in layout: the fragment has the final one.
    let width = fragments[done.fragment].rect.w - bp;
    let align = if matches!(style.get(Property::VerticalAlign), Value::Keyword(Keyword::Baseline)) { Align::Shift(0.0) } else { align(cx, node, baseline, h - baseline) };
    Atomic { fragments, x: 0.0, w: ml + width + bp + mr, h, baseline, margin_left: ml, margin_top: done.margin_top, align, shift: 0.0, root: NONE }
}

/// How far the inline boxes with `position: relative` around `node` (and `node` itself) move it, up to `block`.
fn moved(cx: &Cx, block: NodeId, mut node: NodeId) -> (f32, f32) {
    let (mut x, mut y) = (0.0, 0.0);
    if cx.rel.is_empty() {
        return (x, y);
    }
    while node != block {
        if let Some(&(dx, dy)) = cx.rel.get(&(node.index() as u32)) {
            x += dx;
            y += dy;
        }
        let Some(parent) = cx.tree.doc.flat_parent(node) else { break };
        node = parent;
    }
    (x, y)
}

/// The marker of a list item, on the first line inside it: outside the line, to its left, on the baseline.
fn marker(cx: &mut Cx, item: NodeId, x: f32, baseline: f32) {
    let style = cx.tree.style(item);
    let Some(text) = crate::boxes::marker_text(style, cx.number) else { return };
    let m = Metrics::of(style);
    let w = m.px(font::width(&text));
    let at = cx.out.len() as u32;
    cx.out.push(Fragment { rect: Rect { x: x - w, y: baseline - m.ascent, w, h: m.ascent + m.descent }, node: item, kind: FragmentKind::Marker { number: cx.number }, end: at + 1 });
}

/// The marker of a list item with no line inside: at the top, outside the content box. `dx`, `dy`: the border and padding.
pub fn lone_marker(cx: &mut Cx, item: NodeId, dx: f32, dy: f32) {
    let m = Metrics::of(cx.tree.style(item));
    marker(cx, item, dx, dy + m.ascent);
}

#[cfg(test)]
mod tests {
    use crate::block::tests::{dump, rect, run};
    use crate::fragment::{Kind, Rect};

    /// The fragments inside the `body`, without the lines of `html` and `body`.
    fn inside(html: &str, css: &str) -> String {
        dump(html, css).lines().skip(2).map(|l| format!("{}\n", l.trim_start())).collect()
    }

    #[test]
    fn long_identifiers_wrap_only_when_requested() {
        let html = format!("<div id=x>{}</div>", "x".repeat(120));
        let base = "div { width:200px; font-size:14px; line-height:20px }";
        assert_eq!(rect(&html, base, "x").h, 20.0);
        for rule in ["overflow-wrap:anywhere", "overflow-wrap:break-word", "word-break:break-all"] {
            let css = format!("{base} div {{ {rule} }}");
            assert_eq!(rect(&html, &css, "x").h, 120.0, "{rule}");
        }
    }

    #[test]
    fn wrapping_keeps_clusters_together_across_inline_nodes() {
        use bare_html::dom::NodeData;
        for (html, original, cluster) in [
            ("<div id=x>e<span>\u{301}</span>x</div>", "e\u{301}x", "e\u{301}"),
            ("<div id=x>👩<span>\u{200d}</span>💻x</div>", "👩\u{200d}💻x", "👩\u{200d}💻"),
        ] {
            let css = "div { width:8.4px;font-size:14px;line-height:20px;overflow-wrap:anywhere }";
            let (doc, layout) = run(html, css);
            let mut joined = String::new();
            let mut cluster_y = None;
            for fragment in &layout.fragments {
                let Kind::Text {start, end} = fragment.kind else { continue };
                let NodeData::Text(span) = *doc.data(fragment.node) else { panic!("text node") };
                let text = &doc.text(span)[start as usize..end as usize];
                if joined.len() < cluster.len() {
                    let y = *cluster_y.get_or_insert(fragment.rect.y);
                    assert_eq!(fragment.rect.y, y, "cluster split before {text:?}");
                } else {
                    assert!(fragment.rect.y > cluster_y.unwrap(), "next cluster needs a new line");
                }
                joined.push_str(text);
            }
            assert_eq!(joined, original, "all source bytes occur once");
            assert_eq!(rect(html, css, "x").h, 40.0);
        }
    }

    #[test]
    fn break_all_uses_the_current_line_but_emergency_wrapping_moves_the_word() {
        let html = format!("<div id=x>aaa <span>{}</span></div>", "x".repeat(120));
        let base = "div {width:200px;font-size:14px;line-height:20px}";
        assert_eq!(rect(&html, &format!("{base} div {{word-break:break-all}}"), "x").h, 120.0);
        assert_eq!(rect(&html, &format!("{base} div {{overflow-wrap:anywhere}}"), "x").h, 140.0);
    }

    #[test]
    fn wrapping_preserves_inline_edges_spacing_and_case_expansion() {
        let html = "<div id=x><span style='border:1px solid;padding:1px'>xxxxxxxxxx</span></div>";
        assert_eq!(rect(html, "div {width:25px;font-size:14px;line-height:20px;overflow-wrap:anywhere}", "x").h, 100.0);
        let html = format!("<div id=x>{}</div>", "x".repeat(120));
        assert_eq!(rect(&html, "div {width:200px;font-size:14px;line-height:20px;letter-spacing:2px;word-break:break-all}", "x").h, 140.0);
        assert_eq!(rect(&html, "div {width:200px;font-size:14px;line-height:20px;letter-spacing:-2px;word-break:break-all}", "x").h, 80.0);
        let html = "<div id=x>ßßß</div>";
        assert_eq!(rect(html, "div {width:17px;font-size:14px;line-height:20px;text-transform:uppercase;overflow-wrap:anywhere}", "x").h, 60.0);
    }

    #[test]
    fn a_long_wrapped_identifier_keeps_each_source_byte_once() {
        use bare_html::dom::NodeData;
        let html = format!("<div id=x>{}</div>", "x".repeat(20_000));
        let (doc, layout) = run(&html, "div {width:200px;font-size:14px;line-height:20px;word-break:break-all}");
        let mut bytes = 0;
        for f in &layout.fragments {
            if let Kind::Text {start,end} = f.kind {
                let NodeData::Text(span) = *doc.data(f.node) else { panic!("text") };
                assert!(doc.text(span).is_char_boundary(start as usize) && doc.text(span).is_char_boundary(end as usize));
                assert!(f.rect.w <= 200.01);
                assert_eq!(start, bytes, "source ranges are contiguous and non-overlapping");
                bytes += end - start;
            }
        }
        assert_eq!(bytes, 20_000);
        assert_eq!(rect(&html, "div {width:200px;font-size:14px;line-height:20px;word-break:break-all}", "x").h, 17_400.0);
    }

    #[test]
    fn a_preserved_newline_restores_capitalization_before_wrapping() {
        use bare_html::dom::NodeData;
        let html = "<div><i style='display:inline-block'>z</i><span style='overflow-wrap:normal;white-space:pre'>\n</span>ßßß</div>";
        let (doc, layout) = run(html, "div {width:17px;font-size:14px;line-height:20px;text-transform:capitalize;overflow-wrap:anywhere}");
        let first = layout.fragments.iter().find_map(|f| {
            let Kind::Text {start,end} = f.kind else { return None };
            let NodeData::Text(span) = *doc.data(f.node) else { return None };
            (doc.text(span) == "ßßß").then(|| &doc.text(span)[start as usize..end as usize])
        });
        assert_eq!(first, Some("ß"), "the expanded first cluster fills the line");
    }

    #[test]
    fn an_opted_in_tab_can_break_before_a_normal_word() {
        let html = format!("<div id=x><span style='white-space:pre-wrap;word-break:break-all'>\t</span><span>{}</span></div>", "x".repeat(100));
        assert_eq!(rect(&html, "div {width:70px;font-size:14px;line-height:20px}", "x").h, 40.0);
    }

    #[test]
    fn additional_breaks_change_flex_and_grid_minimums_except_break_word() {
        let html = format!("<section><div id=x>{}</div></section>", "x".repeat(120));
        for display in ["flex", "grid"] {
            let base = format!("section {{display:{display};width:200px;font-size:14px;line-height:20px;grid-template-columns:1fr}}");
            for policy in ["overflow-wrap:anywhere", "word-break:break-all"] {
                let css = format!("{base} div {{ {policy} }}");
                assert_eq!(rect(&html, &css, "x").w, 200.0, "{display}: {policy}");
                assert_eq!(rect(&html, &css, "x").h, 120.0);
            }
            let css = format!("{base} div {{overflow-wrap:break-word}}");
            assert!(rect(&html, &css, "x").w > 1000.0, "break-word keeps the unbroken minimum");
        }
    }

    #[test]
    fn a_tiny_box_consumes_whole_clusters_without_losing_source() {
        let html = format!("<div id=x>{}</div>", "x".repeat(20));
        assert_eq!(rect(&html, "div {width:1px;font-size:14px;line-height:20px;word-break:break-all}", "x").h, 400.0);
    }

    #[test]
    fn wrap_requests_respect_nowrap_and_pre() {
        let html = format!("<div id=x>{}</div>", "x".repeat(120));
        for ws in ["nowrap", "pre"] {
            let css = format!("div {{width:200px;font-size:14px;line-height:20px;overflow-wrap:anywhere;word-break:break-all;white-space:{ws}}}");
            assert_eq!(rect(&html, &css, "x").h, 20.0);
        }
    }

    #[test]
    fn words_wrap_at_spaces() {
        // A letter is 9.6px. "aaa bbb" is 67.2px, and " ccc" does not fit in 100px.
        let out = inside("<div style='width:100px'>aaa bbb ccc</div>", "");
        assert_eq!(out, "div 8,8 100x44\n\"aaa bbb\" 8,8 67.2x22\n\"ccc\" 8,30 28.8x22\n");
    }

    #[test]
    fn letter_spacing_comes_after_each_letter_and_word_spacing_after_each_space() {
        // Chrome 155 (`tools/fixtures/ui/letterspace-cases.txt`): "abcd" at 2px is 4 * 11.6 = 46.4 wide, the last letter included.
        let width = |css: &str, text: &str| rect(&format!("<span id=s style='display:inline-block;{css}'>{text}</span>"), "", "s").w;
        assert!((width("letter-spacing:2px", "abcd") - 46.4).abs() < 0.01);
        assert!((width("letter-spacing:-1px", "abcd") - 34.4).abs() < 0.01);
        assert!((width("word-spacing:10px", "a b  c") - 68.0).abs() < 0.01);
        assert!((width("letter-spacing:1px;word-spacing:5px", "ab cd ef") - 94.8).abs() < 0.01);
        // An accent that sits on a letter is no letter, and a no-break space is a space.
        assert!((width("letter-spacing:2px", "e\u{301}e") - 23.2).abs() < 0.01);
        assert!((width("letter-spacing:2px;word-spacing:3px", "a\u{a0}b") - 37.8).abs() < 0.01);
    }

    #[test]
    fn a_long_word_sticks_out() {
        let out = inside("<div style='width:50px'>aaaaaaaaaa bb</div>", "");
        assert_eq!(out, "div 8,8 50x44\n\"aaaaaaaaaa\" 8,8 96x22\n\"bb\" 8,30 19.2x22\n");
    }

    #[test]
    fn nowrap_and_pre_do_not_wrap() {
        let out = inside("<div style='width:50px;white-space:nowrap'>aa bb cc</div>", "");
        assert_eq!(out, "div 8,8 50x22\n\"aa bb cc\" 8,8 76.8x22\n");
        let out = inside("<pre>a\n\nb\n</pre>", "pre { margin: 0 }");
        assert_eq!(out, "pre 8,8 784x66\n\"a\" 8,8 9.6x22\n\"b\" 8,52 9.6x22\n");
    }

    #[test]
    fn spaces_collapse_across_elements() {
        let out = inside("<p>a <b> b</b></p>", "p { margin: 0 }");
        assert_eq!(out, "p 8,8 784x22\n\"a \" 8,8 19.2x22\n\"b\" 27.2,8 9.6x22\n");
    }

    #[test]
    fn text_align() {
        assert_eq!(rect("<div style='width:100px;text-align:center'><span id=s style='background:red'>ab</span></div>", "", "s").x, 8.0 + 40.4);
        assert_eq!(rect("<div style='width:100px;text-align:right'><span id=s style='background:red'>ab</span></div>", "", "s").x, 8.0 + 80.8);
    }

    /// `start` and `end` follow the direction of the block: in `rtl`, the text starts at the right edge.
    #[test]
    fn text_align_start_and_end_follow_the_direction() {
        let x = |style: &str| rect(&format!("<div style='width:100px;{style}'><span id=s style='background:red'>ab</span></div>"), "", "s").x;
        assert_eq!(x("text-align:start"), 8.0);
        assert_eq!(x("text-align:end"), 8.0 + 80.8);
        assert_eq!(x("direction:rtl"), 8.0 + 80.8);
        assert_eq!(x("direction:rtl;text-align:start"), 8.0 + 80.8);
        assert_eq!(x("direction:rtl;text-align:end"), 8.0);
        assert_eq!(x("direction:rtl;text-align:left"), 8.0);
        assert_eq!(x("direction:rtl;text-align:right"), 8.0 + 80.8);
    }

    #[test]
    fn a_bigger_font_makes_the_line_taller() {
        // 16px: ascent 17, descent 5. 32px: ascent 34, descent 9. One baseline, 34 from the top. The line is 43.
        let out = inside("<p>a<span style='font-size:32px;background:red'>b</span></p>", "p { margin: 0 }");
        assert_eq!(out, "p 8,8 784x43\nspan 17.6,8 19.2x43\n\"a\" 8,25 9.6x22\n\"b\" 17.6,8 19.2x43\n");
        // `line-height: 2` at 16px: 32, so 5 more above and below.
        assert_eq!(rect("<p id=p>a</p>", "p { margin: 0; line-height: 2 }", "p").h, 32.0);
    }

    #[test]
    fn line_breaks() {
        assert_eq!(rect("<p id=p>a<br>b</p>", "p { margin: 0 }", "p").h, 44.0);
        assert_eq!(rect("<p id=p><br><br></p>", "p { margin: 0 }", "p").h, 44.0);
        assert_eq!(rect("<p id=p>a<br></p>", "p { margin: 0 }", "p").h, 22.0);
    }

    #[test]
    fn empty_inline_boxes_make_no_line() {
        assert_eq!(rect("<div id=d><span></span> </div>", "", "d").h, 0.0);
        assert_eq!(rect("<div id=d><span style='padding:1px'></span></div>", "", "d").h, 22.0);
    }

    #[test]
    fn an_inline_box_over_two_lines() {
        let out = inside("<div style='width:100px'><span style='padding:0 5px;background:red'>aaa bbb ccc</span></div>", "");
        assert_eq!(out, "div 8,8 100x44\nspan 8,8 72.2x22\n\"aaa bbb\" 13,8 67.2x22\nspan 8,30 33.8x22\n\"ccc\" 8,30 28.8x22\n");
    }

    #[test]
    fn inline_blocks() {
        // Shrink to fit: as wide as "abc". The baseline is its last line, so it sits like text.
        assert_eq!(rect("<span id=s style='display:inline-block'>abc</span>", "", "s"), Rect { x: 8.0, y: 8.0, w: 28.8, h: 22.0 });
        // No line inside: the bottom edge is the baseline. 30 above the baseline and the strut's 5 below.
        assert_eq!(rect("<div id=d><span style='display:inline-block;width:10px;height:30px'></span></div>", "", "d").h, 35.0);
        // Not wider than the line: "aaa bbb" (67.2) in 50px wraps inside.
        assert_eq!(rect("<div style='width:50px'><span id=s style='display:inline-block'>aaa bbb</span></div>", "", "s"), Rect { x: 8.0, y: 8.0, w: 50.0, h: 44.0 });
        // An inline-block is one unit: it goes to the next line as a whole.
        assert_eq!(rect("<div style='width:50px'>aaa<span id=s style='display:inline-block'>bbb</span></div>", "", "s").y, 30.0);
    }

    #[test]
    fn half_leading_above_is_rounded_down() {
        // Chrome: line-height 25 at 16px has 3 more than the font: 1 above and 2 below. The line height is cut to 1/64 px.
        assert_eq!(rect("<div style='line-height:25px'><span id=s style='background:red'>a</span></div>", "", "s").y, 8.0 + 1.0);
        assert_eq!(rect("<div id=d style='font-size:13.3333px;line-height:1.5'>a</div>", "", "d").h, 19.984375);
    }

    #[test]
    fn vertical_align_moves_a_box_and_the_line() {
        // Numbers from Chrome 155 (`tools/fixtures/ui/valign-cases.txt`). `super` of 16px is 16 / 3 + 1 = 6.328125 up, in 1/64 px.
        let html = "<div id=d>a<span id=s style='vertical-align:super;background:red'>b</span><span id=t style='vertical-align:50%;background:red'>c</span></div>";
        assert_eq!((rect(html, "", "d").h, rect(html, "", "s").y - 8.0, rect(html, "", "t").y - 8.0), (28.0 + 5.0, 28.0 - 17.0 - 6.328125, 0.0));
    }

    #[test]
    fn top_and_bottom_wait_for_the_line_height() {
        // The tallest goes first: the `bottom` box of 90 makes 85 above the baseline, and the `top` box of 60 adds nothing.
        let html = "<div id=d><span id=a style='background:red'>a</span><span style='vertical-align:top;line-height:60px'>b</span><span style='vertical-align:bottom;line-height:90px'>c</span></div>";
        assert_eq!((rect(html, "", "d").h, rect(html, "", "a").y - 8.0), (90.0, 85.0 - 17.0));
    }

    #[test]
    fn a_box_with_vertical_align_is_not_culled() {
        let (_, out) = run("<p>a<b style='vertical-align:super'>b</b></p>", "");
        assert!(out.fragments.iter().any(|f| f.kind == Kind::Box));
    }

    #[test]
    fn a_block_inside_an_inline() {
        let out = inside("<span>a<div>b</div>c</span>", "");
        assert_eq!(out, "\"a\" 8,8 9.6x22\ndiv 8,30 784x22\n\"b\" 8,30 9.6x22\n\"c\" 8,52 9.6x22\n");
        // The `span` has nothing to paint: it makes no fragment (culled). With a background it has one on each line.
        let out = inside("<span style='background:red'>a<div>b</div>c</span>", "");
        assert_eq!(out, "span 8,8 9.6x22\n\"a\" 8,8 9.6x22\ndiv 8,30 784x22\n\"b\" 8,30 9.6x22\nspan 8,52 9.6x22\n\"c\" 8,52 9.6x22\n");
    }

    #[test]
    fn list_marker_on_the_first_line() {
        let (_, layout) = run("<ul><li>x</li></ul>", "");
        let marker = layout.fragments.iter().find(|f| matches!(f.kind, Kind::Marker { .. })).unwrap();
        // `• ` is two letters (19.2px), left of the content box of the `li` (8 + 40), on the first line.
        assert_eq!(marker.rect, Rect { x: 48.0 - 19.2, y: 16.0, w: 19.2, h: 22.0 });
    }

    #[test]
    fn a_tab_in_a_zero_font_has_no_width() {
        // Found by the fuzzer: the tab stop was 0 pixels apart, and `x / 0` made the place NaN.
        let (_, layout) = run("<pre>a<b style='font-size:0'>\tx</b></pre>", "");
        assert!(layout.fragments.iter().all(|f| !f.rect.x.is_nan() && !f.rect.w.is_nan()));
    }

    #[test]
    fn huge_lengths_are_capped() {
        // Found by the fuzzer: `padding: 2e45px` is infinity in `f32`, and infinity minus infinity is NaN.
        let css = "i { display: inline-block; padding: 2e45px } b { line-height: 2e300; font-size: 1e40px }";
        let (_, layout) = run("<div>a<i>b</i><b>c</b></div>", css);
        for f in &layout.fragments {
            let r = f.rect;
            assert!([r.x, r.y, r.w, r.h].iter().all(|v| v.is_finite()), "{r:?}");
        }
    }

    #[test]
    fn deep_inline_nesting() {
        let out = run(&format!("{}x", "<span>".repeat(20_000)), "").1;
        assert_eq!(out.fragments.len(), 3, "html, body and the text: the spans are culled");
        let out = run(&format!("{}x", "<span style='background:red'>".repeat(20_000)), "").1;
        assert_eq!(out.fragments.len(), 20_000 + 3);
    }

    #[test]
    fn a_space_at_the_end_of_a_line_hangs_inside_a_box_too() {
        // As in Chrome: "in span" is 67.2 wide, and the space after it does not count, before or after the padding.
        let html = "<div style='width:300px'>text <span id=s style='padding-left:20px;background:red'>in span </span></div>";
        assert!((rect(html, "", "s").w - (20.0 + 67.2)).abs() < 0.01);
        let html = "<div style='width:300px'>text <span id=s style='padding-right:20px;background:red'>in span </span></div>";
        assert!((rect(html, "", "s").w - (67.2 + 20.0)).abs() < 0.01);
        // Not at the end of the line: the space counts.
        let html = "<div style='width:300px'>text <span id=s style='background:red'>in span </span>after</div>";
        assert!((rect(html, "", "s").w - 76.8).abs() < 0.01);
    }
}
