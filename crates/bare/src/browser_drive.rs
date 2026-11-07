//! The driver inputs of `Session::resume` that act like a user: a click, a key and typed text. They run the window input of
//! `browser_ui.rs`, so a page sees the events, the focus changes and the default actions that Chrome makes (the order was recorded from
//! headless Chrome 155: `tools/ui/chrome_drive.mjs`, `tools/fixtures/ui/drive-steps.txt`).
//!
//! - `drive_click(node)`: the pointer moves to the center of the border box of `node` (`getBoundingClientRect`; (0, 0) when the element has no
//!   box or the page cannot be laid out yet), presses the left button and releases it. The events go to `node` itself, not to what a hit test
//!   finds at that point: a driver means this element. The pointer then stays there, so the next click sends `pointerout` and `mouseout` to this
//!   element. A `pointerdown` or `mousedown` that a listener cancels has the effects of Chrome (no focus change, no mouse events), and a disabled
//!   control gets only the pointer events.
//! - `drive_key(node, spec)`: the element is focused first (when it is not), then the key goes down and up. `spec` is `Enter`, `Tab`, `Space`,
//!   `a`, `A`, an arrow key and so on, with modifiers in front (`Shift+Tab`, `Control+a`). A modifier key makes its own `keydown` first and
//!   its `keyup` last. The default actions are the ones of `browser_ui.rs`.
//! - `drive_type(node, text)`: focus the field, select its text (so the new text replaces it, as `Input::Type` always did), then press each
//!   character as a key: `keydown`, `keypress`, `beforeinput`, the change of the value, `input`, `keyup`. A line feed is the Enter key.
//!   Empty text clears a field with Backspace. Works for a text `<input>` and for a `<textarea>`. A `contenteditable` element is not built.
//!
//! Gaps: no hover settle (Chrome sends `pointerover` to the element under the pointer after a layout change, here the next move does);
//! no scroll into view; no modifier keys on a click; the left button only.

use crate::browser::*;
use crate::browser_ui::{KeyInput, Mods, Pointer, UiOutcome};
use bare_html::dom::NodeId;
use bare_js::eval::Error;

impl UiOutcome {
    fn add(&mut self, o: UiOutcome) {
        self.reports.extend(o.reports);
        self.changed |= o.changed;
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
    /// The center of the border box of `node` in window coordinates. `(0, 0)` when it has none, or when the page cannot be laid out yet
    /// (a style sheet that was not supplied): the click still happens, as it did before layout was used.
    fn box_center(&mut self, node: NodeId) -> R<(f64, f64)> {
        let dom = self.dom.clone();
        let r = self.it.host_scope(|it| {
            if !dom.in_document(it, node)? {
                return Ok(None);
            }
            dom.with_geo(it, |g, s| g.client_rect(s, node))
        });
        match r {
            Ok(Some(r)) => Ok(((r.x + r.w / 2.0) as f64, (r.y + r.h / 2.0) as f64)),
            Ok(None) | Err(Error::Unsupported(_)) => Ok((0.0, 0.0)),
            Err(e) => Err(e),
        }
    }

    /// A user click on `node`. See the header.
    pub fn drive_click(&mut self, node: NodeId) -> R<UiOutcome> {
        let (x, y) = self.box_center(node)?;
        // A driver click is one click: two in a row at one element are not a double click.
        self.dom.st.borrow_mut().ui.last = None;
        let up = Pointer { x, y, ..Pointer::default() };
        let down = Pointer { buttons: 1, ..up };
        let mut out = self.ui_pointer_move(Some(node), up)?;
        out.add(self.ui_pointer_button(Some(node), down, 0, true, None)?);
        out.add(self.ui_pointer_button(Some(node), up, 0, false, None)?);
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

    /// Typed text in a text `<input>` or a `<textarea>`. See the header.
    pub fn drive_type(&mut self, node: NodeId, text: &str) -> R<UiOutcome> {
        if !self.is_text_input(node) {
            return Err(unsupported_input("test driver: type needs an editable <input> or <textarea>"));
        }
        let mut out = UiOutcome::default();
        self.drive_focus(node, &mut out)?;
        if self.active_element()? != Some(node) {
            return Err(unsupported_input("test driver: the field cannot take focus"));
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
