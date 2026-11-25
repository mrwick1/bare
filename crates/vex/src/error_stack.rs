//! `Error.prototype.stack`, `Error.captureStackTrace` and `Error.stackTraceLimit`, in the style of V8.
//!
//! **The loop does little for it.** A native call stores `regs` and `pc` of the caller in `ErrStack` (`publish!`), and `Vm::run` stores the
//! address of its stack and puts the old values back when it ends. That is all the loop does. `error_ctor` and `captureStackTrace` walk the frames from
//! that point. An error that an op or a native raises goes through `unwind`, which walks the frames of the throw point when the error has no
//! stack yet (`Realm::stack_for_throw`).
//!
//! **Capture is compact.** An error records at most `Error.stackTraceLimit` frames as pairs `(code index, pc)`, packed into one string of
//! 16-bit units (4 units a frame), in the hidden own property `\0stk`. The keys are interned once. No text is made.
//!
//! **Text is made at the first read.** `stack` is one accessor on `Error.prototype` (a getter and a setter for all errors). The getter writes
//! `Name: message` and one line for each frame, `    at NAME (SOURCE:LINE:COL)` or `    at SOURCE:LINE:COL` for a function with no name,
//! then makes an own data property with the text and drops the frames. The setter makes an own data property with the assigned value.
//! `captureStackTrace` puts the same accessor, as an own property, on its target.
//!
//! **Gaps.** The host passes no script name, so SOURCE is `<anonymous>`. The frames of a machine that a native started (`call_function`)
//! end at its entry. A native frame is not listed. An error that has no frame (made by the host, or swallowed by a native before it
//! reached `unwind`) reads as `Name: message`. The head of the text is made at the first read, so a later change of `name` or `message`
//! shows in it (V8 fixes it at the creation).
use crate::builtins::{HIDDEN, error_to_string};
use crate::codes::Codes;
use crate::interp::REC;
use crate::interp_rt::{Key, R, Realm};
use crate::object::{F_NOEXT, Obj};
use crate::shapes::ShapeId;
use crate::string::{Chars, Str};
use crate::value::Value;

/// The state of the stacks of one realm.
#[derive(Default)]
pub(crate) struct ErrStack {
    /// Register 0 of the frame of the native call that runs now (the `regs` of the loop, `publish!`), null when there is none. A run puts
    /// back the old value when it ends, and a reallocation of the stack clears it.
    pub top: Top,
    /// The `pc` of that call.
    pub pc: u32,
    /// The address of the first word of the stack of the run that executes now, to turn `top` into an index.
    pub base: usize,
    /// The `Error` constructor, for `stackTraceLimit`.
    pub ctor: Option<Obj>,
    /// The shape of the `Error` constructor and the slot of `stackTraceLimit` in it, as the last read found them.
    lim: Option<(ShapeId, u32)>,
    /// The names `stackTraceLimit`, `\0stk` and `stack`: made at the first capture.
    keys: Option<(Str, Str, Str)>,
    /// The getter and the setter as values, for `captureStackTrace`: made at its first call.
    acc: Option<(Value, Value)>,
    /// The frames of the capture in progress.
    buf: Vec<u16>,
}

/// A raw pointer that has a default (null).
#[derive(Clone, Copy)]
pub(crate) struct Top(pub *const Value);

impl Default for Top {
    fn default() -> Top {
        Top(std::ptr::null())
    }
}

/// `Error.stackTraceLimit` when the script did not change it.
const DEFAULT_LIMIT: usize = 10;
/// The slot that means: no `stackTraceLimit` in the shape.
const NONE: u32 = u32::MAX;
/// A bound for the limit, so that `Infinity` does not walk a whole deep stack.
const MAX_LIMIT: usize = 200;

/// The integer word `k` of the record of the frame `fp`, `None` outside the stack (a frame that is not one).
fn rec_int(stack: &[Value], fp: usize, k: usize) -> Option<i32> {
    Some(stack.get((fp.checked_sub(REC)?) + k)?.bits() as u32 as i32)
}

impl Realm {
    fn stack_keys(&mut self) -> (Str, Str, Str) {
        if let Some(k) = self.es.keys {
            return k;
        }
        let k = (self.intern("stackTraceLimit"), self.intern("\0stk"), self.intern("stack"));
        // Roots: the collector frees an atom that nothing refers to, and `ErrStack` is not traced.
        for s in [k.0, k.1, k.2] {
            self.keep(s.value());
        }
        self.es.keys = Some(k);
        k
    }

