//! Window input for the browser host: pointer and keyboard events, and the default actions that a user makes.
//!
//! The window (`page.rs`) gives a pointer position, a hit-tested element (`bare_paint::hit`) and key data. This file
//! turns them into the events that Chrome sends, in the same order, and then runs the default action of the page, if no
//! listener canceled the event. The events are real `MouseEvent`, `PointerEvent`, `KeyboardEvent` and `InputEvent`
//! objects with `isTrusted` true. They go through the normal dispatch of `browser_events.rs` (capture, target, bubble),
//! one microtask checkpoint after each listener, like the other driver calls.
//!
//! Order (recorded from headless Chrome with `tools/ui/chrome_record.mjs`, kept in `tools/fixtures/ui/chrome-events.json`):
//! - Move: `pointerout`, `pointerleave`, `pointerover`, `pointerenter`, the same four for the mouse, then `pointermove`
//!   and `mousemove`. `out`/`over` go to the old and the new element. `leave` goes to each old ancestor that the pointer
//!   left (inner first), `enter` to each new one (outer first). Chrome sends `enter` and `leave` only to an element that
//!   has a listener for it; so does this file.
//! - Press: `pointerdown`, `mousedown`, then the focus change (`change` at the old field if the user edited it, `blur`,
//!   `focusout`, `focus`, `focusin`). Release: `pointerup`, `mouseup`, `click` at the common ancestor of the two targets.
//!   A canceled `pointerdown` skips the two mouse events. A canceled `mousedown` skips the focus change.
//! - Key down: `keydown`, then (when it was not canceled) `keypress` for a key that makes text and for Enter, `beforeinput`
//!   (cancelable), the change of the value, `input`. Key up: `keyup`. Space on a button, a checkbox or a radio clicks on key up.
//!   Enter on a button or a link clicks after `keypress`. Enter in a textarea inserts a line feed (`beforeinput` and `input` with
//!   `inputType` `insertLineBreak` and no `data`), with no `change` and no submission. Enter in a field: `beforeinput` (`insertLineBreak`), `change`, then a
//!   click on the default submit button of the form (implicit submission).
//! - Tab and Shift+Tab: `keydown`, the focus change, `keyup` at the new element. Tab order is the HTML sequential order:
//!   positive `tabindex` first (ascending), then the rest in tree order; a negative `tabindex` is skipped. The focus wraps.
//!   A field that gets focus by Tab has all its text selected.
//!
//! Default actions (only for window input; `Browser::click` keeps its older rules, see `browser_form.rs`)
//! - A press focuses the nearest focusable ancestor, or blurs when there is none. In a text field it puts the caret where
//!   the caller says; a move with the button down then extends the selection (`drag_select`), a second press selects the word and a third the
//!   value (`word_at`, `line_at`). Two presses count as a double click only when they are within 500 ms and 5px.
//! - A click toggles a checkbox before the click event runs (a listener sees the new state) and undoes it when the click is
//!   canceled; otherwise `input` and `change` follow. A radio does the same and clears the others of its group (same `name`
//!   and form owner). A click on a label focuses and clicks its control. A click on `a[href]` starts a navigation
//!   (`navigate_href`). A submit button submits its form (`browser_form.rs`, `browser_submit.rs`: validation, then native navigation).
//! - Editing: text, Backspace, Delete, Left, Right, Home, End (Shift extends the selection), Ctrl+A. Offsets are UTF-16
//!   units, as for `selectionStart`. The value changes through `put_input_value`, the same state that `input.value` reads.
//!
//! A `<textarea>` is edited like a field (typing, Enter, Backspace, Delete, `maxlength`). Gaps there: Up and Down and Home and End do not
//! move by line (Home and End go to the start and the end of the value), and the box does not wrap or scroll (`page.rs`).
//!
//! Gaps (none is faked): `contenteditable`;
//! clipboard keys; Ctrl+Left/Right word moves; IME and `compositionstart`; `readonly` is honored but
//! `type=number` does not filter text; `contextmenu`; `:hover`/`:focus` styling; radio groups in the Tab order (every
//! radio is a stop); `<summary>` toggling; `<label>` click coordinates of the second click; `UIEvent` and `WheelEvent`.
//!
//! The four constructors (`new MouseEvent(...)` and the others) are in `browser_uievent.rs`. The user-like driver inputs (`Input::Click`,
//! `Input::Type`, `Input::Key`) call this file through `browser_drive.rs`. A canceled `pointerdown` also skips the `mouseup`. A disabled
//! control gets the pointer events and no mouse event. Pointer capture retargets the moves and the release to its holder.

use crate::browser::*;
use crate::browser_dom as nd;
use crate::browser_selection::Dir;
use bare_css::cascade::Change;
use bare_html::dom::{Document, Mark, NodeData, NodeId};
use bare_js::eval::Interp;
use bare_js::value::{ObjId, Value};

type V = R<Value>;

/// Modifier keys held at the time of an event.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub meta: bool,
}

/// A pointer position in CSS pixels of the window, the buttons held, and the modifiers.
#[derive(Clone, Copy, Debug, Default)]
pub struct Pointer {
    pub x: f64,
    pub y: f64,
    pub buttons: u16,
    pub mods: Mods,
}

/// A key as the window sees it: DOM `key` and `code`, the text it types, and the modifiers.
#[derive(Clone, Debug, Default)]
pub struct KeyInput {
    pub key: String,
    pub code: String,
    pub text: Option<String>,
    pub repeat: bool,
    pub mods: Mods,
}

/// The fields of the events that the window makes, and of the events that `new MouseEvent(...)` and the like make.
#[derive(Clone, Default)]
pub(crate) struct Ui {
    pub x: f64,
    pub y: f64,
    pub screen: (f64, f64),
    pub page: (f64, f64),
    pub movement: (f64, f64),
    pub button: i16,
    pub buttons: u16,
    pub detail: i32,
    pub mods: Mods,
    /// The other modifier keys (`modifierCapsLock` and the like), a bit for each name of `uievent::EXTRA_MODS`.
    pub extra: u16,
    pub key: String,
    pub code: String,
    pub repeat: bool,
    pub location: u32,
    pub composing: bool,
    pub key_code: u32,
    pub char_code: u32,
    pub data: Option<String>,
    pub input_type: String,
    /// A `PointerEvent` made by script. `None`: the mouse of the window (`PointerData::MOUSE`).
    pub pointer: Option<Box<PointerData>>,
    /// Coordinates keep their fraction (a `PointerEvent`); a `MouseEvent` floors them, as Chrome does.
    pub frac: bool,
    /// `view` is the window.
    pub view: bool,
}

/// The members of a `PointerEvent` that a `MouseEvent` lacks.
#[derive(Clone)]
pub(crate) struct PointerData {
    pub id: i32,
    pub width: f64,
    pub height: f64,
    pub pressure: f64,
    pub tangential: f64,
    pub tilt: (i32, i32),
    pub twist: i32,
    pub kind: String,
    pub primary: bool,
}

/// State between window events.
#[derive(Default)]
pub(crate) struct UiState {
    hover: Option<NodeId>,
    down: Option<NodeId>,
    /// The last press: node, time, the count of fast presses in a row (`detail`) and the point. A press more than 5px away starts a new count.
    pub(crate) last: Option<(NodeId, f64, u32, (f64, f64))>,
    /// True while a window event runs: the events that the host makes (focus, `submit`) are then trusted.
    pub active: bool,
    /// Raised by each listener that runs and by each window event that can change what the page shows.
    pub epoch: u64,
    /// The extra members of the four interfaces are defined (`browser_uievent.rs`).
    pub ext: bool,
    /// The last pointer position, and the move since it (`movementX`).
    pub(crate) last_pos: Option<(f64, f64)>,
    mv: (f64, f64),
    /// Pointer capture (Pointer Events 3): the element that asked for it (pending), and the one that got `gotpointercapture`.
    pub(crate) capture: Option<NodeId>,
    pub(crate) captured: Option<NodeId>,
    /// The buttons held, for `setPointerCapture` (it does nothing while none is down).
    pub(crate) held: u16,
    /// A listener canceled `pointerdown`: the mouse events of this press are not sent (Chrome), the `click` still is.
    nomouse: bool,
    /// A drag that selects text: the text field and the offset where the press fell (the anchor).
    pub(crate) sel_drag: Option<(NodeId, u32)>,
    /// The last user input was the pointer, not a key: a focus that follows it shows no focus ring (`:focus-visible`), except in a text field.
    pub(crate) by_pointer: bool,
}

