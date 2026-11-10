//! The declaration block of `browser_style`: a list of declarations over the native `style` attribute text.
//!
//! No script runs here and nothing here touches the document. Every function works on a `Vec<Decl>` that the caller made
//! from the attribute a moment ago, so a failure leaves the attribute as it was (the caller writes only at the end).
//!
//! The model (CSSOM "declaration block", with shorthands)
//! - A declaration is one authored `name: value [!important]`. It `own`s a set of longhand properties (`mask`), and one
//!   longhand is owned by at most one declaration: a later write takes the longhands it sets from the older ones. A
//!   shorthand declaration (`margin: 1px 2px`) owns its longhands, so `marginTop`, `length`, `item()` and
//!   `removeProperty("margin-top")` see them. The longhand text is cut from the authored text with the engine's own
//!   value code (`bare_css::values::parse_declaration`): a component of the authored text is that longhand's text when it
//!   parses to the same typed value (`1cm` stays `1cm`). A value that was not written (`flex: 2` sets `flex-basis`) is
//!   written from the typed value, and only when the text reads back to the same typed value.
//! - A custom property (`--x`) is one declaration owned by its exact name. A declaration that the engine does not
//!   understand (an unknown property, or a value it refuses) is `Opaque`: invisible to the CSSOM reads, never changed,
//!   and written back as it was. Nothing in the attribute is dropped except text that is not a declaration (`oops;`).
//! - Pending shorthands retain their source and surviving mask. Their longhands expose empty values until computation.
//!   Partial pending blocks need persistent state in the caller because their serialization is lossy.
//! - Serialization (CSSOM "serialize a CSS declaration block"): an unchanged shorthand declaration is written as it was
//!   authored. Longhands that together make `margin`, `padding`, `inset`, `border-width|style|color`, `border-top|...`,
//!   `border`, `flex`, `gap`, `overflow`, `border-spacing` or `place-*` (all present, same priority, no `var()`) are
//!   written as that shorthand in the shortest form. Everything else is written as longhands.
//! - Caps are checked before the thing grows: bytes of the text, declarations while parsing (the parser stops at the
//!   cap), bytes of the output before each piece is added.

use bare_css::parser::{components, parse_declarations_capped};
use bare_css::values::{Declared, Property, SHORTHANDS, Side, Value, parse_declaration, shorthand_longhands};
use bare_css::var::has_var;
use bare_js::eval::Error;
use std::borrow::Cow;

const _: () = assert!(Property::COUNT <= 128, "a declaration owns its longhands as bits of a u128");

/// Why an operation stopped. The caller turns it into an error (a cap is `Limit(Host(..))`, never catchable).
pub(crate) enum Fail {
    /// `style-bytes`
    Bytes,
    /// `style-decls`
    Decls,
    /// A value that the model cannot write back faithfully.
    Unsupported(&'static str),
    /// A step or heap limit, or any other error of the interpreter.
    Err(Error),
}

impl From<Error> for Fail {
    fn from(e: Error) -> Fail {
        Fail::Err(e)
    }
}

type F<T> = Result<T, Fail>;

/// Charge work to the step budget before it is done.
pub(crate) type Charge<'a> = &'a mut dyn FnMut(u64) -> Result<(), Error>;

#[derive(Clone, Copy)]
pub(crate) struct Lim {
    pub bytes: usize,
    pub decls: usize,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Kind {
    /// A supported longhand.
    Long,
    /// A supported shorthand.
    Short,
    Custom,
    Opaque,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Decl {
    /// As authored: lowercase except a custom property. A supported name is the engine's own `&'static str`: no allocation.
    pub name: Cow<'static, str>,
    /// The text after the `:`, trimmed, without `!important`.
    pub value: String,
    pub important: bool,
    pub kind: Kind,
    /// The longhands this declaration owns now.
    pub mask: u128,
}

fn bit(p: Property) -> u128 {
    1u128 << (p as usize)
}

pub(crate) fn custom(name: &str) -> bool {
    name.len() > 2 && name.starts_with("--")
}

pub(crate) fn supported(name: &str) -> bool {
    Property::from_name(name).is_some() || SHORTHANDS.contains(&name)
}

/// The longhands a supported name sets, in the order of the standard. Empty for anything else.
fn longs(name: &str) -> &'static [Property] {
    if let Some(l) = shorthand_longhands(name) {
        return l;
    }
    match Property::from_name(name) {
        Some(p) => &Property::ALL[p as usize..p as usize + 1],
        None => &[],
    }
}

fn mask_of(name: &str) -> u128 {
    longs(name).iter().fold(0, |m, &p| m | bit(p))
}

/// The longhands that a declaration owns, in the order of its shorthand.
fn owned(d: &Decl) -> impl Iterator<Item = Property> + '_ {
    let list: &'static [Property] = match d.kind {
        Kind::Long | Kind::Short => longs(&d.name),
        _ => &[],
    };
    list.iter().copied().filter(|&p| d.mask & bit(p) != 0)
}

