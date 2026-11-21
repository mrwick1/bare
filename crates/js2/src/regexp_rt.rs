//! `RegExp` for the realm (design-js2-regexp, step R1): the object, the constructor, `exec`, `test` and the cheap getters. Safe code
//! over [`Realm`]; the matcher is `regexp.rs` and the compiled program is a [`Blob`] cell.
//!
//! **Lazy.** The realm has no `RegExp` at start: `Realm::rx` is `None`, and a startup costs nothing for it. The first regex literal
//! (`Op::NewRegExp`) or the first read of the global name `RegExp` that misses (`get_global_slow`) makes the constructor and the
//! prototype ([`Realm::install_regexp`]). Known gap: `globalThis.RegExp`, `hasOwnProperty("RegExp")` and a list of the global
//! properties do not see it before that first use.
//!
//! **The object.** A plain `Object` cell with the flag `F_REGEXP` and four slots, in this order: the hidden `\0src` (the pattern
//! string), the hidden `\0fl` (the flag bits of `Flags::bits`), the hidden `\0prog` (the [`Blob`] of the program, or `undefined`
//! until the first match) and the own `lastIndex` (writable only). All objects start with one shape, `Rx::shape`. An object that a
//! script gave another property has another shape, and the slots are found by name then.
//!
//! **Compile.** The constructor checks the syntax (`regexp::check`), so a bad pattern throws at once. The program is compiled at the
//! first `exec` or `test`. A small cache keeps the last programs by (source cell, flags), so a literal in a loop compiles once.
//!
//! **Steps.** The matcher charges its steps to `Realm::fuel`. When the fuel is out, or the run passes the cap of one run, the native
//! returns `Err(STOP)`: no handler runs, and the script ends with `Exit::Fuel` (the old engine's `Error::Limit` was as final).
use crate::blob::Blob;
use crate::builtins::{NativeFn, arg};
use crate::gc::Marker;
use crate::interp::STOP;
use crate::interp_rt::{ErrKind, Key, R, Realm};
use crate::object::{K_REGEXP, Obj};
use crate::regexp::{self, CompileError, ExecError, FLAG_LETTERS, Flags, Scratch};
use crate::shapes::{A_W, ShapeId};
use crate::string::{Chars, Str};
use crate::value::Value;

/// How many compiled programs the cache keeps.
const CACHE: usize = 32;

/// The state of `RegExp` in a realm. The collector marks it ([`Rx::mark`]).
pub struct Rx {
    proto: Obj,
    shape: ShapeId,
    /// The property names `\0src`, `\0fl`, `\0prog`, `lastIndex`, `index`, `input`, `groups`.
    keys: [Str; 7],
    scratch: Scratch,
    out: Vec<u32>,
    /// The words of `out` that the last match filled: two for each group, from group 0.
    nspans: usize,
    /// (source cell, flag bits, program), the newest last.
    cache: Vec<(Value, u8, Value)>,
}

/// The flag getters, in the order of the bits of `Flags::bits`.
const FLAG_NAMES: [&str; 8] = ["hasIndices", "global", "ignoreCase", "multiline", "dotAll", "unicode", "unicodeSets", "sticky"];
/// The names of their functions.
const GETTER_NAMES: [&str; 8] = ["get hasIndices", "get global", "get ignoreCase", "get multiline", "get dotAll", "get unicode", "get unicodeSets", "get sticky"];

const SRC: usize = 0;
const FL: usize = 1;
const PROG: usize = 2;
const LAST: usize = 3;
const INDEX: usize = 4;
const INPUT: usize = 5;
const GROUPS: usize = 6;

impl Rx {
    pub(crate) fn mark(&self, m: &mut Marker) {
        m.value(self.proto.value());
        m.shape(self.shape);
        self.keys.iter().for_each(|k| m.value(k.value()));
        self.cache.iter().for_each(|c| {
            m.value(c.0);
            m.value(c.2);
        });
    }
}

impl Realm {
    /// Make `RegExp` and `RegExp.prototype` if they are not made yet.
    pub(crate) fn rx(&mut self) -> &mut Rx {
        if self.rx.is_none() {
            self.install_regexp();
        }
        self.rx.as_mut().expect("installed")
    }

    /// Is `name` the global name `RegExp` that is not made yet? (The miss path of a global read.)
    pub(crate) fn is_lazy_regexp(&self, name: Str) -> bool {
        self.rx.is_none() && name.len() == 6 && self.text(name) == "RegExp"
    }

