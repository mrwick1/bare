//! JavaScript regular expressions (ECMA-262 `RegExp` pattern matching, non-Unicode mode).
//!
//! Standalone: this file does not use the lexer, the parser or any JS value. A JS wrapper (later)
//! owns `RegExp` objects, `lastIndex`, the result array and the `String` methods.
//!
//! - [`Regex::new`] compiles a pattern given as UTF-16 code units (a JS string is a sequence of
//!   UTF-16 code units, and may hold a lone surrogate) and a flags string.
//! - [`Regex::exec`] matches against UTF-16 input starting at a code-unit index. It returns the
//!   start and end of the whole match and of every capture group, as code-unit indices.
//!
//! What the matcher does *not* do (the wrapper must):
//! - Read or coerce `lastIndex` (`ToLength`). The wrapper passes the number in.
//! - Write `lastIndex` back. After a match the wrapper sets `lastIndex = end` when the `g` or `y`
//!   flag is set, and `0` after a failed match (`RegExpBuiltinExec`, steps 4, 12 and 15).
//! - Build the result array (`index`, `input`, `groups`), run `Symbol.replace` and friends, or
//!   advance past an empty match (`AdvanceStringIndex`) in `matchAll`/`replace`/`split`.
//! - Honour `g` itself: `exec` only honours `y` (sticky: match only at `last_index`). Without `y` it
//!   searches forward from `last_index`. `g` is only reported by [`Regex::flags`].
//!
//! Supported syntax: literals, `\t \n \v \f \r \0 \cX \xHH \uHHHH` and identity escapes, `.`,
//! `\d \D \s \S \w \W`, classes with ranges and negation, `^ $ \b \B`, alternation, capturing and
//! `(?:...)` groups, `* + ? {n} {n,} {n,m}` (greedy and lazy), backreferences `\1`, lookahead
//! `(?=...)` and `(?!...)`, and the flags `g i m s y`. Annex B (web compatibility) rules for
//! non-Unicode patterns are followed: a lone `]` `}` or `{` is a literal, `\8` is `8`, `\1` with
//! fewer than one group is legacy octal, `\c` without a letter is a backslash, `[\d-x]` has a
//! literal `-`.
//!
//! Explicit errors, never a different pattern: flags `u v d` ([`CompileError::UnsupportedFlag`]),
//! lookbehind, named groups and quantified lookahead ([`CompileError::Unsupported`]). Without `u`,
//! `\p{L}` is the literal text `p{L}`, as in ECMA-262.
//!
//! Case folding (`i`): `Canonicalize` of ECMA-262 22.2.2.7.3 for non-Unicode mode: the full
//! Unicode uppercase of a code unit, if that is exactly one code unit, and not a non-ASCII unit
//! mapped to ASCII. It uses Rust's `char::to_uppercase`, so it follows the Unicode version of the
//! Rust standard library. Characters added to Unicode after that version are not folded.
//!
//! Engine: the pattern becomes a small instruction list, run by a backtracking VM with an explicit
//! heap stack. Matching never recurses, so a hostile pattern cannot overflow the Rust stack.
//! Compile and run are bounded: see [`Limits`] and the `MAX_*` constants. A limit is an error,
//! never "no match".

use std::sync::atomic::{AtomicBool, Ordering};

/// The longest pattern, in UTF-16 code units.
pub const MAX_PATTERN_UNITS: usize = 1 << 16;
/// The deepest group nesting. It bounds recursion in the parser and the compiler.
pub const MAX_DEPTH: usize = 128;
/// The most instructions in a compiled program. `a{1000}` costs 1000, so repeats are bounded.
pub const MAX_PROGRAM: usize = 1 << 17;
/// The most capture groups.
pub const MAX_GROUPS: usize = 1 << 12;
/// The most code units a class may expand under `i` in one pattern (compile-time work bound).
const MAX_CLASS_WORK: u64 = 1 << 22;

// ---------------------------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------------------------

/// Parsed flags. `global` is data for the wrapper; the matcher uses `sticky` only.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Flags {
    pub global: bool,
    pub ignore_case: bool,
    pub multiline: bool,
    pub dot_all: bool,
    pub sticky: bool,
}

impl Flags {
    /// Parse a flags string. Unknown or repeated flags are errors. `u`, `v` and `d` are valid
    /// JS flags that this engine does not support, so they are a different error.
    pub fn parse(s: &str) -> Result<Flags, CompileError> {
        let mut f = Flags::default();
        let mut seen = [false; 128];
        let mut unsupported = None;
        for c in s.chars() {
            let i = c as usize;
            if i >= 128 || !b"dgimsuvy".contains(&(c as u8)) {
                return Err(CompileError::InvalidFlags(c));
            }
            if seen[i] {
                return Err(CompileError::InvalidFlags(c));
            }
            seen[i] = true;
            match c {
                'g' => f.global = true,
                'i' => f.ignore_case = true,
                'm' => f.multiline = true,
                's' => f.dot_all = true,
                'y' => f.sticky = true,
                _ => unsupported = unsupported.or(Some(c)),
            }
        }
        if seen[b'u' as usize] && seen[b'v' as usize] {
            return Err(CompileError::InvalidFlags('v'));
        }
        match unsupported {
            Some(c) => Err(CompileError::UnsupportedFlag(c)),
            None => Ok(f),
        }
    }
}

/// A compile failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompileError {
    /// The pattern is not valid ECMA-262 syntax (a JS `SyntaxError`). `at` is a code-unit index.
    Syntax { msg: &'static str, at: usize },
    /// Valid JS that this engine does not implement. Not a JS `SyntaxError`.
    Unsupported { what: &'static str, at: usize },
    /// The flags string has an unknown or repeated flag (a JS `SyntaxError`).
    InvalidFlags(char),
    /// Valid JS flag that this engine does not implement (`u`, `v`, `d`).
    UnsupportedFlag(char),
    /// A compile bound was exceeded (pattern length, nesting, program size, groups, class work).
    Limit(&'static str),
}

/// A match failure that is not "no match".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecError {
    /// The work budget ran out.
    StepLimit,
    /// The backtrack stack reached `max_stack_frames`.
    StackLimit,
    /// The input is longer than `max_input_units` (or `u32::MAX - 1`).
    InputTooLarge,
    /// The cancel flag was set.
    Cancelled,
}

/// Run-time bounds for one [`Regex::exec`] call. Work includes instructions, search scans,
/// register initialization, capture resets, backreferences and backtrack-stack operations.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_steps: u64,
    pub max_input_units: usize,
    pub max_stack_frames: usize,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits { max_steps: 50_000_000, max_input_units: 1 << 24, max_stack_frames: 1 << 22 }
    }
}

/// A successful match. `spans[0]` is the whole match; `spans[n]` is group `n`, `None` if it did
/// not take part. Indices are UTF-16 code units, `start <= end`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Match {
    pub spans: Vec<Option<(usize, usize)>>,
}

/// Reusable working memory. Reuse one across calls to avoid allocating per match.
#[derive(Default)]
pub struct Scratch {
    regs: Vec<u32>,
    stack: Vec<Frame>,
    /// Work units charged by the last `exec` (instructions, scans and register/stack work).
    pub steps: u64,
    /// Highest backtrack stack depth of the last `exec`.
    pub peak_frames: usize,
}