// ------------------------------------------------------------------------------------------ getters

pub(crate) fn with_ui<T>(d: &Dom, it: &mut Interp<'_>, this: Value, f: impl FnOnce(&EventState, &Ui) -> T) -> R<T> {
    let o = d.event_of(it, this)?;
    let st = d.st.borrow();
    let e = &st.events[&o];
    match &e.ui {
        Some(u) => Ok(f(e, u)),
        None => {
            drop(st);
            Err(it.type_error("Illegal invocation"))
        }
    }
}

macro_rules! num {
    ($name:ident, |$u:ident| $e:expr) => {
        pub(crate) fn $name(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
            with_ui(d, it, this, |_, $u| Value::Number($e as f64))
        }
    };
}
macro_rules! flag {
    ($name:ident, |$u:ident| $e:expr) => {
        pub(crate) fn $name(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
            with_ui(d, it, this, |_, $u| Value::Bool($e))
        }
    };
}

/// A coordinate as the event shows it: a `MouseEvent` floors it, a `PointerEvent` keeps the fraction.
pub(crate) fn coord(v: f64, frac: bool) -> f64 {
    if frac { v } else { v.floor() }
}

num!(client_x, |u| coord(u.x, u.frac));
num!(client_y, |u| coord(u.y, u.frac));
num!(screen_x, |u| coord(u.screen.0, u.frac));
num!(screen_y, |u| coord(u.screen.1, u.frac));
num!(page_x, |u| coord(u.page.0, u.frac));
num!(page_y, |u| coord(u.page.1, u.frac));
num!(button, |u| u.button);
num!(buttons, |u| u.buttons);
num!(detail, |u| u.detail);
num!(pointer_id, |u| u.pointer.as_ref().map_or(PointerData::MOUSE.id, |p| p.id));
num!(location, |u| u.location);
num!(key_code, |u| u.key_code);
flag!(shift, |u| u.mods.shift);
flag!(ctrl, |u| u.mods.ctrl);
flag!(alt, |u| u.mods.alt);
flag!(meta, |u| u.mods.meta);
flag!(repeat, |u| u.repeat);
flag!(is_primary, |u| u.pointer.as_ref().map_or(PointerData::MOUSE.primary, |p| p.primary));
flag!(is_composing, |u| u.composing);

impl PointerData {
    /// The mouse of the window: pointer 1, always primary. The pressure is the one of a button that is down.
    pub(crate) const MOUSE: PointerData = PointerData { id: 1, width: 1.0, height: 1.0, pressure: 0.0, tangential: 0.0, tilt: (0, 0), twist: 0, kind: String::new(), primary: true };
}

pub(crate) fn related(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    with_ui(d, it, this, |e, _| e.related)
}

pub(crate) fn pointer_type(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let k = with_ui(d, it, this, |_, u| u.pointer.as_ref().map_or("mouse".to_string(), |p| p.kind.clone()))?;
    d.js_str(it, &k)
}

fn text_of(d: &Dom, it: &mut Interp<'_>, this: Value, f: impl FnOnce(&Ui) -> String) -> V {
    let s = with_ui(d, it, this, |_, u| f(u))?;
    d.js_str(it, &s)
}

pub(crate) fn key(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    text_of(d, it, this, |u| u.key.clone())
}

pub(crate) fn code(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    text_of(d, it, this, |u| u.code.clone())
}

pub(crate) fn input_type(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    text_of(d, it, this, |u| u.input_type.clone())
}

pub(crate) fn data(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    match with_ui(d, it, this, |_, u| u.data.clone())? {
        Some(s) => d.js_str(it, &s),
        None => Ok(Value::Null),
    }
}

/// The legacy `keyCode`: the ASCII code of a letter or a digit, and the codes of the keys that scripts test.
fn key_code_of(key: &str, code: &str) -> u32 {
    match key {
        "Backspace" => 8,
        "Tab" => 9,
        "Enter" => 13,
        "Shift" => 16,
        "Control" => 17,
        "Meta" => 91,
        "Alt" => 18,
        "Escape" => 27,
        " " => 32,
        "PageUp" => 33,
        "PageDown" => 34,
        "End" => 35,
        "Home" => 36,
        "ArrowLeft" => 37,
        "ArrowUp" => 38,
        "ArrowRight" => 39,
        "ArrowDown" => 40,
        "Delete" => 46,
        _ => match (code.strip_prefix("Key"), code.strip_prefix("Digit")) {
            (Some(l), _) if l.len() == 1 => l.as_bytes()[0] as u32,
            (_, Some(n)) if n.len() == 1 => n.as_bytes()[0] as u32,
            _ => 0,
        },
    }
}

// ------------------------------------------------------------------------------------------ text helpers

/// The byte index of UTF-16 offset `u` in `s` (the end when `u` is past it).
fn byte_at(s: &str, u: u32) -> usize {
    let mut n = 0;
    for (i, c) in s.char_indices() {
        if n >= u {
            return i;
        }
        n += c.len_utf16() as u32;
    }
    s.len()
}

fn u16_len(s: &str) -> u32 {
    s.encode_utf16().count() as u32
}

/// The UTF-16 offset one character before `u`.
fn prev_off(s: &str, u: u32) -> u32 {
    s[..byte_at(s, u)].chars().next_back().map_or(0, |c| u - c.len_utf16() as u32)
}

/// The UTF-16 offset one character after `u`.
fn next_off(s: &str, u: u32) -> u32 {
    s[byte_at(s, u)..].chars().next().map_or(u, |c| u + c.len_utf16() as u32)
}

// ------------------------------------------------------------------------------------------ the engine

fn attr<'a>(doc: &'a Document, n: NodeId, name: &str) -> Option<&'a str> {
    doc.find_atom(name).and_then(|a| doc.attr(n, a))
}

fn is_tag(doc: &Document, n: NodeId, tag: &str) -> bool {
    doc.is_html(n, tag)
}

/// The input types that take text.
fn text_kind(kind: &str) -> bool {
    matches!(kind, "text" | "search" | "tel" | "url" | "email" | "password" | "number")
}

impl Dom {
    fn bump(&self) {
        self.st.borrow_mut().ui.epoch += 1;
    }

