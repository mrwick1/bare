//! `window.console` with a bounded in-memory sink: `console.log`, `console.warn`, `console.error`.
//!
//! What it does
//! - `console` is an own property of the global object (writable, configurable, not enumerable, as WebIDL says for a
//!   namespace). Its three methods are ordinary host functions: writable, enumerable, configurable, `length` 0, not
//!   constructors, return `undefined`. A page can save them (`let e = console.error`), bind them, call them with any
//!   `this` (the receiver is ignored), wrap them, replace them or delete `console`.
//! - Every call is recorded: level, a sequence number and the raw argument `Value`s. The capture is inside the host
//!   function, so `apply`, `call`, `bind` and a saved copy all record. No JavaScript runs while a
//!   call is recorded (no `toString`, no getter, no `valueOf`, no iteration). No `RefCell` borrow is held longer than
//!   the push.
//! - Nothing is printed. Records stay in the process until the driver takes them (`Browser::take_console_records`).
//!   `Debug` of a record, of the stats and of the limits shows only counts, never an argument.
//!
//! Bounds (`ConsoleLimits`, checked before anything is allocated)
//! - `max_args`: arguments of one call. More is `Error::Limit(Host("console-args"))`, fatal, like every other host cap.
//! - `max_records`: kept records. When the sink is full the oldest record is dropped and `dropped` goes up by one.
//!   With 0 nothing is kept and every call counts as dropped.
//! - Each call charges `1 + args` steps. `max_format_bytes` bounds UTF-8 output bytes and UTF-16 text input units
//!   for `format_console_record` (`Limit(Host("console-format"))`). Text scans are charged before copying.
//!
//! Collection
//! - The values of kept records are roots (`Dom::roots`), so a slot they use is never reused while the record exists.
//!   Records need no `prune`: there is no side table keyed by object.
//! - A record that was taken is the caller's. The browser no longer roots it. Pass its values in `extra` of
//!   `Browser::collect`, or they are freed and the stale `Value`s are refused by the heap (generation check) later.
//!
//! Formatting (`Browser::format_console_record`) is a data-only driver formatter. Capture does not run the
//! script-visible Console Standard Formatter coercions; this is a deliberate compatibility limit.
//! - No arguments gives "". A string first argument with more arguments enables substitutions. Other values use
//!   fixed data-only representations. Pieces are joined with one space; this does not claim browser Printer parity.
//! - Formatter handles `%s %d %i %f %o %O %c` and `%%`. `%%` is a local convention beyond the Standard text. A
//!   specifier with no argument left stays as text. `%c` takes an argument and prints nothing.
//! - The Standard converts with `String`, `parseInt` and `parseFloat`, which run user code on an object. This module
//!   never runs user code to log. So an object or function prints as `[object]` or `[function]` for every specifier,
//!   and `%d %i %f` of an object is `NaN`. Symbols print as `Symbol(desc)` (`%d` gives `NaN`). A BigInt prints with an `n`
//!   (`1n`) for every specifier but `%f`, which is `parseFloat` of its digits (as in Node). `%o` and `%O` print
//!   primitives like `%s`, strings in double quotes without escapes. No grouping, indentation, `%c` styling.
//! - A lone surrogate prints as U+FFFD (display only; the records keep the exact value).
//!
//! Gates: only `log`, `warn` and `error` exist (the methods the bundle uses). `info`, `debug`, `trace`, `assert`, `dir`,
//! `table`, `group*`, `count*`, `time*` are absent. No console-level `clear`, no filtering, no devtools.
//!
//! Not compared with a real browser.

use crate::browser::*;
use bare_js::eval::{Error, Interp};
use bare_js::value::{PropertyKey, StrId, Value, is_js_space, number_to_string, string_to_number};
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::fmt;

/// Bounds of the console sink. Each is checked before the thing grows.
#[derive(Clone, Copy, Debug)]
pub struct ConsoleLimits {
    /// Kept records. The oldest is dropped (and counted) when this is full.
    pub max_records: usize,
    /// Arguments of one call. `console-args`.
    pub max_args: usize,
    /// UTF-8 output bytes and UTF-16 input units allowed by the driver formatter. `console-format`.
    pub max_format_bytes: usize,
}