fn wide(t: &str) -> bool {
    ["inherit", "initial", "unset"].iter().any(|w| t.eq_ignore_ascii_case(w))
}

/// Does `value` of `property` mean `text`? Typed values point into a `Side`, so those are compared through it.
fn same(a: (&Value, &Side), b: (&Value, &Side)) -> bool {
    use bare_css::values::Length::Calc;
    match (a.0, b.0) {
        (Value::Length(Calc(i)), Value::Length(Calc(j))) => a.1.calcs.get(*i as usize) == b.1.calcs.get(*j as usize) && a.1.calcs.get(*i as usize).is_some(),
        (Value::Grid(i), Value::Grid(j)) => a.1.grids.get(*i as usize) == b.1.grids.get(*j as usize) && a.1.grids.get(*i as usize).is_some(),
        (Value::Var(_), _) | (_, Value::Var(_)) => false,
        (x, y) => x == y,
    }
}

/// Does `text` as the value of the longhand `property` read to exactly `value`?
fn reads_as(property: Property, text: &str, value: &Value, side: &Side) -> bool {
    let (mut s, mut out) = (Side::default(), Vec::new());
    parse_declaration(property.name(), text, false, &mut s, &mut out)
        && matches!(&out[..], [one] if one.property == property && same((&one.value, &s), (value, side)))
}

/// A typed value that was not written, as text. `None` when no text reads back to it.
fn written(property: Property, value: &Value, side: &Side) -> Option<String> {
    use bare_css::values::Length::*;
    let t = match *value {
        Value::Inherit => "inherit".to_string(),
        Value::Initial => "initial".to_string(),
        Value::Unset => "unset".to_string(),
        Value::Keyword(k) => k.name().to_string(),
        Value::Number(n) => format!("{n}"),
        Value::Length(l) => match l {
            Px(x) => format!("{x}px"),
            Em(x) => format!("{x}em"),
            Rem(x) => format!("{x}rem"),
            Percent(x) => format!("{x}%"),
            Vw(x) => format!("{x}vw"),
            Vh(x) => format!("{x}vh"),
            Vmin(x) => format!("{x}vmin"),
            Vmax(x) => format!("{x}vmax"),
            Calc(_) => return None,
        },
        Value::Color(c) if c.a == 255 => format!("rgb({}, {}, {})", c.r, c.g, c.b),
        Value::Color(c) => format!("rgba({}, {}, {}, {})", c.r, c.g, c.b, (c.a as f32 / 255.0 * 1000.0).round() / 1000.0),
        Value::Var(_) | Value::Grid(_) => return None,
    };
    reads_as(property, &t, value, side).then_some(t)
}

/// The longhand texts of one declaration. `None`: no faithful text. Longhands of a shorthand with `var()` are `""`.
type Texts<'a> = Vec<(Property, Option<Cow<'a, str>>)>;

/// The most top-level pieces of a shorthand value that we look at to find longhand text.
const MAX_PIECES: usize = 16;

