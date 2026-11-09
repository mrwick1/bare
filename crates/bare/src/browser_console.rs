//! `window.console` with a bounded in-memory sink: the methods of the WHATWG Console Standard.
//!
//! What it does
//! - `console` is an own property of the global object (writable, configurable, not enumerable, as WebIDL says for a
//!   namespace). Its methods are ordinary host functions: writable, enumerable, configurable, `length` 0, not
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
//!   never runs user code to log. So an object or function prints as `[object]` or `[function]` for every specifier (an object with an own data string `stack`, else `message`, prints that text),
//!   and `%d %i %f` of an object is `NaN`. Symbols print as `Symbol(desc)` (`%d` gives `NaN`). A BigInt prints with an `n`
//!   (`1n`) for every specifier but `%f`, which is `parseFloat` of its digits (as in Node). `%o` and `%O` print
//!   primitives like `%s`, strings in double quotes without escapes. No grouping, indentation, `%c` styling.
//! - A lone surrogate prints as U+FFFD (display only; the records keep the exact value).
//!
//! Methods (Console Standard). The sink has three levels (`Log`, `Warn`, `Error`; `bare-drive` shows the same three).
//! - `log`, `info`, `debug`, `trace`, `dirxml` record their arguments as `Log`. `dir` and `table` record their first argument as `Log`
//!   (no table layout, no options). `warn` is `Warn`; `error` is `Error`.
//! - `assert(cond, ...data)` records `Assertion failed` and the data as `Error` when `cond` is falsy, and nothing when it is truthy.
//! - `group`, `groupCollapsed` record their arguments (when there are any) and then indent the records that follow by two spaces
//!   for each level; `groupEnd` leaves one level. The depth stops at 64 levels (`MAX_GROUP_DEPTH`); a `groupEnd` at 0 does nothing.
//! - `time(label)`, `timeLog(label, ...data)`, `timeEnd(label)` use the scheduler clock (`performance.now`) and record
//!   `label: 1.234 ms` and the data. `count(label)` records `label: N`; `countReset(label)` sets N to 0. The label is `default` when
//!   it is undefined. A label is a string, a primitive as `%s` prints it, or `[object]` (no user code runs). A missing label of
//!   `timeLog`, `timeEnd` or `countReset`, and a second `time` of a label, record a `Warn`, as the Standard says.
//! - The label tables hold at most 1,024 labels each (`MAX_LABELS`) of at most 256 UTF-8 bytes (`MAX_LABEL_BYTES`). Over either bound a
//!   call keeps nothing and records one `Warn`.
//! - `clear` does nothing. There is no filtering and no devtools.
//!
//! Not compared with a real browser.

use crate::browser::*;
use bare_js::eval::{Error, Interp};
use bare_js::heap::{Property, PropertyDescriptor};
use bare_js::value::{ObjId, PropertyKey, StrId, Value, is_js_space, number_to_string, string_to_number};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
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
    /// The depth of `console.group` when the call was made (at most `MAX_GROUP_DEPTH`).
    pub indent: u8,
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

/// Labels of `time` and of `count`, each table. See the header.
const MAX_LABELS: usize = 1024;
const MAX_LABEL_BYTES: usize = 256;
const MAX_GROUP_DEPTH: u8 = 64;

#[derive(Default)]
struct CState {
    q: VecDeque<ConsoleRecord>,
    values: usize,
    calls: u64,
    dropped: u64,
    /// Open `console.group` levels.
    depth: u8,
    /// The label tables, made by the first `time` or `count` (most pages never call them; `Dom` stays small).
    labels: Option<Box<Labels>>,
}

#[derive(Default)]
struct Labels {
    /// Start of each running timer (label, scheduler clock in ms).
    timers: HashMap<String, f64>,
    counts: HashMap<String, u64>,
}

