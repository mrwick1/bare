//! `JSON.parse` and `JSON.stringify` (plan step S2.7). Both read and write 16-bit units, so a lone surrogate and a string with
//! `\u0000` survive. `parse` makes plain objects and arrays with `Realm::define`, so the key `__proto__` is an own property and a
//! repeated key keeps the last value. Nesting is limited to [`MAX_DEPTH`] (a `RangeError`).
//!
//! `toJSON` of an object is called with `Realm::call_function` (the key is the second argument), so a `Date` serializes.
//!
//! **Known gaps** (each needs a call of a script function from a native, or a wrapper object): a `reviver`, a function `replacer`, a
//! getter on a value that `stringify` reads, and the unwrapping of `Number`, `String` and `Boolean` objects (there are none). Each of
//! the first three throws a `TypeError` that says so, and a wrapper object is serialized as a plain object.
use crate::lazy::{Ent, m};
use crate::bi_array::{get_at, len_of};
use crate::builtins::arg;
use crate::interp_rt::{ErrKind, R, Realm};
use crate::object::Obj;
use crate::value::Value;

/// The deepest nesting of arrays and objects that `parse` and `stringify` take.
const MAX_DEPTH: usize = 1000;

impl Realm {
    pub(crate) fn install_json(&mut self) {
        let j = Obj::new_plain(self);
        let g = self.global;
        self.hidden(g, "JSON", j.value());
        static JSON_METHODS: &[Ent] = &[m("parse", 2, parse), m("stringify", 3, stringify)];
        self.lazy_table(j, JSON_METHODS);
    }

    fn json_syntax(&mut self, msg: &str, at: usize) -> Value {
        self.error(ErrKind::Syntax, &format!("{msg} in JSON at position {at}"))
    }
}

fn callable(v: Value) -> bool {
    Realm::obj(v).is_some_and(|o| o.is_callable())
}

// ---- parse ----

struct Parser<'a> {
    s: &'a [u16],
    i: usize,
}

