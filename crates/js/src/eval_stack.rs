// `Error.stack`: a bounded text of the real call frames at the moment an error object is made. Included by
// `eval.rs` (`include!`), so it shares that module's imports.
//
// `stack` is not in ECMAScript. V8 made it a de-facto API and its text is not standard. This is bare's format:
//
//     Error: boom                         the name and message at construction, no user code is run to read them
//         at inner (script#1:1:27)        function name, script slot, line and column (both from 1, UTF-16 units)
//         at new Foo (script#1:3:5)       `new` for a construct call (also a class constructor, also the default one)
//         at script#1:9:1                 an anonymous function, or the top level of a script
//
// What a frame is. One frame per authored code run: a script's top level, a script function (also a getter, a
// setter, an arrow, a method), a class constructor and an async function (its start and each resume). Natives
// (`forEach`, `JSON.parse`, ...) and host functions have no frame: V8 prints them as `at Array.forEach (<anonymous>)`,
// bare leaves them out. The position of a frame is the start of the call or `new` expression it is running (for the
// top frame, of the one that made the error); it is the span start, where V8 uses the property name of `a.b()`.
// A frame with no call in progress (an interpreter error such as `null.x`) has no position and prints only
// `(script#N)`: the evaluator does not track a current position, and this code does not invent one.
// The script id is the slot of the realm (`Interp::stats().scripts`): bare has no URL or file name to show.
// Names are the declared names (`function f`, `function` expression name, `class C`). A method, getter, setter or
// arrow has no declared name and prints as anonymous. V8 also infers `Object.method` and
// `const f = function(){}`; bare does not. A name that is not a plain identifier prints as anonymous.
//
// Provenance. A job of the microtask queue (promise callback, `queueMicrotask`, async resume) runs with a base
// mark: the frames under it are not part of its stack. An async function shows its own frame only, never the
// function that started it after its first `await`. Before the first `await` the caller is on the stack.
//
// Bounds. The frame list has at most one entry per active call, so the call depth (`max_call_depth`, 100) bounds
// it. Text: at most `STACK_FRAMES` frames, at most `STACK_UNITS` UTF-16 units in all (the header is cut at
// `STACK_HEADER_UNITS`), no frame is cut in the middle. Work: one step per frame and one step per 32 source bytes
// scanned for a line and column, charged BEFORE the scan. The first location in a script scans it once to make
// marks every 4 KiB (`Program::location`); later ones scan at most 4 KiB, so an error deep in a one-line bundle
// is cheap; the text is checked before it grows. The result is one
// ordinary string on the error object. Nothing else is kept, so the GC has nothing to trace or prune and a
// frame never outlives its call. The frames unwind on every exit (return, throw, limit) by truncating to the
// length seen at the push.
//
// The property: an own data property, writable, not enumerable, configurable (as V8 shows it). It is a plain
// string, not V8's lazy accessor. Frames omit arguments, source bodies and URLs.
// The header still contains the error name and message supplied at construction.
//
// `Error.stackTraceLimit` is an own data property of `Error` (default 10, writable, enumerable, configurable). It is
// read as a data property at each capture: a number gives that many frames (a fraction is cut, a negative number or NaN
// is 0). Any other value, or a deleted property, gives `stack` as `undefined`, as in V8. The text caps above still hold.
// `Error.captureStackTrace(obj, fn)` defines `stack` on any object (V8's message for a non-object, a frozen
// object and a non-configurable `stack`). With `fn` it leaves out the frames of the topmost call of `fn` and the
// frames above it: it finds a script function or class by its code. A function that is not on the stack (a native,
// or one that did not run) leaves no frames, as in V8. `Error.prepareStackTrace` does not exist.

/// The default of `Error.stackTraceLimit`.
const STACK_FRAMES: usize = 10;
const STACK_UNITS: usize = 2048;
const STACK_HEADER_UNITS: usize = 512;
const STACK_NAME_UNITS: usize = 64;

#[derive(Clone, Copy)]
enum Where {
    Top,
    Func(FuncId),
    Class(ClassId),
}

#[derive(Clone, Copy)]
struct StackFrame {
    script: u32,
    at: Where,
    new: bool,
    /// `StackState::pos` of the caller, back again when this frame ends. For a frame `i` it is also the position
    /// of the call that frame `i - 1` is running.
    saved_pos: u32,
}

