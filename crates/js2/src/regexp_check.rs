//! Early errors of a regular expression literal: ECMA-262 22.2.1 with the web-compatibility grammar of Annex B.1.2 (the grammar of
//! `u` off). [`check`] reads the pattern once and builds nothing; a pattern with no named group allocates nothing.
//!
//! Not checked (accepted without a look): the names in `\p{...}` and `\P{...}` (the syntax is checked, not the property or its value),
//! and the inside of a class when the `v` flag is on (only the brackets are matched).

use crate::lex::id_char;

type R<T> = Result<T, &'static str>;

/// One level of the path from the pattern to a named group: the disjunction and the number of its alternative. Two groups with the same
/// name are an error unless they are in different alternatives of one disjunction.
struct Path<'a> {
    parent: Option<&'a Path<'a>>,
    disj: u32,
    alt: u32,
}

impl Path<'_> {
    fn list(&self) -> Vec<(u32, u32)> {
        let mut v = vec![(self.disj, self.alt)];
        let mut p = self.parent;
        while let Some(x) = p {
            v.push((x.disj, x.alt));
            p = x.parent;
        }
        v
    }
}

/// The value of a class atom (for the order of a range), or a class escape (`\d`, `\p{..}`).
#[derive(Clone, Copy)]
enum Atom {
    Char(u32),
    Set,
}

struct Checker<'a> {
    s: &'a str,
    i: usize,
    /// The `u` or the `v` flag.
    uni: bool,
    vsets: bool,
    depth: u32,
    disjunctions: u32,
    groups: u32,
    max_backref: u32,
    /// Some group has a name (Annex B: only then is `\k` an escape of its own).
    named: bool,
    /// A `\k` that is not followed by a name.
    bare_k: bool,
    names: Vec<(String, Vec<(u32, u32)>)>,
    refs: Vec<String>,
    /// The second unit of an astral character in a class when `u` is off (the pattern is read in UTF-16 units there).
    trail: Option<u32>,
}

/// Check `pattern` (the text between the slashes) with `flags` (the text after them, already valid: no repeat, only `dgimsuvy`).
pub fn check(pattern: &str, flags: &str) -> R<()> {
    let (u, v) = (flags.bytes().any(|c| c == b'u'), flags.bytes().any(|c| c == b'v'));
    if u && v {
        return Err("the regular expression flags 'u' and 'v' cannot be used together");
    }
    if !(u || v) && simple(pattern.as_bytes()) {
        return Ok(());
    }
    full(pattern, u, v)
}

/// The full check, with no fast path.
fn full(pattern: &str, u: bool, v: bool) -> R<()> {
    let mut c = Checker {
        s: pattern, i: 0, uni: u || v, vsets: v, depth: 0, disjunctions: 0, groups: 0, max_backref: 0, named: false, bare_k: false,
        names: Vec::new(), refs: Vec::new(), trail: None,
    };
    c.disjunction(None)?;
    if c.i < c.s.len() {
        return Err("unmatched ')' in a regular expression");
    }
    if c.uni && c.max_backref > c.groups {
        return Err("invalid backreference in a regular expression");
    }
    if c.named || c.uni {
        if c.bare_k {
            return Err("\\k must be followed by a group name");
        }
        for r in &c.refs {
            if !c.names.iter().any(|(n, _)| n == r) {
                return Err("\\k names a group that does not exist");
            }
        }
    }
    Ok(())
}