impl Parser<'_> {
    fn ws(&mut self) {
        while matches!(self.s.get(self.i), Some(0x20 | 0x09 | 0x0a | 0x0d)) {
            self.i += 1;
        }
    }

    fn fail<T>(&self, rt: &mut Realm) -> R<T> {
        Err(match self.s.get(self.i) {
            None => rt.error(ErrKind::Syntax, "Unexpected end of JSON input"),
            Some(&c) => rt.json_syntax(&format!("Unexpected token {}", char::from_u32(c as u32).unwrap_or('\u{fffd}')), self.i),
        })
    }

    fn lit(&mut self, rt: &mut Realm, word: &str, v: Value) -> R<Value> {
        for b in word.bytes() {
            if self.s.get(self.i) != Some(&(b as u16)) {
                return self.fail(rt);
            }
            self.i += 1;
        }
        Ok(v)
    }

    fn value(&mut self, rt: &mut Realm, depth: usize) -> R<Value> {
        self.ws();
        match self.s.get(self.i).copied() {
            Some(0x7b) => self.object(rt, depth),
            Some(0x5b) => self.array(rt, depth),
            Some(0x22) => {
                let u = self.string(rt)?;
                Ok(rt.make_units(&u))
            }
            Some(0x74) => self.lit(rt, "true", Value::TRUE),
            Some(0x66) => self.lit(rt, "false", Value::FALSE),
            Some(0x6e) => self.lit(rt, "null", Value::NULL),
            Some(0x2d | 0x30..=0x39) => self.number(rt),
            _ => self.fail(rt),
        }
    }

    fn number(&mut self, rt: &mut Realm) -> R<Value> {
        let start = self.i;
        if self.s[self.i] == 0x2d {
            self.i += 1;
        }
        let digits = |p: &mut Self| {
            let s = p.i;
            while matches!(p.s.get(p.i), Some(0x30..=0x39)) {
                p.i += 1;
            }
            p.i - s
        };
        match self.s.get(self.i) {
            Some(0x30) => self.i += 1,
            Some(0x31..=0x39) => {
                digits(self);
            }
            _ => return self.fail(rt),
        }
        if self.s.get(self.i) == Some(&0x2e) {
            self.i += 1;
            if digits(self) == 0 {
                return self.fail(rt);
            }
        }
        if matches!(self.s.get(self.i), Some(0x65 | 0x45)) {
            self.i += 1;
            if matches!(self.s.get(self.i), Some(0x2b | 0x2d)) {
                self.i += 1;
            }
            if digits(self) == 0 {
                return self.fail(rt);
            }
        }
        let t: String = self.s[start..self.i].iter().map(|&c| c as u8 as char).collect();
        Ok(Realm::num(t.parse::<f64>().unwrap_or(f64::NAN)))
    }

    /// A string literal at `"`: its units.
    fn string(&mut self, rt: &mut Realm) -> R<Vec<u16>> {
        self.i += 1;
        let mut out: Vec<u16> = Vec::new();
        loop {
            let Some(&c) = self.s.get(self.i) else { return self.fail(rt) };
            match c {
                0x22 => {
                    self.i += 1;
                    return Ok(out);
                }
                0x5c => {
                    self.i += 1;
                    let Some(&e) = self.s.get(self.i) else { return self.fail(rt) };
                    let u = match e {
                        0x22 | 0x5c | 0x2f => e,
                        0x62 => 8,
                        0x66 => 12,
                        0x6e => 10,
                        0x72 => 13,
                        0x74 => 9,
                        0x75 => {
                            let mut n = 0u16;
                            for k in 1..=4 {
                                match self.s.get(self.i + k).and_then(|&h| char::from_u32(h as u32)).and_then(|h| h.to_digit(16)) {
                                    Some(d) => n = n * 16 + d as u16,
                                    None => {
                                        self.i += k;
                                        return self.fail(rt);
                                    }
                                }
                            }
                            self.i += 4;
                            n
                        }
                        _ => return self.fail(rt),
                    };
                    out.push(u);
                    self.i += 1;
                }
                0..=0x1f => return self.fail(rt),
                _ => {
                    out.push(c);
                    self.i += 1;
                }
            }
        }
    }

    fn array(&mut self, rt: &mut Realm, depth: usize) -> R<Value> {
        if depth >= MAX_DEPTH {
            return Err(rt.error(ErrKind::Range, "JSON is nested too deeply"));
        }
        self.i += 1;
        let mut vals: Vec<Value> = Vec::new();
        self.ws();
        if self.s.get(self.i) == Some(&0x5d) {
            self.i += 1;
            return Ok(rt.new_array(&vals));
        }
        loop {
            vals.push(self.value(rt, depth + 1)?);
            self.ws();
            match self.s.get(self.i) {
                Some(0x2c) => self.i += 1,
                Some(0x5d) => {
                    self.i += 1;
                    return Ok(rt.new_array(&vals));
                }
                _ => return self.fail(rt),
            }
        }
    }

    fn object(&mut self, rt: &mut Realm, depth: usize) -> R<Value> {
        if depth >= MAX_DEPTH {
            return Err(rt.error(ErrKind::Range, "JSON is nested too deeply"));
        }
        self.i += 1;
        let o = Obj::new_plain(rt);
        self.ws();
        if self.s.get(self.i) == Some(&0x7d) {
            self.i += 1;
            return Ok(o.value());
        }
        loop {
            self.ws();
            if self.s.get(self.i) != Some(&0x22) {
                return self.fail(rt);
            }
            let k = self.string(rt)?;
            self.ws();
            if self.s.get(self.i) != Some(&0x3a) {
                return self.fail(rt);
            }
            self.i += 1;
            let v = self.value(rt, depth + 1)?;
            let kv = rt.make_units(&k);
            let key = rt.key_of(kv)?;
            rt.define(o, key, v)?;
            self.ws();
            match self.s.get(self.i) {
                Some(0x2c) => self.i += 1,
                Some(0x7d) => {
                    self.i += 1;
                    return Ok(o.value());
                }
                _ => return self.fail(rt),
            }
        }
    }
}