    /// The number of frames to record, `None` when `Error.stackTraceLimit` is not a number (no stack, as in V8).
    fn stack_limit(&mut self, lk: Str) -> Option<usize> {
        let Some(c) = self.es.ctor else { return Some(DEFAULT_LIMIT) };
        let sid = c.shape(&self.shapes);
        let slot = match self.es.lim {
            Some((s, slot)) if s == sid => slot,
            _ => {
                // An absent property (the entry is still lazy) is cached as well: the slot `NONE`.
                let slot = self.shapes.find(sid, lk).map_or(NONE, |e| e.slot());
                self.es.lim = Some((sid, slot));
                slot
            }
        };
        if slot == NONE {
            return Some(DEFAULT_LIMIT);
        }
        let n = c.slot(&self.shapes, slot).as_f64()?;
        Some(if n >= MAX_LIMIT as f64 { MAX_LIMIT } else if n > 0.0 { n as usize } else { 0 })
    }

    /// Record the frames from `(ci, fp, pc)` outward for `target` (at most `Error.stackTraceLimit`). With `upto`, the frames above and
    /// including the call of that function are dropped (`Error.captureStackTrace(obj, fn)`). `own` puts the accessor on `target` (a
    /// target that is not an error). Every read of the stack is checked: a published frame can be one that has gone away.
    #[cold]
    #[inline(never)]
    #[allow(clippy::too_many_arguments)]
    fn capture_at(&mut self, stack: &[Value], prog: &Codes, (mut ci, mut fp, mut pc): (usize, usize, u32), target: Obj, upto: Option<Value>, (own, fresh): (bool, bool)) {
        let (lk, hk, k) = self.stack_keys();
        let Some(limit) = self.stack_limit(lk) else { return };
        let mut buf = std::mem::take(&mut self.es.buf);
        buf.clear();
        let mut skipping = upto.is_some();
        while buf.len() < 4 * limit {
            if skipping {
                skipping = upto != stack.get(fp.wrapping_sub(REC)).copied();
            } else {
                buf.extend([ci as u16, (ci >> 16) as u16, pc as u16, (pc >> 16) as u16]);
            }
            let (Some(caller), Some(npc)) = (rec_int(stack, fp, 5), rec_int(stack, fp, 3)) else { break };
            let Some(c) = usize::try_from(caller).ok().and_then(|c| Some((c, prog.get(c)?.nregs as usize))) else { break };
            let Some(nfp) = fp.checked_sub(REC + c.1) else { break };
            (ci, fp, pc) = (c.0, nfp, npc as u32);
        }
        if !buf.is_empty() {
            let f = Str::new(&mut self.heap, Chars::Two(&buf)).expect("memory").value();
            self.attach_stack(target, (hk, k), f, own, fresh);
        }
        self.es.buf = buf;
    }

    /// Give `target` the frames `f`. An own accessor only when `own` (see `capture_at`).
    fn attach_stack(&mut self, target: Obj, (hk, k): (Str, Str), f: Value, own: bool, fresh: bool) {
        if target.has_flag(F_NOEXT) {
            return;
        }
        if fresh || self.shapes.find(target.shape(&self.shapes), hk).is_none() {
            // The own hidden key is new: add it with no lookup, as `make_error` adds `message`.
            target.add(&mut self.heap, &mut self.shapes, hk, HIDDEN, &[f]).expect("memory");
        } else {
            let _ = self.define_attrs(target, Key::Name(hk), HIDDEN, f);
        }
        if own {
            let (g, s) = self.stack_accessors();
            let _ = self.define_accessor(target, Key::Name(k), g, false, false);
            let _ = self.define_accessor(target, Key::Name(k), s, true, false);
        }
    }

    fn stack_accessors(&mut self) -> (Value, Value) {
        if let Some(a) = self.es.acc {
            return a;
        }
        let g = self.new_native(stack_get, false, 0, "stack").value();
        let s = self.new_native(stack_set, false, 1, "stack").value();
        self.keep(g);
        self.keep(s);
        *self.es.acc.insert((g, s))
    }

    /// The frames of the call that runs now, for the error `e` that a native just made (`error_ctor`). Nothing when no run published a stack.
    pub(crate) fn capture_here(&mut self, e: Value, upto: Option<Value>, own: bool, fresh: bool) {
        let (top, Some(o)) = (self.es.top.0, Realm::obj(e)) else { return };
        let off = (top as usize).wrapping_sub(self.es.base);
        let fp = off / std::mem::size_of::<Value>();
        if top.is_null() || off % std::mem::size_of::<Value>() != 0 || fp < REC {
            return;
        }
        // SAFETY: `top` is `regs` of the loop that runs the call (`publish!`), so it is in the live stack that starts at `base`: the loop
        // clears it when the stack is reallocated, and a run puts back the one of the run above when it ends. The words below `fp` are
        // initialized (`Vm::start` and `ensure!` fill the stack). The slice is a read of memory that `regs` can reach.
        let stack: &[Value] = unsafe { std::slice::from_raw_parts(top.sub(fp), fp) };
        let Some(ci) = self.function_new(stack[fp - REC]).map(|f| f.0) else { return };
        let prog = self.prog.clone();
        self.capture_at(stack, &prog, (ci, fp, self.es.pc), o, upto, (own, fresh));
    }

