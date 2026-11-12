//! The local control interface (S5): a test driver acts on a [`Page`] through the paths of the window, and nothing else.
//!
//! - Navigation: `Page::open_with` (a new session: a new profile, or the storage of an earlier one), `Page::open_in` (a new session that
//!   shares the connections and the HTTP cache of a client), `navigate` (the address bar) and
//!   `navigate(None)` (reload). They use the session loader of `--run` and `--window`.
//! - Queries: `query_all` (CSS selectors, the native matcher of `querySelectorAll`), `inspect` (read the DOM and the frame of the
//!   window), and the state reads `connected`, `disabled`, `value`, `checked`, `focused`. A driver uses them to find elements and to
//!   check them. They change nothing.
//! - Actions: `click`, `type_text`, `press` and `wheel` make `bare_window::Event`s and give them to `Page::handle`, as the Wayland loop
//!   does. A click hit-tests the point on the frame of the window (`bare_paint::hit`), so the events go to the element on top there,
//!   and focus, default actions, `fetch` calls and navigations follow from the events. No action writes the DOM, a value, the focus or
//!   the storage directly.
//! - Time: `Page::tick` runs the tasks that are due (timers, frames, the `fetch` calls they start). A driver calls it while it waits.
//! - Pixels: `capture` paints the frame that the window would show.
//!
//! Before an action, the target must be actionable, else the action is refused and nothing is sent ([`Refusal`]): the handle is from the
//! document of now and the node is connected; the element has a box with a size, and not `visibility: hidden`; it is not disabled
//! (HTML `disabled`, also by a `fieldset`, or `aria-disabled="true"`); after a scroll into view (only if needed, as Chrome's
//! `DOM.scrollIntoViewIfNeeded`), the center of the part of its border box that is in the window hits the element or one of its
//! descendants. A hit in a shadow tree counts for its host. Playwright checks the same before a click (visible, stable, enabled, receives
//! events). `type_text` also needs an editable text `<input>` or `<textarea>`, and the field must have the focus after the click.
//!
//! Transport: the API is in-process. A driver that runs headless owns the `Page` and calls it on its thread (a thread with a big stack:
//! the VM recurses). A driver that wants a real Wayland surface uses [`Remote`]: the page and its window run on their own thread, and
//! each call is a job that the window loop runs in `App::tick`, between input events. `docs/decisions.md` ("S5 control interface")
//! has the reasons.
//!
//! Gaps: no drag, no right button, no modifier keys on a click; `type_text` sends one key for each character (no IME, no
//! paste); the viewport of a new document is the one of `open_with` (a resize of a real window is not given to the next document).

use crate::browser::Browser;
use crate::browser_navigation::WebStorage;
use crate::browser_scroll::Align;
use crate::browser_session::SessionStop;
use crate::browser_ui::Frame;
use crate::page::{Page, Source};
use bare_css::values::{Keyword, Property, Value as Css};
use bare_html::dom::{Document, NodeId};
use bare_paint::canvas::Canvas;
use bare_window::{App, Control, Event, Mods};
use std::sync::mpsc;
use std::time::Instant;

/// An element of one document of the page. A handle of an older document is `Detached`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Handle {
    pub doc: usize,
    pub node: NodeId,
}

