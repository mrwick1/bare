//! Scrolling in the browser host: the page and the scroll containers, the script API, and the `scroll` event.
//!
//! How it works
//! - Layout never changes for a scroll. `bare_layout::scroll::areas` finds the containers and their content size once per layout
//!   (`Frame::areas`). The offsets live here (`Dom::scrolling`, a `bare_paint::Scroll`): the page, and one pair per container.
//!   Paint and hit testing add them (`bare-paint`). A scroll step is: change an offset, draw the display list again.
//! - The page bars. The window has a vertical bar when the page is taller than the window and a horizontal one when it is wider
//!   (`overflow: scroll` always, `hidden` never). A bar takes 15 px of the layout width or height, so the layout runs again at the
//!   smaller size. The state is kept (`Dom::layout_areas`): a page that does not change its bars is laid out once.
//! - Who may scroll. A wheel or a key scrolls what the user may scroll: `overflow: auto|scroll` only. A script scrolls `hidden` boxes
//!   too. Offsets are whole pixels and are clamped to `0..=scrollWidth - clientWidth` (and the same for the height).
//! - The wheel goes to the innermost container under the pointer that can move that way, and to the page when none can
//!   (scroll chaining). Keys scroll the page when the focus is not a field that uses the key: arrows 40 px, Page keys
//!   `max(h * 0.875, h - 40)` rounded down, Space and Shift+Space a page, Home and End the ends. Chrome gave these numbers
//!   (`tools/ui/chrome_scroll.mjs`).
//! - Script API: `scrollTop`, `scrollLeft` (get and set), `scrollWidth`, `scrollHeight`, `clientWidth`, `clientHeight`, `clientTop`,
//!   `clientLeft`, `getBoundingClientRect`, `scrollTo`, `scrollBy`, `scroll`, `scrollIntoView` on elements; `scrollX`, `scrollY`,
//!   `pageXOffset`, `pageYOffset`, `innerWidth`, `innerHeight`, `scrollTo`, `scrollBy`, `scroll` on the window; `document.scrollingElement`.
//!   Reading an offset costs nothing (the offsets are clamped when they are set and at each frame). The other reads and every write
//!   run the cascade and one layout, like `offsetWidth`.
//! - The `scroll` event. A change of an offset queues the target once, in the order of the changes. One task (`Task::Scroll`, 16 ms
//!   after the first change) sends one `scroll` event to each queued target. The window sends them right after the input that
//!   scrolled. The page's event is sent to the document and bubbles to the window; an element's does not bubble.
//!
//! Gates and differences (recorded, none is faked)
//! - `position: sticky` is not modelled (it scrolls like a normal box). `scroll-behavior`, `scroll-snap` and `overscroll-behavior` are
//!   ignored; every scroll is instant (Chrome animates a key scroll, then ends at the same place).
//! - `scrollWidth` and `scrollHeight` of an element that does not clip are its padding box (its overflow is not counted).
//! - The keys scroll the page only, not the container that holds the focus. A script `focus()` does not scroll; Tab does.
//! - `getBoundingClientRect` is a plain object with the eight fields, like `ResizeObserver` entries. Transforms are not in the engine.
//! - `100vh` and `vw` use the window minus the bars; Chrome uses the window.

use crate::browser::*;
use crate::browser_geometry::border_box;
use crate::browser_platform::V;
use bare_css::cascade::Styles;
use bare_css::values::{Keyword, Property, Value as Cv};
use bare_html::dom::{Document, NodeData, NodeId};
use bare_js::eval::Interp;
use bare_js::value::Value;
use bare_layout::fragment::{Layout, Rect};
use bare_layout::scroll::{self, Areas};
use bare_paint::Scroll;
use std::cell::{Ref, RefCell};

/// The width of a classic scrollbar.
const BAR: f32 = bare_layout::block::SCROLLBAR;
/// An arrow key moves 40 px (Chrome: `kPixelsPerLineStep`).
pub const LINE: f32 = 40.0;