#[derive(Default)]
struct StackState {
    frames: Vec<StackFrame>,
    /// Frames below this index belong to code that did not start the running job.
    base: usize,
    /// Start offset + 1 of the call or `new` expression the top frame is running (0: none).
    pos: u32,
    /// The next `call_script` is a construct call.
    new: bool,
}

impl<'p> Interp<'p> {
    /// Run `f` with a frame for `at` of `script` on the list.
    fn in_frame<T>(&mut self, script: u32, at: Where, new: bool, f: impl FnOnce(&mut Self) -> T) -> T {
        let n = self.stk.frames.len();
        let saved_pos = std::mem::replace(&mut self.stk.pos, 0);
        self.stk.frames.push(StackFrame { script, at, new, saved_pos });
        let r = f(self);
        self.stk.frames.truncate(n);
        self.stk.pos = saved_pos;
        r
    }

    /// Mark the start of the call or `new` expression `e` of the running frame.
    fn mark_call(&mut self, e: ExprId) {
        self.stk.pos = self.prog.expr(e).span.start.saturating_add(1);
    }

    /// A job starts a new stack: the frames under it are not its callers.
    fn in_job<T>(&mut self, f: impl FnOnce(&mut Self) -> T) -> T {
        let base = std::mem::replace(&mut self.stk.base, self.stk.frames.len());
        let pos = std::mem::replace(&mut self.stk.pos, 0);
        let r = f(self);
        self.stk.base = base;
        self.stk.pos = pos;
        r
    }

    /// `name` or `message` of `o` as a data property found on the prototype chain. No getter and no coercion runs.
    fn stack_text_prop(&mut self, o: ObjId, k: PropertyKey) -> Option<StrId> {
        let mut cur = Some(o);
        for _ in 0..16 {
            let c = cur?;
            match self.heap.get_own_property(c, k).ok()? {
                Some(Property::Data { value: Value::String(s), .. }) => return Some(s),
                Some(_) => return None,
                None => cur = self.heap.get_prototype_of(c).ok()?,
            }
        }
        None
    }

    /// `Error.stackTraceLimit` as a frame count. `None`: it is not a number, so there is no stack.
    fn stack_limit(&mut self) -> Option<usize> {
        match self.heap.get_own_property(self.sc.err_ctor, self.sc.k_stack_limit).ok()? {
            Some(Property::Data { value: Value::Number(n), .. }) => Some(if n.is_nan() || n < 0.0 { 0 } else { n as usize }),
            _ => None,
        }
    }

    /// Define `stack` on the new error object `o`. Fails only with a limit (work or heap).
    fn capture_stack(&mut self, o: ObjId) -> Res<()> {
        let s = self.stack_text(o, self.stk.frames.len())?;
        self.def(o, self.k_stack, s.map_or(Value::Undefined, Value::String), true, false, true)
    }

    /// The `stack` text of `o` for the frames below index `top`. `None`: `Error.stackTraceLimit` is not a number.
    fn stack_text(&mut self, o: ObjId, top: usize) -> Res<Option<StrId>> {
        let Some(limit) = self.stack_limit() else { return Ok(None) };
        let mut text: Vec<u16> = Vec::new();
        let name = self.stack_text_prop(o, self.k_name).and_then(|s| self.heap.str_units(s).ok());
        let mut head: Vec<u16> = name.map_or_else(|| "Error".encode_utf16().collect(), |u| u[..u.len().min(STACK_HEADER_UNITS)].to_vec());
        let msg = self.stack_text_prop(o, self.k_message).and_then(|s| self.heap.str_units(s).ok()).filter(|m| !m.is_empty());
        if let Some(m) = msg {
            if !head.is_empty() {
                head.extend(": ".encode_utf16());
            }
            head.extend_from_slice(&m[..m.len().min(STACK_HEADER_UNITS)]);
        }
        head.truncate(STACK_HEADER_UNITS);
        text.extend_from_slice(&head);
        let (base, len) = (self.stk.base, self.stk.frames.len());
        for i in (base..top.max(base)).rev().take(limit) {
            self.spend(1)?;
            let fr = self.stk.frames[i];
            let pos = if i + 1 < len { self.stk.frames[i + 1].saved_pos } else { self.stk.pos };
            let line = self.frame_line(fr, pos)?;
            if text.len() + line.len() > STACK_UNITS {
                break;
            }
            text.extend_from_slice(&line);
        }
        Ok(Some(h!(self, self.heap.intern_vec(text))))
    }

