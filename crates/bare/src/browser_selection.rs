//! Text selection of `<input>`: `selectionStart`, `selectionEnd`, `selectionDirection`, `setSelectionRange`, `select`.
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
//! - Applies to the input types text, search, tel, url and password (an unknown `type` is text). For the other
//!   types the three getters give `null`, `setSelectionRange` and the three setters throw `InvalidStateError`, and
//!   `select()` returns without effect. The applicability is read when the algorithm runs, after the arguments are
//!   converted (a conversion may run JS that changes the type or the value).
//! - Conversions are WebIDL: `unsigned long` is ToUint32 (so -1 is 4294967295 and Infinity is 0, a fraction is cut),
//!   `selectionStart`/`selectionEnd` are `unsigned long?` (null and undefined are null, read as 0 by the algorithm),
//!   `setSelectionRange` needs two arguments (`TypeError` otherwise) and `direction` is a string that is `forward`
//!   or `backward` exactly, else `none`. They run before any state is read, and a throw changes nothing.
//! - `select` event: when a call changes the range or the direction, one task is queued in the scheduler
//!   (`Task::Select`, due now, after the tasks already queued). It fires `select` (bubbles, not cancelable) at the
//!   input and walks the ordinary dispatch. There is no coalescing (the spec queues one task for each change). The number
//!   of queued tasks is capped (`SchedLimits::max_select_tasks`, `Limit(Host("select-tasks"))`), checked before the
//!   state changes. No task runs after a fatal pump error (the pump is sticky). The value setter and the driver queue
//!   nothing (the spec has no event for them). A listener that throws is reported, as for any event.
//!
//! Gates and deviations (on purpose)
//! - `<textarea>` has no value state in this host, so it has no selection. `setRangeText`, `selectionchange`
//!   (not in the spec text read), `document.getSelection`, `Selection` and `Range` are absent.
//! - No caret, no layout, no mouse or keyboard selection, no focus. `select()` does not focus (the spec says it
//!   does not need to); a focus worker can call `Dom::selection_applies` and `Dom::set_selection_range` later.
//! - Initial selection: the spec leaves it open. Here the caret is at the end of the value.
//! - A `type` change keeps the stored selection; it is clamped to the value when read. Not verified against the spec.
//! - Direction `none` is always available (as on Linux and macOS browsers; the spec allows `forward` instead).

use crate::browser::*;
use crate::browser_dom::{arg, input_el, input_kind};
use bare_html::dom::NodeId;
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

impl Dom {
    /// True when selection applies to this `<input>` now (the HTML "does not apply" test).
    pub(crate) fn selection_applies(&self, n: NodeId) -> bool {
        matches!(input_kind(&self.doc.borrow(), n), "text" | "search" | "tel" | "url" | "password")
    }

    /// The selection as JS reads it, the value length in UTF-16 units and in bytes. `None` when it does not apply.
    fn effective(&self, n: NodeId) -> Option<(Sel, u32, usize)> {
        if !self.selection_applies(n) {
            return None;
        }
        let v = self.input_value_of(n)?;
        let len = v.encode_utf16().count().min(u32::MAX as usize) as u32;
        let sel = match self.st.borrow().inputs.get(&n).and_then(|i| i.sel) {
            Some(s) => Sel { start: s.start.min(len), end: s.end.min(len), dir: s.dir },
            None => Sel { start: len, end: len, dir: Dir::None },
        };
        Some((sel, len, v.len()))
    }

    /// "Set the selection range" (the start is clamped after the end, as in the spec). Queues a `select` task when the
    /// range or the direction changed; the cap is checked before the state changes. The caller has checked
    /// `selection_applies` after its conversions. Also the entry for a later focus or driver path.
    pub(crate) fn set_selection_range(&self, it: &mut Interp<'_>, n: NodeId, start: Option<u32>, end: Option<u32>, dir: Dir) -> R<()> {
        let Some((old, len, bytes)) = self.effective(n) else { return Ok(()) };
        it.charge((bytes / 256) as u64)?;
        let end = end.unwrap_or(0).min(len);
        let new = Sel { start: start.unwrap_or(0).min(end), end, dir };
        if new != old {
            self.queue_select(n)?;
        }
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

    /// Fire `select` at the input when its task runs. Nothing is made when no listener is on the event path.
    pub(crate) fn fire_select(&self, it: &mut Interp<'_>, n: NodeId) -> R<()> {
        let listens = {
            let (doc, st) = (self.doc.borrow(), self.st.borrow());
            let on = |t: Target| st.listeners.get(&t).is_some_and(|v| v.iter().any(|l| &*l.ty == "select"));
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
        let ev = self.new_event(it, "select", true, false, false, None)?;
        self.dispatch(it, Target::Node(n), ev).map(|_| ())
    }
}

fn with_selection(d: &Dom, it: &mut Interp<'_>, this: Value) -> R<(NodeId, Option<Sel>)> {
    let n = input_el(d, it, this)?;
    let s = d.effective(n);
    if let Some((_, _, bytes)) = s {
        it.charge((bytes / 256) as u64)?;
    }
    Ok((n, s.map(|x| x.0)))
}

pub(crate) fn selection_start(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    Ok(with_selection(d, it, this)?.1.map_or(Value::Null, |s| Value::Number(s.start as f64)))
}

pub(crate) fn selection_end(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    Ok(with_selection(d, it, this)?.1.map_or(Value::Null, |s| Value::Number(s.end as f64)))
}

pub(crate) fn selection_direction(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    match with_selection(d, it, this)?.1 {
        Some(s) => d.js_str(it, s.dir.name()),
        None => Ok(Value::Null),
    }
}

pub(crate) fn set_selection_start(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = input_el(d, it, this)?;
    let new = d.nullable_ulong(it, arg(a, 0))?;
    d.require_selection(it, n)?;
    let (cur, ..) = d.effective(n).expect("applies");
    let start = new.unwrap_or(0);
    d.set_selection_range(it, n, Some(start), Some(cur.end.max(start)), cur.dir)?;
    Ok(Value::Undefined)
}

pub(crate) fn set_selection_end(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = input_el(d, it, this)?;
    let new = d.nullable_ulong(it, arg(a, 0))?;
    d.require_selection(it, n)?;
    let (cur, ..) = d.effective(n).expect("applies");
    d.set_selection_range(it, n, Some(cur.start), new, cur.dir)?;
    Ok(Value::Undefined)
}

pub(crate) fn set_selection_direction(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = input_el(d, it, this)?;
    let dir = d.direction_arg(it, arg(a, 0))?;
    d.require_selection(it, n)?;
    let (cur, ..) = d.effective(n).expect("applies");
    d.set_selection_range(it, n, Some(cur.start), Some(cur.end), dir)?;
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
        self.dom.input_value_of(input)?;
        self.dom.effective(input).map(|(s, ..)| (s.start, s.end, s.dir.name()))
    }
}
