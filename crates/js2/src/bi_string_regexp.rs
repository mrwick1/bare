//! `String.prototype` methods that take a `RegExp` (design-js2-regexp, step R3): `match`, `matchAll`, `search`, `replace`, `replaceAll`
//! and the `RegExp` branch of `split`. `replace` and `replaceAll` also do the string pattern and the function replacer (a nested call
//! through [`Realm::call_function`]).
//!
//! **A fast path only.** An argument that is a regexp object (kind `K_REGEXP`) is matched by the runtime of `regexp_rt.rs` (`regexp_run`,
//! `run_at`) with no property read. A script that gives a regexp its own `exec` or `Symbol.replace`, or that changes
//! `RegExp.prototype`, is not seen: that is the generic path of the design (section 7.5), which is not built. Known gaps: no named
//! groups (`$<name>` stays literal and a replacer has no `groups` argument), and `matchAll` runs all the matches at the call and
//! returns an array iterator (the tag is `Array Iterator`, not `RegExp String Iterator`).
use crate::bi_string::{find, pattern};
use crate::builtins::arg;
use crate::interp_rt::{R, Realm};
use crate::object::{K_REGEXP, Obj};
use crate::string::{Chars, Str};
use crate::value::Value;

const NONE: u32 = u32::MAX;

/// The regexp object of `v`, if it is one.
fn regexp_of(v: Value) -> Option<Obj> {
    Realm::obj(v).filter(|o| o.is_kind(K_REGEXP))
}

/// `AdvanceStringIndex`: how far an empty match moves the index in `s` at `i`.
fn step(s: Str, i: usize, unicode: bool) -> usize {
    match s.chars() {
        Some(Chars::Two(u)) if unicode && matches!(u.get(i), Some(0xD800..=0xDBFF)) && matches!(u.get(i + 1), Some(0xDC00..=0xDFFF)) => 2,
        _ => 1,
    }
}

/// After an empty match of the regexp `re`, move its `lastIndex` on by one code unit (one code point with `u`).
fn advance(rt: &mut Realm, re: Obj, s: Str, unicode: bool) -> R<()> {
    let li = rt.last_index(re)? as usize;
    rt.set_last_index(re, li + step(s, li, unicode))
}

/// The span of group 0 of the last match.
fn span(rt: &mut Realm) -> (usize, usize) {
    let sp = rt.spans();
    (sp[0] as usize, sp[1] as usize)
}

pub(crate) fn match_(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let s = rt.this_str(this, "match")?;
    let s = rt.flat(s);
    let re = rt.regexp_for(arg(args, 0), "")?;
    let flags = rt.regexp_flags_of(re)?;
    if !flags.global {
        return if rt.regexp_run(re, s)? { rt.match_result(s) } else { Ok(Value::NULL) };
    }
    rt.set_last_index(re, 0)?;
    let mut out = Vec::new();
    while rt.regexp_run(re, s)? {
        let (a, b) = span(rt);
        out.push(rt.substring(s, a, b));
        if a == b {
            advance(rt, re, s, flags.unicode)?;
        }
    }
    Ok(if out.is_empty() { Value::NULL } else { rt.new_array(&out) })
}

pub(crate) fn match_all(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let s = rt.this_str(this, "matchAll")?;
    let s = rt.flat(s);
    let re = match regexp_of(arg(args, 0)) {
        Some(o) => {
            if !rt.regexp_flags_of(o)?.global {
                return rt.type_error("String.prototype.matchAll called with a non-global RegExp argument");
            }
            rt.regexp_clone(o)?
        }
        None => rt.regexp_for(arg(args, 0), "g")?,
    };
    let unicode = rt.regexp_flags_of(re)?.unicode;
    let mut out = Vec::new();
    while rt.regexp_run(re, s)? {
        out.push(rt.match_result(s)?);
        let (a, b) = span(rt);
        if a == b {
            advance(rt, re, s, unicode)?;
        }
    }
    let list = rt.new_array(&out);
    let slot = crate::prelude_pre::PRE.iter().position(|p| p.0 == "arrayIterator").expect("a slot");
    rt.helper(slot, &[list, Value::int(1)])
}