    #[inline(never)]
    pub(crate) fn install_regexp(&mut self) {
        let proto = Obj::new(&mut self.heap, &self.shapes, self.s_object).expect("memory");
        self.keep(proto.value());
        let ks = ["\0src", "\0fl", "\0prog", "lastIndex", "index", "input", "groups"].map(|n| self.intern(n));
        let root = self.shapes.root(crate::cell::Layout::Object, Some(proto.cell()), 4);
        let mut shape = root;
        for (i, k) in ks[..4].iter().enumerate() {
            shape = self.shapes.add(shape, *k, if i == LAST { A_W } else { 0 }).expect("shape");
        }
        self.rx = Some(Box::new(Rx { proto, shape, keys: ks, scratch: Scratch::new(), out: Vec::new(), nspans: 0, cache: Vec::new() }));
        self.ctor("RegExp", 2, regexp_ctor, proto);
        self.method(proto, "exec", 1, regexp_exec);
        self.method(proto, "test", 1, regexp_test);
        self.method(proto, "toString", 0, regexp_to_string);
        self.getter(proto, "source", "get source", regexp_source);
        self.getter(proto, "flags", "get flags", regexp_flags);
        for (n, g) in FLAG_NAMES.into_iter().zip(GETTER_NAMES) {
            self.getter(proto, n, g, regexp_flag);
        }
    }

    /// An accessor property `name` of `o` with a getter and no setter.
    fn getter(&mut self, o: Obj, name: &str, fname: &'static str, f: NativeFn) {
        let k = self.intern(name);
        let g = self.new_native(f, false, 0, fname);
        self.define_accessor(o, Key::Name(k), g.value(), false, false).expect("define");
    }

    /// A new `RegExp` object. The syntax of `src` and `flags` is checked.
    pub(crate) fn new_regexp(&mut self, proto: Option<Obj>, src: Str, flags: &str) -> R<Value> {
        let src = self.flat(src);
        let units = self.units(src);
        if let Err(e) = regexp::check(&units, flags) {
            return Err(self.regexp_error(&e, src));
        }
        let bits = Flags::parse(flags).map_or(0, Flags::bits);
        let rx = self.rx();
        let shape = match proto {
            None => rx.shape,
            Some(p) if p == rx.proto => rx.shape,
            Some(p) => {
                // A class that extends `RegExp`: the same four slots over another prototype.
                let keys = [rx.keys[SRC], rx.keys[FL], rx.keys[PROG], rx.keys[LAST]];
                let mut s = self.shapes.root(crate::cell::Layout::Object, Some(p.cell()), 4);
                for (i, k) in keys.iter().enumerate() {
                    s = self.shapes.add(s, *k, if i == LAST { A_W } else { 0 }).expect("shape");
                }
                s
            }
        };
        let o = Obj::new(&mut self.heap, &self.shapes, shape).expect("memory");
        o.set_kind(K_REGEXP);
        o.set_slot(&self.shapes, SRC as u32, src.value());
        o.set_slot(&self.shapes, FL as u32, Value::int(bits as i32));
        o.set_slot(&self.shapes, PROG as u32, Value::UNDEFINED);
        o.set_slot(&self.shapes, LAST as u32, Value::int(0));
        Ok(o.value())
    }

    /// The object of `Op::NewRegExp`: `src` and `flags` are atoms.
    pub(crate) fn regexp_literal(&mut self, src: Str, flags: Str) -> R<Value> {
        let f = self.text(flags);
        self.new_regexp(None, src, &f)
    }

    fn regexp_error(&mut self, e: &CompileError, src: Str) -> Value {
        let s = self.text(src);
        match *e {
            CompileError::InvalidFlags(c) => self.error(ErrKind::Syntax, &format!("Invalid flags supplied to RegExp constructor '{c}'")),
            CompileError::Syntax { msg, .. } => self.error(ErrKind::Syntax, &format!("Invalid regular expression: /{s}/: {msg}")),
            CompileError::Unsupported { what, .. } => self.error(ErrKind::Syntax, &format!("Invalid regular expression: /{s}/: unsupported: {what}")),
            CompileError::Limit(what) => self.error(ErrKind::Syntax, &format!("Invalid regular expression: /{s}/: {what}")),
        }
    }