/// Byte classes for the fast paths (bit 1: a literal outside a class; bit 2: a literal in a class; bit 4: `\\X` is a valid escape of one
/// character in every mode, outside a class and in it; bit 8: the same, in a class only). A literal is ASCII and has no meaning of its own:
/// reading it changes no state of the checker, and the only thing that can follow it is a quantifier.
static CLASS: [u8; 256] = {
    let mut t = [0u8; 256];
    let mut i = 0;
    while i < 128 {
        let c = i as u8;
        if !matches!(c, b'^' | b'$' | b'*' | b'+' | b'?' | b'{' | b'}' | b'[' | b']' | b'(' | b')' | b'\\' | b'|') {
            t[i] |= 1;
        }
        if !matches!(c, b'\\' | b']' | b'-') {
            t[i] |= 2;
        }
        i += 1;
    }
    let both = b"dDsSwWfnrtv^$\\.*+?()[]{}|/";
    let mut k = 0;
    while k < both.len() {
        t[both[k] as usize] |= 4;
        k += 1;
    }
    t[b'b' as usize] |= 8;
    t[b'-' as usize] |= 8;
    t
};

/// `n` hex digits at `i` (`\\xHH` and `\\uHHHH`): their value.
fn hex_at(b: &[u8], i: usize, n: usize) -> Option<u32> {
    b.get(i..i + n)?.iter().try_fold(0, |v, &d| Some(v * 16 + hex(d)?))
}

/// The fast path for the common pattern with `u` and `v` off: one pass with a table and three states, no recursion and no stack. It answers
/// `true` only when the pattern is certainly valid (the same answer as the full check) and `false` when it is not sure: for an error or for a
/// feature it does not read (named groups, modifiers, lookbehind, `\k`, `\c`, `\p`, a `{` that is not a complete quantifier, a class escape
/// that starts or ends a range). Then the full check runs from the start. Without `u` a backreference is never an error (a number above the
/// number of groups is an octal escape), so `\1` needs no count of groups.
fn simple(b: &[u8]) -> bool {
    // What the last item allows: nothing to repeat (the start, `(`, `|`, `^`, `$`, `\b`, a lazy quantifier), an atom, a quantifier (only `?`
    // may follow, once).
    const NONE: u8 = 0;
    const ATOM: u8 = 1;
    const QUANT: u8 = 2;
    let (mut i, mut depth, mut st) = (0, 0u32, NONE);
    while let Some(&c) = b.get(i) {
        i += 1;
        if CLASS[c as usize] & 1 != 0 || c >= 0x80 {
            st = ATOM;
            continue;
        }
        match c {
            b'*' | b'+' | b'?' => {
                st = match (st, c) {
                    (ATOM, _) => QUANT,
                    (QUANT, b'?') => NONE,
                    _ => return false,
                }
            }
            b'{' if st == ATOM => {
                let num = |i: &mut usize| {
                    let s = *i;
                    let mut n = 0u64;
                    while let Some(d) = b.get(*i).filter(|d| d.is_ascii_digit()) {
                        n = n * 10 + (d - b'0') as u64;
                        *i += 1;
                    }
                    (*i > s && *i - s < 19).then_some(n)
                };
                let Some(lo) = num(&mut i) else { return false };
                let mut hi = lo;
                if b.get(i) == Some(&b',') {
                    i += 1;
                    hi = if b.get(i) == Some(&b'}') { u64::MAX } else if let Some(h) = num(&mut i) { h } else { return false };
                }
                if b.get(i) != Some(&b'}') || lo > hi {
                    return false;
                }
                (i, st) = (i + 1, QUANT);
            }
            b'^' | b'$' | b'|' => st = NONE,
            b'(' => {
                if b.get(i) == Some(&b'?') {
                    if !matches!(b.get(i + 1), Some(b':' | b'=' | b'!')) {
                        return false;
                    }
                    i += 2;
                }
                (depth, st) = (depth + 1, NONE);
            }
            b')' => {
                if depth == 0 {
                    return false;
                }
                (depth, st) = (depth - 1, ATOM);
            }
            b'[' => {
                if !simple_class(b, &mut i) {
                    return false;
                }
                st = ATOM;
            }
            b'\\' => match b.get(i) {
                Some(&e) if CLASS[e as usize] & 4 != 0 || e.is_ascii_digit() => (i, st) = (i + 1, ATOM),
                Some(b'b' | b'B') => (i, st) = (i + 1, NONE),
                Some(b'x') if hex_at(b, i + 1, 2).is_some() => (i, st) = (i + 3, ATOM),
                Some(b'u') if hex_at(b, i + 1, 4).is_some() => (i, st) = (i + 5, ATOM),
                _ => return false,
            },
            _ => return false,
        }
    }
    depth == 0
}