    /// A `Ui` event at `target`. Gives whether it was not canceled.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn fire_ui(&self, it: &mut Interp<'_>, target: NodeId, ty: &str, proto: ObjId, bubbles: bool, cancelable: bool, mut ui: Ui, related: Option<NodeId>) -> R<bool> {
        self.ensure_ui_ext(it)?;
        let sc = self.page_offset();
        (ui.screen, ui.page, ui.view) = ((ui.x, ui.y), (ui.x + sc.0, ui.y + sc.1), true);
        let ev = self.new_event_proto(it, ty, bubbles, cancelable, true, proto)?;
        let rel = match related {
            Some(r) => self.wrap(it, r)?,
            None => Value::Null,
        };
        {
            let mut st = self.st.borrow_mut();
            let e = st.events.get_mut(&ev).expect("just made");
            e.ui = Some(Box::new(ui));
            e.trusted = true;
            e.related = rel;
        }
        self.dispatch(it, Target::Node(target), ev)
    }

    /// A pointer or mouse event. `ty` starts with `pointer` for a `PointerEvent`; `click` is one too (as in Chrome).
    #[allow(clippy::too_many_arguments)]
    fn mouse_ev(&self, it: &mut Interp<'_>, target: NodeId, ty: &str, p: &Pointer, button: i16, buttons: u16, detail: u32, related: Option<NodeId>) -> R<bool> {
        let pointer = ty.starts_with("pointer") || matches!(ty, "click" | "auxclick");
        let enter_leave = ty.ends_with("enter") || ty.ends_with("leave");
        let proto = if pointer { self.p().pointer } else { self.p().mouse };
        let movement = if ty.ends_with("move") { self.st.borrow().ui.mv } else { (0.0, 0.0) };
        let ui = Ui { x: p.x, y: p.y, button, buttons, detail: detail as i32, mods: p.mods, movement, frac: ty.contains("pointer"), ..Ui::default() };
        self.fire_ui(it, target, ty, proto, !enter_leave, !enter_leave, ui, related)
    }

    /// The nodes with a listener of one of `types` added by a script (`addEventListener` or an `on*` property). An agent snapshot shows
    /// them as clickable even without a role.
    pub fn listened(&self, types: &[&str]) -> std::collections::HashSet<NodeId> {
        self.st.borrow().listeners.iter().filter_map(|(t, v)| match t {
            Target::Node(n) if v.iter().any(|l| types.contains(&&*l.ty)) => Some(*n),
            _ => None,
        }).collect()
    }

    fn has_listener(&self, n: NodeId, ty: &str) -> bool {
        crate::browser_handlers::sync(self, Target::Node(n), ty);
        self.st.borrow().listeners.get(&Target::Node(n)).is_some_and(|v| v.iter().any(|l| *l.ty == *ty))
    }

    /// `n` and its ancestors, inner first.
    fn chain(&self, n: Option<NodeId>) -> Vec<NodeId> {
        let doc = self.doc.borrow();
        std::iter::successors(n, |&c| doc.parent(c)).collect()
    }

    fn body_or_root(&self, it: &mut Interp<'_>) -> R<NodeId> {
        let root = self.doc.borrow().root();
        Ok(self.query(it, root, "body", false)?.into_iter().next().unwrap_or(root))
    }

    fn input_kind_of(&self, n: NodeId) -> Option<&'static str> {
        let doc = self.doc.borrow();
        is_tag(&doc, n, "input").then(|| nd::input_kind(&doc, n))
    }

    /// A text field that the user can edit now.
    fn editable(&self, n: NodeId) -> bool {
        let doc = self.doc.borrow();
        (is_tag(&doc, n, "textarea") || (is_tag(&doc, n, "input") && text_kind(nd::input_kind(&doc, n)))) && attr(&doc, n, "readonly").is_none()
    }

    // ------------------------------------------------------------------ pointer

    pub(crate) fn ui_move(&self, it: &mut Interp<'_>, to: Option<NodeId>, p: &Pointer) -> R<()> {
        let (captured, old) = {
            let mut st = self.st.borrow_mut();
            let last = st.ui.last_pos.replace((p.x, p.y));
            st.ui.mv = last.map_or((0.0, 0.0), |l| (p.x - l.0, p.y - l.1));
            (st.ui.capture, st.ui.hover)
        };
        self.capture_events(it, p, -1)?;
        // A captured pointer has no boundary events: the moves go to the element that holds it.
        let to = if captured.is_some() { captured } else { to };
        if captured.is_none() {
            self.hover_to(to);
        }
        if old != to && captured.is_none() {
            self.pointer_boundary(it, old, to, p)?;
        }
        if let Some(t) = to {
            self.mouse_ev(it, t, "pointermove", p, -1, p.buttons, 0, None)?;
            self.mouse_ev(it, t, "mousemove", p, 0, p.buttons, 0, None)?;
        }
        Ok(())
    }

    /// The boundary events of a pointer that went from `old` to `to`, for the pointer and then the mouse: out, leave, over, enter.
    fn pointer_boundary(&self, it: &mut Interp<'_>, old: Option<NodeId>, to: Option<NodeId>, p: &Pointer) -> R<()> {
        let (oc, nc) = (self.chain(old), self.chain(to));
        for (out, over, leave, enter, move_button) in [("pointerout", "pointerover", "pointerleave", "pointerenter", -1), ("mouseout", "mouseover", "mouseleave", "mouseenter", 0)] {
            if let Some(o) = old {
                self.mouse_ev(it, o, out, p, move_button, p.buttons, 0, to)?;
            }
            for &n in oc.iter().filter(|n| !nc.contains(n)) {
                if self.has_listener(n, leave) {
                    self.mouse_ev(it, n, leave, p, move_button, p.buttons, 0, to)?;
                }
            }
            if let Some(n) = to {
                self.mouse_ev(it, n, over, p, move_button, p.buttons, 0, old)?;
            }
            for &n in nc.iter().rev().filter(|n| !oc.contains(n)) {
                if self.has_listener(n, enter) {
                    self.mouse_ev(it, n, enter, p, move_button, p.buttons, 0, old)?;
                }
            }
        }
        Ok(())
    }

    /// The pointer did not move, but the page under it changed: Chrome sends the boundary events (no move) to what is under it now.
    pub(crate) fn ui_settle(&self, it: &mut Interp<'_>, to: Option<NodeId>, p: &Pointer) -> R<()> {
        let old = self.st.borrow().ui.hover;
        if old == to || self.st.borrow().ui.capture.is_some() {
            return Ok(());
        }
        self.hover_to(to);
        self.pointer_boundary(it, old, to, p)
    }

    /// The element under the pointer is `to` now. When a rule reads `:hover`, the page may look different: ask for a frame.
    fn hover_to(&self, to: Option<NodeId>) {
        self.st.borrow_mut().ui.hover = to;
        if self.doc.borrow_mut().set_hover(to) && self.user_rules() & bare_css::selector::USES_HOVER != 0 {
            self.bump();
        }
    }

    /// The pointer capture steps (Pointer Events 3, "process pending pointer capture"): `lostpointercapture` at the old holder and
    /// `gotpointercapture` at the new one, before the next pointer event. They carry the fields of that event (`button`).
    fn capture_events(&self, it: &mut Interp<'_>, p: &Pointer, button: i16) -> R<()> {
        let (want, have) = {
            let st = self.st.borrow();
            (st.ui.capture, st.ui.captured)
        };
        if want == have {
            return Ok(());
        }
        self.st.borrow_mut().ui.captured = want;
        for (ty, n) in [("lostpointercapture", have), ("gotpointercapture", want)] {
            if let Some(n) = n {
                let ui = Ui { x: p.x, y: p.y, button, buttons: p.buttons, mods: p.mods, frac: true, ..Ui::default() };
                self.fire_ui(it, n, ty, self.p().pointer, true, false, ui, None)?;
            }
        }
        Ok(())
    }

    pub(crate) fn ui_leave(&self, it: &mut Interp<'_>, p: &Pointer) -> R<()> {
        let old = self.st.borrow().ui.hover;
        self.hover_to(None);
        if let Some(o) = old {
            for (out, leave) in [("pointerout", "pointerleave"), ("mouseout", "mouseleave")] {
                self.mouse_ev(it, o, out, p, if out.starts_with('p') { -1 } else { 0 }, p.buttons, 0, None)?;
                for n in self.chain(old) {
                    if self.has_listener(n, leave) {
                        self.mouse_ev(it, n, leave, p, if out.starts_with('p') { -1 } else { 0 }, p.buttons, 0, None)?;
                    }
                }
            }
        }
        Ok(())
    }

    pub(crate) fn ui_button(&self, it: &mut Interp<'_>, target: Option<NodeId>, p: &Pointer, button: u8, pressed: bool, caret: Option<u32>) -> R<()> {
        self.st.borrow_mut().ui.held = p.buttons;
        if !pressed && button == 0 {
            self.st.borrow_mut().ui.sel_drag = None;
            self.doc.borrow_mut().set_active(None);
        }
        if pressed {
            self.st.borrow_mut().ui.by_pointer = true;
        }
        let held = self.st.borrow().ui.capture;
        let Some(t) = held.or(target) else {
            if !pressed {
                self.st.borrow_mut().ui.down = None;
            }
            return Ok(());
        };
        // A disabled control gets the pointer events and none of the mouse events (Chrome).
        let disabled = self.is_disabled_control(it, t)?;
        let b = button as i16;
        if pressed {
            let now = self.now_ms();
            let count = match self.st.borrow().ui.last {
                Some((n, at, c, (x, y))) if n == t && now - at < 500.0 && button == 0 && (p.x - x).abs() <= 5.0 && (p.y - y).abs() <= 5.0 => c + 1,
                _ => 1,
            };
            if button == 0 {
                self.st.borrow_mut().ui.last = Some((t, now, count, (p.x, p.y)));
            }
            self.st.borrow_mut().ui.down = Some(t);
            if button == 0 && !disabled {
                self.doc.borrow_mut().set_active(Some(t));
            }
            self.capture_events(it, p, b)?;
            let go = self.mouse_ev(it, t, "pointerdown", p, b, p.buttons, 0, None)?;
            self.st.borrow_mut().ui.nomouse = !go;
            if !go || disabled {
                return Ok(());
            }
            // The mouse events of a press go to the holder of the capture, if a listener of `pointerdown` set one.
            let t = self.st.borrow().ui.capture.unwrap_or(t);
            if self.mouse_ev(it, t, "mousedown", p, b, p.buttons, count, None)? && button == 0 {
                self.focus_by_press(it, t, caret, count)?;
            }
        } else {
            let down = self.st.borrow_mut().ui.down.take();
            let count = self.st.borrow().ui.last.map_or(1, |l| l.2);
            self.capture_events(it, p, b)?;
            self.mouse_ev(it, t, "pointerup", p, b, p.buttons, 0, None)?;
            if disabled {
                self.st.borrow_mut().ui.capture = None;
                return self.capture_events(it, p, b);
            }
            let t = self.st.borrow().ui.capture.unwrap_or(t);
            if !std::mem::take(&mut self.st.borrow_mut().ui.nomouse) {
                self.mouse_ev(it, t, "mouseup", p, b, p.buttons, count, None)?;
            }
            // After the pointer went up, the capture ends (`lostpointercapture`), before the `click`.
            self.st.borrow_mut().ui.capture = None;
            self.capture_events(it, p, b)?;
            let Some(d) = down else { return Ok(()) };
            // The click goes to the nearest common inclusive ancestor of the press and the release.
            let up = self.chain(Some(t));
            let Some(c) = self.chain(Some(d)).into_iter().find(|n| up.contains(n)) else { return Ok(()) };
            let ui = Ui { x: p.x, y: p.y, button: b, buttons: p.buttons, detail: count as i32, mods: p.mods, ..Ui::default() };
            if button == 0 {
                self.user_click(it, c, Some(ui))?;
                if count == 2 {
                    self.mouse_ev(it, c, "dblclick", p, b, p.buttons, 2, None)?;
                }
            } else {
                self.mouse_ev(it, c, "auxclick", p, b, p.buttons, count, None)?;
            }
        }
        Ok(())
    }

    /// The focus change of a press: the nearest focusable ancestor gets focus, or the page loses it.
    fn focus_by_press(&self, it: &mut Interp<'_>, t: NodeId, caret: Option<u32>, count: u32) -> R<()> {
        let mut to = None;
        for n in self.chain(Some(t)) {
            if self.focusable(it, n)? {
                to = Some(n);
                break;
            }
        }
        self.commit_if_edited(it, to)?;
        self.move_focus(it, to)?;
        // A press that focuses a control without text keeps the selection owner, and Chrome sends `select` again to a range there.
        let owner = self.st.borrow().focus.sel_owner;
        if let Some(f) = to.filter(|&f| !self.selection_applies(f))
            && let Some(o) = owner.filter(|&o| o != f)
            && let Some((sel, _)) = self.effective(Some(it), o)?
            && sel.start != sel.end
        {
            self.queue_selection_tasks(o, false, true, true)?;
        }
        if to.is_none() {
            // The press puts the page selection in the document, out of any control.
            self.st.borrow_mut().focus.sel_owner = None;
        }
        if let (Some(f), Some(c)) = (to, caret)
            && self.selection_applies(f)
            && self.current_focus(it)? == Some(f)
        {
            self.set_user_selection(it, f, Some(c), Some(c), Dir::Forward)?;
            self.st.borrow_mut().ui.sel_drag = Some((f, c));
            // Two presses select the word at the caret, three the whole value (the line in a textarea).
            if count >= 2 {
                let value = self.input_value_of(f).unwrap_or_default();
                let (a, b) = if count == 2 { word_at(&value, c) } else { line_at(&value, c, is_tag(&self.doc.borrow(), f, "textarea")) };
                self.set_user_selection(it, f, Some(a), Some(b), Dir::Forward)?;
            }
        }
        Ok(())
    }

    /// The pointer moved with the button down after a press in a text field: the selection runs from the press to `caret`.
    pub(crate) fn drag_select(&self, it: &mut Interp<'_>, caret: u32) -> R<()> {
        let Some((f, anchor)) = self.st.borrow().ui.sel_drag else { return Ok(()) };
        if self.current_focus(it)? != Some(f) {
            self.st.borrow_mut().ui.sel_drag = None;
            return Ok(());
        }
        let dir = if caret < anchor { Dir::Backward } else { Dir::Forward };
        self.set_user_selection(it, f, Some(anchor.min(caret)), Some(anchor.max(caret)), dir)
    }

    /// A field that the user edited gets its `change` event when focus leaves it (`to` is the new focus).
    fn commit_if_edited(&self, it: &mut Interp<'_>, to: Option<NodeId>) -> R<()> {
        let Some(f) = self.current_focus(it)? else { return Ok(()) };
        if Some(f) != to {
            self.fire_change(it, f)?;
        }
        Ok(())
    }

    fn fire_change(&self, it: &mut Interp<'_>, f: NodeId) -> R<()> {
        let before = self.st.borrow_mut().inputs.get_mut(&f).and_then(|i| i.edited.take());
        if before.is_some_and(|b| self.input_value_of(f).as_deref() != Some(b.as_str())) {
            self.fire_plain(it, f, "change")?;
        }
        Ok(())
    }

    /// A trusted plain `Event` that bubbles (`input` of a checkbox, `change`).
    fn fire_plain(&self, it: &mut Interp<'_>, n: NodeId, ty: &str) -> R<bool> {
        let ev = self.new_event(it, ty, true, false, true, None)?;
        self.st.borrow_mut().events.get_mut(&ev).expect("just made").trusted = true;
        self.dispatch(it, Target::Node(n), ev)
    }

    // ------------------------------------------------------------------ click

    /// Before the click event: a checkbox or a radio takes its new state (HTML "legacy-pre-activation behavior").
    /// Gives what undoes it.
    pub(crate) fn pre_activate(&self, it: &mut Interp<'_>, t: NodeId) -> R<Option<Vec<(NodeId, bool)>>> {
        let kind = self.input_kind_of(t);
        if !matches!(kind, Some("checkbox" | "radio")) {
            return Ok(None);
        }
        let checked = |n: NodeId| self.st.borrow().inputs.get(&n).and_then(|i| i.checked).unwrap_or_else(|| attr(&self.doc.borrow(), n, "checked").is_some());
        let mut undo = vec![(t, checked(t))];
        if kind == Some("checkbox") {
            self.driver_input(t, None, Some(!undo[0].1))?;
        } else {
            for n in self.radio_group(it, t)? {
                undo.push((n, checked(n)));
                self.driver_input(n, None, Some(false))?;
            }
            self.driver_input(t, None, Some(true))?;
        }
        Ok(Some(undo))
    }

    /// The other radios of the group of `t`: same `name`, same form owner, same tree.
    pub(crate) fn radio_group(&self, it: &mut Interp<'_>, t: NodeId) -> R<Vec<NodeId>> {
        let (name, root) = {
            let doc = self.doc.borrow();
            let Some(name) = attr(&doc, t, "name").filter(|s| !s.is_empty()).map(str::to_string) else { return Ok(Vec::new()) };
            (name, doc.root())
        };
        let owner = self.form_owner(it, t)?;
        let all: Vec<NodeId> = {
            let doc = self.doc.borrow();
            let mut out = Vec::new();
            let (mut cur, mut depth) = (root, 0);
            while let Some((c, d)) = nd::next_in(&doc, root, cur, depth) {
                (cur, depth) = (c, d);
                if c != t && is_tag(&doc, c, "input") && nd::input_kind(&doc, c) == "radio" && attr(&doc, c, "name") == Some(name.as_str()) {
                    out.push(c);
                }
            }
            out
        };
        let mut group = Vec::new();
        for n in all {
            if self.form_owner(it, n)? == owner {
                group.push(n);
            }
        }
        Ok(group)
    }

    /// After the click event, for a click that the window made: the activation behavior of `t`.
    pub(crate) fn ui_activate(&self, it: &mut Interp<'_>, t: NodeId, canceled: bool, undo: Option<Vec<(NodeId, bool)>>, ui: &Ui) -> R<()> {
        if let Some(undo) = undo {
            if canceled {
                for (n, c) in undo {
                    self.driver_input(n, None, Some(c))?;
                }
            } else if self.is_connected_node(it, t)? && (undo[0].1 != (self.input_checked(t)) || self.input_kind_of(t) == Some("checkbox")) {
                self.fire_plain(it, t, "input")?;
                self.fire_plain(it, t, "change")?;
            }
            return Ok(());
        }
        if canceled {
            return Ok(());
        }
        let (is_label, href, blank) = {
            let doc = self.doc.borrow();
            let a = is_tag(&doc, t, "a");
            (is_tag(&doc, t, "label"), if a { attr(&doc, t, "href").map(str::to_string) } else { None }, a && attr(&doc, t, "target").is_some_and(|v| v.eq_ignore_ascii_case("_blank")))
        };
        if let Some(h) = href {
            // A driver that wants tabs (`agent.rs`) gets the URL; the page stays.
            if blank && self.resolve_http(&h).is_some_and(|u| crate::agent::tab(&u)) {
                return Ok(());
            }
            return self.navigate_href(it, &h);
        }
        if is_label && let Some(c) = self.label_control(it, t)? {
            self.focus_node(it, c)?;
            return self.user_click(it, c, Some(ui.clone())).map(|_| ());
        }
        self.activate(it, t)
    }

    pub(crate) fn input_checked(&self, n: NodeId) -> bool {
        self.st.borrow().inputs.get(&n).and_then(|i| i.checked).unwrap_or_else(|| attr(&self.doc.borrow(), n, "checked").is_some())
    }

    // ------------------------------------------------------------------ keyboard

    fn key_ev(&self, it: &mut Interp<'_>, target: NodeId, ty: &str, k: &KeyInput) -> R<bool> {
        // keyCode: the legacy code of the key. A `keypress` carries the character instead (`charCode`, and `keyCode` the same).
        let char_code = if ty == "keypress" { k.text.as_deref().and_then(|t| t.chars().next()).map_or(13, |c| c as u32) } else { 0 };
        let key_code = if ty == "keypress" { char_code } else { key_code_of(&k.key, &k.code) };
        let location = match k.code.as_str() {
            c if c.ends_with("Left") && c.len() > 4 && !c.starts_with("Arrow") => 1,
            c if c.ends_with("Right") && c.len() > 5 && !c.starts_with("Arrow") => 2,
            c if c.starts_with("Numpad") => 3,
            _ => 0,
        };
        let ui = Ui { key: k.key.clone(), code: k.code.clone(), repeat: k.repeat, mods: k.mods, location, key_code, char_code, ..Ui::default() };
        self.fire_ui(it, target, ty, self.p().keyboard, true, true, ui, None)
    }

    /// `beforeinput` (cancelable) or `input`.
    fn input_ev(&self, it: &mut Interp<'_>, target: NodeId, ty: &str, kind: &str, data: Option<&str>) -> R<bool> {
        let ui = Ui { data: data.map(str::to_string), input_type: kind.to_string(), ..Ui::default() };
        self.fire_ui(it, target, ty, self.p().inputev, true, ty == "beforeinput", ui, None)
    }

    fn zero_click(&self) -> Ui {
        Ui::default()
    }

    fn focus_target(&self, it: &mut Interp<'_>) -> R<NodeId> {
        match self.current_focus(it)? {
            Some(f) => Ok(f),
            None => self.body_or_root(it),
        }
    }

    pub(crate) fn ui_key(&self, it: &mut Interp<'_>, k: &KeyInput, down: bool) -> R<()> {
        let t = self.focus_target(it)?;
        if down {
            self.st.borrow_mut().ui.by_pointer = false;
            if self.key_ev(it, t, "keydown", k)? {
                self.key_default(it, k)?;
            }
        } else if self.key_ev(it, t, "keyup", k)? && k.code == "Space" && !k.mods.ctrl && !k.mods.alt {
            // Space clicks a button, a checkbox or a radio on key up.
            let t = self.focus_target(it)?;
            let by_space = {
                let doc = self.doc.borrow();
                is_tag(&doc, t, "button") || (is_tag(&doc, t, "input") && !text_kind(nd::input_kind(&doc, t)))
            };
            if by_space {
                self.user_click(it, t, Some(self.zero_click()))?;
            }
        }
        Ok(())
    }

    fn key_default(&self, it: &mut Interp<'_>, k: &KeyInput) -> R<()> {
        let focus = self.current_focus(it)?;
        let t = self.focus_target(it)?;
        let edit = focus.filter(|&f| self.editable(f));
        let m = k.mods;
        match k.key.as_str() {
            "Tab" if !m.ctrl && !m.alt && !m.meta => return self.tab(it, focus, m.shift),
            "Enter" => {
                if !self.key_ev(it, t, "keypress", k)? {
                    return Ok(());
                }
                let (button_like, field) = {
                    let doc = self.doc.borrow();
                    let kind = is_tag(&doc, t, "input").then(|| nd::input_kind(&doc, t));
                    let b = is_tag(&doc, t, "button") || is_tag(&doc, t, "summary") || (is_tag(&doc, t, "a") && attr(&doc, t, "href").is_some()) || matches!(kind, Some("submit" | "image" | "reset" | "button" | "checkbox" | "radio"));
                    (b && !matches!(kind, Some("checkbox" | "radio")), kind.is_some() && !matches!(kind, Some("submit" | "image" | "reset" | "button" | "file" | "hidden")))
                };
                if button_like {
                    self.user_click(it, t, Some(self.zero_click()))?;
                } else if self.editable(t) && is_tag(&self.doc.borrow(), t, "textarea") {
                    self.edit(it, t, "insertLineBreak", Some("\n"))?;
                } else if field {
                    if self.editable(t) && !self.input_ev(it, t, "beforeinput", "insertLineBreak", None)? {
                        return Ok(());
                    }
                    self.fire_change(it, t)?;
                    self.implicit_submit(it, t)?;
                }
            }
            "Backspace" | "Delete" if edit.is_some() => {
                let f = edit.expect("checked");
                self.edit(it, f, if k.key == "Backspace" { "deleteContentBackward" } else { "deleteContentForward" }, None)?;
            }
            "ArrowLeft" | "ArrowRight" | "Home" | "End" if focus.is_some_and(|f| self.selection_applies(f)) => {
                self.move_caret(it, focus.expect("checked"), &k.key, m.shift)?;
            }
            "ArrowUp" | "ArrowDown" | "ArrowLeft" | "ArrowRight" | "PageUp" | "PageDown" | "Home" | "End" if !m.ctrl && !m.alt && !m.meta && !(matches!(k.key.as_str(), "ArrowUp" | "ArrowDown") && edit.is_some()) => {
                use crate::browser_scroll::ScrollKey as S;
                let key = match k.key.as_str() {
                    "ArrowUp" => S::Up,
                    "ArrowDown" => S::Down,
                    "ArrowLeft" => S::Left,
                    "ArrowRight" => S::Right,
                    "PageUp" => S::PageUp,
                    "PageDown" => S::PageDown,
                    "Home" => S::Home,
                    _ => S::End,
                };
                self.note_scroll_key(key);
            }
            "a" | "A" if m.ctrl && !m.alt && focus.is_some_and(|f| self.selection_applies(f)) => {
                let f = focus.expect("checked");
                self.set_user_selection(it, f, Some(0), Some(u32::MAX), Dir::Forward)?;
            }
            " " if edit.is_none() && !m.ctrl && !m.alt && !m.meta => {
                if !self.key_ev(it, t, "keypress", k)? {
                    return Ok(());
                }
                // On a button, a checkbox or a radio, Space clicks on key up; elsewhere it scrolls a page.
                let by_space = {
                    let doc = self.doc.borrow();
                    is_tag(&doc, t, "button") || (is_tag(&doc, t, "input") && !text_kind(nd::input_kind(&doc, t)))
                };
                if !by_space {
                    self.note_scroll_key(if m.shift { crate::browser_scroll::ScrollKey::PageUp } else { crate::browser_scroll::ScrollKey::PageDown });
                }
            }
            _ => {
                let Some(text) = k.text.as_deref().filter(|s| !s.is_empty() && !m.ctrl && !m.alt && !m.meta) else { return Ok(()) };
                if !self.key_ev(it, t, "keypress", k)? {
                    return Ok(());
                }
                if let Some(f) = edit {
                    self.edit(it, f, "insertText", Some(text))?;
                }
            }
        }
        Ok(())
    }

    /// Change the value of field `n` as `kind` says, with the events of Chrome.
    fn edit(&self, it: &mut Interp<'_>, n: NodeId, kind: &str, text: Option<&str>) -> R<()> {
        let Some(sel) = self.edit_sel(n)? else { return Ok(()) };
        let value = self.input_value_of(n).unwrap_or_default();
        let (from, to) = match kind {
            _ if sel.start != sel.end => (sel.start, sel.end),
            "deleteContentBackward" => (prev_off(&value, sel.start), sel.start),
            "deleteContentForward" => (sel.start, next_off(&value, sel.start)),
            _ => (sel.start, sel.end),
        };
        if !kind.starts_with("insert") && from == to {
            return Ok(());
        }
        // `maxlength`: the typed text is cut to fit, and nothing happens when none of it fits.
        let fitted;
        let text = match (kind, text, self.max_length(n)) {
            ("insertText" | "insertLineBreak", Some(t), Some(max)) => {
                fitted = crate::browser_submit::fit_units(t, max.saturating_sub(u16_len(&value) - (to - from)));
                if fitted.is_empty() {
                    return Ok(());
                }
                Some(fitted.as_str())
            }
            _ => text,
        };
        // An `insertLineBreak` has no `data`.
        if !self.input_ev(it, n, "beforeinput", kind, if kind == "insertLineBreak" { None } else { text })? {
            return Ok(());
        }
        // A listener may have changed the value: read it and the range again.
        let value = self.input_value_of(n).unwrap_or_default();
        let (a, b) = (byte_at(&value, from), byte_at(&value, to));
        let text = text.unwrap_or("");
        let mut new = String::with_capacity(value.len() + text.len());
        new.push_str(&value[..a]);
        new.push_str(text);
        new.push_str(&value[b..]);
        let caret = from + u16_len(text);
        self.put_input_value(None, n, new)?;
        {
            let mut st = self.st.borrow_mut();
            let i = st.inputs.entry(n).or_default();
            i.edited.get_or_insert(value);
            i.typed = true;
        }
        self.select_text(it, n, Some(caret))?;
        self.input_ev(it, n, "input", kind, if text.is_empty() || kind == "insertLineBreak" { None } else { Some(text) })?;
        Ok(())
    }

    fn move_caret(&self, it: &mut Interp<'_>, n: NodeId, key: &str, extend: bool) -> R<()> {
        let Some((sel, len)) = self.effective(None, n)? else { return Ok(()) };
        let value = self.input_value_of(n).unwrap_or_default();
        // The selection has an anchor and a focus end: Shift moves the focus end.
        let (anchor, focus) = if sel.dir == Dir::Backward { (sel.end, sel.start) } else { (sel.start, sel.end) };
        let (left, right) = (key == "ArrowLeft", key == "ArrowRight");
        let focus = match key {
            _ if !extend && sel.start != sel.end && (left || right) => {
                let c = if left { sel.start } else { sel.end };
                return self.set_user_selection(it, n, Some(c), Some(c), Dir::Forward);
            }
            "ArrowLeft" => prev_off(&value, focus),
            "ArrowRight" => next_off(&value, focus),
            "Home" => 0,
            _ => len,
        };
        if !extend {
            return self.set_user_selection(it, n, Some(focus), Some(focus), Dir::Forward);
        }
        let dir = if focus < anchor { Dir::Backward } else { Dir::Forward };
        self.set_user_selection(it, n, Some(anchor.min(focus)), Some(anchor.max(focus)), dir)
    }

    /// Implicit submission: a click on the default button of the form of field `n`.
    fn implicit_submit(&self, it: &mut Interp<'_>, n: NodeId) -> R<()> {
        let Some(form) = self.form_owner(it, n)? else { return Ok(()) };
        let (button, fields) = {
            let doc = self.doc.borrow();
            let (mut cur, mut depth, mut button, mut fields) = (form, 0, None, 0);
            while let Some((c, d)) = nd::next_in(&doc, form, cur, depth) {
                (cur, depth) = (c, d);
                if button.is_none() && self.submit_button(&doc, c) {
                    button = Some(c);
                }
                if is_tag(&doc, c, "input") && text_kind(nd::input_kind(&doc, c)) {
                    fields += 1;
                }
            }
            (button, fields)
        };
        match button {
            Some(b) => self.user_click(it, b, Some(self.zero_click())).map(|_| ()),
            None if fields <= 1 => self.submit_form(it, form, None),
            None => Ok(()),
        }
    }

    /// Tab order: positive `tabindex` ascending, then the rest in tree order. Negative `tabindex` is skipped.
    fn tab_order(&self, it: &mut Interp<'_>) -> R<Vec<NodeId>> {
        let (mut positive, mut zero) = (Vec::<(i64, NodeId)>::new(), Vec::new());
        let root = self.doc.borrow().root();
        let nodes: Vec<NodeId> = {
            let doc = self.doc.borrow();
            let (mut cur, mut depth, mut out) = (root, 0, Vec::new());
            while let Some((c, d)) = nd::next_in(&doc, root, cur, depth) {
                (cur, depth) = (c, d);
                let candidate = ["a", "button", "input", "select", "textarea", "summary"].iter().any(|t| is_tag(&doc, c, t)) || attr(&doc, c, "tabindex").is_some() || attr(&doc, c, "contenteditable").is_some();
                if candidate {
                    out.push(c);
                }
            }
            out
        };
        for n in nodes {
            let index = attr(&self.doc.borrow(), n, "tabindex").and_then(|v| v.trim().parse::<i64>().ok());
            if index.is_some_and(|i| i < 0) || !self.focusable(it, n)? {
                continue;
            }
            match index {
                Some(i) if i > 0 => positive.push((i, n)),
                _ => zero.push(n),
            }
        }
        positive.sort_by_key(|p| p.0); // stable: tree order inside one value
        Ok(positive.into_iter().map(|p| p.1).chain(zero).collect())
    }

    fn tab(&self, it: &mut Interp<'_>, from: Option<NodeId>, back: bool) -> R<()> {
        let order = self.tab_order(it)?;
        if order.is_empty() {
            return Ok(());
        }
        let at = from.and_then(|f| order.iter().position(|&n| n == f));
        let next = match (at, back) {
            (Some(i), false) => order[(i + 1) % order.len()],
            (Some(i), true) => order[(i + order.len() - 1) % order.len()],
            (None, false) => order[0],
            (None, true) => order[order.len() - 1],
        };
        self.commit_if_edited(it, Some(next))?;
        self.move_focus(it, Some(next))?;
        if self.current_focus(it)? == Some(next) && self.selection_applies(next) {
            self.set_user_selection(it, next, Some(0), Some(u32::MAX), Dir::Forward)?;
        }
        Ok(())
    }

    pub(crate) fn submit_button(&self, doc: &Document, n: NodeId) -> bool {
        match *doc.data(n) {
            NodeData::Element { .. } if is_tag(doc, n, "button") => !matches!(attr(doc, n, "type").map(str::to_ascii_lowercase).as_deref(), Some("reset" | "button")),
            NodeData::Element { .. } if is_tag(doc, n, "input") => matches!(nd::input_kind(doc, n), "submit" | "image"),
            _ => false,
        }
    }
}