    /// `Error.captureStackTrace(obj, fn)`.
    fn error_capture(&mut self, obj: Value, f: Value) -> Res<Value> {
        let Value::Object(o) = obj else { return Err(self.type_err("invalid_argument")) };
        let mut top = self.stk.frames.len();
        if let Value::Object(fo) = f
            && self.funcs.contains_key(&fo)
        {
            // Not on the stack (a native, or a function that did not run): no frames.
            top = self.stk.base;
            let target = match self.funcs.get(&fo) {
                Some(&Callee::Script { script, fid, .. }) => Some((script, Where::Func(fid))),
                Some(Callee::Class) => self.cls.classes.get(&fo).map(|c| (c.script, Where::Class(c.class))),
                _ => None,
            };
            let same = |a: Where, b: Where| match (a, b) {
                (Where::Func(x), Where::Func(y)) => x == y,
                (Where::Class(x), Where::Class(y)) => x == y,
                _ => false,
            };
            if let Some((script, at)) = target
                && let Some(i) = (self.stk.base..self.stk.frames.len()).rev().find(|&i| self.stk.frames[i].script == script && same(self.stk.frames[i].at, at))
            {
                top = i;
            }
        }
        let s = self.stack_text(o, top)?.map_or(Value::Undefined, Value::String);
        let d = PropertyDescriptor { value: Some(s), writable: Some(true), enumerable: Some(false), configurable: Some(true), ..Default::default() };
        if !h!(self, self.heap.define_own_property(o, self.k_stack, &d)) {
            let own = h!(self, self.heap.get_own_property(o, self.k_stack)).is_some();
            return Err(self.type_err(if own { "Cannot redefine property: stack" } else { "Cannot define property stack, object is not extensible" }));
        }
        Ok(Value::Undefined)
    }

    /// `\n    at NAME (script#N:L:C)` as UTF-16.
    #[cold]
    pub(crate) fn te_dump(&mut self, msg: &str) {
        if std::env::var("BARE_TE_DUMP").is_err() { return; }
        let len = self.stk.frames.len();
        eprintln!("TE {msg} js_frames={len}");
        for i in (0..len).rev().take(8) {
            let fr = self.stk.frames[i];
            let pos = if i + 1 < len { self.stk.frames[i + 1].saved_pos } else { self.stk.pos };
            let l = self.frame_line(fr, pos).map(|v| String::from_utf16_lossy(&v)).unwrap_or_default();
            let prog = if fr.script as usize == self.cur { self.prog.clone() } else { self.scripts[fr.script as usize].prog.clone() };
            if i + 1 == len { if let Where::Func(f) = fr.at { let sp = prog.func(f).span; eprintln!("TE topfunc span={}..{} text={}", sp.start, sp.end, prog.text(crate::ast::Span { start: sp.start, end: sp.end.min(sp.start + 700) }).replace('\n', " ")); } }
            let p = pos.saturating_sub(1);
            let snip = prog.text(crate::ast::Span { start: p.saturating_sub(160), end: p + 60 }).replace('\n', " ");
            eprintln!("TE {}  ##  {snip}", l.trim());
        }
    }
    fn frame_line(&mut self, fr: StackFrame, pos: u32) -> Res<Vec<u16>> {
        let code_prog = if fr.script as usize == self.cur { self.prog.clone() } else { self.scripts[fr.script as usize].prog.clone() };
        let span = match fr.at {
            Where::Top => None,
            Where::Func(f) => code_prog.func(f).name,
            Where::Class(c) => code_prog.class(c).name,
        };
        let mut name = span.filter(|s| s.end - s.start <= 4 * STACK_NAME_UNITS as u32).map(|s| code_prog.name(s).into_owned()).unwrap_or_default();
        if name.is_empty() || name.chars().count() > STACK_NAME_UNITS || !name.chars().all(|c| c == '_' || c == '$' || c.is_alphanumeric()) {
            name.clear();
        }
        let mut loc = format!("script#{}", fr.script);
        if pos != 0 {
            self.spend_units(code_prog.location_cost())?;
            let l = code_prog.location(pos - 1);
            loc = format!("{loc}:{}:{}", l.line, l.column + 1);
        }
        let line = match (name.is_empty(), fr.new) {
            (true, false) => format!("\n    at {loc}"),
            (true, true) => format!("\n    at new <anonymous> ({loc})"),
            (false, false) => format!("\n    at {name} ({loc})"),
            (false, true) => format!("\n    at new {name} ({loc})"),
        };
        Ok(line.encode_utf16().collect())
    }
}