/// A key that scrolls the page, found by the key default and used by the window (which has the layout).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ScrollKey {
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    Home,
    End,
}

#[derive(Default)]
struct Inner {
    scroll: Scroll,
    /// The bars of the page at the last layout: vertical, horizontal.
    bars: (bool, bool),
    /// Targets with a `scroll` event to send: `None` is the document.
    pending: Vec<Option<NodeId>>,
    queued: bool,
    key: Option<ScrollKey>,
}

#[derive(Default)]
pub(crate) struct Scrolling {
    s: RefCell<Inner>,
}

/// Everything a scroll decision reads: the page at one time.
pub(crate) struct Geo<'a> {
    pub doc: &'a Document,
    pub styles: &'a Styles,
    pub layout: &'a Layout,
    pub areas: &'a Areas,
    /// The window size, and the page bars.
    pub window: (f32, f32),
    pub bars: (bool, bool),
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Align {
    Start,
    Center,
    End,
    Nearest,
    /// Chrome's focus rule: no move when it is all in view, centered when none of it is, else nearest.
    IfNeeded,
}

fn keyword(styles: &Styles, n: NodeId, p: Property) -> Keyword {
    match styles.style(n).map(|s| s.get(p)) {
        Some(Cv::Keyword(k)) => k,
        _ => Keyword::Visible,
    }
}

fn round(v: f32) -> f32 {
    (v + 0.5).floor()
}

