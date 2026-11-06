//! The interactive page in the window: a `Browser` (with its `Session`, when it came from a URL), painted into the window
//! buffer, and the window events turned into DOM events.
//!
//! Flow of one input event (`Page::handle`): hit test the point on the current layout (`bare_paint::hit`, the same paint
//! order as the picture) -> `Browser::ui_*` (events and default actions: `browser_ui.rs`) -> if the page may look different,
//! let the session run what the listeners queued (microtasks, fetches, a navigation), drop the old layout and ask for one
//! frame. A frame cascades and lays out the page again (only when the old layout was dropped), paints it, and draws what
//! the paint crate does not yet: the text of the `<input>` fields, the selection and the caret.
//!
//! The caret is steady (no blink), so an idle window wakes for nothing. Password text is shown as bullets.
//!
//! Gaps: timers and animations that are not due at an input event do not wake the window (the window loop has no timer
//! yet); a file path has no session, so only a URL window is interactive; checkbox and radio boxes and `<input>` button
//! labels are not drawn (the paint crate draws controls, S4 topic 3); a long value scrolls to keep the caret in view but a
//! click is mapped with the same scroll, so a click right after a scroll can be off by a few pixels at the field edge.

use crate::browser::{Browser, Report};
use crate::browser_dom::input_kind;
use crate::browser_session::{Session, SessionConfig, SessionStop};
use crate::browser_ui::{Frame, KeyInput, Mods, Pointer, UiOutcome};
use bare_css::values::Property;
use bare_html::dom::{Document, NodeId};
use bare_layout::font::{self, Metrics};
use bare_layout::fragment::Kind;
use bare_paint::canvas::{Canvas, IRect};
use bare_paint::display::argb;
use bare_paint::{Painter, Scroll, View};
use crate::browser_scroll::{Align, Geo};
use bare_window::{App, Control, Cursor, Event};
use std::time::{Duration, Instant};

/// Where the page comes from. A session reloads on a link click; a bare browser (tests) does not.
pub enum Source {
    Session(Box<Session>),
    Browser(Box<Browser>),
}

impl Source {
    fn browser(&mut self) -> Option<&mut Browser> {
        match self {
            Source::Session(s) => s.browser(),
            Source::Browser(b) => Some(b),
        }
    }
}

/// What a window event did.
#[derive(Debug, Default, Clone, Copy)]
pub struct Handled {
    pub redraw: bool,
    pub cursor: Option<Cursor>,
}

pub struct Page {
    source: Source,
    frame: Option<Frame>,
    painter: Painter,
    canvas: Canvas,
    pointer: Pointer,
    /// Is the display list of `frame` built? A scroll draws it again with other offsets and builds nothing.
    built: bool,
    /// The text fields of `frame`, found when its display list was built (a pass over all the fragments).
    inputs: Vec<NodeId>,
    /// Listener errors and render errors, kept for tests. The first of each also goes to stderr.
    pub errors: Vec<String>,
    started: Instant,
    pub first_frame: Option<Duration>,
}

fn mods(m: bare_window::Mods) -> Mods {
    Mods { shift: m.shift, ctrl: m.ctrl, alt: m.alt, meta: m.meta }
}

/// Where UTF-16 offset `u` of `text` starts, in pixels from the left of the text.
fn x_of(text: &str, u: u32, m: &Metrics) -> f32 {
    let (mut units, mut n) = (0, 0);
    for c in text.chars() {
        if n >= u {
            break;
        }
        n += c.len_utf16() as u32;
        units += font::width_of(c);
    }
    m.px(units)
}

/// The UTF-16 offset of the boundary that is nearest to `x` pixels from the left of the text.
fn offset_at(text: &str, x: f32, m: &Metrics) -> u32 {
    let (mut left, mut n) = (0.0, 0);
    for c in text.chars() {
        let w = m.px(font::width_of(c));
        if x < left + w / 2.0 {
            break;
        }
        left += w;
        n += c.len_utf16() as u32;
    }
    n
}

/// The geometry of the text of one field.
struct Field {
    /// The left of the content box.
    x: f32,
    baseline: i32,
    clip: IRect,
    m: Metrics,
    color: u32,
    /// What is drawn: the value, or bullets for a password.
    text: String,
    scroll: f32,
}