impl Scratch {
    pub fn new() -> Scratch {
        Scratch::default()
    }
    /// Bytes of heap this scratch keeps between calls.
    pub fn retained_bytes(&self) -> usize {
        self.regs.capacity() * 4 + self.stack.capacity() * std::mem::size_of::<Frame>()
    }
    /// Free the heap this scratch keeps.
    pub fn release(&mut self) {
        self.regs = Vec::new();
        self.stack = Vec::new();
    }
}

/// A compiled pattern. Immutable; one value can serve many matches.
#[derive(Debug)]
pub struct Regex {
    prog: Vec<Inst>,
    classes: Vec<ClassSet>,
    flags: Flags,
    groups: usize,
    nregs: usize,
    tmp_base: usize,
    mark_base: usize,
    /// Search accelerator: ASCII units a match can start with. `None`: unknown or may match empty.
    first: Option<Box<FirstSet>>,
    /// The pattern starts with a non-multiline `^`: only `last_index == 0` can match.
    anchored: bool,
}

/// Possible first code units of a match. Non-ASCII units are always "maybe".
#[derive(Debug)]
struct FirstSet([bool; 128]);

impl Regex {
    /// Compile `pattern` (UTF-16 code units) with `flags` (e.g. `"gi"`).
    pub fn new(pattern: &[u16], flags: &str) -> Result<Regex, CompileError> {
        let flags = Flags::parse(flags)?;
        if pattern.len() > MAX_PATTERN_UNITS {
            return Err(CompileError::Limit("pattern too long"));
        }
        let mut p = Parser::new(pattern, flags);
        let node = p.parse_pattern()?;
        let groups = p.ncap as usize;
        let mut e = Emitter { prog: Vec::new(), classes: p.classes, flags, work: 0, depth: 0, max_slot: 0 };
        e.emit(&node)?;
        e.push(Inst::Match)?;
        let tmp_base = 2 * (groups + 1);
        let mark_base = tmp_base + groups + 1;
        let nregs = mark_base + e.max_slot as usize;
        let mut re = Regex { prog: e.prog, classes: e.classes, flags, groups, nregs, tmp_base, mark_base, first: None, anchored: false };
        re.anchored = matches!(re.prog[0], Inst::Start);
        re.first = re.first_set();
        Ok(re)
    }

    /// Collect the ASCII units a match can begin with. Returns `None` when a match may be empty
    /// or the walk meets something it does not model (then every start position is tried).
    fn first_set(&self) -> Option<Box<FirstSet>> {
        let mut set = [false; 128];
        let mut seen = vec![false; self.prog.len()];
        let mut todo = vec![0usize];
        while let Some(pc) = todo.pop() {
            if std::mem::replace(&mut seen[pc], true) {
                continue;
            }
            match self.prog[pc] {
                Inst::Char(c) => {
                    if c < 128 {
                        set[c as usize] = true;
                    }
                }
                Inst::CharI(k) => (0..128u16).filter(|&c| canon(c) == k).for_each(|c| set[c as usize] = true),
                Inst::Any | Inst::AnyAll => return None,
                Inst::Class(i) => (0..128u16).filter(|&c| self.classes[i as usize].matches(c, self.flags.ignore_case)).for_each(|c| set[c as usize] = true),
                // Zero-width steps: a superset of the real first units is still sound.
                Inst::Start | Inst::End | Inst::MStart | Inst::MEnd | Inst::WordB(_) | Inst::Mark(_) | Inst::GroupStart(_) | Inst::Close(_) | Inst::Reset(..) | Inst::EmptyCheck(_) => todo.push(pc + 1),
                Inst::Jmp(t) => todo.push(t as usize),
                Inst::Split(a, b) => todo.extend([a as usize, b as usize]),
                Inst::Look(_, end) => todo.push(end as usize),
                Inst::BackRef(_) | Inst::LookEnd | Inst::Match => return None,
            }
        }
        Some(Box::new(FirstSet(set)))
    }

    /// Compile from Rust strings. A Rust `&str` cannot hold a lone surrogate; use [`Regex::new`] then.
    pub fn from_str(pattern: &str, flags: &str) -> Result<Regex, CompileError> {
        let units: Vec<u16> = pattern.encode_utf16().take(MAX_PATTERN_UNITS + 1).collect();
        Regex::new(&units, flags)
    }

    pub fn flags(&self) -> Flags {
        self.flags
    }
    /// The number of capture groups, not counting group 0.
    pub fn group_count(&self) -> usize {
        self.groups
    }
    /// The number of instructions.
    pub fn program_len(&self) -> usize {
        self.prog.len()
    }

