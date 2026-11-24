//! String pattern methods and the span-based RegExp replace and split paths.
use crate::bi_string::{find, pattern};
use crate::bi_symbol::{MATCH, MATCH_ALL, REPLACE, SEARCH};
use crate::builtins::arg;
use crate::interp_rt::{ErrKind, R, Realm};
use crate::object::Obj;
use crate::regexp::Flags;
use crate::regexp_rt::protocol::{check_global, string_hook};
use crate::string::{Chars, MAX_LEN, Str};
use crate::value::Value;

const NONE: u32 = u32::MAX;

/// AdvanceStringIndex moves by one code unit or one Unicode code point.
pub(crate) fn step(s: Str, i: usize, unicode: bool) -> usize {
    match s.chars() {
        Some(Chars::Two(u)) if unicode && matches!(u.get(i), Some(0xD800..=0xDBFF)) && matches!(u.get(i + 1), Some(0xDC00..=0xDFFF)) => 2,
        _ => 1,
    }
}

fn match_or_search(rt: &mut Realm, this: Value, args: &[Value], symbol: usize, name: &str, flags: &str) -> R<Value> {
    let pat = arg(args, 0);
    if let Some(value) = string_hook(rt, this, pat, symbol, &[this])? {
        return Ok(value);
    }
    let s = rt.this_str(this, name)?;
    let re = rt.regexp_for(pat, flags)?;
    rt.invoke_rx_symbol(re, symbol, &[s.value()])
}

pub(crate) fn match_(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    match_or_search(rt, this, args, MATCH, "match", "")
}

pub(crate) fn match_all(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    check_global(rt, this, arg(args, 0), "matchAll")?;
    match_or_search(rt, this, args, MATCH_ALL, "matchAll", "g")
}

pub(crate) fn search(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    match_or_search(rt, this, args, SEARCH, "search", "")
}

/// `RegExp.prototype[Symbol.split]` for a regexp object `re`: `lim` is `ToUint32(limit)`.
pub(crate) fn fast_split(rt: &mut Realm, re: Obj, s: Str, lim: u32) -> R<Value> {
    if lim == 0 {
        return Ok(rt.new_array(&[]));
    }
    let s = rt.flat(s);
    let blob = rt.program(re)?;
    let flags = crate::regexp::image_flags(blob.words());
    let size = s.len();
    if size == 0 {
        return Ok(if rt.run_at(blob, s, 0)? { rt.new_array(&[]) } else { rt.new_array(&[s.value()]) });
    }
    let mut out: Vec<Value> = Vec::new();
    let (mut p, mut q) = (0, 0);
    // The spec runs a sticky copy at each `q`; a search that starts at `q` finds the same first match.
    while q < size {
        if !rt.run_at(blob, s, q)? {
            if !flags.sticky {
                break;
            }
            q += step(s, q, flags.unicode);
            continue;
        }
        let sp = rt.spans();
        let (i, e) = (sp[0] as usize, (sp[1] as usize).min(size));
        if i >= size {
            break;
        }
        if e == p {
            q += step(s, q, flags.unicode);
            continue;
        }
        out.push(rt.substring(s, p, i));
        if out.len() as u32 == lim {
            return Ok(rt.new_array(&out));
        }
        p = e;
        for g in sp[2..].chunks(2) {
            out.push(if g[0] == NONE { Value::UNDEFINED } else { rt.substring(s, g[0] as usize, g[1] as usize) });
            if out.len() as u32 == lim {
                return Ok(rt.new_array(&out));
            }
        }
        q = p;
    }
    out.push(rt.substring(s, p, size));
    Ok(rt.new_array(&out))
}

/// The match or one of the captures used in replacement text.
pub(crate) enum Capture<'a> {
    Number(usize),
    Name(&'a [u16]),
}

