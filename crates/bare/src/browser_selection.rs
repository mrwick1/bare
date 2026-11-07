//! Text selection of `<input>` and `<textarea>`: `selectionStart`, `selectionEnd`, `selectionDirection`, `setSelectionRange`, `select`,
//! `setRangeText`, and the `select` and `selectionchange` events.
//! Source: HTML Standard, "APIs for the text control selections", then compared with headless Chrome 155 (the platform rule of the
//! standard lets a browser without directionless selections use `forward`, so the behavior is Chrome's, see `docs/decisions.md`).
//! Chrome files: `tools/fixtures/ui/setrange-cases.txt`, `selmodel-cases.txt` (topic cases) and `sel-steps.txt` (events and mouse, on `sel.html`).
//!
//! Model
//! - The selection is native state (`Input::sel`) of the same native node as the value. A wrapper that the collector
//!   freed gets the same state back when it is made again. Offsets are UTF-16 code units, like JS string indexes
//!   (an offset inside a surrogate pair is valid).
//! - `Input::sel == None` is a fresh control: caret at 0, direction `forward`. The sentinel `END` means "caret at the end" without a scan of
//!   the value: a `value` that differs from the old one stores it, and so does a form reset that drops a dirty value. A stored selection is
//!   clamped to the current value when it is read (`within`). The hooks are in `put_input_value` and `reset_control`, and in `cloneNode`
//!   (the clone does not copy the selection).
//! - The direction is `forward` or `backward`; Chrome never gives `none`, so the argument `none` and any other string is `forward`. A caret
//!   on the focused control is always `forward`; an unfocused caret keeps `backward`.
//! - The page selection has one owner (`FocusState::sel_owner`): the last text control that was focused. A press on a button or link keeps it;
//!   a press on a non-focusable element drops it. Focusing a text control that is not the owner queues its `selectionchange`.
//! - Applies to the input types text, search, tel, url and password (an unknown `type` is text), and to a textarea. For the other
//!   types the three getters give `null`, `setSelectionRange` and the three setters throw `InvalidStateError`, and
//!   `select()` returns without effect. The applicability is read when the algorithm runs, after the arguments are
//!   converted (a conversion may run JS that changes the type or the value).
//! - Conversions are WebIDL: `unsigned long` is ToUint32 (so -1 is 4294967295 and Infinity is 0, a fraction is cut),
//!   `selectionStart`/`selectionEnd` are `unsigned long?` (null and undefined are null, read as 0 by the algorithm),
//!   `setSelectionRange` needs two arguments (`TypeError` otherwise). They run before any state is read, and a throw changes nothing.
//! - `setRangeText(r)` and `setRangeText(r, start, end, mode)` (`set_range_text`): the conversions run in the order replacement, start, end, mode;
//!   then the type check (`InvalidStateError`), `IndexSizeError` for start > end, clamping to the length, the splice, and the new selection by
//!   the mode (`select`, `start`, `end`, `preserve`). The value becomes dirty, no `input` or `change` fires, and the direction is `forward`.
//! - `select()` focuses the control first (also a disabled one), then selects.
//! - `select` event: fired at the control (bubbles, trusted, not cancelable) when a change moves the range or the direction. For script
//!   (`setSelectionRange`, the setters, `select()`, `setRangeText`) it is a `Task::Select` due just before the next frame (the grid of
//!   `browser_frame.rs`), one for each change with no coalescing. For the user (keys, mouse) it is due now and only for a non-collapsed range.
//!   Typing, Backspace and caret moves give only `selectionchange`. A press that focuses a control without text gives `select` to the owner
//!   again when its range is not collapsed (Chrome).
//! - `selectionchange` event: a `Task::SelectionChange` due now, fired at the control (bubbles, trusted), coalesced for each control by
//!   `Input::sc_pending`; the flag is cleared before the event, so a listener's own change queues another. It follows every change of the
//!   extent or the direction, the `value` setter that moves the caret included; `defaultValue` and attributes give none. A key's `select` goes
//!   before it, the `select` of a mouse selection after it. A detached control gets no event.
//! - Mouse (`browser_ui.rs`, `focus_by_press`): a press in a text field puts the caret at the point, a move with the button down
//!   extends the selection (`backward` when the pointer is before the press), two presses select the word at the caret (`word_at`),
//!   three select the value (the line of a textarea). A press more than 5px from the last one starts the count again.
//! - Caps: `SchedLimits::max_select_tasks` (`Limit(Host("select-tasks"))`) and `max_selectionchange_tasks`
//!   (`selectionchange-tasks`). `selection_room` checks every cap a change needs before anything is
//!   queued or written, so a refused call changes nothing. No task runs after a fatal pump error (the pump is
//!   sticky). A listener that throws is reported, as for any event. Tasks hold a node id, not a JS value: they need
//!   no root, and the listeners keep their own roots.
//! - Cost: the value is never copied. `effective` charges `bytes / 256` steps of the JS caller, then scans the
//!   borrowed value once for its UTF-16 length. Each operation measures a value once (a value change that differs
//!   measures the old and the new). The no-JS driver (`Browser::input_selection`, `type_text`) has no budget; its scan
//!   is bounded by `max_text_bytes`.
//! - Namespace seam: `text_control` is the only HTML-input test of this file's logic.
//!
//! Gaps (none is faked)
//! - The document-level `selectionchange` of the Range and Selection API is in `browser_range.rs`. That selection is not linked with this one:
//!   Chrome sends a text control's change only to the control (it bubbles), and `getSelection()` while a control has a selection differs.
//!   `onselectionchange` (and every other event handler property) is absent.
//! - A `type` change keeps the stored selection (Chrome resets it). `setAttribute('value')` clamps the selection when read (Chrome does not
//!   clamp it). The `value` of `date`, `color` and `file` inputs is not sanitized as in Chrome, so `setRangeText` on them differs.
//! - A drag that goes on after the pointer left the window, a drag by word or line after a double or triple click, and a drag that starts in
//!   the text of a textarea scrolled by the page are not done.

