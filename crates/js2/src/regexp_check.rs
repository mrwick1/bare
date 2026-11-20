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
    let (u, v) = (flags.contains('u'), flags.contains('v'));
    if u && v {
        return Err("the regular expression flags 'u' and 'v' cannot be used together");
    }
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
            b'\\' => {
                self.i += 1;
                if self.atom_escape()? {
                    self.quantifier()?;
                }
            }
            _ => {
                self.next();
                self.quantifier()?;
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
        let (mut name, mut value, mut eq) = (0, 0, false);
        loop {
            match self.byte(self.i) {
                Some(b'}') => break,
                Some(b'=') if !eq => eq = true,
                Some(c) if c.is_ascii_alphanumeric() || c == b'_' => *if eq { &mut value } else { &mut name } += 1,
                _ => return bad,
            }
            self.i += 1;
        }
        self.i += 1;
        if name == 0 || (eq && value == 0) { bad } else { Ok(()) }
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
            if self.trail.is_none() && self.eat(b']') {
                return Ok(());
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
