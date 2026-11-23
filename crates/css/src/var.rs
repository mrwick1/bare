//! `var()` and custom properties: `--gap: 8px; margin: var(--gap)`.
//!
//! A custom property is a name that starts with `--` and a piece of text. It is inherited. The text is not read when we read the style sheet:
//! `--x: 1px 2px` can be a margin, or a color, or part of a `calc()`. It means something only where `var(--x)` puts it.
//!
//! What we do:
//! - A declaration with `var()` in the value is kept as text (`VarDecl`). For each element, we replace each `var()` with the text of the
//!   custom property, read the result like a normal value, and take what it gives for the property.
//! - A `var()` has an optional fallback: `var(--x, 10px)`. It is used when `--x` is not set.
//! - A custom property may use `var()` too. We replace them first, and keep the result. A circle (`--a: var(--b); --b: var(--a)`), or a
//!   `var()` with no value and no fallback, makes the property not set. A normal property in that case is `unset`.
//! - The set of custom properties of an element is kept once for all the elements that have the same one (`Sets`).
//!
//! `@property` gives an `initial-value` and `inherits: false` (the registry of `RuleSet`, read by `Sets::text`); its types are not checked. 
//! `env()` (`substitute_env`) is cut and pasted the same way, when the value would be dropped without it: `env(safe-area-inset-left, 0px)`.
//! A window has no notch and no on-screen keyboard, so the insets are `0px`. Another name takes the fallback, and with no fallback the
//! declaration is dropped (Chrome makes it `unset`).
//! The text is cut and pasted, not split in tokens, so `var(--a)px` with `--a: 1` gives `1px` here, and Chrome does not accept it.
//! JS: like a template string, where `${}` is replaced and the result is read again.

/// A declaration that has `var()` in its value: the name that was written (it may be a shorthand), and the value text.
#[derive(Debug, Clone, PartialEq)]
pub struct VarDecl {
    pub name: Box<str>,
    pub text: Box<str>,
}

/// A custom property that a rule sets: `--gap: 8px`.
#[derive(Debug, Clone, PartialEq)]
pub struct CustomDecl {
    pub name: Box<str>,
    pub text: Box<str>,
    pub important: bool,
}

/// The most that a value can have after the `var()` are replaced. A value that is longer is not good. This keeps
/// `--a: var(--b) var(--b); --b: var(--c) var(--c); ...` from using all the memory.
const MAX_LEN: usize = 64 * 1024;

/// `var()` inside a fallback inside a fallback ... goes this deep. Deeper is not good.
const MAX_DEPTH: usize = 32;

/// Is there a `var(` in the text? Not one that is part of a longer name (`--my-var(`), and not one in a string.
pub fn has_var(text: &str) -> bool {
    find_fn(text.as_bytes(), 0, b"var(").is_some()
}

/// Is there an `env(` in the text? The same rules as `has_var`.
pub fn has_env(text: &str) -> bool {
    find_fn(text.as_bytes(), 0, b"env(").is_some()
}

/// The place of the next `func` (`var(` or `env(`) from `from`, outside of strings and comments.
fn find_fn(b: &[u8], from: usize, func: &[u8; 4]) -> Option<usize> {
    let mut i = from;
    while i < b.len() {
        match b[i] {
            q @ (b'"' | b'\'') => i = skip_string(b, i, q),
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i = b[i + 2..].windows(2).position(|w| w == b"*/").map_or(b.len(), |p| i + 2 + p + 2);
            }
            c if c.eq_ignore_ascii_case(&func[0])
                && b.get(i..i + 4).is_some_and(|w| w.eq_ignore_ascii_case(func))
                && (i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'-' || b[i - 1] == b'_')) =>
            {
                return Some(i);
            }
            _ => i += 1,
        }
    }
    None
}

/// The place after the string that starts at `i`.
fn skip_string(b: &[u8], i: usize, quote: u8) -> usize {
    let mut j = i + 1;
    while j < b.len() {
        match b[j] {
            b'\\' => j += 2,
            c if c == quote => return j + 1,
            _ => j += 1,
        }
    }
    b.len()
}