fn texts<'a>(d: &'a Decl, charge: Charge) -> F<Texts<'a>> {
    if d.kind == Kind::Long {
        return Ok(owned(d).map(|p| (p, Some(Cow::Borrowed(d.value.as_str())))).collect());
    }
    let (mut side, mut out) = (Side::default(), Vec::new());
    if !parse_declaration(&d.name, &d.value, false, &mut side, &mut out) {
        return Err(Fail::Unsupported("a style shorthand that the engine no longer reads"));
    }
    let want: Vec<&Declared> = out.iter().filter(|x| d.mask & bit(x.property) != 0).collect();
    if has_var(&d.value) {
        return Ok(want.iter().map(|x| (x.property, Some(Cow::Borrowed("")))).collect());
    }
    // The candidates: each top-level piece, and each run of pieces between `/` (`span 3` in `2 / span 3`).
    let mut cands: Vec<&str> = Vec::new();
    if let Some(parts) = components(&d.value, MAX_PIECES) {
        let slice = |a: usize, b: usize| &d.value[a..b];
        cands.extend(parts.iter().map(|&(a, b)| slice(a, b)));
        for run in parts.split(|&(a, b)| slice(a, b) == "/") {
            if let (Some(&(a, _)), Some(&(_, b))) = (run.first(), run.last())
                && run.len() > 1
            {
                cands.push(slice(a, b));
            }
        }
    }
    charge(1 + (want.len() * (cands.len() + 1)) as u64 * (1 + d.value.len() as u64 / 16))?;
    let mut list = Texts::new();
    for x in want {
        let hit = cands.iter().find(|c| reads_as(x.property, c, &x.value, &side));
        let t = match hit {
            Some(c) => Some(Cow::Borrowed(*c)),
            None => written(x.property, &x.value, &side).map(Cow::Owned),
        };
        list.push((x.property, t));
    }
    Ok(list)
}