/// The text field geometry of `n`, if it is an `<input>` that has a box.
fn field(f: &Frame, doc: &Document, n: NodeId, text: &str, focus_end: Option<u32>) -> Option<Field> {
    let frag = f.layout.fragments.iter().find(|g| g.node == n && g.kind == Kind::Box)?;
    let style = f.styles.style(n)?;
    let px = |p| style.px(p).max(0.0);
    let m = Metrics::of(style);
    let x = frag.rect.x + px(Property::BorderLeftWidth) + px(Property::PaddingLeft);
    let w = (frag.rect.w - px(Property::BorderLeftWidth) - px(Property::BorderRightWidth) - px(Property::PaddingLeft) - px(Property::PaddingRight)).max(0.0);
    let y0 = frag.rect.y + px(Property::BorderTopWidth) + px(Property::PaddingTop);
    let h = (frag.rect.h - px(Property::BorderTopWidth) - px(Property::BorderBottomWidth) - px(Property::PaddingTop) - px(Property::PaddingBottom)).max(0.0);
    let line = m.normal_line_height();
    let baseline = (y0 + (h - line) / 2.0 + m.ascent).round() as i32;
    let shown = if input_kind(doc, n) == "password" { "•".repeat(text.chars().count()) } else { text.to_string() };
    let scroll = focus_end.map_or(0.0, |e| (x_of(&shown, e, &m) - w + 1.0).max(0.0));
    let clip = IRect { x0: x.floor() as i32, y0: y0.floor() as i32, x1: (x + w).ceil() as i32, y1: (y0 + h).ceil() as i32 };
    Some(Field { x, baseline, clip, m, color: argb(style.color(Property::TextColor)), text: shown, scroll })
}

/// Move the geometry of field `n` to where it is drawn now: the page and the scroll containers around it have offsets, and
/// those containers clip it (a fixed box does not move with the page).
fn place(f: &Frame, doc: &Document, scroll: &Scroll, n: NodeId, fld: &mut Field) {
    let fixed = |n: NodeId| f.styles.style(n).is_some_and(|s| s.get(Property::Position) == bare_css::values::Value::Keyword(bare_css::values::Keyword::Fixed));
    let (mut chain, mut pinned) = (Vec::new(), fixed(n));
    let mut cur = doc.parent(n);
    while let Some(a) = cur.filter(|_| !pinned) {
        if f.areas.get(a).is_some() {
            chain.push(a);
        }
        pinned = fixed(a);
        cur = doc.parent(a);
    }
    // From the outermost box in: each clips what is inside it, at the place that the offsets outside it leave.
    let mut off = if pinned { (0, 0) } else { (scroll.x.round() as i32, scroll.y.round() as i32) };
    let mut clip: Option<IRect> = None;
    for &a in chain.iter().rev() {
        let area = f.areas.get(a).expect("listed");
        let c = area.client();
        let r = IRect { x0: area.pad.x.floor() as i32 - off.0, y0: area.pad.y.floor() as i32 - off.1, x1: (area.pad.x + c.0).ceil() as i32 - off.0, y1: (area.pad.y + c.1).ceil() as i32 - off.1 };
        clip = Some(clip.map_or(r, |k| k.intersect(r)));
        let o = scroll.of(a);
        off = (off.0 + o.0.round() as i32, off.1 + o.1.round() as i32);
    }
    fld.x -= off.0 as f32;
    fld.baseline -= off.1;
    let moved = IRect { x0: fld.clip.x0 - off.0, y0: fld.clip.y0 - off.1, x1: fld.clip.x1 - off.0, y1: fld.clip.y1 - off.1 };
    fld.clip = clip.map_or(moved, |k| moved.intersect(k));
}

impl Page {
    pub fn new(source: Source, viewport: (u32, u32)) -> Page {
        Page {
            source,
            frame: None,
            painter: Painter::new(),
            canvas: Canvas::new(viewport.0.max(1), viewport.1.max(1)),
            pointer: Pointer::default(),
            built: false,
            inputs: Vec::new(),
            errors: Vec::new(),
            started: Instant::now(),
            first_frame: None,
        }
    }

    /// Load `url` in a session (it runs the scripts and settles). Run it on a thread with a big stack: the VM recurses.
    pub fn open(url: &str, viewport: (u32, u32)) -> Result<Page, String> {
        let mut cfg = SessionConfig::new(url);
        cfg.viewport = Some((viewport.0 as f32, viewport.1 as f32));
        // A window runs for hours, not for one load: an input event adds a little time and a few tasks each.
        cfg.limits.max_time = Duration::from_secs(7 * 24 * 3600);
        cfg.limits.max_documents = usize::MAX;
        cfg.limits.max_tasks = usize::MAX;
        cfg.limits.max_rounds = usize::MAX;
        // Wait for no timer that is not due: the window cannot sleep inside an event.
        cfg.page.quiet = Duration::ZERO;
        let mut s = Session::new(cfg);
        if let SessionStop::LoadFailed(why) = s.run().stop {
            return Err(format!("{url}: {why}"));
        }
        Ok(Page::new(Source::Session(Box::new(s)), viewport))
    }