impl Geo<'_> {
    /// The size the page content sees: the window without the bars.
    pub fn view(&self) -> (f32, f32) {
        ((self.window.0 - if self.bars.0 { BAR } else { 0.0 }).max(0.0), (self.window.1 - if self.bars.1 { BAR } else { 0.0 }).max(0.0))
    }

    pub fn page_max(&self) -> (f32, f32) {
        let v = self.view();
        (round(self.areas.page.0 - v.0).max(0.0), round(self.areas.page.1 - v.1).max(0.0))
    }

    fn max_of(&self, n: NodeId) -> (f32, f32) {
        self.areas.get(n).map_or((0.0, 0.0), |a| {
            let m = a.max();
            (round(m.0), round(m.1))
        })
    }

    /// Does the user scroll this box? (`auto` or `scroll` on the axis.)
    fn user(&self, n: NodeId) -> (bool, bool) {
        let k = |p| matches!(keyword(self.styles, n, p), Keyword::Auto | Keyword::Scroll);
        (k(Property::OverflowX), k(Property::OverflowY))
    }

    fn page_user(&self) -> (bool, bool) {
        let (x, y) = self.areas.page_overflow;
        let k = |k| !matches!(k, Keyword::Hidden | Keyword::Clip);
        (k(x), k(y))
    }

    /// Put every offset in range, and forget a box that is not a scroll container any more. Gives whether something moved.
    pub fn clamp(&self, s: &mut Scroll) -> bool {
        let before = s.clone();
        let m = self.page_max();
        (s.x, s.y) = (s.x.clamp(0.0, m.0), s.y.clamp(0.0, m.1));
        s.nodes.retain(|n, _| self.areas.get(*n).is_some());
        for (n, o) in s.nodes.iter_mut() {
            let m = self.max_of(*n);
            *o = (o.0.clamp(0.0, m.0), o.1.clamp(0.0, m.1));
        }
        s.nodes.retain(|_, o| *o != (0.0, 0.0));
        *s != before
    }

    /// Scroll by a wheel or a touchpad: the innermost box from `from` that can move that way, else the page.
    pub fn wheel(&self, s: &mut Scroll, from: Option<NodeId>, dx: f32, dy: f32) -> bool {
        let moves = |cur: f32, max: f32, d: f32, user: bool| user && ((d > 0.0 && cur < max) || (d < 0.0 && cur > 0.0));
        let mut cur = from;
        while let Some(n) = cur {
            if self.areas.get(n).is_some() {
                let (o, m, u) = (s.of(n), self.max_of(n), self.user(n));
                let (mx, my) = (moves(o.0, m.0, dx, u.0), moves(o.1, m.1, dy, u.1));
                if mx || my {
                    let to = (if mx { (o.0 + dx).clamp(0.0, m.0) } else { o.0 }, if my { (o.1 + dy).clamp(0.0, m.1) } else { o.1 });
                    s.nodes.insert(n, (round(to.0), round(to.1)));
                    return true;
                }
            }
            cur = self.doc.parent(n);
        }
        let (m, u) = (self.page_max(), self.page_user());
        let (mx, my) = (moves(s.x, m.0, dx, u.0), moves(s.y, m.1, dy, u.1));
        if !(mx || my) {
            return false;
        }
        if mx {
            s.x = round((s.x + dx).clamp(0.0, m.0));
        }
        if my {
            s.y = round((s.y + dy).clamp(0.0, m.1));
        }
        true
    }

    /// Scroll the page by a key. Gives whether it moved.
    pub fn key(&self, s: &mut Scroll, k: ScrollKey) -> bool {
        let (m, v) = (self.page_max(), self.view());
        if !self.page_user().1 && !matches!(k, ScrollKey::Left | ScrollKey::Right) {
            return false;
        }
        let page = (v.1 * 0.875).floor().max(v.1 - LINE);
        let (x, y) = (s.x, s.y);
        match k {
            ScrollKey::Up => s.y -= LINE,
            ScrollKey::Down => s.y += LINE,
            ScrollKey::Left => s.x -= LINE,
            ScrollKey::Right => s.x += LINE,
            ScrollKey::PageUp => s.y -= page,
            ScrollKey::PageDown => s.y += page,
            ScrollKey::Home => s.y = 0.0,
            ScrollKey::End => s.y = m.1,
        }
        (s.x, s.y) = (s.x.clamp(0.0, m.0), s.y.clamp(0.0, m.1));
        (s.x, s.y) != (x, y)
    }

    /// The border box of `n` in window coordinates (what `getBoundingClientRect` gives), with the offsets of the page and of the
    /// containers around it taken off. A box in a fixed box does not move with the page. `None`: no box.
    pub fn client_rect(&self, s: &Scroll, n: NodeId) -> Option<Rect> {
        let mut r = border_box(self.layout, n)?;
        let (mut dx, mut dy) = (0.0, 0.0);
        let mut fixed = keyword(self.styles, n, Property::Position) == Keyword::Fixed;
        let mut cur = self.doc.parent(n);
        while let Some(a) = cur.filter(|_| !fixed) {
            if self.areas.get(a).is_some() {
                let o = s.of(a);
                (dx, dy) = (dx + o.0.round(), dy + o.1.round());
            }
            fixed = keyword(self.styles, a, Property::Position) == Keyword::Fixed;
            cur = self.doc.parent(a);
        }
        if !fixed {
            (dx, dy) = (dx + s.x.round(), dy + s.y.round());
        }
        r.x -= dx;
        r.y -= dy;
        Some(r)
    }

    /// Scroll the containers around `n` and then the page so that `n` shows: `ax` along the line, `ay` across it.
    pub fn reveal(&self, s: &mut Scroll, n: NodeId, ax: Align, ay: Align) -> bool {
        let Some(mut r) = border_box(self.layout, n) else { return false };
        let before = s.clone();
        let mut path = Vec::new();
        let mut cur = self.doc.parent(n);
        let mut fixed = keyword(self.styles, n, Property::Position) == Keyword::Fixed;
        while let Some(a) = cur.filter(|_| !fixed) {
            if self.areas.get(a).is_some() {
                path.push(a);
            }
            fixed = keyword(self.styles, a, Property::Position) == Keyword::Fixed;
            cur = self.doc.parent(a);
        }
        for a in path {
            let area = self.areas.get(a).expect("listed");
            let (o, m, c) = (s.of(a), self.max_of(a), area.client());
            let to = (
                (align(r.x, r.w, area.pad.x + o.0, c.0, ax) - area.pad.x).clamp(0.0, m.0),
                (align(r.y, r.h, area.pad.y + o.1, c.1, ay) - area.pad.y).clamp(0.0, m.1),
            );
            let to = (round(to.0), round(to.1));
            s.nodes.insert(a, to);
            // What the container moved is out of the way of the box for the next container.
            (r.x, r.y) = (r.x - (to.0 - o.0), r.y - (to.1 - o.1));
        }
        if !fixed {
            let (m, v) = (self.page_max(), self.view());
            s.x = round(align(r.x, r.w, s.x, v.0, ax).clamp(0.0, m.0));
            s.y = round(align(r.y, r.h, s.y, v.1, ay).clamp(0.0, m.1));
        }
        s.nodes.retain(|_, o| *o != (0.0, 0.0));
        *s != before
    }
}

