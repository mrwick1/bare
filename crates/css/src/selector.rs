//! Selectors: the part of a rule before the `{`. They say which elements the rule is for.
//!
//! ```text
//! ul > li.item:first-child a[href^="http"], #main p
//! ```
//!
//! That is a list of two selectors, split by `,`. Each selector is a chain of "compounds" (`li.item:first-child`)
//! that are joined by "combinators":
//!
//! | Combinator | Written | Means                              |
//! | ---------- | ------- | ---------------------------------- |
//! | Descendant | space   | `a b`: a `b` anywhere inside an `a` |
//! | Child      | `>`     | `a > b`: a `b` right inside an `a`  |
//! | Next       | `+`     | `a + b`: a `b` right after an `a`   |
//! | Later      | `~`     | `a ~ b`: a `b` after an `a`         |
//!
//! A compound is a list of simple selectors that all must be true for one element:
//! a tag (`li`), `*`, an id (`#main`), a class (`.item`), an attribute (`[href^="http"]`),
//! or a pseudo-class (`:first-child`, `:not(.x)`).
//!
//! Two decisions for speed and memory:
//! - A selector is stored from the right to the left. The last compound comes first. The matcher (see
//!   `matching.rs`) starts at the element and goes up, so the cheapest test (the last compound) runs
//!   first, and most elements fail at once. Browsers do the same.
//! - Tag names and attribute names are `Atom`s of the document. Matching them is a number compare, not a text compare.
//!   So a selector is parsed for one `Document`, and it works only with that document.
//!
//! Specificity is counted here, once: (ids, classes, tags) packed in one `u32`, so the cascade compares one number.
//! `:not(x)` counts like its most specific argument. `:not()`, `:is()` and `:where()` take complex selectors (`:not(.a > b)`), as in Selectors 4. Pseudo-classes and attributes count like classes.
//!
//! Error rule (CSS Selectors 4): if one selector in a list is wrong, the whole rule is thrown away.
//! `parse_selector_list` gives `None` for that.
//!
//! Left out for now:
//! - One bad argument of `:is()` or `:where()` throws the whole rule away (Chrome drops only that argument).
//! - `:has()`, the `of S` part of `:nth-child()`, and `:only-of-type`. A rule that
//!   uses them is thrown away, like an unknown pseudo-class. (Chrome also throws away `:-moz-placeholder` and
//!   the other prefixed ones that it does not know. So do we.)
//! - Pseudo-elements (`::before`, and the old `:before` and `:after`) style a part that is not in the DOM. They are valid at
//!   the end of a selector, but they never match, so `li::before, p` styles only `p` (as in Chrome). Module 4 may bring back
//!   `::before` and `::after`, which make boxes.
//! - Namespaces (`svg|a`).
//! - The form pseudo-classes (`:disabled`, `:checked`, ...) are in `form.rs`.
//! - `:visited`, `:target`, `:valid` and `:invalid` are valid, but they never match. The user action pseudo-classes (`:hover`,
//!   `:active`, `:focus`, `:focus-within`, `:focus-visible`) match what the host stored in the document (`Document::user`).
//!
//! Comments compare Rust with JavaScript, because you know JS.

use bare_html::dom::{Atom, Document};

use crate::form::Form;
use crate::tokenizer::{Token, Tokens, tokenize};

/// How deep `:not(:not(...))` may go. A deeper selector is thrown away, so a bad style sheet cannot overflow the stack.
const MAX_DEPTH: u32 = 32;

/// One selector: `ul > li.item`.
#[derive(Debug, PartialEq)]
pub struct Selector {
    /// The compounds, from the right to the left: `ul > li.item` is `[li.item, ul]`.
    /// The combinator of a compound joins it to the next one in this list (the one on its left).
    /// The last compound has no one on its left, so its combinator means nothing.
    pub compounds: Vec<Compound>,
    /// (ids, classes, tags), 10 bits each: `ids << 20 | classes << 10 | tags`. A count over 1023 stays at 1023.
    pub specificity: u32,
    /// `::slotted(...)` or `::part(...)` at the end. The compounds then say which element the pseudo-element belongs to.
    pub pseudo: Option<Box<ShadowPseudo>>,
}

impl Selector {
    /// The bytes that the selector owns on the heap (not the selector itself). The cache of rule sets sums them to see how large a set is.
    pub fn heap_bytes(&self) -> usize {
        use std::mem::size_of;
        let pseudo = self.pseudo.as_deref().map_or(0, |p| {
            size_of::<ShadowPseudo>()
                + match p {
                    ShadowPseudo::Slotted(v) => simples_bytes(v),
                    ShadowPseudo::Part { names, then } => names.capacity() * size_of::<Box<str>>() + names.iter().map(|n| n.len()).sum::<usize>() + simples_bytes(then),
                }
        });
        compounds_bytes(&self.compounds) + pseudo
    }
}

fn compounds_bytes(v: &Vec<Compound>) -> usize {
    v.capacity() * std::mem::size_of::<Compound>() + v.iter().map(|c| simples_bytes(&c.simples)).sum::<usize>()
}

fn simples_bytes(v: &Vec<Simple>) -> usize {
    v.capacity() * std::mem::size_of::<Simple>() + v.iter().map(simple_bytes).sum::<usize>()
}

fn simple_bytes(s: &Simple) -> usize {
    use std::mem::size_of;
    let lists = |l: &Vec<Vec<Simple>>| l.capacity() * size_of::<Vec<Simple>>() + l.iter().map(simples_bytes).sum::<usize>();
    match s {
        Simple::Id(t) | Simple::Class(t) | Simple::State(t) => t.len(),
        Simple::Attr { test, .. } => match test {
            AttrTest::Exists => 0,
            AttrTest::Equals(t, _) | AttrTest::Word(t, _) | AttrTest::Dash(t, _) | AttrTest::Prefix(t, _) | AttrTest::Suffix(t, _) | AttrTest::Contains(t, _) => t.len(),
        },
        Simple::Not(l) | Simple::Is(l) => lists(l),
        Simple::Complex(c) => size_of::<Complex>() + c.args.capacity() * size_of::<Vec<Compound>>() + c.args.iter().map(compounds_bytes).sum::<usize>(),
        Simple::Host(h) => size_of::<Host>() + lists(&h.args),
        _ => 0,
    }
}