use crate::browser::*;
use crate::browser_dom::{arg, attr_set, input_kind};
use bare_html::dom::{Document, NodeData, NodeId};
use bare_js::eval::{Error, Interp};
use bare_js::value::{Value, to_uint32};

type V = R<Value>;

/// `none` is read and stored as `forward`: Chrome on Linux has no directionless selection (`docs/decisions.md`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum Dir {
    #[default]
    Forward,
    Backward,
}

impl Dir {
    fn name(self) -> &'static str {
        match self {
            Dir::Forward => "forward",
            Dir::Backward => "backward",
        }
    }
}

/// A selection in UTF-16 offsets, `start <= end`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Sel {
    pub start: u32,
    pub end: u32,
    pub dir: Dir,
}

/// Length in UTF-16 units (saturated). The only scan of a value in this file.
fn utf16_len(s: &str) -> u32 {
    #[cfg(test)]
    SCANS.with(|c| c.set(c.get() + 1));
    s.encode_utf16().count().min(u32::MAX as usize) as u32
}

#[cfg(test)]
thread_local!(static SCANS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) });

/// Test probe: how many value scans this thread ran.
#[cfg(test)]
pub(crate) fn scans() -> usize {
    SCANS.with(|c| c.get())
}

/// The one place that says "this node is an HTML `<input>` or `<textarea>`": `Some(false)` for an HTML `<input>`, `Some(true)` for an HTML
/// `<textarea>`, else `None` (one compare of the name). Selection applies to nothing else. `Dom::with_input_value` and
/// `Dom::selection_applies` both go through it. (`input_el`, `driver_input` and `Dom::input_value_of`'s callers keep their own `input`
/// name tests; they are outside this file.)
pub(crate) fn text_control(doc: &Document, n: NodeId) -> Option<bool> {
    match *doc.data(n) {
        NodeData::Element { name, .. } if !doc.is_foreign(n) => match doc.atom_str(name) {
            "input" => Some(false),
            "textarea" => Some(true),
            _ => None,
        },
        _ => None,
    }
}