/// The new start of the view (`view0`, length `vlen`) so that the span `start..start + len` shows as `how` says.
fn align(start: f32, len: f32, view0: f32, vlen: f32, how: Align) -> f32 {
    let end = start + len;
    let inside = start >= view0 && end <= view0 + vlen;
    let apart = end <= view0 || start >= view0 + vlen;
    let nearest = || {
        if start < view0 && end > view0 + vlen {
            view0
        } else if start < view0 || (len > vlen && start > view0) {
            start
        } else if end > view0 + vlen {
            end - vlen
        } else {
            view0
        }
    };
    match how {
        Align::Start => start,
        Align::End => end - vlen,
        Align::Center => start + len / 2.0 - vlen / 2.0,
        Align::Nearest => nearest(),
        Align::IfNeeded if inside => view0,
        Align::IfNeeded if apart => start + len / 2.0 - vlen / 2.0,
        Align::IfNeeded => nearest(),
    }
}

/// The page bars that `areas` needs when the window is `vp` and the bars are `bars` now.
fn need_bars(a: &Areas, vp: (f32, f32), bars: (bool, bool)) -> (bool, bool) {
    let want = |k: Keyword, content: f32, avail: f32| match k {
        Keyword::Scroll => true,
        Keyword::Auto | Keyword::Visible => content > avail + 0.01,
        _ => false,
    };
    let (ox, oy) = a.page_overflow;
    (want(oy, a.page.1, vp.1 - if bars.1 { BAR } else { 0.0 }), want(ox, a.page.0, vp.0 - if bars.0 { BAR } else { 0.0 }))
}

impl Dom {
    /// One bounded layout of the page with its bars: the layout runs again at the smaller size when a bar appears or goes away.
    /// The bars are remembered, so a page that keeps them is laid out once. Gives the layout, the scroll containers and the bars.
    pub(crate) fn layout_areas(&self, it: &mut Interp<'_>, styles: &Styles) -> R<(Layout, Areas, (bool, bool))> {
        let vp = self.plat.s.borrow().viewport;
        let mut bars = self.scrolling.s.borrow().bars;
        let mut n = 0;
        loop {
            let at = (vp.0 - if bars.0 { BAR } else { 0.0 }, vp.1 - if bars.1 { BAR } else { 0.0 });
            let layout = self.layout_at(it, styles, at)?;
            let areas = scroll::areas(&self.doc.borrow(), styles, &layout);
            let need = need_bars(&areas, vp, bars);
            n += 1;
            if need == bars || n >= 3 {
                self.scrolling.s.borrow_mut().bars = bars;
                return Ok((layout, areas, bars));
            }
            bars = need;
        }
    }