    pub fn browser(&mut self) -> Option<&mut Browser> {
        self.source.browser()
    }

    /// The layout of the last frame, made now if the page changed.
    pub fn frame_now(&mut self) -> Option<&Frame> {
        if self.frame.is_none() {
            let r = self.browser().map(|b| b.render());
            match r {
                Some(Ok(f)) => {
                    self.frame = Some(f);
                    self.built = false;
                }
                Some(Err(e)) => self.note(format!("render: {e:?}")),
                None => {}
            }
        }
        self.frame.as_ref()
    }

    fn note(&mut self, text: String) {
        if self.errors.is_empty() {
            eprintln!("bare: {text}");
        }
        self.errors.push(text);
    }

    fn hit(&mut self, x: f64, y: f64) -> Option<NodeId> {
        self.frame_now()?;
        let doc = self.browser()?.document();
        let doc = doc.borrow();
        let f = self.frame.as_ref()?;
        let scroll = self.source.browser()?.scroll();
        bare_paint::hit::hit_test_scrolled(&doc, &f.styles, &f.layout, &f.areas, &scroll, x as f32, y as f32)
    }

    /// The caret offset for a press at `x` in text field `n`.
    fn caret_at(&mut self, n: NodeId, x: f64) -> Option<u32> {
        self.frame_now()?;
        let Page { source, frame, .. } = self;
        let b = source.browser()?;
        let value = b.input_value(n)?;
        let focus_end = b.input_selection(n).map(|s| s.1);
        let doc = b.document();
        let doc = doc.borrow();
        let fr = frame.as_ref()?;
        let mut f = field(fr, &doc, n, &value, focus_end)?;
        place(fr, &doc, &b.scroll(), n, &mut f);
        Some(offset_at(&f.text, x as f32 - f.x + f.scroll, &f.m))
    }

    /// Which document is the page now? A node of an old one means nothing after a navigation.
    fn doc_id(&mut self) -> usize {
        match &self.source {
            Source::Session(s) => s.documents(),
            Source::Browser(_) => 0,
        }
    }

    fn cursor_over(&mut self, n: Option<NodeId>) -> Cursor {
        let Some(n) = n else { return Cursor::Default };
        let Some(b) = self.browser() else { return Cursor::Default };
        let text = b.is_text_input(n);
        let doc = b.document();
        let doc = doc.borrow();
        if text {
            return Cursor::Text;
        }
        let mut cur = Some(n);
        while let Some(c) = cur {
            let button = doc.is_html(c, "button") || (doc.is_html(c, "input") && !matches!(input_kind(&doc, c), "text" | "search" | "tel" | "url" | "email" | "password" | "number" | "hidden"));
            let link = doc.is_html(c, "a") && doc.find_atom("href").is_some_and(|a| doc.attr(c, a).is_some());
            if button || link {
                return Cursor::Pointer;
            }
            cur = doc.parent(c);
        }
        Cursor::Default
    }

    /// Run what a listener queued, then forget the layout. A link click ends the document: the session loads the next.
    fn settle(&mut self) {
        let fatal = match &mut self.source {
            Source::Session(s) => match s.resume(&[]).stop {
                SessionStop::Page(crate::Stop::Fatal(f)) => Some(format!("{f:?}")),
                SessionStop::LoadFailed(why) => Some(why),
                _ => None,
            },
            Source::Browser(b) => b.drain().and_then(|_| b.run_navigation_tasks()).and_then(|_| b.run_scheduled_tasks(1000)).err().map(|e| format!("{e:?}")),
        };
        if let Some(f) = fatal {
            self.note(format!("page: {f}"));
        }
        self.frame = None;
    }

    fn after(&mut self, r: Result<UiOutcome, bare_js::eval::Error>) -> bool {
        let out = match r {
            Ok(o) => o,
            Err(e) => {
                self.note(format!("input: {e:?}"));
                // The call may have changed the page before it stopped.
                UiOutcome { reports: Vec::new(), changed: true }
            }
        };
        for Report { event_type, message, .. } in out.reports {
            self.note(format!("listener of {event_type}: {message}"));
        }
        if out.changed {
            self.settle();
        }
        out.changed
    }