pub(crate) fn search(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let s = rt.this_str(this, "search")?;
    let s = rt.flat(s);
    let re = rt.regexp_for(arg(args, 0), "")?;
    let prev = rt.last_index_value(re)?;
    if !rt.same_value(prev, Value::int(0)) {
        rt.put_last_index(re, Value::int(0))?;
    }
    let hit = rt.regexp_run(re, s)?;
    let cur = rt.last_index_value(re)?;
    if !rt.same_value(cur, prev) {
        rt.put_last_index(re, prev)?;
    }
    Ok(Value::int(if hit { span(rt).0 as i32 } else { -1 }))
}

/// `RegExp.prototype[Symbol.split]` for a regexp object `re`: `lim` is `ToUint32(limit)`.
pub(crate) fn split(rt: &mut Realm, re: Obj, s: Str, lim: u32) -> R<Value> {
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

/// The replacement text for the match `sp` (the spans of its groups) in `h`, added to `out`: `$$`, `$&`, `` $` ``, `$'`, `$n` and `$nn`.
fn expand(out: &mut Vec<u16>, rep: &[u16], h: &[u16], sp: &[u32]) {
    let (at, end, m) = (sp[0] as usize, sp[1] as usize, sp.len() / 2 - 1);
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
            0x26 => out.extend(&h[at..end]),
            0x60 => out.extend(&h[..at]),
            0x27 => out.extend(&h[end..]),
            _ => {
                let Some(d1) = digit(next) else {
                    out.push(c);
                    continue;
                };
                // `$nn` when nn is a group, else `$n`; `$0` and a group that does not exist stay literal.
                let two = rep.get(i + 1).copied().and_then(digit).map(|d2| d1 * 10 + d2).filter(|n| (1..=m).contains(n));
                let (n, used) = two.map_or((d1, 1), |n| (n, 2));
                if !(1..=m).contains(&n) {
                    out.push(c);
                    continue;
                }
                if sp[2 * n] != NONE {
                    out.extend(&h[sp[2 * n] as usize..sp[2 * n + 1] as usize]);
                }
                i += used;
                continue;
            }
        }
        i += 1;
    }
}

/// What a match is replaced with.
enum Rep {
    Text(Vec<u16>),
    Func(Value),
}

fn replace_with(rt: &mut Realm, this: Value, args: &[Value], all: bool) -> R<Value> {
    let s = rt.this_str(this, if all { "replaceAll" } else { "replace" })?;
    let s = rt.flat(s);
    let h = rt.units(s);
    let (pat, rv) = (arg(args, 0), arg(args, 1));
    let re = regexp_of(pat);
    let flags = match re {
        Some(o) => rt.regexp_flags_of(o)?,
        None => Default::default(),
    };
    if all && re.is_some() && !flags.global {
        return rt.type_error("String.prototype.replaceAll called with a non-global RegExp argument");
    }
    let needle = if re.is_none() { Some(pattern(rt, pat)?) } else { None };
    let rep = if Realm::obj(rv).is_some_and(|o| o.is_callable()) { Rep::Func(rv) } else { Rep::Text(pattern(rt, rv)?) };
    // The spans of every match, `stride` words each.
    let mut found: Vec<u32> = Vec::new();
    let mut stride = 2;
    if let Some(n) = &needle {
        let mut from = 0;
        while let Some(i) = find(&h, n, from) {
            found.extend([i as u32, (i + n.len()) as u32]);
            if !all {
                break;
            }
            from = i + n.len().max(1);
        }
    } else if let Some(re) = re {
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
                advance(rt, re, s, flags.unicode)?;
            }
        }
    }
    let mut out = Vec::with_capacity(h.len());
    let mut next = 0;
    for sp in found.chunks(stride) {
        let (pos, end) = (sp[0] as usize, sp[1] as usize);
        if pos < next {
            continue;
        }
        out.extend(&h[next..pos]);
        match &rep {
            Rep::Text(t) => expand(&mut out, t, &h, sp),
            Rep::Func(f) => {
                let mut a = Vec::with_capacity(stride / 2 + 2);
                for g in sp.chunks(2) {
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
    Ok(rt.make_units(&out))
}

pub(crate) fn replace(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    replace_with(rt, this, args, false)
}

pub(crate) fn replace_all(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    replace_with(rt, this, args, true)
}
