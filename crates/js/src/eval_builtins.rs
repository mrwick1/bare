// JSON.parse, RegExp and String.prototype for the evaluator. Included by `eval.rs` (`include!`), so it shares
// that module's imports and private items.
//
// What is bound (everything else is absent, so a call is an ordinary TypeError or ReferenceError):
// - `JSON.parse(text)`. A reviver function is `Unsupported`. `JSON.stringify` is in `eval_json_stringify.rs`.
// - `RegExp(pattern, flags)`, `new RegExp(...)`, regexp literals, `exec`, `test`, `toString`, the flag getters,
//   an own `lastIndex` data property. Engine gates (flags v/d)
//   are `Unsupported`, never a different match. They do not call a user `exec`: a regexp whose `exec` was replaced is
//   `Unsupported`.
// - `@@match`, `@@replace` and `@@split` of the argument are read first and called when defined, as the spec says;
//   `RegExp.prototype` has the intrinsic `[@@match]`, `[@@replace]` and `[@@split]`, so a builtin regexp keeps the builtin path. `includes`, `startsWith`, `endsWith` and the
//   `RegExp` constructor use IsRegExp (`@@match`). `new RegExp(x)` for a non-builtin regexp-like `x` is `Unsupported`.
//   `@@search` and `replaceAll` follow the same rule (`eval_string_more.rs`). `matchAll` is not bound.
// - `String.fromCharCode`, `String.fromCodePoint` and `String.prototype`: indexOf, includes, startsWith, endsWith, slice, substring,
//   charAt, charCodeAt, split, match, replace, concat, trim, toLowerCase, toUpperCase, toString, valueOf.
//   These are the string methods the inspected staging bundles called first (see docs/staging-js-requirements.md).
//   `String.raw` and the batch 1 methods (at, codePointAt, lastIndexOf, padStart, padEnd, repeat, replaceAll, search,
//   normalize, trimStart, trimEnd, trimLeft, trimRight, isWellFormed, toWellFormed, substr, toLocaleLowerCase,
//   toLocaleUpperCase) are in `eval_string_more.rs`; `call_ext` forwards them. `matchAll` is not bound.
//
// State: a regexp object is an ordinary heap object. Its compiled pattern lives in `Ext::regexps`, keyed by the
// object handle (a handle includes the slot generation, so a reused slot never sees old data). `collect` drops the
// entries of dead objects. The source and flags strings of live entries are roots. No code here collects.

use crate::json;
use crate::regexp as rx;

const SYNTAX_ERROR: usize = 4;
const UNSET: (u32, u32) = (u32::MAX, u32::MAX);
/// The regexp scratch memory is released when it grows beyond this many bytes.
const SCRATCH_KEEP: usize = 1 << 22;

/// Which RegExp or JSON bound ended a run. Never catchable by `try`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegexpLimit {
    /// Pattern too long, nested too deep, too many instructions or groups.
    Compile,
    /// One match attempt used more than `BuiltinLimits::regexp_steps`.
    Steps,
    /// The backtrack stack reached `regexp_stack_frames`.
    Stack,
    /// The input is longer than `regexp_input_units`.
    Input,
    /// One global match, replace or split would hold more than `max_matches` capture slots.
    Matches,
    /// A split result would hold more than `max_items` elements.
    Items,
}

/// Bounds of the bindings. They add to `EvalLimits` and the heap limits. The match work of every call is also
/// charged to `EvalLimits::max_steps` (one step per regexp work unit; string scans and copies one step per 8 or 32 units).
#[derive(Clone, Copy, Debug)]
pub struct BuiltinLimits {
    /// Work of one regexp match attempt.
    pub regexp_steps: u64,
    /// Longest input (UTF-16 units) a regexp may scan.
    pub regexp_input_units: usize,
    /// Backtrack stack frames of one match attempt.
    pub regexp_stack_frames: usize,
    /// Capture slots (matches times groups plus one) one global match, replace or split may hold.
    pub max_matches: usize,
    /// Elements one `split` result may hold.
    pub max_items: usize,
    /// `JSON.parse` limits.
    pub json: json::JsonLimits,
    /// `JSON.stringify` limits.
    pub stringify: StringifyLimits,
}

