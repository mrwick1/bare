//! RegExp symbol methods. Each generic step uses script-visible property access and coercion.
use super::{LAST, NativeFn, Obj, Realm, Value, arg};
use crate::bi_string_regexp::{Capture, Rep, expand, fast_replace, fast_split, replacement_string, step};
use crate::bi_symbol::{MATCH, MATCH_ALL, REPLACE, SEARCH, SPECIES, SPLIT, TO_STRING_TAG};
use crate::cell::Layout;
use crate::interp::STOP;
use crate::interp_rt::{Key, R};
use crate::shapes::{A_C, ShapeId};
use crate::string::Str;

pub(super) const METHODS: [(usize, &str, u8, NativeFn); 5] = [
    (MATCH, "[Symbol.match]", 1, rx_match),
    (MATCH_ALL, "[Symbol.matchAll]", 1, rx_match_all),
    (REPLACE, "[Symbol.replace]", 2, rx_replace),
    (SEARCH, "[Symbol.search]", 1, rx_search),
    (SPLIT, "[Symbol.split]", 2, rx_split),
];

fn require_coercible(rt: &mut Realm, v: Value) -> R<()> {
    if v.is_undefined() || v.is_null() {
        return rt.type_error("String method called on null or undefined");
    }
    Ok(())
}

fn get_flags(rt: &mut Realm, re: Value) -> R<Str> {
    let v = rt.get_named(re, "flags")?;
    let flags = rt.to_str(v)?;
    Ok(rt.flat(flags))
}

fn has_flag(s: Str, b: u8) -> bool {
    (0..s.len()).any(|i| s.unit_at(i) == Some(b as u16))
}

fn unicode(s: Str) -> bool {
    has_flag(s, b'u') || has_flag(s, b'v')
}

#[inline]
pub(crate) fn check_global(rt: &mut Realm, this: Value, pat: Value, name: &str) -> R<()> {
    require_coercible(rt, this)?;
    if !rt.is_regexp(pat)? {
        return Ok(());
    }
    let o = Realm::obj(pat).expect("RegExp");
    let global = if rt.plain_rx_flags(o) {
        rt.plain_flags(o).global
    } else {
        let flags = rt.get_named(pat, "flags")?;
        require_coercible(rt, flags)?;
        let flags = rt.to_str(flags)?;
        has_flag(rt.flat(flags), b'g')
    };
    if !global {
        return rt.type_error(&format!("String.prototype.{name} called with a non-global RegExp"));
    }
    Ok(())
}

/// GetMethod on an object pattern, then Call with the String method's original arguments.
#[inline]
pub(crate) fn string_hook(rt: &mut Realm, this: Value, pat: Value, symbol: usize, args: &[Value]) -> R<Option<Value>> {
    require_coercible(rt, this)?;
    let Some(o) = Realm::obj(pat) else { return Ok(None) };
    if rt.plain_rx_method(o, symbol) {
        return direct(rt, pat, symbol, args).map(Some);
    }
    let method = rt.get_prop(pat, Key::Name(rt.wk[symbol]))?;
    if method.is_undefined() || method.is_null() {
        Ok(None)
    } else {
        rt.call_function(method, pat, args).map(Some)
    }
}

fn direct(rt: &mut Realm, pat: Value, symbol: usize, args: &[Value]) -> R<Value> {
    (METHODS[symbol - MATCH].3)(rt, pat, args)
}

impl Realm {
    pub(crate) fn invoke_rx_symbol(&mut self, re: Value, symbol: usize, args: &[Value]) -> R<Value> {
        if self.plain_rx_method(Realm::obj(re).expect("RegExp"), symbol) {
            return direct(self, re, symbol, args);
        }
        let method = self.get_prop(re, Key::Name(self.wk[symbol]))?;
        self.call_function(method, re, args)
    }

    fn rx_species_constructor(&mut self, re: Value) -> R<Value> {
        let default = self.rx().ctor.value();
        let ctor = self.get_named(re, "constructor")?;
        if ctor.is_undefined() {
            return Ok(default);
        }
        if Realm::obj(ctor).is_none() {
            return self.type_error("RegExp constructor property must be an object");
        }
        let species = self.get_prop(ctor, Key::Name(self.wk[SPECIES]))?;
        if species.is_undefined() || species.is_null() {
            Ok(default)
        } else if self.is_constructor(species) {
            Ok(species)
        } else {
            self.type_error("RegExp species is not a constructor")
        }
    }
}