/// The place of the `)` that closes a `(` that was just opened, from `from`. `None` when it is not closed.
fn close_paren(b: &[u8], from: usize) -> Option<usize> {
    let (mut depth, mut i) = (0usize, from);
    while i < b.len() {
        match b[i] {
            q @ (b'"' | b'\'') => {
                i = skip_string(b, i, q);
                continue;
            }
            b'(' => depth += 1,
            b')' if depth == 0 => return Some(i),
            b')' => depth -= 1,
            _ => {}
        }
        i += 1;
    }
    None
}

/// Replace each `var()` in `text`. `lookup` gives the text of a custom property, or `None` when it is not set.
/// `None` when a `var()` has no value and no fallback, when it is not written well, or when the result is too long.
pub fn substitute(text: &str, lookup: &mut dyn FnMut(&str) -> Option<String>) -> Option<String> {
    substitute_at(text, b"var(", lookup, 0)
}

/// Replace each `env()` in `text` with the value of the browser, or with its fallback. `None` like `substitute`.
pub fn substitute_env(text: &str) -> Option<String> {
    substitute_at(text, b"env(", &mut |name| env_value(name).map(str::to_string), 0)
}

/// The value of an `env()` name. The insets are 0: a window has no notch, no rounded screen and no on-screen keyboard.
fn env_value(name: &str) -> Option<&'static str> {
    let side = name.strip_prefix("safe-area-inset-").or_else(|| name.strip_prefix("safe-area-max-inset-"));
    let zero = side.is_some_and(|s| matches!(s, "top" | "right" | "bottom" | "left"))
        || name.strip_prefix("keyboard-inset-").is_some_and(|s| matches!(s, "top" | "right" | "bottom" | "left" | "width" | "height"));
    zero.then_some("0px")
}

fn substitute_at(text: &str, func: &[u8; 4], lookup: &mut dyn FnMut(&str) -> Option<String>, depth: usize) -> Option<String> {
    if depth >= MAX_DEPTH {
        return None;
    }
    let b = text.as_bytes();
    let mut out = String::new();
    let mut at = 0;
    while let Some(start) = find_fn(b, at, func) {
        out.push_str(&text[at..start]);
        let inner_start = start + 4;
        let end = close_paren(b, inner_start)?;
        let inner = &text[inner_start..end];
        // The name is up to the first comma that is not in a block. The rest is the fallback.
        let comma = {
            let ib = inner.as_bytes();
            let mut i = 0;
            let mut found = None;
            while i < ib.len() {
                match ib[i] {
                    b',' => {
                        found = Some(i);
                        break;
                    }
                    b'(' => i = close_paren(ib, i + 1).map_or(ib.len(), |p| p + 1),
                    _ => i += 1,
                }
            }
            found
        };
        let (name, fallback) = match comma {
            Some(c) => (inner[..c].trim(), Some(&inner[c + 1..])),
            None => (inner.trim(), None),
        };
        if if func == b"var(" { !name.starts_with("--") || name.len() == 2 } else { name.is_empty() } {
            return None;
        }
        let value = match lookup(name) {
            Some(v) => v,
            None => substitute_at(fallback?.trim(), func, lookup, depth + 1)?,
        };
        out.push_str(&value);
        if out.len() > MAX_LEN {
            return None;
        }
        at = end + 1;
    }
    out.push_str(&text[at..]);
    (out.len() <= MAX_LEN).then_some(out)
}

use std::collections::HashMap;

/// The fast hasher of the DOM: the names are short, and SipHash cost 4% of a cascade.
type Fnv = std::hash::BuildHasherDefault<bare_html::dom::Fnv>;

