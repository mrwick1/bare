//! The interactive page in the window: a `Browser` (with its `Session`, when it came from a URL), painted into the window
//! buffer, and the window events turned into DOM events.
//!
//! Flow of one input event (`Page::handle`): hit test the point on the current layout (`bare_paint::hit`, the same paint
//! order as the picture) -> `Browser::ui_*` (events and default actions: `browser_ui.rs`) -> if the page may look different,
//! let the session run what the listeners queued (microtasks, fetches, a navigation), mark the frame stale and ask for one
//! frame. A stale frame cascades and lays out the page again only when its render key moved (`Browser::render_key`: the
//! tree, its text, the window size, the sheets, and the `:hover`, `:active` and focus state that the rules read). When only
//! the user action state moved, the new cascade is compared with the old one (`Styles::change`): the same values keep the
//! frame, and a change of colors and the like keeps the layout and builds the display list again. A stale frame with the same
//! key builds the display list again only when the values, checkboxes and selects that it shows changed. A frame then paints, and draws what the paint crate does not yet: the text of the `<input>` and
//! `<textarea>` fields, the selection and the caret.
//!
//! The caret is steady (no blink), so an idle window wakes for nothing. Password text is shown as bullets.
//!
//! Timers (S5): `tick` runs the tasks that are due between input events (timers, animation frames, and the `fetch` calls and
//! navigations that they start), and the window sleeps until the next one (`App::tick`). A page with no queued task wakes for nothing.
//!
//! Gaps: a file path has no session, so only a URL window is interactive; checkbox and radio boxes and `<input>` button
//! labels are not drawn (the paint crate draws controls, S4 topic 3); a textarea draws the lines that end with a line feed (no wrap, no scroll,
//! and a click maps to the line under the pointer); a long value scrolls to keep the caret in view but a
//! click is mapped with the same scroll, so a click right after a scroll can be off by a few pixels at the field edge.

use crate::browser::{Browser, Report, Survive};
use crate::browser_dom::input_kind;
use crate::browser_session::{Session, SessionConfig, SessionStop};
use crate::browser_ui::{Frame, KeyInput, Mods, Pointer, UiOutcome};
use bare_css::cascade::Change;
use bare_css::values::Property;
use bare_html::dom::{Document, NodeId};
use bare_layout::font::{self, Metrics};
use bare_layout::fragment::{Fragment, Kind};
use bare_paint::canvas::{Canvas, IRect};
use bare_paint::display::argb;
use bare_paint::display::{each_in_band, extents, ALL};
use bare_paint::{Painter, Scroll, View};
use crate::browser_scroll::{Align, Geo};
use bare_window::{App, Control, Cursor, Event};
use std::time::{Duration, Instant};

pub use crate::browser_navigation_storage::StorageState;

/// The fragments and the page bars of a frame made from nothing.
type Fresh = (Vec<Fragment>, (bool, bool));

/// Where the page comes from. A session reloads on a link click; a bare browser (tests) does not.
pub enum Source {
    Session(Box<Session>),
    Browser(Box<Browser>),
}