fn receiver(rt: &mut Realm, this: Value, args: &[Value]) -> R<(Obj, Str)> {
    let Some(o) = Realm::obj(this) else { return rt.type_error("RegExp method requires an object") };
    let s = rt.to_str(arg(args, 0))?;
    Ok((o, rt.flat(s)))
}

fn advance(rt: &mut Realm, re: Obj, s: Str, unicode: bool) -> R<()> {
    let v = rt.last_index_value(re)?;
    let i = rt.to_length(v)?;
    let n = if i < s.len() as f64 { step(s, i as usize, unicode) } else { 1 };
    rt.put_last_index(re, Realm::num(i + n as f64))
}

fn tick(rt: &mut Realm) -> R<()> {
    let Some(fuel) = rt.fuel.checked_sub(1) else { return Err(STOP) };
    rt.fuel = fuel;
    Ok(())
}

fn rx_match(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let (re, s) = receiver(rt, this, args)?;
    if rt.plain_rx_exec(re) && rt.plain_rx_flags(re) {
        let flags = rt.plain_flags(re);
        if !flags.global {
            return rt.rx_builtin_exec(re, s);
        }
        rt.set_last_index(re, 0)?;
        let mut out = Vec::new();
        while rt.regexp_run(re, s)? {
            let (a, b) = (rt.rx().out[0] as usize, rt.rx().out[1] as usize);
            out.push(rt.substring(s, a, b));
            if a == b {
                rt.set_last_index(re, b + step(s, b, flags.unicode))?;
            }
        }
        return Ok(if out.is_empty() { Value::NULL } else { rt.new_array(&out) });
    }
    let flags = get_flags(rt, this)?;
    if !has_flag(flags, b'g') {
        return rt.rx_exec(re, s);
    }
    let unicode = unicode(flags);
    let mut out = Vec::new();
    rt.put_last_index(re, Value::int(0))?;
    loop {
        tick(rt)?;
        let result = rt.rx_exec(re, s)?;
        if result.is_null() {
            return Ok(if out.is_empty() { Value::NULL } else { rt.new_array(&out) });
        }
        let matched = rt.get_prop(result, Key::Idx(0))?;
        let m = rt.to_str(matched)?;
        out.push(m.value());
        if m.len() == 0 {
            advance(rt, re, s, unicode)?;
        }
    }
}

fn rx_search(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let (re, s) = receiver(rt, this, args)?;
    let zero = Value::int(0);
    if rt.plain_rx_exec(re) {
        let prev = re.inline_slot(LAST);
        if !rt.same_value(prev, zero) {
            re.set_inline_slot(LAST, zero);
        }
        let matched = rt.regexp_run(re, s)?;
        re.set_inline_slot(LAST, prev);
        return Ok(Value::int(if matched { rt.rx().out[0] as i32 } else { -1 }));
    }
    let prev = rt.last_index_value(re)?;
    if !rt.same_value(prev, zero) {
        rt.put_last_index(re, zero)?;
    }
    let result = rt.rx_exec(re, s)?;
    let cur = rt.last_index_value(re)?;
    if !rt.same_value(cur, prev) {
        rt.put_last_index(re, prev)?;
    }
    if result.is_null() { Ok(Value::int(-1)) } else { rt.get_named(result, "index") }
}

pub(crate) fn split_limit(rt: &mut Realm, v: Value) -> R<u32> {
    Ok(if v.is_undefined() { u32::MAX } else { Realm::to_int32(rt.to_number(v)?) as u32 })
}