/// The custom properties of all the elements of one cascade.
///
/// GitHub sets 2,500 custom properties on the root, and every element inherits them. A list of names and texts for each set made the
/// cascade copy the 2,500 names and hash them again for each new set: 40% of the cascade. So each name and each text is kept once here,
/// with a number. A set is a list of text numbers, one for each name number, and 0 is "not set". JS: like `texts[set[nameNumber]]`.
/// A new set is a copy of plain numbers, and two sets are equal when their numbers are. Each set is kept once, and an element keeps
/// its number. Set 0 is the empty one.
pub struct Sets {
    names: Vec<Box<str>>,
    name_at: HashMap<Box<str>, u32, Fnv>,
    /// Text 0 is not used: 0 is "not set".
    texts: Vec<Box<str>>,
    text_at: HashMap<Box<str>, u32, Fnv>,
    /// A set has no 0 at the end. So a set made before a new name came is equal to the same set made after.
    sets: Vec<Box<[u32]>>,
    /// The places of the sets in `sets`, by the hash of their numbers (`hash`).
    set_at: HashMap<u64, Vec<u32>, Fnv>,
    /// The name numbers, sorted by name. Made on the first call of `pairs`, and dropped when a name is added.
    /// `OnceCell` holds a value that is made once, on first use. JS: like a lazy getter that keeps its result.
    sorted: std::cell::OnceCell<Vec<u32>>,
    /// `@property`: the text number of the initial value of a name number, 0 for none. Empty when the page registers nothing.
    initial: Vec<u32>,
    /// `@property` with `inherits: false`: the name numbers whose value the child does not take from the parent.
    not_inherited: Vec<u32>,
    /// The same, by name number: O(1) to ask.
    no_inherit: Vec<bool>,
    /// For each set after the first: does it hold a value of a name in `not_inherited`? An element whose parent set has one cannot share it.
    flagged: Vec<bool>,
}

impl Sets {
    /// What stays in place while the real sets are moved out for a while (`mem::replace`): it makes no allocation, and nothing may use it.
    pub fn moved_out() -> Sets {
        Sets { names: Vec::new(), name_at: HashMap::default(), texts: Vec::new(), text_at: HashMap::default(), sets: Vec::new(), set_at: HashMap::default(), sorted: Default::default(), initial: Vec::new(), not_inherited: Vec::new(), flagged: Vec::new(), no_inherit: Vec::new() }
    }
}

impl Default for Sets {
    fn default() -> Sets {
        let mut set_at = HashMap::default();
        set_at.insert(hash(&[]), vec![0]);
        Sets { names: Vec::new(), name_at: HashMap::default(), texts: vec!["".into()], text_at: HashMap::default(), sets: vec![Box::default()], set_at, sorted: Default::default(), initial: Vec::new(), not_inherited: Vec::new(), flagged: vec![false], no_inherit: Vec::new() }
    }
}

/// A hash of the numbers of a set, 8 bytes at a step. `Fnv` takes one byte at a step, and a set of GitHub has 10,000 bytes.
fn hash(set: &[u32]) -> u64 {
    let mut h = set.len() as u64;
    for two in set.chunks(2) {
        let word = u64::from(two[0]) | u64::from(two.get(1).copied().unwrap_or(0)) << 32;
        h = (h.rotate_left(5) ^ word).wrapping_mul(0x517c_c1b7_2722_0a95);
    }
    h
}

impl Sets {
    /// The number of a name. A new name gets the next number.
    pub fn name(&mut self, name: &str) -> u32 {
        if let Some(&n) = self.name_at.get(name) {
            return n;
        }
        let n = self.names.len() as u32;
        self.names.push(name.into());
        self.name_at.insert(name.into(), n);
        self.sorted.take();
        n
    }

    /// The text of the custom property `name` in the set `set`. `None` when it is not set.
    pub fn get(&self, set: u32, name: &str) -> Option<&str> {
        let &n = self.name_at.get(name)?;
        self.text(&self.sets[set as usize], n)
    }

