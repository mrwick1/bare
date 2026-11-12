//! The driver inputs of `Session::resume` that act like a user: a click, a key and typed text. They run the window input of
//! `browser_ui.rs`, so a page sees the events, the focus changes and the default actions that Chrome makes (the order was recorded from
//! headless Chrome 155: `tools/ui/chrome_drive.mjs`, `tools/fixtures/ui/drive-steps.txt`).
//!
//! - `drive_click(node)`: the page scrolls so that `node` shows (only if needed: Chrome's `DOM.scrollIntoViewIfNeeded`). The pointer moves to the
//!   center of the part of the border box of `node` that is in the window ((0, 0) when the element has no box or the page cannot be laid out yet:
//!   then the events go to `node`). The events go to the top element at that point (`elementsFromPoint`: `z-index`, overlays, `pointer-events`,
//!   `visibility`), as for a user, and not to `node`. When that element is not `node` or a child of it, `UiOutcome::intercepted` says what it
//!   was (`Input::Click` puts it in `PageResult::intercepted`). A hit inside a shadow tree goes to its outermost host (events do not cross a
//!   shadow root here). The pointer then stays there, so the next click sends `pointerout` and `mouseout` to that element. A `pointerdown` or
//!   `mousedown` that a listener cancels has the effects of Chrome (no focus change, no mouse events), and a disabled control gets only the
//!   pointer events. The `scroll` events of the reveal come with the next scheduler turn.
//!   In a text field the press puts the caret where the layout says (`caret_at`; a fresh field has its caret at 0, so a click without it would
//!   type at the start).
//! - `drive_text_down(node, at, nth)`, `drive_text_move(node, at)`, `drive_text_up(node, at)`: the pointer at the boundary `at` of the text of a
//!   field. A press (`nth` 1, 2, 3: a click, a double click, a triple click), a move with the button down and a release. They give a click, a
//!   word or line selection and a drag selection (`tools/fixtures/ui/sel-steps.txt`). The caller moves the clock between them.
//! - `drive_key(node, spec)`: the element is focused first (when it is not), then the key goes down and up. `spec` is `Enter`, `Tab`, `Space`,
//!   `a`, `A`, an arrow key and so on, with modifiers in front (`Shift+Tab`, `Control+a`). A modifier key makes its own `keydown` first and
//!   its `keyup` last. The default actions are the ones of `browser_ui.rs`.
//! - `drive_type(node, text)`: focus the field, select its text (so the new text replaces it, as `Input::Type` always did), then press each
//!   character as a key: `keydown`, `keypress`, `beforeinput`, the change of the value, `input`, `keyup`. A line feed is the Enter key.
//!   Empty text clears a field with Backspace. Works for a text `<input>` and for a `<textarea>`. A `contenteditable` element is edited through its key path (caret to the end, old content selected, deleted for an empty text).
//!
//! Gaps: the hover settle after a click only; no modifier keys on a click; the left button only.

use crate::browser::*;
use crate::browser_scroll::Align;
use crate::browser_ui::{KeyInput, Mods, Pointer, UiOutcome};
use bare_html::dom::NodeId;
use bare_js::eval::Error;
use bare_js::value::Value;

impl UiOutcome {
    fn add(&mut self, o: UiOutcome) {
        self.reports.extend(o.reports);
        self.changed |= o.changed;
        self.intercepted = self.intercepted.take().or(o.intercepted);
    }
}

fn unsupported_input(what: &'static str) -> Error {
    Error::Unsupported(unsupported(what))
}

/// The key a character makes: `a` is `KeyA`, `5` is `Digit5`, a line feed is Enter. The code of another character is empty.
fn char_key(c: char) -> KeyInput {
    match c {
        '\n' => named("Enter"),
        ' ' => named("Space"),
        c => {
            let code = match c {
                'a'..='z' | 'A'..='Z' => format!("Key{}", c.to_ascii_uppercase()),
                '0'..='9' => format!("Digit{c}"),
                _ => String::new(),
            };
            KeyInput { key: c.to_string(), code, text: Some(c.to_string()), ..KeyInput::default() }
        }
    }
}

/// A key by its DOM name. `Space` is the key `" "`.
fn named(name: &str) -> KeyInput {
    let (key, code, text) = match name {
        "Space" => (" ", "Space", Some(" ")),
        "Enter" => ("Enter", "Enter", Some("\r")),
        "Esc" => ("Escape", "Escape", None),
        k => (k, k, None),
    };
    KeyInput { key: key.into(), code: code.into(), text: text.map(Into::into), ..KeyInput::default() }
}