/// Why an action was not done. Nothing was sent to the page.
#[derive(Clone, Debug, PartialEq)]
pub enum Refusal {
    /// The node left the document, or the page has loaded another document since.
    Detached,
    /// No box with a size (`display: none`, an empty box), `visibility: hidden`, or none of it is in the window after the scroll.
    Hidden(&'static str),
    Disabled,
    /// Another element is on top at the point: its `tag#id.class`, the element (`None`: nothing takes the pointer there), and the point in
    /// window pixels.
    Covered { by: String, top: Option<Handle>, at: (i32, i32) },
    /// `type_text` needs an editable text `<input>` or `<textarea>`.
    NotEditable,
    /// The field did not have the focus after the click (a listener canceled it).
    NoFocus,
    /// The page has no document, or it cannot be laid out (the error of the render).
    NoPage(String),
    /// The element is not the kind that the action needs, or the action did not give the state: a fixed label for the kind, and why.
    Wrong(&'static str, String),
}

/// One line of the diagnostics of a page: `level` is `log`, `warn` or `error` (the console), `exception` (a listener threw), `page` (a
/// fatal stop or a render error) or `fetch` (a request that was blocked or failed; fixed labels and the origin only).
#[derive(Clone, Debug)]
pub struct Log {
    pub level: &'static str,
    pub text: String,
}

const LETTERS: [&str; 26] = ["KeyA", "KeyB", "KeyC", "KeyD", "KeyE", "KeyF", "KeyG", "KeyH", "KeyI", "KeyJ", "KeyK", "KeyL", "KeyM", "KeyN", "KeyO", "KeyP", "KeyQ", "KeyR", "KeyS", "KeyT", "KeyU", "KeyV", "KeyW", "KeyX", "KeyY", "KeyZ"];
const DIGITS: [&str; 10] = ["Digit0", "Digit1", "Digit2", "Digit3", "Digit4", "Digit5", "Digit6", "Digit7", "Digit8", "Digit9"];

/// The DOM `code` of the key that types `c` on a US keyboard, `""` for a character with no key.
fn code_of(c: char) -> &'static str {
    match c {
        'a'..='z' => LETTERS[(c as u8 - b'a') as usize],
        'A'..='Z' => LETTERS[(c as u8 - b'A') as usize],
        '0'..='9' => DIGITS[(c as u8 - b'0') as usize],
        ' ' => "Space",
        '-' | '_' => "Minus",
        '=' | '+' => "Equal",
        '.' | '>' => "Period",
        ',' | '<' => "Comma",
        '/' | '?' => "Slash",
        ';' | ':' => "Semicolon",
        '\'' | '"' => "Quote",
        '[' | '{' => "BracketLeft",
        ']' | '}' => "BracketRight",
        '\\' | '|' => "Backslash",
        '`' | '~' => "Backquote",
        _ => "",
    }
}

/// The events of a key spec (`Enter`, `a`, `Shift+Tab`, `Control+a`): the modifier keys go down, the key goes down and up, the modifier
/// keys go up in the reverse order. The text of a key is what the window gives: typed text only, none with Control, Alt or Meta held.
pub fn key_events(spec: &str) -> Option<Vec<Event>> {
    let (mods, last) = match spec.rfind('+') {
        Some(i) if i + 1 < spec.len() => (&spec[..i], &spec[i + 1..]),
        _ => ("", spec),
    };
    let key = |key: &str, code, pressed, text: Option<&str>, mods| Event::Key { key: key.into(), code, pressed, repeat: false, text: text.map(Into::into), mods };
    let (mut held, mut down, mut up) = (Mods::default(), Vec::new(), Vec::new());
    for m in mods.split('+').filter(|m| !m.is_empty()) {
        let (name, code, flag) = match m {
            "Shift" => ("Shift", "ShiftLeft", &mut held.shift),
            "Control" | "Ctrl" => ("Control", "ControlLeft", &mut held.ctrl),
            "Alt" => ("Alt", "AltLeft", &mut held.alt),
            "Meta" => ("Meta", "MetaLeft", &mut held.meta),
            _ => return None,
        };
        *flag = true;
        down.push(key(name, code, true, None, held));
        up.push((name, code));
    }
    let (name, code, text): (&str, &'static str, Option<&str>) = match last {
        "Space" | " " => (" ", "Space", Some(" ")),
        "Esc" | "Escape" => ("Escape", "Escape", None),
        "Enter" => ("Enter", "Enter", None),
        "Tab" => ("Tab", "Tab", None),
        "Backspace" => ("Backspace", "Backspace", None),
        "Delete" => ("Delete", "Delete", None),
        "Home" => ("Home", "Home", None),
        "End" => ("End", "End", None),
        "PageUp" => ("PageUp", "PageUp", None),
        "PageDown" => ("PageDown", "PageDown", None),
        "ArrowUp" => ("ArrowUp", "ArrowUp", None),
        "ArrowDown" => ("ArrowDown", "ArrowDown", None),
        "ArrowLeft" => ("ArrowLeft", "ArrowLeft", None),
        "ArrowRight" => ("ArrowRight", "ArrowRight", None),
        _ if last.chars().count() == 1 => (last, code_of(last.chars().next()?), Some(last)),
        _ => return None,
    };
    let text = text.filter(|_| !(held.ctrl || held.alt || held.meta));
    down.push(key(name, code, true, text, held));
    down.push(key(name, code, false, None, held));
    for (name, code) in up.into_iter().rev() {
        match name {
            "Shift" => held.shift = false,
            "Control" => held.ctrl = false,
            "Alt" => held.alt = false,
            _ => held.meta = false,
        }
        down.push(key(name, code, false, None, held));
    }
    Some(down)
}

impl Browser {
    /// Every element in tree order that matches `selector` (the native matcher of `querySelectorAll`; JS does not run).
    pub fn query_all(&mut self, selector: &str) -> crate::browser::R<Vec<NodeId>> {
        let dom = self.dom.clone();
        self.it.host_scope(|it| {
            let root = dom.doc.borrow().root();
            dom.query(it, root, selector, true)
        })
    }
}

impl Page {
    /// The size of the window in pixels.
    pub fn size(&self) -> (u32, u32) {
        self.viewport()
    }

    /// The number of the document of now. A handle of another number is detached.
    pub fn generation(&mut self) -> usize {
        self.doc_id()
    }

    /// The URL of the document, with its query and fragment. Give it to a person only through a redacting filter.
    pub fn url(&mut self) -> Option<String> {
        self.browser().and_then(|b| b.document_url())
    }