thread_local! {
    /// The output of `parse_declaration` when only its answer matters: one buffer, not one allocation for each call.
    static SCRATCH: std::cell::RefCell<Vec<Declared>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// A declaration from authored text. A name that is not supported, or a value that the engine refuses, is `Opaque`.
fn classify(name: &str, value: &str, important: bool) -> Decl {
    let mk = |name: Cow<'static, str>, kind, mask| Decl { name, value: value.to_string(), important, kind, mask };
    let own = || Cow::Owned(name.to_string());
    if custom(name) {
        return mk(own(), Kind::Custom, 0);
    }
    let long = Property::from_name(name);
    let fixed = long.map(Property::name).or_else(|| SHORTHANDS.iter().copied().find(|&s| s == name));
    match fixed {
        Some(fixed) if SCRATCH.with_borrow_mut(|out| {
            out.clear();
            parse_declaration(name, value, important, &mut Side::default(), out)
        }) => match long {
            Some(p) => mk(Cow::Borrowed(fixed), Kind::Long, bit(p)),
            None => mk(Cow::Borrowed(fixed), Kind::Short, mask_of(name)),
        },
        _ => mk(own(), Kind::Opaque, 0),
    }
}

/// Does a new declaration take every longhand of `e`, or the name of `e`?
fn vanishes(e: &Decl, new: &Decl) -> bool {
    match e.kind {
        Kind::Custom => new.kind == Kind::Custom && e.name == new.name,
        Kind::Long | Kind::Short => e.mask & new.mask != 0 && e.mask & !new.mask == 0,
        Kind::Opaque => false,
    }
}

/// Add `new`: it takes its longhands (or its name) from the older declarations and goes where the first one was.
/// `parsing`: the block is being read as written, so an earlier `!important` is not taken by a later normal one.
fn absorb(list: &mut Vec<Decl>, mut new: Decl, parsing: bool) {
    if new.kind == Kind::Opaque {
        list.push(new);
        return;
    }
    if parsing {
        if new.kind == Kind::Custom {
            if list.iter().any(|e| e.important && !new.important && e.kind == Kind::Custom && e.name == new.name) {
                return;
            }
        } else {
            let keep = list.iter().filter(|e| e.important && !new.important).fold(new.mask, |m, e| m & !e.mask);
            if keep == 0 {
                return;
            }
            new.mask = keep;
        }
    }
    let (mut at, mut kept, mut after) = (None, 0usize, false);
    list.retain_mut(|e| {
        let hit = match e.kind {
            Kind::Custom => new.kind == Kind::Custom && e.name == new.name,
            Kind::Long | Kind::Short => e.mask & new.mask != 0,
            Kind::Opaque => false,
        };
        let mut keep = true;
        if hit {
            at.get_or_insert(kept);
            if e.kind == Kind::Custom {
                keep = false;
            } else {
                let taken = e.mask & new.mask;
                if at == Some(kept) && e.mask != taken {
                    // The older shorthand keeps some longhands. The new one goes before it if its longhand comes first.
                    after = rank(e, taken) > rank(e, e.mask & !taken);
                }
                e.mask &= !new.mask;
                keep = e.mask != 0;
            }
        }
        kept += keep as usize;
        keep
    });
    match at {
        Some(i) => list.insert(i + usize::from(after), new),
        None => list.push(new),
    }
}

/// The place, in the order of its shorthand, of the first of the longhands `mask` that `d` has.
fn rank(d: &Decl, mask: u128) -> usize {
    longs(&d.name).iter().position(|&p| mask & bit(p) != 0).unwrap_or(usize::MAX)
}

/// Read a block of declarations (the attribute, or `cssText`).
pub(crate) fn parse(text: &str, lim: Lim, charge: Charge) -> F<Vec<Decl>> {
    if text.len() > lim.bytes {
        return Err(Fail::Bytes);
    }
    charge(1 + (text.len() / 4) as u64)?;
    let (parsed, more) = parse_declarations_capped(text, lim.decls);
    if more {
        return Err(Fail::Decls);
    }
    let mut list: Vec<Decl> = Vec::with_capacity(parsed.len());
    for p in &parsed {
        charge(1 + (p.value.len() / 8 + list.len()) as u64)?;
        absorb(&mut list, classify(&p.name, p.value, p.important), true);
    }
    Ok(list)
}

// ------------------------------------------------------------------------------------------------ reads

pub(crate) fn length(list: &[Decl]) -> usize {
    list.iter().map(|d| if d.kind == Kind::Custom { 1 } else { d.mask.count_ones() as usize }).sum()
}

/// The `i`-th longhand or custom property name, in the order of the list.
pub(crate) fn item(list: &[Decl], i: usize) -> Option<String> {
    list.iter().flat_map(|d| -> Vec<String> {
        match d.kind {
            Kind::Custom => vec![d.name.to_string()],
            Kind::Long | Kind::Short => owned(d).map(|p| p.name().to_string()).collect(),
            Kind::Opaque => vec![],
        }
    }).nth(i)
}

fn owner(list: &[Decl], p: Property) -> Option<usize> {
    list.iter().position(|d| d.mask & bit(p) != 0)
}

/// The `value` of longhand `p` in `d`, from the texts of `d`.
fn long_text<'a>(d: &'a Decl, p: Property, charge: Charge) -> F<Option<Cow<'a, str>>> {
    if d.kind == Kind::Long {
        return Ok(Some(Cow::Borrowed(d.value.as_str())));
    }
    match texts(d, charge)?.into_iter().find(|(q, _)| *q == p) {
        Some((_, Some(t))) => Ok(Some(t)),
        _ => Err(Fail::Unsupported("a style longhand with no faithful text (CSSStyleDeclaration)")),
    }
}

/// The shorthands that can be written from their longhands, with the way to join the texts.
#[derive(Clone, Copy)]
enum Join {
    Box4,
    Pair,
    Seq,
    Border,
}