impl Default for BuiltinLimits {
    fn default() -> Self {
        BuiltinLimits {
            regexp_steps: 50_000_000,
            regexp_input_units: 1 << 24,
            regexp_stack_frames: 1 << 20,
            max_matches: 1 << 22,
            max_items: 1 << 20,
            json: json::JsonLimits { max_source_units: 1 << 24, max_depth: 1024, max_nodes: 1 << 22, max_text_units: 1 << 24 },
            stringify: StringifyLimits::default(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Bi {
    JsonParse,
    JsonStringify,
    RegExpCtor,
    ReExec,
    ReTest,
    ReToString,
    /// `RegExp.prototype[@@match]`, `[@@replace]`, `[@@split]`: the builtin operations behind the String methods.
    ReSymMatch,
    ReSymReplace,
    ReSymSplit,
    ReGetter(u8),
    StrFromCharCode,
    StrFromCodePoint,
    StrToString,
    StrValueOf,
    StrIndexOf,
    StrIncludes,
    StrStartsWith,
    StrEndsWith,
    StrSlice,
    StrSubstring,
    StrCharAt,
    StrCharCodeAt,
    StrSplit,
    StrMatch,
    StrReplace,
    StrConcat,
    StrTrim,
    StrToLower,
    StrToUpper,
    // Batch 1 (`eval_string_more.rs`).
    StrRaw,
    StrAt,
    StrCodePointAt,
    StrLastIndexOf,
    StrPadStart,
    StrPadEnd,
    StrRepeat,
    StrReplaceAll,
    StrSearch,
    StrNormalize,
    StrTrimStart,
    StrTrimEnd,
    StrIsWellFormed,
    StrToWellFormed,
    StrSubstr,
    StrToLocaleLower,
    StrToLocaleUpper,
    ReSymSearch,
    StrMatchAll,
    ReSymMatchAll,
}

const RE_GETTERS: [&str; 9] = ["source", "flags", "global", "ignoreCase", "multiline", "dotAll", "sticky", "unicode", "hasIndices"];

/// What `str_hook` found for a String method argument.
enum Hook {
    /// A user method ran; this is its result.
    Done(Value),
    /// The argument is a builtin regexp that still has the intrinsic method: run the builtin operation on it.
    Builtin(ObjId),
    /// No method: convert the argument to a string.
    Absent,
}

/// A regexp literal, compiled once by the first walk. The object is made fresh on every evaluation.
struct Lit {
    re: Rc<rx::Regex>,
    source: Vec<u16>,
    flags: String,
    strs: Option<(StrId, StrId)>,
}

struct ReData {
    re: Rc<rx::Regex>,
    source: StrId,
    flags: StrId,
}

/// State of the bindings. Every handle in it is a root (see `ext_roots`) except `regexps` keys and `cache` keys.
struct Ext {
    bl: BuiltinLimits,
    string_proto: ObjId,
    regexp_proto: ObjId,
    json_obj: ObjId,
    regexp_ctor: Option<ObjId>,
    exec_fn: Option<ObjId>,
    regexps: FxMap<ObjId, ReData>,
    lits: FxMap<(u32, u32), Lit>,
    /// Compiled patterns of `RegExp(src, flags)` by string handles. Cleared by `collect`.
    cache: FxMap<(StrId, StrId), (Rc<rx::Regex>, StrId, StrId)>,
    scratch: rx::Scratch,
    k_last_index: PropertyKey,
    k_index: PropertyKey,
    k_input: PropertyKey,
    k_groups: PropertyKey,
    k_exec: PropertyKey,
}

impl Ext {
    fn new(heap: &mut Heap, object_proto: ObjId) -> Result<Ext, HeapError> {
        let string_proto = heap.new_object(Some(object_proto))?;
        let regexp_proto = heap.new_object(Some(object_proto))?;
        let json_obj = heap.new_object(Some(object_proto))?;
        Ok(Ext {
            bl: BuiltinLimits::default(),
            string_proto,
            regexp_proto,
            json_obj,
            regexp_ctor: None,
            exec_fn: None,
            regexps: FxMap::default(),
            lits: FxMap::default(),
            cache: FxMap::default(),
            scratch: rx::Scratch::new(),
            k_last_index: heap.key_from_str("lastIndex")?,
            k_index: heap.key_from_str("index")?,
            k_input: heap.key_from_str("input")?,
            k_groups: heap.key_from_str("groups")?,
            k_exec: heap.key_from_str("exec")?,
        })
    }
}

/// Matches of one call: `n` slots per match, `(start, end)` in UTF-16 units, `UNSET` for a group that did not take part.
struct MatchList {
    n: usize,
    v: Vec<(u32, u32)>,
    /// The regexp, when it has named groups: `$<name>` and the `groups` argument of a replacer need them.
    names: Option<Rc<rx::Regex>>,
}

impl MatchList {
    fn len(&self) -> usize {
        self.v.len() / self.n
    }
    fn row(&self, i: usize) -> &[(u32, u32)] {
        &self.v[i * self.n..(i + 1) * self.n]
    }
}

fn is_js_space(c: u16) -> bool {
    matches!(c, 9..=13 | 0x20 | 0xA0 | 0x1680 | 0x2000..=0x200A | 0x2028 | 0x2029 | 0x202F | 0x205F | 0x3000 | 0xFEFF)
}

/// Work-bounded `indexOf` on code units. `budget` is in steps (1 per 8 units). `None` when the budget ran out.
fn find_units(hay: &[u16], needle: &[u16], from: usize, budget: &mut u64) -> Option<Option<usize>> {
    let n = needle.len();
    if from > hay.len() {
        return Some(None);
    }
    if n == 0 {
        return Some(Some(from));
    }
    if hay.len() < n {
        return Some(None);
    }
    let (last, first) = (hay.len() - n, needle[0]);
    let mut i = from;
    let mut work = 0u64;
    let spend = |work: &mut u64, budget: &mut u64| -> bool {
        let steps = *work / 8;
        *work -= steps * 8;
        if steps > *budget {
            *budget = 0;
            return false;
        }
        *budget -= steps;
        true
    };
    while i <= last {
        // Bound each scan before checking its accumulated work.
        let end = (i + 256).min(last + 1);
        match hay[i..end].iter().position(|&c| c == first) {
            None => {
                work += (end - i) as u64;
                if !spend(&mut work, budget) {
                    return None;
                }
                i = end;
                continue;
            }
            Some(k) => {
                i += k;
                work += k as u64 + 1;
            }
        }
        let mut m = 0;
        while m < n {
            let end = (m + 256).min(n);
            let matched = hay[i + m..i + end].iter().zip(&needle[m..end]).take_while(|(a, b)| a == b).count();
            work += matched as u64 + u64::from(matched < end - m);
            if !spend(&mut work, budget) {
                return None;
            }
            m += matched;
            if m < end {
                break;
            }
        }
        if m == n {
            return if spend(&mut work, budget) { Some(Some(i)) } else { None };
        }
        if work >= 8 && !spend(&mut work, budget) {
            return None;
        }
        i += 1;
    }
    if spend(&mut work, budget) { Some(None) } else { None }
}

/// `EscapeRegExpPattern`: the text `source` shows. `/` outside a class and line terminators are escaped.
fn escape_source(p: &[u16]) -> Vec<u16> {
    if p.is_empty() {
        return "(?:)".encode_utf16().collect();
    }
    let mut out = Vec::with_capacity(p.len() + 4);
    let mut in_class = false;
    let lt = |c: u16| match c {
        0x0A => Some("\\n"),
        0x0D => Some("\\r"),
        0x2028 => Some("\\u2028"),
        0x2029 => Some("\\u2029"),
        _ => None,
    };
    let mut i = 0;
    while i < p.len() {
        let c = p[i];
        if c == b'\\' as u16 && i + 1 < p.len() {
            if let Some(e) = lt(p[i + 1]) {
                out.extend(e.encode_utf16());
            } else {
                out.extend_from_slice(&p[i..i + 2]);
            }
            i += 2;
            continue;
        }
        if let Some(e) = lt(c) {
            out.extend(e.encode_utf16());
        } else if c == b'/' as u16 && !in_class {
            out.extend("\\/".encode_utf16());
        } else {
            if c == b'[' as u16 {
                in_class = true;
            } else if c == b']' as u16 {
                in_class = false;
            }
            out.push(c);
        }
        i += 1;
    }
    out
}

fn canon_flags(f: rx::Flags) -> String {
    let mut s = String::new();
    for (on, c) in [(f.has_indices, 'd'), (f.global, 'g'), (f.ignore_case, 'i'), (f.multiline, 'm'), (f.dot_all, 's'), (f.unicode, 'u'), (f.sticky, 'y')] {
        if on {
            s.push(c);
        }
    }
    s
}

fn flag_gate(c: char) -> &'static str {
    match c {
        'u' => "regexp flag u",
        _ => "regexp flag v",
    }
}

impl<'p> Interp<'p> {
    /// Replace the bounds of the bindings (defaults: [`BuiltinLimits::default`]).
    pub fn set_builtin_limits(&mut self, bl: BuiltinLimits) {
        self.ext.bl = bl;
    }

    /// How many regexp objects the evaluator tracks (for tools and tests). `collect` drops the dead ones.
    pub fn regexp_objects(&self) -> usize {
        self.ext.regexps.len()
    }

    // ------------------------------------------------------------ hooks for eval.rs

    /// The permanent roots of the bindings, and the strings of every live regexp.
    fn ext_roots(&self, v: &mut Vec<Value>) {
        let e = &self.ext;
        v.extend([e.string_proto, e.regexp_proto, e.json_obj].map(Value::Object));
        v.extend(e.regexp_ctor.into_iter().chain(e.exec_fn).map(Value::Object));
        for k in [e.k_last_index, e.k_index, e.k_input, e.k_groups, e.k_exec] {
            if let PropertyKey::String(s) = k {
                v.push(Value::String(s));
            }
        }
        for d in e.regexps.values() {
            v.push(Value::String(d.source));
            v.push(Value::String(d.flags));
        }
        for l in e.lits.values() {
            if let Some((a, b)) = l.strs {
                v.push(Value::String(a));
                v.push(Value::String(b));
            }
        }
    }

    /// Forget the regexps of dead objects and every cached string handle (called by `collect`, before `Heap::collect`).
    fn ext_prune(&mut self, seen: &Seen) {
        self.ext.regexps.retain(|o, _| seen.contains(o));
        self.ext.cache.clear();
    }

    /// First walk: compile a literal now, so a bad pattern fails `Interp::new` before any script effect.
    /// There is no early SyntaxError channel in `Interp::new`, so an invalid pattern is reported as `Unsupported`
    /// with the literal's span. The parser checks only the flags, never the pattern.
    fn ext_walk_regexp(&mut self, e: ExprId, span: Span) -> Res<()> {
        let Some((pat, flags)) = self.prog.regexp(span) else { return Err(unsupported("regexp literal", span)) };
        let units: Vec<u16> = pat.encode_utf16().collect();
        let re = match rx::Regex::new(&units, flags) {
            Ok(re) => re,
            Err(rx::CompileError::Syntax { .. }) => return Err(unsupported("regexp literal: invalid pattern (early SyntaxError)", span)),
            Err(rx::CompileError::Unsupported { what, .. }) => return Err(unsupported(what, span)),
            Err(rx::CompileError::InvalidFlags(_)) => return Err(unsupported("regexp literal: invalid flags (early SyntaxError)", span)),
            Err(rx::CompileError::UnsupportedFlag(c)) => return Err(unsupported(flag_gate(c), span)),
            Err(rx::CompileError::Limit(_)) => return Err(unsupported("regexp literal: compile limit", span)),
        };
        let flags = canon_flags(re.flags());
        self.ext.lits.insert((self.cur as u32, e.index() as u32), Lit { re: Rc::new(re), source: escape_source(&units), flags, strs: None });
        Ok(())
    }

    /// A regexp literal evaluates to a new object every time. Only the immutable compiled pattern is shared.
    fn ext_eval_regexp(&mut self, e: ExprId) -> Res<Value> {
        let Some(l) = self.ext.lits.get(&(self.cur as u32, e.index() as u32)) else { return Err(unsupported("regexp literal", Span::default())) };
        let (re, strs) = (l.re.clone(), l.strs);
        let (src, fl) = match strs {
            Some(p) => p,
            None => {
                let (u, f) = (l.source.clone(), l.flags.clone());
                let p = (h!(self, self.heap.intern_vec(u)), h!(self, self.heap.intern_str(&f)));
                if let Some(l) = self.ext.lits.get_mut(&(self.cur as u32, e.index() as u32)) {
                    l.strs = Some(p);
                }
                p
            }
        };
        Ok(Value::Object(self.make_regexp(re, src, fl)?))
    }

    // ------------------------------------------------------------ installation

    fn install_ext(&mut self) -> Res<()> {
        let g = self.global;
        let at = |s: &mut Self, o: ObjId, name: &str, v: Value| -> Res<()> {
            let k = h!(s, s.heap.key_from_str(name));
            s.def(o, k, v, true, false, true)
        };
        // JSON
        let j = self.ext.json_obj;
        self.method(j, "parse", Nat::Ext(Bi::JsonParse), 2)?;
        self.method(j, "stringify", Nat::Ext(Bi::JsonStringify), 3)?;
        self.sc_tag(j, "JSON")?;
        at(self, g, "JSON", Value::Object(j))?;
        // RegExp
        let rp = self.ext.regexp_proto;
        let c = self.native(Nat::Ext(Bi::RegExpCtor), "RegExp", 2)?;
        self.def(c, self.k_prototype, Value::Object(rp), false, false, false)?;
        self.def(rp, self.k_constructor, Value::Object(c), true, false, true)?;
        at(self, g, "RegExp", Value::Object(c))?;
        self.ext.regexp_ctor = Some(c);
        self.sc_getter(c, self.wk_key(WK_SPECIES), "get [Symbol.species]", ScFn::Species)?;
        self.ext.exec_fn = Some(self.method(rp, "exec", Nat::Ext(Bi::ReExec), 1)?);
        self.method(rp, "test", Nat::Ext(Bi::ReTest), 1)?;
        self.method(rp, "toString", Nat::Ext(Bi::ReToString), 0)?;
        for (wk, x, name, len) in [(WK_MATCH, Bi::ReSymMatch, "[Symbol.match]", 1), (WK_REPLACE, Bi::ReSymReplace, "[Symbol.replace]", 2), (WK_SPLIT, Bi::ReSymSplit, "[Symbol.split]", 2)] {
            let f = self.native(Nat::Ext(x), name, len)?;
            self.def(rp, self.wk_key(wk), Value::Object(f), true, false, true)?;
        }
        for (i, name) in RE_GETTERS.iter().enumerate() {
            let f = self.native(Nat::Ext(Bi::ReGetter(i as u8)), &format!("get {name}"), 0)?;
            let k = h!(self, self.heap.key_from_str(name));
            let d = PropertyDescriptor { get: Some(Some(f)), set: Some(None), enumerable: Some(false), configurable: Some(true), ..Default::default() };
            h!(self, self.heap.define_own_property(rp, k, &d));
        }
        // String
        let sk = h!(self, self.heap.key_from_str("String"));
        let Some(Property::Data { value: Value::Object(sc), .. }) = h!(self, self.heap.get_own_property(g, sk)) else { return Err(unsupported("String constructor missing", Span::default())) };
        let sp = self.ext.string_proto;
        self.def(sc, self.k_prototype, Value::Object(sp), false, false, false)?;
        self.def(sp, self.k_constructor, Value::Object(sc), true, false, true)?;
        // `String.prototype` is a String object with the empty string: `length` is 0 and is not configurable.
        self.def(sp, self.k_length, Value::Number(0.0), false, false, false)?;
        self.method(sc, "fromCharCode", Nat::Ext(Bi::StrFromCharCode), 1)?;
        self.method(sc, "fromCodePoint", Nat::Ext(Bi::StrFromCodePoint), 1)?;
        let methods: [(&str, Bi, u32); 17] = [
            ("toString", Bi::StrToString, 0),
            ("valueOf", Bi::StrValueOf, 0),
            ("indexOf", Bi::StrIndexOf, 1),
            ("includes", Bi::StrIncludes, 1),
            ("startsWith", Bi::StrStartsWith, 1),
            ("endsWith", Bi::StrEndsWith, 1),
            ("slice", Bi::StrSlice, 2),
            ("substring", Bi::StrSubstring, 2),
            ("charAt", Bi::StrCharAt, 1),
            ("charCodeAt", Bi::StrCharCodeAt, 1),
            ("split", Bi::StrSplit, 2),
            ("match", Bi::StrMatch, 1),
            ("replace", Bi::StrReplace, 2),
            ("concat", Bi::StrConcat, 1),
            ("trim", Bi::StrTrim, 0),
            ("toLowerCase", Bi::StrToLower, 0),
            ("toUpperCase", Bi::StrToUpper, 0),
        ];
        for (name, x, len) in methods {
            self.method(sp, name, Nat::Ext(x), len)?;
        }
        self.install_string_more(sc, sp, rp)
    }

    // ------------------------------------------------------------ budgets and small helpers

    /// Charge `n` steps. A bulk charge is checked after the work, so the work itself must be bounded by the caller.
    fn spend(&mut self, n: u64) -> Res<()> {
        self.steps = self.steps.saturating_add(n);
        if self.steps > self.step_limit { Err(Error::Limit(LimitKind::Steps)) } else { Ok(()) }
    }

    fn spend_units(&mut self, n: usize) -> Res<()> {
        self.spend(n as u64 / 32)
    }

    fn this_str(&mut self, this: Value, name: &str) -> Res<StrId> {
        if is_nullish(this) {
            return Err(self.type_err(&format!("String.prototype.{name} called on null or undefined")));
        }
        self.coerce_string(this)
    }

    /// `ToIntegerOrInfinity`.
    fn int_arg(&mut self, v: Value) -> Res<f64> {
        let n = self.coerce_number(v)?;
        Ok(if n.is_nan() { 0.0 } else { n.trunc() })
    }

    fn sub(&mut self, s: StrId, a: usize, b: usize) -> Res<StrId> {
        self.spend_units(b.saturating_sub(a))?;
        Ok(h!(self, self.heap.substring(s, a, b)))
    }

    fn str_value(&mut self, s: &str) -> Res<Value> {
        Ok(Value::String(h!(self, self.heap.intern_str(s))))
    }

    fn new_str(&mut self, units: Vec<u16>) -> Res<Value> {
        Ok(Value::String(h!(self, self.heap.intern_vec(units))))
    }

    /// The String-method hook of a well-known symbol (`@@match`, `@@replace`, `@@split`). The spec reads
    /// `GetMethod(target, @@sym)` before it converts `this` to a string and calls a defined method with `extra`.
    /// `RegExp.prototype` holds the intrinsic methods, so a builtin regexp that still finds its own intrinsic takes
    /// the builtin path. A missing method means the argument is converted to a string, as in the spec.
    fn str_hook(&mut self, this: Value, name: &str, target: Value, wk: usize, extra: &[Value]) -> Res<Hook> {
        if is_nullish(this) {
            return Err(self.type_err(&format!("String.prototype.{name} called on null or undefined")));
        }
        if is_nullish(target) {
            return Ok(Hook::Absent);
        }
        let m = self.get_prop(target, self.wk_key(wk))?;
        if is_nullish(m) {
            return Ok(Hook::Absent);
        }
        if !self.is_callable(m) {
            let t = self.display(m);
            return Err(self.type_err(&format!("{t} is not a function")));
        }
        if let (Value::Object(mo), Some(r)) = (m, self.regexp_of(target)) {
            let own = match wk {
                WK_MATCH => Bi::ReSymMatch,
                WK_REPLACE => Bi::ReSymReplace,
                WK_SEARCH => Bi::ReSymSearch,
                WK_MATCH_ALL => Bi::ReSymMatchAll,
                _ => Bi::ReSymSplit,
            };
            if matches!(self.funcs.get(&mo), Some(Callee::Native(Nat::Ext(b))) if *b == own) {
                return Ok(Hook::Builtin(r));
            }
        }
        Ok(Hook::Done(self.call_fn(m, target, extra)?))
    }

    /// `IsRegExp`: a defined `@@match` decides, otherwise a builtin regexp is one.
    fn is_regexp(&mut self, v: Value) -> Res<bool> {
        if !matches!(v, Value::Object(_)) {
            return Ok(false);
        }
        let m = self.get_prop(v, self.wk_key(WK_MATCH))?;
        if !matches!(m, Value::Undefined) {
            return self.truthy(m);
        }
        Ok(self.regexp_of(v).is_some())
    }

    fn regexp_of(&self, v: Value) -> Option<ObjId> {
        match v {
            Value::Object(o) if self.ext.regexps.contains_key(&o) => Some(o),
            _ => None,
        }
    }

    /// Budgeted `indexOf` of `needle` in `hay` from `from`.
    fn search(&mut self, hay: StrId, needle: StrId, from: usize) -> Res<Option<usize>> {
        let mut budget = self.step_limit.saturating_sub(self.steps);
        let before = budget;
        let r = {
            let (h, n) = (h!(self, self.heap.str_units(hay)), h!(self, self.heap.str_units(needle)));
            find_units(h, n, from, &mut budget)
        };
        self.steps += before - budget;
        match r {
            Some(r) => Ok(r),
            None => Err(Error::Limit(LimitKind::Steps)),
        }
    }

    // ------------------------------------------------------------ dispatch

    fn call_ext(&mut self, x: Bi, this: Value, args: &[Value], is_new: bool) -> Res<Value> {
        let a0 = arg(args, 0);
        match x {
            Bi::JsonParse => self.json_parse(a0, arg(args, 1)),
            Bi::JsonStringify => self.json_stringify(a0, arg(args, 1), arg(args, 2)),
            Bi::RegExpCtor => self.regexp_ctor(a0, arg(args, 1), is_new),
            Bi::ReExec => {
                let r = self.this_regexp(this, "exec")?;
                let s = self.coerce_string(a0)?;
                match self.re_exec_raw(r, s)? {
                    None => Ok(Value::Null),
                    Some((m, named)) => self.match_result(r, s, &m, named),
                }
            }
            Bi::ReTest => {
                let r = self.this_regexp(this, "test")?;
                let s = self.coerce_string(a0)?;
                if self.user_exec(r)? {
                    return self.re_test_g(r, s);
                }
                Ok(Value::Bool(self.re_exec_raw(r, s)?.is_some()))
            }
            Bi::ReToString => {
                let Value::Object(o) = this else { return Err(self.type_err("RegExp.prototype.toString requires that 'this' be an Object")) };
                let ks = h!(self, self.heap.key_from_str("source"));
                let kf = h!(self, self.heap.key_from_str("flags"));
                let src = self.get_prop(Value::Object(o), ks)?;
                let src = self.coerce_string(src)?;
                let fl = self.get_prop(Value::Object(o), kf)?;
                let fl = self.coerce_string(fl)?;
                let mut out: Vec<u16> = vec![b'/' as u16];
                self.append_string(&mut out, h!(self, self.heap.str_units(src)))?;
                out.push(b'/' as u16);
                self.append_string(&mut out, h!(self, self.heap.str_units(fl)))?;
                self.new_str(out)
            }
            Bi::ReGetter(i) => self.re_getter(this, i),
            Bi::StrFromCharCode => {
                let mut out = Vec::with_capacity(args.len());
                for &a in args {
                    self.tick()?;
                    out.push(to_uint32(self.coerce_number(a)?) as u16);
                }
                self.new_str(out)
            }
            Bi::StrFromCodePoint => {
                let mut out = Vec::with_capacity(args.len());
                for &a in args {
                    self.tick()?;
                    let x = self.coerce_number(a)?;
                    let Some(c) = (x.trunc() == x && (0.0..=1114111.0).contains(&x)).then_some(x as u32) else {
                        return Err(self.mk_err(RANGE_ERROR, &format!("Invalid code point {}", number_to_string(x))));
                    };
                    match char::from_u32(c) {
                        Some(ch) => out.extend_from_slice(ch.encode_utf16(&mut [0; 2])),
                        None => out.push(c as u16), // a lone surrogate is one unit
                    }
                }
                self.new_str(out)
            }
            Bi::StrToString | Bi::StrValueOf => match self.this_string_value(this)? {
                Some(s) => Ok(Value::String(s)),
                None => Err(self.type_err(&format!("String.prototype.{} requires that 'this' be a String", if x == Bi::StrToString { "toString" } else { "valueOf" }))),
            },
            Bi::StrIndexOf => {
                let s = self.this_str(this, "indexOf")?;
                let needle = self.coerce_string(a0)?;
                let len = h!(self, self.heap.str_len(s));
                let from = self.int_arg(arg(args, 1))?.clamp(0.0, len as f64) as usize;
                Ok(Value::Number(self.search(s, needle, from)?.map_or(-1.0, |i| i as f64)))
            }
            Bi::StrIncludes | Bi::StrStartsWith | Bi::StrEndsWith => self.str_search_method(x, this, args),
            Bi::StrSlice => {
                let s = self.this_str(this, "slice")?;
                let len = h!(self, self.heap.str_len(s)) as f64;
                let start = self.int_arg(a0)?;
                let end = if matches!(arg(args, 1), Value::Undefined) { len } else { self.int_arg(arg(args, 1))? };
                let rel = |n: f64| if n < 0.0 { (len + n).max(0.0) } else { n.min(len) };
                let (from, to) = (rel(start) as usize, rel(end) as usize);
                if from >= to {
                    return self.str_value("");
                }
                Ok(Value::String(self.sub(s, from, to)?))
            }
            Bi::StrSubstring => {
                let s = self.this_str(this, "substring")?;
                let len = h!(self, self.heap.str_len(s)) as f64;
                let a = self.int_arg(a0)?.clamp(0.0, len);
                let b = if matches!(arg(args, 1), Value::Undefined) { len } else { self.int_arg(arg(args, 1))?.clamp(0.0, len) };
                Ok(Value::String(self.sub(s, a.min(b) as usize, a.max(b) as usize)?))
            }
            Bi::StrCharAt => {
                let s = self.this_str(this, "charAt")?;
                let len = h!(self, self.heap.str_len(s));
                let p = self.int_arg(a0)?;
                if p < 0.0 || p >= len as f64 {
                    return self.str_value("");
                }
                Ok(Value::String(self.sub(s, p as usize, p as usize + 1)?))
            }
            Bi::StrCharCodeAt => {
                let s = self.this_str(this, "charCodeAt")?;
                let len = h!(self, self.heap.str_len(s));
                let p = self.int_arg(a0)?;
                if p < 0.0 || p >= len as f64 {
                    return Ok(Value::Number(f64::NAN));
                }
                Ok(Value::Number(h!(self, self.heap.code_unit_at(s, p as usize)).unwrap_or(0) as f64))
            }
            Bi::StrSplit => self.str_split(this, a0, arg(args, 1)),
            Bi::StrMatch => {
                let hook = self.str_hook(this, "match", a0, WK_MATCH, &[this])?;
                let s = self.this_str(this, "match")?;
                let r = match hook {
                    Hook::Done(v) => return Ok(v),
                    Hook::Builtin(r) => r,
                    Hook::Absent => {
                        let p = if matches!(a0, Value::Undefined) { h!(self, self.heap.intern_str("")) } else { self.coerce_string(a0)? };
                        let empty = h!(self, self.heap.intern_str(""));
                        self.regexp_create(p, empty)?
                    }
                };
                self.re_match(r, s)
            }
            Bi::StrReplace => {
                let (sv, rv) = (a0, arg(args, 1));
                let hook = self.str_hook(this, "replace", sv, WK_REPLACE, &[this, rv])?;
                let s = self.this_str(this, "replace")?;
                let func = if self.is_callable(rv) { Some(rv) } else { None };
                match hook {
                    Hook::Done(v) => return Ok(v),
                    Hook::Builtin(r) => return self.re_replace(r, s, func, rv),
                    Hook::Absent => {}
                }
                let search = self.coerce_string(sv)?;
                let tpl = if func.is_none() { Some(self.coerce_string(rv)?) } else { None };
                let Some(p) = self.search(s, search, 0)? else { return Ok(Value::String(s)) };
                let m = h!(self, self.heap.str_len(search));
                let ml = MatchList { n: 1, v: vec![(p as u32, (p + m) as u32)], names: None };
                self.build_replacement(&ml, s, func, tpl)
            }
            Bi::ReSymMatch => {
                let r = self.this_regexp(this, "[Symbol.match]")?;
                let s = self.coerce_string(a0)?;
                self.re_match(r, s)
            }
            Bi::ReSymReplace => {
                let r = self.this_regexp(this, "[Symbol.replace]")?;
                let s = self.coerce_string(a0)?;
                let rv = arg(args, 1);
                let func = if self.is_callable(rv) { Some(rv) } else { None };
                self.re_replace(r, s, func, rv)
            }
            Bi::ReSymSplit => {
                let r = self.this_regexp(this, "[Symbol.split]")?;
                let s = self.coerce_string(a0)?;
                let limv = arg(args, 1);
                let lim = if matches!(limv, Value::Undefined) { u32::MAX } else { to_uint32(self.coerce_number(limv)?) };
                self.re_split(r, s, lim)
            }
            Bi::StrConcat => {
                let s = self.this_str(this, "concat")?;
                let mut out: Vec<u16> = Vec::new();
                self.append_string(&mut out, h!(self, self.heap.str_units(s)))?;
                for &a in args {
                    self.tick()?;
                    let t = self.coerce_string(a)?;
                    self.append_string(&mut out, h!(self, self.heap.str_units(t)))?;
                }
                self.spend_units(out.len())?;
                self.new_str(out)
            }
            Bi::StrTrim => {
                let s = self.this_str(this, "trim")?;
                self.spend_units(h!(self, self.heap.str_len(s)))?;
                let (a, b) = {
                    let u = h!(self, self.heap.str_units(s));
                    let a = u.iter().position(|&c| !is_js_space(c)).unwrap_or(u.len());
                    let b = u.iter().rposition(|&c| !is_js_space(c)).map_or(a, |i| i + 1);
                    (a, b)
                };
                Ok(Value::String(self.sub(s, a, b)?))
            }
            Bi::StrToLower | Bi::StrToUpper => {
                let s = self.this_str(this, if x == Bi::StrToLower { "toLowerCase" } else { "toUpperCase" })?;
                let out = self.case_map(s, x == Bi::StrToUpper)?;
                self.new_str(out)
            }
            _ => self.call_str_more(x, this, args),
        }
    }

    /// `String.prototype.match` on a builtin regexp.
    fn re_match(&mut self, r: ObjId, s: StrId) -> Res<Value> {
        if self.user_exec(r)? {
            return self.re_match_g(r, s);
        }
        let flags = self.ext.regexps[&r].re.flags();
        if !flags.global {
            return match self.re_exec_raw(r, s)? {
                None => Ok(Value::Null),
                Some((m, named)) => self.match_result(r, s, &m, named),
            };
        }
        let ml = self.collect_matches(r, s, false)?;
        if ml.len() == 0 {
            return Ok(Value::Null);
        }
        let mut items = Vec::with_capacity(ml.len());
        for i in 0..ml.len() {
            self.tick()?;
            let (a, b) = ml.row(i)[0];
            items.push(Value::String(self.sub(s, a as usize, b as usize)?));
        }
        self.new_array_from(&items)
    }

    /// `String.prototype.replace` on a builtin regexp. `rv` is the replacement (`func` when it is callable).
    fn re_replace(&mut self, r: ObjId, s: StrId, func: Option<Value>, rv: Value) -> Res<Value> {
        let tpl = if func.is_none() { Some(self.coerce_string(rv)?) } else { None };
        if self.user_exec(r)? {
            return self.re_replace_g(r, s, func, tpl);
        }
        let flags = self.ext.regexps[&r].re.flags();
        let ml = if flags.global {
            self.collect_matches(r, s, true)?
        } else {
            match self.re_exec_raw(r, s)? {
                None => return Ok(Value::String(s)),
                Some((m, named)) => {
                    let names = if named { Some(self.ext.regexps[&r].re.clone()) } else { None };
                    MatchList { n: m.spans.len(), v: m.spans.iter().map(|x| x.map_or(UNSET, |(a, b)| (a as u32, b as u32))).collect(), names }
                }
            }
        };
        self.build_replacement(&ml, s, func, tpl)
    }

    /// `includes`, `startsWith`, `endsWith`: the argument must not be a regexp.
    fn str_search_method(&mut self, x: Bi, this: Value, args: &[Value]) -> Res<Value> {
        let name = match x {
            Bi::StrIncludes => "includes",
            Bi::StrStartsWith => "startsWith",
            _ => "endsWith",
        };
        let s = self.this_str(this, name)?;
        if self.is_regexp(arg(args, 0))? {
            return Err(self.type_err(&format!("First argument to String.prototype.{name} must not be a regular expression")));
        }
        let needle = self.coerce_string(arg(args, 0))?;
        let len = h!(self, self.heap.str_len(s));
        let m = h!(self, self.heap.str_len(needle));
        let pos = arg(args, 1);
        match x {
            Bi::StrIncludes => {
                let from = self.int_arg(pos)?.clamp(0.0, len as f64) as usize;
                Ok(Value::Bool(self.search(s, needle, from)?.is_some()))
            }
            Bi::StrStartsWith => {
                let from = self.int_arg(pos)?.clamp(0.0, len as f64) as usize;
                self.spend_units(m)?;
                let (h, n) = (h!(self, self.heap.str_units(s)), h!(self, self.heap.str_units(needle)));
                Ok(Value::Bool(from + m <= len && h[from..from + m] == *n))
            }
            _ => {
                let end = if matches!(pos, Value::Undefined) { len } else { self.int_arg(pos)?.clamp(0.0, len as f64) as usize };
                self.spend_units(m)?;
                let (h, n) = (h!(self, self.heap.str_units(s)), h!(self, self.heap.str_units(needle)));
                Ok(Value::Bool(m <= end && h[end - m..end] == *n))
            }
        }
    }

    /// Full-string case mapping (Rust's Unicode tables). Lone surrogates stay. Segments end at ASCII white space and
    /// lone surrogates, which cannot change a final-sigma decision, so the work and the growth are bounded per segment.
    fn case_map(&mut self, s: StrId, upper: bool) -> Res<Vec<u16>> {
        let len = h!(self, self.heap.str_len(s));
        self.spend_units(len)?;
        let src = h!(self, self.heap.str_units(s));
        let mut out: Vec<u16> = Vec::new();
        if src.iter().all(|&c| c < 128) {
            self.append_string(&mut out, src)?;
            for c in out.iter_mut() {
                if upper { c.make_ascii_uppercase_u16() } else { c.make_ascii_lowercase_u16() }
            }
            return Ok(out);
        }
        let cap = self.heap.limits().max_string_units;
        let mut seg = String::new();
        let flush = |seg: &mut String, out: &mut Vec<u16>| -> Res<()> {
            if seg.is_empty() {
                return Ok(());
            }
            let m = if upper { seg.to_uppercase() } else { seg.to_lowercase() };
            if m.encode_utf16().count() > cap.saturating_sub(out.len()) {
                return Err(Error::Limit(LimitKind::Heap(Limit::StringLength)));
            }
            out.extend(m.encode_utf16());
            seg.clear();
            Ok(())
        };
        for r in char::decode_utf16(src.iter().copied()) {
            match r {
                Ok(c) if !c.is_ascii_whitespace() => seg.push(c),
                Ok(c) => {
                    flush(&mut seg, &mut out)?;
                    self.append_string(&mut out, &[c as u16])?;
                }
                Err(e) => {
                    flush(&mut seg, &mut out)?;
                    self.append_string(&mut out, &[e.unpaired_surrogate()])?;
                }
            }
        }
        flush(&mut seg, &mut out)?;
        Ok(out)
    }

    // ------------------------------------------------------------ split

    fn str_split(&mut self, this: Value, sepv: Value, limv: Value) -> Res<Value> {
        let hook = self.str_hook(this, "split", sepv, WK_SPLIT, &[this, limv])?;
        let s = self.this_str(this, "split")?;
        let lim = if matches!(limv, Value::Undefined) { u32::MAX } else { to_uint32(self.coerce_number(limv)?) };
        match hook {
            Hook::Done(v) => return Ok(v),
            Hook::Builtin(r) => {
                return self.re_split(r, s, lim);
            }
            Hook::Absent => {}
        }
        let sep = if matches!(sepv, Value::Undefined) { None } else { Some(self.coerce_string(sepv)?) };
        if lim == 0 {
            return self.new_array_from(&[]);
        }
        let Some(sep) = sep else { return self.new_array_from(&[Value::String(s)]) };
        let size = h!(self, self.heap.str_len(s));
        let m = h!(self, self.heap.str_len(sep));
        let max = self.ext.bl.max_items;
        if size == 0 {
            return if m == 0 { self.new_array_from(&[]) } else { self.new_array_from(&[Value::String(s)]) };
        }
        let mut items: Vec<Value> = Vec::new();
        if m == 0 {
            let n = size.min(lim as usize);
            if n > max {
                return Err(Error::Limit(LimitKind::Regexp(RegexpLimit::Items)));
            }
            items.reserve(n);
            for i in 0..n {
                self.tick()?;
                items.push(Value::String(self.sub(s, i, i + 1)?));
            }
            return self.new_array_from(&items);
        }
        let mut p = 0;
        loop {
            self.tick()?;
            let Some(q) = self.search(s, sep, p)? else { break };
            if items.len() >= max {
                return Err(Error::Limit(LimitKind::Regexp(RegexpLimit::Items)));
            }
            items.push(Value::String(self.sub(s, p, q)?));
            if items.len() as u64 == lim as u64 {
                return self.new_array_from(&items);
            }
            p = q + m;
        }
        if items.len() >= max {
            return Err(Error::Limit(LimitKind::Regexp(RegexpLimit::Items)));
        }
        items.push(Value::String(self.sub(s, p, size)?));
        self.new_array_from(&items)
    }

    /// `RegExp.prototype[@@split]` without the species constructor: the same pieces, no copy of the regexp is made,
    /// and the regexp's own `lastIndex` is never read or written.
    fn split_regexp(&mut self, r: ObjId, s: StrId, lim: u32) -> Res<Value> {
        let re = self.ext.regexps[&r].re.clone();
        let sticky = re.flags().sticky;
        if lim == 0 {
            return self.new_array_from(&[]);
        }
        let size = h!(self, self.heap.str_len(s));
        if size == 0 {
            return match self.run_engine(&re, s, 0)? {
                Some(_) => self.new_array_from(&[]),
                None => self.new_array_from(&[Value::String(s)]),
            };
        }
        let max = self.ext.bl.max_items;
        let mut items: Vec<Value> = Vec::new();
        let (mut p, mut q) = (0usize, 0usize);
        macro_rules! push {
            ($v:expr) => {{
                if items.len() >= max {
                    return Err(Error::Limit(LimitKind::Regexp(RegexpLimit::Items)));
                }
                items.push($v);
                if items.len() as u64 == lim as u64 {
                    return self.new_array_from(&items);
                }
            }};
        }
        while q < size {
            self.tick()?;
            let Some(m) = self.run_engine(&re, s, q)? else {
                if sticky {
                    q = self.advance(&re, s, q)?;
                    continue;
                }
                break;
            };
            let (ms, me) = m.spans[0].unwrap_or((q, q));
            if ms >= size {
                break;
            }
            let e = me.min(size);
            if e == p {
                q = self.advance(&re, s, ms)?;
                continue;
            }
            let piece = self.sub(s, p, ms)?;
            push!(Value::String(piece));
            p = e;
            for g in 1..m.spans.len() {
                let v = match m.spans[g] {
                    Some((a, b)) => Value::String(self.sub(s, a, b)?),
                    None => Value::Undefined,
                };
                push!(v);
            }
            q = p;
        }
        let tail = self.sub(s, p, size)?;
        push!(Value::String(tail));
        self.new_array_from(&items)
    }

    // ------------------------------------------------------------ replace

    /// Build the result of `replace` from the matches. Callbacks run after every match was found, as in the spec.
    fn build_replacement(&mut self, ml: &MatchList, s: StrId, func: Option<Value>, tpl: Option<StrId>) -> Res<Value> {
        let mut out: Vec<u16> = Vec::new();
        let mut next = 0usize;
        for i in 0..ml.len() {
            self.tick()?;
            let row = ml.row(i);
            let (ms, me) = (row[0].0 as usize, row[0].1 as usize);
            let repl: Option<StrId> = if let Some(f) = func {
                let mut cargs: Vec<Value> = Vec::with_capacity(row.len() + 2);
                for &(a, b) in row {
                    cargs.push(if (a, b) == UNSET { Value::Undefined } else { Value::String(self.sub(s, a as usize, b as usize)?) });
                }
                cargs.push(Value::Number(ms as f64));
                cargs.push(Value::String(s));
                if let Some(re) = &ml.names {
                    self.push_groups(&mut cargs, re, s, row)?;
                }
                let rv = self.call_fn(f, Value::Undefined, &cargs)?;
                Some(self.coerce_string(rv)?)
            } else {
                None
            };
            if ms >= next {
                self.append_string(&mut out, &h!(self, self.heap.str_units(s))[next..ms])?;
                match (repl, tpl) {
                    (Some(r), _) => self.append_string(&mut out, h!(self, self.heap.str_units(r)))?,
                    (None, Some(t)) => self.append_subst(&mut out, t, s, row, ml.names.as_deref())?,
                    (None, None) => {}
                }
                next = me;
            }
        }
        let len = h!(self, self.heap.str_len(s));
        if next < len {
            self.append_string(&mut out, &h!(self, self.heap.str_units(s))[next..])?;
        }
        self.spend_units(out.len())?;
        self.new_str(out)
    }

    /// `$<name>` of a template, `t` being the text after `$<`: appends the group and gives how many units to skip (`$<name>` is skipped whole;
    /// without a `>` only the `$` is written).
    #[cold]
    #[inline(never)]
    fn append_named(&self, out: &mut Vec<u16>, t: &[u16], src: &[u16], row: &[(u32, u32)], names: Option<&rx::Regex>) -> Res<usize> {
        let Some(end) = t.iter().position(|&x| x == b'>' as u16) else {
            self.append_string(out, &[b'$' as u16])?;
            return Ok(1);
        };
        let name = &t[..end];
        let g = names.and_then(|re| re.group_names().iter().find(|(n, _)| n.encode_utf16().eq(name.iter().copied())));
        if let Some(&(_, g)) = g
            && row[g as usize] != UNSET
        {
            self.append_string(out, &src[row[g as usize].0 as usize..row[g as usize].1 as usize])?;
        }
        Ok(end + 3)
    }

    /// `GetSubstitution`: `$$ $& $` $' $n $nn`, and `$<name>` when the regexp `names` has named groups.
    fn append_subst(&self, out: &mut Vec<u16>, tpl: StrId, s: StrId, row: &[(u32, u32)], names: Option<&rx::Regex>) -> Res<()> {
        let t = self.heap.str_units(tpl).map_err(Error::Heap)?;
        let src = self.heap.str_units(s).map_err(Error::Heap)?;
        let (ms, me) = (row[0].0 as usize, row[0].1 as usize);
        let ncap = row.len() - 1;
        let dollar = b'$' as u16;
        let digit = |c: Option<u16>| c.and_then(|c| (b'0' as u16..=b'9' as u16).contains(&c).then(|| (c - b'0' as u16) as usize));
        let mut i = 0;
        while i < t.len() {
            let j = t[i..].iter().position(|&c| c == dollar).map_or(t.len(), |k| i + k);
            if j > i {
                self.append_string(out, &t[i..j])?;
                i = j;
                continue;
            }
            let next = t.get(i + 1).copied();
            match next {
                Some(c) if c == dollar => {
                    self.append_string(out, &[dollar])?;
                    i += 2;
                }
                Some(c) if c == b'&' as u16 => {
                    self.append_string(out, &src[ms..me])?;
                    i += 2;
                }
                Some(c) if c == b'`' as u16 => {
                    self.append_string(out, &src[..ms])?;
                    i += 2;
                }
                Some(c) if c == b'\'' as u16 => {
                    self.append_string(out, &src[me.min(src.len())..])?;
                    i += 2;
                }
                Some(c) if c == b'<' as u16 && names.is_some() => i += self.append_named(out, &t[i + 2..], src, row, names)?,
                _ => {
                    let Some(d1) = digit(next) else {
                        self.append_string(out, &[dollar])?;
                        i += 1;
                        continue;
                    };
                    let two = digit(t.get(i + 2).copied()).map(|d2| d1 * 10 + d2).filter(|&n| (1..=ncap).contains(&n));
                    let (g, used) = match two {
                        Some(n) => (n, 3),
                        None if (1..=ncap).contains(&d1) => (d1, 2),
                        None => {
                            self.append_string(out, &[dollar])?;
                            i += 1;
                            continue;
                        }
                    };
                    if row[g] != UNSET {
                        self.append_string(out, &src[row[g].0 as usize..row[g].1 as usize])?;
                    }
                    i += used;
                }
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------ JSON

    fn json_parse(&mut self, text: Value, reviver: Value) -> Res<Value> {
        let s = self.coerce_string(text)?;
        let len = h!(self, self.heap.str_len(s));
        if len > self.ext.bl.json.max_source_units {
            return Err(Error::Limit(LimitKind::Json(json::LimitKind::Source)));
        }
        self.spend((len / 16) as u64)?;
        let units = h!(self, self.heap.str_units(s)).to_vec();
        let opts = json::JsonOptions { object_proto: Some(self.object_proto), array_proto: Some(self.array_proto) };
        let lim = self.ext.bl.json;
        match json::parse(&mut self.heap, &units, &opts, &lim) {
            Ok(v) if self.is_callable(reviver) => self.json_revive(v, reviver),
            Ok(v) => Ok(v),
            Err(e) => Err(match e.kind {
                json::ErrorKind::Syntax(k) => {
                    let what = match k {
                        json::Syntax::UnexpectedEnd => return Err(self.mk_err(SYNTAX_ERROR, "Unexpected end of JSON input")),
                        json::Syntax::BadNumber => "Bad number",
                        json::Syntax::BadEscape => "Bad escaped character",
                        json::Syntax::ControlChar => "Bad control character in string literal",
                        json::Syntax::TrailingData => "Unexpected non-whitespace character after JSON",
                        json::Syntax::InvalidUtf8 | json::Syntax::UnexpectedChar => "Unexpected token",
                    };
                    self.mk_err(SYNTAX_ERROR, &format!("{what} in JSON at position {}", e.offset))
                }
                json::ErrorKind::Limit(k) => Error::Limit(LimitKind::Json(k)),
                json::ErrorKind::Heap(he) => self.herr(he),
            }),
        }
    }

    // ------------------------------------------------------------ RegExp objects

    fn make_regexp(&mut self, re: Rc<rx::Regex>, source: StrId, flags: StrId) -> Res<ObjId> {
        self.make_regexp_proto(re, source, flags, self.ext.regexp_proto)
    }

    fn make_regexp_proto(&mut self, re: Rc<rx::Regex>, source: StrId, flags: StrId, proto: ObjId) -> Res<ObjId> {
        self.tick()?;
        let o = h!(self, self.heap.new_object(Some(proto)));
        self.def(o, self.ext.k_last_index, Value::Number(0.0), true, false, false)?;
        self.ext.regexps.insert(o, ReData { re, source, flags });
        Ok(o)
    }

    fn compile_err(&mut self, e: rx::CompileError, pat: &[u16]) -> Error {
        match e {
            rx::CompileError::Syntax { msg, .. } => {
                let p = String::from_utf16_lossy(pat);
                self.mk_err(SYNTAX_ERROR, &format!("Invalid regular expression: /{p}/: {msg}"))
            }
            rx::CompileError::Unsupported { what, .. } => unsupported(what, Span::default()),
            rx::CompileError::InvalidFlags(c) => self.mk_err(SYNTAX_ERROR, &format!("Invalid flags supplied to RegExp constructor '{c}'")),
            rx::CompileError::UnsupportedFlag(c) => unsupported(flag_gate(c), Span::default()),
            rx::CompileError::Limit(_) => Error::Limit(LimitKind::Regexp(RegexpLimit::Compile)),
        }
    }

    /// `new RegExp(pattern, flags)` from two strings. The compiled pattern and the `source` and `flags` strings
    /// are cached by the two handles until the next `collect`, so a hot `new RegExp(s)` pays one hash lookup.
    fn regexp_create(&mut self, pat: StrId, flags: StrId) -> Res<ObjId> {
        self.regexp_create_proto(pat, flags, self.ext.regexp_proto)
    }

    fn regexp_create_proto(&mut self, pat: StrId, flags: StrId, proto: ObjId) -> Res<ObjId> {
        if let Some((re, src, fl)) = self.ext.cache.get(&(pat, flags)).cloned() {
            return self.make_regexp_proto(re, src, fl, proto);
        }
        let units = h!(self, self.heap.str_units(pat)).to_vec();
        let fl = h!(self, self.heap.to_string_lossy(flags));
        let re = match rx::Regex::new(&units, &fl) {
            Ok(re) => Rc::new(re),
            Err(e) => return Err(self.compile_err(e, &units)),
        };
        self.spend((units.len() + re.program_len()) as u64 / 8)?;
        let src = h!(self, self.heap.intern_vec(escape_source(&units)));
        let fl = h!(self, self.heap.intern_str(&canon_flags(re.flags())));
        if self.ext.cache.len() >= 256 {
            self.ext.cache.clear();
        }
        self.ext.cache.insert((pat, flags), (re.clone(), src, fl));
        self.make_regexp_proto(re, src, fl, proto)
    }

    fn regexp_ctor(&mut self, p: Value, f: Value, is_new: bool) -> Res<Value> {
        self.regexp_ctor_nt(p, f, is_new, None)
    }

    fn regexp_ctor_nt(&mut self, p: Value, f: Value, is_new: bool, nt: Option<ObjId>) -> Res<Value> {
        let is_re = self.is_regexp(p)?;
        let po = self.regexp_of(p);
        if is_re && po.is_none() {
            return Err(unsupported("RegExp constructor with a regexp-like object (Symbol.match)", Span::default()));
        }
        if po.is_some() && matches!(f, Value::Undefined) && !is_new && is_re {
            let c = self.get_prop(p, self.k_constructor)?;
            if matches!((c, self.ext.regexp_ctor), (Value::Object(a), Some(b)) if a == b) { return Ok(p); }
        }
        // IsRegExp precedes allocation, but source/flag string conversion follows GetPrototypeFromConstructor.
        let proto = if let Some(nt) = nt { self.proto_from_ctor_or(nt, self.ext.regexp_proto)? } else { self.ext.regexp_proto };
        if let Some(po) = po {
            if matches!(f, Value::Undefined) {
                let d = &self.ext.regexps[&po];
                let (re, src, fl) = (d.re.clone(), d.source, d.flags);
                return Ok(Value::Object(self.make_regexp_proto(re, src, fl, proto)?));
            }
            let src = self.ext.regexps[&po].source;
            let fl = self.coerce_string(f)?;
            return Ok(Value::Object(self.regexp_create_proto(src, fl, proto)?));
        }
        let ps = if matches!(p, Value::Undefined) { h!(self, self.heap.intern_str("")) } else { self.coerce_string(p)? };
        let fs = if matches!(f, Value::Undefined) { h!(self, self.heap.intern_str("")) } else { self.coerce_string(f)? };
        Ok(Value::Object(self.regexp_create_proto(ps, fs, proto)?))
    }

    fn this_regexp(&mut self, this: Value, m: &str) -> Res<ObjId> {
        match self.regexp_of(this) {
            Some(o) => Ok(o),
            None => {
                let t = self.display(this);
                Err(self.type_err(&format!("Method RegExp.prototype.{m} called on incompatible receiver {t}")))
            }
        }
    }

    fn re_getter(&mut self, this: Value, i: u8) -> Res<Value> {
        let Value::Object(o) = this else { return Err(self.type_err("RegExp getter called on non-object")) };
        let Some(d) = self.ext.regexps.get(&o) else {
            if o == self.ext.regexp_proto {
                return if i == 0 { self.str_value("(?:)") } else if i == 1 { self.str_value("") } else { Ok(Value::Undefined) };
            }
            if i == 1 {
                return self.flags_of_object(this);
            }
            let t = RE_GETTERS[i as usize];
            return Err(self.type_err(&format!("RegExp.prototype.{t} getter called on non-RegExp object")));
        };
        let f = d.re.flags();
        Ok(match i {
            0 => Value::String(d.source),
            1 => Value::String(d.flags),
            2 => Value::Bool(f.global),
            3 => Value::Bool(f.ignore_case),
            4 => Value::Bool(f.multiline),
            5 => Value::Bool(f.dot_all),
            6 => Value::Bool(f.sticky),
            7 => Value::Bool(f.unicode),
            _ => Value::Bool(f.has_indices),
        })
    }

    /// The `flags` getter of an object that is not a regexp: read the flag properties (in the order of V8) and join the letters as `dgimsuvy`.
    #[cold]
    #[inline(never)]
    fn flags_of_object(&mut self, this: Value) -> Res<Value> {
        let mut on = [false; 8];
        for (n, name) in ["hasIndices", "global", "ignoreCase", "multiline", "dotAll", "unicode", "sticky", "unicodeSets"].into_iter().enumerate() {
            let k = h!(self, self.heap.key_from_str(name));
            let v = self.get_prop(this, k)?;
            on[n] = self.truthy(v)?;
        }
        let s: String = [(0, 'd'), (1, 'g'), (2, 'i'), (3, 'm'), (4, 's'), (5, 'u'), (7, 'v'), (6, 'y')].into_iter().filter(|&(n, _)| on[n]).map(|(_, c)| c).collect();
        self.str_value(&s)
    }

    /// `RegExp.prototype[@@split]` on a builtin regexp.
    fn re_split(&mut self, r: ObjId, s: StrId, lim: u32) -> Res<Value> {
        if self.user_exec(r)? {
            return self.re_split_g(r, s, lim);
        }
        self.split_regexp(r, s, lim)
    }

    fn set_last_index(&mut self, r: ObjId, n: f64) -> Res<()> {
        let saved = std::mem::replace(&mut self.strict, true);
        let res = self.put_prop(Value::Object(r), self.ext.k_last_index, Value::Number(n));
        self.strict = saved;
        res
    }

    /// One bounded match attempt. Its work is charged to the evaluator budget.
    /// `AdvanceStringIndex` of `s` at `i`: by a code point with `u`, else by a code unit.
    fn advance(&self, re: &rx::Regex, s: StrId, i: usize) -> Res<usize> {
        Ok(re.advance(self.heap.str_units(s).map_err(Error::Heap)?, i))
    }

    fn run_engine(&mut self, re: &rx::Regex, s: StrId, from: usize) -> Res<Option<rx::Match>> {
        let left = self.step_limit.saturating_sub(self.steps);
        if left == 0 {
            return Err(Error::Limit(LimitKind::Steps));
        }
        let cap = self.ext.bl.regexp_steps;
        let lim = rx::Limits { max_steps: cap.min(left), max_input_units: self.ext.bl.regexp_input_units, max_stack_frames: self.ext.bl.regexp_stack_frames };
        let units = self.heap.str_units(s).map_err(Error::Heap)?;
        let r = re.exec(units, from, &lim, None, &mut self.ext.scratch);
        self.steps = self.steps.saturating_add(self.ext.scratch.steps);
        if self.ext.scratch.retained_bytes() > SCRATCH_KEEP {
            self.ext.scratch.release();
        }
        match r {
            Ok(m) if self.steps > self.step_limit => {
                let _ = m;
                Err(Error::Limit(LimitKind::Steps))
            }
            Ok(m) => Ok(m),
            Err(rx::ExecError::StepLimit) => Err(Error::Limit(if left <= cap { LimitKind::Steps } else { LimitKind::Regexp(RegexpLimit::Steps) })),
            Err(rx::ExecError::StackLimit) => Err(Error::Limit(LimitKind::Regexp(RegexpLimit::Stack))),
            Err(rx::ExecError::InputTooLarge) => Err(Error::Limit(LimitKind::Regexp(RegexpLimit::Input))),
            Err(rx::ExecError::Cancelled) => Err(unsupported("regexp cancel flag", Span::default())),
        }
    }

    /// `RegExpBuiltinExec` up to the result array: reads and coerces `lastIndex` (this can run script), matches,
    /// then writes `lastIndex` for `g` and `y`. The regexp data is copied first, so nothing stays borrowed.
    /// The match, and whether the regexp has named groups (`Match` has no room for it: it is the same size as before, 24 bytes).
    fn re_exec_raw(&mut self, r: ObjId, s: StrId) -> Res<Option<(rx::Match, bool)>> {
        let re = self.ext.regexps[&r].re.clone();
        let lv = self.get_prop(Value::Object(r), self.ext.k_last_index)?;
        let n = self.coerce_number(lv)?;
        let fl = re.flags();
        let track = fl.global || fl.sticky;
        let li = if !track || n.is_nan() || n <= 0.0 { 0.0 } else { n.trunc().min(9007199254740991.0) };
        let len = h!(self, self.heap.str_len(s));
        if li > len as f64 {
            if track {
                self.set_last_index(r, 0.0)?;
            }
            return Ok(None);
        }
        let m = self.run_engine(&re, s, li as usize)?;
        match m {
            None => {
                if track {
                    self.set_last_index(r, 0.0)?;
                }
                Ok(None)
            }
            Some(m) => {
                if track {
                    let end = m.spans[0].map_or(0, |x| x.1);
                    self.set_last_index(r, end as f64)?;
                }
                Ok(Some((m, re.has_names() || re.flags().has_indices)))
            }
        }
    }

    /// All matches of a global regexp, from index 0. Sets `lastIndex` to 0 first (a read-only `lastIndex` throws)
    /// and leaves it 0, as the spec's loop does. The capture slots are checked against `max_matches` before each push.
    fn collect_matches(&mut self, r: ObjId, s: StrId, groups: bool) -> Res<MatchList> {
        let re = self.ext.regexps[&r].re.clone();
        self.set_last_index(r, 0.0)?;
        let len = h!(self, self.heap.str_len(s));
        let n = if groups { re.group_count() + 1 } else { 1 };
        let mut ml = MatchList { n, v: Vec::new(), names: None };
        let mut pos = 0usize;
        while pos <= len {
            self.tick()?;
            let Some(m) = self.run_engine(&re, s, pos)? else { break };
            if ml.v.len() + n > self.ext.bl.max_matches {
                return Err(Error::Limit(LimitKind::Regexp(RegexpLimit::Matches)));
            }
            for g in 0..n {
                ml.v.push(m.spans[g].map_or(UNSET, |(a, b)| (a as u32, b as u32)));
            }
            let (a, b) = m.spans[0].unwrap_or((pos, pos));
            pos = if a == b { self.advance(&re, s, b)? } else { b };
        }
        self.set_last_index(r, 0.0)?;
        if re.has_names() {
            ml.names = Some(re);
        }
        Ok(ml)
    }

    /// The `groups` object of a match: no prototype, one property for each group name, in pattern order. `span(g)` is group `g`.
    #[cold]
    #[inline(never)]
    fn push_groups(&mut self, cargs: &mut Vec<Value>, re: &rx::Regex, s: StrId, row: &[(u32, u32)]) -> Res<()> {
        let g = self.groups_object(re, s, |g| (row[g] != UNSET).then(|| (row[g].0 as usize, row[g].1 as usize)))?;
        cargs.push(g);
        Ok(())
    }

    fn groups_object(&mut self, re: &rx::Regex, s: StrId, span: impl Fn(usize) -> Option<(usize, usize)>) -> Res<Value> {
        let o = h!(self, self.heap.new_object(None));
        for (name, g) in re.group_names() {
            let k = h!(self, self.heap.key_from_str(name));
            let v = match span(*g as usize) {
                Some((x, y)) => Value::String(self.sub(s, x, y)?),
                None => Value::Undefined,
            };
            self.def(o, k, v, true, true, true)?;
        }
        Ok(Value::Object(o))
    }

    /// `groups`, then `indices` for a regexp with named groups or flag `d`. `r` is the regexp that made `m`.
    #[cold]
    #[inline(never)]
    fn match_extras(&mut self, a: ObjId, r: ObjId, s: StrId, m: &rx::Match) -> Res<()> {
        let re = self.ext.regexps[&r].re.clone();
        let groups = if re.has_names() { self.groups_object(&re, s, |g| m.spans[g])? } else { Value::Undefined };
        self.def(a, self.ext.k_groups, groups, true, true, true)?;
        if !re.flags().has_indices {
            return Ok(());
        }
        let pair = |it: &mut Self, sp: Option<(usize, usize)>| match sp {
            Some((x, y)) => it.new_array_from(&[Value::Number(x as f64), Value::Number(y as f64)]),
            None => Ok(Value::Undefined),
        };
        let mut pairs = Vec::with_capacity(m.spans.len());
        for sp in &m.spans {
            pairs.push(pair(self, *sp)?);
        }
        let ia = self.new_array_from(&pairs)?;
        let Value::Object(io) = ia else { return Ok(()) };
        let ig = if re.has_names() {
            let o = h!(self, self.heap.new_object(None));
            for (name, g) in re.group_names() {
                let k = h!(self, self.heap.key_from_str(name));
                let v = pair(self, m.spans[*g as usize])?;
                self.def(o, k, v, true, true, true)?;
            }
            Value::Object(o)
        } else {
            Value::Undefined
        };
        self.def(io, self.ext.k_groups, ig, true, true, true)?;
        let ki = h!(self, self.heap.key_from_str("indices"));
        self.def(a, ki, ia, true, true, true)
    }

    /// The array `exec` returns: captures, then `index`, `input`, `groups`. `r` is the regexp that made `m`.
    fn match_result(&mut self, r: ObjId, s: StrId, m: &rx::Match, named: bool) -> Res<Value> {
        let spans = &m.spans;
        let a = h!(self, self.heap.new_array(Some(self.array_proto)));
        for (i, sp) in spans.iter().enumerate() {
            let v = match *sp {
                Some((x, y)) => Value::String(self.sub(s, x, y)?),
                None => Value::Undefined,
            };
            self.def(a, PropertyKey::Index(i as u32), v, true, true, true)?;
        }
        let start = spans[0].map_or(0, |x| x.0);
        self.def(a, self.ext.k_index, Value::Number(start as f64), true, true, true)?;
        self.def(a, self.ext.k_input, Value::String(s), true, true, true)?;
        if named {
            self.match_extras(a, r, s, m)?;
        } else {
            self.def(a, self.ext.k_groups, Value::Undefined, true, true, true)?;
        }
        Ok(Value::Object(a))
    }
}

trait AsciiCaseU16 {
    fn make_ascii_uppercase_u16(&mut self);
    fn make_ascii_lowercase_u16(&mut self);
}

impl AsciiCaseU16 for u16 {
    fn make_ascii_uppercase_u16(&mut self) {
        if (b'a' as u16..=b'z' as u16).contains(self) {
            *self -= 32;
        }
    }
    fn make_ascii_lowercase_u16(&mut self) {
        if (b'A' as u16..=b'Z' as u16).contains(self) {
            *self += 32;
        }
    }
}