/// The caret at the end of any value: a stored selection is clamped to the value when it is read, so this needs no scan.
pub(crate) const END: Sel = Sel { start: u32::MAX, end: u32::MAX, dir: Dir::Forward };

/// The caret at the end of `value`.
pub(crate) fn caret_end(value: &str) -> Sel {
    let len = utf16_len(value);
    Sel { start: len, end: len, dir: Dir::Forward }
}

/// The selection a stored state gives in a value of `len` UTF-16 units: clamped. With none stored the caret is at 0, `forward`
/// (Chrome: also for an input whose value comes from the markup or from `defaultValue`).
fn within(stored: Option<Sel>, len: u32) -> Sel {
    stored.map_or(Sel { start: 0, end: 0, dir: Dir::Forward }, |s| Sel { start: s.start.min(len), end: s.end.min(len), dir: s.dir })
}

impl Dom {
    /// True when selection applies to this `<input>` now (the HTML "does not apply" test).
    pub(crate) fn selection_applies(&self, n: NodeId) -> bool {
        let doc = self.doc.borrow();
        self.applies(&doc, n).is_some()
    }

    /// `Some(is a textarea)` when selection applies to `n`, else `None`.
    fn applies(&self, doc: &Document, n: NodeId) -> Option<bool> {
        match text_control(doc, n)? {
            true => Some(true),
            false => matches!(input_kind(doc, n), "text" | "search" | "tel" | "url" | "password").then_some(false),
        }
    }

    fn stored_sel(&self, n: NodeId) -> Option<Sel> {
        self.st.borrow().inputs.get(&n).and_then(|i| i.sel)
    }

    /// The selection as JS reads it and the value length in UTF-16 units. `None` when it does not apply.
    /// The value is borrowed, never copied. A JS caller (`it`) pays `bytes / 256` steps before the one scan, so a
    /// huge value under a small budget fails first. The no-JS driver (`it` is `None`) has no budget to charge: its
    /// scan is bounded by `max_text_bytes`, which caps both a dirty value and an attribute.
    pub(crate) fn effective(&self, it: Option<&mut Interp<'_>>, n: NodeId) -> R<Option<(Sel, u32)>> {
        if self.applies(&self.doc.borrow(), n).is_none() {
            return Ok(None);
        }
        let Some(bytes) = self.with_input_value(n, str::len) else { return Ok(None) };
        if let Some(it) = it {
            it.charge((bytes / 256) as u64)?;
        }
        let Some(len) = self.with_input_value(n, utf16_len) else { return Ok(None) };
        Ok(Some((within(self.stored_sel(n), len), len)))
    }

    /// The range that typing replaces. A field that selection does not apply to (`email`, `number`) still has one inside, kept only
    /// here: the caret at the end, or all of the text after `select_text`. `None`: not a field that the user can edit.
    pub(crate) fn edit_sel(&self, n: NodeId) -> R<Option<Sel>> {
        if let Some((s, _)) = self.effective(None, n)? {
            return Ok(Some(s));
        }
        let Some(len) = self.with_input_value(n, utf16_len) else { return Ok(None) };
        Ok(Some(within(Some(self.stored_sel(n).unwrap_or(END)), len)))
    }

    /// Put the caret at `at` (UTF-16 units) in a field, or select all its text when `at` is `None`. Works for the fields of `edit_sel`.
    pub(crate) fn select_text(&self, it: &mut Interp<'_>, n: NodeId, at: Option<u32>) -> R<()> {
        if self.selection_applies(n) {
            return self.set_user_selection(it, n, Some(at.unwrap_or(0)), Some(at.unwrap_or(u32::MAX)), Dir::Forward);
        }
        self.st.borrow_mut().inputs.entry(n).or_default().sel = Some(Sel { start: at.unwrap_or(0), end: at.unwrap_or(u32::MAX), dir: Dir::Forward });
        Ok(())
    }