/// The key of a spec, with its modifier keys in front (`Shift+Tab`). `None` for a name that is not a key.
fn parse(spec: &str) -> Option<(Vec<KeyInput>, KeyInput)> {
    let (mods, last) = match spec.rfind('+') {
        Some(i) if i + 1 < spec.len() => (&spec[..i], &spec[i + 1..]),
        _ => ("", spec),
    };
    let mut held = Mods::default();
    let mut keys = Vec::new();
    for m in mods.split('+').filter(|m| !m.is_empty()) {
        let (key, flag) = match m {
            "Shift" => ("Shift", &mut held.shift),
            "Control" | "Ctrl" => ("Control", &mut held.ctrl),
            "Alt" => ("Alt", &mut held.alt),
            "Meta" => ("Meta", &mut held.meta),
            _ => return None,
        };
        *flag = true;
        keys.push(KeyInput { key: key.into(), code: format!("{key}Left"), mods: held, ..KeyInput::default() });
    }
    let mut main = match last {
        "Space" | "Enter" | "Esc" | "Escape" | "Tab" | "Backspace" | "Delete" | "Home" | "End" | "PageUp" | "PageDown" | "ArrowUp" | "ArrowDown" | "ArrowLeft" | "ArrowRight" => named(last),
        _ if last.chars().count() == 1 => char_key(last.chars().next()?),
        _ => return None,
    };
    main.mods = held;
    // A key with Control or Alt makes no text.
    if held.ctrl || held.alt || held.meta {
        main.text = None;
    }
    Some((keys, main))
}

impl Browser {
    /// The line rectangles of an element that has no box fragment (a plain inline), in window pixels. Empty for `display: none`.
    pub fn inline_rects(&mut self, node: NodeId) -> Vec<bare_layout::fragment::Rect> {
        let dom = self.dom.clone();
        self.it.host_scope(|it| crate::browser_rects::element_rects(&dom, it, node)).unwrap_or_default()
    }

    /// Where a user clicks on `node`, and the element that is on top there. The page scrolls so that `node` shows (if needed, as Chrome's
    /// `DOM.scrollIntoViewIfNeeded`), and the point is the center of the part of the border box that is in the window. `(0, 0)` and no
    /// element when `node` has no box, or when the page cannot be laid out yet (a style sheet that was not supplied): the click still
    /// happens, at `node`, as it did before layout was used. A hit inside a shadow tree is its outermost host: the events do not cross a shadow root here.
    fn click_point(&mut self, node: NodeId) -> R<((f64, f64), Option<NodeId>)> {
        let dom = self.dom.clone();
        let r = self.it.host_scope(|it| {
            if !dom.in_document(it, node)? {
                return Ok(None);
            }
            dom.with_geo(it, |g, s| {
                g.reveal(s, node, Align::IfNeeded, Align::IfNeeded);
                let r = g.client_rect(s, node)?;
                let (w, h) = g.view();
                let (x0, y0, x1, y1) = (r.x.max(0.0), r.y.max(0.0), (r.x + r.w).min(w), (r.y + r.h).min(h));
                let (x, y) = if x0 < x1 && y0 < y1 { ((x0 + x1) / 2.0, (y0 + y1) / 2.0) } else { (r.x + r.w / 2.0, r.y + r.h / 2.0) };
                let top = bare_paint::hit::hits_scrolled(g.doc, g.styles, g.layout, g.areas, s, x, y).first().map(|&n| crate::browser_point::retarget(g.doc, n, &[g.doc.root()]));
                Some(((x as f64, y as f64), top))
            })
        });
        match r {
            Ok(Some(p)) => Ok(p),
            Ok(_) | Err(Error::Unsupported(_)) => Ok(((0.0, 0.0), None)),
            Err(e) => Err(e),
        }
    }

    /// The caret offset of a press at (`x`, `y`) in a text field, from the layout. `None` for another node or when the page cannot be laid out.
    fn caret_at(&mut self, node: NodeId, x: f64, y: f64) -> Option<u32> {
        if !self.is_text_input(node) {
            return None;
        }
        let frame = self.render().ok()?;
        crate::page::caret_in(self, &frame, node, x, y)
    }

    /// The window point of the boundary `at` of the text of a field, or its center when it has no layout.
    fn text_point(&mut self, node: NodeId, at: u32) -> R<(f64, f64)> {
        if let Ok(frame) = self.render()
            && let Some(p) = crate::page::point_in(self, &frame, node, at)
        {
            return Ok(p);
        }
        Ok(self.click_point(node)?.0)
    }