fn parse(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let t = rt.to_str(arg(args, 0))?;
    if callable(arg(args, 1)) {
        return rt.type_error("JSON.parse with a reviver needs a call of a script function from native code (not built yet)");
    }
    let u = rt.units(t);
    let mut p = Parser { s: &u, i: 0 };
    let v = p.value(rt, 0)?;
    p.ws();
    if p.i < u.len() {
        return p.fail(rt);
    }
    Ok(v)
}

// ---- stringify ----

struct Writer {
    out: Vec<u16>,
    gap: Vec<u16>,
    /// The `replacer` array: the keys to write, in order. `None` writes every own enumerable key.
    keys: Option<Vec<Value>>,
    /// The objects that are being written (to find a cycle).
    stack: Vec<Obj>,
}

fn push_ascii(out: &mut Vec<u16>, s: &str) {
    out.extend(s.bytes().map(u16::from));
}

/// `QuoteJSONString`: the units of `u` in quotes, with the escapes of the specification. A lone surrogate becomes `\udXXX`.
fn quote(out: &mut Vec<u16>, u: &[u16]) {
    out.push(0x22);
    let mut i = 0;
    while i < u.len() {
        let c = u[i];
        match c {
            0x22 => push_ascii(out, "\\\""),
            0x5c => push_ascii(out, "\\\\"),
            8 => push_ascii(out, "\\b"),
            12 => push_ascii(out, "\\f"),
            10 => push_ascii(out, "\\n"),
            13 => push_ascii(out, "\\r"),
            9 => push_ascii(out, "\\t"),
            0..=0x1f => push_ascii(out, &format!("\\u{c:04x}")),
            0xd800..=0xdbff if matches!(u.get(i + 1), Some(0xdc00..=0xdfff)) => {
                out.push(c);
                out.push(u[i + 1]);
                i += 1;
            }
            0xd800..=0xdfff => push_ascii(out, &format!("\\u{c:04x}")),
            _ => out.push(c),
        }
        i += 1;
    }
    out.push(0x22);
}

impl Writer {
    fn newline(&mut self, indent: &[u16]) {
        if !self.gap.is_empty() {
            self.out.push(10);
            self.out.extend(indent);
        }
    }

    /// `SerializeJSONProperty` for the value `v` (already read from its holder, under `key`: a string or a number, made a string only
    /// for a `toJSON` call). Returns false when nothing was written (`undefined`, a function), so the caller drops the key.
    fn value(&mut self, rt: &mut Realm, mut v: Value, key: Value, indent: &[u16]) -> R<bool> {
        if Realm::obj(v).is_some() {
            let to_json = rt.get_named(v, "toJSON")?;
            if callable(to_json) {
                let k = rt.to_str(key)?.value();
                v = rt.call_function(to_json, v, &[k])?;
            }
        }
        if let Some(o) = Realm::obj(v) {
            if o.is_callable() {
                return Ok(false);
            }
            return if rt.is_array(o) { self.array(rt, o, indent) } else { self.object(rt, o, indent) }.map(|()| true);
        }
        if v.is_null() {
            push_ascii(&mut self.out, "null");
        } else if let Some(b) = v.as_bool() {
            push_ascii(&mut self.out, if b { "true" } else { "false" });
        } else if let Some(s) = Realm::str_of(v) {
            let u = rt.units(s);
            quote(&mut self.out, &u);
        } else if let Some(f) = v.as_f64() {
            push_ascii(&mut self.out, &if f.is_finite() { Realm::num_to_string(f) } else { "null".to_string() });
        } else {
            return Ok(false);
        }
        Ok(true)
    }