    /// One window event. Gives whether to draw a frame and the cursor shape.
    pub fn handle(&mut self, event: Event) -> Handled {
        let mut h = Handled::default();
        match event {
            Event::PointerMove { x, y, buttons, mods: m } => {
                self.pointer = Pointer { x, y, buttons, mods: mods(m) };
                let target = self.hit(x, y);
                let (p, doc) = (self.pointer, self.doc_id());
                let r = self.browser().map(|b| b.ui_pointer_move(target, p));
                if let Some(r) = r {
                    h.redraw = self.after(r);
                }
                h.cursor = Some(if self.doc_id() == doc { self.cursor_over(target) } else { Cursor::Default });
            }
            Event::PointerButton { x, y, button, buttons, pressed, mods: m } => {
                self.pointer = Pointer { x, y, buttons, mods: mods(m) };
                let target = self.hit(x, y);
                let caret = match target {
                    Some(n) if pressed && button == 0 && self.browser().is_some_and(|b| b.is_text_input(n)) => self.caret_at(n, x),
                    _ => None,
                };
                let (p, doc) = (self.pointer, self.doc_id());
                let r = self.browser().map(|b| b.ui_pointer_button(target, p, button, pressed, caret));
                if let Some(r) = r {
                    h.redraw = self.after(r);
                }
                // The back and forward buttons of a mouse.
                if !pressed && (button == 3 || button == 4) {
                    h.redraw |= self.history_go(if button == 3 { -1 } else { 1 });
                }
                // The page under the pointer may have changed, or be another document.
                h.cursor = Some(if self.doc_id() == doc { self.cursor_over(target) } else { Cursor::Default });
            }
            Event::PointerLeave => {
                let p = self.pointer;
                let r = self.browser().map(|b| b.ui_pointer_leave(p));
                if let Some(r) = r {
                    h.redraw = self.after(r);
                }
            }
            Event::Wheel { x, y, dx, dy, .. } => {
                // Only a scroll container can take the wheel before the page: with none, the hit test would find nothing to use.
                let target = if self.frame_now().is_some_and(|f| !f.areas.list.is_empty()) { self.hit(x, y) } else { None };
                if self.scroll_with(|g, s| g.wheel(s, target, dx as f32, dy as f32)) {
                    h.redraw = true;
                    self.send_scroll_events();
                }
            }
            Event::Key { key, code, pressed, repeat, text, mods: m } => {
                let tab = pressed && key == "Tab";
                let key_name = key.clone();
                let before = if tab { self.browser().and_then(|b| b.active_element().ok().flatten()) } else { None };
                let k = KeyInput { key, code: code.to_string(), text, repeat, mods: mods(m) };
                let r = self.browser().map(|b| b.ui_key(k, pressed));
                if let Some(r) = r {
                    h.redraw = self.after(r);
                }
                if pressed && m.alt && (key_name == "ArrowLeft" || key_name == "ArrowRight") {
                    h.redraw |= self.history_go(if key_name == "ArrowLeft" { -1 } else { 1 });
                }
                if pressed {
                    if let Some(k) = self.browser().and_then(|b| b.take_scroll_key()) {
                        h.redraw |= self.scroll_with(|g, s| g.key(s, k));
                    }
                    // Tab to a field that is out of the window: scroll it in (Chrome centers it when none of it shows).
                    let now = if tab { self.browser().and_then(|b| b.active_element().ok().flatten()) } else { None };
                    if let Some(n) = now.filter(|&n| Some(n) != before) {
                        h.redraw |= self.scroll_with(|g, s| g.reveal(s, n, Align::IfNeeded, Align::IfNeeded));
                    }
                    self.send_scroll_events();
                }
            }
        }
        h
    }