/// The two pseudo-elements that reach into a shadow tree (CSS Scoping).
#[derive(Debug, PartialEq)]
pub enum ShadowPseudo {
    /// `slot::slotted(.x)`: the nodes that the slot takes, when they match the compound.
    Slotted(Vec<Simple>),
    /// `host::part(a b):hover`: the elements of the shadow tree of the host that have all the names in `part`, and match `then`.
    Part { names: Vec<Box<str>>, then: Vec<Simple> },
}

/// Where a selector is read. It decides what `:host`, `::slotted()` and `::part()` are.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Mode {
    /// A script asks (`querySelector`, `matches`): the three are valid and match nothing.
    Script,
    /// A style sheet of the document (or an `@supports` test): `::part()` works, `:host` and `::slotted()` match nothing.
    Document,
    /// A style sheet of a shadow tree: all three work.
    Shadow,
}

#[derive(Debug, PartialEq)]
pub struct Compound {
    pub simples: Vec<Simple>,
    pub combinator: Combinator,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Combinator {
    /// `a b`
    Descendant,
    /// `a > b`
    Child,
    /// `a + b`
    Next,
    /// `a ~ b`
    Later,
}

#[derive(Debug, PartialEq)]
pub enum Simple {
    /// `li`. A tag name that is in lowercase, because the HTML parser makes element names lowercase.
    Tag(Atom),
    /// `#main`
    Id(Box<str>),
    /// `.item`
    Class(Box<str>),
    /// `[href^="http"]`
    Attr { name: Atom, test: AttrTest },
    /// `:first-child`: no element before this one in its parent.
    FirstChild,
    /// `:last-child`
    LastChild,
    /// `:only-child`
    OnlyChild,
    /// `:nth-child(2n+1)` and the like. See `Nth`.
    Nth(Nth),
    /// `:root`: the top element, `html`.
    Root,
    /// The reference element of a script selector.
    Scope(Option<std::rc::Rc<std::cell::Cell<Option<bare_html::dom::NodeId>>>>),
    /// `:empty`: no child elements, and no text.
    Empty,
    /// `:link`: an `a` or `area` with an `href`. (We have no history, so every link is "not visited".)
    Link,
    /// `:defined`: every element, except a custom element that no constructor has run for (`Document::is_defined`).
    Defined,
    /// `:state(name)`: a custom element with that custom state (`Document::states`).
    State(Box<str>),
    /// `:disabled`, `:checked` and the other form pseudo-classes. See `form.rs`.
    Form(crate::form::Form),
    /// `:visited` and friends. Never true.
    Never,
    /// `:hover` and the other user action pseudo-classes. One variant: an arm for each in `simple_matches` slows every other test.
    User(User),
    /// `:not(.a, b)`: true when none of the compounds match. Each one is a list of simple selectors.
    Not(Vec<Vec<Simple>>),
    /// `:is(.a, b)` and `:where(.a, b)`: true when one of the compounds matches. They match the same. Only the specificity differs.
    Is(Vec<Vec<Simple>>),
    /// `:not()`, `:is()` or `:where()` with an argument that has a combinator: `:not(.a > b, c)`. One boxed variant, like `Host`: most lists
    /// are compounds only, and they keep the cheaper `Not` and `Is`.
    Complex(Box<Complex>),
    /// `:host`, `:host(.a)` and `:host-context(.a)`. One variant: an arm for each in `simple_matches` slows every other test.
    Host(Box<Host>),
}

/// `:host` (no compound) and `:host(.a)`: the host of the shadow tree that the sheet belongs to. The cascade offers the rule only to the host.
/// `:host-context(.a)` (`context`): a host that has an ancestor (or is itself) that matches the compound.
#[derive(Debug, PartialEq)]
pub struct Host {
    pub context: bool,
    pub args: Vec<Vec<Simple>>,
}

/// The arguments of `Simple::Complex`. Each one is a complex selector: its compounds from the right to the left, like `Selector::compounds`.
/// The first compound tests the element itself, and the others walk up or back from it.
#[derive(Debug, PartialEq)]
pub struct Complex {
    /// `:not()`: true when no argument matches. Else `:is()` or `:where()`: true when one matches.
    pub not: bool,
    pub args: Vec<Vec<Compound>>,
}

fn is_host(s: &Simple) -> bool {
    matches!(s, Simple::Host(_))
}

/// The bits of `user_flags`: which user action state a selector reads.
/// A user action pseudo-class. It matches what the host stored in `Document::user`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum User {
    /// `:hover`: the element under the pointer, or one of its ancestors.
    Hover,
    /// `:active`: the element that the primary button holds down, or one of its ancestors.
    Active,
    /// `:focus`
    Focus,
    /// `:focus-within`: the focused element or one of its ancestors.
    FocusWithin,
    /// `:focus-visible`: the focused element, when it shows a focus ring.
    FocusVisible,
}

pub const USES_HOVER: u8 = 1;
pub const USES_ACTIVE: u8 = 2;
pub const USES_FOCUS: u8 = 4;
/// `:checked`, `:placeholder-shown` and the other form pseudo-classes, which read `Document::ui`.
pub const USES_FORM: u8 = 8;

/// Which user action or form state the selector reads (`USES_*`), also inside `:not()` and `:is()`.
pub fn user_flags(selector: &Selector) -> u8 {
    fn of(simples: &[Simple]) -> u8 {
        simples.iter().fold(0, |f, s| {
            f | match s {
                Simple::User(User::Hover) => USES_HOVER,
                Simple::User(User::Active) => USES_ACTIVE,
                Simple::User(_) => USES_FOCUS,
                Simple::Form(_) => USES_FORM,
                Simple::Not(args) | Simple::Is(args) => args.iter().fold(0, |f, a| f | of(a)),
                Simple::Complex(c) => c.args.iter().flatten().fold(0, |f, c| f | of(&c.simples)),
                _ => 0,
            }
        })
    }
    selector.compounds.iter().fold(0, |f, c| f | of(&c.simples))
}