/// GetSubstitution. The caller supplies the matched text and captures.
#[inline]
pub(crate) fn expand<E>(
    out: &mut Vec<u16>, rep: &[u16], h: &[u16], at: usize, end: usize, count: usize, named: bool,
    mut capture: impl FnMut(&mut Vec<u16>, Capture<'_>) -> Result<(), E>,
) -> Result<(), E> {
    let digit = |u: u16| (0x30..=0x39).contains(&u).then(|| (u - 0x30) as usize);
    let mut i = 0;
    while i < rep.len() {
        let c = rep[i];
        let next = rep.get(i + 1).copied().unwrap_or(0);
        i += 1;
        if c != 0x24 {
            out.push(c);
            continue;
        }
        match next {
            0x24 => out.push(c),
            0x26 => capture(out, Capture::Number(0))?,
            0x60 => out.extend(&h[..at]),
            0x27 => out.extend(&h[end..]),
            0x3c if named => {
                let Some(n) = rep[i + 1..].iter().position(|&u| u == 0x3e) else {
                    out.push(c);
                    continue;
                };
                capture(out, Capture::Name(&rep[i + 1..i + 1 + n]))?;
                i += n + 2;
                continue;
            }
            _ => {
                let Some(d1) = digit(next) else {
                    out.push(c);
                    continue;
                };
                let two = rep.get(i + 1).copied().and_then(digit).map(|d2| d1 * 10 + d2).filter(|n| (1..=count).contains(n));
                let (n, used) = two.map_or((d1, 1), |n| (n, 2));
                if !(1..=count).contains(&n) {
                    out.push(c);
                    continue;
                }
                capture(out, Capture::Number(n))?;
                i += used;
                continue;
            }
        }
        i += 1;
    }
    Ok(())
}

pub(crate) enum Rep {
    Text(Vec<u16>),
    Func(Value),
}

impl Rep {
    pub(crate) fn new(rt: &mut Realm, v: Value) -> R<Self> {
        Ok(if Realm::obj(v).is_some_and(|o| o.is_callable()) { Self::Func(v) } else { Self::Text(pattern(rt, v)?) })
    }
}

fn replace_with(rt: &mut Realm, this: Value, args: &[Value], all: bool) -> R<Value> {
    let (pat, rv) = (arg(args, 0), arg(args, 1));
    if Realm::obj(pat).is_some() {
        if all {
            check_global(rt, this, pat, "replaceAll")?;
        }
        if let Some(value) = string_hook(rt, this, pat, REPLACE, &[this, rv])? {
            return Ok(value);
        }
    }
    let s = rt.this_str(this, if all { "replaceAll" } else { "replace" })?;
    let s = rt.flat(s);
    let h = rt.units(s);
    let needle = pattern(rt, pat)?;
    let rep = Rep::new(rt, rv)?;
    let mut from = Some(0);
    let mut found = std::iter::from_fn(|| {
        let i = find(&h, &needle, from?)?;
        from = all.then(|| i + needle.len().max(1));
        Some([i as u32, (i + needle.len()) as u32])
    }).peekable();
    if found.peek().is_none() {
        return Ok(s.value());
    }
    replace_spans(rt, s, &h, rep, found)
}

pub(crate) fn fast_replace(rt: &mut Realm, re: Obj, s: Str, rep: Rep, flags: Flags) -> R<Value> {
    let h = rt.units(s);
    let mut found = Vec::new();
    let mut stride = 2;
    if flags.global {
        rt.set_last_index(re, 0)?;
    }
    while rt.regexp_run(re, s)? {
        let sp = rt.spans();
        stride = sp.len();
        found.extend(&sp);
        if !flags.global {
            break;
        }
        if sp[0] == sp[1] {
            let i = sp[1] as usize;
            rt.set_last_index(re, i + step(s, i, flags.unicode))?;
        }
    }
    if found.is_empty() {
        return Ok(s.value());
    }
    replace_spans(rt, s, &h, rep, found.chunks_exact(stride))
}

fn replace_spans<T: AsRef<[u32]>>(rt: &mut Realm, s: Str, h: &[u16], rep: Rep, found: impl Iterator<Item = T>) -> R<Value> {
    let mut out = Vec::with_capacity(h.len());
    let mut next = 0;
    for sp in found {
        let sp = sp.as_ref();
        let stride = sp.len();
        let (pos, end) = (sp[0] as usize, sp[1] as usize);
        out.extend(&h[next..pos]);
        match &rep {
            Rep::Text(t) => {
                expand(&mut out, t, h, pos, end, stride / 2 - 1, false, |out, g| {
                    if let Capture::Number(i) = g {
                        let a = sp[2 * i];
                        if a != NONE {
                            out.extend(&h[a as usize..sp[2 * i + 1] as usize]);
                        }
                    }
                    Ok::<_, std::convert::Infallible>(())
                }).unwrap();
            }
            Rep::Func(f) => {
                let mut a = Vec::with_capacity(stride / 2 + 2);
                for g in sp.chunks_exact(2) {
                    a.push(if g[0] == NONE { Value::UNDEFINED } else { rt.substring(s, g[0] as usize, g[1] as usize) });
                }
                a.push(Realm::num(pos as f64));
                a.push(s.value());
                let r = rt.call_function(*f, Value::UNDEFINED, &a)?;
                let r = rt.to_str(r)?;
                rt.push_units(&mut out, r);
            }
        }
        next = end;
    }
    out.extend(&h[next..]);
    replacement_string(rt, &out)
}

pub(crate) fn replacement_string(rt: &mut Realm, out: &[u16]) -> R<Value> {
    if out.len() > MAX_LEN {
        return Err(rt.error(ErrKind::Range, "invalid string length"));
    }
    Ok(rt.make_units(out))
}

pub(crate) fn replace(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    replace_with(rt, this, args, false)
}

pub(crate) fn replace_all(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    replace_with(rt, this, args, true)
}