impl Default for ConsoleLimits {
    fn default() -> Self {
        ConsoleLimits { max_records: 1024, max_args: 64, max_format_bytes: 64 << 10 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConsoleLevel {
    Log,
    Warn,
    Error,
}

/// One call. `args` are the raw values the page passed. `Debug` shows no argument.
#[derive(Clone)]
pub struct ConsoleRecord {
    /// 0 for the first call of the realm. A gap between two records means drops.
    pub seq: u64,
    pub level: ConsoleLevel,
    pub args: Vec<Value>,
}

impl fmt::Debug for ConsoleRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ConsoleRecord {{ seq: {}, level: {:?}, args: {} }}", self.seq, self.level, self.args.len())
    }
}

/// Counters for tests and the load tool.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ConsoleStats {
    /// Records kept now.
    pub records: usize,
    /// Values kept now (and rooted).
    pub values: usize,
    /// Calls so far, kept or dropped.
    pub calls: u64,
    /// Records dropped because the sink was full.
    pub dropped: u64,
}

#[derive(Default)]
struct CState {
    q: VecDeque<ConsoleRecord>,
    values: usize,
    calls: u64,
    dropped: u64,
}

#[derive(Default)]
pub(crate) struct Console {
    lim: Cell<ConsoleLimits>,
    st: RefCell<CState>,
}

impl Console {
    pub(crate) fn roots(&self, out: &mut Vec<Value>) {
        for r in &self.st.borrow().q {
            out.extend_from_slice(&r.args);
        }
    }

    fn drop_oldest(&self, st: &mut CState) {
        if let Some(r) = st.q.pop_front() {
            st.values -= r.args.len();
        }
        st.dropped += 1;
    }
}

fn record(d: &Dom, it: &mut Interp<'_>, level: ConsoleLevel, args: &[Value]) -> R<Value> {
    let lim = d.console.lim.get();
    if args.len() > lim.max_args {
        return Err(d.lim("console-args"));
    }
    it.charge(1 + args.len() as u64)?;
    let mut st = d.console.st.borrow_mut();
    let seq = st.calls;
    st.calls += 1;
    if lim.max_records == 0 {
        st.dropped += 1;
        return Ok(Value::Undefined);
    }
    while st.q.len() >= lim.max_records {
        d.console.drop_oldest(&mut st);
    }
    st.values += args.len();
    st.q.push_back(ConsoleRecord { seq, level, args: args.to_vec() });
    Ok(Value::Undefined)
}

/// Record a console call from the host with one string argument. It charges no step: the host uses it right after a task ended
/// on the step budget (`Browser::survive`), when a charge would fail.
pub(crate) fn push_text(d: &Dom, it: &mut Interp<'_>, level: ConsoleLevel, text: &str) {
    let lim = d.console.lim.get();
    let Ok(s) = it.heap.intern_str(text) else { return };
    let mut st = d.console.st.borrow_mut();
    let seq = st.calls;
    st.calls += 1;
    if lim.max_records == 0 {
        st.dropped += 1;
        return;
    }
    while st.q.len() >= lim.max_records {
        d.console.drop_oldest(&mut st);
    }
    st.values += 1;
    st.q.push_back(ConsoleRecord { seq, level, args: vec![Value::String(s)] });
}