/// After `[`: a class of literals, escapes and ranges of two characters. False when unsure.
fn simple_class(b: &[u8], i: &mut usize) -> bool {
    // The value of a character; `Some(None)` for a class escape (`\d`, or `\1`), which cannot start or end a range.
    let atom = |i: &mut usize| -> Option<Option<u32>> {
        let c = *b.get(*i)?;
        *i += 1;
        if c != b'\\' {
            return (CLASS[c as usize] & 2 != 0 || c == b'-').then_some(Some(c as u32));
        }
        let e = *b.get(*i)?;
        *i += 1;
        Some(match e {
            b'd' | b'D' | b's' | b'S' | b'w' | b'W' | b'0'..=b'9' => None,
            b'f' => Some(12),
            b'n' => Some(10),
            b'r' => Some(13),
            b't' => Some(9),
            b'v' => Some(11),
            b'b' => Some(8),
            b'x' => {
                let v = hex_at(b, *i, 2)?;
                *i += 2;
                Some(v)
            }
            b'u' => {
                let v = hex_at(b, *i, 4)?;
                *i += 4;
                Some(v)
            }
            _ if CLASS[e as usize] & 4 != 0 || e == b'-' => Some(e as u32),
            _ => return None,
        })
    };
    if b.get(*i) == Some(&b'^') {
        *i += 1;
    }
    loop {
        if b.get(*i) == Some(&b']') {
            *i += 1;
            return true;
        }
        let Some(a) = atom(i) else { return false };
        if b.get(*i) == Some(&b'-') && b.get(*i + 1).is_some_and(|&n| n != b']') {
            *i += 1;
            match (a, atom(i)) {
                (Some(x), Some(Some(y))) if x <= y => {}
                _ => return false,
            }
        }
    }
}

fn hex(b: u8) -> Option<u32> {
    (b as char).to_digit(16)
}