    /// Find the first match at or after `last_index` (only at `last_index` with the `y` flag).
    /// `Ok(None)` means no match, including `last_index > input.len()`. `cancel` is polled
    /// at entry and periodically during execution. Register initialization is charged in bulk.
    pub fn exec(
        &self,
        input: &[u16],
        last_index: usize,
        limits: &Limits,
        cancel: Option<&AtomicBool>,
        sc: &mut Scratch,
    ) -> Result<Option<Match>, ExecError> {
        sc.steps = 0;
        sc.peak_frames = 0;
        if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) { return Err(ExecError::Cancelled); }
        if input.len() > limits.max_input_units || input.len() >= u32::MAX as usize {
            return Err(ExecError::InputTooLarge);
        }
        if last_index > input.len() {
            return Ok(None);
        }
        let mut s = last_index;
        if self.anchored && !self.flags.sticky && s > 0 {
            return Ok(None);
        }
        loop {
            if let (Some(f), false) = (&self.first, self.flags.sticky) {
                // A match consumes at least one unit and cannot start with a skipped one.
                while s < input.len() && input[s] < 128 && !f.0[input[s] as usize] {
                    charge(&mut sc.steps, 1, limits, cancel)?;
                    s += 1;
                }
                if s >= input.len() {
                    return Ok(None);
                }
            }
            charge(&mut sc.steps, self.nregs as u64, limits, cancel)?;
            sc.regs.clear();
            sc.regs.resize(self.nregs, UNSET);
            sc.stack.clear();
            if self.run(input, s, limits, cancel, sc)? {
                let mut spans = Vec::with_capacity(self.groups + 1);
                for g in 0..=self.groups {
                    let (a, b) = (sc.regs[2 * g], sc.regs[2 * g + 1]);
                    spans.push(if a == UNSET || b == UNSET { None } else { Some((a as usize, b as usize)) });
                }
                return Ok(Some(Match { spans }));
            }
            if self.flags.sticky || s == input.len() || self.anchored {
                return Ok(None);
            }
            s += 1;
        }
    }

    fn run(
        &self,
        input: &[u16],
        start: usize,
        lim: &Limits,
        cancel: Option<&AtomicBool>,
        sc: &mut Scratch,
    ) -> Result<bool, ExecError> {
        let Scratch { regs, stack, steps, peak_frames } = sc;
        let icase = self.flags.ignore_case;
        let len = input.len();
        let mut pc = 0usize;
        let mut pos = start;
        let mut failed = false;
        regs[0] = start as u32;

        macro_rules! push {
            ($f:expr) => {{
                if stack.len() >= lim.max_stack_frames {
                    return Err(ExecError::StackLimit);
                }
                stack.push($f);
                if stack.len() > *peak_frames {
                    *peak_frames = stack.len();
                }
            }};
        }
        macro_rules! set {
            ($r:expr, $v:expr) => {{
                let r = $r as usize;
                if !stack.is_empty() {
                    push!(Frame::Restore(r as u32, regs[r]));
                }
                regs[r] = $v;
            }};
        }

        loop {
            if failed {
                charge(steps, 1, lim, cancel)?;
                match stack.pop() {
                    None => return Ok(false),
                    Some(Frame::Branch(p, s)) => {
                        pc = p as usize;
                        pos = s as usize;
                        failed = false;
                    }
                    Some(Frame::Restore(r, old)) => regs[r as usize] = old,
                    Some(Frame::Look { neg, pos: s, end }) => {
                        if neg {
                            pc = end as usize;
                            pos = s as usize;
                            failed = false;
                        }
                    }
                }
                continue;
            }
            charge(steps, 1, lim, cancel)?;
            match self.prog[pc] {
                Inst::Char(c) => {
                    if pos < len && input[pos] == c {
                        pos += 1;
                        pc += 1;
                    } else {
                        failed = true;
                    }
                }
                Inst::CharI(c) => {
                    if pos < len && canon(input[pos]) == c {
                        pos += 1;
                        pc += 1;
                    } else {
                        failed = true;
                    }
                }
                Inst::Any => {
                    if pos < len && !is_line_terminator(input[pos]) {
                        pos += 1;
                        pc += 1;
                    } else {
                        failed = true;
                    }
                }
                Inst::AnyAll => {
                    if pos < len {
                        pos += 1;
                        pc += 1;
                    } else {
                        failed = true;
                    }
                }
                Inst::Class(i) => {
                    if pos < len && self.classes[i as usize].matches(input[pos], icase) {
                        pos += 1;
                        pc += 1;
                    } else {
                        failed = true;
                    }
                }
                Inst::Start => {
                    if pos == 0 {
                        pc += 1
                    } else {
                        failed = true
                    }
                }
                Inst::End => {
                    if pos == len {
                        pc += 1
                    } else {
                        failed = true
                    }
                }
                Inst::MStart => {
                    if pos == 0 || is_line_terminator(input[pos - 1]) {
                        pc += 1
                    } else {
                        failed = true
                    }
                }
                Inst::MEnd => {
                    if pos == len || is_line_terminator(input[pos]) {
                        pc += 1
                    } else {
                        failed = true
                    }
                }
                Inst::WordB(want) => {
                    let a = pos > 0 && is_word(input[pos - 1]);
                    let b = pos < len && is_word(input[pos]);
                    if (a != b) == want {
                        pc += 1
                    } else {
                        failed = true
                    }
                }
                Inst::BackRef(g) => {
                    let (s, e) = (regs[2 * g as usize], regs[2 * g as usize + 1]);
                    if s == UNSET || e == UNSET {
                        pc += 1;
                    } else {
                        let (s, e) = (s as usize, e as usize);
                        let n = e - s;
                        let mut ok = pos + n <= len;
                        if ok {
                            for k in 0..n {
                                charge(steps, 1, lim, cancel)?;
                                let (x, y) = (input[s + k], input[pos + k]);
                                if x != y && !(icase && canon(x) == canon(y)) { ok = false; break; }
                            }
                        }
                        if ok {
                            pos += n;
                            pc += 1;
                        } else {
                            failed = true;
                        }
                    }
                }
                Inst::Split(a, b) => {
                    push!(Frame::Branch(b, pos as u32));
                    pc = a as usize;
                }
                Inst::Jmp(t) => pc = t as usize,
                Inst::Mark(r) => {
                    set!(self.mark_base + r as usize, pos as u32);
                    pc += 1;
                }
                Inst::GroupStart(g) => {
                    set!(self.tmp_base + g as usize, pos as u32);
                    pc += 1;
                }
                Inst::EmptyCheck(r) => {
                    if regs[self.mark_base + r as usize] == pos as u32 {
                        failed = true;
                    } else {
                        pc += 1;
                    }
                }
                Inst::Close(g) => {
                    let g = g as usize;
                    let st = regs[self.tmp_base + g];
                    set!(2 * g, st);
                    set!(2 * g + 1, pos as u32);
                    pc += 1;
                }
                Inst::Reset(lo, hi) => {
                    for g in lo as usize..hi as usize {
                        charge(steps, 1, lim, cancel)?;
                        if regs[2 * g] != UNSET || regs[2 * g + 1] != UNSET {
                            set!(2 * g, UNSET);
                            set!(2 * g + 1, UNSET);
                        }
                    }
                    pc += 1;
                }
                Inst::Look(neg, end) => {
                    push!(Frame::Look { neg, pos: pos as u32, end });
                    pc += 1;
                }
                Inst::LookEnd => {
                    // The nearest barrier on the stack belongs to this lookahead: inner ones are gone.
                    let mut idx = stack.len();
                    loop {
                        charge(steps, 1, lim, cancel)?;
                        idx = idx.checked_sub(1).expect("barrier");
                        if matches!(stack[idx], Frame::Look { .. }) { break; }
                    }
                    let Frame::Look { neg, pos: s, end } = stack[idx] else { unreachable!() };
                    if neg {
                        // The body matched, so `(?!...)` fails. Undo the body's captures.
                        while let Some(f) = stack.pop() {
                            charge(steps, 1, lim, cancel)?;
                            match f {
                                Frame::Restore(r, old) => regs[r as usize] = old,
                                Frame::Look { .. } => break,
                                Frame::Branch(..) => {}
                            }
                        }
                        failed = true;
                    } else {
                        // Atomic: drop the body's choice points, keep undo records for its captures.
                        let mut w = idx;
                        for r in idx + 1..stack.len() {
                            charge(steps, 1, lim, cancel)?;
                            if let Frame::Restore(..) = stack[r] {
                                stack[w] = stack[r];
                                w += 1;
                            }
                        }
                        stack.truncate(w);
                        pos = s as usize;
                        pc = end as usize;
                    }
                }
                Inst::Match => {
                    regs[1] = pos as u32;
                    return Ok(true);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Character helpers
// ---------------------------------------------------------------------------------------------

fn charge(steps: &mut u64, work: u64, limits: &Limits, cancel: Option<&AtomicBool>) -> Result<(), ExecError> {
    let old = *steps;
    *steps = steps.checked_add(work).ok_or(ExecError::StepLimit)?;
    if *steps > limits.max_steps { return Err(ExecError::StepLimit); }
    if old >> 8 != *steps >> 8 && cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
        return Err(ExecError::Cancelled);
    }
    Ok(())
}

const UNSET: u32 = u32::MAX;

fn is_line_terminator(c: u16) -> bool {
    matches!(c, 0x0A | 0x0D | 0x2028 | 0x2029)
}

fn is_word(c: u16) -> bool {
    matches!(c, 0x30..=0x39 | 0x41..=0x5A | 0x61..=0x7A | 0x5F)
}

/// `Canonicalize(ch)` for non-Unicode `i` patterns (ECMA-262 22.2.2.7.3).
pub fn canon(c: u16) -> u16 {
    if c < 128 {
        return if (b'a' as u16..=b'z' as u16).contains(&c) { c - 32 } else { c };
    }
    let Some(ch) = char::from_u32(c as u32) else { return c };
    let mut up = ch.to_uppercase();
    match (up.next(), up.next()) {
        (Some(u), None) if (u as u32) <= 0xFFFF && (u as u32) >= 128 => u as u16,
        _ => c,
    }
}

const DIGIT: &[(u16, u16)] = &[(0x30, 0x39)];
const WORD: &[(u16, u16)] = &[(0x30, 0x39), (0x41, 0x5A), (0x5F, 0x5F), (0x61, 0x7A)];
/// WhiteSpace and LineTerminator: TAB VT FF SP NBSP ZWNBSP, Zs, LF CR LS PS.
const SPACE: &[(u16, u16)] = &[
    (0x09, 0x0D),
    (0x20, 0x20),
    (0xA0, 0xA0),
    (0x1680, 0x1680),
    (0x2000, 0x200A),
    (0x2028, 0x2029),
    (0x202F, 0x202F),
    (0x205F, 0x205F),
    (0x3000, 0x3000),
    (0xFEFF, 0xFEFF),
];

/// Sort and merge ranges.
fn normalize(r: &mut Vec<(u16, u16)>) {
    r.sort_unstable();
    let mut out: Vec<(u16, u16)> = Vec::with_capacity(r.len());
    for &(lo, hi) in r.iter() {
        match out.last_mut() {
            Some(l) if (lo as u32) <= l.1 as u32 + 1 => l.1 = l.1.max(hi),
            _ => out.push((lo, hi)),
        }
    }
    *r = out;
}

fn complement(r: &[(u16, u16)]) -> Vec<(u16, u16)> {
    let mut v = r.to_vec();
    normalize(&mut v);
    let mut out = Vec::new();
    let mut next = 0u32;
    for (lo, hi) in v {
        if (lo as u32) > next {
            out.push((next as u16, lo - 1));
        }
        next = hi as u32 + 1;
    }
    if next <= 0xFFFF {
        out.push((next as u16, 0xFFFF));
    }
    out
}

fn contains(r: &[(u16, u16)], c: u16) -> bool {
    if r.len() <= 8 {
        return r.iter().any(|&(lo, hi)| lo <= c && c <= hi);
    }
    r.binary_search_by(|&(lo, hi)| {
        if hi < c {
            std::cmp::Ordering::Less
        } else if lo > c {
            std::cmp::Ordering::Greater
        } else {
            std::cmp::Ordering::Equal
        }
    })
    .is_ok()
}

#[derive(Debug)]
struct ClassSet {
    /// Members. With `i`, this is the set of canonical forms of the members.
    ranges: Vec<(u16, u16)>,
    neg: bool,
}

impl ClassSet {
    fn matches(&self, c: u16, icase: bool) -> bool {
        let k = if icase { canon(c) } else { c };
        contains(&self.ranges, k) != self.neg
    }
}

// ---------------------------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------------------------

enum Node {
    Empty,
    Char(u16),
    Any,
    Class(u32),
    Start,
    End,
    WordB(bool),
    BackRef(u32),
    Group(u32, Box<Node>),
    Look(bool, Box<Node>),
    Cat(Vec<Node>),
    Alt(Vec<Node>),
    Repeat { node: Box<Node>, min: u32, max: Option<u32>, greedy: bool, cap_lo: u32, cap_hi: u32 },
}

enum Esc {
    Char(u16),
    Set(Vec<(u16, u16)>),
    WordB(bool),
    BackRef(u32),
}

struct Parser<'a> {
    p: &'a [u16],
    i: usize,
    flags: Flags,
    total_groups: u32,
    ncap: u32,
    depth: usize,
    classes: Vec<ClassSet>,
    class_work: u64,
}

fn syn<T>(msg: &'static str, at: usize) -> Result<T, CompileError> {
    Err(CompileError::Syntax { msg, at })
}

fn hex(c: u16) -> Option<u32> {
    char::from_u32(c as u32)?.to_digit(16)
}

impl<'a> Parser<'a> {
    fn new(p: &'a [u16], flags: Flags) -> Parser<'a> {
        // Count capturing groups first: `\N` is a backreference only if N <= the total.
        let (mut n, mut i, mut in_class) = (0u32, 0, false);
        while i < p.len() {
            match p[i] {
                0x5C => i += 1,
                0x5B => in_class = true,
                0x5D => in_class = false,
                0x28 if !in_class => {
                    let named = p.get(i + 1) == Some(&0x3F)
                        && p.get(i + 2) == Some(&0x3C)
                        && !matches!(p.get(i + 3), Some(0x3D) | Some(0x21));
                    if p.get(i + 1) != Some(&0x3F) || named {
                        n = n.saturating_add(1);
                    }
                }
                _ => {}
            }
            i += 1;
        }
        Parser { p, i: 0, flags, total_groups: n, ncap: 0, depth: 0, classes: Vec::new(), class_work: 0 }
    }

    fn peek(&self) -> Option<u16> {
        self.p.get(self.i).copied()
    }
    fn eat(&mut self, c: u8) -> bool {
        if self.peek() == Some(c as u16) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn parse_pattern(&mut self) -> Result<Node, CompileError> {
        let n = self.parse_alt()?;
        if self.i < self.p.len() {
            return syn("Unmatched ')'", self.i);
        }
        if self.ncap as usize > MAX_GROUPS {
            return Err(CompileError::Limit("too many groups"));
        }
        Ok(n)
    }

    fn parse_alt(&mut self) -> Result<Node, CompileError> {
        let mut alts = vec![self.parse_cat()?];
        while self.eat(b'|') {
            alts.push(self.parse_cat()?);
        }
        Ok(if alts.len() == 1 { alts.pop().unwrap() } else { Node::Alt(alts) })
    }

    fn parse_cat(&mut self) -> Result<Node, CompileError> {
        let mut items = Vec::new();
        while let Some(c) = self.peek() {
            if c == b'|' as u16 || c == b')' as u16 {
                break;
            }
            items.push(self.parse_term()?);
        }
        Ok(match items.len() {
            0 => Node::Empty,
            1 => items.pop().unwrap(),
            _ => Node::Cat(items),
        })
    }

    /// `{n}`, `{n,}` or `{n,m}` at `i` (which holds `{`). Returns (min, max, next index).
    fn brace(&self, i: usize) -> Option<(u32, Option<u32>, usize)> {
        let mut j = i + 1;
        let num = |j: &mut usize| -> Option<u32> {
            let s = *j;
            let mut v = 0u32;
            while let Some(d) = self.p.get(*j).and_then(|&c| char::from_u32(c as u32)).and_then(|c| c.to_digit(10)) {
                v = v.saturating_mul(10).saturating_add(d);
                *j += 1;
            }
            (*j > s).then_some(v)
        };
        let min = num(&mut j)?;
        let max = if self.p.get(j) == Some(&(b',' as u16)) {
            j += 1;
            num(&mut j)
        } else {
            Some(min)
        };
        (self.p.get(j) == Some(&(b'}' as u16))).then_some((min, max, j + 1))
    }

    fn parse_term(&mut self) -> Result<Node, CompileError> {
        let at = self.i;
        let c = self.p[self.i];
        self.i += 1;
        let cap_lo = self.ncap + 1;
        let atom = match c as u8 as char {
            _ if c > 0x7F => Node::Char(c),
            '^' => return self.no_quant(Node::Start),
            '$' => return self.no_quant(Node::End),
            '(' => {
                let look = if self.p[self.i..].starts_with(&[b'?' as u16, b'=' as u16]) {
                    Some(false)
                } else if self.p[self.i..].starts_with(&[b'?' as u16, b'!' as u16]) {
                    Some(true)
                } else {
                    None
                };
                let mut cap = None;
                if look.is_some() {
                    self.i += 2;
                } else if self.eat(b'?') {
                    if self.eat(b':') {
                    } else if self.peek() == Some(b'<' as u16) {
                        return Err(match self.p.get(self.i + 1) {
                            Some(0x3D) | Some(0x21) => CompileError::Unsupported { what: "lookbehind", at },
                            _ => CompileError::Unsupported { what: "named group", at },
                        });
                    } else {
                        return syn("Invalid group", at);
                    }
                } else {
                    self.ncap += 1;
                    cap = Some(self.ncap);
                }
                self.depth += 1;
                if self.depth > MAX_DEPTH {
                    return Err(CompileError::Limit("groups nested too deeply"));
                }
                let inner = self.parse_alt()?;
                self.depth -= 1;
                if !self.eat(b')') {
                    return syn("Unterminated group", at);
                }
                match (look, cap) {
                    (Some(neg), _) => {
                        if self.quantifier_follows() {
                            return Err(CompileError::Unsupported { what: "quantified lookahead", at });
                        }
                        return Ok(Node::Look(neg, Box::new(inner)));
                    }
                    (None, Some(n)) => Node::Group(n, Box::new(inner)),
                    (None, None) => inner,
                }
            }
            ')' => return syn("Unmatched ')'", at),
            '.' => Node::Any,
            '[' => self.parse_class(at)?,
            '\\' => match self.parse_escape(false)? {
                Esc::Char(c) => Node::Char(c),
                Esc::Set(r) => self.add_class(r, false)?,
                Esc::WordB(neg) => return self.no_quant(Node::WordB(neg)),
                Esc::BackRef(n) => Node::BackRef(n),
            },
            '*' | '+' | '?' => return syn("Nothing to repeat", at),
            '{' => {
                if self.brace(at).is_some() {
                    return syn("Nothing to repeat", at);
                }
                Node::Char(c)
            }
            _ => Node::Char(c),
        };
        // Optional quantifier.
        let (min, max) = match self.peek() {
            Some(0x2A) => (0, None),
            Some(0x2B) => (1, None),
            Some(0x3F) => (0, Some(1)),
            Some(0x7B) => match self.brace(self.i) {
                Some((a, b, next)) => {
                    if b.is_some_and(|b| b < a) {
                        return syn("numbers out of order in {} quantifier", self.i);
                    }
                    self.i = next - 1;
                    (a, b)
                }
                None => return Ok(atom),
            },
            _ => return Ok(atom),
        };
        self.i += 1;
        let greedy = !self.eat(b'?');
        let cap_hi = self.ncap + 1;
        Ok(Node::Repeat { node: Box::new(atom), min, max, greedy, cap_lo, cap_hi })
    }

    fn quantifier_follows(&self) -> bool {
        match self.peek() {
            Some(0x2A) | Some(0x2B) | Some(0x3F) => true,
            Some(0x7B) => self.brace(self.i).is_some(),
            _ => false,
        }
    }

    fn no_quant(&mut self, n: Node) -> Result<Node, CompileError> {
        if self.quantifier_follows() {
            return syn("Nothing to repeat", self.i);
        }
        Ok(n)
    }

    fn parse_escape(&mut self, in_class: bool) -> Result<Esc, CompileError> {
        let Some(c) = self.peek() else { return syn("\\ at end of pattern", self.i) };
        self.i += 1;
        let ch = if c < 128 { c as u8 as char } else { '\u{80}' };
        Ok(match ch {
            'd' => Esc::Set(DIGIT.to_vec()),
            'D' => Esc::Set(complement(DIGIT)),
            's' => Esc::Set(SPACE.to_vec()),
            'S' => Esc::Set(complement(SPACE)),
            'w' => Esc::Set(WORD.to_vec()),
            'W' => Esc::Set(complement(WORD)),
            'b' if in_class => Esc::Char(8),
            'b' => Esc::WordB(true),
            'B' if !in_class => Esc::WordB(false),
            't' => Esc::Char(9),
            'n' => Esc::Char(10),
            'v' => Esc::Char(11),
            'f' => Esc::Char(12),
            'r' => Esc::Char(13),
            'c' => match self.peek() {
                Some(l) if l < 128 && (l as u8).is_ascii_alphabetic() => {
                    self.i += 1;
                    Esc::Char(l % 32)
                }
                Some(l) if in_class && l < 128 && ((l as u8).is_ascii_digit() || l == b'_' as u16) => {
                    self.i += 1;
                    Esc::Char(l % 32)
                }
                _ => {
                    // Annex B: the backslash is a literal, and `c` is read again as a literal.
                    self.i -= 1;
                    Esc::Char(0x5C)
                }
            },
            'x' => match (self.peek().and_then(hex), self.p.get(self.i + 1).copied().and_then(hex)) {
                (Some(a), Some(b)) => {
                    self.i += 2;
                    Esc::Char((a * 16 + b) as u16)
                }
                _ => Esc::Char(c),
            },
            'u' => {
                let v: Option<Vec<u32>> = (0..4).map(|k| self.p.get(self.i + k).copied().and_then(hex)).collect();
                match v {
                    Some(d) => {
                        self.i += 4;
                        Esc::Char(d.iter().fold(0, |a, &x| a * 16 + x) as u16)
                    }
                    None => Esc::Char(c),
                }
            }
            '0'..='9' => {
                if c != b'0' as u16 && !in_class {
                    let mut j = self.i;
                    let mut n = (c - 0x30) as u32;
                    while let Some(&d) = self.p.get(j) {
                        if !(0x30..=0x39).contains(&d) {
                            break;
                        }
                        n = n.saturating_mul(10).saturating_add((d - 0x30) as u32);
                        j += 1;
                    }
                    if n <= self.total_groups {
                        self.i = j;
                        return Ok(Esc::BackRef(n));
                    }
                }
                if c >= b'8' as u16 {
                    Esc::Char(c)
                } else {
                    // Annex B legacy octal: up to three digits, value <= 0o377.
                    let mut v = (c - 0x30) as u32;
                    let mut max_digits = if c <= b'3' as u16 { 2 } else { 1 };
                    while max_digits > 0 {
                        match self.peek() {
                            Some(d) if (0x30..=0x37).contains(&d) => {
                                v = v * 8 + (d - 0x30) as u32;
                                self.i += 1;
                                max_digits -= 1;
                            }
                            _ => break,
                        }
                    }
                    Esc::Char(v as u16)
                }
            }
            _ => Esc::Char(c),
        })
    }

    fn parse_class(&mut self, at: usize) -> Result<Node, CompileError> {
        let neg = self.eat(b'^');
        let mut set: Vec<(u16, u16)> = Vec::new();
        loop {
            let Some(c) = self.peek() else { return syn("Unterminated character class", at) };
            if c == b']' as u16 {
                self.i += 1;
                break;
            }
            let a = self.class_atom()?;
            if self.peek() == Some(b'-' as u16) && self.p.get(self.i + 1).is_some_and(|&n| n != b']' as u16) {
                self.i += 1;
                let at_b = self.i;
                let b = self.class_atom()?;
                match (a, b) {
                    (Esc::Char(x), Esc::Char(y)) => {
                        if x > y {
                            return syn("Range out of order in character class", at_b);
                        }
                        set.push((x, y));
                    }
                    (a, b) => {
                        // Annex B: a class escape next to `-` makes the `-` a literal.
                        for e in [a, b] {
                            match e {
                                Esc::Char(x) => set.push((x, x)),
                                Esc::Set(r) => set.extend(r),
                                _ => unreachable!(),
                            }
                        }
                        set.push((0x2D, 0x2D));
                    }
                }
            } else {
                match a {
                    Esc::Char(x) => set.push((x, x)),
                    Esc::Set(r) => set.extend(r),
                    _ => unreachable!(),
                }
            }
            if set.len() > MAX_PROGRAM {
                return Err(CompileError::Limit("class too large"));
            }
        }
        self.add_class(set, neg)
    }

    fn class_atom(&mut self) -> Result<Esc, CompileError> {
        let c = self.p[self.i];
        self.i += 1;
        if c == b'\\' as u16 {
            self.parse_escape(true)
        } else {
            Ok(Esc::Char(c))
        }
    }

    fn add_class(&mut self, mut ranges: Vec<(u16, u16)>, neg: bool) -> Result<Node, CompileError> {
        normalize(&mut ranges);
        if self.flags.ignore_case {
            // Replace the members by their canonical forms: `canon(ch)` in set <=> some member
            // has the same canonical form as `ch`. Exact, and fast at match time.
            let mut bits = vec![0u64; 1024];
            for &(lo, hi) in &ranges {
                self.class_work += (hi - lo) as u64 + 1;
                if self.class_work > MAX_CLASS_WORK {
                    return Err(CompileError::Limit("character class folding too large"));
                }
                for c in lo..=hi {
                    let k = canon(c) as usize;
                    bits[k >> 6] |= 1 << (k & 63);
                }
            }
            ranges.clear();
            let mut c = 0usize;
            while c < 65536 {
                if bits[c >> 6] >> (c & 63) & 1 == 1 {
                    let s = c;
                    while c + 1 < 65536 && bits[(c + 1) >> 6] >> ((c + 1) & 63) & 1 == 1 {
                        c += 1;
                    }
                    ranges.push((s as u16, c as u16));
                }
                c += 1;
            }
        }
        self.classes.push(ClassSet { ranges, neg });
        Ok(Node::Class(self.classes.len() as u32 - 1))
    }
}

// ---------------------------------------------------------------------------------------------
// Compiler
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
enum Inst {
    Char(u16),
    /// Compare `canon(input)` with this already canonical unit.
    CharI(u16),
    Any,
    AnyAll,
    Class(u32),
    Start,
    End,
    MStart,
    MEnd,
    /// `true` for `\b`, `false` for `\B`.
    WordB(bool),
    BackRef(u32),
    /// Try the first target; on failure, the second.
    Split(u32, u32),
    Jmp(u32),
    /// Save the position in loop-mark slot N.
    Mark(u32),
    /// Save the position as the start of group N.
    GroupStart(u32),
    EmptyCheck(u32),
    /// Set group N from its saved start and the current position.
    Close(u32),
    /// Clear groups lo..hi (start of a repeat iteration).
    Reset(u32, u32),
    /// Start a lookahead: (negative, index after the body).
    Look(bool, u32),
    LookEnd,
    Match,
}

#[derive(Clone, Copy, Debug)]
enum Frame {
    Branch(u32, u32),
    Restore(u32, u32),
    Look { neg: bool, pos: u32, end: u32 },
}

struct Emitter {
    prog: Vec<Inst>,
    classes: Vec<ClassSet>,
    flags: Flags,
    work: usize,
    depth: usize,
    /// Number of loop-mark registers needed (the deepest loop nesting).
    max_slot: u32,
}

impl Emitter {
    fn push(&mut self, i: Inst) -> Result<usize, CompileError> {
        if self.prog.len() >= MAX_PROGRAM {
            return Err(CompileError::Limit("pattern too large after expanding repeats"));
        }
        self.prog.push(i);
        Ok(self.prog.len() - 1)
    }

    fn pc(&self) -> u32 {
        self.prog.len() as u32
    }

    /// Smallest number of code units the node can match (0 means it may match empty).
    fn can_be_empty(n: &Node) -> bool {
        match n {
            Node::Empty | Node::Start | Node::End | Node::WordB(_) | Node::BackRef(_) | Node::Look(..) => true,
            Node::Char(_) | Node::Any | Node::Class(_) => false,
            Node::Group(_, b) => Self::can_be_empty(b),
            Node::Cat(v) => v.iter().all(Self::can_be_empty),
            Node::Alt(v) => v.iter().any(Self::can_be_empty),
            Node::Repeat { node, min, .. } => *min == 0 || Self::can_be_empty(node),
        }
    }

    fn emit(&mut self, n: &Node) -> Result<(), CompileError> {
        self.depth += 1;
        if self.depth > 4 * MAX_DEPTH + 8 {
            return Err(CompileError::Limit("pattern nested too deeply"));
        }
        let r = self.emit_inner(n);
        self.depth -= 1;
        r
    }

    fn emit_inner(&mut self, n: &Node) -> Result<(), CompileError> {
        match n {
            Node::Empty => {}
            Node::Char(c) => {
                if self.flags.ignore_case {
                    let k = canon(*c);
                    self.push(Inst::CharI(k))?;
                } else {
                    self.push(Inst::Char(*c))?;
                }
            }
            Node::Any => {
                self.push(if self.flags.dot_all { Inst::AnyAll } else { Inst::Any })?;
            }
            Node::Class(i) => {
                self.push(Inst::Class(*i))?;
            }
            Node::Start => {
                self.push(if self.flags.multiline { Inst::MStart } else { Inst::Start })?;
            }
            Node::End => {
                self.push(if self.flags.multiline { Inst::MEnd } else { Inst::End })?;
            }
            Node::WordB(b) => {
                self.push(Inst::WordB(*b))?;
            }
            Node::BackRef(g) => {
                self.push(Inst::BackRef(*g))?;
            }
            Node::Group(g, b) => {
                self.push(Inst::GroupStart(*g))?;
                self.emit(b)?;
                self.push(Inst::Close(*g))?;
            }
            Node::Look(neg, b) => {
                let l = self.push(Inst::Look(*neg, 0))?;
                self.emit(b)?;
                self.push(Inst::LookEnd)?;
                self.prog[l] = Inst::Look(*neg, self.pc());
            }
            Node::Cat(v) => {
                for x in v {
                    self.emit(x)?;
                }
            }
            Node::Alt(v) => {
                let mut jumps = Vec::new();
                for (k, x) in v.iter().enumerate() {
                    if k + 1 < v.len() {
                        let s = self.push(Inst::Split(0, 0))?;
                        self.emit(x)?;
                        jumps.push(self.push(Inst::Jmp(0))?);
                        self.prog[s] = Inst::Split(s as u32 + 1, self.pc());
                    } else {
                        self.emit(x)?;
                    }
                }
                let end = self.pc();
                for j in jumps {
                    self.prog[j] = Inst::Jmp(end);
                }
            }
            Node::Repeat { node, min, max, greedy, cap_lo, cap_hi } => {
                self.emit_repeat(node, *min, *max, *greedy, *cap_lo, *cap_hi)?
            }
        }
        Ok(())
    }

    fn body(&mut self, node: &Node, lo: u32, hi: u32) -> Result<(), CompileError> {
        if hi > lo {
            self.push(Inst::Reset(lo, hi))?;
        }
        self.emit(node)
    }

    fn emit_repeat(
        &mut self,
        node: &Node,
        min: u32,
        max: Option<u32>,
        greedy: bool,
        lo: u32,
        hi: u32,
    ) -> Result<(), CompileError> {
        let split = |g: bool, body: u32, exit: u32| if g { Inst::Split(body, exit) } else { Inst::Split(exit, body) };
        // Each copy counts as work even when it emits nothing, so `(?:){4294967295}` is bounded.
        for _ in 0..min {
            self.work += 1;
            if self.work > MAX_PROGRAM {
                return Err(CompileError::Limit("pattern too large after expanding repeats"));
            }
            self.body(node, lo, hi)?;
        }
        let check = Self::can_be_empty(node);
        let slot = self.depth as u32; // enclosing emit depth: distinct for nested loops
        if check {
            self.max_slot = self.max_slot.max(slot + 1);
        }
        match max {
            None => {
                let l = self.push(Inst::Split(0, 0))?;
                if check {
                    self.push(Inst::Mark(slot))?;
                }
                self.body(node, lo, hi)?;
                if check {
                    self.push(Inst::EmptyCheck(slot))?;
                }
                self.push(Inst::Jmp(l as u32))?;
                self.prog[l] = split(greedy, l as u32 + 1, self.pc());
            }
            Some(max) => {
                let mut splits = Vec::new();
                for _ in min..max {
                    self.work += 1;
                    if self.work > MAX_PROGRAM {
                        return Err(CompileError::Limit("pattern too large after expanding repeats"));
                    }
                    splits.push(self.push(Inst::Split(0, 0))?);
                    if check {
                        self.push(Inst::Mark(slot))?;
                    }
                    self.body(node, lo, hi)?;
                    if check {
                        self.push(Inst::EmptyCheck(slot))?;
                    }
                }
                let end = self.pc();
                for s in splits {
                    self.prog[s] = split(greedy, s as u32 + 1, end);
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }
    /// Spans of the first match as "s,e;s,e;-", or "N" for no match.
    fn run(p: &str, f: &str, s: &str) -> String {
        run_at(p, f, s, 0)
    }
    fn run_at(p: &str, f: &str, s: &str, li: usize) -> String {
        let re = Regex::from_str(p, f).unwrap();
        fmt(re.exec(&u(s), li, &Limits::default(), None, &mut Scratch::new()).unwrap())
    }
    fn fmt(m: Option<Match>) -> String {
        match m {
            None => "N".into(),
            Some(m) => m.spans.iter().map(|s| s.map_or("-".into(), |(a, b)| format!("{a},{b}"))).collect::<Vec<_>>().join(";"),
        }
    }

    // Expected values below were checked with Node 22 (`/p/gd.exec`); see tools/js/regexp_oracle.mjs
    // for the machine-checked set.
    #[test]
    fn basics() {
        assert_eq!(run("abc", "", "xxabcxx"), "2,5");
        assert_eq!(run("a*?", "", "aaa"), "0,0");
        assert_eq!(run("a|ab", "", "abc"), "0,1");
        assert_eq!(run("(z)((a+)?(b+)?(c))*", "", "zaacbbbcac"), "0,10;0,1;8,10;8,9;-;9,10");
        assert_eq!(run("(a*)*", "", "b"), "0,0;-");
        assert_eq!(run("(a*)+", "", "b"), "0,0;0,0");
        assert_eq!(run("(?=(a+))a*b\\1", "", "baaabac"), "3,6;3,4");
        assert_eq!(run("(.*?)a(?!(a+)b\\2c)\\2(.*)", "", "baaabaac"), "0,8;0,2;-;3,8");
    }

    #[test]
    fn target_route_lookahead() {
        let p = "^\\/journeys\\/(?!new\\/?$)[^/]+\\/?$";
        assert_eq!(run(p, "", "/journeys/abc/"), "0,14");
        assert_eq!(run(p, "", "/journeys/new"), "N");
        assert_eq!(run(p, "", "/journeys/new/"), "N");
        assert_eq!(run(p, "", "/journeys/new2"), "0,14");
    }

    #[test]
    fn utf16_units() {
        // Non-BMP: `.` matches one code unit, `^..$` matches a pair, a lone surrogate is a unit.
        assert_eq!(run("^.$", "", "\u{1F600}"), "N");
        assert_eq!(run("^..$", "", "\u{1F600}"), "0,2");
        let re = Regex::new(&[0xD800], "").unwrap();
        let input = [0x61, 0xD800, 0x62];
        let m = re.exec(&input, 0, &Limits::default(), None, &mut Scratch::new()).unwrap();
        assert_eq!(fmt(m), "1,2");
        let re = Regex::new(&[0xDE00], "").unwrap();
        let pair = u("\u{1F600}");
        let m = re.exec(&pair, 0, &Limits::default(), None, &mut Scratch::new()).unwrap();
        assert_eq!(fmt(m), "1,2"); // no Unicode mode: the low surrogate matches alone
    }

    #[test]
    fn case_folding() {
        assert_eq!(run("abc", "i", "ABC"), "0,3");
        assert_eq!(run("ß", "i", "SS"), "N"); // full uppercase is two units: no fold
        assert_eq!(run("k", "i", "\u{212A}"), "N"); // Kelvin sign maps to ASCII: not folded
        assert_eq!(run("σ", "i", "ς"), "0,1");
        assert_eq!(run("[a-z]+", "i", "ABC"), "0,3");
        assert_eq!(run("[^a-z]", "i", "A"), "N");
        assert_eq!(run("(a)\\1", "i", "aA"), "0,2;0,1");
    }

    #[test]
    fn lines_and_anchors() {
        assert_eq!(run("^a", "m", "b\ra"), "2,3");
        assert_eq!(run("a$", "m", "a\u{2028}b"), "0,1");
        assert_eq!(run("^a", "", "b\na"), "N");
        assert_eq!(run(".", "", "\r"), "N");
        assert_eq!(run(".", "s", "\r"), "0,1");
        assert_eq!(run("\\b", "", ""), "N");
        assert_eq!(run("\\B", "", ""), "0,0");
    }

    #[test]
    fn search_start_and_sticky() {
        assert_eq!(run_at("a", "", "ba", 1), "1,2");
        assert_eq!(run_at("a", "y", "ba", 0), "N");
        assert_eq!(run_at("a", "y", "ba", 1), "1,2");
        assert_eq!(run_at("", "", "ab", 2), "2,2");
        assert_eq!(run_at("", "", "ab", 3), "N"); // last_index > len
        assert_eq!(run_at("a", "g", "aa", 1), "1,2"); // g is wrapper state; exec searches from last_index
    }

    #[test]
    fn accelerators_keep_results() {
        // Skipping start positions must not change the answer, with non-ASCII input around.
        assert_eq!(run("b+", "", "ééb\u{1F600}bb"), "2,3");
        assert_eq!(run("é|b", "", "xxb"), "2,3");
        assert_eq!(run("k", "i", "xx\u{212A}K"), "3,4");
        assert_eq!(run("(?=a)", "", "ba"), "1,1");
        assert_eq!(run("^a", "", "ba"), "N");
        assert_eq!(run_at("^a", "", "aa", 1), "N");
        assert_eq!(run_at("^a", "y", "aa", 1), "N");
        assert_eq!(run_at("^a", "m", "a\na", 1), "2,3");
        assert_eq!(run("$", "", "ab"), "2,2");
        assert_eq!(run("x*", "", "ab"), "0,0");
    }

    #[test]
    fn syntax_errors_and_unsupported() {
        for p in ["(", ")", "[", "*", "a**", "a{2,1}", "{1}", "(?x)", "[b-a]", "\\", "^*", "\\b+"] {
            assert!(matches!(Regex::from_str(p, ""), Err(CompileError::Syntax { .. })), "{p}");
        }
        for p in ["]", "}", "{", "a{", "a{,5}", "\\8", "\\c", "[\\d-x]", "\\p{L}", "\\u{3}"] {
            assert!(Regex::from_str(p, "").is_ok(), "{p}");
        }
        assert!(matches!(Regex::from_str("(?<=a)b", ""), Err(CompileError::Unsupported { what: "lookbehind", .. })));
        assert!(matches!(Regex::from_str("(?<!a)b", ""), Err(CompileError::Unsupported { what: "lookbehind", .. })));
        assert!(matches!(Regex::from_str("(?<n>a)", ""), Err(CompileError::Unsupported { what: "named group", .. })));
        assert!(matches!(Regex::from_str("(?=a)*", ""), Err(CompileError::Unsupported { .. })));
        for f in ["u", "v", "d", "gu"] {
            assert!(matches!(Regex::from_str("a", f), Err(CompileError::UnsupportedFlag(_))), "{f}");
        }
        for f in ["gg", "x", "G", "é", "uv"] {
            assert!(matches!(Regex::from_str("a", f), Err(CompileError::InvalidFlags(_))), "{f}");
        }
        assert_eq!(Flags::parse("gimsy").unwrap(), Flags { global: true, ignore_case: true, multiline: true, dot_all: true, sticky: true });
    }

    #[test]
    fn compile_bounds() {
        let lim = |p: &str| Regex::from_str(p, "").err();
        assert!(matches!(lim("a{4294967295}"), Some(CompileError::Limit(_))));
        assert!(matches!(lim("(?:){4294967295}"), Some(CompileError::Limit(_))));
        assert!(matches!(lim("((a{300}){300}){300}"), Some(CompileError::Limit(_))));
        assert!(matches!(lim(&"(".repeat(MAX_DEPTH + 1)), Some(CompileError::Limit(_))));
        assert!(Regex::from_str(&format!("{}a{}", "(".repeat(MAX_DEPTH), ")".repeat(MAX_DEPTH)), "").is_ok());
        assert!(matches!(lim(&"a".repeat(MAX_PATTERN_UNITS + 1)), Some(CompileError::Limit(_))));
        assert!(Regex::from_str(&"a".repeat(MAX_PATTERN_UNITS), "").is_ok());
        // Each paren level may add up to four tree levels (group, concatenation, repeat): still fits.
        let nest = format!("{}a{}", "(a".repeat(MAX_DEPTH), ")*".repeat(MAX_DEPTH));
        let re = Regex::from_str(&nest, "").unwrap();
        assert_eq!(fmt(re.exec(&u("aaa"), 0, &Limits::default(), None, &mut Scratch::new()).unwrap()).split(';').next(), Some("0,3"));
        let many = "()".repeat(MAX_GROUPS + 1);
        assert!(matches!(lim(&many), Some(CompileError::Limit(_))));
        let classes = "[\\u0000-\\uffff]".repeat(100);
        assert!(matches!(Regex::from_str(&classes, "i").err(), Some(CompileError::Limit(_))));
    }

    #[test]
    fn run_bounds() {
        let mut sc = Scratch::new();
        let evil = Regex::from_str("(a*)*b", "").unwrap();
        let a = vec![b'a' as u16; 40];
        let l = Limits { max_steps: 100_000, ..Limits::default() };
        assert_eq!(evil.exec(&a, 0, &l, None, &mut sc), Err(ExecError::StepLimit));
        assert!(sc.steps > 100_000);
        let l = Limits { max_stack_frames: 100, ..Limits::default() };
        let re = Regex::from_str("(?:a|b)*c", "").unwrap();
        assert_eq!(re.exec(&vec![b'a' as u16; 1000], 0, &l, None, &mut sc), Err(ExecError::StackLimit));
        let l = Limits { max_input_units: 10, ..Limits::default() };
        assert_eq!(re.exec(&a, 0, &l, None, &mut sc), Err(ExecError::InputTooLarge));
        let stop = AtomicBool::new(true);
        let l = Limits { max_steps: u64::MAX, ..Limits::default() };
        assert_eq!(evil.exec(&a, 0, &l, Some(&stop), &mut sc), Err(ExecError::Cancelled));
        // A limit is never "no match": the same pattern with room answers.
        assert_eq!(fmt(evil.exec(&a[..10], 0, &Limits::default(), None, &mut sc).unwrap()), "N");
        // Hostile pattern shapes do not overflow the Rust stack (matching is iterative).
        let deep = format!("{}a{}", "(?:".repeat(MAX_DEPTH), ")*".repeat(MAX_DEPTH));
        let re = Regex::from_str(&deep, "").unwrap();
        let _ = re.exec(&u("aaaa"), 0, &Limits { max_steps: 1_000_000, ..Limits::default() }, None, &mut sc);
        let long = Regex::from_str("(?:a|b)*$", "").unwrap();
        let big = vec![b'a' as u16; 2_000_000];
        let r = long.exec(&big, 0, &Limits { max_stack_frames: usize::MAX, ..Limits::default() }, None, &mut sc);
        assert_eq!(fmt(r.unwrap()), "0,2000000");
    }

    #[test]
    fn scratch_is_reusable() {
        let re = Regex::from_str("(a)|b", "").unwrap();
        let mut sc = Scratch::new();
        for _ in 0..3 {
            assert_eq!(fmt(re.exec(&u("b"), 0, &Limits::default(), None, &mut sc).unwrap()), "0,1;-");
            assert_eq!(fmt(re.exec(&u("a"), 0, &Limits::default(), None, &mut sc).unwrap()), "0,1;0,1");
        }
        assert!(sc.retained_bytes() > 0);
        sc.release();
        assert_eq!(sc.retained_bytes(), 0);
    }

    #[test]
    fn canon_matches_spec_samples() {
        assert_eq!(canon(b'a' as u16), b'A' as u16);
        assert_eq!(canon(0xDF), 0xDF); // ß: uppercase is "SS"
        assert_eq!(canon(0x17F), 0x17F); // ſ would map to ASCII S: kept
        assert_eq!(canon(0x212A), 0x212A);
        assert_eq!(canon(0x3C2), 0x3A3); // ς -> Σ
        assert_eq!(canon(0xD800), 0xD800);
    }
    #[test]
    fn search_work_and_short_calls_honor_limits() {
        let re = Regex::from_str("a", "").unwrap();
        let mut sc = Scratch::new();
        let zero = Limits { max_steps: 0, ..Limits::default() };
        assert_eq!(re.exec(&u("bbbb"), 0, &zero, None, &mut sc), Err(ExecError::StepLimit));
        let stop = AtomicBool::new(true);
        assert_eq!(re.exec(&u("a"), 0, &Limits::default(), Some(&stop), &mut sc), Err(ExecError::Cancelled));
    }

    #[test]
    fn backreference_comparisons_consume_budget() {
        let re = Regex::from_str(r"(a{1000})\1", "y").unwrap();
        let input = vec![b'a' as u16; 2000];
        let lim = Limits { max_steps: 1500, ..Limits::default() };
        assert_eq!(re.exec(&input, 0, &lim, None, &mut Scratch::new()), Err(ExecError::StepLimit));
        assert!(re.exec(&input, 0, &Limits::default(), None, &mut Scratch::new()).unwrap().is_some());
    }

}