impl Source {
    pub(crate) fn browser(&mut self) -> Option<&mut Browser> {
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
    pub(crate) source: Source,
    pub(crate) frame: Option<Frame>,
    painter: Painter,
    /// The size of the window, or the viewport of a headless page.
    size: (u32, u32),
    /// The pixels of a headless page (`paint`), made at its first paint. A window page draws into the buffer of the window
    /// (`App::frame`) and keeps none: this stays empty unless a capture or `check_frames` asks for one.
    pub(crate) canvas: Canvas<'static>,
    /// Does the page draw into a window (`App::frame` ran)?
    pub(crate) windowed: bool,
    pointer: Pointer,
    /// Is the pointer in the window, and did the boxes move under it since it last moved?
    inside: bool,
    moved: bool,
    /// The document and the render key that `frame` was made for, and whether the page may have changed since.
    key: (usize, [u64; 8]),
    stale: bool,
    /// The key at which the last render stopped for a style sheet that is not supplied yet: no frame, and no new try until the key moves
    /// (a supplied sheet, a changed document or window). A try with the same inputs gives the same answer.
    waiting_sheet: Option<(usize, [u64; 8])>,
    /// The renders (cascade and layout) of the page so far, and the cascades that kept the layout. `kept`: the renders that
    /// styled and laid out only what changed (`Browser::refresh`); they are in `renders` too.
    pub renders: u64,
    pub restyles: u64,
    pub kept: u64,
    /// Is the display list of `frame` built, with the control state of now? A scroll draws it again with other offsets and builds nothing.
    built: bool,
    /// Is the display list made from the layout of `frame`? Then a change of the control state only builds it again when the
    /// state that it shows changed.
    listed: bool,
    /// The `<input>`, `<textarea>` and `<select>` boxes in `band`, with their fragments, found when the list is made.
    controls: Vec<(NodeId, u32)>,
    /// The part of the page (page pixels) that the display list holds: the window and one window size around it. A scroll that
    /// leaves it builds the list again.
    band: IRect,
    /// The `extents` of the fragments of `frame`, made when a band list or a hit test first needs them after a layout (then `ext.0`).
    ext: (bool, Vec<[f32; 4]>),
    /// Moves each time `frame` may have changed. The last hit test (the point, `epoch`, the offsets and the element) is kept for it.
    pub(crate) epoch: u64,
    last_hit: Option<(f64, f64, u64, Scroll, Option<NodeId>)>,
    /// Check each frame against a frame made from nothing (`check_frame`), and stop at the first difference. Set by
    /// `BARE_CHECK_FRAMES=1` and by the tests.
    pub check_frames: bool,
    /// Below this many fragments (default 4096), the display list holds the whole page, not a band around the window: the list is
    /// cheap, and a scroll never builds it again. The tests set 0, so that each page tests the band.
    pub whole: usize,
    /// The text fields of `frame` in `band`, with their fragments, found when its display list was built.
    inputs: Vec<(NodeId, u32)>,
    /// Listener errors and render errors, kept for tests. The first of each also goes to stderr, unless `quiet`.
    pub errors: Vec<String>,
    /// Print nothing (a test driver: it reports the errors itself, through its redacting filter).
    pub quiet: bool,
    /// Does a wake (`wake_on_done`) tell the loop that a job ended?
    woken: bool,
    pub(crate) started: Instant,
    pub first_frame: Option<Duration>,
    /// The rounds of the window loop that hosts this page for a driver (`control::Remote`), since a job last took them.
    pub ticks: crate::control::Ticks,
    /// The pictures of the document (`images.rs`).
    pub(crate) images: crate::images::Images,
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
    /// A textarea: the lines of `text` are drawn one under the other from the top of the box.
    area: bool,
}

/// The box of `n` in `f`, if it has one.
fn box_of(f: &Frame, n: NodeId) -> Option<&Fragment> {
    f.layout.fragments.iter().find(|g| g.node == n && g.kind == Kind::Box)
}

/// The text field geometry of `n`, an `<input>` or a `<textarea>` with the box `frag`.
fn field(f: &Frame, doc: &Document, n: NodeId, frag: &Fragment, text: &str, focus_end: Option<u32>) -> Option<Field> {
    let style = f.styles.style(n)?;
    let px = |p| style.px(p).max(0.0);
    let m = Metrics::of(style);
    let x = frag.rect.x + px(Property::BorderLeftWidth) + px(Property::PaddingLeft);
    let w = (frag.rect.w - px(Property::BorderLeftWidth) - px(Property::BorderRightWidth) - px(Property::PaddingLeft) - px(Property::PaddingRight)).max(0.0);
    let y0 = frag.rect.y + px(Property::BorderTopWidth) + px(Property::PaddingTop);
    let h = (frag.rect.h - px(Property::BorderTopWidth) - px(Property::BorderBottomWidth) - px(Property::PaddingTop) - px(Property::PaddingBottom)).max(0.0);
    let line = m.normal_line_height();
    let area = doc.is_html(n, "textarea");
    let baseline = if area { (y0 + m.ascent).round() as i32 } else { (y0 + (h - line) / 2.0 + m.ascent).round() as i32 };
    let shown = if !area && input_kind(doc, n) == "password" { "•".repeat(text.chars().count()) } else { text.to_string() };
    let scroll = focus_end.filter(|_| !area).map_or(0.0, |e| (x_of(&shown, e, &m) - w + 1.0).max(0.0));
    let clip = IRect { x0: x.floor() as i32, y0: y0.floor() as i32, x1: (x + w).ceil() as i32, y1: (y0 + h).ceil() as i32 };
    Some(Field { x, baseline, clip, m, color: argb(style.color(Property::TextColor)), text: shown, scroll, area })
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

/// The caret offset for a press at (`x`, `y`) in text field `n`, on the layout `fr`.
pub(crate) fn caret_in(b: &mut Browser, fr: &Frame, n: NodeId, x: f64, y: f64) -> Option<u32> {
    let value = b.input_value(n)?;
    let focus_end = b.input_selection(n).map(|s| s.1);
    let doc = b.document();
    let doc = doc.borrow();
    let mut f = field(fr, &doc, n, box_of(fr, n)?, &value, focus_end)?;
    place(fr, &doc, &b.scroll(), n, &mut f);
    if f.area {
        // The line under the pointer (the last one for a point below the text), then the offset inside it.
        let line = ((y as f32 - (f.baseline as f32 - f.m.ascent)) / f.m.normal_line_height()).floor().max(0.0) as usize;
        let (mut start, mut pick) = (0, ("", 0));
        for (i, t) in f.text.split('\n').enumerate() {
            pick = (t, start);
            if i == line {
                break;
            }
            start += t.encode_utf16().count() as u32 + 1;
        }
        return Some(pick.1 + offset_at(pick.0, x as f32 - f.x, &f.m));
    }
    Some(offset_at(&f.text, x as f32 - f.x + f.scroll, &f.m))
}

/// The window point of the boundary `at` of the text of field `n`, on the layout `fr`: the inverse of `caret_in`.
pub(crate) fn point_in(b: &mut Browser, fr: &Frame, n: NodeId, at: u32) -> Option<(f64, f64)> {
    let value = b.input_value(n)?;
    let focus_end = b.input_selection(n).map(|s| s.1);
    let doc = b.document();
    let doc = doc.borrow();
    let mut f = field(fr, &doc, n, box_of(fr, n)?, &value, focus_end)?;
    place(fr, &doc, &b.scroll(), n, &mut f);
    let top = f.baseline as f32 - f.m.ascent;
    let lh = f.m.normal_line_height();
    if f.area {
        let (mut line, mut start) = (0, 0);
        for t in f.text.split('\n') {
            let end = start + t.encode_utf16().count() as u32;
            if at <= end {
                return Some(((f.x + x_of(t, at - start, &f.m)) as f64, (top + (line as f32 + 0.5) * lh) as f64));
            }
            line += 1;
            start = end + 1;
        }
        return None;
    }
    Some(((f.x - f.scroll + x_of(&f.text, at, &f.m)) as f64, (top + lh / 2.0) as f64))
}

impl Page {
    pub fn new(source: Source, viewport: (u32, u32)) -> Page {
        Page::with_canvas(source, (viewport.0.max(1), viewport.1.max(1)), Canvas::new(0, 0))
    }

    fn with_canvas(source: Source, size: (u32, u32), canvas: Canvas<'static>) -> Page {
        Page {
            source,
            frame: None,
            painter: Painter::new(),
            size,
            canvas,
            windowed: false,
            pointer: Pointer::default(),
            inside: false,
            moved: false,
            key: (0, [0; 8]),
            stale: false,
            waiting_sheet: None,
            renders: 0,
            restyles: 0,
            kept: 0,
            built: false,
            listed: false,
            controls: Vec::new(),
            band: IRect { x0: 0, y0: 0, x1: 0, y1: 0 },
            ext: (false, Vec::new()),
            epoch: 0,
            last_hit: None,
            check_frames: std::env::var_os("BARE_CHECK_FRAMES").is_some_and(|v| v == "1"),
            whole: 4096,
            inputs: Vec::new(),
            errors: Vec::new(),
            quiet: false,
            woken: false,
            started: Instant::now(),
            first_frame: None,
            ticks: Default::default(),
            images: Default::default(),
        }
    }

    /// Load `url` in a live session. It returns at once with no document: the ticks download it, parse it and run its scripts
    /// (`wait_load` runs them until `load`). Run it on a thread with a big stack: the VM recurses.
    pub fn open(url: &str, viewport: (u32, u32)) -> Result<Page, String> {
        Page::open_with(url, viewport, None)
    }

    /// `open` with the storage of an earlier session (`None`: empty storage, a new profile).
    pub fn open_with(url: &str, viewport: (u32, u32), storage: Option<crate::browser_navigation::WebStorage>) -> Result<Page, String> {
        let mut cfg = SessionConfig::new(url);
        cfg.storage = storage;
        Page::open_cfg(cfg, viewport)
    }

    /// `open` in a new session with empty storage that shares the connections and the HTTP cache of `net`, as a new tab of one browser
    /// does. A driver that opens many pages pays no new TLS handshake and no new download of the immutable scripts of an app.
    pub fn open_in(url: &str, viewport: (u32, u32), net: &bare_net::http::Client) -> Result<Page, String> {
        Page::open_in_with(url, viewport, net, None)
    }

    /// `open_in` that starts from a saved state (`None`: empty storage). The state is plain data, so it can go to a window thread.
    pub fn open_in_with(url: &str, viewport: (u32, u32), net: &bare_net::http::Client, state: Option<&StorageState>) -> Result<Page, String> {
        Page::open_cfg(Page::config_in(url, net, state)?, viewport)
    }

    fn config_in(url: &str, net: &bare_net::http::Client, state: Option<&StorageState>) -> Result<SessionConfig, String> {
        let mut cfg = SessionConfig::new(url);
        if let Some(st) = state {
            let store = crate::browser_navigation::WebStorage::default();
            store.import(st).map_err(|e| format!("session state: {e:?}"))?;
            cfg.storage = Some(store);
        }
        let net = net.fork();
        cfg.client = Box::new(move || net.fork());
        Ok(cfg)
    }

    /// `open_in` in this window, as the `open` of a driver: the old document ends first, and the call returns at once. The window (or the
    /// canvas of a headless page) keeps the pixels of the old page until the first frame of the new one. If the new session
    /// cannot start, no document is left.
    pub fn reopen_in(&mut self, url: &str, net: &bare_net::http::Client) -> Result<(), String> {
        self.reopen_in_with(url, net, None)
    }

    /// `reopen_in` that starts from a saved state (`None`: empty storage).
    pub fn reopen_in_with(&mut self, url: &str, net: &bare_net::http::Client, state: Option<&StorageState>) -> Result<(), String> {
        let cfg = Page::config_in(url, net, state)?;
        self.release_frame();
        if let Source::Session(s) = &mut self.source {
            s.end_document();
        }
        let s = Page::live_session(cfg, self.viewport())?;
        let canvas = std::mem::replace(&mut self.canvas, Canvas::new(0, 0));
        let (ticks, woken, quiet, windowed) = (self.ticks, self.woken, self.quiet, self.windowed);
        *self = Page::with_canvas(Source::Session(Box::new(s)), self.size, canvas);
        (self.ticks, self.woken, self.quiet, self.windowed) = (ticks, woken, quiet, windowed);
        Ok(())
    }

    fn open_cfg(cfg: SessionConfig, viewport: (u32, u32)) -> Result<Page, String> {
        Ok(Page::new(Source::Session(Box::new(Page::live_session(cfg, viewport)?)), viewport))
    }

    fn live_session(mut cfg: SessionConfig, viewport: (u32, u32)) -> Result<Session, String> {
        let url = cfg.url.clone();
        cfg.viewport = Some((viewport.0 as f32, viewport.1 as f32));
        // A window runs for hours, not for one load: an input event adds a little time and a few tasks each.
        cfg.limits.max_time = Duration::from_secs(7 * 24 * 3600);
        cfg.limits.max_documents = usize::MAX;
        cfg.limits.max_tasks = usize::MAX;
        cfg.limits.max_rounds = usize::MAX;
        // Wait for no timer that is not due: the window cannot sleep inside an event.
        cfg.page.quiet = Duration::ZERO;
        // An input event or a tick is one turn of the event loop: it waits for no reply and no next document.
        cfg.live = true;
        let mut s = Session::new(cfg);
        if let SessionStop::LoadFailed(why) = s.run().stop {
            return Err(format!("{url}: {why}"));
        }
        Ok(s)
    }

    /// Run the ticks until the document is in and has fired `load`, as the old blocking `open` did. `Err`: the load failed, the page
    /// stopped, or `limit` passed.
    pub fn wait_load(&mut self, limit: Duration) -> Result<(), String> {
        let end = Instant::now() + limit;
        loop {
            let loaded = match &mut self.source {
                Source::Session(s) => s.loading().is_none() && s.browser().is_some_and(|b| b.loaded()),
                Source::Browser(_) => true,
            };
            if loaded {
                return Ok(());
            }
            let (ran, next) = self.tick();
            if ran {
                continue;
            }
            let Some(next) = next.filter(|&n| n < end) else {
                return Err(if Instant::now() < end && next.is_none() { self.no_document() } else { "the time limit passed before load".into() });
            };
            std::thread::sleep(next.saturating_duration_since(Instant::now()));
        }
    }

    /// Is a document on its way, or in but not shown yet? A live session loads it in the ticks after `open` and `navigate`.
    pub fn loading(&self) -> bool {
        matches!(&self.source, Source::Session(s) if s.loading().is_some())
    }

    /// Why the last document did not load, if it did not. The ticks of a live session load it after `open` and `navigate`.
    pub fn load_error(&self) -> Option<&str> {
        match &self.source {
            Source::Session(s) => match s.ended() {
                Some(SessionStop::LoadFailed(why)) => Some(why),
                _ => None,
            },
            Source::Browser(_) => None,
        }
    }

    /// Why the page stopped for good, if it did: a fatal error of the engine or the host, or too many tasks aborted by script budgets
    /// (`Fatal::Aborted`). The DOM stays and can be read, but no script, timer or input runs any more. A driver should fail a step
    /// that waits for a change with this text.
    pub fn stopped(&self) -> Option<String> {
        match &self.source {
            Source::Session(s) => match s.ended() {
                Some(SessionStop::Page(crate::Stop::Fatal(crate::Fatal::Aborted(why)))) => Some(why.clone()),
                Some(SessionStop::Page(crate::Stop::Fatal(f))) => Some(format!("{f:?}")),
                _ => None,
            },
            Source::Browser(_) => None,
        }
    }

    /// Why the page has no document (or no loaded one): the load failed, the document is on its way, or the page stopped.
    pub fn no_document(&self) -> String {
        match &self.source {
            Source::Session(s) => match s.ended() {
                Some(SessionStop::LoadFailed(why)) => format!("the document did not load: {why}"),
                Some(stop) => format!("the page stopped: {stop:?}"),
                None if s.loading().is_some() => "no document yet: it is loading".into(),
                None => "no document".into(),
            },
            Source::Browser(_) => "no document".into(),
        }
    }

    pub fn browser(&mut self) -> Option<&mut Browser> {
        self.source.browser()
    }

    /// A picture of a style arrived. Layout does not read it: the display list is made again from the same layout.
    pub(crate) fn relist(&mut self) {
        (self.listed, self.built, self.stale) = (false, false, true);
    }

    /// The page changed outside the window events (a driver ran a script): the next frame checks the render key.
    pub fn touched(&mut self) {
        self.stale = true;
    }

    /// The pixels of a frame made from nothing (a new cascade, layout, display list and glyph cache), and its fragments and page
    /// bars. The kept frame, and the changes that the document logged for it, stay as they were.
    pub(crate) fn paint_fresh(&mut self) -> (Vec<u32>, Option<Fresh>) {
        let frame = self.frame.take();
        let painter = std::mem::replace(&mut self.painter, Painter::new());
        let (key, stale, built, listed, renders, inputs, controls, inside, band, ext, moved) =
            (self.key, self.stale, self.built, self.listed, self.renders, self.inputs.clone(), self.controls.clone(), self.inside, self.band, std::mem::take(&mut self.ext), self.moved);
        let damage = self.browser().map(|b| b.document().borrow().peek_damage());
        self.inside = false;
        let check = std::mem::replace(&mut self.check_frames, false);
        let px = self.paint().to_vec();
        let fresh = self.frame.take().map(|f| (f.layout.fragments, f.bars));
        if let (Some(d), Some(b)) = (damage, self.browser()) {
            b.document().borrow_mut().restore_damage(d);
        }
        (self.frame, self.painter, self.key, self.stale, self.built, self.listed, self.renders, self.inputs, self.controls, self.inside, self.band, self.ext, self.check_frames, self.moved) =
            (frame, painter, key, stale, built, listed, renders, inputs, controls, inside, band, ext, check, moved);
        (px, fresh)
    }

    /// Compare the frame of the window with a frame made from nothing: the pixels, the fragments and the page bars. The first
    /// difference, if any.
    pub fn check_frame(&mut self) -> Result<(), String> {
        let px = self.canvas.pixels.to_vec();
        self.check_pixels(&px)
    }

    /// `check_frame` for the pixels `px` of the frame.
    fn check_pixels(&mut self, px: &[u32]) -> Result<(), String> {
        let kept = self.frame.as_ref().map(|f| (f.layout.fragments.clone(), f.bars));
        let (fresh_px, fresh) = self.paint_fresh();
        if let Some(i) = px.iter().zip(&fresh_px).position(|(a, b)| a != b) {
            let n = px.iter().zip(&fresh_px).filter(|(a, b)| a != b).count();
            return Err(format!("{n} pixels differ from a frame made from nothing, the first at {},{}", i % self.size.0 as usize, i / self.size.0 as usize));
        }
        match (kept, fresh) {
            (Some((a, ab)), Some((b, bb))) => {
                if let Some(i) = (0..a.len().max(b.len())).find(|&i| a.get(i) != b.get(i)) {
                    return Err(format!("fragment {i} of {} differs: {:?} and {:?} made from nothing", a.len(), a.get(i), b.get(i)));
                }
                if ab != bb {
                    return Err(format!("page bars {ab:?} and {bb:?} made from nothing"));
                }
                Ok(())
            }
            (None, None) => Ok(()),
            (a, b) => Err(format!("a frame: {}, made from nothing: {}", a.is_some(), b.is_some())),
        }
    }

    /// The size of the frame: the viewport given at the open, or the window size after a `resize`.
    pub fn viewport(&self) -> (u32, u32) {
        self.size
    }

    /// The layout of the last frame, made now if the page changed. A stale frame with the same render key keeps its layout.
    pub fn frame_now(&mut self) -> Option<&Frame> {
        if self.frame.is_none() || self.stale {
            self.stale = false;
            self.built = false;
            // The focus fixup rule: a focused element that left the document or became inert gives the focus to the viewport (`:focus-within`).
            if let Some(b) = self.browser() {
                let _ = b.active_element();
            }
            let doc = self.doc_id();
            let key = self.browser().map(|b| (doc, b.render_key()));
            if self.frame.is_none() && key.is_some() && key == self.waiting_sheet {
                return None;
            }
            if self.frame.is_none() || key != Some(self.key) {
                self.epoch += 1;
                // The same document, window and sheets: the frame is updated for what changed (`Browser::refresh`).
                let near = key.is_some_and(|k| k.0 == self.key.0 && k.1[2..4] == self.key.1[2..4]);
                let Page { source, frame, .. } = self;
                let quick = match (source.browser(), frame.as_mut()) {
                    (Some(b), Some(f)) if near => b.refresh(f),
                    _ => Ok(None),
                };
                match quick {
                    Ok(Some(Change::Same)) => self.key = key.expect("a browser"),
                    Ok(Some(Change::Paint)) => {
                        self.key = key.expect("a browser");
                        self.listed = false;
                        self.restyles += 1;
                    }
                    Ok(Some(Change::Layout)) => {
                        self.key = (doc, self.browser().expect("a browser").render_key());
                        (self.listed, self.ext.0, self.moved) = (false, false, true);
                        self.renders += 1;
                        self.kept += 1;
                    }
                    Err(e) => {
                        self.frame = None;
                        self.render_failed(&e, key);
                    }
                    Ok(None) => self.render_all(doc, key),
                }
            }
        }
        self.frame.as_ref()
    }

    /// The loop of this page sleeps until `wake` is called from another thread. Then a reply or a next document in flight does not make
    /// `tick` ask for a call every 4 ms: the worker that ends the job calls `wake` (`bare_net::parallel::on_end`). `tick` then asks
    /// for a call each second only, in case a wake was lost. One hook for the process: the last call wins.
    pub fn wake_on_done(&mut self, wake: impl Fn() + Send + Sync + 'static) {
        bare_net::parallel::on_end(wake);
        self.woken = true;
    }

    /// Run the tasks that are due now (timers, animation frames, and the `fetch` calls and navigations that they start), as a browser
    /// does between input events. Gives whether a task ran (the next frame checks the render key then) and when the next task is due.
    /// `None`: no task is queued, or the page stopped for good (a fatal error), so only an input event can change it.
    /// A live page (a session) runs one slice of work in a tick, and a reply or a next document that is on its way is queued work: the
    /// tick asks to be called again in 4 ms (1 s when `wake_on_done` was called).
    pub fn tick(&mut self) -> (bool, Option<Instant>) {
        // A picture that arrived is a change of the page: the next call (a capture waits for the pictures) sees it.
        let (arrived, finished) = self.images_step();
        self.stale |= arrived;
        let ran = self.work().0;
        if ran {
            self.settle();
        }
        let (now, next) = self.work();
        (ran || finished, if now || finished { Some(Instant::now()) } else { next })
    }

    /// Can a tick do something now, and when is the next task due (or the next look at the network)?
    fn work(&mut self) -> (bool, Option<Instant>) {
        let poll = if self.woken { Duration::from_secs(1) } else { Duration::from_millis(4) };
        let (stopped, loading) = match &self.source {
            Source::Session(s) => (s.ended().is_some(), s.loading()),
            Source::Browser(b) => (b.sched_stats().halted, None),
        };
        if let Some(done) = loading {
            return (done, Some(Instant::now() + poll));
        }
        let images = self.images.pending();
        let Some(b) = self.browser().filter(|_| !stopped) else { return (false, images.then(|| Instant::now() + poll)) };
        let (due, now) = (b.next_scheduled_due(), b.now());
        let net = if b.live() { b.live_work() } else { (b.pending_fetches() > 0).then_some(true) };
        let at = due.map(|d| Instant::now() + Duration::from_secs_f64((d - now).max(0.0) / 1000.0));
        let at = if net == Some(false) || images { Some(at.map_or(Instant::now() + poll, |a| a.min(Instant::now() + poll))) } else { at };
        (due.is_some_and(|d| d <= now) || net == Some(true), at)
    }

    /// A new cascade, and a new layout unless only values that layout does not read changed. `refresh` could not update the frame.
    fn render_all(&mut self, doc: usize, key: Option<(usize, [u64; 8])>) {
        // Only the `:hover`, `:active`, focus or form state moved: the tree is the same, so a cascade whose values layout does not read keeps the layout.
        let user_only = self.frame.is_some() && key.is_some_and(|k| k.0 == self.key.0 && k.1[..4] == self.key.1[..4]);
        let styles = self.browser().map(|b| b.restyle());
        let change = match (&styles, &self.frame) {
            (Some(Ok(new)), Some(f)) if user_only => f.styles.change(new),
            _ => Change::Layout,
        };
        match (styles, change) {
            (Some(Ok(new)), Change::Same | Change::Paint) => {
                // The new styles have the same values, or differ only in what layout does not read: the layout stays.
                let Page { source, frame, .. } = self;
                if let (Some(b), Some(f)) = (source.browser(), frame.as_mut()) {
                    b.restyle_frame(f, new);
                }
                self.key = key.expect("a browser");
                if change == Change::Paint {
                    self.listed = false;
                    self.restyles += 1;
                }
            }
            (Some(Ok(new)), _) => {
                let old = self.frame.take();
                match self.browser().map(|b| b.render_with(new, old).map(|f| (f, b.render_key()))) {
                    Some(Ok((f, k))) => {
                        self.frame = Some(f);
                        (self.listed, self.ext.0) = (false, false);
                        self.key = (doc, k);
                        self.renders += 1;
                        self.moved = true;
                    }
                    Some(Err(e)) => self.render_failed(&e, key),
                    None => {}
                }
            }
            (Some(Err(e)), _) => {
                self.frame = None;
                self.render_failed(&e, key);
            }
            (None, _) => self.frame = None,
        }
    }

    /// A render stopped. A style sheet that is not supplied yet is not a failure while the session runs: the page waits for it, quietly,
    /// and the key says when to try again. It is a line of the console only if the session has ended (the sheet will never come).
    fn render_failed(&mut self, e: &bare_js::eval::Error, key: Option<(usize, [u64; 8])>) {
        if crate::browser_computed::is_missing_sheet(e) {
            self.waiting_sheet = key;
            if matches!(&self.source, Source::Session(s) if s.ended().is_none()) {
                return;
            }
        }
        self.note(format!("render: {e:?}"));
    }

    fn note(&mut self, text: String) {
        if self.errors.is_empty() && !self.quiet {
            eprintln!("bare: {text}");
        }
        self.errors.push(text);
    }

    /// The element under the window point: one hit test for each point, frame and set of offsets (a click asks three times).
    pub(crate) fn hit(&mut self, x: f64, y: f64) -> Option<NodeId> {
        self.frame_now()?;
        let Page { source, frame, ext, epoch, last_hit, .. } = self;
        let b = source.browser()?;
        let scroll = b.scroll();
        if let Some((hx, hy, g, s, n)) = last_hit
            && (*hx, *hy, *g) == (x, y, *epoch)
            && *s == *scroll
        {
            return *n;
        }
        let doc = b.document();
        let doc = doc.borrow();
        let f = frame.as_ref()?;
        if !ext.0 {
            extents(&f.styles, &f.layout, &mut ext.1);
            ext.0 = true;
        }
        let ext = &ext.1;
        let n = bare_paint::hit::hit_test_within(&doc, &f.styles, &f.layout, &f.areas, &scroll, ext, x as f32, y as f32);
        *last_hit = Some((x, y, *epoch, scroll.clone(), n));
        n
    }

    /// After the boxes moved under a still pointer, Chrome sends a "fake mouse move": `:hover` and the boundary events follow the new
    /// layout. Once for each frame, so a listener that changes the page again cannot loop.
    pub(crate) fn rehover(&mut self) {
        self.frame_now();
        if !(self.inside && self.moved) {
            return;
        }
        self.moved = false;
        let p = self.pointer;
        let to = self.hit(p.x, p.y);
        if let Some(r) = self.browser().map(|b| b.ui_pointer_settle(to, p)) {
            self.after(r);
        }
    }

    /// The caret offset for a press at `x` in text field `n`.
    fn caret_at(&mut self, n: NodeId, x: f64, y: f64) -> Option<u32> {
        self.frame_now()?;
        let Page { source, frame, .. } = self;
        caret_in(source.browser()?, frame.as_ref()?, n, x, y)
    }

    /// Which document is the page now? A node of an old one means nothing after a navigation.
    pub(crate) fn doc_id(&mut self) -> usize {
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

    /// The document is about to end. Drop the frame (styles, layout, display list) that was made for it. The pixels of the
    /// old page stay on screen (or in the canvas) until the new one has its first paint. Then the old document and the new one never share the heap.
    pub(crate) fn release_frame(&mut self) {
        self.frame = None;
        self.painter.list = Default::default();
        (self.built, self.listed, self.controls, self.inputs, self.ext, self.last_hit, self.key) = (false, false, Vec::new(), Vec::new(), (false, Vec::new()), None, (0, [0; 8]));
    }

    /// Run what a listener queued, then forget the layout. A link click ends the document: the session loads the next.
    pub(crate) fn settle(&mut self) {
        // A next document that is in replaces the old one in this call: the frame of the old one goes first.
        if matches!(&self.source, Source::Session(s) if s.loading() == Some(true)) {
            self.release_frame();
        }
        let fatal = match &mut self.source {
            Source::Session(s) => match s.resume(&[]).stop {
                SessionStop::Page(crate::Stop::Fatal(f)) => Some(format!("{f:?}")),
                SessionStop::LoadFailed(why) => Some(why),
                _ => None,
            },
            Source::Browser(b) => match b.drain().and_then(|_| b.run_navigation_tasks()).and_then(|_| b.run_scheduled_tasks(1000)) {
                Ok(_) => None,
                Err(e) => match b.survive(e, "task") {
                    Survive::Aborted => None,
                    Survive::Exhausted(why) => Some(why),
                    Survive::Fatal => Some(format!("{e:?}")),
                },
            },
        };
        if let Some(f) = fatal {
            self.note(format!("page: {f}"));
        }
        self.stale = true;
    }

    pub(crate) fn after(&mut self, r: Result<UiOutcome, bare_js::eval::Error>) -> bool {
        let out = match r {
            Ok(o) => o,
            Err(e) => {
                // A script budget ends only this task (the console has the error); the page takes the next input.
                match self.browser().map(|b| b.survive(e, "event handler")) {
                    Some(Survive::Aborted) => {}
                    Some(Survive::Exhausted(why)) => self.note(format!("input: {why}")),
                    _ => self.note(format!("input: {e:?}")),
                }
                // The call may have changed the page before it stopped.
                UiOutcome { reports: Vec::new(), changed: true, intercepted: None }
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
                (self.inside, self.moved) = (true, false);
                let (p, doc) = (self.pointer, self.doc_id());
                let r = self.browser().map(|b| b.ui_pointer_move(target, p));
                if let Some(r) = r {
                    h.redraw = self.after(r);
                }
                // A drag that started in a text field selects text, also outside the field.
                if buttons & 1 != 0
                    && let Some(n) = self.browser().and_then(|b| b.selecting())
                    && let Some(c) = self.caret_at(n, x, y)
                    && let Some(r) = self.browser().map(|b| b.ui_drag_select(c))
                {
                    h.redraw |= self.after(r);
                }
                h.cursor = Some(if self.doc_id() == doc { self.cursor_over(target) } else { Cursor::Default });
            }
            Event::PointerButton { x, y, button, buttons, pressed, mods: m } => {
                self.pointer = Pointer { x, y, buttons, mods: mods(m) };
                let target = self.hit(x, y);
                let caret = match target {
                    Some(n) if pressed && button == 0 && self.browser().is_some_and(|b| b.is_text_input(n)) => self.caret_at(n, x, y),
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
                self.inside = false;
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

    /// Paint the page into its own canvas (headless). The canvas keeps the last frame while the next document is on its way.
    pub fn paint(&mut self) -> &[u32] {
        let mut canvas = std::mem::replace(&mut self.canvas, Canvas::new(0, 0));
        if (canvas.width, canvas.height) != self.size {
            canvas = Canvas::new(self.size.0, self.size.1);
        }
        self.paint_into(&mut canvas);
        self.canvas = canvas;
        &self.canvas.pixels
    }

    /// Paint the page into `canvas` (of the size of the page), then the fields. The display list is built once for a frame; a scroll
    /// draws it again. All of the canvas is drawn. `false`: a next document is on its way, and nothing was drawn (the last frame stays).
    pub(crate) fn paint_into(&mut self, canvas: &mut Canvas) -> bool {
        self.rehover();
        self.frame_now();
        let mut drew = true;
        let Page { source, frame, painter, built, listed, controls, inputs, band, ext, whole, .. } = self;
        let b = source.browser();
        if let (Some(b), Some(f)) = (b, frame.as_ref()) {
            let focus = b.active_element().ok().flatten();
            let scroll = b.scroll().clone();
            let doc = b.document();
            let doc = doc.borrow();
            // What the window shows, in page pixels. The list holds a band around it: a scroll that leaves the band builds it again.
            let (w, h) = (canvas.width as i32, canvas.height as i32);
            let shown = IRect { x0: scroll.x.round() as i32, y0: scroll.y.round() as i32, x1: scroll.x.round() as i32 + w, y1: scroll.y.round() as i32 + h };
            if shown.intersect(*band) != shown {
                (*built, *listed) = (false, false);
            }
            if !*built {
                if !ext.0 {
                    extents(&f.styles, &f.layout, &mut ext.1);
                    ext.0 = true;
                }
                let ext = &ext.1;
                // The state of the controls that the list shows: paint draws the placeholder of an empty field, the option of a select
                // and the mark of a checkbox or a radio. The text of a field, the selection and the caret are drawn here.
                if !*listed {
                    *band = if f.layout.fragments.len() < *whole { ALL } else { IRect { x0: shown.x0 - w, y0: shown.y0 - h, x1: shown.x1 + w, y1: shown.y1 + h } };
                    controls.clear();
                    each_in_band(&f.styles, &f.layout, ext, *band, |j, g| {
                        if g.kind == Kind::Box && ["input", "textarea", "select"].iter().any(|t| doc.is_html(g.node, t)) {
                            controls.push((g.node, j as u32));
                        }
                    });
                }
                let (mut chosen, mut ticked) = (Vec::new(), Vec::new());
                inputs.clear();
                for &(n, j) in controls.iter() {
                    if doc.is_html(n, "select") {
                        chosen.push((n, b.selected_option(n)));
                    } else if b.is_text_input(n) {
                        inputs.push((n, j));
                    } else if matches!(input_kind(&doc, n), "checkbox" | "radio") {
                        ticked.push((n, b.input_checked(n)));
                    }
                }
                let hosted: Vec<_> = inputs.iter().map(|&(n, _)| (n, b.input_value(n).is_some_and(|v| !v.is_empty()))).collect();
                let list = &mut painter.list;
                if !*listed || hosted != list.hosted || chosen != list.chosen || ticked != list.ticked {
                    (list.hosted, list.chosen, list.ticked) = (hosted, chosen, ticked);
                    painter.build_band(&doc, &f.styles, &f.layout, &f.areas, *band, ext);
                    *listed = true;
                }
                *built = true;
            }
            painter.draw_view(canvas, Some(&View { scroll: &scroll, areas: &f.areas, bars: f.bars }));
            for &(n, j) in inputs.iter() {
                let value = b.input_value(n).unwrap_or_default();
                let sel = b.input_selection(n);
                let focused = focus == Some(n);
                let end = sel.filter(|_| focused).map(|s| if s.2 == "backward" { s.0 } else { s.1 });
                let Some(mut fld) = field(f, &doc, n, &f.layout.fragments[j as usize], &value, end) else { continue };
                place(f, &doc, &scroll, n, &mut fld);
                Page::draw_field(painter, canvas, &fld, sel.filter(|_| focused));
            }
        } else if !matches!(source, Source::Session(s) if s.loading().is_some()) {
            canvas.pixels.fill(0xFFFF_FFFF);
        } else {
            // A next document on its way: the last frame of the old one stays.
            drew = false;
        }
        self.first_frame.get_or_insert_with(|| self.started.elapsed());
        if drew
            && self.check_frames
            && let Err(e) = self.check_pixels(&canvas.pixels)
        {
            panic!("BARE_CHECK_FRAMES: {e}");
        }
        drew
    }

    /// Run `f` on the page geometry and a copy of the offsets; keep the offsets when it says that it moved something.
    pub(crate) fn scroll_with(&mut self, f: impl FnOnce(&Geo, &mut Scroll) -> bool) -> bool {
        self.frame_now();
        let Page { source, frame, size, .. } = self;
        let (Some(b), Some(fr)) = (source.browser(), frame.as_ref()) else { return false };
        let mut s = b.scroll().clone();
        let moved = {
            let doc = b.document();
            let doc = doc.borrow();
            f(&Geo { doc: &doc, styles: &fr.styles, layout: &fr.layout, areas: &fr.areas, window: (size.0 as f32, size.1 as f32), bars: fr.bars }, &mut s)
        };
        if moved {
            b.set_scroll(s);
            self.moved = true;
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
    pub(crate) fn send_scroll_events(&mut self) {
        if self.browser().is_some_and(|b| b.scroll_pending()) {
            let r = self.browser().map(|b| b.ui_scroll_events());
            if let Some(r) = r {
                self.after(r);
            }
        }
    }

    /// The lines of a textarea, with the selection and the caret on the lines they fall on.
    fn draw_area(painter: &mut Painter, canvas: &mut Canvas, f: &Field, sel: Option<(u32, u32, &str)>) {
        let line = f.m.normal_line_height();
        let mut start = 0;
        for (i, text) in f.text.split('\n').enumerate() {
            let end = start + text.encode_utf16().count() as u32;
            let baseline = f.baseline + (i as f32 * line).round() as i32;
            let top = (baseline as f32 - f.m.ascent).round() as i32;
            if let Some((s, e, _)) = sel {
                let at = |u: u32| (f.x + x_of(text, u - start, &f.m)).round() as i32;
                if s != e && s <= end && e > start {
                    canvas.fill(IRect { x0: at(s.max(start)), y0: top, x1: at(e.min(end)), y1: top + line as i32 }.intersect(f.clip), 0xFFB3_D7FF);
                } else if s == e && (start..=end).contains(&s) {
                    canvas.fill(IRect { x0: at(s), y0: top, x1: at(s) + 1, y1: top + line as i32 }.intersect(f.clip), f.color);
                }
            }
            bare_paint::text(canvas, &mut painter.glyphs, f.clip, f.x, baseline, f.m.size, f.color, false, (0.0, 0.0), text);
            start = end + 1;
        }
    }

    fn draw_field(painter: &mut Painter, canvas: &mut Canvas, f: &Field, sel: Option<(u32, u32, &str)>) {
        if f.area {
            return Page::draw_area(painter, canvas, f, sel);
        }
        let x0 = f.x - f.scroll;
        let line = f.m.normal_line_height();
        let top = (f.baseline as f32 - f.m.ascent).round() as i32;
        if let Some((s, e, _)) = sel.filter(|s| s.0 != s.1) {
            let (a, b) = (x_of(&f.text, s, &f.m), x_of(&f.text, e, &f.m));
            canvas.fill(IRect { x0: (x0 + a).round() as i32, y0: top, x1: (x0 + b).round() as i32, y1: top + line as i32 }.intersect(f.clip), 0xFFB3_D7FF);
        }
        bare_paint::text(canvas, &mut painter.glyphs, f.clip, x0, f.baseline, f.m.size, f.color, false, (0.0, 0.0), &f.text);
        if let Some((s, _, _)) = sel.filter(|s| s.0 == s.1) {
            let cx = (x0 + x_of(&f.text, s, &f.m)).round() as i32;
            canvas.fill(IRect { x0: cx, y0: top, x1: cx + 1, y1: top + line as i32 }.intersect(f.clip), f.color);
        }
    }
}

impl App for Page {
    fn resize(&mut self, width: u32, height: u32) {
        self.size = (width, height);
        let r = self.browser().map(|b| b.set_viewport(width as f32, height as f32));
        if let Some(Err(e)) = r {
            self.note(format!("viewport: {e:?}"));
        }
        // The window size is in the render key: the next frame cascades (`@media`, `vw`) and lays out again.
        self.settle();
    }

    fn frame(&mut self, _: &mut Control, out: &mut [u32]) -> bool {
        self.windowed = true;
        self.paint_into(&mut Canvas::over(self.size.0, self.size.1, out))
    }

    fn tick(&mut self, ctl: &mut Control) -> Option<Instant> {
        let (ran, next) = Page::tick(self);
        if ran {
            ctl.redraw();
        }
        next
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
