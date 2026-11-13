//! JavaScript regular expressions (ECMA-262 `RegExp` pattern matching, with and without `u`).
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
//! `(?=...)` and `(?!...)`, lookbehind `(?<=...)` and `(?<!...)`, and the flags `d g i m s u v y`. Annex B (web compatibility) rules for
//! non-Unicode patterns are followed: a lone `]` `}` or `{` is a literal, `\8` is `8`, `\1` with
//! fewer than one group is legacy octal, `\c` without a letter is a backslash, `[\d-x]` has a
//! literal `-`.
//!
//! With `v`, classes take set notation: nested classes, `--`, `&&` and `\q{a|bc}` strings (a class that holds strings is an
//! alternation, longest string first). Properties of strings (`\p{RGI_Emoji}`) are a `SyntaxError`. A quantified lookahead
//! follows Annex B without `u` and is a `SyntaxError` with `u`. With `u`, `\p{...}` and `\P{...}` read General_Category, Script, Script_Extensions and the
//! binary properties of ECMA-262 from `bare_unicode::property` (the sets are made when the pattern is compiled); an unknown name
//! is a `SyntaxError`. Without `u`, `\p{L}` is the literal text `p{L}`, as in ECMA-262.
//!
//! Case folding (`i`): `Canonicalize` of ECMA-262 22.2.2.7.3 for non-Unicode mode: the full
//! Unicode uppercase of a code unit, if that is exactly one code unit, and not a non-ASCII unit
//! mapped to ASCII. It uses Rust's `char::to_uppercase`, so it follows the Unicode version of the
//! Rust standard library. Characters added to Unicode after that version are not folded.
//! With `u`, `Canonicalize` is simple case folding (CaseFolding.txt, statuses C and S, from
//! `bare-unicode`). A literal becomes the class of the code points with the same folding, a class
//! is closed over folding, and `\w` `\W` `\b` also count U+017F and U+212A as word characters.
//!
//! Engine: the pattern becomes a small instruction list, run by a backtracking VM with an explicit
//! heap stack. Matching never recurses, so a hostile pattern cannot overflow the Rust stack.
//! Compile and run are bounded: see [`Limits`] and the `MAX_*` constants. A limit is an error,
//! never "no match".

use bare_unicode::fold;
use bare_unicode::tables::{CASE_FOLD, CASE_FOLD_INV};
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
    /// `u` or `v`: the pattern and the input are code points (see the header).
    pub unicode: bool,
    /// `v`: also set notation in classes. `unicode` is true too.
    pub unicode_sets: bool,
    /// `d`: the wrapper adds `indices` to match results. The matcher ignores it.
    pub has_indices: bool,
}