// ------------------------------------------------------------------------------------------ the driver

/// What a window input call did: listener errors and whether the page may look different now.
#[derive(Debug, Default)]
pub struct UiOutcome {
    pub reports: Vec<Report>,
    /// A listener ran or the default action of an event may have changed the page. Draw a new frame.
    pub changed: bool,
    /// A driver click that went to another element than the one it aimed at: what it hit and the point.
    pub intercepted: Option<(String, (i32, i32))>,
}

/// The style and the layout of the page at one time: what a frame paints and what hit testing reads.
pub struct Frame {
    pub styles: std::rc::Rc<bare_css::cascade::Styles>,
    pub layout: bare_layout::fragment::Layout,
    /// The scroll containers and the size of the page content.
    pub areas: bare_layout::scroll::Areas,
    /// The page bars: vertical, horizontal. The layout is narrower and shorter by their width.
    pub bars: (bool, bool),
    /// What `Browser::refresh` needs to update the frame: the rule set build of the styles, and the `:hover`, `:active` and focus
    /// element chains that they were made with. `None`: only a new frame is right.
    since: Option<(u64, [Vec<NodeId>; 3])>,
}

/// The element and its ancestors, for each of `:hover`, `:active` and focus.
pub(crate) fn chains(doc: &Document) -> [Vec<NodeId>; 3] {
    let u = doc.user();
    // The shadow-including ancestors: `:hover` and `:focus-within` match a host (`Document::in_chain`).
    [u.hover, u.active, u.focus].map(|n| std::iter::successors(n, |&p| doc.parent(p).or_else(|| doc.shadow_host(p))).filter(|&p| matches!(doc.data(p), NodeData::Element { .. })).collect())
}

