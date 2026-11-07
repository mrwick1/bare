//! Text selection of `<input>` and `<textarea>`: `selectionStart`, `selectionEnd`, `selectionDirection`, `setSelectionRange`, `select`.
//! Source: HTML Standard, "APIs for the text control selections" (read by hand, not checked in a browser).
//!
//! Model
//! - The selection is native state (`Input::sel`) of the same native node as the value. A wrapper that the collector
//!   freed gets the same state back when it is made again. Offsets are UTF-16 code units, like JS string indexes
//!   (an offset inside a surrogate pair is valid).
//! - `Input::sel == None` means "caret at the end of the value, direction none". That is the state of an input that
//!   never had a selection set, and the state after the value changed. A stored selection is clamped to the current
//!   value when it is read, so every path that changes the value (the `value` setter, the test driver, `defaultValue`
//!   of a not-dirty input) is reflected without a hook of its own. The only hooks are in `put_input_value` (a value
//!   that differs resets the selection, as the `value` setter does in the spec) and in `cloneNode` (the clone does not
//!   copy the selection; the spec's cloning steps copy value and checkedness only).
//! - A text control that holds the page selection never has the direction `none`. It is the last focused one, also after a blur
//!   (`FocusState::sel_owner`; Chrome 155, `tools/fixtures/ui/react-steps.txt`). Its caret is `forward`. Focusing a button drops it.
//! - A `<textarea>` has the same state. With none stored it has the caret at 0 with direction `forward`; a `value` that changes puts the
//!   caret at the end (`forward`); the direction `none` is stored as `forward` (all as in Chrome, `tools/fixtures/ui/textarea-cases.txt`).
//! - Applies to the input types text, search, tel, url and password (an unknown `type` is text), and to a textarea. For the other
//!   types the three getters give `null`, `setSelectionRange` and the three setters throw `InvalidStateError`, and
//!   `select()` returns without effect. The applicability is read when the algorithm runs, after the arguments are
//!   converted (a conversion may run JS that changes the type or the value).
//! - Conversions are WebIDL: `unsigned long` is ToUint32 (so -1 is 4294967295 and Infinity is 0, a fraction is cut),
//!   `selectionStart`/`selectionEnd` are `unsigned long?` (null and undefined are null, read as 0 by the algorithm),
//!   `setSelectionRange` needs two arguments (`TypeError` otherwise) and `direction` is a string that is `forward`
//!   or `backward` exactly, else `none`. They run before any state is read, and a throw changes nothing.
//! - `select` event (HTML "set the selection range"): when a call changes the range or the direction, one task is queued
//!   in the scheduler (`Task::Select`, due now, after the tasks already queued). It fires `select` (bubbles, not
//!   cancelable) at the input and walks the ordinary dispatch. There is no coalescing (the spec queues one task for each
//!   change). The value setter and the driver queue no `select` (the HTML text has none there).
//! - `selectionchange` event (Selection API, "selectionchange event", "Scheduling" and "Firing"; read by hand, not
//!   checked in a browser): when the selection of an `<input>` that provides one changes in extent or direction, by
//!   `setSelectionRange`, the three setters, `select()`, a `value` that moves the caret, a driver `type_text`, or a
//!   `defaultValue` that a not-dirty input shows, a `Task::SelectionChange` is queued unless this input has one
//!   pending (`Input::sc_pending`, the spec's "has scheduled selectionchange event"). The flag is cleared when the
//!   task is taken, before the event is dispatched, so a listener's own change queues a new task. The event bubbles,
//!   is not cancelable and is fired at the input. The spec text orders it against `select` nowhere; the task for a
//!   change is queued before its `select` task (the selection changes before HTML queues `select`).
//! - Caps: `SchedLimits::max_select_tasks` (`Limit(Host("select-tasks"))`) and `max_selectionchange_tasks`
//!   (`select-tasks`, `selectionchange-tasks`). `selection_room` checks every cap a change needs before anything is
//!   queued or written, so a refused call changes nothing. No task runs after a fatal pump error (the pump is
//!   sticky). A listener that throws is reported, as for any event. Tasks hold a node id, not a JS value: they need
//!   no root, and the listeners keep their own roots.
//! - Cost: the value is never copied. `effective` charges `bytes / 256` steps of the JS caller, then scans the
//!   borrowed value once for its UTF-16 length. Each operation measures a value once (a value change that differs
//!   measures the old and the new). The no-JS driver (`Browser::input_selection`, `type_text`) has no budget; its scan
//!   is bounded by `max_text_bytes`.
//! - Namespace seam: `text_control` is the only HTML-input test of this file's logic.
//!
//! Gates and deviations (on purpose)
//! - `setRangeText`, the document-level
//!   `selectionchange` and `onselectionchange` are absent. `document.getSelection`, `Selection` and `Range` are in `browser_range.rs` and are not linked with this selection. A `selectionchange`
//!   from `setAttribute('value')`, `removeAttribute` or a `type` change that alters what the selection reads is not
//!   scheduled (only `defaultValue` is).
//! - No caret, no layout, no mouse or keyboard selection, no focus. `select()` does not focus (the spec says it
//!   does not need to); a focus worker can call `Dom::selection_applies` and `Dom::set_selection_range` later.
//! - Initial selection: the spec leaves it open. Here the caret is at the end of the value.
//! - A `type` change keeps the stored selection; it is clamped to the value when read. Not verified against the spec.
//! - Direction `none` is always available (as on Linux and macOS browsers; the spec allows `forward` instead).

