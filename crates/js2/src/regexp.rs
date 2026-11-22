//! JavaScript regular expressions for js2 (ECMA-262 `RegExp` pattern matching, with and without `u`): the backtracking side of
//! `docs/design-js2-regexp.md` (step R1). It is the matcher of the old engine (`crates/js/src/regexp.rs`), ported to a flat program.
//!
//! Safe code: this file holds no `unsafe`. It uses no JS value, no heap and no realm. It works on `&[u8]` (a Latin-1 string) or `&[u16]`.
//!
//! - [`compile`] turns a pattern (UTF-16 code units) and a flags string into an **image**: a `Vec<u64>` that holds the whole program
//!   (the header, the first-unit set, the instructions, the class table and the class ranges). Every field is a plain word, so the
//!   image can sit in a `Blob` cell of the heap (`blob.rs`) with no `Vec`, no `Box` and no finalizer. The R2 linear side
//!   (`regexp_linear.rs`) reads the same image: it is the one program of a pattern.
//! - [`exec`] runs an image on one input from a start index. It writes the spans of the match into a slice of the caller, so a run
//!   allocates nothing (the registers and the backtrack stack are kept in a [`Scratch`] between runs).
//!
//! What the matcher does *not* do (the wrapper in `regexp_rt.rs` does): read or write `lastIndex`, build the result array, run
//! `Symbol.replace` and friends. `exec` honours the `y` flag of the image (match only at the start index); `g` is data of the wrapper.
//!
//! Budget. Every step of the matcher is counted. A block of 256 steps costs [`FUEL_PER_BLOCK`] units of the fuel that the caller
//! gives (`Vm::fuel`), and one run has the cap [`MAX_STEPS`] too. Running out of either is an error, never "no match".
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
//! Compile and run are bounded: see the `MAX_*` constants. A limit is an error, never "no match".

use bare_unicode::fold;
use bare_unicode::tables::{CASE_FOLD, CASE_FOLD_INV};

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
/// The most steps of one run (the cap of design decision 3).
pub const MAX_STEPS: u64 = 50_000_000;
/// The longest input of one run, in units.
pub const MAX_INPUT_UNITS: usize = 1 << 24;
/// The most frames of the backtrack stack.
const MAX_FRAMES: usize = 1 << 22;
/// The units of fuel that one block of 256 steps costs. A step is about the cost of one op of the interpreter, and a back edge costs
/// 1 unit, so 256 steps cost as much as 16 loop turns of 8 ops (set by measurement, `docs/progress.md`).
pub const FUEL_PER_BLOCK: u64 = 16;

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

/// The letters of the flags, in the order of the bits of [`Flags::bits`] (the order of the `flags` getter).
pub const FLAG_LETTERS: &[u8; 8] = b"dgimsuvy";