    /// `unwind` calls it for the thrown value `e` at the frame `at`: an error that has no frames yet (an internal one) gets them now.
    pub(crate) fn stack_for_throw(&mut self, stack: &[Value], prog: &Codes, at: (usize, usize, u32), e: Value) {
        let Some(o) = Realm::obj(e) else { return };
        let hk = self.stack_keys().1;
        if self.shapes.find(o.shape(&self.shapes), hk).is_none() && self.is_error(e) {
            self.capture_at(stack, prog, at, o, None, (false, true));
        }
    }

    /// `    at NAME (SOURCE:LINE:COL)` for the frame `(ci, pc)`.
    fn frame_line(&self, ci: usize, pc: u32) -> String {
        let Some(code) = self.prog.get(ci) else { return "    at <anonymous>".into() };
        let name = code.atoms.get(code.fn_name as usize).map(|&a| self.text(self.atoms.adopt(a))).filter(|n| !n.is_empty());
        let at = match (self.lazy.as_ref().and_then(|l| l.source_of(ci)), code.source_at(pc)) {
            (Some(src), Some(off)) => {
                let off = (off as usize).min(src.len());
                let head = src.as_bytes().get(..off).unwrap_or_default();
                let line = head.iter().filter(|&&b| b == b'\n').count() + 1;
                let start = head.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
                let col = src.get(start..off).map_or(off - start, |s| s.encode_utf16().count()) + 1;
                format!("<anonymous>:{line}:{col}")
            }
            _ => "<anonymous>".into(),
        };
        match name {
            Some(n) => format!("    at {n} ({at})"),
            None => format!("    at {at}"),
        }
    }
}

/// The getter of `stack`: make the text from the frames, keep it as an own data property.
pub(crate) fn stack_get(rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    let Some(o) = Realm::obj(this) else { return Ok(Value::UNDEFINED) };
    let (_, hk, k) = rt.stack_keys();
    let frames = rt.shapes.find(o.shape(&rt.shapes), hk).and_then(|e| Realm::str_of(o.slot(&rt.shapes, e.slot())));
    if frames.is_none() && !rt.is_error(this) {
        return Ok(Value::UNDEFINED);
    }
    let units: Vec<u16> = frames.map(|f| (0..f.len()).filter_map(|i| f.unit_at(i)).collect()).unwrap_or_default();
    let head = error_to_string(rt, this, &[])?;
    let mut text = Realm::str_of(head).map(|h| rt.text(h)).unwrap_or_default();
    for fr in units.chunks_exact(4) {
        let (ci, pc) = (fr[0] as usize | (fr[1] as usize) << 16, fr[2] as u32 | (fr[3] as u32) << 16);
        text.push('\n');
        text.push_str(&rt.frame_line(ci, pc));
    }
    let v = rt.string(&text);
    if frames.is_some() {
        let _ = rt.define_attrs(o, Key::Name(k), HIDDEN, v);
        let _ = rt.define_attrs(o, Key::Name(hk), HIDDEN, Value::UNDEFINED);
    }
    Ok(v)
}

/// The setter of `stack`: an own data property with the value.
pub(crate) fn stack_set(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    if let Some(o) = Realm::obj(this) {
        let (_, hk, k) = rt.stack_keys();
        let _ = rt.define_attrs(o, Key::Name(k), HIDDEN, args.first().copied().unwrap_or(Value::UNDEFINED));
        if rt.shapes.find(o.shape(&rt.shapes), hk).is_some() {
            let _ = rt.define_attrs(o, Key::Name(hk), HIDDEN, Value::UNDEFINED);
        }
    }
    Ok(Value::UNDEFINED)
}

/// `Error.captureStackTrace(obj[, fn])`.
pub(crate) fn capture_stack_trace(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let o = args.first().copied().unwrap_or(Value::UNDEFINED);
    if Realm::obj(o).is_none() {
        return rt.type_error("Invalid argument");
    }
    rt.capture_here(o, args.get(1).copied().filter(|f| !f.is_undefined()), true, false);
    Ok(Value::UNDEFINED)
}

#[cfg(test)]
#[path = "error_stack_gc_tests.rs"]
mod gc_tests;