fn log(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> R<Value> {
    record(d, it, ConsoleLevel::Log, a)
}
fn warn(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> R<Value> {
    record(d, it, ConsoleLevel::Warn, a)
}
fn error(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> R<Value> {
    record(d, it, ConsoleLevel::Error, a)
}

pub(crate) fn install(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    let g = *d.global.get().expect("installed");
    let console = d.obj(it, it.object_prototype())?;
    for (name, h) in [("log", log as H), ("warn", warn), ("error", error)] {
        let key = d.key(it, name)?;
        let f = d.add_fn(it, h, name, 0, false)?;
        it.define_data(console, key, Value::Object(f), true, true, true)?;
    }
    // `console[Symbol.toStringTag]` is "console".
    let (ks, kt) = (d.key(it, "Symbol")?, d.key(it, "toStringTag")?);
    let sym = it.get_value(Value::Object(g), ks)?;
    if let Value::Symbol(s) = it.get_value(sym, kt)? {
        let tag = d.js_str(it, "console")?;
        it.define_data(console, PropertyKey::Symbol(s), tag, false, false, true)?;
    }
    let k = d.key(it, "console")?;
    it.define_data(g, k, Value::Object(console), true, false, true)
}

// ------------------------------------------------------------------------------------------ driver API

impl Browser {
    /// Replace the bounds. Call it before scripts run. A smaller `max_records` drops the oldest records now.
    pub fn set_console_limits(&mut self, l: ConsoleLimits) {
        let c = &self.dom.console;
        c.lim.set(l);
        let mut st = c.st.borrow_mut();
        while st.q.len() > l.max_records {
            c.drop_oldest(&mut st);
        }
    }

    pub fn console_stats(&self) -> ConsoleStats {
        let st = self.dom.console.st.borrow();
        ConsoleStats { records: st.q.len(), values: st.values, calls: st.calls, dropped: st.dropped }
    }

    /// The kept records, oldest first. Empties the sink. The values are no longer rooted: see the header.
    pub fn take_console_records(&mut self) -> Vec<ConsoleRecord> {
        let mut st = self.dom.console.st.borrow_mut();
        st.values = 0;
        st.q.drain(..).collect()
    }

    /// The text of a record (see the header for the gates). Runs no JavaScript. Returns the text to the caller only.
    pub fn format_console_record(&mut self, r: &ConsoleRecord) -> R<String> {
        let (dom, max) = (self.dom.clone(), self.dom.console.lim.get().max_format_bytes);
        self.it.host_scope(|it| format(&dom, it, &r.args, max))
    }
}

// ------------------------------------------------------------------------------------------ formatter

struct Out {
    s: String,
    max: usize,
}

impl Out {
    fn push(&mut self, d: &Dom, t: &str) -> R<()> {
        if self.s.len().saturating_add(t.len()) > self.max {
            return Err(d.lim("console-format"));
        }
        self.s.push_str(t);
        Ok(())
    }
}

/// Check UTF-8 size and work before allocating a display string. Lone surrogates use U+FFFD.
fn text(d: &Dom, it: &mut Interp<'_>, s: StrId, max: usize) -> R<String> {
    let len = it.heap.str_len(s).map_err(Error::Heap)?;
    if len > max { return Err(d.lim("console-format")); }
    it.charge(1 + (len / 16) as u64)?;
    let mut bytes = 0usize;
    for c in char::decode_utf16(it.heap.str_units(s).map_err(Error::Heap)?.iter().copied()) {
        bytes = bytes.saturating_add(c.unwrap_or(char::REPLACEMENT_CHARACTER).len_utf8());
        if bytes > max { return Err(d.lim("console-format")); }
    }
    it.heap.to_string_lossy(s).map_err(Error::Heap)
}

/// A primitive as `%s` prints it. An object is a fixed tag: no user code runs.
fn plain(d: &Dom, it: &mut Interp<'_>, v: Value, quote: bool, out: &mut Out) -> R<()> {
    it.charge(1)?;
    match v {
        Value::String(s) => {
            let room = out.max.saturating_sub(out.s.len());
            let t = text(d, it, s, room.saturating_sub(if quote { 2 } else { 0 }))?;
            if quote {
                out.push(d, "\"")?;
                out.push(d, &t)?;
                out.push(d, "\"")
            } else { out.push(d, &t) }
        }
        Value::Symbol(s) => {
            let desc = it.heap.symbol_description(s).map_err(Error::Heap)?;
            out.push(d, "Symbol(")?;
            if let Some(desc) = desc {
                let t = text(d, it, desc, out.max.saturating_sub(out.s.len()).saturating_sub(1))?;
                out.push(d, &t)?;
            }
            out.push(d, ")")
        }
        Value::Number(0.0) => out.push(d, "0"),
        Value::BigInt(_) | Value::BigHeap(_) => {
            out.push(d, &it.display(v))?;
            out.push(d, "n")
        }
        Value::Object(o) => out.push(d, if it.is_callable_value(Value::Object(o)) { "[function]" } else { "[object]" }),
        v => out.push(d, &it.display(v)),
    }
}

/// The decimal prefix used by parseInt(..., 10) or parseFloat, over borrowed UTF-16.
fn numeric_prefix(units: &[u16], float: bool) -> f64 {
    let start = units.iter().position(|&u| !is_js_space(u)).unwrap_or(units.len());
    let t = &units[start..];
    let mut i = usize::from(matches!(t.first(), Some(43 | 45)));
    if float && t[i..].starts_with(&[73, 110, 102, 105, 110, 105, 116, 121]) {
        return if t.first() == Some(&45) { f64::NEG_INFINITY } else { f64::INFINITY };
    }
    let mut digits = 0;
    while matches!(t.get(i), Some(48..=57)) { i += 1; digits += 1; }
    if float && t.get(i) == Some(&46) {
        i += 1;
        while matches!(t.get(i), Some(48..=57)) { i += 1; digits += 1; }
    }
    if digits == 0 { return f64::NAN; }
    let mantissa_end = i;
    if float && matches!(t.get(i), Some(69 | 101)) {
        let mut j = i + 1;
        if matches!(t.get(j), Some(43 | 45)) { j += 1; }
        let e0 = j;
        while matches!(t.get(j), Some(48..=57)) { j += 1; }
        i = if j > e0 { j } else { mantissa_end };
    }
    string_to_number(&t[..i])
}

/// Numeric formatting stays data-only. Scan borrowed strings within the format-input and step caps.
fn numeric(d: &Dom, it: &mut Interp<'_>, v: Value, float: bool, max: usize) -> R<f64> {
    Ok(match v {
        Value::Number(n) if float => n,
        Value::Number(n) => numeric_prefix(&number_to_string(n).encode_utf16().collect::<Vec<_>>(), false),
        Value::String(s) => {
            let len = it.heap.str_len(s).map_err(Error::Heap)?;
            if len > max { return Err(d.lim("console-format")); }
            it.charge(1 + (len / 16) as u64)?;
            numeric_prefix(it.heap.str_units(s).map_err(Error::Heap)?, float)
        }
        _ => f64::NAN,
    })
}

fn format(d: &Dom, it: &mut Interp<'_>, args: &[Value], max: usize) -> R<String> {
    let mut out = Out { s: String::new(), max };
    let Some((&first, rest)) = args.split_first() else { return Ok(out.s) };
    let target = match first {
        Value::String(s) if !rest.is_empty() => {
            Some(text(d, it, s, max)?)
        }
        _ => None,
    };
    let mut next = 0;
    if let Some(t) = target {
        let mut chars = t.chars().peekable();
        let mut lit = String::new();
        while let Some(c) = chars.next() {
            if c != '%' {
                lit.push(c);
                continue;
            }
            let Some(&spec) = chars.peek() else { lit.push('%'); break };
            if spec == '%' {
                chars.next();
                lit.push('%');
                continue;
            }
            if !"sdifoOc".contains(spec) || next >= rest.len() {
                lit.push('%');
                continue;
            }
            chars.next();
            out.push(d, &lit)?;
            lit.clear();
            let v = rest[next];
            next += 1;
            match spec {
                'c' => {}
                's' => plain(d, it, v, false, &mut out)?,
                'o' | 'O' => plain(d, it, v, true, &mut out)?,
                // `%d` and `%i` print a BigInt with its `n`, as Node does. `%f` is parseFloat of its digits.
                'd' | 'i' if v.is_bigint() => plain(d, it, v, false, &mut out)?,
                _ => {
                    let n = if matches!(v, Value::Symbol(_) | Value::Object(_)) {
                        f64::NAN
                    } else if v.is_bigint() {
                        numeric_prefix(&it.display(v).encode_utf16().collect::<Vec<_>>(), true)
                    } else {
                        numeric(d, it, v, spec == 'f', max)?
                    };
                    plain(d, it, Value::Number(n), false, &mut out)?;
                }
            }
        }
        out.push(d, &lit)?;
    } else if rest.is_empty() {
        plain(d, it, first, false, &mut out)?;
        return Ok(out.s);
    } else {
        plain(d, it, first, false, &mut out)?;
    }
    for &v in &rest[next..] {
        out.push(d, " ")?;
        plain(d, it, v, false, &mut out)?;
    }
    Ok(out.s)
}