impl Browser {
    /// See `Dom::listened`.
    pub fn listened(&self, types: &[&str]) -> std::collections::HashSet<NodeId> {
        self.dom.listened(types)
    }

    /// Cascade and lay out the page now, with the caps of `getComputedStyle` and `offsetWidth` (a linked style sheet that
    /// was not supplied is `Unsupported`, as there).
    pub fn render(&mut self) -> R<Frame> {
        let dom = self.dom.clone();
        self.it.host_scope(|it| {
            let styles = dom.cascade_now(it)?;
            let (layout, areas, bars) = dom.layout_areas(it, &styles, None)?;
            // A page that got shorter (or a box that is not a container now) puts its offsets back in range.
            dom.clamp_scroll(&styles, &layout, &areas, bars);
            Ok(Frame { styles, layout, areas, bars, since: None })
        })
    }

    /// The cascade of a window frame, with no layout. It keeps what `refresh` needs, and the document logs its changes from now on.
    pub fn restyle(&mut self) -> R<std::rc::Rc<bare_css::cascade::Styles>> {
        let dom = self.dom.clone();
        let styles = self.it.host_scope(|it| dom.cascade_frame(it))?;
        let mut doc = dom.doc.borrow_mut();
        doc.track_damage(bare_html::dom::DAMAGE_FRAME);
        Ok(styles)
    }