impl Flags {
    /// Parse a flags string. Unknown or repeated flags are errors, and so is `u` with `v`.
    pub fn parse(s: &str) -> Result<Flags, CompileError> {
        let mut f = Flags::default();
        let mut seen = [false; 128];
        for c in s.chars() {
            let i = c as usize;
            if i >= 128 || !FLAG_LETTERS.contains(&(c as u8)) {
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

    /// One bit for each letter of [`FLAG_LETTERS`]: `d` is bit 0, `y` is bit 7. `u` and `v` are separate bits.
    pub fn bits(self) -> u8 {
        [self.has_indices, self.global, self.ignore_case, self.multiline, self.dot_all, self.unicode && !self.unicode_sets, self.unicode_sets, self.sticky]
            .iter()
            .enumerate()
            .fold(0, |a, (i, &b)| a | (b as u8) << i)
    }

    /// The flags of [`Flags::bits`].
    pub fn from_bits(b: u8) -> Flags {
        let on = |i: u32| b >> i & 1 != 0;
        Flags { has_indices: on(0), global: on(1), ignore_case: on(2), multiline: on(3), dot_all: on(4), unicode: on(5) || on(6), unicode_sets: on(6), sticky: on(7) }
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
    /// The fuel of the caller ran out.
    Fuel,
    /// The run passed [`MAX_STEPS`].
    Steps,
    /// The backtrack stack reached its limit.
    Stack,
    /// The input is longer than [`MAX_INPUT_UNITS`].
    Input,
}

/// A unit of the input: a byte of a one-byte string or a 16-bit unit. The matcher is generic over it, so a one-byte string needs no copy.
pub trait Unit: Copy {
    fn u(self) -> u16;
}

impl Unit for u8 {
    #[inline(always)]
    fn u(self) -> u16 {
        self as u16
    }
}

impl Unit for u16 {
    #[inline(always)]
    fn u(self) -> u16 {
        self
    }
}

/// Reusable working memory. Reuse one across calls to avoid allocating per match.
#[derive(Default)]
pub struct Scratch {
    regs: Vec<u32>,
    stack: Vec<Frame>,
    /// The steps of the last run (for the tests and the cost rows).
    pub steps: u64,
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

/// The step counter of one run. It counts down inside a block of 256 steps, so a step costs one subtraction and one test; the
/// fuel of the caller is charged when a block is used up.
struct Steps<'a> {
    /// Steps left in the block.
    left: i64,
    /// Steps of the finished blocks.
    done: u64,
    fuel: &'a mut u64,
}

const BLOCK: i64 = 256;

impl Steps<'_> {
    #[inline(always)]
    fn charge(&mut self, work: u64) -> Result<(), ExecError> {
        // `work` is at most the registers of a program or the length of an input, so it fits an `i64`.
        self.left -= work as i64;
        if self.left <= 0 {
            return self.blocks();
        }
        Ok(())
    }

    /// The steps of the run so far.
    fn total(&self) -> u64 {
        self.done + (BLOCK - self.left) as u64
    }

    /// The block is used up: charge the fuel, and check the cap of the run.
    #[cold]
    #[inline(never)]
    fn blocks(&mut self) -> Result<(), ExecError> {
        let used = (BLOCK - self.left) as u64;
        let rem = used & 255;
        self.done += used - rem;
        self.left = BLOCK - rem as i64;
        if self.done > MAX_STEPS {
            return Err(ExecError::Steps);
        }
        let cost = (used >> 8) * FUEL_PER_BLOCK;
        if *self.fuel < cost {
            *self.fuel = 0;
            return Err(ExecError::Fuel);
        }
        *self.fuel -= cost;
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// The image
// ---------------------------------------------------------------------------------------------
//
// Words of an image, all `u64`:
//   0: instructions | classes << 32          1: groups | registers << 32       2: tmp_base | mark_base << 32
//   3: flags bits | anchored << 8 | has first set << 9 | has names << 10 | has last set << 11 | class ranges << 32
//   then: the first-unit set (2 words if present), instructions, class table, class ranges, names, last-unit set (2 words if present).
// An instruction is `op | a << 8 | b << 36` (`Inst::enc`). A class table entry is `first range | length << 32 | negated << 63`.
// A class range is `lo << 32 | hi`. A name follows the ranges as index | length << 32 and packed UTF-16 units.

const HAS_LAST_SET: u64 = 1 << 11;

/// A compiled pattern, read from its image.
struct Prog<'a> {
    insts: &'a [u64],
    ctab: &'a [u64],
    ranges: &'a [u64],
    /// The ASCII units that a match can start with, as 2 words of bits. Empty when the set is unknown.
    first: &'a [u64],
    groups: usize,
    nregs: usize,
    tmp_base: usize,
    mark_base: usize,
    flags: Flags,
    anchored: bool,
}

impl<'a> Prog<'a> {
    #[inline]
    fn new(w: &'a [u64]) -> Prog<'a> {
        let (ni, nc, nr) = (w[0] as u32 as usize, (w[0] >> 32) as usize, (w[3] >> 32) as usize);
        let nf = if w[3] >> 9 & 1 != 0 { 2 } else { 0 };
        let (first, rest) = w[4..].split_at(nf);
        let (insts, rest) = rest.split_at(ni);
        let (ctab, rest) = rest.split_at(nc);
        Prog {
            insts,
            ctab,
            ranges: &rest[..nr],
            first,
            groups: w[1] as u32 as usize,
            nregs: (w[1] >> 32) as usize,
            tmp_base: w[2] as u32 as usize,
            mark_base: (w[2] >> 32) as usize,
            flags: Flags::from_bits(w[3] as u8),
            anchored: w[3] >> 8 & 1 != 0,
        }
    }

    /// `c` is a code unit without `u`, a code point with `u`. `icase` is for non-Unicode `i` only.
    #[inline]
    fn class_matches(&self, i: u32, c: u32, icase: bool) -> bool {
        let e = self.ctab[i as usize];
        let (start, len, neg) = (e as u32 as usize, (e >> 32) as u32 as usize & 0x7FFF_FFFF, e >> 63 != 0);
        let r = &self.ranges[start..start + len];
        let k = if icase { canon(c as u16) as u32 } else { c };
        let hit = if r.len() <= 8 {
            r.iter().any(|&w| (w >> 32) as u32 <= k && k <= w as u32)
        } else {
            let i = r.partition_point(|&w| (w as u32) < k);
            i < r.len() && (r[i] >> 32) as u32 <= k
        };
        hit != neg
    }
}

/// Compile `pattern` (UTF-16 code units) with `flags` (e.g. `"gi"`) into an image.
pub fn compile(pattern: &[u16], flags: &str) -> Result<Vec<u64>, CompileError> {
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
    let first = first_set(&e.prog, &e.classes, flags);
    let last = last_set(&node, flags);
    let anchored = matches!(e.prog[0], Inst::Start);
    // The class ranges, then the table.
    let mut ranges: Vec<u64> = Vec::new();
    let mut ctab: Vec<u64> = Vec::with_capacity(e.classes.len());
    for c in &e.classes {
        ctab.push(ranges.len() as u64 | (c.ranges.len() as u64) << 32 | (c.neg as u64) << 63);
        ranges.extend(c.ranges.iter().map(|&(lo, hi)| (lo as u64) << 32 | hi as u64));
    }
    let mut w: Vec<u64> = Vec::with_capacity(4 + 2 * (usize::from(first.is_some()) + usize::from(last.is_some())) + e.prog.len() + ctab.len() + ranges.len());
    w.push(e.prog.len() as u64 | (ctab.len() as u64) << 32);
    w.push(groups as u64 | (nregs as u64) << 32);
    w.push(tmp_base as u64 | (mark_base as u64) << 32);
    let has_last = if last.is_some() { HAS_LAST_SET } else { 0 };
    w.push(flags.bits() as u64 | (anchored as u64) << 8 | (first.is_some() as u64) << 9 | (!p.names.is_empty() as u64) << 10 | has_last | (ranges.len() as u64) << 32);
    if let Some(f) = first {
        w.extend(f);
    }
    w.extend(e.prog.iter().map(|i| i.enc()));
    w.extend(ctab);
    w.extend(ranges);
    for (name, index) in p.names {
        let units: Vec<_> = name.encode_utf16().collect();
        w.push(index as u64 | (units.len() as u64) << 32);
        w.extend(units.chunks(4).map(|chunk| {
            chunk.iter().enumerate().fold(0, |word, (i, &u)| word | (u as u64) << (16 * i))
        }));
    }
    if let Some(last) = last {
        w.extend([last as u64, (last >> 64) as u64]);
    }
    Ok(w)
}

/// Check `pattern` and `flags` without building a program: the flags, the length and the grammar of the pattern. Everything that
/// [`compile`] reports before it emits code is reported here, with the same error. The program limits (code size, nesting of the
/// emit pass) are not checked: they show at [`compile`].
pub fn check(pattern: &[u16], flags: &str) -> Result<(), CompileError> {
    let flags = Flags::parse(flags)?;
    if pattern.len() > MAX_PATTERN_UNITS {
        return Err(CompileError::Limit("pattern too long"));
    }
    Parser::new(pattern, flags).parse_pattern().map(drop)
}

/// The flags of an image.
pub fn image_flags(img: &[u64]) -> Flags {
    Flags::from_bits(img[3] as u8)
}

/// Whether a match result needs named properties or an indices array.
#[inline]
pub(crate) fn image_extra_groups(w: &[u64]) -> bool {
    w[3] & (1 | 1 << 10) != 0
}

/// The names in capture order, with their group numbers.
pub(crate) fn image_names(w: &[u64]) -> impl Iterator<Item = (Vec<u16>, usize)> + '_ {
    let end = 4 + 2 * ((w[3] >> 9 & 1) as usize) + w[0] as u32 as usize + (w[0] >> 32) as usize + (w[3] >> 32) as usize;
    let names_end = w.len() - 2 * usize::from(w[3] & HAS_LAST_SET != 0);
    let mut words = &w[end..names_end];
    std::iter::from_fn(move || {
        let (&head, rest) = words.split_first()?;
        let len = (head >> 32) as usize;
        let (name, tail) = rest.split_at(len.div_ceil(4));
        words = tail;
        Some(((0..len).map(|i| (name[i / 4] >> (16 * (i % 4))) as u16).collect(), head as u32 as usize))
    })
}

/// The number of capture groups of an image, not counting group 0.
pub fn image_groups(img: &[u64]) -> usize {
    img[1] as u32 as usize
}

/// Collect the ASCII units a match can begin with, as 2 words of bits. Returns `None` when a match may be empty
/// or the walk meets something it does not model (then every start position is tried).
fn first_set(prog: &[Inst], classes: &[ClassSet], flags: Flags) -> Option<[u64; 2]> {
    let mut set = [false; 128];
    let mut seen = vec![false; prog.len()];
    let mut todo = vec![0usize];
    while let Some(pc) = todo.pop() {
        if std::mem::replace(&mut seen[pc], true) {
            continue;
        }
        match prog[pc] {
            Inst::Char(c) => {
                if c < 128 {
                    set[c as usize] = true;
                }
            }
            Inst::CharI(k) => (0..128u16).filter(|&c| canon(c) == k).for_each(|c| set[c as usize] = true),
            Inst::Any | Inst::AnyAll | Inst::AnyU | Inst::AnyAllU => return None,
            Inst::Class(i) => (0..128u32).filter(|&c| classes[i as usize].matches(c, flags.ignore_case && !flags.unicode)).for_each(|c| set[c as usize] = true),
            Inst::ClassU(i) => (0..128u32).filter(|&c| classes[i as usize].matches(c, false)).for_each(|c| set[c as usize] = true),
            // Zero-width steps: a superset of the real first units is still sound.
            Inst::Start | Inst::End | Inst::MStart | Inst::MEnd | Inst::WordB(_) | Inst::NotInPair | Inst::Mark(_) | Inst::GroupStart(_) | Inst::Close(_) | Inst::Reset(..) | Inst::EmptyCheck(_) => todo.push(pc + 1),
            Inst::Jmp(t) => todo.push(t as usize),
            Inst::Split(a, b) => todo.extend([a as usize, b as usize]),
            Inst::Look(_, end) => todo.push(end as usize),
            Inst::BackRef(_) | Inst::LookEnd | Inst::Backward | Inst::ReverseClose(_) | Inst::Match => return None,
        }
    }
    let mut w = [0u64; 2];
    set.iter().enumerate().filter(|(_, b)| **b).for_each(|(c, _)| w[c >> 6] |= 1 << (c & 63));
    Some(w)
}

/// A non-multiline `$` can only match at the end of the input. Record the possible ASCII literals
/// immediately before it, if every alternative consumes one. Keep the full matcher for any other suffix.
fn last_set(node: &Node, flags: Flags) -> Option<u128> {
    if flags.multiline || flags.ignore_case || flags.unicode {
        return None;
    }
    let Node::Cat(nodes) = node else { return None };
    let [.., tail, Node::End] = nodes.as_slice() else { return None };

    fn literals(node: &Node) -> Option<u128> {
        match node {
            Node::Char(c) if *c < 128 => Some(1 << c),
            Node::Group(_, body) => literals(body),
            Node::Cat(nodes) => literals(nodes.last()?),
            Node::Alt(nodes) => nodes.iter().try_fold(0, |set, node| Some(set | literals(node)?)),
            _ => None,
        }
    }
    literals(tail)
}

// ---------------------------------------------------------------------------------------------
// The matcher
// ---------------------------------------------------------------------------------------------

/// Find the first match of the image `img` in `input` at or after `from` (only at `from` with the `y` flag). `out` receives the
/// spans: two words for each group from group 0, `u32::MAX` for a group that did not take part; it holds `2 * (groups + 1)` words at
/// least. `Ok(false)` means no match, including `from > input.len()`. `fuel` is the budget of the caller (see the header).
#[inline]
pub fn exec<U: Unit>(img: &[u64], input: &[U], from: usize, sc: &mut Scratch, fuel: &mut u64, out: &mut [u32]) -> Result<bool, ExecError> {
    if img[3] & HAS_LAST_SET != 0 && from <= input.len() && input.len() <= MAX_INPUT_UNITS {
        let viable = input.last().is_some_and(|unit| {
            let c = unit.u() as usize;
            c < 128 && img[img.len() - 2 + (c >> 6)] >> (c & 63) & 1 != 0
        });
        if !viable {
            // One unit check costs less than a block. No candidate reached the matcher.
            sc.steps = 1;
            return Ok(false);
        }
    }
    let p = Prog::new(img);
    let mut st = Steps { left: BLOCK, done: 0, fuel };
    let r = if p.flags.unicode && p.flags.ignore_case { p.exec_in::<U, true>(input, from, sc, &mut st, out) } else { p.exec_in::<U, false>(input, from, sc, &mut st, out) };
    sc.steps = st.total();
    r
}

/// `AdvanceStringIndex`: the next index after `i`. With `u`, a surrogate pair at `i` is skipped whole.
pub fn advance<U: Unit>(unicode: bool, input: &[U], i: usize) -> usize {
    if unicode && i < input.len() { i + code_point(input, i).1 } else { i + 1 }
}

#[derive(Clone, Copy, Debug)]
enum Frame {
    Branch(u32, u32),
    Restore(u32, u32),
    Look { neg: bool, pos: u32, end: u32 },
}

impl Prog<'_> {
    /// `FOLD`: `i` with `u`. `\b` and backreferences then use simple case folding. A const parameter compiles a
    /// separate matcher for it, so the other patterns run a loop without that code. Extra instructions or call
    /// arguments in the shared loop made `short` and `caps` 2 to 5% slower (measured on the old engine).
    #[inline(never)]
    fn exec_in<U: Unit, const FOLD: bool>(&self, input: &[U], last_index: usize, sc: &mut Scratch, st: &mut Steps, out: &mut [u32]) -> Result<bool, ExecError> {
        if input.len() > MAX_INPUT_UNITS {
            return Err(ExecError::Input);
        }
        if last_index > input.len() {
            return Ok(false);
        }
        let mut s = last_index;
        // With `u`, an index inside a surrogate pair first tries from the start of the pair, then from the index
        // itself: V8 compiles this choice (`OptionallyStepBackToLeadSurrogate`); the oracle checks it against Node.
        if self.flags.unicode && inside_pair(input, s) && self.attempt::<U, FOLD>(input, s - 1, sc, st)? {
            self.spans(sc, out);
            return Ok(true);
        }
        if self.anchored && !self.flags.sticky && s > 0 {
            return Ok(false);
        }
        loop {
            if !self.first.is_empty() && !self.flags.sticky {
                // A match consumes at least one unit and cannot start with a skipped one.
                let from = s;
                while s < input.len() {
                    let c = input[s].u();
                    if c >= 128 || self.first[(c >> 6) as usize] >> (c & 63) & 1 != 0 {
                        break;
                    }
                    s += 1;
                }
                st.charge((s - from) as u64)?;
                if s >= input.len() {
                    return Ok(false);
                }
            }
            st.charge(self.nregs as u64)?;
            sc.regs.clear();
            sc.regs.resize(self.nregs, UNSET);
            sc.stack.clear();
            if self.run::<U, FOLD>(input, s, sc, st)? {
                self.spans(sc, out);
                return Ok(true);
            }
            if self.flags.sticky || s == input.len() || self.anchored {
                return Ok(false);
            }
            // By one code unit, also with `u`: a zero-width match can sit inside a surrogate pair, as in V8. A
            // character never starts there (see `char_at`).
            s += 1;
        }
    }

    /// The cold first attempt of a `u` index inside a surrogate pair, at the start of the pair.
    #[cold]
    #[inline(never)]
    fn attempt<U: Unit, const FOLD: bool>(&self, input: &[U], s: usize, sc: &mut Scratch, st: &mut Steps) -> Result<bool, ExecError> {
        st.charge(self.nregs as u64)?;
        sc.regs.clear();
        sc.regs.resize(self.nregs, UNSET);
        sc.stack.clear();
        self.run::<U, FOLD>(input, s, sc, st)
    }

    fn spans(&self, sc: &Scratch, out: &mut [u32]) {
        out[..2 * (self.groups + 1)].copy_from_slice(&sc.regs[..2 * (self.groups + 1)]);
        // A group with one end unset did not take part.
        for g in 0..=self.groups {
            if out[2 * g] == UNSET || out[2 * g + 1] == UNSET {
                (out[2 * g], out[2 * g + 1]) = (UNSET, UNSET);
            }
        }
    }

    /// One `u` character step (`AnyU`, `AnyAllU`, `ClassU`) at `pos`: the position after it, or `None`. Out of line,
    /// so the dispatch loop keeps its registers for the code-unit instructions.
    #[inline(never)]
    fn read_u<U: Unit>(&self, input: &[U], pos: usize, i: Inst) -> Option<usize> {
        let (c, w) = char_at(input, pos)?;
        let ok = match i {
            Inst::AnyU => !is_line_terminator_cp(c),
            Inst::ClassU(k) => self.class_matches(k, c, false),
            _ => true,
        };
        ok.then_some(pos + w)
    }

    /// A backreference with `i` and `u`: compare simple case foldings, one code point at a time. A folding can
    /// change the length in units, so each side walks on its own. Returns the end position and the work done.
    fn backref_fold<U: Unit>(&self, input: &[U], regs: &[u32], g: u32, pos: usize) -> (Option<usize>, u64) {
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
    fn backward<U: Unit>(&self, input: &[U], regs: &[u32], pos: usize, inst: Inst, st: &mut Steps) -> Result<Option<usize>, ExecError> {
        // Read a complete code point before the assertion position in Unicode mode.
        let prev = |p: usize| {
            let mut q = p.checked_sub(1)?;
            if self.flags.unicode && q > 0 && (0xDC00..=0xDFFF).contains(&input[q].u()) && (0xD800..=0xDBFF).contains(&input[q - 1].u()) {
                q -= 1;
            }
            Some(q)
        };
        if let Inst::BackRef(g) = inst {
            let (s, e) = (regs[2 * g as usize], regs[2 * g as usize + 1]);
            if s == UNSET || e == UNSET {
                return Ok(Some(pos));
            }
            let (mut a, mut b) = (e as usize, pos);
            while a > s as usize {
                st.charge(1)?;
                let Some(x) = prev(a) else { return Ok(None) };
                let Some(y) = prev(b) else { return Ok(None) };
                let (c, d) = if self.flags.unicode { (code_point(input, x).0, code_point(input, y).0) } else { (input[x].u() as u32, input[y].u() as u32) };
                let equal = if self.flags.ignore_case {
                    if self.flags.unicode { fold(c) == fold(d) } else { canon(c as u16) == canon(d as u16) }
                } else {
                    c == d
                };
                if !equal {
                    return Ok(None);
                }
                (a, b) = (x, y);
            }
            return Ok(Some(b));
        }
        let unicode = matches!(inst, Inst::AnyU | Inst::AnyAllU | Inst::ClassU(_));
        let p = if unicode { prev(pos) } else { pos.checked_sub(1) };
        let Some(p) = p else { return Ok(None) };
        let c = input[p].u();
        let ok = match inst {
            Inst::Char(k) => c == k,
            Inst::CharI(k) => canon(c) == k,
            Inst::Any => !is_line_terminator(c),
            Inst::AnyAll => true,
            Inst::Class(k) => self.class_matches(k, c as u32, self.flags.ignore_case && !self.flags.unicode),
            Inst::AnyU | Inst::AnyAllU | Inst::ClassU(_) => self.read_u(input, p, inst) == Some(pos),
            _ => unreachable!(),
        };
        Ok(ok.then_some(p))
    }

    // Inlined into both callers: `exec_in` is the hot one.
    #[inline(always)]
    fn run<U: Unit, const FOLD: bool>(&self, input: &[U], start: usize, sc: &mut Scratch, st: &mut Steps) -> Result<bool, ExecError> {
        let Scratch { regs, stack, .. } = sc;
        // Non-Unicode `i` only. `i` with `u` is `FOLD`: its classes are already closed over folding.
        let icase = self.flags.ignore_case && !FOLD;
        let len = input.len();
        let mut pc = 0usize;
        let mut pos = start;
        let mut failed = false;
        regs[0] = start as u32;

        macro_rules! push {
            ($f:expr) => {{
                if stack.len() >= MAX_FRAMES {
                    return Err(ExecError::Stack);
                }
                stack.push($f);
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
                st.charge(1)?;
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
            st.charge(1)?;
            let w = self.insts[pc];
            let (ia, ib) = ((w >> 8) as u32 & 0x0FFF_FFFF, (w >> 36) as u32);
            // The opcodes are those of `Inst::dec`.
            match w as u8 {
                0 => {
                    let c = ia as u16;
                    if pos < len && input[pos].u() == c {
                        pos += 1;
                        pc += 1;
                    } else {
                        failed = true;
                    }
                }
                1 => {
                    let c = ia as u16;
                    if pos < len && canon(input[pos].u()) == c {
                        pos += 1;
                        pc += 1;
                    } else {
                        failed = true;
                    }
                }
                2 => {
                    if pos < len && !is_line_terminator(input[pos].u()) {
                        pos += 1;
                        pc += 1;
                    } else {
                        failed = true;
                    }
                }
                3 => {
                    if pos < len {
                        pos += 1;
                        pc += 1;
                    } else {
                        failed = true;
                    }
                }
                4 => {
                    let i = ia;
                    if pos < len && self.class_matches(i, input[pos].u() as u32, icase) {
                        pos += 1;
                        pc += 1;
                    } else {
                        failed = true;
                    }
                }
                5..=7 => match self.read_u(input, pos, Inst::dec(w)) {
                    Some(p) => {
                        pos = p;
                        pc += 1;
                    }
                    None => failed = true,
                },
                8 => {
                    if pos == 0 {
                        pc += 1
                    } else {
                        failed = true
                    }
                }
                9 => {
                    if pos == len {
                        pc += 1
                    } else {
                        failed = true
                    }
                }
                10 => {
                    if pos == 0 || is_line_terminator(input[pos - 1].u()) {
                        pc += 1
                    } else {
                        failed = true
                    }
                }
                11 => {
                    if pos == len || is_line_terminator(input[pos].u()) {
                        pc += 1
                    } else {
                        failed = true
                    }
                }
                12 => {
                    let want = ia != 0;
                    let a = pos > 0 && is_word_in::<FOLD>(input[pos - 1].u());
                    let b = pos < len && is_word_in::<FOLD>(input[pos].u());
                    if (a != b) == want {
                        pc += 1
                    } else {
                        failed = true
                    }
                }
                13 if FOLD => {
                    let g = ia;
                    let (r, n) = self.backref_fold(input, regs, g, pos);
                    st.charge(n)?;
                    match r {
                        Some(p) => {
                            pos = p;
                            pc += 1;
                        }
                        None => failed = true,
                    }
                }
                13 => {
                    let g = ia;
                    let (s, e) = (regs[2 * g as usize], regs[2 * g as usize + 1]);
                    if s == UNSET || e == UNSET {
                        pc += 1;
                    } else {
                        let (s, e) = (s as usize, e as usize);
                        let n = e - s;
                        let mut ok = pos + n <= len;
                        if ok {
                            for k in 0..n {
                                st.charge(1)?;
                                let (x, y) = (input[s + k].u(), input[pos + k].u());
                                if x != y && !(icase && canon(x) == canon(y)) {
                                    ok = false;
                                    break;
                                }
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
                14 => {
                    if inside_pair(input, pos) {
                        failed = true;
                    } else {
                        pc += 1;
                    }
                }
                15 => {
                    let (a, b) = (ia, ib);
                    push!(Frame::Branch(b, pos as u32));
                    pc = a as usize;
                }
                16 => pc = ia as usize,
                17 => {
                    let r = ia;
                    set!(self.mark_base + r as usize, pos as u32);
                    pc += 1;
                }
                18 => {
                    let g = ia;
                    set!(self.tmp_base + g as usize, pos as u32);
                    pc += 1;
                }
                19 => {
                    let r = ia;
                    if regs[self.mark_base + r as usize] == pos as u32 {
                        failed = true;
                    } else {
                        pc += 1;
                    }
                }
                20 => {
                    let g = ia;
                    let g = g as usize;
                    let s0 = regs[self.tmp_base + g];
                    set!(2 * g, s0);
                    set!(2 * g + 1, pos as u32);
                    pc += 1;
                }
                21 => {
                    let (lo, hi) = (ia, ib);
                    for g in lo as usize..hi as usize {
                        st.charge(1)?;
                        if regs[2 * g] != UNSET || regs[2 * g + 1] != UNSET {
                            set!(2 * g, UNSET);
                            set!(2 * g + 1, UNSET);
                        }
                    }
                    pc += 1;
                }
                22 => {
                    let (neg, end) = (ia != 0, ib);
                    push!(Frame::Look { neg, pos: pos as u32, end });
                    pc += 1;
                }
                23 => {
                    // The nearest barrier on the stack belongs to this lookahead: inner ones are gone.
                    let mut idx = stack.len();
                    loop {
                        st.charge(1)?;
                        idx = idx.checked_sub(1).expect("barrier");
                        if matches!(stack[idx], Frame::Look { .. }) {
                            break;
                        }
                    }
                    let Frame::Look { neg, pos: s, end } = stack[idx] else { unreachable!() };
                    if neg {
                        // The body matched, so `(?!...)` fails. Undo the body's captures.
                        while let Some(f) = stack.pop() {
                            st.charge(1)?;
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
                            st.charge(1)?;
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
                24 => match self.backward(input, regs, pos, Inst::dec(self.insts[pc + 1]), st)? {
                    Some(p) => {
                        pos = p;
                        pc += 2;
                    }
                    None => failed = true,
                },
                25 => {
                    let g = ia;
                    let g = g as usize;
                    set!(2 * g, pos as u32);
                    set!(2 * g + 1, regs[self.tmp_base + g]);
                    pc += 1;
                }
                _ => {
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
fn char_at<U: Unit>(input: &[U], pos: usize) -> Option<(u32, usize)> {
    if pos >= input.len() || inside_pair(input, pos) {
        return None;
    }
    Some(code_point(input, pos))
}

/// Is `pos` between the lead and the trail of a surrogate pair?
fn inside_pair<U: Unit>(input: &[U], pos: usize) -> bool {
    pos > 0 && pos < input.len() && is_lead(input[pos - 1].u()) && is_trail(input[pos].u())
}

/// The code point at `i` (`i < input.len()`) and its length in units: a lead and a trail surrogate are one code point
/// (ECMA-262 `CodePointAt`). A lone surrogate is itself.
fn code_point<U: Unit>(input: &[U], i: usize) -> (u32, usize) {
    let a = input[i].u();
    match input.get(i + 1).map(|b| b.u()) {
        Some(b) if is_lead(a) && is_trail(b) => (0x10000 + ((a as u32 - 0xD800) << 10) + (b as u32 - 0xDC00), 2),
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
    NamedRef(Vec<u32>),
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
    NamedRef(Vec<u32>),
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
/// (and ZWNJ, ZWJ). It also accepts Unicode escapes.
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
                    v = hex4(p.get(i..i + 4)?)?;
                    i += 4;
                    if (0xD800..=0xDBFF).contains(&v) && p.get(i..i + 2) == Some(&[0x5C, 0x75]) {
                        let lo = hex4(p.get(i + 2..i + 6)?)?;
                        if !(0xDC00..=0xDFFF).contains(&lo) {
                            return None;
                        }
                        v = 0x10000 + ((v - 0xD800) << 10) + (lo - 0xDC00);
                        i += 6;
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
        let ok = if ch.is_ascii() {
            ch == '$' || ch == '_' || ch.is_ascii_alphabetic() || (!name.is_empty() && ch.is_ascii_digit())
        } else {
            crate::lex::id_char(cp, name.is_empty())
        };
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

fn hex4(units: &[u16]) -> Option<u32> {
    units.iter().try_fold(0, |v, &u| Some(v * 16 + hex(u)?))
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
        if self.names.len() > 1 {
            self.check_duplicate_names()?;
        }
        Ok(n)
    }

    /// Duplicate names must occur on separate paths through a disjunction.
    #[cold]
    fn check_duplicate_names(&self) -> Result<(), CompileError> {
        if self.names.iter().enumerate().any(|(i, (name, _))| self.names[..i].iter().any(|(n, _)| n == name)) {
            let flags = if self.flags.unicode_sets { "v" } else if self.flags.unicode { "u" } else { "" };
            crate::regexp_check::check(&String::from_utf16_lossy(self.p), flags)
                .map_err(|msg| CompileError::Syntax { msg, at: 0 })?;
        }
        Ok(())
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
                Esc::NamedRef(groups) => Node::NamedRef(groups),
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
        let Some((_, end)) = group_name(self.p, self.i + 1) else { return syn("Invalid capture group name", at) };
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
        let mut groups = self.names.iter().filter(|(x, _)| *x == name).map(|&(_, n)| n);
        let Some(n) = groups.next() else { return syn("Invalid named capture referenced", at) };
        self.i = end;
        let Some(second) = groups.next() else { return Ok(Esc::BackRef(n)) };
        let closed = [n, second].into_iter().chain(groups).filter(|n| !self.open.contains(n)).collect();
        Ok(Esc::NamedRef(closed))
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
                if op != 0 && (want == 0 || op != want) {
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

impl Inst {
    /// The word of the instruction: `op | a << 8 | b << 36`. The operands are below 2^28 (the program has at most [`MAX_PROGRAM`] instructions).
    fn enc(self) -> u64 {
        let (op, a, b): (u8, u32, u32) = match self {
            Inst::Char(c) => (0, c as u32, 0),
            Inst::CharI(c) => (1, c as u32, 0),
            Inst::Any => (2, 0, 0),
            Inst::AnyAll => (3, 0, 0),
            Inst::Class(i) => (4, i, 0),
            Inst::AnyU => (5, 0, 0),
            Inst::AnyAllU => (6, 0, 0),
            Inst::ClassU(i) => (7, i, 0),
            Inst::Start => (8, 0, 0),
            Inst::End => (9, 0, 0),
            Inst::MStart => (10, 0, 0),
            Inst::MEnd => (11, 0, 0),
            Inst::WordB(w) => (12, w as u32, 0),
            Inst::BackRef(g) => (13, g, 0),
            Inst::NotInPair => (14, 0, 0),
            Inst::Split(a, b) => (15, a, b),
            Inst::Jmp(t) => (16, t, 0),
            Inst::Mark(r) => (17, r, 0),
            Inst::GroupStart(g) => (18, g, 0),
            Inst::EmptyCheck(r) => (19, r, 0),
            Inst::Close(g) => (20, g, 0),
            Inst::Reset(lo, hi) => (21, lo, hi),
            Inst::Look(n, e) => (22, n as u32, e),
            Inst::LookEnd => (23, 0, 0),
            Inst::Backward => (24, 0, 0),
            Inst::ReverseClose(g) => (25, g, 0),
            Inst::Match => (26, 0, 0),
        };
        debug_assert!(a < 1 << 28 && b < 1 << 28);
        op as u64 | (a as u64) << 8 | (b as u64) << 36
    }

    #[inline(always)]
    fn dec(w: u64) -> Inst {
        let (a, b) = ((w >> 8) as u32 & 0x0FFF_FFFF, (w >> 36) as u32);
        match w as u8 {
            0 => Inst::Char(a as u16),
            1 => Inst::CharI(a as u16),
            2 => Inst::Any,
            3 => Inst::AnyAll,
            4 => Inst::Class(a),
            5 => Inst::AnyU,
            6 => Inst::AnyAllU,
            7 => Inst::ClassU(a),
            8 => Inst::Start,
            9 => Inst::End,
            10 => Inst::MStart,
            11 => Inst::MEnd,
            12 => Inst::WordB(a != 0),
            13 => Inst::BackRef(a),
            14 => Inst::NotInPair,
            15 => Inst::Split(a, b),
            16 => Inst::Jmp(a),
            17 => Inst::Mark(a),
            18 => Inst::GroupStart(a),
            19 => Inst::EmptyCheck(a),
            20 => Inst::Close(a),
            21 => Inst::Reset(a, b),
            22 => Inst::Look(a != 0, b),
            23 => Inst::LookEnd,
            24 => Inst::Backward,
            25 => Inst::ReverseClose(a),
            _ => Inst::Match,
        }
    }
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
            Node::Empty | Node::Start | Node::End | Node::WordB(_) | Node::BackRef(_) | Node::NamedRef(_) | Node::Look(..) => true,
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
            Node::Char(_) | Node::Any | Node::Class(_) | Node::BackRef(_) | Node::NamedRef(_) | Node::WordB(_) => {
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
            Node::NamedRef(groups) => {
                // At most one alternative captures this name. Each unset reference consumes nothing.
                for &g in groups {
                    self.push(Inst::BackRef(g))?;
                }
                if self.flags.unicode && !groups.is_empty() {
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
#[path = "regexp_tests.rs"]
mod tests;