    /// The slot of the hidden property `which` of the regexp `o`.
    fn rx_slot(&mut self, o: Obj, which: usize) -> Option<u32> {
        let rx = self.rx.as_ref()?;
        if o.shape(&self.shapes) == rx.shape {
            return Some(which as u32);
        }
        // Another shape: look the name up. A script can delete a hidden property, so the answer may be none.
        let k = rx.keys[which];
        self.shapes.find(o.shape(&self.shapes), k).map(|e| e.slot())
    }

    /// The regexp object of `this`, or a `TypeError`.
    fn this_regexp(&mut self, this: Value, what: &str) -> R<Obj> {
        match Realm::obj(this).filter(|o| o.is_kind(K_REGEXP)) {
            Some(o) if self.rx_slot(o, SRC).is_some() => Ok(o),
            _ => self.type_error(&format!("RegExp.prototype.{what} requires that 'this' be a RegExp object")),
        }
    }

    /// The program of the regexp `o`: compiled now if it is not.
    fn program(&mut self, o: Obj) -> R<Blob> {
        let Some(ps) = self.rx_slot(o, PROG) else { return self.type_error("invalid RegExp object") };
        if let Some(b) = Realm::blob_of(o.slot(&self.shapes, ps)) {
            return Ok(b);
        }
        let (Some(ss), Some(fs)) = (self.rx_slot(o, SRC), self.rx_slot(o, FL)) else { return self.type_error("invalid RegExp object") };
        let (Some(src), Some(bits)) = (Realm::str_of(o.slot(&self.shapes, ss)), o.slot(&self.shapes, fs).as_int()) else {
            return self.type_error("invalid RegExp object");
        };
        let bits = bits as u8;
        let rx = self.rx();
        let hit = rx.cache.iter().position(|c| c.0 == src.value() && c.1 == bits);
        let blob = match hit {
            Some(i) => {
                let c = rx.cache.remove(i);
                rx.cache.push(c);
                Realm::blob_of(c.2).expect("a blob")
            }
            None => {
                let units = self.units(src);
                let letters: String = (0..8).filter(|i| bits >> i & 1 != 0).map(|i| FLAG_LETTERS[i] as char).collect();
                let img = match regexp::compile(&units, &letters) {
                    Ok(img) => img,
                    Err(e) => return Err(self.regexp_error(&e, src)),
                };
                let Some(b) = Blob::new(&mut self.heap, &img) else { return Err(self.error(ErrKind::Range, "out of memory")) };
                let rx = self.rx();
                if rx.cache.len() == CACHE {
                    rx.cache.remove(0);
                }
                rx.cache.push((src.value(), bits, b.value()));
                b
            }
        };
        o.set_slot(&self.shapes, ps, blob.value());
        Ok(blob)
    }

    /// `ToLength(lastIndex)` of the regexp `o`.
    fn last_index(&mut self, o: Obj) -> R<f64> {
        let Some(s) = self.rx_slot(o, LAST) else {
            let k = self.rx().keys[LAST];
            let v = self.get_prop(o.value(), Key::Name(k))?;
            return self.to_length(v);
        };
        let v = o.slot(&self.shapes, s);
        match v.as_int() {
            Some(i) => Ok(i.max(0) as f64),
            None => self.to_length(v),
        }
    }

    fn to_length(&mut self, v: Value) -> R<f64> {
        let n = self.to_number(v)?;
        Ok(if n.is_nan() || n <= 0.0 { 0.0 } else { n.trunc().min(9007199254740991.0) })
    }

    fn set_last_index(&mut self, o: Obj, n: usize) -> R<()> {
        let v = Realm::num(n as f64);
        if o.shape(&self.shapes) == self.rx().shape {
            o.set_slot(&self.shapes, LAST as u32, v);
            return Ok(());
        }
        let k = self.rx().keys[LAST];
        self.set_checked(o.value(), Key::Name(k), v, false)
    }