    /// The window frame with `styles`, the cascade of the page now (`restyle`). Its layout keeps what the next `refresh` can use again.
    /// The new layout writes in the memory of `old`, the frame that this one replaces.
    pub fn render_with(&mut self, styles: std::rc::Rc<bare_css::cascade::Styles>, old: Option<Frame>) -> R<Frame> {
        let dom = self.dom.clone();
        let since = Some((dom.rule_builds(), chains(&dom.doc.borrow())));
        let kept = old.and_then(|f| {
            let mut k = f.layout.kept?;
            k.recycle(f.layout.fragments);
            Some(k)
        });
        self.it.host_scope(|it| {
            let (layout, areas, bars) = dom.layout_areas(it, &styles, Some((kept, None)))?;
            dom.clamp_scroll(&styles, &layout, &areas, bars);
            Ok(Frame { styles, layout, areas, bars, since })
        })
    }

    /// The window frame `f` keeps `styles` (a new `restyle`), with the same layout: the new styles differ from the old ones only in
    /// what layout does not read. `refresh` can update it after this.
    pub fn restyle_frame(&mut self, f: &mut Frame, styles: std::rc::Rc<bare_css::cascade::Styles>) {
        f.styles = styles;
        f.since = Some((self.dom.rule_builds(), chains(&self.dom.doc.borrow())));
    }