impl Checker<'_> {
    #[inline]
    fn byte(&self, at: usize) -> Option<u8> {
        self.s.as_bytes().get(at).copied()
    }

    #[inline]
    fn eat(&mut self, c: u8) -> bool {
        let r = self.byte(self.i) == Some(c);
        self.i += r as usize;
        r
    }

    #[inline]
    fn next(&mut self) -> Option<char> {
        let b = self.byte(self.i)?;
        if b < 0x80 {
            self.i += 1;
            return Some(b as char);
        }
        let c = self.s[self.i..].chars().next()?;
        self.i += c.len_utf8();
        Some(c)
    }

    fn disjunction(&mut self, parent: Option<&Path>) -> R<()> {
        self.depth += 1;
        if self.depth > 250 {
            return Err("a regular expression is nested too deeply");
        }
        let (disj, mut alt) = (self.disjunctions, 0);
        self.disjunctions += 1;
        loop {
            let here = Path { parent, disj, alt };
            while !matches!(self.byte(self.i), None | Some(b'|' | b')')) {
                self.term(&here)?;
            }
            if !self.eat(b'|') {
                break;
            }
            alt += 1;
        }
        self.depth -= 1;
        Ok(())
    }

    /// `{n}`, `{n,}` or `{n,m}` at `at`: the position after it and the two numbers (saturated).
    fn braces(&self, at: usize) -> Option<(usize, u64, Option<u64>)> {
        let b = self.s.as_bytes();
        let num = |mut i: usize| {
            let s = i;
            let mut n = 0u64;
            while let Some(d) = b.get(i).filter(|d| d.is_ascii_digit()) {
                n = n.saturating_mul(10).saturating_add((d - b'0') as u64);
                i += 1;
            }
            (i > s).then_some((i, n))
        };
        if b.get(at) != Some(&b'{') {
            return None;
        }
        let (mut i, lo) = num(at + 1)?;
        let mut hi = Some(lo);
        if b.get(i) == Some(&b',') {
            match num(i + 1) {
                Some((j, n)) => (i, hi) = (j, Some(n)),
                None => (i, hi) = (i + 1, None),
            }
        }
        (b.get(i) == Some(&b'}')).then_some((i + 1, lo, hi))
    }

    /// A quantifier after an atom, if there is one.
    #[inline]
    fn quantifier(&mut self) -> R<()> {
        match self.byte(self.i) {
            Some(b'*' | b'+' | b'?' | b'{') => self.quantifier_at(),
            _ => Ok(()),
        }
    }

    fn quantifier_at(&mut self) -> R<()> {
        match self.byte(self.i) {
            Some(b'*' | b'+' | b'?') => self.i += 1,
            Some(b'{') => match self.braces(self.i) {
                Some((end, lo, hi)) => {
                    if hi.is_some_and(|h| lo > h) {
                        return Err("numbers out of order in a regular expression quantifier");
                    }
                    self.i = end;
                }
                None if self.uni => return Err("incomplete quantifier in a regular expression"),
                None => return Ok(()),
            },
            _ => return Ok(()),
        }
        self.eat(b'?');
        Ok(())
    }

    fn term(&mut self, path: &Path) -> R<()> {
        let c = self.byte(self.i).unwrap();
        match c {
            b'^' | b'$' => self.i += 1,
            b'*' | b'+' | b'?' => return Err("nothing to repeat in a regular expression"),
            b'{' if self.uni || self.braces(self.i).is_some() => return Err("nothing to repeat in a regular expression"),
            b'}' | b']' if self.uni => return Err("lone quantifier bracket in a regular expression"),
            b'(' => {
                self.i += 1;
                if self.group(path)? {
                    self.quantifier()?;
                }
            }
            b'[' => {
                self.i += 1;
                self.class()?;
                self.quantifier()?;
            }
            b'\\' if self.byte(self.i + 1).is_some_and(|e| CLASS[e as usize] & 4 != 0) => {
                self.i += 2;
                self.quantifier()?;
            }
            b'\\' => {
                self.i += 1;
                if self.atom_escape()? {
                    self.quantifier()?;
                }
            }
            _ => {
                // A run of literals: only the last one can take a quantifier, so it is left for the slow path.
                let b = self.s.as_bytes();
                let mut j = self.i;
                while b.get(j).is_some_and(|&c| CLASS[c as usize] & 1 != 0) {
                    j += 1;
                }
                if j > self.i && matches!(b.get(j), Some(b'*' | b'+' | b'?' | b'{')) {
                    j -= 1;
                }
                if j > self.i {
                    self.i = j;
                } else {
                    self.next();
                    self.quantifier()?;
                }
            }
        }
        Ok(())
    }

    /// After `(`. True when a quantifier may follow.
    fn group(&mut self, path: &Path) -> R<bool> {
        let mut quantifiable = true;
        if self.eat(b'?') {
            match self.byte(self.i) {
                Some(b':') => self.i += 1,
                Some(b'=' | b'!') => {
                    self.i += 1;
                    quantifiable = !self.uni;
                }
                Some(b'<') if matches!(self.byte(self.i + 1), Some(b'=' | b'!')) => {
                    self.i += 2;
                    quantifiable = false;
                }
                Some(b'<') => {
                    self.i += 1;
                    let name = self.group_name()?;
                    let here = path.list();
                    for (n, p) in &self.names {
                        if *n == name && !p.iter().any(|a| here.iter().any(|b| a.0 == b.0 && a.1 != b.1)) {
                            return Err("duplicate group name in a regular expression");
                        }
                    }
                    self.names.push((name, here));
                    self.named = true;
                    self.groups += 1;
                }
                _ => self.modifiers()?,
            }
        } else {
            self.groups += 1;
        }
        self.disjunction(Some(path))?;
        if !self.eat(b')') {
            return Err("unterminated group in a regular expression");
        }
        Ok(quantifiable)
    }

    /// `(?ims-ims:` after `(?`.
    fn modifiers(&mut self) -> R<()> {
        let mut sets = [0u8; 2];
        let mut dash = false;
        let mut k = 0;
        loop {
            match self.byte(self.i) {
                Some(c @ (b'i' | b'm' | b's')) => {
                    let bit = match c { b'i' => 1, b'm' => 2, _ => 4 };
                    if sets[k] & bit != 0 {
                        return Err("duplicate modifier in a regular expression group");
                    }
                    sets[k] |= bit;
                }
                Some(b'-') if !dash => (dash, k) = (true, 1),
                Some(b':') => break,
                _ => return Err("invalid group in a regular expression"),
            }
            self.i += 1;
        }
        self.i += 1;
        if sets[0] & sets[1] != 0 || (dash && sets[0] | sets[1] == 0) {
            return Err("invalid modifiers in a regular expression group");
        }
        Ok(())
    }

    /// `name>` after `(?<` or `\k<`. Escapes are read; the result is the name as the engine sees it.
    fn group_name(&mut self) -> R<String> {
        let mut name = String::new();
        loop {
            let cp = match self.next() {
                None => return Err("unterminated group name in a regular expression"),
                Some('>') if !name.is_empty() => return Ok(name),
                Some('\\') => {
                    if !self.eat(b'u') {
                        return Err("invalid group name in a regular expression");
                    }
                    self.unicode_escape(true).ok_or("invalid group name in a regular expression")?
                }
                Some(c) => c as u32,
            };
            let ok = if cp < 0x80 {
                let c = cp as u8;
                c == b'$' || c == b'_' || c.is_ascii_alphabetic() || (!name.is_empty() && c.is_ascii_digit())
            } else {
                id_char(cp, name.is_empty())
            };
            match char::from_u32(cp).filter(|_| ok) {
                Some(c) => name.push(c),
                None => return Err("invalid group name in a regular expression"),
            }
        }
    }

    /// After `\u`: `XXXX`, `{X...}` (with `uni`) or a surrogate pair `\uXXXX\uXXXX` (with `uni`). Takes nothing when it fails.
    fn unicode_escape(&mut self, uni: bool) -> Option<u32> {
        let at = self.i;
        let r = self.unicode_escape_at(uni);
        if r.is_none() {
            self.i = at;
        }
        r
    }

    fn unicode_escape_at(&mut self, uni: bool) -> Option<u32> {
        let four = |s: &Self, at: usize| -> Option<u32> {
            let mut v = 0;
            for k in 0..4 {
                v = v * 16 + hex(s.byte(at + k)?)?;
            }
            Some(v)
        };
        if uni && self.byte(self.i) == Some(b'{') {
            let (mut v, mut n) = (0u32, 0);
            self.i += 1;
            while let Some(d) = self.byte(self.i).and_then(hex) {
                v = v.checked_mul(16)?.checked_add(d)?;
                if v > 0x10FFFF {
                    return None;
                }
                (n, self.i) = (n + 1, self.i + 1);
            }
            return (n > 0 && self.eat(b'}')).then_some(v);
        }
        let v = four(self, self.i)?;
        self.i += 4;
        if uni
            && (0xD800..0xDC00).contains(&v)
            && self.byte(self.i) == Some(b'\\')
            && self.byte(self.i + 1) == Some(b'u')
            && let Some(t) = four(self, self.i + 2).filter(|t| (0xDC00..0xE000).contains(t))
        {
            self.i += 6;
            return Some(0x10000 + ((v - 0xD800) << 10) + (t - 0xDC00));
        }
        Some(v)
    }

    /// After `\` outside a class. True when a quantifier may follow.
    fn atom_escape(&mut self) -> R<bool> {
        match self.byte(self.i) {
            Some(b'b' | b'B') => {
                self.i += 1;
                Ok(false)
            }
            Some(c @ b'1'..=b'9') => {
                let mut n = (c - b'0') as u32;
                self.i += 1;
                while let Some(d) = self.byte(self.i).filter(u8::is_ascii_digit) {
                    n = n.saturating_mul(10).saturating_add((d - b'0') as u32);
                    self.i += 1;
                }
                self.max_backref = self.max_backref.max(n);
                Ok(true)
            }
            Some(b'k') => {
                self.i += 1;
                self.k_escape()?;
                Ok(true)
            }
            _ => {
                self.char_escape(false)?;
                Ok(true)
            }
        }
    }

    /// After `\k`. Without a group name the escape is an error when the pattern has a named group (even one after it) or `u` is on, so
    /// it is checked at the end. Without `u` and without a named group `\k` is the letter `k`.
    fn k_escape(&mut self) -> R<()> {
        if self.eat(b'<') {
            let at = self.i;
            match self.group_name() {
                Ok(n) => {
                    self.refs.push(n);
                    return Ok(());
                }
                Err(_) => self.i = at - 1,
            }
        }
        self.bare_k = true;
        Ok(())
    }

    /// After `\`, the escapes that are the same in a class and outside it. Gives the value of the character, or `Atom::Set` for a class escape.
    fn char_escape(&mut self, class: bool) -> R<Atom> {
        let Some(c) = self.next() else { return Err("a regular expression ends with a backslash") };
        let ch = |v: u32| Ok(Atom::Char(v));
        match c {
            'd' | 'D' | 's' | 'S' | 'w' | 'W' => Ok(Atom::Set),
            'p' | 'P' if self.uni => {
                self.property()?;
                Ok(Atom::Set)
            }
            'f' => ch(12),
            'n' => ch(10),
            'r' => ch(13),
            't' => ch(9),
            'v' => ch(11),
            'b' if class => ch(8),
            '-' if class && self.uni => ch(45),
            'c' => match self.byte(self.i) {
                Some(l) if l.is_ascii_alphabetic() || (class && !self.uni && (l.is_ascii_digit() || l == b'_')) => {
                    self.i += 1;
                    ch((l % 32) as u32)
                }
                _ if self.uni => Err("invalid \\c escape in a regular expression"),
                _ => {
                    self.i -= 1; // The backslash is a character and the `c` is the next one.
                    ch(92)
                }
            },
            '0' if !self.byte(self.i).is_some_and(|d| d.is_ascii_digit()) => ch(0),
            '0'..='7' if !self.uni => {
                let mut v = c as u32 - 48;
                while let Some(d) = self.byte(self.i).filter(|d| (b'0'..=b'7').contains(d)).filter(|_| v * 8 < 256) {
                    v = v * 8 + (d - b'0') as u32;
                    self.i += 1;
                }
                ch(v)
            }
            '1'..='9' if class && !self.uni => ch(c as u32),
            'x' => {
                match (self.byte(self.i).and_then(hex), self.byte(self.i + 1).and_then(hex)) {
                    (Some(a), Some(b)) => {
                        self.i += 2;
                        ch(a * 16 + b)
                    }
                    _ if self.uni => Err("invalid \\x escape in a regular expression"),
                    _ => ch('x' as u32),
                }
            }
            'u' => match self.unicode_escape(self.uni) {
                Some(v) => ch(v),
                None if self.uni => Err("invalid \\u escape in a regular expression"),
                None => ch('u' as u32),
            },
            'k' if !self.uni => {
                // Only a class gets here: `\k` is the letter, and an error when the pattern has a named group (checked at the end).
                self.bare_k = true;
                ch('k' as u32)
            }
            _ if self.uni => {
                if "^$\\.*+?()[]{}|/".contains(c) {
                    ch(c as u32)
                } else {
                    Err("invalid escape in a regular expression")
                }
            }
            _ => ch(c as u32),
        }
    }

    /// After `\p` or `\P` (with `u` or `v`): `{Name}` or `{Name=Value}`.
    fn property(&mut self) -> R<()> {
        let bad = Err("invalid property name in a regular expression");
        if !self.eat(b'{') {
            return bad;
        }
        let start = self.i;
        let mut eq = None;
        loop {
            match self.byte(self.i) {
                Some(b'}') => break,
                Some(b'=') if eq.is_none() => eq = Some(self.i),
                Some(c) if c.is_ascii_alphanumeric() || c == b'_' => {}
                _ => return bad,
            }
            self.i += 1;
        }
        let text: String = (start..self.i).filter_map(|i| self.byte(i)).map(char::from).collect();
        self.i += 1;
        let (name, value) = match eq {
            Some(e) => (&text[..e - start], Some(&text[e - start + 1..])),
            None => (text.as_str(), None),
        };
        // The names are those of ECMA-262; a property of strings (`RGI_Emoji`) is not known here, as in the matcher.
        if name.is_empty() || value == Some("") || bare_unicode::property::property_ranges(name, value).is_none() { bad } else { Ok(()) }
    }

    /// After `[`.
    fn class(&mut self) -> R<()> {
        if self.vsets {
            // Only the brackets: `[a[b]]`, `\]` and `\[` do not close a class.
            let mut depth = 1;
            while depth > 0 {
                match self.next() {
                    None => return Err("unterminated character class in a regular expression"),
                    Some('\\') => {
                        self.next();
                    }
                    Some('[') => depth += 1,
                    Some(']') => depth -= 1,
                    _ => {}
                }
            }
            return Ok(());
        }
        self.eat(b'^');
        loop {
            if self.trail.is_none() {
                if self.eat(b']') {
                    return Ok(());
                }
                // A run of literals and simple escapes that start no range. The atom before a `-` is left for the slow path.
                let b = self.s.as_bytes();
                let (mut j, mut last) = (self.i, self.i);
                loop {
                    match b.get(j) {
                        // `a-z`: a range of two literals in order.
                        Some(&c) if CLASS[c as usize] & 2 != 0 && b.get(j + 1) == Some(&b'-')
                            && b.get(j + 2).is_some_and(|&d| CLASS[d as usize] & 2 != 0 && d >= c) => (last, j) = (j + 3, j + 3),
                        Some(&c) if CLASS[c as usize] & 2 != 0 => (last, j) = (j, j + 1),
                        Some(b'\\') if b.get(j + 1).is_some_and(|&e| CLASS[e as usize] & 12 != 0) => (last, j) = (j, j + 2),
                        _ => break,
                    }
                }
                if b.get(j) == Some(&b'-') {
                    j = last;
                }
                if j > self.i {
                    self.i = j;
                    continue;
                }
            }
            let a = self.class_atom()?;
            if self.trail.is_none() && self.byte(self.i) == Some(b'-') && self.byte(self.i + 1) != Some(b']') {
                if self.byte(self.i + 1).is_none() {
                    return Err("unterminated character class in a regular expression");
                }
                self.i += 1;
                match (a, self.class_atom()?) {
                    (Atom::Char(x), Atom::Char(y)) if x > y => return Err("range out of order in a character class"),
                    (Atom::Char(_), Atom::Char(_)) => {}
                    _ if self.uni => return Err("invalid character class range"),
                    _ => {}
                }
            }
        }
    }

    fn class_atom(&mut self) -> R<Atom> {
        if let Some(t) = self.trail.take() {
            return Ok(Atom::Char(t));
        }
        match self.next() {
            None => Err("unterminated character class in a regular expression"),
            Some('\\') => self.char_escape(true),
            Some(c) if !self.uni && c as u32 > 0xFFFF => {
                let v = c as u32 - 0x10000;
                self.trail = Some(0xDC00 + (v & 0x3FF));
                Ok(Atom::Char(0xD800 + (v >> 10)))
            }
            Some(c) => Ok(Atom::Char(c as u32)),
        }
    }
}

#[cfg(test)]
#[path = "regexp_check_tests.rs"]
mod tests;