const BUILD: &[(&str, Join)] = &[
    ("margin", Join::Box4), ("padding", Join::Box4), ("inset", Join::Box4), ("border-width", Join::Box4), ("border-style", Join::Box4),
    ("border-color", Join::Box4), ("gap", Join::Pair), ("overflow", Join::Pair), ("border-spacing", Join::Pair), ("place-items", Join::Pair),
    ("place-self", Join::Pair), ("place-content", Join::Pair), ("flex", Join::Seq), ("border-top", Join::Seq), ("border-right", Join::Seq),
    ("border-bottom", Join::Seq), ("border-left", Join::Seq), ("border", Join::Border),
];

fn join(how: Join, t: &[&str], max: usize) -> F<Option<String>> {
    let joined = |parts: &[&str]| -> F<Option<String>> {
        let len = parts.iter().try_fold(parts.len().saturating_sub(1), |n, p| n.checked_add(p.len()));
        if len.is_none_or(|n| n > max) { return Err(Fail::Bytes); }
        Ok(Some(parts.join(" ")))
    };
    if wide(t[0]) && t.iter().all(|x| x.eq_ignore_ascii_case(t[0])) {
        if t[0].len() > max { return Err(Fail::Bytes); }
        return Ok(Some(t[0].to_ascii_lowercase()));
    }
    if t.iter().any(|x| wide(x) || has_var(x)) { return Ok(None); }
    match how {
        Join::Box4 => {
            let n = if t[1] != t[3] { 4 } else if t[0] != t[2] { 3 } else if t[0] != t[1] { 2 } else { 1 };
            joined(&t[..n])
        }
        Join::Pair => joined(if t[0] == t[1] { &t[..1] } else { t }),
        Join::Seq => joined(t),
        Join::Border => {
            let same = |a: &[&str]| a.iter().all(|x| *x == a[0]);
            if !(same(&t[0..4]) && same(&t[4..8]) && same(&t[8..12])) { return Ok(None); }
            joined(&[t[0], t[4], t[8]])
        }
    }
}

/// The value of a shorthand written from its longhands. `None` when a longhand is missing, the priorities differ, or the
/// longhands cannot be joined.
fn build(list: &[Decl], name: &str, how: Join, max: usize, charge: Charge) -> F<Option<(String, bool)>> {
    let mut cache: Vec<(usize, Texts)> = Vec::new();
    let mut owners = Vec::new();
    let mut important = None;
    for &p in longs(name) {
        let Some(i) = owner(list, p) else { return Ok(None) };
        if *important.get_or_insert(list[i].important) != list[i].important {
            return Ok(None);
        }
        if !cache.iter().any(|(j, _)| *j == i) {
            cache.push((i, texts(&list[i], charge)?));
        }
        owners.push((i, p));
    }
    let mut refs = Vec::new();
    for (i, p) in owners {
        let t = &cache.iter().find(|(j, _)| *j == i).expect("cached").1;
        match t.iter().find(|(q, _)| *q == p) {
            Some((_, Some(x))) if !x.is_empty() => refs.push(x.as_ref()),
            _ => return Ok(None),
        }
    }
    Ok(join(how, &refs, max)?.map(|s| (s, important.unwrap_or(false))))
}

/// `getPropertyValue`. `None`: nothing to read (the property is not set, or a shorthand that cannot be written).
pub(crate) fn get(list: &[Decl], name: &str, max: usize, charge: Charge) -> F<Option<String>> {
    if custom(name) {
        return Ok(list.iter().find(|d| d.kind == Kind::Custom && d.name == name).map(|d| d.value.clone()));
    }
    if let Some(p) = Property::from_name(name) {
        return match owner(list, p) {
            None => Ok(None),
            Some(i) => {
                let t = long_text(&list[i], p, charge)?;
                if t.as_ref().is_some_and(|t| t.len() > max) { return Err(Fail::Bytes); }
                Ok(t.map(|t| t.into_owned()))
            }
        };
    }
    if !SHORTHANDS.contains(&name) {
        return Ok(None);
    }
    if let Some(d) = list.iter().find(|d| d.kind == Kind::Short && d.name == name && d.mask == mask_of(name)) {
        return Ok(Some(d.value.clone()));
    }
    match BUILD.iter().find(|(n, _)| *n == name) {
        Some(&(_, how)) => Ok(build(list, name, how, max, charge)?.map(|(s, _)| s)),
        None => Ok(None),
    }
}