    /// The page as scrolling sees it now: runs the cascade and the layout, then `f`. `f` may change the offsets: the changes are
    /// stored, and the targets are queued for a `scroll` event.
    fn with_geo<T>(&self, it: &mut Interp<'_>, f: impl FnOnce(&Geo, &mut Scroll) -> T) -> R<T> {
        let styles = self.cascade_now(it)?;
        let (layout, areas, bars) = self.layout_areas(it, &styles)?;
        let doc = self.doc.borrow();
        let g = Geo { doc: &doc, styles: &styles, layout: &layout, areas: &areas, window: self.plat.s.borrow().viewport, bars };
        let mut s = self.scrolling.s.borrow().scroll.clone();
        g.clamp(&mut s);
        let out = f(&g, &mut s);
        drop(doc);
        self.store_scroll(s);
        Ok(out)
    }

    /// Keep the new offsets and queue a `scroll` event for each target that moved.
    pub(crate) fn store_scroll(&self, new: Scroll) {
        let mut st = self.scrolling.s.borrow_mut();
        let old = std::mem::replace(&mut st.scroll, new);
        let now = &st.scroll;
        let mut moved = Vec::new();
        if (old.x, old.y) != (now.x, now.y) {
            moved.push(None);
        }
        let mut nodes: Vec<NodeId> = old.nodes.keys().chain(now.nodes.keys()).copied().collect();
        nodes.sort_by_key(|n| n.index());
        nodes.dedup();
        moved.extend(nodes.into_iter().filter(|&n| old.of(n) != now.of(n)).map(Some));
        for t in moved {
            if !st.pending.contains(&t) {
                st.pending.push(t);
            }
        }
    }

    /// Put the stored offsets in range for this layout. A change queues a `scroll` event.
    pub(crate) fn clamp_scroll(&self, styles: &Styles, layout: &Layout, areas: &Areas, bars: (bool, bool)) {
        let doc = self.doc.borrow();
        let g = Geo { doc: &doc, styles, layout, areas, window: self.plat.s.borrow().viewport, bars };
        let mut s = self.scrolling.s.borrow().scroll.clone();
        if g.clamp(&mut s) {
            drop(doc);
            self.store_scroll(s);
        }
    }

    /// Should the scheduler queue the `scroll` step now? Marks it queued when it says yes.
    pub(crate) fn scroll_wants_task(&self) -> bool {
        let mut st = self.scrolling.s.borrow_mut();
        if st.queued || st.pending.is_empty() {
            return false;
        }
        st.queued = true;
        true
    }

    /// Send one `scroll` event to each target that moved since the last time.
    pub(crate) fn flush_scroll(&self, it: &mut Interp<'_>) -> R<()> {
        let list = {
            let mut st = self.scrolling.s.borrow_mut();
            st.queued = false;
            std::mem::take(&mut st.pending)
        };
        for t in list {
            let (target, bubbles) = match t {
                None => (self.doc.borrow().root(), true),
                Some(n) => (n, false),
            };
            let ev = self.new_event(it, "scroll", bubbles, false, false, None)?;
            self.st.borrow_mut().events.get_mut(&ev).expect("just made").trusted = true;
            self.dispatch(it, Target::Node(target), ev)?;
        }
        Ok(())
    }

    fn root_element(&self) -> Option<NodeId> {
        let doc = self.doc.borrow();
        doc.children(doc.root()).find(|&n| matches!(doc.data(n), NodeData::Element { .. }))
    }

    /// The stored offset of `el`: the page for the root element, else the container's (zero for a box that is not one).
    fn offset_of(&self, el: NodeId) -> (f32, f32) {
        let st = self.scrolling.s.borrow();
        if Some(el) == self.root_element() { (st.scroll.x, st.scroll.y) } else { st.scroll.of(el) }
    }

    pub(crate) fn note_scroll_key(&self, k: ScrollKey) {
        self.scrolling.s.borrow_mut().key = Some(k);
    }

    /// Put the scroll key of an earlier call away (`None`), and say whether one is set (`Some(true)`).
    pub(crate) fn scroll_key_set(&self, clear: bool) -> bool {
        let mut st = self.scrolling.s.borrow_mut();
        if clear {
            st.key = None;
        }
        st.key.is_some()
    }
}