    /// Paint the page into the canvas, then the fields. The display list is built once for a frame; a scroll draws it again.
    pub fn paint(&mut self) -> &[u32] {
        self.frame_now();
        let Page { source, frame, painter, canvas, built, inputs, .. } = self;
        let b = source.browser();
        if let (Some(b), Some(f)) = (b, frame.as_ref()) {
            let focus = b.active_element().ok().flatten();
            let scroll = b.scroll().clone();
            let doc = b.document();
            let doc = doc.borrow();
            if !*built {
                *inputs = f.layout.fragments.iter().filter(|g| g.kind == Kind::Box && doc.is_html(g.node, "input") && b.is_text_input(g.node)).map(|g| g.node).collect();
                // Paint draws the placeholder of an empty field. The text, the selection and the caret are drawn here.
                painter.list.hosted.clear();
                painter.list.hosted.extend(inputs.iter().map(|&n| (n, !b.input_value(n).unwrap_or_default().is_empty())));
                painter.list.chosen.clear();
                painter.list.chosen.extend(f.layout.fragments.iter().filter(|g| g.kind == Kind::Box && doc.is_html(g.node, "select")).map(|g| (g.node, b.selected_option(g.node))));
                painter.build(&doc, &f.styles, &f.layout, &f.areas, (canvas.width, canvas.height));
                *built = true;
            }
            painter.draw_view(canvas, Some(&View { scroll: &scroll, areas: &f.areas, bars: f.bars }));
            for &n in inputs.iter() {
                let value = b.input_value(n).unwrap_or_default();
                let sel = b.input_selection(n);
                let focused = focus == Some(n);
                let end = sel.filter(|_| focused).map(|s| if s.2 == "backward" { s.0 } else { s.1 });
                let Some(mut fld) = field(f, &doc, n, &value, end) else { continue };
                place(f, &doc, &scroll, n, &mut fld);
                Page::draw_field(painter, canvas, &fld, sel.filter(|_| focused));
            }
        } else {
            canvas.pixels.fill(0xFFFF_FFFF);
        }
        self.first_frame.get_or_insert_with(|| self.started.elapsed());
        &self.canvas.pixels
    }

    /// Run `f` on the page geometry and a copy of the offsets; keep the offsets when it says that it moved something.
    fn scroll_with(&mut self, f: impl FnOnce(&Geo, &mut Scroll) -> bool) -> bool {
        self.frame_now();
        let Page { source, frame, canvas, .. } = self;
        let (Some(b), Some(fr)) = (source.browser(), frame.as_ref()) else { return false };
        let mut s = b.scroll().clone();
        let moved = {
            let doc = b.document();
            let doc = doc.borrow();
            f(&Geo { doc: &doc, styles: &fr.styles, layout: &fr.layout, areas: &fr.areas, window: (canvas.width as f32, canvas.height as f32), bars: fr.bars }, &mut s)
        };
        if moved {
            b.set_scroll(s);
        }
        moved
    }

    /// Go back (`-1`) or forward (`1`) in the history: inside the document, or to the neighbor document of the session.
    fn history_go(&mut self, delta: i32) -> bool {
        let r = self.browser().map(|b| b.history_go(delta));
        if let Some(Err(e)) = r {
            self.note(format!("history: {e:?}"));
            return false;
        }
        self.settle();
        true
    }

    /// Send the `scroll` events that a scroll queued, now (one for each target), instead of at the next task.
    fn send_scroll_events(&mut self) {
        if self.browser().is_some_and(|b| b.scroll_pending()) {
            let r = self.browser().map(|b| b.ui_scroll_events());
            if let Some(r) = r {
                self.after(r);
            }
        }
    }

    fn draw_field(painter: &mut Painter, canvas: &mut Canvas, f: &Field, sel: Option<(u32, u32, &str)>) {
        let x0 = f.x - f.scroll;
        let line = f.m.normal_line_height();
        let top = (f.baseline as f32 - f.m.ascent).round() as i32;
        if let Some((s, e, _)) = sel.filter(|s| s.0 != s.1) {
            let (a, b) = (x_of(&f.text, s, &f.m), x_of(&f.text, e, &f.m));
            canvas.fill(IRect { x0: (x0 + a).round() as i32, y0: top, x1: (x0 + b).round() as i32, y1: top + line as i32 }.intersect(f.clip), 0xFFB3_D7FF);
        }
        bare_paint::text(canvas, &mut painter.glyphs, f.clip, x0, f.baseline, f.m.size, f.color, false, &f.text);
        if let Some((s, _, _)) = sel.filter(|s| s.0 == s.1) {
            let cx = (x0 + x_of(&f.text, s, &f.m)).round() as i32;
            canvas.fill(IRect { x0: cx, y0: top, x1: cx + 1, y1: top + line as i32 }.intersect(f.clip), f.color);
        }
    }
}

impl App for Page {
    fn resize(&mut self, width: u32, height: u32) {
        self.canvas = Canvas::new(width, height);
        let r = self.browser().map(|b| b.set_viewport(width as f32, height as f32));
        if let Some(Err(e)) = r {
            self.note(format!("viewport: {e:?}"));
        }
        self.frame = None;
    }

    fn frame(&mut self, _: &mut Control) -> &[u32] {
        self.paint()
    }

    fn event(&mut self, event: Event, ctl: &mut Control) {
        let h = self.handle(event);
        if let Some(c) = h.cursor {
            ctl.set_cursor(c);
        }
        if h.redraw {
            ctl.redraw();
        }
    }
}