/// `getPropertyPriority`: all the longhands are set, and all are important.
pub(crate) fn priority(list: &[Decl], name: &str) -> bool {
    if custom(name) {
        return list.iter().any(|d| d.kind == Kind::Custom && d.name == name && d.important);
    }
    let l = longs(name);
    !l.is_empty() && l.iter().all(|&p| owner(list, p).is_some_and(|i| list[i].important))
}

// ------------------------------------------------------------------------------------------------ writes

/// `setProperty` with a non-empty value. `Ok(false)`: the name, the value or nothing is acceptable, and the list is as it was.
pub(crate) fn set(list: &mut Vec<Decl>, name: &str, value: &str, important: bool, lim: Lim, probe: &mut String, charge: Charge) -> F<bool> {
    if !custom(name) && !supported(name) {
        return Ok(false);
    }
    if name.len() + value.len() > lim.bytes {
        return Err(Fail::Bytes);
    }
    charge(1 + (value.len() / 4) as u64)?;
    // The value must be exactly one declaration: no `;`, no `!important`, nothing cut off.
    probe.clear();
    for x in [name, ":", value] {
        probe.push_str(x);
    }
    let (parsed, _) = parse_declarations_capped(probe, 1);
    let [one] = &parsed[..] else { return Ok(false) };
    if one.important || one.name != name || one.value != value.trim() {
        return Ok(false);
    }
    let new = classify(name, one.value, important);
    if new.kind == Kind::Opaque {
        return Ok(false);
    }
    // The new declaration must be writable as longhands, or the block would hold something that cannot be read back.
    if new.kind == Kind::Short && texts(&new, charge)?.iter().any(|(_, t)| t.is_none()) {
        return Err(Fail::Unsupported("a style shorthand value with no faithful longhand text"));
    }
    let gone = list.iter().filter(|e| vanishes(e, &new)).count();
    if list.len() + 1 - gone > lim.decls {
        return Err(Fail::Decls);
    }
    charge(1 + list.len() as u64)?;
    absorb(list, new, false);
    Ok(true)
}

/// `removeProperty`: the old value, and whether the list changed.
pub(crate) fn remove(list: &mut Vec<Decl>, name: &str, max: usize, charge: Charge) -> F<(String, bool)> {
    let old = get(list, name, max, charge)?.unwrap_or_default();
    charge(1 + list.len() as u64)?;
    let before = (list.len(), length(list));
    if custom(name) {
        list.retain(|d| !(d.kind == Kind::Custom && d.name == name));
    } else {
        let m = mask_of(name);
        for d in list.iter_mut().filter(|d| matches!(d.kind, Kind::Long | Kind::Short)) {
            d.mask &= !m;
        }
        list.retain(|d| d.kind != Kind::Long && d.kind != Kind::Short || d.mask != 0);
    }
    let after = (list.len(), length(list));
    Ok((old, before != after))
}

pub(crate) fn inputs(list: &[Decl]) -> Vec<bare_css::cascade::InputDeclaration> {
    list.iter().map(|d| bare_css::cascade::InputDeclaration {
        name: d.name.to_string(), value: d.value.clone(), important: d.important, mask: d.mask,
    }).collect()
}

// ------------------------------------------------------------------------------------------------ output

/// An identifier as CSS text: what is not a name character is written as a hex escape.
fn ident(name: &str, max: usize) -> F<Cow<'_, str>> {
    let mut len = 0usize;
    let mut escape = false;
    for c in name.chars() {
        let safe = c.is_ascii_alphanumeric() || c == '-' || c == '_' || !c.is_ascii();
        escape |= !safe;
        let n = if safe { c.len_utf8() } else { 2 + if (c as u32) < 16 { 1 } else { 2 } };
        len = len.checked_add(n).ok_or(Fail::Bytes)?;
        if len > max { return Err(Fail::Bytes); }
    }
    if !escape { return Ok(Cow::Borrowed(name)); }
    let mut s = String::with_capacity(len);
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' || !c.is_ascii() { s.push(c); }
        else { s.push_str(&format!("\\{:x} ", c as u32)); }
    }
    Ok(Cow::Owned(s))
}