/// `:nth-child(an+b)`: the place of the element among its siblings (the first is 1) must be `a*n + b` for some `n >= 0`.
/// `:first-of-type` is `0n+1` of the type, and `:last-child` stays its own variant because it is cheaper.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Nth {
    pub a: i32,
    pub b: i32,
    /// `:nth-last-child`: count from the last sibling.
    pub from_end: bool,
    /// `:nth-of-type`: count only the siblings with the same tag name.
    pub of_type: bool,
}

/// What `[name...]` asks about the value.
#[derive(Debug, PartialEq)]
pub enum AttrTest {
    /// `[a]`: the attribute is there.
    Exists,
    /// `[a=v]`
    Equals(Box<str>, Case),
    /// `[a~=v]`: `v` is one of the words, split by spaces.
    Word(Box<str>, Case),
    /// `[a|=v]`: the value is `v`, or starts with `v-`. (For `lang`: `en` and `en-US`.)
    Dash(Box<str>, Case),
    /// `[a^=v]`
    Prefix(Box<str>, Case),
    /// `[a$=v]`
    Suffix(Box<str>, Case),
    /// `[a*=v]`
    Contains(Box<str>, Case),
}

/// The flag after the value: `[a=v i]`. With `Insensitive`, the value in the selector is stored in lowercase.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Case {
    Sensitive,
    Insensitive,
}

/// Parse the prelude of a style rule: `a, b > c`. Gives `None` when any selector in it is not valid.
/// The names go in the name table of `doc`.
pub fn parse_selector_list(text: &str, doc: &mut Document) -> Option<Vec<Selector>> {
    parse_selector_list_in(text, doc, Mode::Script)
}

/// `parse_selector_list` for a style sheet: `mode` says what `:host`, `::slotted()` and `::part()` are.
pub fn parse_selector_list_in(text: &str, doc: &mut Document, mode: Mode) -> Option<Vec<Selector>> {
    parse_scoped(text, doc, mode, None)
}

/// Parse a script selector with its reference element.
pub fn parse_scoped(text: &str, doc: &mut Document, mode: Mode, scope: Option<std::rc::Rc<std::cell::Cell<Option<bare_html::dom::NodeId>>>>) -> Option<Vec<Selector>> {
    let mut parser = Parser { tokens: Peek { tokens: tokenize(text), peeked: None }, doc, mode, scope, pseudo: None, host: false };
    let mut list = Vec::new();
    loop {
        list.push(parser.selector(0)?);
        match parser.tokens.next() {
            None => return Some(list),
            Some(Token::Comma) => {}
            Some(_) => return None,
            // `selector` stops at a comma, at `)`, or at the end. A `)` here has no `(`, so it is an error.
        }
    }
}

/// What `Parser::pseudo_element` found.
enum Pe {
    /// No pseudo-element.
    No,
    /// One that never matches.
    Never,
    /// `::slotted()` or `::part()`: kept in `Parser::pseudo`.
    Taken,
}

struct Parser<'a, 'd> {
    tokens: Peek<'a>,
    doc: &'d mut Document,
    mode: Mode,
    scope: Option<std::rc::Rc<std::cell::Cell<Option<bare_html::dom::NodeId>>>>,
    /// The `::slotted()` or `::part()` that the last compound had.
    pseudo: Option<ShadowPseudo>,
    /// Did the selector have `:host` or `:host-context()`? Only a sheet of a shadow tree can.
    host: bool,
}

/// The tokens, and a way to look at the next one without taking it (JS: like an iterator with a `peek()`).
/// It is the same as `std::iter::Peekable`, but we need `Tokens::rest` too, and `Peekable` hides the iterator.
struct Peek<'a> {
    tokens: Tokens<'a>,
    peeked: Option<Token<'a>>,
}