// ---------------------------------------------------------------------------------------------- script API

fn num(it: &mut Interp<'_>, v: Value) -> R<f32> {
    let n = it.to_js_number(v)?;
    Ok(if n.is_finite() { n as f32 } else { 0.0 })
}

/// The target of `scrollTo` and `scrollBy`: `(x, y)` or `{ left, top }`. A missing member is `cur`.
fn target_of(d: &Dom, it: &mut Interp<'_>, a: &[Value], cur: (f32, f32), by: bool) -> R<(f32, f32)> {
    let base = if by { (0.0, 0.0) } else { cur };
    let pair = match a {
        [] => base,
        [Value::Object(o)] => {
            let get = |name: &str, d0: f32, it: &mut Interp<'_>| -> R<f32> {
                let k = d.key(it, name)?;
                match it.get_value(Value::Object(*o), k)? {
                    Value::Undefined => Ok(d0),
                    v => num(it, v),
                }
            };
            (get("left", base.0, it)?, get("top", base.1, it)?)
        }
        [x] => (num(it, *x)?, 0.0),
        [x, y, ..] => (num(it, *x)?, num(it, *y)?),
    };
    Ok(if by { (cur.0 + pair.0, cur.1 + pair.1) } else { pair })
}

impl Dom {
    /// Scroll `el` (the root element is the page) to `to`, or by `to` when `by`.
    fn scroll_el(&self, it: &mut Interp<'_>, el: NodeId, a: &[Value], by: bool) -> R<()> {
        if !self.in_document(it, el)? {
            return Ok(());
        }
        let to = target_of(self, it, a, self.offset_of(el), by)?;
        let root = Some(el) == self.root_element();
        self.with_geo(it, |g, s| {
            let m = if root { g.page_max() } else { g.max_of(el) };
            if !root && g.areas.get(el).is_none() {
                return;
            }
            let v = (round(to.0.clamp(0.0, m.0)), round(to.1.clamp(0.0, m.1)));
            if root {
                (s.x, s.y) = v;
            } else if v == (0.0, 0.0) {
                s.nodes.remove(&el);
            } else {
                s.nodes.insert(el, v);
            }
        })
    }
}

fn el_scroll_to(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let el = d.el(it, this)?;
    d.scroll_el(it, el, a, false)?;
    Ok(Value::Undefined)
}

fn el_scroll_by(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let el = d.el(it, this)?;
    d.scroll_el(it, el, a, true)?;
    Ok(Value::Undefined)
}