    /// Update the window frame `f` for the changes since it was made (or last updated), and say what changed: `Same`, `Paint` (styles
    /// that layout does not read: the layout stays) or `Layout`. Only the elements that the changes can reach are styled again
    /// (`Dom::restyle_frame`), and layout lays out again only the changed boxes and their ancestors (`bare_layout::layout_kept`).
    /// `None` when it cannot: `f` is as it was, and the caller makes a new frame. On an error `f` is not usable.
    pub fn refresh(&mut self, f: &mut Frame) -> R<Option<Change>> {
        let Some((builds, old)) = f.since.take() else { return Ok(None) };
        let dom = self.dom.clone();
        let Some(marks) = dom.doc.borrow_mut().take_damage(bare_html::dom::DAMAGE_FRAME) else { return Ok(None) };
        self.it.host_scope(|it| {
            let new = chains(&dom.doc.borrow());
            let Some(changed) = dom.restyle_frame(it, &mut f.styles, builds, &marks, &old, &new)? else { return Ok(None) };
            f.since = Some((builds, new));
            let doc = dom.doc.borrow();
            // The boxes that layout must lay out again: the changed ones and all their ancestors.
            let mut dirty: Vec<bool> = Vec::new();
            let mut seed = |n: NodeId| {
                if dirty.is_empty() {
                    dirty.resize(doc.len(), false);
                }
                let mut cur = Some(n);
                while let Some(c) = cur.filter(|c| !dirty[c.index()]) {
                    dirty[c.index()] = true;
                    cur = doc.parent(c);
                }
            };
            for m in &marks {
                match *m {
                    // A node that moved inside its parent is laid out again too.
                    Mark::Link { parent, node } => {
                        seed(node);
                        seed(parent);
                    }
                    Mark::Unlink { parent, .. } => seed(parent),
                    Mark::Text { node } => seed(node),
                    Mark::Attr { el, .. } => seed(el),
                    // Only selectors read the form state.
                    Mark::Ui { .. } => {}
                    Mark::Image { el } => seed(el),
                }
            }
            let mut out = Change::Same;
            for &(n, old) in &changed {
                match f.styles.change_of(old, n) {
                    Change::Layout => seed(n),
                    c => out = out.max(c),
                }
            }
            if dirty.is_empty() {
                return Ok(Some(out));
            }
            drop(doc);
            let mut kept = f.layout.kept.take();
            if let Some(k) = &mut kept {
                k.recycle(std::mem::take(&mut f.layout.fragments));
            }
            (f.layout, f.areas, f.bars) = dom.layout_areas(it, &f.styles, Some((kept, Some(&dirty))))?;
            dom.clamp_scroll(&f.styles, &f.layout, &f.areas, f.bars);
            Ok(Some(Change::Layout))
        })
    }