    /// `RegExpBuiltinExec` without the result object: the spans of the match go to `Rx::out`. `Ok(true)` is a match.
    fn regexp_run(&mut self, o: Obj, s: Str) -> R<bool> {
        let blob = self.program(o)?;
        let img = blob.words();
        let flags = regexp::image_flags(img);
        let sticky_or_global = flags.global || flags.sticky;
        let li = self.last_index(o)?;
        let from = if sticky_or_global { li } else { 0.0 };
        let s = self.flat(s);
        if from > s.len() as f64 {
            if sticky_or_global {
                self.set_last_index(o, 0)?;
            }
            return Ok(false);
        }
        let n = 2 * (regexp::image_groups(img) + 1);
        let rx = self.rx.as_mut().expect("installed");
        rx.nspans = n;
        let (mut sc, mut out) = (std::mem::take(&mut rx.scratch), std::mem::take(&mut rx.out));
        // `exec` writes all the spans of a match, so a buffer that is long enough needs no reset.
        if out.len() < n {
            out.resize(n, u32::MAX);
        }
        let mut fuel = self.fuel;
        let r = match s.chars() {
            Some(Chars::Two(u)) => regexp::exec(img, u, from as usize, &mut sc, &mut fuel, &mut out),
            Some(Chars::One(b)) => regexp::exec(img, b, from as usize, &mut sc, &mut fuel, &mut out),
            None => regexp::exec::<u8>(img, &[], from as usize, &mut sc, &mut fuel, &mut out),
        };
        self.fuel = fuel;
        let rx = self.rx.as_mut().expect("installed");
        (rx.scratch, rx.out) = (sc, out);
        match r {
            Err(ExecError::Fuel) => Err(STOP),
            Err(ExecError::Steps | ExecError::Stack | ExecError::Input) => {
                // A limit of one run: as final as the fuel (the script ends), not a catchable error.
                self.fuel = 0;
                Err(STOP)
            }
            Ok(false) => {
                if sticky_or_global {
                    self.set_last_index(o, 0)?;
                }
                Ok(false)
            }
            Ok(true) => {
                if sticky_or_global {
                    let end = self.rx().out[1] as usize;
                    self.set_last_index(o, end)?;
                }
                Ok(true)
            }
        }
    }

    /// The substring of `s` from `a` to `b`.
    fn substring(&mut self, s: Str, a: usize, b: usize) -> Value {
        let heap = &mut self.heap;
        let r = match s.chars() {
            Some(Chars::One(c)) => Str::new(heap, Chars::One(&c[a..b])),
            Some(Chars::Two(c)) => Str::new(heap, Chars::Two(&c[a..b])),
            None => Str::new(heap, Chars::One(&[])),
        };
        r.expect("memory").value()
    }

    /// The result array of the match in `Rx::out`: the groups, then `index`, `input` and `groups`.
    fn match_result(&mut self, s: Str) -> R<Value> {
        let (out, n) = (std::mem::take(&mut self.rx().out), self.rx().nspans);
        let mut vals = Vec::with_capacity(n / 2);
        for p in out[..n].chunks(2) {
            vals.push(if p[0] == u32::MAX { Value::UNDEFINED } else { self.substring(s, p[0] as usize, p[1] as usize) });
        }
        let index = out[0];
        self.rx().out = out;
        let a = self.new_array(&vals);
        let ao = Realm::obj(a).expect("array");
        let keys = {
            let k = &self.rx().keys;
            [k[INDEX], k[INPUT], k[GROUPS]]
        };
        for (k, v) in keys.into_iter().zip([Value::int(index as i32), s.value(), Value::UNDEFINED]) {
            self.define(ao, Key::Name(k), v)?;
        }
        Ok(a)
    }

    /// `ToString(v)` of the input of `exec` and `test`.
    fn input_string(&mut self, args: &[Value]) -> R<Str> {
        self.to_str(arg(args, 0))
    }
}

fn regexp_ctor(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let (p, f) = (arg(args, 0), arg(args, 1));
    let from = Realm::obj(p).filter(|o| o.is_kind(K_REGEXP));
    // `RegExp(r)` with no `new` and no flags gives `r` itself when its constructor is `RegExp`.
    if let (Some(o), true, true) = (from, rt.nt.is_undefined(), f.is_undefined()) {
        let c = rt.get_named(o.value(), "constructor")?;
        if c == rt.callee {
            return Ok(p);
        }
    }
    let proto = rt.new_proto()?;
    let (src, flags) = match from {
        Some(o) => {
            let (Some(ss), Some(fs)) = (rt.rx_slot(o, SRC), rt.rx_slot(o, FL)) else { return rt.type_error("invalid RegExp object") };
            let src = Realm::str_of(o.slot(&rt.shapes, ss)).expect("a string");
            let bits = o.slot(&rt.shapes, fs).as_int().unwrap_or(0) as u8;
            let letters: String = (0..8).filter(|i| bits >> i & 1 != 0).map(|i| FLAG_LETTERS[i] as char).collect();
            (src, if f.is_undefined() { letters } else { rt.to_str(f).map(|s| rt.text(s))? })
        }
        None => {
            let src = if p.is_undefined() { rt.intern("") } else { rt.to_str(p)? };
            (src, if f.is_undefined() { String::new() } else { rt.to_str(f).map(|s| rt.text(s))? })
        }
    };
    rt.new_regexp(Some(proto), src, &flags)
}