    /// The storage of the session (it lives through every document), for a new session of the same profile.
    pub fn storage(&self) -> Option<WebStorage> {
        match &self.source {
            Source::Session(s) => Some(s.storage()),
            Source::Browser(_) => None,
        }
    }

    /// Load `url` as the address bar does, or the current document again (`None`). It returns at once: the old document ends, the ticks
    /// load the next one, and the window shows the old pixels until then. `Err`: the page has no session.
    pub fn navigate(&mut self, url: Option<&str>) -> Result<(), String> {
        if !matches!(self.source, Source::Session(_)) {
            return Err("a page without a session cannot navigate".into());
        }
        // The old document ends in `Session::navigate`: its frame goes first, the pixels stay.
        self.release_frame();
        let Source::Session(s) = &mut self.source else { unreachable!("checked") };
        let stop = s.navigate(url).stop;
        self.touched();
        match stop {
            SessionStop::LoadFailed(why) => Err(format!("the document did not load: {why}")),
            SessionStop::Page(crate::Stop::Fatal(f)) => Err(format!("the page stopped: {f:?}")),
            _ => Ok(()),
        }
    }

    /// Go back (`-1`) or forward (`1`) in the history, as the mouse buttons of the window do. It returns at once, as `navigate` does.
    pub fn go(&mut self, delta: i32) -> Result<(), String> {
        if self.history_go(delta) { Ok(()) } else { Err("no history to go to".into()) }
    }

    /// The elements that match `selector` now.
    pub fn query_all(&mut self, selector: &str) -> Result<Vec<Handle>, String> {
        let doc = self.doc_id();
        if self.browser().is_none() {
            return Err(self.no_document());
        }
        let b = self.browser().expect("checked");
        match b.query_all(selector) {
            Ok(list) => Ok(list.into_iter().map(|node| Handle { doc, node }).collect()),
            Err(bare_js::eval::Error::Throw(_)) => Err(format!("the selector is not valid: {selector}")),
            Err(e) => Err(format!("the query failed: {e:?}")),
        }
    }

    /// Read the document and the frame of the window (made now if the page changed). `None`: no document, or the page cannot be laid
    /// out (the reason is the last of `errors`).
    pub fn inspect<R>(&mut self, f: impl FnOnce(&Document, &Frame) -> R) -> Option<R> {
        self.frame_now()?;
        let Page { source, frame, .. } = self;
        let doc = source.browser()?.document();
        let doc = doc.borrow();
        Some(f(&doc, frame.as_ref()?))
    }

    /// Is the handle from the document of now, and is its node connected?
    pub fn connected(&mut self, h: Handle) -> bool {
        if h.doc != self.doc_id() {
            return false;
        }
        let Some(b) = self.browser() else { return false };
        let doc = b.document();
        let doc = doc.borrow();
        let mut cur = Some(h.node);
        while let Some(c) = cur {
            if c == doc.root() {
                return true;
            }
            cur = doc.parent_or_host(c);
        }
        false
    }

    /// `tag#id.class` of the element, at most 80 characters.
    pub fn describe(&mut self, h: Handle) -> String {
        self.browser().map_or_else(String::new, |b| b.describe(h.node))
    }

    /// Is the element disabled: HTML `disabled` (also by a `fieldset`), or `aria-disabled="true"` on it or an ancestor?
    pub fn disabled(&mut self, h: Handle) -> bool {
        let Some(b) = self.browser() else { return false };
        let dom = b.dom.clone();
        if b.it.host_scope(|it| dom.is_disabled_control(it, h.node)).unwrap_or(false) {
            return true;
        }
        let doc = b.document();
        let doc = doc.borrow();
        let aria = doc.find_atom("aria-disabled");
        let mut cur = Some(h.node);
        while let Some(c) = cur {
            if aria.and_then(|a| doc.attr(c, a)).is_some_and(|v| v.eq_ignore_ascii_case("true")) {
                return true;
            }
            cur = doc.parent(c);
        }
        false
    }

    /// The value of an `<input>` or `<textarea>`.
    pub fn value(&mut self, h: Handle) -> Option<String> {
        self.browser().and_then(|b| b.input_value(h.node))
    }

    /// The labels of the selected options of a `<select>`, or `None` for another element.
    pub fn selected_labels(&mut self, h: Handle) -> Option<Vec<String>> {
        self.browser().and_then(|b| b.selected_labels(h.node))
    }

    pub fn checked(&mut self, h: Handle) -> bool {
        self.browser().is_some_and(|b| b.input_checked(h.node))
    }

    pub fn focused(&mut self, h: Handle) -> bool {
        h.doc == self.doc_id() && self.browser().and_then(|b| b.active_element().ok().flatten()) == Some(h.node)
    }

    /// Is it a password `<input>`? A driver redacts what it types there.
    pub fn is_password(&mut self, h: Handle) -> bool {
        let Some(b) = self.browser() else { return false };
        let doc = b.document();
        let doc = doc.borrow();
        doc.is_html(h.node, "input") && crate::browser_dom::input_kind(&doc, h.node) == "password"
    }