    /// The button goes down in text field `node` at the boundary `at` of its text. `nth` is the number of the press in a row
    /// (1: a click, 2: a double click, 3: a triple click). The caller moves the clock between the presses.
    pub fn drive_text_down(&mut self, node: NodeId, at: u32, nth: u32) -> R<UiOutcome> {
        let (x, y) = self.text_point(node, at)?;
        if nth <= 1 {
            self.dom.st.borrow_mut().ui.last = None;
        }
        let up = Pointer { x, y, ..Pointer::default() };
        let caret = self.caret_at(node, x, y);
        let mut out = self.ui_pointer_move(Some(node), up)?;
        out.add(self.ui_pointer_button(Some(node), Pointer { buttons: 1, ..up }, 0, true, caret)?);
        Ok(out)
    }

    /// The pointer moves to the boundary `at` of the text of `node`, with the button down: the selection of the press follows.
    pub fn drive_text_move(&mut self, node: NodeId, at: u32) -> R<UiOutcome> {
        let (x, y) = self.text_point(node, at)?;
        let p = Pointer { x, y, buttons: 1, ..Pointer::default() };
        let mut out = self.ui_pointer_move(Some(node), p)?;
        if let Some(c) = self.caret_at(node, x, y) {
            out.add(self.ui_drag_select(c)?);
        }
        Ok(out)
    }

    /// The button goes up in text field `node` at the boundary `at` of its text.
    pub fn drive_text_up(&mut self, node: NodeId, at: u32) -> R<UiOutcome> {
        let (x, y) = self.text_point(node, at)?;
        let up = Pointer { x, y, ..Pointer::default() };
        let mut out = self.ui_pointer_move(Some(node), up)?;
        out.add(self.ui_pointer_button(Some(node), up, 0, false, None)?);
        Ok(out)
    }

    /// `tag#id.class.class` of an element, at most 80 characters.
    pub(crate) fn describe(&self, n: NodeId) -> String {
        describe_in(&self.document().borrow(), n)
    }

    /// Text of a script value for tools (`Interp::inspect`): a node is `<tag#id.class>`, an object is compact JSON of at most `cap` characters.
    pub fn inspect(&mut self, v: Value, cap: usize) -> String {
        // `Rc::clone` copies the pointer, not the DOM, so the closure can read the DOM while `self.it` walks the value.
        let dom = self.dom.clone();
        let node = |o| dom.node_of(Value::Object(o)).map(|n| format!("<{}>", describe_in(&dom.doc.borrow(), n)));
        self.it.inspect(v, cap, &node)
    }

    /// A user click on `node`. See the header.
    pub fn drive_click(&mut self, node: NodeId) -> R<UiOutcome> {
        let ((x, y), top) = self.click_point(node)?;
        let hit = top.unwrap_or(node);
        let doc = self.document();
        let (intercepted, mut cur) = (top.is_some(), Some(hit));
        let covered = intercepted && {
            let doc = doc.borrow();
            while let Some(c) = cur.filter(|&c| c != node) {
                cur = doc.parent(c);
            }
            cur.is_none()
        };
        let mut out = UiOutcome::default();
        if covered {
            out.intercepted = Some((self.describe(hit), (x.round() as i32, y.round() as i32)));
        }
        // A driver click is one click: two in a row at one element are not a double click.
        self.dom.st.borrow_mut().ui.last = None;
        let up = Pointer { x, y, ..Pointer::default() };
        let down = Pointer { buttons: 1, ..up };
        let caret = self.caret_at(hit, x, y);
        out.add(self.ui_pointer_move(Some(hit), up)?);
        out.add(self.ui_pointer_button(Some(hit), down, 0, true, caret)?);
        out.add(self.ui_pointer_button(Some(hit), up, 0, false, None)?);
        out.add(self.settle_hover(up)?);
        Ok(out)
    }

    /// After a change of the page, Chrome tells the element under the pointer (a "fake mouse move": the boundary events, no move). Skipped
    /// when the page cannot be laid out, or when the pointer has no place yet.
    fn settle_hover(&mut self, p: Pointer) -> R<UiOutcome> {
        if self.dom.st.borrow().ui.last_pos.is_none() {
            return Ok(UiOutcome::default());
        }
        let frame = match self.render() {
            Ok(f) => f,
            Err(Error::Unsupported(_)) => return Ok(UiOutcome::default()),
            Err(e) => return Err(e),
        };
        let to = {
            let doc = self.document();
            let doc = doc.borrow();
            bare_paint::hit::hit_test_scrolled(&doc, &frame.styles, &frame.layout, &frame.areas, &self.scroll(), p.x as f32, p.y as f32)
        };
        self.ui_call(false, |d, it| d.ui_settle(it, to, &p))
    }

    /// Focus `node` the way `el.focus()` does, unless it already has focus.
    fn drive_focus(&mut self, node: NodeId, out: &mut UiOutcome) -> R<()> {
        if self.active_element()? != Some(node) {
            // The focus events of the window are trusted, as the ones of `el.focus()` are in Chrome.
            self.dom.st.borrow_mut().ui.active = true;
            let f = self.focus(node);
            self.dom.st.borrow_mut().ui.active = false;
            out.reports.extend(f?.reports);
            out.changed = true;
        }
        Ok(())
    }