    /// What the styles and the layout of `render` are made from, the scroll offsets left out: the tree and its text, the window
    /// size, the supplied sheets, and the user action and form state that the rules read. Two renders with the same key give the same
    /// frame. The form state is out of the tree part: a checkbox click with no `:checked` rule keeps the layout.
    pub fn render_key(&self) -> [u64; 8] {
        let dom = &self.dom;
        let doc = dom.doc.borrow();
        let vp = dom.plat.s.borrow().viewport;
        let (rules, user) = (dom.user_rules(), &doc.user().revision);
        let used = |bit: u8, i: usize| if rules & bit != 0 { user[i] } else { 0 };
        use bare_css::selector::{USES_ACTIVE, USES_FOCUS, USES_FORM, USES_HOVER};
        let vp = u64::from(vp.0.to_bits()) << 32 | u64::from(vp.1.to_bits());
        let form = if rules & USES_FORM != 0 { doc.ui_revision() } else { 0 };
        [doc.revision() - doc.ui_revision(), doc.text_revision(), vp, dom.computed.sheet_gen.get(), used(USES_HOVER, 0), used(USES_ACTIVE, 1), used(USES_FOCUS, 2), form]
    }

    pub(crate) fn ui_call(&mut self, always: bool, f: impl FnOnce(&Dom, &mut Interp<'_>) -> R<()>) -> R<UiOutcome> {
        let before = self.dom.st.borrow().ui.epoch;
        self.dom.st.borrow_mut().ui.active = true;
        let r = self.driven(f);
        self.dom.st.borrow_mut().ui.active = false;
        let (_, reports) = r?;
        // A key that only scrolls the page changes no node: its offsets are not the page (see `ui_key`).
        if always && !self.dom.scroll_key_set(false) {
            self.dom.bump();
        }
        // The events are freed by `collect`: keep room for the next call (the cap is `max_events`). A call that runs many events (a typed
        // text) also collects when the heap has grown to twice its live size (`gc_due`), so its garbage does not pile up.
        if self.dom.st.borrow().events.len() > self.dom.lim.max_events / 2 || self.it.gc_due() {
            self.collect(&[])?;
        }
        Ok(UiOutcome { reports, changed: self.dom.st.borrow().ui.epoch != before, intercepted: None })
    }

    /// The pointer moved over `target` (the element under it, from hit testing). `None`: no element.
    pub fn ui_pointer_move(&mut self, target: Option<NodeId>, p: Pointer) -> R<UiOutcome> {
        self.ui_call(false, |d, it| d.ui_move(it, target, &p))
    }

    /// Send the `scroll` events that wait (one for each target that moved).
    pub fn ui_scroll_events(&mut self) -> R<UiOutcome> {
        self.ui_call(false, |d, it| d.flush_scroll(it))
    }

    /// The pointer did not move, but the page under it changed: `to` is under it now.
    pub fn ui_pointer_settle(&mut self, to: Option<NodeId>, p: Pointer) -> R<UiOutcome> {
        self.ui_call(false, |d, it| d.ui_settle(it, to, &p))
    }

    /// The pointer left the window.
    pub fn ui_pointer_leave(&mut self, p: Pointer) -> R<UiOutcome> {
        self.ui_call(false, |d, it| d.ui_leave(it, &p))
    }

    /// A button went down or up over `target`. `p.buttons` is the mask after the event. `caret` is the UTF-16 offset
    /// for a press in a text field (the caller measures the text).
    pub fn ui_pointer_button(&mut self, target: Option<NodeId>, p: Pointer, button: u8, pressed: bool, caret: Option<u32>) -> R<UiOutcome> {
        self.ui_call(true, |d, it| d.ui_button(it, target, &p, button, pressed, caret))
    }

    /// A key went down or up.
    pub fn ui_key(&mut self, k: KeyInput, down: bool) -> R<UiOutcome> {
        // Only a key down and the Space key up have a default action that may change the page. A listener that runs says so itself.
        self.dom.scroll_key_set(true);
        let always = down || k.code == "Space";
        self.ui_call(always, |d, it| d.ui_key(it, &k, down))
    }

    /// The text field whose text a drag with the button down selects now, if any.
    pub fn selecting(&self) -> Option<NodeId> {
        self.dom.st.borrow().ui.sel_drag.map(|d| d.0)
    }

    /// The pointer moved to `caret` of the text field that `selecting` gave, with the button down.
    pub fn ui_drag_select(&mut self, caret: u32) -> R<UiOutcome> {
        self.ui_call(true, |d, it| d.drag_select(it, caret))
    }

    /// Is `node` an `<input>` that takes text and that the user can edit?
    pub fn is_text_input(&self, node: NodeId) -> bool {
        self.dom.editable(node)
    }

    /// The tab order now, for tests and for a caller that draws a focus ring.
    pub fn tab_order(&mut self) -> R<Vec<NodeId>> {
        let dom = self.dom.clone();
        self.it.host_scope(|it| dom.tab_order(it))
    }
}

/// Where a double click at offset `c` of `text` selects (UTF-16 offsets): the run of letters, digits and `_`, or the run of spaces,
/// that holds the character after `c` (the one before it at the end). Any other character stands alone (Chrome).
fn word_at(text: &str, c: u32) -> (u32, u32) {
    let (mut cs, mut u) = (Vec::new(), 0);
    for ch in text.chars() {
        cs.push((u, ch));
        u += ch.len_utf16() as u32;
    }
    let class = |ch: char| match ch {
        '\n' => 0,
        ch if ch.is_alphanumeric() || ch == '_' => 1,
        ch if ch.is_whitespace() => 2,
        _ => 0,
    };
    let Some(i) = cs.iter().position(|&(s, ch)| s + ch.len_utf16() as u32 > c).or(cs.len().checked_sub(1)) else { return (0, 0) };
    let k = class(cs[i].1);
    let (mut a, mut b) = (i, i + 1);
    if k != 0 {
        while a > 0 && class(cs[a - 1].1) == k {
            a -= 1;
        }
        while b < cs.len() && class(cs[b].1) == k {
            b += 1;
        }
    }
    (cs[a].0, cs.get(b).map_or(u, |x| x.0))
}

/// Where a triple click at offset `c` selects: the whole value, or the line of a textarea.
fn line_at(text: &str, c: u32, area: bool) -> (u32, u32) {
    let u: Vec<u16> = text.encode_utf16().collect();
    if !area {
        return (0, u.len() as u32);
    }
    let c = (c as usize).min(u.len());
    let a = u[..c].iter().rposition(|&x| x == 10).map_or(0, |i| i + 1);
    let b = u[c..].iter().position(|&x| x == 10).map_or(u.len(), |i| c + i);
    (a as u32, b as u32)
}