    /// The border box of the element in window pixels (what `getBoundingClientRect` gives on the frame of the window), when it is
    /// rendered: a box with a size, and not `visibility: hidden`. Else why it is hidden.
    pub fn visible_box(&mut self, h: Handle) -> Result<bare_layout::fragment::Rect, Refusal> {
        if !self.connected(h) {
            return Err(Refusal::Detached);
        }
        if self.frame_now().is_none() {
            return Err(Refusal::NoPage(self.errors.last().cloned().unwrap_or_else(|| "no layout".into())));
        }
        let (mut hidden, mut rect) = (false, None);
        self.scroll_with(|g, s| {
            hidden = matches!(g.styles.style(h.node).map(|st| st.get(Property::Visibility)), Some(Css::Keyword(Keyword::Hidden | Keyword::Collapse)));
            rect = g.client_rect(s, h.node);
            false
        });
        // A plain inline element has no box fragment, only text fragments: use its first line rectangle (as `getClientRects` does).
        if rect.is_none() && !hidden {
            rect = self.browser().and_then(|b| b.inline_rects(h.node).into_iter().find(|r| r.w > 0.0 && r.h > 0.0));
        }
        match rect {
            _ if hidden => Err(Refusal::Hidden("visibility: hidden")),
            Some(r) if r.w > 0.0 && r.h > 0.0 => Ok(r),
            Some(_) => Err(Refusal::Hidden("its box has no size")),
            None => Err(Refusal::Hidden("it has no box (display: none, or not rendered)")),
        }
    }

    /// The nodes among `nodes` whose box lies outside the window (a box with no layout is not listed). One pass, no scroll.
    pub fn offscreen(&mut self, nodes: &[NodeId]) -> std::collections::HashSet<NodeId> {
        let mut out = std::collections::HashSet::new();
        if nodes.is_empty() {
            return out;
        }
        self.scroll_with(|g, s| {
            let (w, h) = g.window;
            for &n in nodes {
                if let Some(r) = g.client_rect(s, n).filter(|r| r.w > 0.0 && r.h > 0.0)
                    && (r.x + r.w <= 0.0 || r.y + r.h <= 0.0 || r.x >= w || r.y >= h)
                {
                    out.insert(n);
                }
            }
            false
        });
        out
    }

    /// The point where a user clicks the element, after the checks of the header; scrolls it into view if needed.
    pub fn actionable(&mut self, h: Handle) -> Result<(f64, f64), Refusal> {
        self.visible_box(h)?;
        if self.disabled(h) {
            return Err(Refusal::Disabled);
        }
        // An inline element has no box: bring its nearest ancestor with a box into view.
        let moved = self.scroll_with(|g, s| {
            let mut n = h.node;
            while g.client_rect(s, n).is_none() && let Some(p) = g.doc.parent(n) {
                n = p;
            }
            g.reveal(s, n, Align::IfNeeded, Align::IfNeeded)
        });
        if moved {
            self.send_scroll_events();
            self.rehover();
        }
        let r = self.visible_box(h)?;
        let mut view = (0.0, 0.0);
        self.scroll_with(|g, _| {
            view = g.view();
            false
        });
        let (x0, y0, x1, y1) = (r.x.max(0.0), r.y.max(0.0), (r.x + r.w).min(view.0), (r.y + r.h).min(view.1));
        if x0 >= x1 || y0 >= y1 {
            return Err(Refusal::Hidden("no part of it is in the window (a clip or an overflow hides it)"));
        }
        let (x, y) = (((x0 + x1) / 2.0) as f64, ((y0 + y1) / 2.0) as f64);
        let top = self.hit(x, y);
        let doc = self.browser().map(|b| b.document()).ok_or(Refusal::Detached)?;
        let inside = {
            let doc = doc.borrow();
            let mut cur = top;
            while let Some(c) = cur.filter(|&c| c != h.node) {
                cur = doc.parent_or_host(c);
            }
            cur.is_some()
        };
        if !inside {
            let by = top.map_or_else(|| "nothing (pointer-events: none)".to_string(), |t| self.describe(Handle { doc: h.doc, node: t }));
            return Err(Refusal::Covered { by, top: top.map(|node| Handle { doc: h.doc, node }), at: (x.round() as i32, y.round() as i32) });
        }
        Ok((x, y))
    }

    /// Settle what a frame of the window settles: the hover of a still pointer after the boxes moved under it (Chrome's fake mouse move).
    /// The window does it before each frame; a driver with no window does it after each action.
    pub fn after_action(&mut self) {
        self.rehover();
    }