    /// A key, down and up, at the focused element (`node` is focused first). See the header.
    pub fn drive_key(&mut self, node: Option<NodeId>, spec: &str) -> R<UiOutcome> {
        let (mods, key) = parse(spec).ok_or_else(|| unsupported_input("test driver: unknown key name"))?;
        let mut out = UiOutcome::default();
        if let Some(n) = node {
            self.drive_focus(n, &mut out)?;
        }
        for k in &mods {
            out.add(self.ui_key(k.clone(), true)?);
        }
        out.add(self.ui_key(key.clone(), true)?);
        out.add(self.ui_key(key, false)?);
        for k in mods.iter().rev() {
            let mut up = k.clone();
            // The modifier is up now: the keys that are still held are the ones before it.
            up.mods = Mods { shift: up.mods.shift && k.key != "Shift", ctrl: up.mods.ctrl && k.key != "Control", alt: up.mods.alt && k.key != "Alt", meta: up.mods.meta && k.key != "Meta" };
            out.add(self.ui_key(up, false)?);
        }
        Ok(out)
    }

    /// A pick in a `<select>`: the options that match `wanted` (label, else value) are selected, then `input` and `change` fire, as for a
    /// user pick. They fire only when the selection changed. `Err(why)` when no option matches.
    pub fn drive_select(&mut self, node: NodeId, wanted: &[String]) -> R<Result<UiOutcome, String>> {
        let mut bad = None;
        let out = self.ui_call(true, |d, it| {
            match d.sel_pick(it, node, wanted)? {
                Ok(true) => {
                    d.fire_plain(it, node, "input")?;
                    d.fire_plain(it, node, "change")?;
                }
                Ok(false) => {}
                Err(e) => bad = Some(e),
            }
            Ok(())
        })?;
        Ok(bad.map_or(Ok(out), Err))
    }

    /// The files of an `<input type=file>`: they are set, then `input` and `change` fire, as for a pick in the file dialog.
    pub(crate) fn drive_files(&mut self, node: NodeId, files: Vec<FileRef>) -> R<UiOutcome> {
        self.dom.st.borrow_mut().inputs.entry(node).or_default().files = files;
        self.ui_call(true, |d, it| {
            d.fire_plain(it, node, "input")?;
            d.fire_plain(it, node, "change")?;
            Ok(())
        })
    }

    /// Typed text in a text `<input>` or a `<textarea>`. See the header.
    pub fn drive_type(&mut self, node: NodeId, text: &str) -> R<UiOutcome> {
        let host = self.is_editing_host(node);
        if !host && !self.is_text_input(node) {
            return Err(unsupported_input("test driver: type needs an editable <input>, <textarea> or contenteditable element"));
        }
        let mut out = UiOutcome::default();
        self.drive_focus(node, &mut out)?;
        if self.active_element()? != Some(node) {
            return Err(unsupported_input("test driver: the field cannot take focus"));
        }
        if host {
            // The caret goes to the end; old content is selected, and deleted for an empty text. A line feed is Enter.
            self.driven(|d, it| d.caret_into_host(it, node))?;
            if self.has_children(node) {
                out.add(self.drive_key(None, "Control+a")?);
                if text.is_empty() {
                    out.add(self.drive_key(None, "Backspace")?);
                }
            }
            for c in text.chars() {
                let k = char_key(c);
                out.add(self.ui_key(k.clone(), true)?);
                out.add(self.ui_key(k, false)?);
            }
            return Ok(out);
        }
        let had_text = self.input_value(node).is_some_and(|v| !v.is_empty());
        if had_text {
            self.driven(|d, it| d.select_text(it, node, None))?;
        }
        if text.is_empty() && had_text {
            return self.drive_key(None, "Backspace").map(|o| {
                out.add(o);
                out
            });
        }
        for c in text.chars() {
            let k = char_key(c);
            out.add(self.ui_key(k.clone(), true)?);
            out.add(self.ui_key(k, false)?);
        }
        Ok(out)
    }
}

/// `tag#id.class.class` of an element of `doc`, at most 80 characters.
fn describe_in(doc: &bare_html::dom::Document, n: NodeId) -> String {
    let mut out = doc.element_name(n).to_ascii_lowercase();
    if let Some(id) = doc.attr(n, bare_html::dom::Atom::ID).filter(|i| !i.is_empty()) {
        out.push('#');
        out.push_str(id);
    }
    for c in doc.find_atom("class").and_then(|a| doc.attr(n, a)).unwrap_or("").split_ascii_whitespace() {
        out.push('.');
        out.push_str(c);
    }
    out.chars().take(80).collect()
}