/// Output text with a byte cap that is checked before each piece is added.
struct Out<'a> {
    s: &'a mut String,
    max: usize,
    /// Every declaration was written as it is in the list, under its own name: reading the text back gives the same list.
    exact: bool,
}

impl Out<'_> {
    fn decl(&mut self, name: &str, value: &str, important: bool) -> F<()> {
        let tail = if important { " !important;" } else { ";" };
        let rest = usize::from(!self.s.is_empty()).checked_add(2).and_then(|n| n.checked_add(value.len())).and_then(|n| n.checked_add(tail.len())).ok_or(Fail::Bytes)?;
        let room = self.max.checked_sub(self.s.len()).and_then(|n| n.checked_sub(rest)).ok_or(Fail::Bytes)?;
        let name = ident(name, room)?;
        self.exact &= matches!(name, Cow::Borrowed(_));
        if !self.s.is_empty() {
            self.s.push(' ');
        }
        for x in [&*name, ": ", value, tail] {
            self.s.push_str(x);
        }
        Ok(())
    }
}

/// The families that can be written for the longhand `p`, the largest first. Built once: `BUILD` is fixed.
fn families(p: Property) -> &'static [(&'static str, Join)] {
    static TABLE: std::sync::OnceLock<Vec<Vec<(&'static str, Join)>>> = std::sync::OnceLock::new();
    &TABLE.get_or_init(|| {
        Property::ALL
            .iter()
            .map(|&p| {
                let mut v: Vec<_> = BUILD.iter().filter(|(n, _)| longs(n).contains(&p)).copied().collect();
                v.sort_by_key(|(n, _)| std::cmp::Reverse(longs(n).len()));
                v
            })
            .collect()
    })[p as usize]
}

/// The exposed text can be exact, or require separate pending state.
pub(crate) struct Serialized {
    pub exact: bool,
    pub pending: bool,
}

/// Serialize into `s`, which is cleared first. Pending longhands expose empty values.
pub(crate) fn serialize(list: &[Decl], lim: Lim, s: &mut String, charge: Charge) -> F<Serialized> {
    charge(1 + list.len() as u64)?;
    s.clear();
    let mut out = Out { s, max: lim.bytes, exact: true };
    let mut done = 0u128;
    let mut pending = false;
    for d in list {
        match d.kind {
            Kind::Custom | Kind::Opaque => out.decl(&d.name, &d.value, d.important)?,
            Kind::Short if d.mask == mask_of(&d.name) && d.mask & done == 0 => {
                out.decl(&d.name, &d.value, d.important)?;
                done |= d.mask;
            }
            Kind::Long | Kind::Short => {
                for p in owned(d) {
                    if done & bit(p) != 0 {
                        continue;
                    }
                    let mut wrote = false;
                    for &(name, how) in families(p) {
                        if mask_of(name) & done != 0 {
                            continue;
                        }
                        if let Some((v, important)) = build(list, name, how, lim.bytes, charge)? {
                            out.decl(name, &v, important)?;
                            out.exact = false;
                            done |= mask_of(name);
                            wrote = true;
                            break;
                        }
                    }
                    if !wrote {
                        out.exact &= d.kind == Kind::Long;
                        let t = long_text(d, p, charge)?.unwrap_or_default();
                        pending |= t.is_empty();
                        out.decl(p.name(), &t, d.important)?;
                        done |= bit(p);
                    }
                }
            }
        }
    }
    Ok(Serialized { exact: out.exact, pending })
}

#[cfg(test)]
#[path = "browser_style_pending_tests.rs"]
mod pending_reduction;