    /// A user click on the element: the pointer moves to the point of `actionable`, the left button goes down and up. Gives the point.
    pub fn click(&mut self, h: Handle) -> Result<(f64, f64), Refusal> {
        let (x, y) = self.actionable(h)?;
        // A driver click is one click: two driver clicks at one point are not a double click (as the CDP click of Playwright).
        if let Some(b) = self.browser() {
            b.dom.st.borrow_mut().ui.last = None;
        }
        let mods = Mods::default();
        self.handle(Event::PointerMove { x, y, buttons: 0, mods });
        self.handle(Event::PointerButton { x, y, button: 0, buttons: 1, pressed: true, mods });
        self.handle(Event::PointerButton { x, y, button: 0, buttons: 0, pressed: false, mods });
        self.after_action();
        Ok((x, y))
    }

    /// A double click: the pointer moves to the point of `actionable`, the left button goes down and up twice at once (`dblclick` fires).
    pub fn dblclick(&mut self, h: Handle) -> Result<(f64, f64), Refusal> {
        let (x, y) = self.actionable(h)?;
        if let Some(b) = self.browser() {
            b.dom.st.borrow_mut().ui.last = None;
        }
        let mods = Mods::default();
        self.handle(Event::PointerMove { x, y, buttons: 0, mods });
        for _ in 0..2 {
            self.handle(Event::PointerButton { x, y, button: 0, buttons: 1, pressed: true, mods });
            self.handle(Event::PointerButton { x, y, button: 0, buttons: 0, pressed: false, mods });
        }
        self.after_action();
        Ok((x, y))
    }

    /// Move the pointer over the element (the point of `actionable`): `:hover` rules apply and `pointerover`, `mouseover`, `pointerenter`
    /// and `mouseenter` fire. No button goes down.
    pub fn hover(&mut self, h: Handle) -> Result<(f64, f64), Refusal> {
        let (x, y) = self.actionable(h)?;
        self.handle(Event::PointerMove { x, y, buttons: 0, mods: Mods::default() });
        self.after_action();
        Ok((x, y))
    }

    /// Put a checkbox or a radio in the state `on`: a click only when the state differs, then a check of the state.
    pub fn set_checked(&mut self, h: Handle, on: bool) -> Result<(), Refusal> {
        let kind = self.browser().and_then(|b| {
            let doc = b.document();
            let doc = doc.borrow();
            doc.is_html(h.node, "input").then(|| crate::browser_dom::input_kind(&doc, h.node))
        });
        if !matches!(kind, Some("checkbox" | "radio")) {
            return Err(Refusal::Wrong("not_checkable", format!("{} is not a checkbox or a radio", self.describe(h))));
        }
        if self.checked(h) != on {
            self.click(h)?;
            if self.checked(h) != on {
                return Err(Refusal::Wrong("not_checked", format!("{} did not become {}checked after the click", self.describe(h), if on { "" } else { "un" })));
            }
        }
        Ok(())
    }

    /// Pick the options of a `<select>` by label, else value, and fire `input` and `change` (when the selection changed). The select
    /// must be rendered and enabled; no pointer moves, as a native list is not drawn by the page.
    pub fn select(&mut self, h: Handle, wanted: &[String]) -> Result<(), Refusal> {
        self.visible_box(h)?;
        if self.disabled(h) {
            return Err(Refusal::Disabled);
        }
        let is_select = self.browser().is_some_and(|b| b.document().borrow().is_html(h.node, "select"));
        if !is_select {
            return Err(Refusal::Wrong("not_select", format!("{} is not a select", self.describe(h))));
        }
        let r = self.browser().map(|b| b.drive_select(h.node, wanted));
        let Some(r) = r else { return Err(Refusal::NoPage("no document".into())) };
        let r = match r {
            Ok(Ok(out)) => Ok(out),
            Ok(Err(why)) => return Err(Refusal::Wrong("no_option", why)),
            Err(e) => Err(e),
        };
        self.after(r);
        self.after_action();
        Ok(())
    }

    /// Set the files of an `<input type=file>` from local paths and fire `input` and `change`. The input may be hidden (a page often hides
    /// it behind a button). A file over 100 MiB, or one that is not a regular file, is refused. The bytes are read when the page or a
    /// submit asks for them.
    pub fn upload(&mut self, h: Handle, paths: &[String]) -> Result<(), Refusal> {
        if !self.connected(h) {
            return Err(Refusal::Detached);
        }
        if self.disabled(h) {
            return Err(Refusal::Disabled);
        }
        let (kind, multiple) = self.browser().map_or((None, false), |b| {
            let doc = b.document();
            let doc = doc.borrow();
            let k = doc.is_html(h.node, "input").then(|| crate::browser_dom::input_kind(&doc, h.node));
            (k, doc.find_atom("multiple").is_some_and(|a| doc.attr(h.node, a).is_some()))
        });
        if kind != Some("file") {
            return Err(Refusal::Wrong("not_file_input", format!("{} is not a file input", self.describe(h))));
        }
        if paths.is_empty() || (paths.len() > 1 && !multiple) {
            return Err(Refusal::Wrong("one_file", format!("{} takes {}", self.describe(h), if paths.is_empty() { "at least one file" } else { "one file (it has no multiple attribute)" })));
        }
        let mut files = Vec::new();
        for p in paths {
            let bad = |why: String| Refusal::Wrong("bad_file", format!("{p}: {why}"));
            let meta = std::fs::metadata(p).map_err(|e| bad(e.to_string()))?;
            if !meta.is_file() {
                return Err(bad("not a regular file".into()));
            }
            if meta.len() > crate::browser_web::MAX_UPLOAD {
                return Err(bad(format!("{} bytes is over the {} MiB limit", meta.len(), crate::browser_web::MAX_UPLOAD >> 20)));
            }
            let path = std::path::PathBuf::from(p);
            let name = path.file_name().map_or_else(|| p.clone(), |n| n.to_string_lossy().into_owned());
            let modified_ms = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0.0, |d| d.as_millis() as f64);
            files.push(crate::browser::FileRef { path, name, modified_ms });
        }
        let r = self.browser().map(|b| b.drive_files(h.node, files));
        let Some(r) = r else { return Err(Refusal::NoPage("no document".into())) };
        self.after(r);
        self.after_action();
        Ok(())
    }