impl<'a> Peek<'a> {
    fn peek(&mut self) -> Option<&Token<'a>> {
        if self.peeked.is_none() {
            self.peeked = self.tokens.next();
        }
        self.peeked.as_ref()
    }

    fn next(&mut self) -> Option<Token<'a>> {
        self.peeked.take().or_else(|| self.tokens.next())
    }

    fn next_if(&mut self, f: impl Fn(&Token<'a>) -> bool) -> Option<Token<'a>> {
        if self.peek().is_some_and(f) { self.next() } else { None }
    }
}

impl Parser<'_, '_> {
    fn skip_space(&mut self) -> bool {
        let mut skipped = false;
        while self.tokens.next_if(|t| *t == Token::Whitespace).is_some() {
            skipped = true;
        }
        skipped
    }

    /// One selector. Stops before a `,`, a `)` or the end (it does not take them).
    fn selector(&mut self, depth: u32) -> Option<Selector> {
        let mut counts = [0u32; 3];
        let compounds = self.complex(depth, &mut counts)?;
        let specificity = counts[0].min(1023) << 20 | counts[1].min(1023) << 10 | counts[2].min(1023);
        Some(Selector { compounds, specificity, pseudo: self.pseudo.take().map(Box::new) })
    }

    /// The compounds of one selector, from the right to the left. Adds their specificity to `counts`. Stops like `selector`.
    /// The argument of `:not()` and `:is()` is one of these too.
    fn complex(&mut self, depth: u32, counts: &mut [u32; 3]) -> Option<Vec<Compound>> {
        let mut compounds = Vec::new();
        // From left to right here. We turn it around at the end.
        self.skip_space();
        loop {
            let simples = self.compound(depth, counts)?;
            let spaced = self.skip_space();
            let combinator = match self.tokens.peek() {
                None | Some(Token::Comma | Token::CloseParen) => {
                    compounds.push(Compound { simples, combinator: Combinator::Descendant });
                    break;
                }
                Some(Token::Delim('>')) => Combinator::Child,
                Some(Token::Delim('+')) => Combinator::Next,
                Some(Token::Delim('~')) => Combinator::Later,
                Some(_) if spaced => Combinator::Descendant,
                Some(_) => return None,
                // No space and no combinator, and still not the end of the compound: like `a"b"`.
            };
            if self.pseudo.is_some() {
                // `a::part(x) b`: a pseudo-element ends the selector.
                return None;
            }
            if combinator != Combinator::Descendant || !spaced {
                self.tokens.next();
                self.skip_space();
            }
            compounds.push(Compound { simples, combinator });
        }
        // Now `compounds[i].combinator` joins it to `compounds[i + 1]`, on its right. We want the
        // other side: each compound joined to the one on its left. So shift the combinators by one,
        // then reverse.
        let mut prev = Combinator::Descendant;
        for c in &mut compounds {
            std::mem::swap(&mut prev, &mut c.combinator);
        }
        compounds.reverse();
        // Check: `a > b c`. Left to right: [a(>), b(descendant), c(-)]. After the shift: [a(-), b(>), c(descendant)].
        // After the reverse: [c(descendant), b(>), a(-)]. `c` is joined to `b` by a space, `b` to `a` by `>`. Right.
        // `:host` is the shadow host: nothing can be to its left.
        let last = compounds.len() - 1;
        if self.host && compounds[..last].iter().any(|c| c.simples.iter().any(is_host)) {
            return None;
        }
        Some(compounds)
    }

    /// A compound selector: `li.item:first-child`. It has at least one simple selector, or `*`.
    fn compound(&mut self, depth: u32, counts: &mut [u32; 3]) -> Option<Vec<Simple>> {
        let mut simples = Vec::new();
        let mut any = false;
        // Where `::part()` came in `simples`: what follows it is for the part, not for the host.
        let mut taken = None;
        // `*` adds no simple selector, but it makes a compound that is not empty.
        match self.tokens.peek() {
            Some(Token::Ident(_)) => {
                let Some(Token::Ident(name)) = self.tokens.next() else { unreachable!() };
                simples.push(Simple::Tag(self.doc.intern(&name.to_ascii_lowercase())));
                counts[2] += 1;
                any = true;
            }
            Some(Token::Delim('*')) => {
                self.tokens.next();
                any = true;
            }
            _ => {}
        }
        loop {
            match self.tokens.peek() {
                Some(Token::Hash { is_id: true, .. }) => {
                    let Some(Token::Hash { value, .. }) = self.tokens.next() else { unreachable!() };
                    simples.push(Simple::Id(value.into()));
                    counts[0] += 1;
                }
                Some(Token::Delim('.')) => {
                    self.tokens.next();
                    let Some(Token::Ident(name)) = self.tokens.next() else { return None };
                    simples.push(Simple::Class(name.into()));
                    counts[1] += 1;
                }
                Some(Token::OpenSquare) => {
                    self.tokens.next();
                    simples.push(self.attribute()?);
                    counts[1] += 1;
                }
                Some(Token::Colon) => {
                    self.tokens.next();
                    match self.pseudo_element(depth, counts)? {
                        Pe::Never => simples.push(Simple::Never),
                        Pe::Taken => taken = Some(simples.len()),
                        Pe::No => {
                            let simple = self.pseudo(depth, counts)?;
                            simples.push(simple);
                        }
                    }
                }
                _ => break,
            }
            any = true;
        }
        if let (Some(at), Some(ShadowPseudo::Part { then, .. })) = (taken, &mut self.pseudo) {
            *then = simples.split_off(at);
        }
        // The host has no type, id, class or attribute of its own that a rule of the shadow tree may read (CSS Scoping: it is featureless).
        if self.host && simples.iter().any(is_host) && simples.iter().any(|s| matches!(s, Simple::Tag(_) | Simple::Id(_) | Simple::Class(_) | Simple::Attr { .. })) {
            return None;
        }
        any.then_some(simples)
    }

    /// After the first `:`. Reads `::before` (or the old `:before`) and gives `Pe::Never`, `::slotted()` or `::part()` and gives `Pe::Taken`,
    /// or reads nothing and gives `Pe::No`. A pseudo-element styles a part that is not in the DOM, so it never matches an element. It must end
    /// the selector, and it cannot be inside `:not()`. Unknown names drop the rule, except `::-webkit-` ones (as in Chrome).
    fn pseudo_element(&mut self, depth: u32, counts: &mut [u32; 3]) -> Option<Pe> {
        const OLD: [&str; 4] = ["before", "after", "first-line", "first-letter"];
        const KNOWN: [&str; 9] = ["before", "after", "first-line", "first-letter", "marker", "placeholder", "selection", "backdrop", "file-selector-button"];
        let double = self.tokens.next_if(|t| *t == Token::Colon).is_some();
        let old = |t: &Token| matches!(t, Token::Ident(n) if OLD.iter().any(|o| n.eq_ignore_ascii_case(o)));
        if double {
            let which = match self.tokens.peek() {
                Some(Token::Function(f)) if f.eq_ignore_ascii_case("slotted") => Some(true),
                Some(Token::Function(f)) if f.eq_ignore_ascii_case("part") => Some(false),
                _ => None,
            };
            if let Some(slotted) = which {
                self.tokens.next();
                return self.shadow_pseudo(slotted, depth, counts);
            }
        }
        if !double && !self.tokens.peek().is_some_and(old) {
            return Some(Pe::No);
        }
        let Some(Token::Ident(name)) = self.tokens.next() else { return None };
        let known = KNOWN.iter().any(|k| name.eq_ignore_ascii_case(k)) || name.starts_with("-webkit-");
        self.skip_space();
        let end = matches!(self.tokens.peek(), None | Some(Token::Comma));
        (known && end && depth == 0).then_some(Pe::Never)
    }

    /// After `::slotted(` or `::part(`: the argument and the `)`. `::part()` may have pseudo-classes after it. Only a sheet that can use the
    /// pseudo-element keeps it (`Mode`): else it is `Pe::Never`.
    fn shadow_pseudo(&mut self, slotted: bool, depth: u32, counts: &mut [u32; 3]) -> Option<Pe> {
        if depth != 0 || self.pseudo.is_some() {
            return None;
        }
        counts[2] += 1;
        let pseudo = if slotted {
            self.skip_space();
            let mut c = [0u32; 3];
            let arg = self.compound(depth + 1, &mut c)?;
            self.skip_space();
            if self.tokens.next()? != Token::CloseParen || !matches!(self.tokens.peek(), None | Some(Token::Comma)) {
                return None;
            }
            for (total, add) in counts.iter_mut().zip(c) {
                *total += add;
            }
            ShadowPseudo::Slotted(arg)
        } else {
            let mut names: Vec<Box<str>> = Vec::new();
            loop {
                self.skip_space();
                match self.tokens.next()? {
                    Token::Ident(n) => names.push(n.as_ref().into()),
                    Token::CloseParen if !names.is_empty() => break,
                    _ => return None,
                }
            }
            ShadowPseudo::Part { names, then: Vec::new() }
        };
        let works = match self.mode {
            Mode::Shadow => true,
            Mode::Document => !slotted,
            Mode::Script => false,
        };
        if !works {
            return Some(Pe::Never);
        }
        self.pseudo = Some(pseudo);
        Some(Pe::Taken)
    }

    /// After `[`. Reads up to and with the `]`.
    fn attribute(&mut self) -> Option<Simple> {
        self.skip_space();
        let Some(Token::Ident(name)) = self.tokens.next() else { return None };
        let name = self.doc.intern(&name.to_ascii_lowercase());
        // HTML attribute names are in lowercase in the DOM.
        self.skip_space();
        let op = match self.tokens.next()? {
            Token::CloseSquare => return Some(Simple::Attr { name, test: AttrTest::Exists }),
            Token::Delim('=') => '=',
            Token::Delim(c @ ('~' | '|' | '^' | '$' | '*')) => {
                if self.tokens.next() != Some(Token::Delim('=')) {
                    return None;
                }
                c
            }
            _ => return None,
        };
        self.skip_space();
        let value: Box<str> = match self.tokens.next()? {
            Token::Ident(v) | Token::String(v) => v.into(),
            _ => return None,
        };
        self.skip_space();
        let mut case = Case::Sensitive;
        if let Some(Token::Ident(flag)) = self.tokens.peek() {
            case = if flag.eq_ignore_ascii_case("i") {
                Case::Insensitive
            } else if flag.eq_ignore_ascii_case("s") {
                Case::Sensitive
            } else {
                return None;
            };
            self.tokens.next();
            self.skip_space();
        }
        if self.tokens.next() != Some(Token::CloseSquare) {
            return None;
        }
        let value = if case == Case::Insensitive { value.to_ascii_lowercase().into() } else { value };
        let test = match op {
            '=' => AttrTest::Equals(value, case),
            '~' => AttrTest::Word(value, case),
            '|' => AttrTest::Dash(value, case),
            '^' => AttrTest::Prefix(value, case),
            '$' => AttrTest::Suffix(value, case),
            _ => AttrTest::Contains(value, case),
        };
        Some(Simple::Attr { name, test })
    }

    /// After the `:`.
    fn pseudo(&mut self, depth: u32, counts: &mut [u32; 3]) -> Option<Simple> {
        match self.tokens.next()? {
            Token::Ident(name) => {
                counts[1] += 1;
                Some(match &*name.to_ascii_lowercase() {
                    "first-child" => Simple::FirstChild,
                    "last-child" => Simple::LastChild,
                    "only-child" => Simple::OnlyChild,
                    "root" => Simple::Root,
                    "scope" => Simple::Scope(self.scope.clone()),
                    "empty" => Simple::Empty,
                    "host" => if self.mode == Mode::Shadow { self.host = true; Simple::Host(Box::new(Host { context: false, args: Vec::new() })) } else { Simple::Never },
                    "link" | "any-link" => Simple::Link,
                    "defined" => Simple::Defined,
                    "disabled" => Simple::Form(Form::Disabled),
                    "enabled" => Simple::Form(Form::Enabled),
                    "required" => Simple::Form(Form::Required),
                    "optional" => Simple::Form(Form::Optional),
                    "read-only" => Simple::Form(Form::ReadOnly),
                    "read-write" => Simple::Form(Form::ReadWrite),
                    "placeholder-shown" => Simple::Form(Form::PlaceholderShown),
                    "checked" => Simple::Form(Form::Checked),
                    "default" => Simple::Form(Form::Default),
                    "indeterminate" => Simple::Form(Form::Indeterminate),
                    "first-of-type" => Simple::Nth(Nth { a: 0, b: 1, from_end: false, of_type: true }),
                    "last-of-type" => Simple::Nth(Nth { a: 0, b: 1, from_end: true, of_type: true }),
                    "hover" => Simple::User(User::Hover),
                    "active" => Simple::User(User::Active),
                    "focus" => Simple::User(User::Focus),
                    "focus-within" => Simple::User(User::FocusWithin),
                    "focus-visible" => Simple::User(User::FocusVisible),
                    "visited" | "target" | "invalid" | "valid" => Simple::Never,
                    _ => return None,
                })
            }
            Token::Function(name) if matches!(&*name.to_ascii_lowercase(), "not" | "is" | "where" | "matches" | "-webkit-any" | "-moz-any") => {
                let kind = name.to_ascii_lowercase();
                if depth >= MAX_DEPTH {
                    return None;
                }
                let mut args = Vec::new();
                let mut best = [0u32; 3];
                // The most specific argument. Arrays compare from the first number: ids, then classes, then tags.
                loop {
                    let mut c = [0u32; 3];
                    self.skip_space();
                    args.push(self.complex(depth + 1, &mut c)?);
                    best = best.max(c);
                    match self.tokens.next()? {
                        Token::Comma => {}
                        Token::CloseParen => break,
                        _ => return None,
                    }
                }
                // `:where` has no specificity at all.
                if kind != "where" {
                    for (total, add) in counts.iter_mut().zip(best) {
                        *total += add;
                    }
                }
                let not = kind == "not";
                if args.iter().any(|a| a.len() > 1) {
                    return Some(Simple::Complex(Box::new(Complex { not, args })));
                }
                // Compounds only. (JS: `args.map(a => a.pop().simples)`; `into_iter` takes the vectors, so nothing is copied.)
                let args = args.into_iter().filter_map(|mut a| a.pop()).map(|c| c.simples).collect();
                Some(if not { Simple::Not(args) } else { Simple::Is(args) })
            }
            Token::Function(name) if name.eq_ignore_ascii_case("host") || name.eq_ignore_ascii_case("host-context") => {
                // The argument is one compound. It counts like the argument of `:is()`, and the pseudo-class like a class.
                let context = name.eq_ignore_ascii_case("host-context");
                if depth >= MAX_DEPTH {
                    return None;
                }
                let mut c = [0u32; 3];
                self.skip_space();
                let arg = self.compound(depth + 1, &mut c)?;
                self.skip_space();
                if self.tokens.next()? != Token::CloseParen {
                    return None;
                }
                counts[1] += 1;
                for (total, add) in counts.iter_mut().zip(c) {
                    *total += add;
                }
                self.host |= self.mode == Mode::Shadow;
                Some(match (self.mode == Mode::Shadow, context) {
                    (false, _) => Simple::Never,
                    (true, context) => Simple::Host(Box::new(Host { context, args: vec![arg] })),
                })
            }
            Token::Function(name) if name.eq_ignore_ascii_case("state") => {
                counts[1] += 1;
                self.skip_space();
                let Token::Ident(state) = self.tokens.next()? else { return None };
                self.skip_space();
                (self.tokens.next()? == Token::CloseParen).then(|| Simple::State(state.as_ref().into()))
            }
            Token::Function(name) => {
                let (from_end, of_type) = match &*name.to_ascii_lowercase() {
                    "nth-child" => (false, false),
                    "nth-last-child" => (true, false),
                    "nth-of-type" => (false, true),
                    "nth-last-of-type" => (true, true),
                    _ => return None,
                };
                counts[1] += 1;
                // The tokens lose the sign of `+1` in `2n+1`, and a number that is not signed is wrong there. So read the text.
                let rest = self.tokens.tokens.rest();
                let end = rest.find(')')?;
                let (a, b) = an_plus_b(&rest[..end])?;
                while self.tokens.next()? != Token::CloseParen {}
                Some(Simple::Nth(Nth { a, b, from_end, of_type }))
            }
            _ => None,
        }
    }
}

/// Read `odd`, `even`, `5`, `n`, `-n+3`, `2n + 1`, `+3n-2`. Gives `(a, b)`. Spaces are allowed only
/// around the sign between `an` and `b`.
fn an_plus_b(text: &str) -> Option<(i32, i32)> {
    let text = text.trim_matches(|c: char| c.is_ascii_whitespace()).to_ascii_lowercase();
    match &*text {
        "odd" => return Some((2, 1)),
        "even" => return Some((2, 0)),
        _ => {}
    }
    let Some(n) = text.find('n') else {
        return text.parse::<i32>().ok().filter(|_| text.bytes().all(|c| c.is_ascii_digit() || c == b'-' || c == b'+')).map(|b| (0, b));
    };
    let (a, b) = text.split_at(n);
    let a = match a {
        "" | "+" => 1,
        "-" => -1,
        _ => a.parse::<i32>().ok()?,
    };
    let b = b[1..].trim_start_matches(|c: char| c.is_ascii_whitespace());
    if b.is_empty() {
        return Some((a, 0));
    }
    let (sign, digits) = b.split_at_checked(1)?;
    // A sign is one byte. `split_at(1)` panics inside a longer character.
    let digits = digits.trim_start_matches(|c: char| c.is_ascii_whitespace());
    if !digits.bytes().all(|c| c.is_ascii_digit()) || digits.is_empty() {
        return None;
    }
    let b = digits.parse::<i32>().ok()?;
    match sign {
        "+" => Some((a, b)),
        "-" => Some((a, -b)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nth_with_a_wide_character_after_n_is_not_a_panic() {
        assert_eq!(an_plus_b("2n\u{fffd}1"), None);
    }

    fn parse(text: &str) -> Option<Vec<Selector>> {
        parse_selector_list(text, &mut Document::new())
    }

    fn spec(text: &str) -> (u32, u32, u32) {
        let list = parse(text).unwrap_or_else(|| panic!("{text} did not parse"));
        let s = list[0].specificity;
        (s >> 20, (s >> 10) & 1023, s & 1023)
    }

    #[test]
    fn simple_selectors() {
        let mut doc = Document::new();
        let list = parse_selector_list("li.a#b[href]", &mut doc).unwrap();
        let li = doc.intern("li");
        let href = doc.intern("href");
        assert_eq!(
            list[0].compounds[0].simples,
            vec![
                Simple::Tag(li),
                Simple::Class("a".into()),
                Simple::Id("b".into()),
                Simple::Attr { name: href, test: AttrTest::Exists },
            ]
        );
    }

    #[test]
    fn selectors_are_stored_from_the_right() {
        let mut doc = Document::new();
        let list = parse_selector_list("a > b c + d", &mut doc).unwrap();
        let tags: Vec<_> = list[0]
            .compounds
            .iter()
            .map(|c| match c.simples[0] {
                Simple::Tag(t) => (doc.atom_str(t).to_string(), c.combinator),
                _ => panic!(),
            })
            .collect();
        // `d` is next to `c`, `c` is inside `b`, `b` is a child of `a`.
        assert_eq!(
            &tags[..3],
            &[("d".into(), Combinator::Next), ("c".into(), Combinator::Descendant), ("b".into(), Combinator::Child)]
        );
        assert_eq!(tags[3].0, "a");
    }

    #[test]
    fn combinators_with_and_without_spaces() {
        for text in ["a>b", "a > b", "a >b", "a> b", "a+b", "a ~ b", "a   b"] {
            assert_eq!(parse(text).unwrap()[0].compounds.len(), 2, "{text}");
        }
    }

    #[test]
    fn lists_split_on_commas() {
        let list = parse(" a , b.c ,d ").unwrap();
        assert_eq!(list.len(), 3);
    }

    #[test]
    fn universal_selector() {
        assert_eq!(parse("*").unwrap()[0].compounds[0].simples, vec![]);
        assert_eq!(spec("*"), (0, 0, 0));
        assert_eq!(spec("* > p"), (0, 0, 1));
        assert_eq!(parse("*.a").unwrap()[0].compounds[0].simples.len(), 1);
    }

    #[test]
    fn specificity_counts() {
        assert_eq!(spec("p"), (0, 0, 1));
        assert_eq!(spec("ul li"), (0, 0, 2));
        assert_eq!(spec(".a"), (0, 1, 0));
        assert_eq!(spec("li.a.b"), (0, 2, 1));
        assert_eq!(spec("#a .b c"), (1, 1, 1));
        assert_eq!(spec("[href]"), (0, 1, 0));
        assert_eq!(spec("a:first-child"), (0, 1, 1));
        assert_eq!(spec(":not(.a)"), (0, 1, 0));
        assert_eq!(spec(":not(#a, .b, c)"), (1, 0, 0));
        assert_eq!(spec("p:not(.a):not(.b)"), (0, 2, 1));
        assert_eq!(spec(":is(#a, .b, c)"), (1, 0, 0));
        assert_eq!(spec("p:is(.a, .b)"), (0, 1, 1));
        assert_eq!(spec(":where(#a, .b, c)"), (0, 0, 0));
        assert_eq!(spec("p:where(#a)"), (0, 0, 1));
        assert_eq!(spec(":is(:where(#a), .b)"), (0, 1, 0));
        // A complex argument counts all its compounds.
        assert_eq!(spec("a:not(.x a)"), (0, 1, 2));
        assert_eq!(spec(":not(.a > .b, #c)"), (1, 0, 0));
        assert_eq!(spec(":not(.a > .b .c, #c)"), (1, 0, 0));
        assert_eq!(spec(":not(.a > .b .c, .d)"), (0, 3, 0));
        assert_eq!(spec(":is(#a .b, c)"), (1, 1, 0));
        assert_eq!(spec(":where(#a .b, c)"), (0, 0, 0));
    }

    #[test]
    fn complex_arguments() {
        // Selectors 4: `:not()`, `:is()` and `:where()` take a list of complex selectors.
        for text in ["a:not(.x a)", ":not(.a > .b, .c ~ .d)", ":not(.a + .b)", ":is(a b)", ":where(a > b)", ":not(:is(.a .b) .c)", "#mw-content-text a[href][title]:not(.extiw, .mw-cite-backlink a, [href^=\"#\"])"] {
            assert!(parse(text).is_some(), "{text:?} should parse");
        }
        let simple = |t: &str| parse(t).unwrap().remove(0).compounds.remove(0).simples.remove(0);
        let Simple::Complex(c) = simple(":not(.a > .b, .c)") else { panic!("not complex") };
        let (not, args) = (c.not, c.args);
        assert!(not);
        assert_eq!(args.len(), 2);
        // From the right to the left, like a selector.
        assert_eq!(args[0].iter().map(|c| (c.simples.len(), c.combinator)).collect::<Vec<_>>(), [(1, Combinator::Child), (1, Combinator::Descendant)]);
        assert_eq!(args[0][0].simples, [Simple::Class("b".into())]);
        assert_eq!(args[1].len(), 1);
        let Simple::Complex(c) = simple(":where(a b)") else { panic!("not complex") };
        assert!(!c.not);
    }

    #[test]
    fn specificity_orders_as_a_number() {
        let sp = |t: &str| parse(t).unwrap()[0].specificity;
        assert!(sp("#a") > sp(".a.b.c.d.e.f.g.h.i.j"));
        assert!(sp(".a") > sp("a b c d e f g h i j"));
        assert!(sp("a b") > sp("a"));
    }

    #[test]
    fn specificity_stops_at_1023() {
        let text = ".a".repeat(2000);
        assert_eq!(spec(&text), (0, 1023, 0));
    }

    #[test]
    fn attribute_forms() {
        let mut doc = Document::new();
        let a = doc.intern("a");
        let one = |text: &str, doc: &mut Document| parse_selector_list(text, doc).unwrap().remove(0).compounds.remove(0).simples.remove(0);
        let s = Case::Sensitive;
        for (text, test) in [
            ("[a]", AttrTest::Exists),
            ("[ a ]", AttrTest::Exists),
            ("[a=b]", AttrTest::Equals("b".into(), s)),
            ("[a='b c']", AttrTest::Equals("b c".into(), s)),
            ("[a = \"b\" ]", AttrTest::Equals("b".into(), s)),
            ("[a~=b]", AttrTest::Word("b".into(), s)),
            ("[a|=b]", AttrTest::Dash("b".into(), s)),
            ("[a^=b]", AttrTest::Prefix("b".into(), s)),
            ("[a$=b]", AttrTest::Suffix("b".into(), s)),
            ("[a*=b]", AttrTest::Contains("b".into(), s)),
            ("[a=B i]", AttrTest::Equals("b".into(), Case::Insensitive)),
            ("[a=B s]", AttrTest::Equals("B".into(), s)),
            ("[A=b]", AttrTest::Equals("b".into(), s)),
        ] {
            assert_eq!(one(text, &mut doc), Simple::Attr { name: a, test }, "{text}");
        }
    }

    #[test]
    fn escapes_in_names() {
        let list = parse(r".a\:b #\31 23").unwrap();
        assert_eq!(list[0].compounds[1].simples, vec![Simple::Class("a:b".into())]);
        assert_eq!(list[0].compounds[0].simples, vec![Simple::Id("123".into())]);
    }

    #[test]
    fn tag_names_are_lowercase() {
        let mut doc = Document::new();
        let list = parse_selector_list("DIV", &mut doc).unwrap();
        let div = doc.intern("div");
        assert_eq!(list[0].compounds[0].simples, vec![Simple::Tag(div)]);
    }

    #[test]
    fn pseudo_classes() {
        let simple = |t: &str| parse(t).unwrap().remove(0).compounds.remove(0).simples.remove(0);
        assert_eq!(simple(":first-child"), Simple::FirstChild);
        assert_eq!(simple(":LAST-child"), Simple::LastChild);
        assert_eq!(simple(":only-child"), Simple::OnlyChild);
        assert_eq!(simple(":root"), Simple::Root);
        assert_eq!(simple(":empty"), Simple::Empty);
        assert_eq!(simple(":link"), Simple::Link);
        assert_eq!(simple(":hover"), Simple::User(User::Hover));
        assert_eq!(simple(":visited"), Simple::Never);
        assert_eq!(simple(":is(.a, #b)"), Simple::Is(vec![vec![Simple::Class("a".into())], vec![Simple::Id("b".into())]]));
        assert_eq!(simple(":not(.a, #b)"), Simple::Not(vec![vec![Simple::Class("a".into())], vec![Simple::Id("b".into())]]));
    }

    #[test]
    fn an_plus_b_forms() {
        for (text, ab) in [
            ("odd", (2, 1)), ("EVEN", (2, 0)), ("3", (0, 3)), ("+3", (0, 3)), ("-3", (0, -3)), ("0", (0, 0)),
            ("n", (1, 0)), ("+n", (1, 0)), ("-n", (-1, 0)), ("2n", (2, 0)), ("2n+1", (2, 1)), ("2n-1", (2, -1)),
            ("2n + 1", (2, 1)), ("2n - 1", (2, -1)), (" -n+3 ", (-1, 3)), ("+3n-2", (3, -2)), ("N+2", (1, 2)),
        ] {
            assert_eq!(an_plus_b(text), Some(ab), "{text}");
        }
        for text in ["", "n+", "n 1", "2 n", "1.5", "a", "n+-1", "n--1", "--n", "2n+1n", "99999999999"] {
            assert_eq!(an_plus_b(text), None, "{text}");
        }
    }

    #[test]
    fn nth_pseudo_classes() {
        let nth = |t: &str| match parse(t).unwrap().remove(0).compounds.remove(0).simples.remove(0) {
            Simple::Nth(n) => n,
            other => panic!("{other:?}"),
        };
        assert_eq!(nth(":nth-child(2n+1)"), Nth { a: 2, b: 1, from_end: false, of_type: false });
        assert_eq!(nth(":NTH-LAST-CHILD( odd )"), Nth { a: 2, b: 1, from_end: true, of_type: false });
        assert_eq!(nth(":nth-of-type(3)"), Nth { a: 0, b: 3, from_end: false, of_type: true });
        assert_eq!(nth(":nth-last-of-type(-n + 2)"), Nth { a: -1, b: 2, from_end: true, of_type: true });
        assert_eq!(nth(":first-of-type"), Nth { a: 0, b: 1, from_end: false, of_type: true });
        assert_eq!(nth(":last-of-type"), Nth { a: 0, b: 1, from_end: true, of_type: true });
        // Selectors go on after it, and it counts as a class.
        assert_eq!(spec("li:nth-child(2n+1) > a"), (0, 1, 2));
        assert_eq!(parse("li:nth-child(2n + 1).on:not(:nth-child(2))").unwrap()[0].compounds[0].simples.len(), 4);
    }

    #[test]
    fn a_wrong_selector_throws_away_the_list() {
        for text in [
            "", " ", ",", "a,", ",a", "a,,b", ">", "a >", "a > > b", ".", ". a", "#", "#1", "a#1",
            "[", "[a", "[a=", "[a=]", "[=a]", "[a=b c]", "[a=b i j]", "[a=b x]", "[a!=b]", "[a ~ = b]", "[1]",
            ":", "a:", ":unknown", "::x", ":x::before", "a::before b", "a::before.c", "a::before > b", ":not(::before)", ":is(a::after)", "::", "::before(", ":nth-child()", ":nth-child(2 n)", ":nth-child(n 1)", ":nth-child(1.5)", ":nth-child(a)", ":nth-child(2n+)", ":nth-child(2n+1", ":is()", ":not()", ":not(a >)", ":not(> a)", ":not(a > > b)", ":is(a b,)", ":not(a ::before)", ":where(a::after b)",
            ":not(a", ":not(a,)", "a)", "a b)", "a(", "a\"b\"", "svg|a", "|a", "a{", "a;b", "!", "a!b",
        ] {
            assert!(parse(text).is_none(), "{text:?} should not parse");
        }
    }

    #[test]
    fn a_pseudo_element_is_valid_and_never_matches() {
        // As in Chrome: `::before` and the old `:before` end a selector. They match no element, but they do not throw away the list.
        for text in ["::before", "a::after", "li:before", "p:first-line", "p::FIRST-LETTER", "li::marker", "input::placeholder", "::-webkit-scrollbar", "a::before , b"] {
            assert!(parse(text).is_some(), "{text:?} should parse");
        }
        assert_eq!(parse("li::before, p").unwrap().len(), 2);
        assert_eq!(parse("a::after").unwrap()[0].compounds[0].simples.last(), Some(&Simple::Never));
    }

    #[test]
    fn deep_not_does_not_overflow() {
        let text = format!("{}a{}", ":not(".repeat(100_000), ")".repeat(100_000));
        assert!(parse(&text).is_none());
        let ok = format!("{}a{}", ":not(".repeat(10), ")".repeat(10));
        assert!(parse(&ok).is_some());
    }

    #[test]
    fn random_input_never_panics() {
        // A small fixed-seed generator, so a failure repeats. Letters that make selector tokens.
        let alphabet: Vec<char> = "ab.#:[]()>+~*,= \"'|^$-\\1i:not(".chars().collect();
        let mut seed = 0x2545_f491_4f6c_dd1d_u64;
        for _ in 0..4000 {
            let mut text = String::new();
            for _ in 0..(seed % 24) {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                text.push(alphabet[(seed % alphabet.len() as u64) as usize]);
            }
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let _ = parse(&text);
        }
    }
}