impl CState {
    fn lab(&mut self) -> &mut Labels {
        self.labels.get_or_insert_with(Default::default)
    }
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
    let indent = st.depth;
    st.q.push_back(ConsoleRecord { seq, level, indent, args: args.to_vec() });
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
    st.q.push_back(ConsoleRecord { seq, level, indent: 0, args: vec![Value::String(s)] });
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
/// `dir` and `table`: the first argument only (no options, no layout).
fn first(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> R<Value> {
    record(d, it, ConsoleLevel::Log, &a[..a.len().min(1)])
}
fn nothing(_: &Dom, _: &mut Interp<'_>, _: Value, _: &[Value]) -> R<Value> {
    Ok(Value::Undefined)
}

fn assert(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> R<Value> {
    if a.first().map_or(Ok(false), |&v| it.heap.to_boolean(v).map_err(|e| it.heap_error(e)))? {
        return Ok(Value::Undefined);
    }
    let mut args = vec![d.js_str(it, "Assertion failed")?];
    args.extend_from_slice(a.get(1..).unwrap_or_default());
    record(d, it, ConsoleLevel::Error, &args)
}

fn group(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> R<Value> {
    if !a.is_empty() {
        record(d, it, ConsoleLevel::Log, a)?;
    }
    let mut st = d.console.st.borrow_mut();
    st.depth = (st.depth + 1).min(MAX_GROUP_DEPTH);
    Ok(Value::Undefined)
}

fn group_end(d: &Dom, _: &mut Interp<'_>, _: Value, _: &[Value]) -> R<Value> {
    let mut st = d.console.st.borrow_mut();
    st.depth = st.depth.saturating_sub(1);
    Ok(Value::Undefined)
}

/// A one-string record of the host (`label: 1.234 ms`, a warning) followed by `rest`.
fn say(d: &Dom, it: &mut Interp<'_>, level: ConsoleLevel, text: &str, rest: &[Value]) -> R<Value> {
    let mut args = vec![d.js_str(it, text)?];
    args.extend_from_slice(rest);
    record(d, it, level, &args)
}

/// The label of a call: `default` for undefined, else a string, a primitive as `%s` prints it, or `[object]`. `None` after the
/// warning for a label over `MAX_LABEL_BYTES` (nothing is kept).
fn label(d: &Dom, it: &mut Interp<'_>, v: Option<&Value>, name: &str) -> R<Option<String>> {
    let t = match v.copied() {
        None | Some(Value::Undefined) => "default".to_string(),
        Some(Value::String(s)) => {
            if it.heap.str_len(s).map_err(Error::Heap)? > MAX_LABEL_BYTES {
                say(d, it, ConsoleLevel::Warn, &format!("console.{name}: the label is longer than {MAX_LABEL_BYTES} bytes and is not kept"), &[])?;
                return Ok(None);
            }
            text(d, it, s, MAX_LABEL_BYTES)?
        }
        Some(Value::Object(_)) => "[object]".to_string(),
        Some(v) => it.display(v),
    };
    if t.len() > MAX_LABEL_BYTES {
        say(d, it, ConsoleLevel::Warn, &format!("console.{name}: the label is longer than {MAX_LABEL_BYTES} bytes and is not kept"), &[])?;
        return Ok(None);
    }
    Ok(Some(t))
}

fn full(d: &Dom, it: &mut Interp<'_>, name: &str) -> R<Value> {
    say(d, it, ConsoleLevel::Warn, &format!("console.{name}: {MAX_LABELS} labels are in use, so this label is not kept"), &[])
}

fn time(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> R<Value> {
    let Some(l) = label(d, it, a.first(), "time")? else { return Ok(Value::Undefined) };
    let now = d.now_ms();
    let mut st = d.console.st.borrow_mut();
    if st.lab().timers.contains_key(&l) {
        drop(st);
        return say(d, it, ConsoleLevel::Warn, &format!("Timer '{l}' already exists"), &[]);
    }
    if st.lab().timers.len() >= MAX_LABELS {
        drop(st);
        return full(d, it, "time");
    }
    st.lab().timers.insert(l, now);
    Ok(Value::Undefined)
}

fn time_log_or_end(d: &Dom, it: &mut Interp<'_>, a: &[Value], end: bool) -> R<Value> {
    let name = if end { "timeEnd" } else { "timeLog" };
    let Some(l) = label(d, it, a.first(), name)? else { return Ok(Value::Undefined) };
    let now = d.now_ms();
    let start = {
        let mut st = d.console.st.borrow_mut();
        if end { st.lab().timers.remove(&l) } else { st.lab().timers.get(&l).copied() }
    };
    match start {
        Some(t) => say(d, it, ConsoleLevel::Log, &format!("{l}: {:.3} ms", now - t), if end { &[] } else { a.get(1..).unwrap_or_default() }),
        None => say(d, it, ConsoleLevel::Warn, &format!("Timer '{l}' does not exist"), &[]),
    }
}

fn time_log(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> R<Value> {
    time_log_or_end(d, it, a, false)
}
fn time_end(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> R<Value> {
    time_log_or_end(d, it, a, true)
}

fn count(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> R<Value> {
    let Some(l) = label(d, it, a.first(), "count")? else { return Ok(Value::Undefined) };
    let mut st = d.console.st.borrow_mut();
    if st.lab().counts.len() >= MAX_LABELS && !st.lab().counts.contains_key(&l) {
        drop(st);
        return full(d, it, "count");
    }
    let n = st.lab().counts.entry(l.clone()).or_insert(0);
    *n += 1;
    let n = *n;
    drop(st);
    say(d, it, ConsoleLevel::Log, &format!("{l}: {n}"), &[])
}

fn count_reset(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> R<Value> {
    let Some(l) = label(d, it, a.first(), "countReset")? else { return Ok(Value::Undefined) };
    let known = d.console.st.borrow_mut().lab().counts.get_mut(&l).map(|n| *n = 0).is_some();
    if known {
        return Ok(Value::Undefined);
    }
    say(d, it, ConsoleLevel::Warn, &format!("Count for '{l}' does not exist"), &[])
}

/// The global `console` is an accessor until the first read or write (as `CSS`, `crypto` and `Intl`): a page that never reads
/// it does not pay for 19 functions. The first read makes the object and replaces the accessor with a data property.
pub(crate) fn install(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    let g = *d.global.get().expect("installed");
    let f = d.add_fn(it, lazy, "console", 0, false)?;
    let k = d.key(it, "console")?;
    let desc = PropertyDescriptor { get: Some(Some(f)), set: Some(Some(f)), enumerable: Some(false), configurable: Some(true), ..Default::default() };
    it.heap.define_own_property(g, k, &desc).map_err(|e| it.heap_error(e))?;
    Ok(())
}

/// The getter (no argument) and the setter (the new value) of the global.
fn lazy(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> R<Value> {
    let g = *d.global.get().expect("installed");
    let k = d.key(it, "console")?;
    if let Some(&v) = a.first() {
        it.define_data(g, k, v, true, false, true)?;
        return Ok(Value::Undefined);
    }
    let console = d.obj(it, it.object_prototype())?;
    for (name, h) in [
        ("log", log as H),
        ("warn", warn),
        ("error", error),
        ("info", log),
        ("debug", log),
        ("trace", log),
        ("dirxml", log),
        ("dir", first),
        ("table", first),
        ("assert", assert),
        ("group", group),
        ("groupCollapsed", group),
        ("groupEnd", group_end),
        ("time", time),
        ("timeLog", time_log),
        ("timeEnd", time_end),
        ("count", count),
        ("countReset", count_reset),
        ("clear", nothing),
    ] {
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
    it.define_data(g, k, Value::Object(console), true, false, true)?;
    Ok(Value::Object(console))
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
        if r.indent == 0 {
            return self.it.host_scope(|it| format(&dom, it, &r.args, max));
        }
        let pad = " ".repeat(2 * r.indent as usize);
        let text = self.it.host_scope(|it| format(&dom, it, &r.args, max.saturating_sub(pad.len())))?;
        Ok(pad + &text)
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
        Value::Object(_) if it.is_callable_value(v) => out.push(d, "[function]"),
        Value::Object(o) => match error_text(d, it, o, out.max.saturating_sub(out.s.len()))? {
            Some(t) => out.push(d, &t),
            None => out.push(d, "[object]"),
        },
        v => out.push(d, &it.display(v)),
    }
}

/// The own data string `stack`, else `message`, of an object (an error). A getter or a non-string gives `None`: no user code runs.
fn error_text(d: &Dom, it: &mut Interp<'_>, o: ObjId, room: usize) -> R<Option<String>> {
    for k in ["stack", "message"] {
        let key = d.key(it, k)?;
        if let Some(Property::Data { value: Value::String(s), .. }) = it.heap.peek_own_property(o, key).map_err(Error::Heap)? {
            return text(d, it, s, room).map(Some);
        }
    }
    Ok(None)
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