    /// Type `text` into a field as a user does: click it, select its text with Control+A (so the new text replaces it), then press one key
    /// for each character (a line feed is Enter). Empty text clears the field with Backspace.
    pub fn type_text(&mut self, h: Handle, text: &str) -> Result<(), Refusal> {
        self.type_start(h, text.is_empty())?;
        for c in text.chars() {
            self.type_char(c);
        }
        self.after_action();
        Ok(())
    }

    /// The start of `type_text`: the checks, the click and the select all. `clear` also deletes the old text. A driver then sends one
    /// `type_char` in each tick, as a person types.
    pub fn type_start(&mut self, h: Handle, clear: bool) -> Result<(), Refusal> {
        self.visible_box(h)?;
        if self.disabled(h) {
            return Err(Refusal::Disabled);
        }
        if !self.browser().is_some_and(|b| b.is_text_input(h.node)) {
            return Err(Refusal::NotEditable);
        }
        self.click(h)?;
        if !self.focused(h) {
            return Err(Refusal::NoFocus);
        }
        if self.value(h).is_some_and(|v| !v.is_empty()) {
            self.press("Control+a").expect("a known key");
            if clear {
                self.press("Backspace").expect("a known key");
            }
        }
        Ok(())
    }

    /// Give the element the focus, as `el.focus()` does, so that a key goes to it. Fails when it is hidden or disabled, or when it takes no focus.
    pub fn focus(&mut self, h: Handle) -> Result<(), Refusal> {
        self.visible_box(h)?;
        if self.disabled(h) {
            return Err(Refusal::Disabled);
        }
        if !self.focused(h) {
            if let Some(b) = self.browser() {
                let _ = b.focus(h.node);
            }
            self.after_action();
        }
        if self.focused(h) { Ok(()) } else { Err(Refusal::NoFocus) }
    }

    /// The focused element, if any.
    pub fn active(&mut self) -> Option<Handle> {
        let doc = self.doc_id();
        self.browser().and_then(|b| b.active_element().ok().flatten()).map(|node| Handle { doc, node })
    }

    /// The count of the changes of the document (its `revision`): a driver compares two to tell that a key did something.
    pub fn revision(&mut self) -> u64 {
        self.browser().map_or(0, |b| b.document().borrow().revision())
    }

    /// The key events of one character at the focused element (a line feed is Enter).
    pub fn type_char(&mut self, c: char) {
        let mut buf = [0u8; 4];
        let spec = if c == '\n' { "Enter" } else { c.encode_utf8(&mut buf) };
        for e in key_events(spec).expect("one character is a key") {
            self.handle(e);
        }
    }

    /// Press a key at the focused element (see `key_events` for the spec). `Err` for a name that is not a key.
    pub fn press(&mut self, spec: &str) -> Result<(), String> {
        let events = key_events(spec).ok_or_else(|| format!("not a key: {spec}"))?;
        for e in events {
            self.handle(e);
        }
        self.after_action();
        Ok(())
    }

    /// Turn the wheel over the element (the center of its part in the window) or over the middle of the window: the pointer moves there
    /// first. `dx` and `dy` are CSS pixels, positive to the right and down. Gives whether something scrolled.
    pub fn wheel(&mut self, over: Option<Handle>, dx: f64, dy: f64) -> Result<bool, Refusal> {
        let (w, h) = self.size();
        let (x, y) = match over {
            Some(t) => {
                let r = self.visible_box(t)?;
                let (x0, y0, x1, y1) = (r.x.max(0.0), r.y.max(0.0), (r.x + r.w).min(w as f32), (r.y + r.h).min(h as f32));
                if x0 >= x1 || y0 >= y1 {
                    return Err(Refusal::Hidden("no part of it is in the window"));
                }
                (((x0 + x1) / 2.0) as f64, ((y0 + y1) / 2.0) as f64)
            }
            None => (w as f64 / 2.0, h as f64 / 2.0),
        };
        let mods = Mods::default();
        self.handle(Event::PointerMove { x, y, buttons: 0, mods });
        let moved = self.handle(Event::Wheel { x, y, dx, dy, mods }).redraw;
        self.after_action();
        Ok(moved)
    }