fn rx_split(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let (re, s) = receiver(rt, this, args)?;
    let limit = arg(args, 1);
    if Realm::obj(limit).is_none() && rt.plain_rx_exec(re) && rt.plain_rx_flags(re) && rt.plain_rx_species(re) {
        let limit = split_limit(rt, limit)?;
        return fast_split(rt, re, s, limit);
    }
    let ctor = rt.rx_species_constructor(this)?;
    let flags = get_flags(rt, this)?;
    let unicode = unicode(flags);
    let flags = if has_flag(flags, b'y') {
        flags.value()
    } else {
        let mut units = rt.units(flags);
        units.push(b'y' as u16);
        replacement_string(rt, &units)?
    };
    let splitter = rt.construct_value(ctor, &[this, flags])?;
    let re = Realm::obj(splitter).expect("constructor result");
    let limit = split_limit(rt, limit)? as usize;
    if limit == 0 {
        return Ok(rt.new_array(&[]));
    }
    if s.len() == 0 {
        let result = rt.rx_exec(re, s)?;
        return Ok(if result.is_null() { rt.new_array(&[s.value()]) } else { rt.new_array(&[]) });
    }
    let mut out = Vec::new();
    let (mut p, mut q) = (0, 0);
    while q < s.len() {
        tick(rt)?;
        rt.put_last_index(re, Realm::num(q as f64))?;
        let result = rt.rx_exec(re, s)?;
        if result.is_null() {
            q += step(s, q, unicode);
            continue;
        }
        let last = rt.last_index_value(re)?;
        let e = rt.to_length(last)?.min(s.len() as f64) as usize;
        if e == p {
            q += step(s, q, unicode);
            continue;
        }
        out.push(rt.substring(s, p, q));
        if out.len() == limit {
            return Ok(rt.new_array(&out));
        }
        p = e;
        let len = rt.get_named(result, "length")?;
        let count = rt.to_length(len)? as u64;
        for i in 1..count {
            tick(rt)?;
            out.push(indexed(rt, result, i)?);
            if out.len() == limit {
                return Ok(rt.new_array(&out));
            }
        }
        q = p;
    }
    out.push(rt.substring(s, p, s.len()));
    Ok(rt.new_array(&out))
}

fn indexed(rt: &mut Realm, result: Value, i: u64) -> R<Value> {
    let key = if i < u32::MAX as u64 { Key::Idx(i as u32) } else { Key::Name(rt.intern(&i.to_string())) };
    rt.get_prop(result, key)
}

fn rx_replace(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let (re, s) = receiver(rt, this, args)?;
    let rep = Rep::new(rt, arg(args, 1))?;
    if rt.plain_rx_exec(re) && rt.plain_rx_flags(re) && !crate::regexp::image_extra_groups(rt.program(re)?.words()) {
        return fast_replace(rt, re, s, rep, rt.plain_flags(re));
    }
    let flags = get_flags(rt, this)?;
    let global = has_flag(flags, b'g');
    let unicode = unicode(flags);
    if global {
        rt.put_last_index(re, Value::int(0))?;
    }
    let mut matches = Vec::new();
    loop {
        tick(rt)?;
        let result = rt.rx_exec(re, s)?;
        if result.is_null() {
            break;
        }
        matches.push(result);
        if !global {
            break;
        }
        let matched = rt.get_prop(result, Key::Idx(0))?;
        if rt.to_str(matched)?.len() == 0 {
            advance(rt, re, s, unicode)?;
        }
    }
    if matches.is_empty() {
        return Ok(s.value());
    }
    let h = rt.units(s);
    let mut out = Vec::with_capacity(h.len());
    let mut next = 0;
    for result in matches {
        let len = rt.get_named(result, "length")?;
        let count = rt.to_length(len)? as u64;
        let matched = rt.get_prop(result, Key::Idx(0))?;
        let matched = rt.to_str(matched)?;
        let index = rt.get_named(result, "index")?;
        let pos = (rt.to_number(index)?.trunc() as usize).min(h.len());
        let mut captures = vec![matched.value()];
        for i in 1..count {
            tick(rt)?;
            let v = indexed(rt, result, i)?;
            captures.push(if v.is_undefined() { v } else { rt.to_str(v)?.value() });
        }
        let groups = rt.get_named(result, "groups")?;
        let end = pos + matched.len();
        let mut replacement = Vec::new();
        match &rep {
            Rep::Text(text) => {
                let groups = if groups.is_undefined() { None } else { Some(rt.to_object(groups)?) };
                expand(&mut replacement, text, &h, pos, end.min(h.len()), captures.len() - 1, groups.is_some(), |out, capture| {
                    let v = match capture {
                        Capture::Number(i) => captures[i],
                        Capture::Name(units) => {
                            let name = rt.make_units(units);
                            let key = rt.key_of(name)?;
                            rt.get_prop(groups.expect("named captures").value(), key)?
                        }
                    };
                    if !v.is_undefined() {
                        let s = rt.to_str(v)?;
                        rt.push_units(out, s);
                    }
                    Ok(())
                })?;
            }
            Rep::Func(f) => {
                captures.push(Realm::num(pos as f64));
                captures.push(s.value());
                if !groups.is_undefined() {
                    captures.push(groups);
                }
                let r = rt.call_function(*f, Value::UNDEFINED, &captures)?;
                let r = rt.to_str(r)?;
                rt.push_units(&mut replacement, r);
            }
        }
        if pos >= next {
            out.extend(&h[next..pos]);
            out.extend(replacement);
            next = end;
        }
    }
    out.extend(&h[next.min(h.len())..]);
    replacement_string(rt, &out)
}