    /// Would the value `new` move the selection of this text control? Used before a value write. Pays for the
    /// scan of the old value and of `new` (see `effective`). False when selection does not apply.
    pub(crate) fn selection_after_value(&self, mut it: Option<&mut Interp<'_>>, n: NodeId, new: &str) -> R<bool> {
        let Some((old, _)) = self.effective(it.as_deref_mut(), n)? else { return Ok(false) };
        if let Some(it) = it {
            it.charge((new.len() / 256) as u64)?;
        }
        Ok(old != caret_end(new))
    }

    /// Focus moved into `n`. A text control that selection applies to takes the page selection, and its caret has no direction. When the page
    /// selection was elsewhere (nowhere, the document, or another control), Chrome sends `selectionchange` to it. Focusing a button or a link
    /// moves nothing.
    pub(crate) fn focus_selection(&self, n: NodeId) -> R<()> {
        if !self.selection_applies(n) {
            return Ok(());
        }
        let moved = self.st.borrow().focus.sel_owner != Some(n);
        if moved {
            self.selection_room(n, true, false)?;
            self.queue_selection_tasks(n, true, false, false)?;
        }
        let mut st = self.st.borrow_mut();
        st.focus.sel_owner = Some(n);
        if let Some(s) = st.inputs.get_mut(&n).and_then(|i| i.sel.as_mut()).filter(|s| s.start == s.end) {
            s.dir = Dir::Forward;
        }
        Ok(())
    }

    /// `defaultValue = s`: the `value` attribute. A not-dirty input shows it, so the selection read in it can move
    /// (the caret at the end follows the length, a stored range is clamped). A dirty input shows its own value.
    pub(crate) fn set_value_attr(&self, it: &mut Interp<'_>, n: NodeId, s: &str) -> R<()> {
        let dirty = self.st.borrow().inputs.get(&n).is_some_and(|i| i.value.is_some());
        let mut moved = false;
        if !dirty && let Some((old, _)) = self.effective(Some(&mut *it), n)? {
            it.charge((s.len() / 256) as u64)?;
            moved = within(self.stored_sel(n), utf16_len(s)) != old;
        }
        self.selection_room(n, moved, false)?;
        attr_set(self, it, n, "value", s)?;
        self.queue_selection_tasks(n, moved, false, false)
    }

    /// "Set the selection range" (the start is clamped after the end, as in the spec). When the range or the
    /// direction changed, queues `selectionchange` (coalesced) and `select`. Every cap is checked before the state
    /// changes. The caller has checked `selection_applies` after its conversions. Also the entry for a later focus or
    /// driver path.
    pub(crate) fn set_selection_range(&self, it: &mut Interp<'_>, n: NodeId, start: Option<u32>, end: Option<u32>, dir: Dir) -> R<()> {
        self.update_selection(it, n, false, |_| (start, end, dir))
    }

    /// `set_selection_range` for a change that the user made (a key, the pointer): Chrome sends `select` only for a range, before
    /// `selectionchange`, and not at the next frame.
    pub(crate) fn set_user_selection(&self, it: &mut Interp<'_>, n: NodeId, start: Option<u32>, end: Option<u32>, dir: Dir) -> R<()> {
        self.update_selection(it, n, true, |_| (start, end, dir))
    }