    /// The pixels of the frame that the window shows now.
    pub fn capture(&mut self) -> Canvas<'static> {
        let (w, h) = self.size();
        let pixels = self.paint().to_vec();
        if self.windowed {
            // A window page draws into the buffers of the window: it keeps no canvas of its own after the capture.
            self.canvas = Canvas::new(0, 0);
        }
        Canvas::from_vec(w, h, pixels)
    }

    /// A picture as wide as the window and `height` pixels tall, painted from layout without a scroll: from the top of the page
    /// (`top`), or from where the window is scrolled to (then a window box lies at the same place in the picture). `None`: no frame.
    pub fn capture_tall(&mut self, height: u32, top: bool) -> Option<Canvas<'static>> {
        self.frame_now()?;
        let mut canvas = Canvas::new(self.size().0, height.max(1));
        self.paint_at(&mut canvas, top).then_some(canvas)
    }

    /// The height of the whole page in pixels (at least the window), and how far it is scrolled down.
    pub fn page_height(&mut self) -> (u32, f32) {
        let win = self.size().1;
        let y = self.browser().map_or(0.0, |b| b.scroll().y);
        let page = self.frame_now().map_or(0.0, |f| f.areas.page.1);
        (win.max(page.ceil() as u32), y)
    }

    /// The console calls, listener errors, page errors and failed requests since the last call. The engine prints none of them.
    pub fn take_logs(&mut self) -> Vec<Log> {
        let mut out: Vec<Log> = self.errors.drain(..).map(|text| Log { level: if text.starts_with("listener") { "exception" } else { "page" }, text }).collect();
        if let Some(b) = self.browser() {
            for r in b.take_console_records() {
                let level = match r.level {
                    crate::browser_console::ConsoleLevel::Log => "log",
                    crate::browser_console::ConsoleLevel::Warn => "warn",
                    crate::browser_console::ConsoleLevel::Error => "error",
                };
                let text = b.format_console_record(&r).unwrap_or_else(|e| format!("(not formatted: {e:?})"));
                out.push(Log { level, text });
            }
            for text in b.take_fetch_diagnostics() {
                out.push(Log { level: "fetch", text });
            }
            for r in b.take_reports() {
                out.push(Log { level: "exception", text: format!("listener of {}: {}", r.event_type, r.message) });
            }
        }
        out
    }
}

type Job = Box<dyn FnOnce(&mut Page) + Send>;
/// The way to send jobs to the window thread: the queue and the waker of its loop.
type Jobs = (mpsc::Sender<Job>, bare_window::Waker);

/// A page in a real Wayland window, on its own thread (with the big stack of the VM). A driver sends it jobs; the window loop runs each
/// one in `App::tick`, between input events, and draws a frame after it. Dropping the `Remote` closes the window.
pub struct Remote {
    tx: Option<mpsc::Sender<Job>>,
    waker: bare_window::Waker,
    thread: Option<std::thread::JoinHandle<()>>,
    ran: Ran,
}

/// How many rounds of the window loop ran a task of the page (`Page::tick`), and a signal for each.
type Ran = std::sync::Arc<(std::sync::Mutex<u64>, std::sync::Condvar)>;

/// The long rounds of a loop: how many, the longest, and how many took more than 16 ms (a missed frame at 60 Hz) and 50 ms (a long task).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Ticks {
    pub n: u32,
    pub max: std::time::Duration,
    pub over_16: u32,
    pub over_50: u32,
}

impl Ticks {
    pub fn add(&mut self, d: std::time::Duration) {
        self.n += 1;
        self.max = self.max.max(d);
        self.over_16 += (d.as_micros() > 16_000) as u32;
        self.over_50 += (d.as_micros() > 50_000) as u32;
    }

    pub fn merge(&mut self, o: Ticks) {
        self.n += o.n;
        self.max = self.max.max(o.max);
        self.over_16 += o.over_16;
        self.over_50 += o.over_50;
    }
}

struct Hosted {
    page: Page,
    rx: mpsc::Receiver<Job>,
    /// Told once, when the first frame is drawn.
    ready: Option<mpsc::Sender<Result<Jobs, String>>>,
    jobs: Option<Jobs>,
    ran: Ran,
}