    /// The text of a name in a set: the set's own, else the `initial-value` of its `@property`.
    fn text<'a>(&'a self, set: &[u32], name: u32) -> Option<&'a str> {
        let t = match set.get(name as usize) {
            Some(&t) if t != 0 => t,
            _ => self.initial.get(name as usize).copied().unwrap_or(0),
        };
        (t != 0).then(|| &*self.texts[t as usize])
    }

    /// What an element takes from the parent set for `name`: nothing for a registered property that is not inherited.
    fn inherited<'a>(&'a self, set: &[u32], name: u32) -> Option<&'a str> {
        if self.no_inherit.get(name as usize) == Some(&true) { self.text(&[], name) } else { self.text(set, name) }
    }

    /// Register `@property name`: `initial` is its `initial-value`. Done once for a new `Sets`, before any element. Types are not checked.
    pub fn register(&mut self, name: &str, initial: Option<&str>, inherits: bool) {
        let n = self.name(name) as usize;
        if let Some(text) = initial {
            let t = self.text_number(text.to_string());
            if self.initial.len() <= n {
                self.initial.resize(n + 1, 0);
            }
            self.initial[n] = t;
        }
        if !inherits {
            self.not_inherited.push(n as u32);
            if self.no_inherit.len() <= n {
                self.no_inherit.resize(n + 1, false);
            }
            self.no_inherit[n] = true;
        }
    }

    /// Does the set hold a value that a child must not take (`inherits: false`)? O(1).
    pub fn blocks_sharing(&self, set: u32) -> bool {
        self.flagged[set as usize]
    }

    /// The custom properties of the set `set`, sorted by name: `("--gap", "8px")`.
    pub fn pairs(&self, set: u32) -> Vec<(&str, &str)> {
        let sorted = self.sorted.get_or_init(|| {
            let mut all: Vec<u32> = (0..self.names.len() as u32).collect();
            all.sort_unstable_by(|&a, &b| self.names[a as usize].cmp(&self.names[b as usize]));
            all
        });
        let set = &self.sets[set as usize];
        sorted.iter().filter_map(|&n| Some((&*self.names[n as usize], self.text(set, n)?))).collect()
    }

    /// The set of an element: the set `parent`, with the custom properties that the element sets (`own`, the winners of the cascade:
    /// the number of a name, from `name`, and its text). Each one that has `var()` is replaced with the others of the new set. One that
    /// cannot be (a circle, a name with no value) is not in the set, and the one of the parent goes too. Gives the number of the set.
    pub fn merge(&mut self, parent: u32, own: &[(u32, &str)]) -> u32 {
        #[derive(Clone)]
        enum Slot<'a> {
            Raw(&'a str),
            Visiting,
            Done(Option<String>),
        }
        type At = HashMap<u32, usize, Fnv>;
        let mut slots: Vec<(u32, Slot)> = own.iter().map(|&(n, t)| (n, Slot::Raw(t))).collect();
        // Where each name is in `slots`: the first one, if a name is there twice. A scan of `slots` for each lookup is slow for 20,000 properties.
        let mut at = At::with_capacity_and_hasher(slots.len(), Default::default());
        for (i, (n, _)) in slots.iter().enumerate() {
            at.entry(*n).or_insert(i);
        }

        fn get(n: u32, slots: &mut Vec<(u32, Slot)>, at: &At, parent: &[u32], sets: &Sets, depth: usize) -> Option<String> {
            let Some(&i) = at.get(&n) else {
                return sets.inherited(parent, n).map(str::to_string);
            };
            match slots[i].1.clone() {
                Slot::Done(v) => v,
                // A circle.
                Slot::Visiting => None,
                // The words that every property takes: `initial` is not set, and the others are the value of the parent.
                Slot::Raw(text) if text.eq_ignore_ascii_case("initial") => None,
                Slot::Raw(text) if text.eq_ignore_ascii_case("inherit") || text.eq_ignore_ascii_case("unset") => {
                    let v = if text.eq_ignore_ascii_case("inherit") { sets.text(parent, n) } else { sets.inherited(parent, n) }.map(str::to_string);
                    slots[i].1 = Slot::Done(v.clone());
                    v
                }
                Slot::Raw(text) => {
                    if depth >= MAX_DEPTH {
                        return None;
                    }
                    slots[i].1 = Slot::Visiting;
                    // A name that has no number is in no set.
                    let value = if has_var(text) { substitute(text, &mut |name| get(*sets.name_at.get(name)?, slots, at, parent, sets, depth + 1)) } else { Some(text.to_string()) };
                    slots[i].1 = Slot::Done(value.clone());
                    value
                }
            }
        }

        let parent_set = &self.sets[parent as usize];
        for i in 0..slots.len() {
            get(slots[i].0, &mut slots, &at, parent_set, self, 0);
        }
        // Copy the numbers of the parent, then write the ones of the element.
        let len = own.iter().map(|&(n, _)| n as usize + 1).max().unwrap_or(0).max(parent_set.len());
        let mut set = parent_set.to_vec();
        set.resize(len, 0);
        for &n in &self.not_inherited {
            if let Some(t) = set.get_mut(n as usize) {
                *t = 0;
            }
        }
        for (n, slot) in slots {
            set[n as usize] = match slot {
                Slot::Done(Some(v)) => self.text_number(v),
                _ => 0,
            };
        }
        while set.last() == Some(&0) {
            set.pop();
        }
        // Most often the element sets what the parent has, as Tailwind's `*` rule does.
        if *set == *self.sets[parent as usize] {
            return parent;
        }
        let h = hash(&set);
        if let Some(found) = self.set_at.get(&h)
            && let Some(&i) = found.iter().find(|&&i| *self.sets[i as usize] == *set)
        {
            return i;
        }
        let i = self.sets.len() as u32;
        self.flagged.push(self.not_inherited.iter().any(|&n| set.get(n as usize).is_some_and(|&t| t != 0)));
        self.sets.push(set.into_boxed_slice());
        self.set_at.entry(h).or_default().push(i);
        i
    }

    /// The number of a text. A new text gets the next number.
    fn text_number(&mut self, text: String) -> u32 {
        if let Some(&t) = self.text_at.get(text.as_str()) {
            return t;
        }
        let t = self.texts.len() as u32;
        self.text_at.insert(text.as_str().into(), t);
        self.texts.push(text.into_boxed_str());
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sub(text: &str, vars: &[(&str, &str)]) -> Option<String> {
        substitute(text, &mut |n| vars.iter().find(|(m, _)| *m == n).map(|(_, v)| v.to_string()))
    }

    #[test]
    fn finds_var() {
        assert!(has_var("var(--a)"));
        assert!(has_var("1px VAR(--a)"));
        assert!(has_var("calc(var(--a) + 1px)"));
        assert!(!has_var("1px 2px"));
        assert!(!has_var("--my-var(x)"));
        assert!(!has_var("\"var(--a)\""));
        assert!(!has_var("/* var(--a) */ red"));
        assert!(!has_var("myvar(--a)"));
    }

    #[test]
    fn replaces_var() {
        let vars = [("--a", "1px"), ("--b", "red")];
        assert_eq!(sub("var(--a)", &vars).as_deref(), Some("1px"));
        assert_eq!(sub("var( --a ) var(--b)", &vars).as_deref(), Some("1px red"));
        assert_eq!(sub("calc(var(--a) * 2)", &vars).as_deref(), Some("calc(1px * 2)"));
        assert_eq!(sub("var(--zz, 5px)", &vars).as_deref(), Some("5px"));
        assert_eq!(sub("var(--zz,)", &vars).as_deref(), Some(""));
        assert_eq!(sub("var(--a, 5px)", &vars).as_deref(), Some("1px"));
        assert_eq!(sub("var(--zz, var(--b))", &vars).as_deref(), Some("red"));
        assert_eq!(sub("var(--zz, rgb(1, 2, 3))", &vars).as_deref(), Some("rgb(1, 2, 3)"));
        assert_eq!(sub("var(--zz, var(--yy, 7px))", &vars).as_deref(), Some("7px"));
        assert_eq!(sub("\"var(--zz)\" var(--a)", &vars).as_deref(), Some("\"var(--zz)\" 1px"));
        assert_eq!(sub("1px 2px", &vars).as_deref(), Some("1px 2px"));
    }

    #[test]
    fn bad_var() {
        for text in ["var(--zz)", "var(a)", "var()", "var(--)", "var(--a", "var(--zz, var(--yy))", "var(--a) var(--zz)"] {
            assert_eq!(sub(text, &[("--a", "1")]), None, "{text}");
        }
    }

    #[test]
    fn deep_and_big_are_stopped() {
        let deep = format!("{}1px{}", "var(--zz,".repeat(1000), ")".repeat(1000));
        assert_eq!(sub(&deep, &[]), None);
        let big = "x".repeat(40_000);
        assert_eq!(sub("var(--a) var(--a)", &[("--a", &big)]), None);
        assert!(sub("var(--a)", &[("--a", &big)]).is_some());
    }

    fn set(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs.iter().map(|&(n, v)| (n.into(), v.into())).collect()
    }

    /// `Sets::merge` of a parent set and `own`, as names and texts.
    fn merge(parent: &[(String, String)], own: &[(&str, &str)]) -> Vec<(String, String)> {
        let mut sets = Sets::default();
        let parent: Vec<(u32, &str)> = parent.iter().map(|(n, t)| (sets.name(n), t.as_str())).collect();
        let parent = sets.merge(0, &parent);
        let own: Vec<(u32, &str)> = own.iter().map(|&(n, t)| (sets.name(n), t)).collect();
        let out = sets.merge(parent, &own);
        sets.pairs(out).into_iter().map(|(n, t)| (n.into(), t.into())).collect()
    }

    #[test]
    fn sets_are_kept_once() {
        let mut sets = Sets::default();
        let (a, b) = (sets.name("--a"), sets.name("--b"));
        assert_eq!(sets.name("--a"), a);
        let one = sets.merge(0, &[(a, "1px")]);
        // The same set, made from another parent or after a new name came, gets the same number.
        let two = sets.merge(0, &[(a, "1px"), (b, "2px")]);
        let c = sets.name("--c");
        assert_eq!(sets.merge(two, &[(b, "initial"), (c, "initial")]), one);
        // An element that sets what the parent has keeps the set of the parent.
        assert_eq!(sets.merge(one, &[(a, "1px")]), one);
        assert_eq!(sets.merge(one, &[]), one);
        // Only removes give the empty set.
        assert_eq!(sets.merge(one, &[(a, "initial")]), 0);
        assert_eq!(sets.get(two, "--b"), Some("2px"));
        assert_eq!(sets.get(two, "--c"), None);
        assert_eq!(sets.get(two, "--zz"), None);
        // The names come out sorted, not in the order of their numbers.
        let z = sets.name("--0");
        let three = sets.merge(two, &[(z, "x")]);
        assert_eq!(sets.pairs(three), [("--0", "x"), ("--a", "1px"), ("--b", "2px")]);
        assert_eq!(sets.pairs(0), []);
    }

    #[test]
    fn merge_sets() {
        let parent = set(&[("--a", "1px"), ("--b", "2px")]);
        // An own value hides the parent one, and a new name is added. The result is sorted.
        assert_eq!(merge(&parent, &[("--b", "3px"), ("--c", "4px")]), set(&[("--a", "1px"), ("--b", "3px"), ("--c", "4px")]));
        // `var()` reads the new set first, then the parent one.
        assert_eq!(merge(&parent, &[("--c", "var(--a) var(--d)"), ("--d", "var(--b)")]), set(&[("--a", "1px"), ("--b", "2px"), ("--c", "1px 2px"), ("--d", "2px")]));
        // A circle, and something that depends on a circle, are not set. The parent value goes too.
        assert_eq!(merge(&parent, &[("--a", "var(--b2)"), ("--b2", "var(--a)"), ("--e", "var(--a)")]), set(&[("--b", "2px")]));
        // A name that is not set and has no fallback.
        assert_eq!(merge(&parent, &[("--a", "var(--nope)")]), set(&[("--b", "2px")]));
        // A name that uses its own parent value: `--a: var(--a)` is a circle.
        assert_eq!(merge(&parent, &[("--a", "var(--a)")]), set(&[("--b", "2px")]));
        assert_eq!(merge(&[], &[]), set(&[]));
        // `initial` removes it. `inherit` and `unset` take the parent value.
        assert_eq!(merge(&parent, &[("--a", "initial"), ("--b", "inherit"), ("--c", "unset")]), set(&[("--b", "2px")]));
    }

    #[test]
    fn a_long_chain_is_stopped() {
        let names: Vec<String> = (0..200).map(|i| format!("--v{i}")).collect();
        let texts: Vec<String> = (0..200).map(|i| if i == 199 { "1px".to_string() } else { format!("var(--v{})", i + 1) }).collect();
        let own: Vec<(&str, &str)> = names.iter().zip(&texts).map(|(n, t)| (n.as_str(), t.as_str())).collect();
        let out = merge(&[], &own);
        assert!(out.len() < 200);
    }
}