    fn enter(&mut self, rt: &mut Realm, o: Obj) -> R<()> {
        if self.stack.iter().any(|p| p.cell() == o.cell()) {
            return rt.type_error("converting circular structure to JSON");
        }
        if self.stack.len() >= MAX_DEPTH {
            return Err(rt.error(ErrKind::Range, "JSON is nested too deeply"));
        }
        self.stack.push(o);
        Ok(())
    }

    fn array(&mut self, rt: &mut Realm, o: Obj, indent: &[u16]) -> R<()> {
        self.enter(rt, o)?;
        let inner: Vec<u16> = [indent, &self.gap].concat();
        let len = len_of(rt, o.value())?;
        self.out.push(0x5b);
        let mut i = 0.0;
        while i < len {
            if i > 0.0 {
                self.out.push(0x2c);
            }
            self.newline(&inner);
            let v = get_at(rt, o.value(), i)?;
            if !self.value(rt, v, Realm::num(i), &inner)? {
                push_ascii(&mut self.out, "null");
            }
            i += 1.0;
        }
        if len > 0.0 {
            self.newline(indent);
        }
        self.out.push(0x5d);
        self.stack.pop();
        Ok(())
    }

    fn object(&mut self, rt: &mut Realm, o: Obj, indent: &[u16]) -> R<()> {
        self.enter(rt, o)?;
        let inner: Vec<u16> = [indent, &self.gap].concat();
        let keys = match &self.keys {
            Some(k) => k.clone(),
            None => rt.own_key_list_h(o, true)?,
        };
        self.out.push(0x7b);
        let mut any = false;
        for k in keys {
            let key = rt.key_of(k)?;
            let v = rt.get_prop(o.value(), key)?;
            let mark = self.out.len();
            if any {
                self.out.push(0x2c);
            }
            self.newline(&inner);
            let ks = rt.to_str(k)?;
            let ku = rt.units(ks);
            quote(&mut self.out, &ku);
            self.out.push(0x3a);
            if !self.gap.is_empty() {
                self.out.push(0x20);
            }
            if self.value(rt, v, k, &inner)? {
                any = true;
            } else {
                self.out.truncate(mark);
            }
        }
        if any {
            self.newline(indent);
        }
        self.out.push(0x7d);
        self.stack.pop();
        Ok(())
    }
}

fn stringify(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let (replacer, space) = (arg(args, 1), arg(args, 2));
    let mut keys = None;
    if callable(replacer) {
        return rt.type_error("JSON.stringify with a replacer function needs a call of a script function from native code (not built yet)");
    }
    if let Some(r) = Realm::obj(replacer).filter(|r| rt.is_array(*r)) {
        let n = len_of(rt, r.value())?;
        let mut list: Vec<Value> = Vec::new();
        let mut i = 0.0;
        while i < n {
            let e = get_at(rt, r.value(), i)?;
            let item = if Realm::str_of(e).is_some() {
                Some(e)
            } else if e.is_number() {
                Some(rt.to_str(e)?.value())
            } else {
                None
            };
            if let Some(it) = item {
                let (a, b) = (Realm::str_of(it).expect("a string"), it);
                let au = rt.units(a);
                let mut dup = false;
                for &x in &list {
                    let xs = Realm::str_of(x).expect("a string");
                    if rt.units(xs) == au {
                        dup = true;
                        break;
                    }
                }
                if !dup {
                    list.push(b);
                }
            }
            i += 1.0;
        }
        keys = Some(list);
    }
    let gap: Vec<u16> = if let Some(n) = space.as_f64() {
        let n = if n.is_nan() { 0.0 } else { n.trunc().min(10.0) };
        vec![0x20; n.max(0.0) as usize]
    } else if let Some(s) = Realm::str_of(space) {
        rt.units(s).into_iter().take(10).collect()
    } else {
        Vec::new()
    };
    let mut w = Writer { out: Vec::new(), gap, keys, stack: Vec::new() };
    let key = rt.string("");
    if w.value(rt, arg(args, 0), key, &[])? { Ok(rt.make_units(&w.out)) } else { Ok(Value::UNDEFINED) }
}