impl App for Hosted {
    fn resize(&mut self, w: u32, h: u32) {
        self.page.resize(w, h);
    }
    fn frame(&mut self, ctl: &mut Control, out: &mut [u32]) -> bool {
        if let (Some(r), Some(j)) = (self.ready.take(), self.jobs.take()) {
            let _ = r.send(Ok(j));
        }
        self.page.frame(ctl, out)
    }
    fn event(&mut self, event: Event, ctl: &mut Control) {
        self.page.event(event, ctl);
    }
    fn tick(&mut self, ctl: &mut Control) -> Option<Instant> {
        // One job in a round, so that each job, with its draw, is one tick (`round`). A job ran: the next round comes at once, for the
        // next job.
        let mut ran = false;
        if self.ready.is_none() {
            match self.rx.try_recv() {
                Ok(job) => {
                    job(&mut self.page);
                    ctl.redraw();
                    ran = true;
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    ctl.close();
                    return None;
                }
            }
        }
        let (work, next) = self.page.tick();
        if work {
            ctl.redraw();
            *self.ran.0.lock().unwrap() += 1;
            self.ran.1.notify_all();
        }
        if ran { Some(Instant::now()) } else { next }
    }
    /// Each round of the window loop is one tick of `page.ticks`; a job takes them with `std::mem::take`.
    fn round(&mut self, busy: std::time::Duration) {
        self.page.ticks.add(busy);
    }
}

impl Remote {
    /// Load `url` in a new session with empty storage and show it in a window of `size`. The session shares the connections and the HTTP
    /// cache of `net` (`Page::open_in`): a job that opens another page with `Page::open_in(url, page.viewport(), &net)` on the window
    /// thread keeps them. Returns when the first frame is drawn. The storage is not `Send`: a job opens a page of the same profile with
    /// `Page::open_with(url, size, page.storage())` on the window thread.
    pub fn open(url: &str, size: (u32, u32), net: &bare_net::http::Client) -> Result<Remote, String> {
        Remote::open_with(url, size, net, None, None)
    }

    /// `open` that starts from a saved state (`None`: empty storage).
    pub fn open_with(url: &str, size: (u32, u32), net: &bare_net::http::Client, state: Option<crate::page::StorageState>, downloads: Option<crate::Downloads>) -> Result<Remote, String> {
        let (ready_tx, ready_rx) = mpsc::channel();
        let (url, net) = (url.to_string(), net.fork());
        let ran = Ran::default();
        let ran2 = ran.clone();
        let stack = if cfg!(debug_assertions) { 64 << 20 } else { 8 << 20 };
        let thread = std::thread::Builder::new()
            .stack_size(stack)
            .spawn(move || {
                let fail = |e: String, r: &mpsc::Sender<Result<_, String>>| drop(r.send(Err(e)));
                let t = Instant::now();
                let mut page = match Page::open_in_with(&url, size, &net, state.as_ref(), downloads) {
                    Ok(p) => p,
                    Err(e) => return fail(e, &ready_tx),
                };
                // The load runs on the window thread before the loop: the first tick.
                page.ticks.add(t.elapsed());
                page.quiet = true;
                let config = bare_window::Config { title: "bare (driven)".into(), app_id: "bare".into(), size };
                let window = match bare_window::Window::open(&config) {
                    Ok(w) => w,
                    Err(e) => return fail(format!("window: {e}"), &ready_tx),
                };
                let (tx, rx) = mpsc::channel();
                let waker = window.waker();
                page.wake_on_done(move || waker.wake());
                let mut host = Hosted { page, rx, ready: Some(ready_tx.clone()), jobs: Some((tx, window.waker())), ran: ran2 };
                if let Err(e) = window.run(&mut host) {
                    fail(format!("window: {e}"), &ready_tx);
                }
            })
            .map_err(|e| format!("thread: {e}"))?;
        match ready_rx.recv() {
            Ok(Ok((tx, waker))) => Ok(Remote { tx: Some(tx), waker, thread: Some(thread), ran }),
            Ok(Err(e)) => Err(e),
            Err(_) => Err("the window thread ended".into()),
        }
    }

    /// How many rounds of the window loop have run a task of the page.
    pub fn work_count(&self) -> u64 {
        *self.ran.0.lock().unwrap()
    }

    /// Sleep until `until`, or until the window loop runs a task of the page after `seen` (a value of `work_count`). The loop runs a
    /// reply as soon as it is in (`Page::wake_on_done`), so a driver that waits here sees it at once: `Page::tick` then asks for a
    /// call only each second.
    pub fn wait_work(&self, seen: u64, until: Instant) {
        let mut n = self.ran.0.lock().unwrap();
        while *n == seen {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return;
            }
            n = self.ran.1.wait_timeout(n, left).unwrap().0;
        }
    }

    /// Run `f` on the page, in the window loop, and give its result.
    pub fn call<R: Send + 'static>(&self, f: impl FnOnce(&mut Page) -> R + Send + 'static) -> Result<R, String> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.tx.as_ref().ok_or("the window closed")?.send(Box::new(move |p| drop(tx.send(f(p))))).map_err(|_| "the window closed".to_string())?;
        self.waker.wake();
        rx.recv().map_err(|_| "the window closed".to_string())
    }
}

impl Drop for Remote {
    fn drop(&mut self) {
        self.tx = None;
        self.waker.wake();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