    /// Like `set_selection_range`, from the current selection (one scan of the value for the read and the write).
    fn update_selection(&self, it: &mut Interp<'_>, n: NodeId, user: bool, f: impl FnOnce(Sel) -> (Option<u32>, Option<u32>, Dir)) -> R<()> {
        let Some((old, len)) = self.effective(Some(it), n)? else { return Ok(()) };
        let (start, end, dir) = f(old);
        let end = end.unwrap_or(0).min(len);
        let start = start.unwrap_or(0).min(end);
        // The caret of the focused control has no direction (Chrome).
        let dir = if start == end && self.st.borrow().focus.node == Some(n) { Dir::Forward } else { dir };
        let new = Sel { start, end, dir };
        let changed = new != old;
        let select = changed && (!user || start != end);
        self.selection_room(n, changed, select)?;
        self.queue_selection_tasks(n, changed, select, user)?;
        self.st.borrow_mut().inputs.entry(n).or_default().sel = Some(new);
        Ok(())
    }

    fn require_selection(&self, it: &mut Interp<'_>, n: NodeId) -> R<()> {
        if self.selection_applies(n) {
            return Ok(());
        }
        let kind = input_kind(&self.doc.borrow(), n);
        Err(self.dom_err(it, "InvalidStateError", &format!("The input element's type ('{kind}') does not support selection.")))
    }

    /// WebIDL `unsigned long?`.
    fn nullable_ulong(&self, it: &mut Interp<'_>, v: Value) -> R<Option<u32>> {
        match v {
            Value::Null | Value::Undefined => Ok(None),
            _ => Ok(Some(to_uint32(it.to_js_number(v)?))),
        }
    }

    /// The direction string: `forward` or `backward` exactly, else `none`. A null or undefined is `none`.
    fn direction_arg(&self, it: &mut Interp<'_>, v: Value) -> R<Dir> {
        if matches!(v, Value::Null | Value::Undefined) {
            return Ok(Dir::Forward);
        }
        let s = it.to_js_string(v)?;
        let len = it.heap.str_len(s).map_err(Error::Heap)?;
        if len != 8 {
            return Ok(Dir::Forward);
        }
        Ok(match it.heap.to_string_lossy(s).map_err(|e| it.heap_error(e))?.as_str() {
            "backward" => Dir::Backward,
            _ => Dir::Forward,
        })
    }

    /// Fire `select` or `selectionchange` (both bubble, neither is cancelable) at the input when its task runs.
    pub(crate) fn fire_at_input(&self, it: &mut Interp<'_>, n: NodeId, ty: &str) -> R<()> {
        self.fire_selection_event(it, n, ty, true)
    }

    /// Fire `selectionchange` at the document (it does not bubble), for a change of the Range and Selection API.
    pub(crate) fn fire_at_document(&self, it: &mut Interp<'_>, ty: &str) -> R<()> {
        let root = self.doc.borrow().root();
        self.fire_selection_event(it, root, ty, false)
    }

    /// Nothing is made when no listener is on the event path. A control that is not in the document gets its event too (Chrome), and so do
    /// the nodes above it; the window is on the path only when the node is in the document.
    fn fire_selection_event(&self, it: &mut Interp<'_>, n: NodeId, ty: &str, bubbles: bool) -> R<()> {
        let listens = {
            let (doc, st) = (self.doc.borrow(), self.st.borrow());
            let on = |t: Target| st.listeners.get(&t).is_some_and(|v| v.iter().any(|l| &*l.ty == ty));
            let mut cur = n;
            let mut found = on(Target::Node(cur));
            while let Some(p) = doc.parent(cur) {
                it.charge(1)?;
                cur = p;
                found |= on(Target::Node(cur));
            }
            found || (cur == doc.root() && on(Target::Window))
        };
        if !listens {
            return Ok(());
        }
        let ev = self.new_event(it, ty, bubbles, false, false, None)?;
        self.st.borrow_mut().events.get_mut(&ev).expect("just made").trusted = true;
        self.dispatch(it, Target::Node(n), ev).map(|_| ())
    }
}