fn regexp_exec(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let o = rt.this_regexp(this, "exec")?;
    let s = rt.input_string(args)?;
    if rt.regexp_run(o, s)? {
        let s = rt.flat(s);
        rt.match_result(s)
    } else { Ok(Value::NULL) }
}

fn regexp_test(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let o = rt.this_regexp(this, "test")?;
    let s = rt.input_string(args)?;
    Ok(Value::bool(rt.regexp_run(o, s)?))
}

/// The pattern and the flag bits of a regexp object, or `None` for `RegExp.prototype` itself.
fn parts(rt: &mut Realm, this: Value, what: &str) -> R<Option<(Str, u8)>> {
    if Realm::obj(this).is_some_and(|o| o.is_kind(K_REGEXP)) {
        let o = rt.this_regexp(this, what)?;
        let (ss, fs) = (rt.rx_slot(o, SRC).expect("slot"), rt.rx_slot(o, FL).expect("slot"));
        let src = Realm::str_of(o.slot(&rt.shapes, ss)).expect("a string");
        return Ok(Some((src, o.slot(&rt.shapes, fs).as_int().unwrap_or(0) as u8)));
    }
    if Realm::obj(this).is_some_and(|o| rt.rx.as_ref().is_some_and(|x| x.proto == o)) {
        return Ok(None);
    }
    rt.type_error(&format!("RegExp.prototype.{what} requires that 'this' be a RegExp object"))
}

fn letters(bits: u8) -> String {
    (0..8).filter(|i| bits >> i & 1 != 0).map(|i| FLAG_LETTERS[i] as char).collect()
}

fn regexp_flag(rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    let name = rt.native_id(rt.callee).map_or("", |i| rt.natives[i].name.trim_start_matches("get "));
    let bit = FLAG_NAMES.iter().position(|n| *n == name);
    match (parts(rt, this, name)?, bit) {
        (Some((_, bits)), Some(b)) => Ok(Value::bool(bits >> b & 1 != 0)),
        _ => Ok(Value::UNDEFINED),
    }
}

fn regexp_flags(rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    // The spec reads each flag getter; the built-in objects answer the same without the calls.
    match parts(rt, this, "flags")? {
        Some((_, bits)) => Ok(rt.string(&letters(bits))),
        None => Ok(rt.string("")),
    }
}

fn regexp_source(rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    let Some((src, _)) = parts(rt, this, "source")? else { return Ok(rt.string("(?:)")) };
    let u = rt.units(src);
    if u.is_empty() {
        return Ok(rt.string("(?:)"));
    }
    // EscapeRegExpPattern: a `/` outside a class and the line terminators.
    let mut out = Vec::with_capacity(u.len());
    let (mut class, mut i) = (false, 0);
    while i < u.len() {
        let c = u[i];
        match c {
            0x5C if i + 1 < u.len() => {
                let n = u[i + 1];
                match n {
                    0x0A => out.extend([0x5Cu16, 0x6E]),
                    0x0D => out.extend([0x5Cu16, 0x72]),
                    0x2028 => out.extend("\\u2028".encode_utf16()),
                    0x2029 => out.extend("\\u2029".encode_utf16()),
                    _ => out.extend([c, n]),
                }
                i += 1;
            }
            0x5B => {
                class = true;
                out.push(c);
            }
            0x5D => {
                class = false;
                out.push(c);
            }
            0x2F if !class => out.extend([0x5C, 0x2F]),
            0x0A => out.extend("\\n".encode_utf16()),
            0x0D => out.extend("\\r".encode_utf16()),
            0x2028 => out.extend("\\u2028".encode_utf16()),
            0x2029 => out.extend("\\u2029".encode_utf16()),
            _ => out.push(c),
        }
        i += 1;
    }
    Ok(rt.make_units(&out))
}

fn regexp_to_string(rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    if Realm::obj(this).is_none() {
        return rt.type_error("RegExp.prototype.toString requires that 'this' be an Object");
    }
    let s = rt.get_named(this, "source")?;
    let f = rt.get_named(this, "flags")?;
    let (s, f) = (rt.to_str(s)?, rt.to_str(f)?);
    let mut out: Vec<u16> = vec![0x2F];
    rt.push_units(&mut out, s);
    out.push(0x2F);
    rt.push_units(&mut out, f);
    Ok(rt.make_units(&out))
}