use crate::browser::*;
use crate::browser_dom::{arg, attr_set, input_kind};
use bare_html::dom::{Document, NodeData, NodeId};
use bare_js::eval::{Error, Interp};
use bare_js::value::{Value, to_uint32};

type V = R<Value>;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum Dir {
    #[default]
    None,
    Forward,
    Backward,
}

impl Dir {
    fn name(self) -> &'static str {
        match self {
            Dir::None => "none",
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

/// The caret at the end of `value`, as Chrome has it after a `value` write. `forward`: the direction is `forward`, not `none`.
pub(crate) fn caret_end(value: &str, forward: bool) -> Sel {
    let len = utf16_len(value);
    Sel { start: len, end: len, dir: if forward { Dir::Forward } else { Dir::None } }
}

/// The direction Chrome reports. A textarea, and a control that holds the page selection (`owned`: it was focused last), has no
/// direction `none`, and a caret of the latter is always `forward`.
fn dir_of(dir: Dir, collapsed: bool, area: bool, owned: bool) -> Dir {
    if dir == Dir::None && (area || owned) || owned && collapsed { Dir::Forward } else { dir }
}

/// The selection a stored state gives in a value of `len` UTF-16 units: clamped. With none stored an input has the caret at the end
/// and a textarea has it at the start (Chrome: a textarea starts at 0, `forward`).
fn within(stored: Option<Sel>, len: u32, area: bool, owned: bool) -> Sel {
    match stored {
        Some(s) => {
            let (start, end) = (s.start.min(len), s.end.min(len));
            Sel { start, end, dir: dir_of(s.dir, start == end, area, owned) }
        }
        None if area => Sel { start: 0, end: 0, dir: Dir::Forward },
        None => Sel { start: len, end: len, dir: if owned { Dir::Forward } else { Dir::None } },
    }
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

    /// The stored selection of `n`, and whether `n` holds the page selection (it was focused last).
    fn stored_sel(&self, n: NodeId) -> (Option<Sel>, bool) {
        let st = self.st.borrow();
        (st.inputs.get(&n).and_then(|i| i.sel), st.focus.sel_owner == Some(n))
    }

    /// The selection as JS reads it and the value length in UTF-16 units. `None` when it does not apply.
    /// The value is borrowed, never copied. A JS caller (`it`) pays `bytes / 256` steps before the one scan, so a
    /// huge value under a small budget fails first. The no-JS driver (`it` is `None`) has no budget to charge: its
    /// scan is bounded by `max_text_bytes`, which caps both a dirty value and an attribute.
    pub(crate) fn effective(&self, it: Option<&mut Interp<'_>>, n: NodeId) -> R<Option<(Sel, u32)>> {
        let Some(area) = self.applies(&self.doc.borrow(), n) else { return Ok(None) };
        let Some(bytes) = self.with_input_value(n, str::len) else { return Ok(None) };
        if let Some(it) = it {
            it.charge((bytes / 256) as u64)?;
        }
        let Some(len) = self.with_input_value(n, utf16_len) else { return Ok(None) };
        let (stored, owned) = self.stored_sel(n);
        Ok(Some((within(stored, len, area, owned), len)))
    }

    /// The range that typing replaces. A field that selection does not apply to (`email`, `number`) still has one inside, kept only
    /// here: the caret at the end, or all of the text after `select_text`. `None`: not a field that the user can edit.
    pub(crate) fn edit_sel(&self, n: NodeId) -> R<Option<Sel>> {
        if let Some((s, _)) = self.effective(None, n)? {
            return Ok(Some(s));
        }
        let Some(len) = self.with_input_value(n, utf16_len) else { return Ok(None) };
        Ok(Some(within(self.stored_sel(n).0, len, false, false)))
    }

    /// Put the caret at `at` (UTF-16 units) in a field, or select all its text when `at` is `None`. Works for the fields of `edit_sel`.
    pub(crate) fn select_text(&self, it: &mut Interp<'_>, n: NodeId, at: Option<u32>) -> R<()> {
        if self.selection_applies(n) {
            return self.set_selection_range(it, n, Some(at.unwrap_or(0)), Some(at.unwrap_or(u32::MAX)), Dir::None);
        }
        self.st.borrow_mut().inputs.entry(n).or_default().sel = Some(Sel { start: at.unwrap_or(0), end: at.unwrap_or(u32::MAX), dir: Dir::None });
        Ok(())
    }

    /// Would the value `new` move the selection of this text control? Used before a value write. Pays for the
    /// scan of the old value and of `new` (see `effective`). False when selection does not apply.
    pub(crate) fn selection_after_value(&self, mut it: Option<&mut Interp<'_>>, n: NodeId, new: &str) -> R<bool> {
        let Some((old, _)) = self.effective(it.as_deref_mut(), n)? else { return Ok(false) };
        if let Some(it) = it {
            it.charge((new.len() / 256) as u64)?;
        }
        Ok(old != caret_end(new, self.doc.borrow().is_html(n, "textarea") || self.stored_sel(n).1))
    }

    /// `defaultValue = s`: the `value` attribute. A not-dirty input shows it, so the selection read in it can move
    /// (the caret at the end follows the length, a stored range is clamped). A dirty input shows its own value.
    pub(crate) fn set_value_attr(&self, it: &mut Interp<'_>, n: NodeId, s: &str) -> R<()> {
        let dirty = self.st.borrow().inputs.get(&n).is_some_and(|i| i.value.is_some());
        let mut moved = false;
        if !dirty && let Some((old, _)) = self.effective(Some(&mut *it), n)? {
            it.charge((s.len() / 256) as u64)?;
            let (stored, owned) = self.stored_sel(n);
            moved = within(stored, utf16_len(s), false, owned) != old;
        }
        self.selection_room(n, moved, false)?;
        attr_set(self, it, n, "value", s)?;
        self.queue_selection_tasks(n, moved, false)
    }

    /// "Set the selection range" (the start is clamped after the end, as in the spec). When the range or the
    /// direction changed, queues `selectionchange` (coalesced) and `select`. Every cap is checked before the state
    /// changes. The caller has checked `selection_applies` after its conversions. Also the entry for a later focus or
    /// driver path.
    pub(crate) fn set_selection_range(&self, it: &mut Interp<'_>, n: NodeId, start: Option<u32>, end: Option<u32>, dir: Dir) -> R<()> {
        self.update_selection(it, n, |_| (start, end, dir))
    }

    /// Like `set_selection_range`, from the current selection (one scan of the value for the read and the write).
    fn update_selection(&self, it: &mut Interp<'_>, n: NodeId, f: impl FnOnce(Sel) -> (Option<u32>, Option<u32>, Dir)) -> R<()> {
        let Some((old, len)) = self.effective(Some(it), n)? else { return Ok(()) };
        let (start, end, dir) = f(old);
        let end = end.unwrap_or(0).min(len);
        let start = start.unwrap_or(0).min(end);
        let dir = dir_of(dir, start == end, self.doc.borrow().is_html(n, "textarea"), self.stored_sel(n).1);
        let new = Sel { start, end, dir };
        let changed = new != old;
        self.selection_room(n, changed, changed)?;
        self.queue_selection_tasks(n, changed, changed)?;
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
            return Ok(Dir::None);
        }
        let s = it.to_js_string(v)?;
        let len = it.heap.str_len(s).map_err(Error::Heap)?;
        if len != 7 && len != 8 {
            return Ok(Dir::None);
        }
        Ok(match it.heap.to_string_lossy(s).map_err(|e| it.heap_error(e))?.as_str() {
            "forward" => Dir::Forward,
            "backward" => Dir::Backward,
            _ => Dir::None,
        })
    }

    /// Fire `select` or `selectionchange` (both bubble, neither is cancelable) at the input when its task runs.
    /// Nothing is made when no listener is on the event path.
    pub(crate) fn fire_at_input(&self, it: &mut Interp<'_>, n: NodeId, ty: &str) -> R<()> {
        let listens = {
            let (doc, st) = (self.doc.borrow(), self.st.borrow());
            let on = |t: Target| st.listeners.get(&t).is_some_and(|v| v.iter().any(|l| &*l.ty == ty));
            let mut cur = n;
            let mut found = on(Target::Node(cur));
            while !found && let Some(p) = doc.parent(cur) {
                it.charge(1)?;
                cur = p;
                found = on(Target::Node(cur));
            }
            found || (cur == doc.root() && on(Target::Window))
        };
        if !listens {
            return Ok(());
        }
        let ev = self.new_event(it, ty, true, false, false, None)?;
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
    d.update_selection(it, n, |cur| (Some(start), Some(cur.end.max(start)), cur.dir))?;
    Ok(Value::Undefined)
}

pub(crate) fn set_selection_end(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = input_el(d, it, this)?;
    let new = d.nullable_ulong(it, arg(a, 0))?;
    d.require_selection(it, n)?;
    d.update_selection(it, n, |cur| (Some(cur.start), new, cur.dir))?;
    Ok(Value::Undefined)
}

pub(crate) fn set_selection_direction(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = input_el(d, it, this)?;
    let dir = d.direction_arg(it, arg(a, 0))?;
    d.require_selection(it, n)?;
    d.update_selection(it, n, |cur| (Some(cur.start), Some(cur.end), dir))?;
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
        d.set_selection_range(it, n, Some(0), Some(u32::MAX), Dir::None)?;
    }
    Ok(Value::Undefined)
}

impl Browser {
    /// The native selection of an `<input>`: `(start, end, direction)` in UTF-16 offsets, read without JS. `None`
    /// when the node is not an `<input>` or selection does not apply to its type.
    pub fn input_selection(&self, input: NodeId) -> Option<(u32, u32, &'static str)> {
        // No JS caller, so no step budget: see `Dom::effective` for the bound.
        self.dom.effective(None, input).ok().flatten().map(|(s, _)| (s.start, s.end, s.dir.name()))
    }
}