/// `this` of a selection accessor: an `<input>` or a `<textarea>`.
fn input_el(d: &Dom, it: &mut Interp<'_>, this: Value) -> R<NodeId> {
    let n = d.el(it, this)?;
    let ok = text_control(&d.doc.borrow(), n).is_some();
    if ok { Ok(n) } else { Err(it.type_error("Illegal invocation")) }
}

fn with_selection(d: &Dom, it: &mut Interp<'_>, this: Value) -> R<Option<Sel>> {
    let n = input_el(d, it, this)?;
    Ok(d.effective(Some(it), n)?.map(|x| x.0))
}

pub(crate) fn selection_start(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    Ok(with_selection(d, it, this)?.map_or(Value::Null, |s| Value::Number(s.start as f64)))
}

pub(crate) fn selection_end(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    Ok(with_selection(d, it, this)?.map_or(Value::Null, |s| Value::Number(s.end as f64)))
}

pub(crate) fn selection_direction(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    match with_selection(d, it, this)? {
        Some(s) => d.js_str(it, s.dir.name()),
        None => Ok(Value::Null),
    }
}

pub(crate) fn set_selection_start(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = input_el(d, it, this)?;
    let new = d.nullable_ulong(it, arg(a, 0))?;
    d.require_selection(it, n)?;
    let start = new.unwrap_or(0);
    d.update_selection(it, n, false, |cur| (Some(start), Some(cur.end.max(start)), cur.dir))?;
    Ok(Value::Undefined)
}

pub(crate) fn set_selection_end(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = input_el(d, it, this)?;
    let new = d.nullable_ulong(it, arg(a, 0))?;
    d.require_selection(it, n)?;
    d.update_selection(it, n, false, |cur| (Some(cur.start), new, cur.dir))?;
    Ok(Value::Undefined)
}

pub(crate) fn set_selection_direction(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = input_el(d, it, this)?;
    let dir = d.direction_arg(it, arg(a, 0))?;
    d.require_selection(it, n)?;
    d.update_selection(it, n, false, |cur| (Some(cur.start), Some(cur.end), dir))?;
    Ok(Value::Undefined)
}

pub(crate) fn set_selection_range_method(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = input_el(d, it, this)?;
    if a.len() < 2 {
        return Err(it.type_error(&format!("Failed to execute 'setSelectionRange': 2 arguments required, but only {} present.", a.len())));
    }
    let start = to_uint32(it.to_js_number(a[0])?);
    let end = to_uint32(it.to_js_number(a[1])?);
    let dir = d.direction_arg(it, arg(a, 2))?;
    d.require_selection(it, n)?;
    d.set_selection_range(it, n, Some(start), Some(end), dir)?;
    Ok(Value::Undefined)
}

pub(crate) fn select_method(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let n = input_el(d, it, this)?;
    if d.selection_applies(n) {
        // Chrome focuses the control first (a control that cannot take focus is still selected).
        d.focus_node(it, n)?;
        d.set_selection_range(it, n, Some(0), Some(u32::MAX), Dir::Forward)?;
    }
    Ok(Value::Undefined)
}