// The iterator stores its state in the same object slots as other built-in iterators.
const F_RX_ITER: u32 = 1 << 22;

impl Realm {
    fn rx_iterator_shape(&mut self) -> ShapeId {
        if let Some(shape) = self.rx().iterator_shape {
            return shape;
        }
        let base = Realm::obj(self.iterator_prototype()).expect("iterator prototype");
        let shape = self.shapes.root(Layout::Object, Some(base.cell()), 4);
        let proto = Obj::new(&mut self.heap, &self.shapes, shape).expect("memory");
        self.method(proto, "next", 0, rx_iterator_next);
        let tag = self.string("RegExp String Iterator");
        self.define_attrs(proto, Key::Name(self.wk[TO_STRING_TAG]), A_C, tag).expect("define");
        let mut shape = self.shapes.root(Layout::Object, Some(proto.cell()), 3);
        for n in ["\0rxMatcher", "\0rxInput", "\0rxFlags"] {
            let k = self.intern(n);
            shape = self.shapes.add(shape, k, 0).expect("shape");
        }
        self.keep(proto.value());
        self.pins.push(shape);
        self.rx().iterator_shape = Some(shape);
        shape
    }
}

fn rx_match_all(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let (re, s) = receiver(rt, this, args)?;
    let ctor = rt.rx_species_constructor(this)?;
    let flags = get_flags(rt, this)?;
    let matcher = rt.construct_value(ctor, &[this, flags.value()])?;
    let v = rt.last_index_value(re)?;
    let index = rt.to_length(v)?;
    rt.put_last_index(Realm::obj(matcher).expect("constructor result"), Realm::num(index))?;
    let shape = rt.rx_iterator_shape();
    let o = Obj::new(&mut rt.heap, &rt.shapes, shape).expect("memory");
    o.set_flag(F_RX_ITER);
    o.set_slot(&rt.shapes, 0, matcher);
    o.set_slot(&rt.shapes, 1, s.value());
    o.set_slot(&rt.shapes, 2, Value::int(has_flag(flags, b'g') as i32 | (unicode(flags) as i32) << 1));
    Ok(o.value())
}

fn rx_iterator_next(rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    let Some(o) = Realm::obj(this).filter(|o| o.has_flag(F_RX_ITER)) else {
        return rt.type_error("RegExp String Iterator.prototype.next called on incompatible receiver");
    };
    let Some(re) = Realm::obj(o.slot(&rt.shapes, 0)) else {
        return Ok(rt.iter_result(Value::UNDEFINED, true));
    };
    let s = Realm::str_of(o.slot(&rt.shapes, 1)).expect("input string");
    let flags = o.slot(&rt.shapes, 2).as_int().expect("iterator flags");
    let result = rt.rx_exec(re, s)?;
    if result.is_null() || flags & 1 == 0 {
        o.set_slot(&rt.shapes, 0, Value::UNDEFINED);
        o.set_slot(&rt.shapes, 1, Value::UNDEFINED);
    } else {
        let matched = rt.get_prop(result, Key::Idx(0))?;
        if rt.to_str(matched)?.len() == 0 {
            advance(rt, re, s, flags & 2 != 0)?;
        }
    }
    let done = result.is_null();
    Ok(rt.iter_result(if done { Value::UNDEFINED } else { result }, done))
}