impl Flags {
    /// Parse a flags string. Unknown or repeated flags are errors, and so is `u` with `v`.
    pub fn parse(s: &str) -> Result<Flags, CompileError> {
        let mut f = Flags::default();
        let mut seen = [false; 128];
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
                'u' => f.unicode = true,
                'd' => f.has_indices = true,
                _ => {
                    f.unicode = true;
                    f.unicode_sets = true;
                }
            }
        }
        if seen[b'u' as usize] && seen[b'v' as usize] {
            return Err(CompileError::InvalidFlags('v'));
        }
        Ok(f)
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
    /// `(name, group number)` of each named group, in pattern order. `None` when the pattern has none.
    names: Option<Box<Vec<(String, u32)>>>,
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
        let mut e = Emitter { prog: Vec::new(), classes: p.classes, flags, work: 0, depth: 0, max_slot: 0, backwards: false };
        e.emit(&node)?;
        e.push(Inst::Match)?;
        let tmp_base = 2 * (groups + 1);
        let mark_base = tmp_base + groups + 1;
        let nregs = mark_base + e.max_slot as usize;
        let names = (!p.names.is_empty()).then(|| Box::new(std::mem::take(&mut p.names)));
        let mut re = Regex { prog: e.prog, classes: e.classes, flags, groups, names, nregs, tmp_base, mark_base, first: None, anchored: false };
        re.anchored = matches!(re.prog[0], Inst::Start);
        re.first = re.first_set();
        Ok(re)
    }

    /// Check `pattern` and `flags` without building a program: the flags, the length and the grammar of the pattern. Everything that
    /// [`Regex::new`] reports before it emits code is reported here, with the same error. The program limits (code size, nesting of the
    /// emit pass) are not checked: they show at [`Regex::new`].
    pub fn check(pattern: &[u16], flags: &str) -> Result<(), CompileError> {
        let flags = Flags::parse(flags)?;
        if pattern.len() > MAX_PATTERN_UNITS {
            return Err(CompileError::Limit("pattern too long"));
        }
        Parser::new(pattern, flags).parse_pattern().map(drop)
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
                Inst::Any | Inst::AnyAll | Inst::AnyU | Inst::AnyAllU => return None,
                Inst::Class(i) => (0..128u32).filter(|&c| self.classes[i as usize].matches(c, self.flags.ignore_case && !self.flags.unicode)).for_each(|c| set[c as usize] = true),
                Inst::ClassU(i) => (0..128u32).filter(|&c| self.classes[i as usize].matches(c, false)).for_each(|c| set[c as usize] = true),
                // Zero-width steps: a superset of the real first units is still sound.
                Inst::Start | Inst::End | Inst::MStart | Inst::MEnd | Inst::WordB(_) | Inst::NotInPair | Inst::Mark(_) | Inst::GroupStart(_) | Inst::Close(_) | Inst::Reset(..) | Inst::EmptyCheck(_) => todo.push(pc + 1),
                Inst::Jmp(t) => todo.push(t as usize),
                Inst::Split(a, b) => todo.extend([a as usize, b as usize]),
                Inst::Look(_, end) => todo.push(end as usize),
                Inst::BackRef(_) | Inst::LookEnd | Inst::Backward | Inst::ReverseClose(_) | Inst::Match => return None,
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
    /// `(name, group number)` of each named group, in pattern order.
    pub fn group_names(&self) -> &[(String, u32)] {
        self.names.as_deref().map_or(&[], |v| v.as_slice())
    }
    pub fn has_names(&self) -> bool {
        self.names.is_some()
    }
    /// The number of instructions.
    pub fn program_len(&self) -> usize {
        self.prog.len()
    }

    /// Find the first match at or after `last_index` (only at `last_index` with the `y` flag).
    /// `Ok(None)` means no match, including `last_index > input.len()`. `cancel` is polled
    /// at entry and periodically during execution. Register initialization is charged in bulk.
    #[inline]
    pub fn exec(
        &self,
        input: &[u16],
        last_index: usize,
        limits: &Limits,
        cancel: Option<&AtomicBool>,
        sc: &mut Scratch,
    ) -> Result<Option<Match>, ExecError> {
        if self.flags.unicode && self.flags.ignore_case {
            self.exec_in::<true>(input, last_index, limits, cancel, sc)
        } else {
            self.exec_in::<false>(input, last_index, limits, cancel, sc)
        }
    }

    /// `FOLD`: `i` with `u`. `\b` and backreferences then use simple case folding. A const parameter compiles a
    /// separate matcher for it, so the other patterns run a loop without that code. Extra instructions or call
    /// arguments in the shared loop made `short` and `caps` 2 to 5% slower (measured).
    #[inline(never)]
    fn exec_in<const FOLD: bool>(
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
        // With `u`, an index inside a surrogate pair first tries from the start of the pair, then from the index
        // itself: V8 compiles this choice (`OptionallyStepBackToLeadSurrogate`); the oracle checks it against Node.
        if self.flags.unicode
            && inside_pair(input, s)
            && let Some(m) = self.attempt::<FOLD>(input, s - 1, limits, cancel, sc)?
        {
            return Ok(Some(m));
        }
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
            if self.run::<FOLD>(input, s, limits, cancel, sc)? {
                return Ok(Some(self.spans(sc)));
            }
            if self.flags.sticky || s == input.len() || self.anchored {
                return Ok(None);
            }
            // By one code unit, also with `u`: a zero-width match can sit inside a surrogate pair, as in V8. A
            // character never starts there (see `char_at`).
            s += 1;
        }
    }

    /// The cold first attempt of a `u` index inside a surrogate pair, at the start of the pair.
    #[cold]
    #[inline(never)]
    fn attempt<const FOLD: bool>(&self, input: &[u16], s: usize, limits: &Limits, cancel: Option<&AtomicBool>, sc: &mut Scratch) -> Result<Option<Match>, ExecError> {
        charge(&mut sc.steps, self.nregs as u64, limits, cancel)?;
        sc.regs.clear();
        sc.regs.resize(self.nregs, UNSET);
        sc.stack.clear();
        Ok(if self.run::<FOLD>(input, s, limits, cancel, sc)? { Some(self.spans(sc)) } else { None })
    }

    fn spans(&self, sc: &Scratch) -> Match {
        let mut spans = Vec::with_capacity(self.groups + 1);
        for g in 0..=self.groups {
            let (a, b) = (sc.regs[2 * g], sc.regs[2 * g + 1]);
            spans.push(if a == UNSET || b == UNSET { None } else { Some((a as usize, b as usize)) });
        }
        Match { spans }
    }

    /// One `u` character step (`AnyU`, `AnyAllU`, `ClassU`) at `pos`: the position after it, or `None`. Out of line,
    /// so the dispatch loop keeps its registers for the code-unit instructions.
    #[inline(never)]
    fn read_u(&self, input: &[u16], pos: usize, i: Inst) -> Option<usize> {
        let (c, w) = char_at(input, pos)?;
        let ok = match i {
            Inst::AnyU => !is_line_terminator_cp(c),
            Inst::ClassU(k) => self.classes[k as usize].matches(c, false),
            _ => true,
        };
        ok.then_some(pos + w)
    }

    /// A backreference with `i` and `u`: compare simple case foldings, one code point at a time. A folding can
    /// change the length in units, so each side walks on its own. Returns the end position and the work done.
    fn backref_fold(&self, input: &[u16], regs: &[u32], g: u32, pos: usize) -> (Option<usize>, u64) {
        let (s, e) = (regs[2 * g as usize], regs[2 * g as usize + 1]);
        if s == UNSET || e == UNSET {
            return (Some(pos), 0);
        }
        let (cap, mut i, mut p, mut work) = (&input[..e as usize], s as usize, pos, 0);
        while i < cap.len() {
            work += 1;
            let Some((y, v)) = char_at(input, p) else { return (None, work) };
            let (x, w) = code_point(cap, i);
            if fold(x) != fold(y) {
                return (None, work);
            }
            (i, p) = (i + w, p + v);
        }
        (Some(p), work)
    }

    #[cold]
    #[inline(never)]
    fn backward(&self, input: &[u16], regs: &[u32], pos: usize, inst: Inst,
        steps: &mut u64, lim: &Limits, cancel: Option<&AtomicBool>) -> Result<Option<usize>, ExecError> {
        // Read a complete code point before the assertion position in Unicode mode.
        let prev = |p: usize| {
            let mut q = p.checked_sub(1)?;
            if self.flags.unicode && q > 0 && (0xDC00..=0xDFFF).contains(&input[q])
                && (0xD800..=0xDBFF).contains(&input[q - 1]) { q -= 1; }
            Some(q)
        };
        if let Inst::BackRef(g) = inst {
            let (s, e) = (regs[2 * g as usize], regs[2 * g as usize + 1]);
            if s == UNSET || e == UNSET { return Ok(Some(pos)); }
            let (mut a, mut b) = (e as usize, pos);
            while a > s as usize {
                charge(steps, 1, lim, cancel)?;
                let Some(x) = prev(a) else { return Ok(None) };
                let Some(y) = prev(b) else { return Ok(None) };
                let (c, d) = if self.flags.unicode {
                    (code_point(input, x).0, code_point(input, y).0)
                } else { (input[x] as u32, input[y] as u32) };
                let equal = if self.flags.ignore_case {
                    if self.flags.unicode { fold(c) == fold(d) } else { canon(c as u16) == canon(d as u16) }
                } else { c == d };
                if !equal { return Ok(None); }
                (a, b) = (x, y);
            }
            return Ok(Some(b));
        }
        let unicode = matches!(inst, Inst::AnyU | Inst::AnyAllU | Inst::ClassU(_));
        let p = if unicode { prev(pos) } else { pos.checked_sub(1) };
        let Some(p) = p else { return Ok(None) };
        let c = input[p];
        let ok = match inst {
            Inst::Char(k) => c == k,
            Inst::CharI(k) => canon(c) == k,
            Inst::Any => !is_line_terminator(c),
            Inst::AnyAll => true,
            Inst::Class(k) => self.classes[k as usize].matches(c as u32, self.flags.ignore_case && !self.flags.unicode),
            Inst::AnyU | Inst::AnyAllU | Inst::ClassU(_) => self.read_u(input, p, inst) == Some(pos),
            _ => unreachable!(),
        };
        Ok(ok.then_some(p))
    }

    /// `AdvanceStringIndex`: the next index after `i`. With `u`, a surrogate pair at `i` is skipped whole.
    pub fn advance(&self, input: &[u16], i: usize) -> usize {
        if self.flags.unicode && i < input.len() { i + code_point(input, i).1 } else { i + 1 }
    }

    // Inlined into both callers: `exec` is the hot one.
    #[inline(always)]
    fn run<const FOLD: bool>(
        &self,
        input: &[u16],
        start: usize,
        lim: &Limits,
        cancel: Option<&AtomicBool>,
        sc: &mut Scratch,
    ) -> Result<bool, ExecError> {
        let Scratch { regs, stack, steps, peak_frames } = sc;
        // Non-Unicode `i` only. `i` with `u` is `FOLD`: its classes are already closed over folding.
        let icase = self.flags.ignore_case && !FOLD;
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
                    if pos < len && self.classes[i as usize].matches(input[pos] as u32, icase) {
                        pos += 1;
                        pc += 1;
                    } else {
                        failed = true;
                    }
                }
                i @ (Inst::AnyU | Inst::AnyAllU | Inst::ClassU(_)) => match self.read_u(input, pos, i) {
                    Some(p) => {
                        pos = p;
                        pc += 1;
                    }
                    None => failed = true,
                },
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
                    let a = pos > 0 && is_word_in::<FOLD>(input[pos - 1]);
                    let b = pos < len && is_word_in::<FOLD>(input[pos]);
                    if (a != b) == want {
                        pc += 1
                    } else {
                        failed = true
                    }
                }
                Inst::BackRef(g) if FOLD => {
                    let (r, n) = self.backref_fold(input, regs, g, pos);
                    charge(steps, n, lim, cancel)?;
                    match r {
                        Some(p) => {
                            pos = p;
                            pc += 1;
                        }
                        None => failed = true,
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
                Inst::NotInPair => {
                    if inside_pair(input, pos) {
                        failed = true;
                    } else {
                        pc += 1;
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
                Inst::Backward => {
                    match self.backward(input, regs, pos, self.prog[pc + 1], steps, lim, cancel)? {
                        Some(p) => { pos = p; pc += 2; }
                        None => failed = true,
                    }
                }
                Inst::ReverseClose(g) => {
                    let g = g as usize;
                    set!(2 * g, pos as u32);
                    set!(2 * g + 1, regs[self.tmp_base + g]);
                    pc += 1;
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

fn is_line_terminator_cp(c: u32) -> bool {
    matches!(c, 0x0A | 0x0D | 0x2028 | 0x2029)
}

fn is_lead(c: u16) -> bool {
    (0xD800..=0xDBFF).contains(&c)
}

fn is_trail(c: u16) -> bool {
    (0xDC00..=0xDFFF).contains(&c)
}

/// With `u`: the character at `pos` and its length in units, `None` at the end and at the trail of a pair (no
/// character starts there).
fn char_at(input: &[u16], pos: usize) -> Option<(u32, usize)> {
    if pos >= input.len() || inside_pair(input, pos) {
        return None;
    }
    Some(code_point(input, pos))
}

/// Is `pos` between the lead and the trail of a surrogate pair?
fn inside_pair(input: &[u16], pos: usize) -> bool {
    pos > 0 && pos < input.len() && is_lead(input[pos - 1]) && is_trail(input[pos])
}

/// The code point at `i` (`i < input.len()`) and its length in units: a lead and a trail surrogate are one code point
/// (ECMA-262 `CodePointAt`). A lone surrogate is itself.
fn code_point(input: &[u16], i: usize) -> (u32, usize) {
    let a = input[i];
    match input.get(i + 1) {
        Some(&b) if is_lead(a) && is_trail(b) => (0x10000 + ((a as u32 - 0xD800) << 10) + (b as u32 - 0xDC00), 2),
        _ => (a as u32, 1),
    }
}

/// The code points whose simple case folding equals that of `c`, as sorted ranges.
fn fold_group(c: u32) -> Vec<(u32, u32)> {
    let f = fold(c);
    let mut g: Vec<(u32, u32)> = slice(CASE_FOLD_INV, f, f).iter().map(|p| (p.1, p.1)).collect();
    g.push((f, f));
    normalize(&mut g);
    g
}

/// The pairs of a folding table (sorted by first value) whose first value is in `lo..=hi`.
fn slice(t: &[(u32, u32)], lo: u32, hi: u32) -> &[(u32, u32)] {
    &t[t.partition_point(|p| p.0 < lo)..t.partition_point(|p| p.0 <= hi)]
}

fn is_word(c: u16) -> bool {
    matches!(c, 0x30..=0x39 | 0x41..=0x5A | 0x61..=0x7A | 0x5F)
}

/// IsWordChar. With `FOLD` (`i` and `u`), U+017F and U+212A fold into the word characters, so they count too.
fn is_word_in<const FOLD: bool>(c: u16) -> bool {
    is_word(c) || (FOLD && (c == 0x17F || c == 0x212A))
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

const DIGIT: &[(u32, u32)] = &[(0x30, 0x39)];
const WORD: &[(u32, u32)] = &[(0x30, 0x39), (0x41, 0x5A), (0x5F, 0x5F), (0x61, 0x7A)];
/// With `i` and `u`, WordCharacters also holds the characters that fold into it: U+017F and U+212A.
const WORD_FOLD: &[(u32, u32)] = &[(0x30, 0x39), (0x41, 0x5A), (0x5F, 0x5F), (0x61, 0x7A), (0x17F, 0x17F), (0x212A, 0x212A)];
/// WhiteSpace and LineTerminator: TAB VT FF SP NBSP ZWNBSP, Zs, LF CR LS PS.
const SPACE: &[(u32, u32)] = &[
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

/// The last code point: of a code unit without `u`, of Unicode with `u`.
fn max_char(unicode: bool) -> u32 {
    if unicode { 0x10FFFF } else { 0xFFFF }
}

/// Sort and merge ranges.
fn normalize(r: &mut Vec<(u32, u32)>) {
    r.sort_unstable();
    let mut out: Vec<(u32, u32)> = Vec::with_capacity(r.len());
    for &(lo, hi) in r.iter() {
        match out.last_mut() {
            Some(l) if lo <= l.1 + 1 => l.1 = l.1.max(hi),
            _ => out.push((lo, hi)),
        }
    }
    *r = out;
}

fn complement(r: &[(u32, u32)], max: u32) -> Vec<(u32, u32)> {
    let mut v = r.to_vec();
    normalize(&mut v);
    let mut out = Vec::new();
    let mut next = 0u32;
    for (lo, hi) in v {
        if lo > next {
            out.push((next, lo - 1));
        }
        next = hi + 1;
    }
    if next <= max {
        out.push((next, max));
    }
    out
}

fn contains(r: &[(u32, u32)], c: u32) -> bool {
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

/// The value of a `v` class expression: code points, and strings of 0 or 2+ code points (a one code point string is a
/// member of `ranges`).
#[derive(Default)]
struct CSet {
    ranges: Vec<(u32, u32)>,
    strs: Vec<Vec<u32>>,
}

impl CSet {
    fn add_str(&mut self, s: Vec<u32>) {
        if let [c] = s[..] {
            self.ranges.push((c, c));
        } else {
            self.strs.push(s);
        }
    }

    /// Sort and merge the ranges. Sort the strings longest first and drop the repeats.
    fn norm(&mut self) {
        normalize(&mut self.ranges);
        self.strs.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
        self.strs.dedup();
    }

    fn union(&mut self, o: CSet) {
        self.ranges.extend(o.ranges);
        self.strs.extend(o.strs);
    }

    fn intersect(&mut self, mut o: CSet) {
        self.norm();
        o.norm();
        self.ranges = intersect_ranges(&self.ranges, &o.ranges);
        self.strs.retain(|s| o.strs.contains(s));
    }

    fn subtract(&mut self, mut o: CSet) {
        self.norm();
        o.norm();
        self.ranges = intersect_ranges(&self.ranges, &complement(&o.ranges, 0x10FFFF));
        self.strs.retain(|s| !o.strs.contains(s));
    }
}

/// The intersection of two normalized range lists.
fn intersect_ranges(a: &[(u32, u32)], b: &[(u32, u32)]) -> Vec<(u32, u32)> {
    let (mut i, mut j, mut out) = (0, 0, Vec::new());
    while i < a.len() && j < b.len() {
        let (lo, hi) = (a[i].0.max(b[j].0), a[i].1.min(b[j].1));
        if lo <= hi {
            out.push((lo, hi));
        }
        if a[i].1 < b[j].1 { i += 1 } else { j += 1 }
    }
    out
}

#[derive(Debug)]
struct ClassSet {
    /// Members. With `i`, this is the set of canonical forms of the members.
    ranges: Vec<(u32, u32)>,
    neg: bool,
}

impl ClassSet {
    /// `c` is a code unit without `u`, a code point with `u`. `icase` is for non-Unicode `i` only: with `u`, the
    /// class is already closed over case folding.
    fn matches(&self, c: u32, icase: bool) -> bool {
        let k = if icase { canon(c as u16) as u32 } else { c };
        contains(&self.ranges, k) != self.neg
    }
}

// ---------------------------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------------------------

enum Node {
    Empty,
    /// A code unit, or with `u` a code point.
    Char(u32),
    Any,
    Class(u32),
    Start,
    End,
    WordB(bool),
    BackRef(u32),
    Group(u32, Box<Node>),
    Look(bool, bool, Box<Node>),
    Cat(Vec<Node>),
    Alt(Vec<Node>),
    Repeat { node: Box<Node>, min: u32, max: Option<u32>, greedy: bool, cap_lo: u32, cap_hi: u32 },
}

enum Esc {
    Char(u32),
    Set(Vec<(u32, u32)>),
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
    /// Capture groups not closed yet: a backreference inside its own group matches empty (V8 parses it as empty, so
    /// the `u` surrogate check of `BackRef` does not apply to it).
    open: Vec<u32>,
    /// `(name, group number)` of the named groups, read before the parse (`\k<name>` may name a group that comes later).
    names: Vec<(String, u32)>,
}

/// A group name at `p[i..]` up to its `>`: the name and the index after the `>`. The name is `$`, `_` or a letter, then those and digits
/// (and ZWNJ, ZWJ). `\uXXXX` and `\u{X}` escapes are read; a pair of `\u` surrogate escapes is not.
fn group_name(p: &[u16], mut i: usize) -> Option<(String, usize)> {
    let mut name = String::new();
    loop {
        let c = *p.get(i)?;
        i += 1;
        let cp = match c {
            0x3E => return (!name.is_empty()).then_some((name, i)),
            0x5C if p.get(i) == Some(&0x75) => {
                i += 1;
                let mut v = 0u32;
                if p.get(i) == Some(&0x7B) {
                    let st = i + 1;
                    i = st;
                    while let Some(h) = p.get(i).and_then(|&h| hex(h)) {
                        v = v.saturating_mul(16).saturating_add(h);
                        i += 1;
                    }
                    if i == st || p.get(i) != Some(&0x7D) {
                        return None;
                    }
                    i += 1;
                } else {
                    for _ in 0..4 {
                        v = v * 16 + hex(*p.get(i)?)?;
                        i += 1;
                    }
                }
                v
            }
            0xD800..=0xDBFF => {
                let lo = *p.get(i).filter(|l| (0xDC00..=0xDFFF).contains(*l))?;
                i += 1;
                0x10000 + ((c as u32 - 0xD800) << 10) + (lo as u32 - 0xDC00)
            }
            c => c as u32,
        };
        let ch = char::from_u32(cp)?;
        let ok = ch == '$' || ch == '_' || if name.is_empty() { ch.is_alphabetic() } else { ch.is_alphanumeric() || ch == '\u{200c}' || ch == '\u{200d}' };
        if !ok {
            return None;
        }
        name.push(ch);
    }
}

/// Pre-scan: record the name of the group `n` whose name starts at `p[i]`, when it is a valid name.
#[cold]
#[inline(never)]
fn scan_name(p: &[u16], i: usize, n: u32, names: &mut Vec<(String, u32)>) {
    if let Some((name, _)) = group_name(p, i) {
        names.push((name, n));
    }
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
        let (mut n, mut i, mut in_class) = (0u32, 0, 0u32);
        let mut names = Vec::new();
        while i < p.len() {
            match p[i] {
                0x5C => i += 1,
                // With `v` classes nest. Without it the first `]` closes the class.
                0x5B => in_class = if flags.unicode_sets { in_class + 1 } else { 1 },
                0x5D => in_class = if flags.unicode_sets { in_class.saturating_sub(1) } else { 0 },
                0x28 if in_class == 0 => {
                    let named = p.get(i + 1) == Some(&0x3F)
                        && p.get(i + 2) == Some(&0x3C)
                        && !matches!(p.get(i + 3), Some(0x3D) | Some(0x21));
                    if p.get(i + 1) != Some(&0x3F) || named {
                        n = n.saturating_add(1);
                    }
                    if named {
                        scan_name(p, i + 3, n, &mut names);
                    }
                }
                _ => {}
            }
            i += 1;
        }
        Parser { p, i: 0, flags, total_groups: n, ncap: 0, depth: 0, classes: Vec::new(), class_work: 0, open: Vec::new(), names }
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
            _ if c > 0x7F => Node::Char(self.literal(c)),
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
                let behind = look.is_none() && self.p[self.i..].starts_with(&[63, 60])
                    && matches!(self.p.get(self.i + 2), Some(61) | Some(33));
                let look = if behind { Some(self.p[self.i + 2] == 33) } else { look };
                let mut cap = None;
                if behind { self.i += 3; }
                else if look.is_some() {
                    self.i += 2;
                } else if self.eat(b'?') {
                    if self.eat(b':') {
                    } else if self.peek() == Some(b'<' as u16) {
                        cap = Some(self.named_group(at)?);
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
                self.open.extend(cap);
                let inner = self.parse_alt()?;
                if cap.is_some() {
                    self.open.pop();
                }
                self.depth -= 1;
                if !self.eat(b')') {
                    return syn("Unterminated group", at);
                }
                match (look, cap) {
                    (Some(neg), _) => {
                        if !self.quantifier_follows() {
                            return Ok(Node::Look(neg, behind, Box::new(inner)));
                        }
                        // Annex B QuantifiableAssertion: a lookahead takes a quantifier, only without `u`. The repeat of a
                        // zero-width node follows RepeatMatcher: with min 0 the empty iteration fails, so it matches nothing.
                        if behind || self.flags.unicode { return syn("Invalid quantifier", at); }
                        Node::Look(neg, false, Box::new(inner))
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
                Esc::BackRef(n) if self.open.contains(&n) => Node::Empty,
                Esc::BackRef(n) => Node::BackRef(n),
            },
            '*' | '+' | '?' => return syn("Nothing to repeat", at),
            '{' => {
                if self.brace(at).is_some() {
                    return syn("Nothing to repeat", at);
                }
                if self.flags.unicode {
                    return syn("Lone quantifier brackets", at);
                }
                Node::Char(c as u32)
            }
            // Annex B allows a lone `]` and `}` only without `u`.
            ']' | '}' if self.flags.unicode => return syn("Lone quantifier brackets", at),
            _ => Node::Char(c as u32),
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

    /// With `u`, a lead surrogate followed by a trail surrogate in the pattern text is one code point.
    fn literal(&mut self, c: u16) -> u32 {
        if self.flags.unicode && is_lead(c) && self.peek().is_some_and(is_trail) {
            let t = self.p[self.i];
            self.i += 1;
            return 0x10000 + ((c as u32 - 0xD800) << 10) + (t as u32 - 0xDC00);
        }
        c as u32
    }

    /// `\uXXXX` after the `u`: four hex digits. With `u`, also `\u{X...}` and an escaped surrogate pair.
    fn unicode_escape(&mut self, at: usize) -> Result<Option<u32>, CompileError> {
        let u = self.flags.unicode;
        if u && self.eat(b'{') {
            let mut v = 0u32;
            let first = self.i;
            while let Some(d) = self.peek().and_then(hex) {
                v = v.saturating_mul(16).saturating_add(d);
                self.i += 1;
            }
            if self.i == first || v > 0x10FFFF || !self.eat(b'}') {
                return syn("Invalid Unicode escape", at);
            }
            return Ok(Some(v));
        }
        let four = |i: usize| -> Option<u32> { (0..4).map(|k| self.p.get(i + k).copied().and_then(hex)).try_fold(0, |a, x| Some(a * 16 + x?)) };
        let Some(v) = four(self.i) else { return if u { syn("Invalid Unicode escape", at) } else { Ok(None) } };
        self.i += 4;
        if u
            && (0xD800..=0xDBFF).contains(&v)
            && self.p.get(self.i) == Some(&0x5C)
            && self.p.get(self.i + 1) == Some(&(b'u' as u16))
            && let Some(t) = four(self.i + 2).filter(|t| (0xDC00..=0xDFFF).contains(t))
        {
            self.i += 6;
            return Ok(Some(0x10000 + ((v - 0xD800) << 10) + (t - 0xDC00)));
        }
        Ok(Some(v))
    }

    fn parse_escape(&mut self, in_class: bool) -> Result<Esc, CompileError> {
        let at = self.i - 1;
        let Some(c) = self.peek() else { return syn("\\ at end of pattern", self.i) };
        self.i += 1;
        let u = self.flags.unicode;
        let max = max_char(u);
        let ch = if c < 128 { c as u8 as char } else { '\u{80}' };
        Ok(match ch {
            'd' => Esc::Set(DIGIT.to_vec()),
            'D' => Esc::Set(complement(DIGIT, max)),
            's' => Esc::Set(SPACE.to_vec()),
            'S' => Esc::Set(complement(SPACE, max)),
            'w' => Esc::Set(self.word().to_vec()),
            'W' => Esc::Set(complement(self.word(), max)),
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
                    Esc::Char(l as u32 % 32)
                }
                Some(l) if !u && in_class && l < 128 && ((l as u8).is_ascii_digit() || l == b'_' as u16) => {
                    self.i += 1;
                    Esc::Char(l as u32 % 32)
                }
                _ if u => return syn("Invalid escape", at),
                _ => {
                    // Annex B: the backslash is a literal, and `c` is read again as a literal.
                    self.i -= 1;
                    Esc::Char(0x5C)
                }
            },
            'x' => match (self.peek().and_then(hex), self.p.get(self.i + 1).copied().and_then(hex)) {
                (Some(a), Some(b)) => {
                    self.i += 2;
                    Esc::Char(a * 16 + b)
                }
                _ if u => return syn("Invalid escape", at),
                _ => Esc::Char(c as u32),
            },
            'u' => match self.unicode_escape(at)? {
                Some(v) => Esc::Char(v),
                None => Esc::Char(c as u32),
            },
            // `u` grammar: `\0` not before a digit is NUL; a decimal escape is a backreference to an existing group,
            // never an octal escape, and never inside a class.
            '0' if u => {
                if self.peek().is_some_and(|d| (0x30..=0x39).contains(&d)) {
                    return syn("Invalid decimal escape", at);
                }
                Esc::Char(0)
            }
            '1'..='9' if u => {
                let mut n = (c - 0x30) as u32;
                while let Some(d) = self.peek().filter(|d| (0x30..=0x39).contains(d)) {
                    n = n.saturating_mul(10).saturating_add((d - 0x30) as u32);
                    self.i += 1;
                }
                if in_class || n > self.total_groups {
                    return syn("Invalid escape", at);
                }
                Esc::BackRef(n)
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
                    Esc::Char(c as u32)
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
                    Esc::Char(v)
                }
            }
            'p' | 'P' if u => {
                let set = self.property_set(at)?;
                Esc::Set(if ch == 'P' { complement(&set, max) } else { set })
            }
            // `\k<name>` is a reference when the pattern has a named group; without one (and without `u`) it is the letter `k`.
            'k' if u || !self.names.is_empty() => self.named_ref(at, in_class)?,
            // `u` identity escapes: only the syntax characters, `/`, and `-` inside a class.
            '^' | '$' | '\\' | '.' | '*' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | '/' if u => Esc::Char(c as u32),
            '-' if u && in_class => Esc::Char(c as u32),
            _ if u => return syn("Invalid escape", at),
            _ => Esc::Char(c as u32),
        })
    }

    /// `(?<name>` with `self.i` at the `<`: checks the name, takes the next group number.
    #[cold]
    #[inline(never)]
    fn named_group(&mut self, at: usize) -> Result<u32, CompileError> {
        let Some((name, end)) = group_name(self.p, self.i + 1) else { return syn("Invalid capture group name", at) };
        if self.names.iter().filter(|(n, _)| *n == name).count() > 1 {
            return syn("Duplicate capture group name", at);
        }
        self.i = end;
        self.ncap += 1;
        Ok(self.ncap)
    }

    /// `\k<name>` with `self.i` after the `k`.
    #[cold]
    #[inline(never)]
    fn named_ref(&mut self, at: usize, in_class: bool) -> Result<Esc, CompileError> {
        let named = if self.peek() == Some(b'<' as u16) { group_name(self.p, self.i + 1) } else { None };
        let Some((name, end)) = named.filter(|_| !in_class) else { return syn("Invalid named reference", at) };
        let Some(&(_, n)) = self.names.iter().find(|(x, _)| *x == name) else { return syn("Invalid named capture referenced", at) };
        self.i = end;
        Ok(Esc::BackRef(n))
    }

    /// After `\p` or `\P` in `u` mode: `{Name}` or `{Name=Value}` and the code points of the property.
    fn property_set(&mut self, at: usize) -> Result<Vec<(u32, u32)>, CompileError> {
        if !self.eat(b'{') {
            return syn("Invalid property name", at);
        }
        let start = self.i;
        while let Some(c) = self.peek() {
            if !(c < 128 && ((c as u8).is_ascii_alphanumeric() || c == b'_' as u16 || c == b'=' as u16)) {
                break;
            }
            self.i += 1;
        }
        let text: String = self.p[start..self.i].iter().map(|&c| c as u8 as char).collect();
        if !self.eat(b'}') {
            return syn("Invalid property name", at);
        }
        let (name, value) = match text.split_once('=') {
            Some((n, v)) => (n, Some(v)),
            None => (text.as_str(), None),
        };
        match bare_unicode::property::property_ranges(name, value) {
            Some(set) => Ok(set),
            None => syn("Invalid property name", at),
        }
    }

    fn parse_class(&mut self, at: usize) -> Result<Node, CompileError> {
        if self.flags.unicode_sets {
            let (set, neg) = self.class_set_v(at)?;
            return self.class_node(set, neg);
        }
        let neg = self.eat(b'^');
        let mut set: Vec<(u32, u32)> = Vec::new();
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
                    _ if self.flags.unicode => return syn("Invalid character class", at_b),
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

    /// A `v` class after its `[`: the members and whether it is negated. Parses `ClassUnion`, `ClassIntersection` and
    /// `ClassSubtraction` (ECMA-262 22.2.1). A negated class that holds strings is a `SyntaxError`.
    fn class_set_v(&mut self, at: usize) -> Result<(CSet, bool), CompileError> {
        let neg = self.eat(b'^');
        let (mut acc, mut count, mut op) = (CSet::default(), 0u32, 0u8);
        loop {
            let Some(c) = self.peek() else { return syn("Unterminated character class", at) };
            if c == b']' as u16 {
                self.i += 1;
                break;
            }
            if op != 0 || count == 1 {
                let dbl = |ch: u16| c == ch && self.p.get(self.i + 1) == Some(&ch);
                let want = if dbl(b'&' as u16) { b'&' } else if dbl(b'-' as u16) { b'-' } else { 0 };
                if want == 0 && op != 0 || want != 0 && op != 0 && op != want {
                    return syn("Invalid set operation in character class", self.i);
                }
                if want != 0 {
                    self.i += 2;
                    if want == b'&' && self.peek() == Some(b'&' as u16) {
                        return syn("Invalid set operation in character class", self.i);
                    }
                    op = want;
                    let (o, _) = self.class_operand(at)?;
                    if op == b'&' { acc.intersect(o) } else { acc.subtract(o) }
                    count += 1;
                    continue;
                }
            }
            let (set, single) = self.class_operand(at)?;
            match single {
                Some(x) if self.peek() == Some(b'-' as u16) && self.p.get(self.i + 1) != Some(&(b'-' as u16)) => {
                    self.i += 1;
                    let at_b = self.i;
                    match self.class_operand(at)?.1 {
                        Some(y) if x <= y => acc.ranges.push((x, y)),
                        Some(_) => return syn("Range out of order in character class", at_b),
                        None => return syn("Invalid character class", at_b),
                    }
                    count += 2;
                }
                _ => {
                    acc.union(set);
                    count += 1;
                }
            }
            if acc.ranges.len() > MAX_PROGRAM {
                return Err(CompileError::Limit("class too large"));
            }
        }
        acc.norm();
        if neg && !acc.strs.is_empty() {
            return syn("Negated character class may contain strings", at);
        }
        Ok((acc, neg))
    }

    /// One operand of a `v` class: a nested class, `\q{...}`, a class escape, or a character (then also its code point,
    /// for ranges).
    fn class_operand(&mut self, at: usize) -> Result<(CSet, Option<u32>), CompileError> {
        match (self.peek(), self.p.get(self.i + 1).map(|&c| if c < 128 { c as u8 } else { 0 })) {
            (Some(0x5B), _) => {
                self.i += 1;
                self.depth += 1;
                if self.depth > MAX_DEPTH {
                    return Err(CompileError::Limit("classes nested too deeply"));
                }
                let (mut set, neg) = self.class_set_v(at)?;
                self.depth -= 1;
                if neg {
                    set.ranges = complement(&set.ranges, 0x10FFFF);
                }
                Ok((set, None))
            }
            (Some(0x5C), Some(b'q')) => {
                self.i += 2;
                if !self.eat(b'{') {
                    return syn("Invalid escape", self.i);
                }
                let (mut set, mut cur) = (CSet::default(), Vec::new());
                loop {
                    match self.peek() {
                        None => return syn("Unterminated character class", at),
                        Some(0x7D) => {
                            self.i += 1;
                            set.add_str(cur);
                            break;
                        }
                        Some(0x7C) => {
                            self.i += 1;
                            set.add_str(std::mem::take(&mut cur));
                        }
                        Some(_) => cur.push(self.class_set_char()?),
                    }
                }
                Ok((set, None))
            }
            (Some(0x5C), Some(b'd' | b'D' | b's' | b'S' | b'w' | b'W' | b'p' | b'P')) => {
                self.i += 1;
                match self.parse_escape(true)? {
                    Esc::Set(ranges) => Ok((CSet { ranges, strs: Vec::new() }, None)),
                    _ => syn("Invalid character class", self.i),
                }
            }
            _ => {
                let c = self.class_set_char()?;
                Ok((CSet { ranges: vec![(c, c)], strs: Vec::new() }, Some(c)))
            }
        }
    }

    /// A `ClassSetCharacter`: not a syntax character (`( ) [ ] { } / - \ |`) and not the first of a reserved double
    /// punctuator (`&&`, `!!`, ...), or an escape.
    fn class_set_char(&mut self) -> Result<u32, CompileError> {
        let Some(c) = self.peek() else { return syn("Unterminated character class", self.i) };
        let at = self.i;
        self.i += 1;
        if c == b'\\' as u16 {
            return match self.peek() {
                Some(n) if n < 128 && b"&-!#%,:;<=>@`~".contains(&(n as u8)) => {
                    self.i += 1;
                    Ok(n as u32)
                }
                _ => match self.parse_escape(true)? {
                    Esc::Char(x) => Ok(x),
                    _ => syn("Invalid character class", at),
                },
            };
        }
        if c < 128 {
            if b"()[]{}/-|".contains(&(c as u8)) {
                return syn("Invalid character in character class", at);
            }
            if b"&!#$%*+,.:;<=>?@^`~".contains(&(c as u8)) && self.peek() == Some(c) {
                return syn("Invalid set operation in character class", at);
            }
        }
        Ok(self.literal(c))
    }

    /// The node of a `v` class: a class of code points, or an alternation of its strings (longest first, as the spec
    /// orders them), then the class, then the empty string.
    fn class_node(&mut self, set: CSet, neg: bool) -> Result<Node, CompileError> {
        if set.strs.is_empty() {
            return self.add_class(set.ranges, neg);
        }
        let mut alts = Vec::with_capacity(set.strs.len() + 1);
        let mut empty = false;
        for s in &set.strs {
            if s.is_empty() {
                empty = true;
            } else {
                alts.push(Node::Cat(s.iter().map(|&c| Node::Char(c)).collect()));
            }
        }
        if !set.ranges.is_empty() {
            alts.push(self.add_class(set.ranges, false)?);
        }
        if empty {
            alts.push(Node::Empty);
        }
        Ok(if alts.len() == 1 { alts.pop().unwrap() } else { Node::Alt(alts) })
    }

    fn class_atom(&mut self) -> Result<Esc, CompileError> {
        let c = self.p[self.i];
        self.i += 1;
        if c == b'\\' as u16 {
            self.parse_escape(true)
        } else {
            Ok(Esc::Char(self.literal(c)))
        }
    }

    fn word(&self) -> &'static [(u32, u32)] {
        if self.flags.unicode && self.flags.ignore_case { WORD_FOLD } else { WORD }
    }

    fn add_class(&mut self, mut ranges: Vec<(u32, u32)>, neg: bool) -> Result<Node, CompileError> {
        normalize(&mut ranges);
        if self.flags.ignore_case && self.flags.unicode {
            // Close the set over simple case folding: add every member of a folding group that has a member in
            // the set. Both tables are sorted, so each range finds its pairs by binary search: the cost depends on
            // the pairs that the set touches, not on the size of the ranges.
            let mut hit = Vec::new();
            for &(lo, hi) in &ranges {
                hit.extend(slice(CASE_FOLD, lo, hi).iter().map(|p| p.1));
                hit.extend(slice(CASE_FOLD_INV, lo, hi).iter().map(|p| p.0));
            }
            hit.sort_unstable();
            hit.dedup();
            self.class_work += (ranges.len() + hit.len()) as u64;
            if self.class_work > MAX_CLASS_WORK {
                return Err(CompileError::Limit("character class folding too large"));
            }
            for f in hit {
                ranges.extend(slice(CASE_FOLD_INV, f, f).iter().map(|p| (p.1, p.1)));
                ranges.push((f, f));
            }
            normalize(&mut ranges);
        } else if self.flags.ignore_case {
            // Replace the members by their canonical forms: `canon(ch)` in set <=> some member
            // has the same canonical form as `ch`. Exact, and fast at match time.
            let mut bits = vec![0u64; 1024];
            for &(lo, hi) in &ranges {
                self.class_work += (hi - lo) as u64 + 1;
                // Without `u` every member is a code unit, so `canon` sees all of them.
                if self.class_work > MAX_CLASS_WORK {
                    return Err(CompileError::Limit("character class folding too large"));
                }
                for c in lo..=hi {
                    let k = canon(c as u16) as usize;
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
                    ranges.push((s as u32, c as u32));
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
    /// `u` forms of `Any`, `AnyAll` and `Class`: they read one code point (a surrogate pair is one).
    AnyU,
    AnyAllU,
    ClassU(u32),
    Start,
    End,
    MStart,
    MEnd,
    /// `true` for `\b`, `false` for `\B`.
    WordB(bool),
    BackRef(u32),
    /// With `u`, after a backreference: fail between the lead and the trail of a surrogate pair (V8 emits the same
    /// check, `CheckNotInSurrogatePair`; the oracle checks it against Node).
    NotInPair,
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
    /// Execute the next consuming instruction backwards.
    Backward,
    ReverseClose(u32),
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
    backwards: bool,
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
            Node::Char(_) | Node::Any | Node::Class(_) | Node::BackRef(_) | Node::WordB(_) => {
                let start = self.prog.len();
                self.emit_leaf(n)?;
                if self.backwards && !matches!(n, Node::WordB(_)) {
                    let code: Vec<_> = self.prog.drain(start..).collect();
                    for inst in code.into_iter().rev() {
                        if matches!(inst, Inst::NotInPair) { continue; }
                        self.push(Inst::Backward)?;
                        self.push(inst)?;
                    }
                }
            },
            Node::Start => {
                self.push(if self.flags.multiline { Inst::MStart } else { Inst::Start })?;
            }
            Node::End => {
                self.push(if self.flags.multiline { Inst::MEnd } else { Inst::End })?;
            }
            Node::Group(g, b) => {
                self.push(Inst::GroupStart(*g))?;
                self.emit(b)?;
                self.push(if self.backwards { Inst::ReverseClose(*g) } else { Inst::Close(*g) })?;
            }
            Node::Look(neg, behind, b) => {
                let l = self.push(Inst::Look(*neg, 0))?;
                let old = self.backwards;
                self.backwards = *behind;
                self.emit(b)?;
                self.backwards = old;
                self.push(Inst::LookEnd)?;
                self.prog[l] = Inst::Look(*neg, self.pc());
            }
            Node::Cat(v) => {
                if self.backwards {
                    for x in v.iter().rev() { self.emit(x)?; }
                } else {
                    for x in v { self.emit(x)?; }
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

    /// Leaf nodes with the `u` variants. Kept out of the recursive `emit_inner`
    /// so that its stack frame stays small for deep nesting.
    #[inline(never)]
    fn emit_leaf(&mut self, n: &Node) -> Result<(), CompileError> {
        match n {
            &Node::Char(c) => {
                let fold_u = self.flags.ignore_case && self.flags.unicode;
                let group = if fold_u { fold_group(c) } else { Vec::new() };
                if fold_u && group != [(c, c)] && group[group.len() - 1].1 < 128 {
                    // An ASCII group (every letter but k and s): only ASCII maps to it under `canon` too, so the
                    // inline code-unit step is exact.
                    self.push(Inst::CharI(canon(c as u16)))?;
                } else if fold_u && group != [(c, c)] {
                    // The code points with the same simple case folding, compared as whole code points.
                    self.classes.push(ClassSet { ranges: group, neg: false });
                    self.push(Inst::ClassU(self.classes.len() as u32 - 1))?;
                } else if self.flags.ignore_case && !fold_u {
                    self.push(Inst::CharI(canon(c as u16)))?;
                } else if c > 0xFFFF {
                    // An astral code point is its surrogate pair.
                    let c = c - 0x10000;
                    self.push(Inst::Char(0xD800 + (c >> 10) as u16))?;
                    self.push(Inst::Char(0xDC00 + (c & 0x3FF) as u16))?;
                } else if self.flags.unicode && (0xD800..=0xDFFF).contains(&c) {
                    // A lone surrogate must not match half of a pair: compare whole code points.
                    self.classes.push(ClassSet { ranges: vec![(c, c)], neg: false });
                    self.push(Inst::ClassU(self.classes.len() as u32 - 1))?;
                } else {
                    self.push(Inst::Char(c as u16))?;
                }
            }
            Node::Any => {
                self.push(match (self.flags.unicode, self.flags.dot_all) {
                    (false, false) => Inst::Any,
                    (false, true) => Inst::AnyAll,
                    (true, false) => Inst::AnyU,
                    (true, true) => Inst::AnyAllU,
                })?;
            }
            Node::Class(i) => {
                // With `u`, a class without negation and without members from U+D800 can never match half of a
                // surrogate pair, so the inline code-unit step gives the same result.
                let c = &self.classes[*i as usize];
                let units = !c.neg && c.ranges.last().is_none_or(|r| r.1 < 0xD800);
                self.push(if self.flags.unicode && !units { Inst::ClassU(*i) } else { Inst::Class(*i) })?;
            }
            Node::WordB(b) => {
                self.push(Inst::WordB(*b))?;
            }
            Node::BackRef(g) => {
                self.push(Inst::BackRef(*g))?;
                if self.flags.unicode {
                    self.push(Inst::NotInPair)?;
                }
            }
            _ => unreachable!(),
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
    fn lookbehind() {
        assert_eq!(run("(?<=a)b", "", "ab"), "1,2");
        assert_eq!(run("(?<!a)b", "", "cb"), "1,2");
        assert_eq!(run("(?<!a)b", "", "ab"), "N");
        assert_eq!(run("(?<=([ab]+)([bc]+))$", "", "abc"), "3,3;0,1;1,3");
        assert_eq!(run("(?<=([ab])+)c", "", "abc"), "2,3;0,1");
        assert_eq!(run("(?<=a|ba)c", "", "bac"), "2,3");
        assert_eq!(run("(?<=a(?=b))b", "", "ab"), "1,2");
        assert_eq!(run("(?<=\\1(a))b", "", "aab"), "2,3;1,2");
        assert_eq!(run("(?<=😀)x", "u", "😀x"), "2,3");
        assert_eq!(run("(?<=k)x", "iu", "Kx"), "1,2");
        assert_eq!(run("(?<=a)x", "i", "Ax"), "1,2");
    }

    #[test]
    fn quantified_lookahead_follows_annex_b() {
        // Checked with Node 22.
        assert_eq!(run("(?=a)*", "", "b"), "0,0");
        assert_eq!(run("(?=(a))*a", "", "ab"), "0,1;-");
        assert_eq!(run("(?=(a))+a", "", "ab"), "0,1;0,1");
        assert_eq!(run("(?=(a)){2}a", "", "ab"), "0,1;0,1");
        assert_eq!(run("(?=(a))*?a", "", "ab"), "0,1;-");
        assert_eq!(run("x(?!b)+c", "", "xbc"), "N");
        assert_eq!(run("x(?!b)+c", "", "xc"), "0,2");
        assert_eq!(run("(?=a){1,3}", "", "zzz"), "N");
        assert!(Regex::from_str("(?=a)*", "u").is_err());
        assert!(Regex::from_str("(?<=a)*", "").is_err());
    }

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
        // With `u`: simple case folding, by code point.
        assert_eq!(run("k", "iu", "\u{212A}"), "0,1");
        assert_eq!(run("\\W", "iu", "\u{17F}"), "N"); // U+017F folds to "s": a word character
        assert_eq!(run("(s)\\1", "iu", "S\u{17F}"), "0,2;0,1");
        assert_eq!(run("\u{10400}", "iu", "\u{10428}"), "0,2");
        assert_eq!(run("\u{1C5}", "iu", "\u{1C4}"), "0,1"); // a group of adjacent code points is one range
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


        assert!(Regex::from_str("(?<n>a)", "").is_ok());
        assert_eq!(std::mem::size_of::<Match>(), 24);
        let re = Regex::from_str("(a)(?<n>b)\\k<n>(?<m>c)?", "").unwrap();
        assert_eq!(re.group_names(), &[("n".to_string(), 2), ("m".to_string(), 3)]);
        assert!(Regex::from_str("(a)", "").unwrap().group_names().is_empty());
        assert!(Regex::from_str("(?=a)*", "").is_ok());
        assert!(matches!(Regex::from_str("(?=a)*", "u"), Err(CompileError::Syntax { .. })));
        for f in ["v", "dv", "gv"] {
            assert!(Regex::from_str("a", f).is_ok(), "{f}");
        }
        for f in ["gg", "x", "G", "é", "uv"] {
            assert!(matches!(Regex::from_str("a", f), Err(CompileError::InvalidFlags(_))), "{f}");
        }
        assert_eq!(Flags::parse("gimsy").unwrap(), Flags { global: true, ignore_case: true, multiline: true, dot_all: true, sticky: true, unicode: false, unicode_sets: false, has_indices: false });
        assert!(Flags::parse("dg").unwrap().has_indices);
        assert!(Flags::parse("u").unwrap().unicode);
    }

    /// Whole-string match of `p` (flag `v` plus `extra`) against `s`.
    fn vfull(p: &str, extra: &str, s: &str) -> bool {
        let re = Regex::from_str(&format!("^(?:{p})$"), &format!("v{extra}")).unwrap();
        re.exec(&u(s), 0, &Limits::default(), None, &mut Scratch::new()).unwrap().is_some()
    }

    #[test]
    fn unicode_sets_flag() {
        let f = Flags::parse("v").unwrap();
        assert!(f.unicode_sets && f.unicode);
        assert!(!Flags::parse("u").unwrap().unicode_sets);
        assert!(matches!(Flags::parse("uv"), Err(CompileError::InvalidFlags(_))));
        // Code point semantics, as with `u`.
        assert!(vfull(".", "", "\u{1F600}"));
    }

    #[test]
    fn unicode_sets_classes() {
        // A plain class and ranges.
        assert!(vfull("[a-c]+", "", "abcba") && !vfull("[a-c]+", "", "abd"));
        assert!(vfull("[^a-c]", "", "d") && !vfull("[^a-c]", "", "b"));
        assert!(vfull("[\\u{1F600}-\\u{1F64F}]", "", "\u{1F610}"));
        // Subtraction.
        assert!(vfull("[\\w--\\d]+", "", "ab_") && !vfull("[\\w--\\d]", "", "5"));
        assert!(vfull("[[a-z]--[aeiou]]+", "", "xyz") && !vfull("[[a-z]--[aeiou]]", "", "e"));
        assert!(vfull("[[a-z]--b--c]+", "", "adz") && !vfull("[[a-z]--b--c]", "", "c"));
        // Intersection.
        assert!(vfull("[\\w&&\\d]+", "", "123") && !vfull("[\\w&&\\d]", "", "a"));
        assert!(vfull("[[a-f]&&[d-z]&&[^e]]+", "", "df") && !vfull("[[a-f]&&[d-z]&&[^e]]", "", "e"));
        // Nested classes and union.
        assert!(vfull("[a[0-9][x-z]]+", "", "a1y") && !vfull("[a[0-9][x-z]]", "", "b"));
        assert!(vfull("[[a-z]--[^a-c]]+", "", "abc") && !vfull("[[a-z]--[^a-c]]", "", "d"));
        assert!(vfull("[^[a-c][x-z]]", "", "m") && !vfull("[^[a-c][x-z]]", "", "y"));
        // Escapes: reserved punctuators and syntax characters.
        assert!(vfull("[\\&\\-\\!\\#\\%\\,\\:\\;\\<\\=\\>\\@\\`\\~]+", "", "&-!#%,:;<=>@`~"));
        assert!(vfull("[\\(\\)\\[\\]\\{\\}\\/\\|]+", "", "()[]{}/|"));
        assert!(vfull("[a\\-z]+", "", "a-z") && !vfull("[a\\-z]", "", "b"));
        // Properties.
        assert!(vfull("[\\p{L}--[a-z]]+", "", "ABé") && !vfull("[\\p{L}--[a-z]]", "", "a"));
        assert!(vfull("[\\p{L}&&\\p{ASCII}]+", "", "az") && !vfull("[\\p{L}&&\\p{ASCII}]", "", "é"));
        // Ignore case.
        assert!(vfull("[a-c]+", "i", "ABC"));
        assert!(vfull("[\\q{ab}x]+", "i", "ABX"));
        // Quantified.
        assert!(vfull("[[a-c]--b]{3}", "", "aca"));
    }

    #[test]
    fn unicode_sets_string_literals() {
        assert!(vfull("[\\q{abc}]", "", "abc") && !vfull("[\\q{abc}]", "", "ab"));
        assert!(vfull("[\\q{abc|d|}x]", "", "abc") && vfull("[\\q{abc|d|}x]", "", "d") && vfull("[\\q{abc|d|}x]", "", "x") && vfull("[\\q{abc|d|}x]", "", ""));
        // The longest string wins over a prefix that is also a member.
        let re = Regex::from_str("[\\q{ab|a}]", "v").unwrap();
        assert_eq!(fmt(re.exec(&u("abc"), 0, &Limits::default(), None, &mut Scratch::new()).unwrap()), "0,2");
        assert_eq!(run("[\\q{ab|a}]", "v", "abc"), "0,2");
        // Strings in operations.
        assert!(vfull("[\\q{ab|cd|e}--\\q{cd}]", "", "ab") && !vfull("[\\q{ab|cd|e}--\\q{cd}]", "", "cd"));
        assert!(vfull("[\\q{ab|cd|e}--\\q{cd}]", "", "e"));
        assert!(vfull("[\\q{ab|cd|e}&&\\q{cd|e|f}]", "", "cd") && !vfull("[\\q{ab|cd|e}&&\\q{cd|e|f}]", "", "ab"));
        assert!(vfull("[\\q{ab|cd|e}&&\\q{cd|e|f}]", "", "e"));
        // A one code point string is a class member (also in a subtraction from a range).
        assert!(vfull("[[a-c]--\\q{b}]+", "", "ac") && !vfull("[[a-c]--\\q{b}]", "", "b"));
        assert!(vfull("[\\q{\\u{1F600}}]", "", "\u{1F600}"));
        // Escapes inside \q.
        assert!(vfull("[\\q{a\\|b|c\\}}]", "", "a|b") && vfull("[\\q{a\\|b|c\\}}]", "", "c}"));
        assert!(vfull("[\\q{\\n\\x41}]", "", "\nA"));
    }

    #[test]
    fn unicode_sets_syntax_errors() {
        for p in [
            "[a&&&b]", "[a&&b--c]", "[a--b&&c]", "[ab&&c]", "[a&&bc]", "[a-z&&b]", "[a-z--b]", "[a--]", "[--a]", "[&&a]",
            "[(]", "[)]", "[{]", "[}]", "[/]", "[|]", "[-]", "[a-]", "[[a]", "[a]]", "[!!]", "[##]", "[a^^]",
            "[^\\q{ab}]", "[^\\q{}]", "[\\q{a]", "[\\q{a}", "[\\q]", "[z-a]", "[\\d-z]", "[a-\\d]", "[\\p{RGI_Emoji}]", "[\\k<a>]",
        ] {
            assert!(matches!(Regex::from_str(p, "v"), Err(CompileError::Syntax { .. })), "{p}");
        }
        // Valid in `v`.
        for p in ["[a&b]", "[a!b]", "[^\\q{a|b}]", "[\\q{}]", "[]", "[^]", "[a-z]", "[a\\-b]"] {
            assert!(Regex::from_str(p, "v").is_ok(), "{p}");
        }
        // Without `v` the old syntax stays: `&&` and `--` are plain members.
        for p in ["[a&&b]", "[(]", "[!--b]", "[\\q]"] {
            assert!(Regex::from_str(p, "").is_ok(), "{p}");
        }
        for p in ["[a&&b]", "[(]", "[!--b]"] {
            assert!(Regex::from_str(p, "u").is_ok(), "{p}");
        }
        // A group count scan is not fooled by a `(` inside a nested class.
        assert_eq!(Regex::from_str("[[a]\\(](b)", "v").map(|r| r.group_count()), Ok(1));
    }

    #[test]
    fn unicode_property_escapes() {
        // General_Category, short and long names, in the `u` mode.
        assert_eq!(run("\\p{L}+", "u", "12héllo wörld"), "2,7");
        assert_eq!(run("\\p{Letter}+", "u", "12héllo wörld"), "2,7");
        assert_eq!(run("\\p{Lu}", "u", "abcDef"), "3,4");
        assert_eq!(run("\\p{Uppercase_Letter}", "u", "abcDef"), "3,4");
        assert_eq!(run("\\p{gc=Lu}", "u", "abcDef"), "3,4");
        assert_eq!(run("\\p{General_Category=Ll}+", "u", "ABcdE"), "2,4");
        assert_eq!(run("\\p{N}+", "u", "ab٣4Ⅷ½x"), "2,6");
        assert_eq!(run("\\p{Nd}+", "u", "ab٣4Ⅷ"), "2,4");
        assert_eq!(run("\\p{P}+", "u", "ab!?。c"), "2,5");
        assert_eq!(run("\\p{S}", "u", "ab+c"), "2,3");
        assert_eq!(run("\\p{Z}", "u", "ab c"), "2,3");
        assert_eq!(run("\\p{C}", "u", "ab\u{1}c"), "2,3");
        // Negated, and inside a class.
        assert_eq!(run("\\P{L}+", "u", "ab12-c"), "2,5");
        assert_eq!(run("[\\p{Lu}\\d]+", "u", "abD1eF2g"), "2,4");
        assert_eq!(run("[^\\p{L}]+", "u", "ab12-c"), "2,5");
        assert_eq!(run("[\\P{L}a]+", "u", "b1a-b"), "1,4");
        // Scripts.
        assert_eq!(run("\\p{Script=Greek}+", "u", "abαβγd"), "2,5");
        assert_eq!(run("\\p{sc=Grek}+", "u", "abαβγd"), "2,5");
        assert_eq!(run("\\p{Script=Cyrillic}+", "u", "abжиd"), "2,4");
        assert_eq!(run("\\p{Script=Han}+", "u", "ab字漢d"), "2,4");
        assert_eq!(run("\\p{Script=Hiragana}+", "u", "abあいd"), "2,4");
        assert_eq!(run("\\p{sc=Katakana}+", "u", "abカタd"), "2,4");
        assert_eq!(run("\\p{sc=Arabic}+", "u", "abعربd"), "2,5");
        assert_eq!(run("\\p{sc=Hebrew}+", "u", "abאבd"), "2,4");
        assert_eq!(run("\\p{sc=Devanagari}+", "u", "abअबd"), "2,4");
        assert_eq!(run("\\p{sc=Thai}+", "u", "abกขd"), "2,4");
        assert_eq!(run("\\p{sc=Hangul}+", "u", "ab한글d"), "2,4");
        assert_eq!(run("\\p{sc=Latin}+", "u", "12abé3"), "2,5");
        // Script_Extensions: the prolonged sound mark is Common, and also in Hiragana and Katakana.
        assert_eq!(run("\\p{sc=Hiragana}+", "u", "あー"), "0,1");
        assert_eq!(run("\\p{scx=Hiragana}+", "u", "あー"), "0,2");
        assert_eq!(run("\\p{Script_Extensions=Katakana}+", "u", "カー"), "0,2");
        // Binary properties.
        assert_eq!(run("\\p{Alphabetic}+", "u", "12ab字3"), "2,5");
        assert_eq!(run("\\p{White_Space}", "u", "ab\u{3000}c"), "2,3");
        assert_eq!(run("\\p{Uppercase}", "u", "abCd"), "2,3");
        assert_eq!(run("\\p{Lowercase}", "u", "ABcD"), "2,3");
        assert_eq!(run("\\p{ASCII}+", "u", "éabcé"), "1,4");
        assert_eq!(run("\\P{ASCII}+", "u", "abéé"), "2,4");
        assert_eq!(run("^\\p{Any}+$", "u", "a😀\u{10ffff}"), "0,5");
        assert_eq!(run("\\p{Assigned}", "u", "\u{378}a"), "1,2");
        assert_eq!(run("\\p{Emoji}", "u", "ab😀"), "2,4");
        assert_eq!(run("\\p{ID_Start}+", "u", "1ab字_"), "1,4");
        assert_eq!(run("\\p{ID_Continue}+", "u", "-ab1_-"), "1,5");
        // An astral code point is one match.
        assert_eq!(run("^\\p{So}$", "u", "😀"), "0,2");
        assert_eq!(run("^\\p{L}$", "iu", "A"), "0,1");
        // Without `u`, `\p` is the letter p.
        assert_eq!(run("\\p{L}", "", "p{L}"), "0,4");
        // A name that is not known, or not well made, is a SyntaxError.
        for bad in [
            "\\p{Foo}", "\\p{lu}", "\\p{Script}", "\\p{General_Category}", "\\p{Latin}", "\\p{sc=Foo}", "\\p{sc=latin}", "\\p{L", "\\p", "\\p{}", "\\p{ L}", "\\p{L }", "\\p{sc=}",
            "\\p{=Latin}", "\\p{sc=Latin=x}", "\\P{Foo}", "[\\p{Foo}]", "\\p{Block=Basic_Latin}", "\\p{Alphabetic=Yes}", "\\p{Other_Alphabetic}", "\\p{L-}", "[\\p{L}-z]", "\\pL",
        ] {
            assert!(matches!(Regex::from_str(bad, "u"), Err(CompileError::Syntax { .. })), "{bad}");
        }
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