/// `setRangeText(replacement)` and `setRangeText(replacement, start, end, selectMode)` of an `<input>` (`area` false) and a `<textarea>`.
/// HTML "setRangeText"; the argument rules, the errors and the events are those of Chrome 155 (`tools/fixtures/ui/setrange-cases.txt`).
/// The value is dirty after it, and no `input` or `change` event is sent. The conversions run first, then the type is tested (a conversion
/// may run JS that changes it). The new selection is a change like any other: `selectionchange` and `select` when the range or the
/// direction differs from the one before. The direction after is `forward`.
fn set_range_text(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value], area: bool) -> V {
    let n = d.el(it, this)?;
    if text_control(&d.doc.borrow(), n) != Some(area) {
        return Err(it.type_error("Illegal invocation"));
    }
    let iface = if area { "HTMLTextAreaElement" } else { "HTMLInputElement" };
    let fail = |it: &mut Interp<'_>, m: &str| it.type_error(&format!("Failed to execute 'setRangeText' on '{iface}': {m}"));
    match a.len() {
        0 => return Err(fail(it, "1 argument required, but only 0 present.")),
        2 => return Err(fail(it, "Overload resolution failed.")),
        _ => {}
    }
    let rep = d.string_arg(it, a[0])?;
    let range = if a.len() > 2 { Some((to_uint32(it.to_js_number(a[1])?), to_uint32(it.to_js_number(a[2])?))) } else { None };
    let mode = match arg(a, 3) {
        Value::Undefined => 3,
        v => {
            let m = d.string_arg(it, v)?;
            match ["select", "start", "end", "preserve"].iter().position(|k| *k == m) {
                Some(i) => i,
                None => return Err(fail(it, &format!("The provided value '{m}' is not a valid enum value of type SelectionMode."))),
            }
        }
    };
    if !d.selection_applies(n) {
        let kind = input_kind(&d.doc.borrow(), n);
        return Err(d.dom_err(it, "InvalidStateError", &format!("Failed to execute 'setRangeText' on '{iface}': The input element's type ('{kind}') does not support selection.")));
    }
    let Some((old, len)) = d.effective(Some(it), n)? else { return Ok(Value::Undefined) };
    let (start, end) = range.unwrap_or((old.start, old.end));
    if start > end {
        return Err(d.dom_err(it, "IndexSizeError", &format!("Failed to execute 'setRangeText' on '{iface}': The provided start value ({start}) is larger than the provided end value ({end}).")));
    }
    let (start, end) = (start.min(len), end.min(len));
    let value = d.input_value_of(n).unwrap_or_default();
    it.charge(((value.len() + rep.len()) / 256) as u64)?;
    let mut units: Vec<u16> = value.encode_utf16().collect();
    let new_len = rep.encode_utf16().count() as u32;
    units.splice(start as usize..end as usize, rep.encode_utf16());
    let text = String::from_utf16_lossy(&units);
    let text = if area { crate::browser_textarea::api_value(&text).into_owned() } else { crate::browser_controls::sanitize(input_kind(&d.doc.borrow(), n), false, &text).into_owned() };
    // HTML: the selection in "preserve" mode moves with the text that is replaced.
    let delta = new_len as i64 - (end - start) as i64;
    let (ns, ne) = match mode {
        0 => (start, start.saturating_add(new_len)),
        1 => (start, start),
        2 => (start.saturating_add(new_len), start.saturating_add(new_len)),
        _ => {
            let p = |x: u32, inside: u32| if x > end { (x as i64 + delta).clamp(0, u32::MAX as i64) as u32 } else if x > start { inside } else { x };
            (p(old.start, start), p(old.end, start.saturating_add(new_len)))
        }
    };
    let new = Sel { start: ns, end: ne, dir: Dir::Forward };
    let changed = new != old;
    d.selection_room(n, changed, changed)?;
    d.write_value(n, text, false)?;
    d.queue_selection_tasks(n, changed, changed, false)?;
    d.st.borrow_mut().inputs.entry(n).or_default().sel = Some(new);
    Ok(Value::Undefined)
}

pub(crate) fn set_range_text_input(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    set_range_text(d, it, this, a, false)
}

pub(crate) fn set_range_text_area(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    set_range_text(d, it, this, a, true)
}

impl Browser {
    /// The native selection of an `<input>`: `(start, end, direction)` in UTF-16 offsets, read without JS. `None`
    /// when the node is not an `<input>` or selection does not apply to its type.
    pub fn input_selection(&self, input: NodeId) -> Option<(u32, u32, &'static str)> {
        // No JS caller, so no step budget: see `Dom::effective` for the bound.
        self.dom.effective(None, input).ok().flatten().map(|(s, _)| (s.start, s.end, s.dir.name()))
    }
}