fn win_scroll_to(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> V {
    if let Some(r) = d.root_element() {
        d.scroll_el(it, r, a, false)?;
    }
    Ok(Value::Undefined)
}

fn win_scroll_by(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> V {
    if let Some(r) = d.root_element() {
        d.scroll_el(it, r, a, true)?;
    }
    Ok(Value::Undefined)
}

fn get_top(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let el = d.el(it, this)?;
    Ok(Value::Number(d.offset_of(el).1 as f64))
}

fn get_left(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let el = d.el(it, this)?;
    Ok(Value::Number(d.offset_of(el).0 as f64))
}

fn set_top(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let el = d.el(it, this)?;
    let v = num(it, a.first().copied().unwrap_or(Value::Undefined))?;
    let x = d.offset_of(el).0;
    let (k, y) = (d.key(it, "left")?, d.key(it, "top")?);
    let o = d.obj(it, it.object_prototype())?;
    it.define_data(o, k, Value::Number(x as f64), true, true, true)?;
    it.define_data(o, y, Value::Number(v as f64), true, true, true)?;
    d.scroll_el(it, el, &[Value::Object(o)], false)?;
    Ok(Value::Undefined)
}

fn set_left(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let el = d.el(it, this)?;
    let v = num(it, a.first().copied().unwrap_or(Value::Undefined))?;
    let y = d.offset_of(el).1;
    let (k, t) = (d.key(it, "left")?, d.key(it, "top")?);
    let o = d.obj(it, it.object_prototype())?;
    it.define_data(o, k, Value::Number(v as f64), true, true, true)?;
    it.define_data(o, t, Value::Number(y as f64), true, true, true)?;
    d.scroll_el(it, el, &[Value::Object(o)], false)?;
    Ok(Value::Undefined)
}

/// `scrollWidth`, `scrollHeight`, `clientWidth`, `clientHeight`, `clientTop`, `clientLeft` of `el`, in whole pixels.
fn sizes(d: &Dom, it: &mut Interp<'_>, el: NodeId) -> R<[f32; 6]> {
    if !d.in_document(it, el)? {
        return Ok([0.0; 6]);
    }
    let root = Some(el) == d.root_element();
    d.with_geo(it, |g, _| {
        if root {
            let v = g.view();
            return [round(g.areas.page.0.max(v.0)), round(g.areas.page.1.max(v.1)), round(v.0), round(v.1), 0.0, 0.0];
        }
        let Some(b) = border_box(g.layout, el) else { return [0.0; 6] };
        let style = g.styles.style(el);
        let px = |p| style.map_or(0.0, |s| s.px(p).max(0.0));
        let (l, t) = (px(Property::BorderLeftWidth), px(Property::BorderTopWidth));
        let (r, bt) = (px(Property::BorderRightWidth), px(Property::BorderBottomWidth));
        match g.areas.get(el) {
            Some(a) => {
                let c = a.client();
                [round(a.content.0), round(a.content.1), round(c.0), round(c.1), round(t), round(l)]
            }
            None => {
                let (w, h) = ((b.w - l - r).max(0.0), (b.h - t - bt).max(0.0));
                [round(w), round(h), round(w), round(h), round(t), round(l)]
            }
        }
    })
}

macro_rules! size_get {
    ($name:ident, $i:expr) => {
        fn $name(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
            let el = d.el(it, this)?;
            Ok(Value::Number(sizes(d, it, el)?[$i] as f64))
        }
    };
}
size_get!(scroll_width, 0);
size_get!(scroll_height, 1);
size_get!(client_width, 2);
size_get!(client_height, 3);
size_get!(client_top, 4);
size_get!(client_left, 5);

fn bounding_rect(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let el = d.el(it, this)?;
    let r = if d.in_document(it, el)? {
        d.with_geo(it, |g, s| g.client_rect(s, el))?
    } else {
        None
    };
    let r = r.unwrap_or(Rect { x: 0.0, y: 0.0, w: 0.0, h: 0.0 });
    let rect = d.obj(it, it.object_prototype())?;
    let (x, y, w, h) = (r.x as f64, r.y as f64, r.w as f64, r.h as f64);
    for (name, v) in [("x", x), ("y", y), ("width", w), ("height", h), ("top", y), ("right", x + w), ("bottom", y + h), ("left", x)] {
        let k = d.key(it, name)?;
        it.define_data(rect, k, Value::Number(v), true, true, true)?;
    }
    Ok(Value::Object(rect))
}

fn align_word(it: &mut Interp<'_>, o: Value, name: &str, default: Align) -> Align {
    match it.get_str(o, name).as_str() {
        "start" => Align::Start,
        "center" => Align::Center,
        "end" => Align::End,
        "nearest" => Align::Nearest,
        _ => default,
    }
}

fn scroll_into_view(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let el = d.el(it, this)?;
    let (ay, ax) = match a.first().copied() {
        None | Some(Value::Undefined) => (Align::Start, Align::Nearest),
        Some(o @ Value::Object(_)) => (align_word(it, o, "block", Align::Start), align_word(it, o, "inline", Align::Nearest)),
        Some(Value::Bool(false)) => (Align::End, Align::Nearest),
        Some(_) => (Align::Start, Align::Nearest),
    };
    if d.in_document(it, el)? {
        d.with_geo(it, |g, s| g.reveal(s, el, ax, ay))?;
    }
    Ok(Value::Undefined)
}

fn win_scroll_x(d: &Dom, _: &mut Interp<'_>, _: Value, _: &[Value]) -> V {
    Ok(Value::Number(d.scrolling.s.borrow().scroll.x as f64))
}

fn win_scroll_y(d: &Dom, _: &mut Interp<'_>, _: Value, _: &[Value]) -> V {
    Ok(Value::Number(d.scrolling.s.borrow().scroll.y as f64))
}

fn win_inner_width(d: &Dom, _: &mut Interp<'_>, _: Value, _: &[Value]) -> V {
    Ok(Value::Number(d.plat.s.borrow().viewport.0 as f64))
}

fn win_inner_height(d: &Dom, _: &mut Interp<'_>, _: Value, _: &[Value]) -> V {
    Ok(Value::Number(d.plat.s.borrow().viewport.1 as f64))
}

fn scrolling_element(d: &Dom, it: &mut Interp<'_>, _: Value, _: &[Value]) -> V {
    match d.root_element() {
        Some(r) => d.wrap(it, r),
        None => Ok(Value::Null),
    }
}

pub(crate) fn install(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    let p = d.p();
    d.def_get(it, p.element, "scrollTop", get_top, Some(set_top))?;
    d.def_get(it, p.element, "scrollLeft", get_left, Some(set_left))?;
    for (name, h) in [("scrollWidth", scroll_width as H), ("scrollHeight", scroll_height), ("clientWidth", client_width), ("clientHeight", client_height), ("clientTop", client_top), ("clientLeft", client_left)] {
        d.def_get(it, p.element, name, h, None)?;
    }
    d.def_method(it, p.element, "getBoundingClientRect", 0, bounding_rect)?;
    d.def_method(it, p.element, "scrollTo", 2, el_scroll_to)?;
    d.def_method(it, p.element, "scroll", 2, el_scroll_to)?;
    d.def_method(it, p.element, "scrollBy", 2, el_scroll_by)?;
    d.def_method(it, p.element, "scrollIntoView", 0, scroll_into_view)?;
    d.def_get(it, p.document, "scrollingElement", scrolling_element, None)?;
    let g = *d.global.get().expect("installed");
    for name in ["scrollX", "pageXOffset"] {
        d.def_get(it, g, name, win_scroll_x, None)?;
    }
    for name in ["scrollY", "pageYOffset"] {
        d.def_get(it, g, name, win_scroll_y, None)?;
    }
    d.def_get(it, g, "innerWidth", win_inner_width, None)?;
    d.def_get(it, g, "innerHeight", win_inner_height, None)?;
    d.def_method(it, g, "scrollTo", 2, win_scroll_to)?;
    d.def_method(it, g, "scroll", 2, win_scroll_to)?;
    d.def_method(it, g, "scrollBy", 2, win_scroll_by)?;
    Ok(())
}

// ---------------------------------------------------------------------------------------------- driver API

impl Browser {
    /// The offsets of the page and of the scroll containers.
    pub fn scroll(&self) -> Ref<'_, Scroll> {
        Ref::map(self.dom.scrolling.s.borrow(), |s| &s.scroll)
    }

    /// Replace the offsets (the caller clamped them). A target that moved gets a `scroll` event, see `flush_scroll`.
    pub fn set_scroll(&mut self, s: Scroll) {
        self.dom.store_scroll(s);
    }

    /// The key that the last key default found as a scroll of the page, once.
    pub fn take_scroll_key(&mut self) -> Option<ScrollKey> {
        self.dom.scrolling.s.borrow_mut().key.take()
    }

    /// The page bars of the last layout.
    pub fn page_bars(&self) -> (bool, bool) {
        self.dom.scrolling.s.borrow().bars
    }

    /// Does a `scroll` event wait to be sent?
    pub fn scroll_pending(&self) -> bool {
        !self.dom.scrolling.s.borrow().pending.is_empty()
    }
}
