//! `RegExp` for the realm (design-js2-regexp, step R1): the object, the constructor, `exec`, `test` and the cheap getters. Safe code
//! over [`Realm`]; the matcher is `regexp.rs` and the compiled program is a [`Blob`] cell.
//!
//! **Lazy.** The realm has no `RegExp` at start: `Realm::rx` is `None`, and a startup costs nothing for it. The first regex literal
//! (`Op::NewRegExp`) or the first read of the global name `RegExp` that misses (`get_global_slow`) makes the constructor and the
//! prototype ([`Realm::install_regexp`]). Known gap: `globalThis.RegExp`, `hasOwnProperty("RegExp")` and a list of the global
//! properties do not see it before that first use.
//!
//! **The object.** A plain `Object` cell with the kind `K_REGEXP` and four slots, in this order: the hidden `\0src` (the pattern
//! string), the hidden `\0fl` (the flag bits of `Flags::bits`), the hidden `\0prog` (the [`Blob`] or the pending emit limit)
//! and the own `lastIndex` (writable only). All objects start with one shape, `Rx::shape`. An object that a
//! script gave another property has another shape, and the slots are found by name then.
//!
//! **Compile.** A bad pattern throws at creation. The emitter reuses the validated tree and records emit limits for first use.
//! A small cache shares immutable setup by source content and flags. Each wrapper owns its `lastIndex`.
//!
//! **Steps.** The matcher charges its steps to `Realm::fuel`. When the fuel is out, or the run passes the cap of one run, the native
//! returns `Err(STOP)`: no handler runs, and the script ends with `Exit::Fuel` (the old engine's `Error::Limit` was as final).
use crate::blob::Blob;
use crate::bi_symbol::{MATCH, SPECIES};
use crate::builtins::{HIDDEN, NativeFn, arg};
use crate::cell::Layout;
use crate::gc::Marker;
use crate::interp::STOP;
use crate::interp_rt::{ErrKind, Key, Put, R, Realm};
use crate::object::{K_REGEXP, Obj};
use crate::regexp::{self, CompileError, ExecError, FLAG_LETTERS, Flags, Scratch};
use crate::shapes::{A_W, ShapeId};
use crate::string::{Chars, Str};
use crate::value::Value;

#[path = "regexp_protocol.rs"]
pub(crate) mod protocol;

/// How many verified patterns the cache keeps.
const CACHE: usize = 32;

struct CachedPattern {
    src: Str,
    bits: u8,
    program: Value,
}

#[cfg(test)]
thread_local! {
    static CACHE_HITS: std::cell::Cell<(usize, usize)> = const { std::cell::Cell::new((0, 0)) };
}

/// The state of `RegExp` in a realm. The collector marks it ([`Rx::mark`]).
pub struct Rx {
    proto: Obj,
    shape: ShapeId,
    proto_shape: ShapeId,
    ctor: Obj,
    ctor_shape: ShapeId,
    species_slot: u32,
    species: Value,
    exec: Value,
    flags_getters: [Value; 9],
    methods: [Value; 5],
    iterator_shape: Option<ShapeId>,
    /// The property names `\0src`, `\0fl`, `\0prog`, `lastIndex`, `index`, `input`, `groups`.
    keys: [Str; 7],
    scratch: Scratch,
    out: Vec<u32>,
    /// The words of `out` that the last match filled: two for each group, from group 0.
    nspans: usize,
    /// Ordered by last use, newest last.
    cache: Vec<CachedPattern>,
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
            m.value(c.src.value());
            m.value(c.program);
        });
    }

    fn cached(&self, src: Str, bits: u8) -> Option<usize> {
        let chars = src.chars().expect("flat pattern");
        let i = self.cache.iter().rposition(|c| {
            c.bits == bits && (c.src == src || c.src.len() == chars.len() && chars.same(&c.src.chars().expect("flat pattern")))
        })?;
        #[cfg(test)]
        CACHE_HITS.with(|hits| {
            let (pointer, content) = hits.get();
            if self.cache[i].src == src {
                hits.set((pointer + 1, content));
            } else {
                hits.set((pointer, content + 1));
            }
        });
        Some(i)
    }

    fn remember(&mut self, hit: Option<usize>, src: Str, bits: u8, program: Value) {
        if let Some(i) = hit {
            self.cache.remove(i);
        } else if self.cache.len() == CACHE {
            self.cache.remove(0);
        }
        self.cache.push(CachedPattern { src, bits, program });
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
        let proto_root = self.shapes.root(Layout::Object, Some(self.object_proto.cell()), 29);
        let proto = Obj::new(&mut self.heap, &self.shapes, proto_root).expect("memory");
        self.keep(proto.value());
        let ks = ["\0src", "\0fl", "\0prog", "lastIndex", "index", "input", "groups"].map(|n| self.intern(n));
        let root = self.shapes.root(crate::cell::Layout::Object, Some(proto.cell()), 4);
        let mut shape = root;
        for (i, k) in ks[..4].iter().enumerate() {
            shape = self.shapes.add(shape, *k, if i == LAST { A_W } else { 0 }).expect("shape");
        }
        let ctor = self.ctor("RegExp", 2, regexp_ctor, proto);
        let species_key = self.wk[SPECIES];
        let species = self.new_native(regexp_species, false, 0, "get [Symbol.species]").value();
        self.define_accessor(ctor, Key::Name(species_key), species, false, false).expect("define");
        let ctor_shape = ctor.shape(&self.shapes);
        let species_slot = self.shapes.find(ctor_shape, species_key).expect("species").slot();
        let exec = self.new_native(regexp_exec, false, 1, "exec").value();
        let exec_key = self.intern("exec");
        self.define_attrs(proto, Key::Name(exec_key), HIDDEN, exec).expect("define");
        self.method(proto, "test", 1, regexp_test);
        self.method(proto, "toString", 0, regexp_to_string);
        self.getter(proto, "source", "get source", regexp_source);
        let mut flags_getters = [Value::UNDEFINED; 9];
        flags_getters[0] = self.getter(proto, "flags", "get flags", regexp_flags);
        for (i, (n, g)) in FLAG_NAMES.into_iter().zip(GETTER_NAMES).enumerate() {
            flags_getters[i + 1] = self.getter(proto, n, g, regexp_flag);
        }
        let methods = protocol::METHODS.map(|(symbol, name, len, f)| {
            let method = self.new_native(f, false, len, name).value();
            self.define_attrs(proto, Key::Name(self.wk[symbol]), HIDDEN, method).expect("define");
            method
        });
        let proto_shape = proto.shape(&self.shapes);
        self.pins.extend([proto_shape, ctor_shape]);
        // A replaced function must not be collected and have its address reused.
        for v in [ctor.value(), species, exec].into_iter().chain(flags_getters).chain(methods) {
            self.keep(v);
        }
        self.rx = Some(Box::new(Rx {
            proto, shape, proto_shape, ctor, ctor_shape, species_slot, species, exec, flags_getters, methods, iterator_shape: None,
            keys: ks, scratch: Scratch::new(), out: Vec::new(), nspans: 0, cache: Vec::new(),
        }));
    }

    /// The original shape fixes the property slots and the prototype of the receiver.
    #[inline(always)]
    fn plain_rx(&self, o: Obj) -> Option<&Rx> {
        let rx = self.rx.as_ref()?;
        (o.shape(&self.shapes) == rx.shape && o.is_kind(K_REGEXP) && rx.proto.shape(&self.shapes) == rx.proto_shape).then_some(rx)
    }

    /// The original shape fixes the slots, but a same-shape write can still change a slot value.
    #[inline(always)]
    pub(crate) fn plain_rx_method(&self, o: Obj, symbol: usize) -> bool {
        self.plain_rx(o).is_some_and(|rx| {
            let i = symbol - MATCH;
            rx.proto.inline_slot(24 + i) == rx.methods[i]
        })
    }

    #[inline(always)]
    fn plain_rx_exec(&self, o: Obj) -> bool {
        self.plain_rx(o).is_some_and(|rx| rx.proto.inline_slot(1) == rx.exec)
    }

    #[inline]
    fn plain_rx_flags(&self, o: Obj) -> bool {
        self.plain_rx(o).is_some_and(|rx| {
            rx.flags_getters.iter().enumerate().all(|(i, v)| rx.proto.inline_slot(6 + 2 * i) == *v)
        })
    }

    fn plain_rx_species(&self, o: Obj) -> bool {
        self.plain_rx(o).is_some_and(|rx| {
            rx.proto.inline_slot(0) == rx.ctor.value()
                && rx.ctor.shape(&self.shapes) == rx.ctor_shape
                && rx.ctor.slot(&self.shapes, rx.species_slot) == rx.species
        })
    }

    pub(crate) fn is_regexp(&mut self, v: Value) -> R<bool> {
        let Some(o) = Realm::obj(v) else { return Ok(false) };
        if self.plain_rx_method(o, MATCH) {
            return Ok(true);
        }
        let m = self.get_prop(v, Key::Name(self.wk[MATCH]))?;
        Ok(if m.is_undefined() { o.is_kind(K_REGEXP) } else { Realm::truthy(m) })
    }

    /// An accessor property `name` of `o` with a getter and no setter.
    fn getter(&mut self, o: Obj, name: &str, fname: &'static str, f: NativeFn) -> Value {
        let k = self.intern(name);
        let g = self.new_native(f, false, 0, fname);
        self.define_accessor(o, Key::Name(k), g.value(), false, false).expect("define");
        g.value()
    }

    /// A new `RegExp` object. The syntax of `src` and `flags` is checked.
    pub(crate) fn new_regexp(&mut self, proto: Option<Obj>, src: Str, flags: &str) -> R<Value> {
        let src = self.flat(src);
        let flags = Flags::parse(flags).map_err(|e| self.regexp_error(&e, src))?;
        let bits = flags.bits();
        let hit = self.rx.as_ref().and_then(|rx| rx.cached(src, bits));
        let program = match hit {
            Some(i) => self.rx().cache[i].program,
            None => {
                let parsed = regexp::parse(&self.units(src), flags).map_err(|e| self.regexp_error(&e, src))?;
                match regexp::emit(parsed) {
                    // If the heap refuses the image, retry at first use as before.
                    Ok(img) => Blob::new(&mut self.heap, &img).map_or(Value::UNDEFINED, Blob::value),
                    Err(CompileError::Limit(reason)) => self.intern(reason).value(),
                    Err(e) => return Err(self.regexp_error(&e, src)),
                }
            }
        };
        let rx = self.rx();
        rx.remember(hit, src, bits, program);
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
        o.set_slot(&self.shapes, PROG as u32, program);
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

    /// The program of the regexp `o`, or the error on first use.
    #[inline(always)]
    pub(crate) fn program(&mut self, o: Obj) -> R<Blob> {
        let Some(ps) = self.rx_slot(o, PROG) else { return self.type_error("invalid RegExp object") };
        match Realm::blob_of(o.slot(&self.shapes, ps)) {
            Some(b) => Ok(b),
            None => self.compile_program(o, ps),
        }
    }

    #[inline(never)]
    fn compile_program(&mut self, o: Obj, ps: u32) -> R<Blob> {
        let (Some(ss), Some(fs)) = (self.rx_slot(o, SRC), self.rx_slot(o, FL)) else { return self.type_error("invalid RegExp object") };
        let (Some(src), Some(bits)) = (Realm::str_of(o.slot(&self.shapes, ss)), o.slot(&self.shapes, fs).as_int()) else {
            return self.type_error("invalid RegExp object");
        };
        if let Some(reason) = Realm::str_of(o.slot(&self.shapes, ps)) {
            let (src, reason) = (self.text(src), self.text(reason));
            return Err(self.error(ErrKind::Syntax, &format!("Invalid regular expression: /{src}/: {reason}")));
        }
        let bits = bits as u8;
        let rx = self.rx();
        let hit = rx.cached(src, bits);
        let blob = match hit.and_then(|i| Realm::blob_of(rx.cache[i].program)) {
            Some(b) => b,
            None => {
                let units = self.units(src);
                let img = match regexp::compile(&units, &letters(bits)) {
                    Ok(img) => img,
                    Err(e) => return Err(self.regexp_error(&e, src)),
                };
                let Some(b) = Blob::new(&mut self.heap, &img) else { return Err(self.error(ErrKind::Range, "out of memory")) };
                b
            }
        };
        self.rx().remember(hit, src, bits, blob.value());
        o.set_slot(&self.shapes, ps, blob.value());
        Ok(blob)
    }

    /// `ToLength(lastIndex)` of the regexp `o`.
    #[inline(always)]
    pub(crate) fn last_index(&mut self, o: Obj) -> R<f64> {
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

    pub(crate) fn to_length(&mut self, v: Value) -> R<f64> {
        let n = self.to_number(v)?;
        Ok(if n.is_nan() || n <= 0.0 { 0.0 } else { n.trunc().min(9007199254740991.0) })
    }

    pub(crate) fn set_last_index(&mut self, o: Obj, n: usize) -> R<()> {
        let v = Realm::num(n as f64);
        if o.shape(&self.shapes) == self.rx().shape {
            o.set_slot(&self.shapes, LAST as u32, v);
            return Ok(());
        }
        self.put_last_index(o, v)
    }

    /// `RegExpBuiltinExec` without the result object: the spans of the match go to `Rx::out`. `Ok(true)` is a match.
    pub(crate) fn regexp_run(&mut self, o: Obj, s: Str) -> R<bool> {
        let blob = self.program(o)?;
        self.regexp_run_program(o, s, blob)
    }

    #[inline(always)]
    fn regexp_run_program(&mut self, o: Obj, s: Str, blob: Blob) -> R<bool> {
        let flags = regexp::image_flags(blob.words());
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
        let hit = self.run_at(blob, s, from as usize)?;
        if sticky_or_global {
            let end = if hit { self.rx().out[1] as usize } else { 0 };
            self.set_last_index(o, end)?;
        }
        Ok(hit)
    }

    /// Run the program `blob` on the flat string `s` from `from` (at `from` only for a sticky program); the spans go to `Rx::out`. It
    /// reads and writes no `lastIndex`.
    #[inline(always)]
    pub(crate) fn run_at(&mut self, blob: Blob, s: Str, from: usize) -> R<bool> {
        let img = blob.words();
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
            Some(Chars::Two(u)) => regexp::exec(img, u, from, &mut sc, &mut fuel, &mut out),
            Some(Chars::One(b)) => regexp::exec(img, b, from, &mut sc, &mut fuel, &mut out),
            None => regexp::exec::<u8>(img, &[], from, &mut sc, &mut fuel, &mut out),
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
            Ok(hit) => Ok(hit),
        }
    }

    /// The spans of the last match: two words for each group, from group 0. `u32::MAX` is a group that did not take part.
    pub(crate) fn spans(&mut self) -> Vec<u32> {
        let rx = self.rx();
        rx.out[..rx.nspans].to_vec()
    }

    /// The internal flags of a RegExp on the fast path.
    fn plain_flags(&self, o: Obj) -> Flags {
        Flags::from_bits(o.inline_slot(FL).as_int().expect("flags") as u8)
    }

    /// RegExpCreate with the intrinsic prototype. Match, matchAll and search use it as their fallback.
    pub(crate) fn regexp_for(&mut self, v: Value, flags: &str) -> R<Value> {
        let src = if v.is_undefined() { self.intern("") } else { self.to_str(v)? };
        self.new_regexp(None, src, flags)
    }

    /// `lastIndex` of the regexp `o` as a value, and the write that puts it back (`search`).
    pub(crate) fn last_index_value(&mut self, o: Obj) -> R<Value> {
        let k = self.rx().keys[LAST];
        self.get_prop(o.value(), Key::Name(k))
    }

    pub(crate) fn put_last_index(&mut self, o: Obj, v: Value) -> R<()> {
        let k = self.rx().keys[LAST];
        match self.set_prop_g(o.value(), Key::Name(k), v)? {
            Put::Done => Ok(()),
            Put::Fail => self.type_error("cannot set lastIndex"),
            Put::Set(f) => self.call_function(f, o.value(), &[v]).map(|_| ()),
        }
    }

    /// The substring of `s` from `a` to `b`.
    #[inline(always)]
    pub(crate) fn substring(&mut self, s: Str, a: usize, b: usize) -> Value {
        let heap = &mut self.heap;
        let r = match s.chars() {
            Some(Chars::One(c)) => Str::new(heap, Chars::One(&c[a..b])),
            Some(Chars::Two(c)) => Str::new(heap, Chars::Two(&c[a..b])),
            None => Str::new(heap, Chars::One(&[])),
        };
        r.expect("memory").value()
    }

    /// The result array of the match in `Rx::out`: the groups, then `index`, `input` and `groups`.
    #[inline(always)]
    fn match_result(&mut self, s: Str, blob: Blob) -> R<Value> {
        let (out, n) = (std::mem::take(&mut self.rx().out), self.rx().nspans);
        let mut vals = Vec::with_capacity(n / 2);
        for p in out[..n].chunks(2) {
            vals.push(if p[0] == u32::MAX { Value::UNDEFINED } else { self.substring(s, p[0] as usize, p[1] as usize) });
        }
        let index = out[0];
        let extra = regexp::image_extra_groups(blob.words());
        let indices = if extra && regexp::image_flags(blob.words()).has_indices {
            self.match_indices(&out[..n])
        } else {
            Vec::new()
        };
        self.rx().out = out;
        let a = self.new_array(&vals);
        let ao = Realm::obj(a).expect("array");
        let keys = {
            let k = &self.rx().keys;
            [k[INDEX], k[INPUT], k[GROUPS]]
        };
        let groups = if extra { self.named_groups(blob, &vals)? } else { Value::UNDEFINED };
        for (k, v) in keys.into_iter().zip([Value::int(index as i32), s.value(), groups]) {
            self.define(ao, Key::Name(k), v)?;
        }
        if !indices.is_empty() {
            let array = Realm::obj(self.new_array(&indices)).expect("array");
            let groups = self.named_groups(blob, &indices)?;
            self.define(array, Key::Name(keys[2]), groups)?;
            let key = self.intern("indices");
            self.define(ao, Key::Name(key), array.value())?;
        }
        Ok(a)
    }

    #[cold]
    fn match_indices(&mut self, out: &[u32]) -> Vec<Value> {
        out.chunks_exact(2).map(|p| {
            if p[0] == u32::MAX {
                Value::UNDEFINED
            } else {
                self.new_array(&[Value::int(p[0] as i32), Value::int(p[1] as i32)])
            }
        }).collect()
    }

    #[cold]
    fn named_groups(&mut self, blob: Blob, vals: &[Value]) -> R<Value> {
        let mut names = regexp::image_names(blob.words()).peekable();
        if names.peek().is_none() {
            return Ok(Value::UNDEFINED);
        }
        let shape = self.shapes.root(Layout::Object, None, 4);
        let o = Obj::new(&mut self.heap, &self.shapes, shape).expect("memory");
        for (name, i) in names {
            let key = self.make_units(&name);
            let key = self.key_of(key)?;
            if !vals[i].is_undefined() || self.get_own(o, key).is_none() {
                self.define(o, key, vals[i])?;
            }
        }
        Ok(o.value())
    }

    fn rx_builtin_exec(&mut self, o: Obj, s: Str) -> R<Value> {
        let blob = self.program(o)?;
        if self.regexp_run_program(o, s, blob)? {
            let s = self.flat(s);
            self.match_result(s, blob)
        } else {
            Ok(Value::NULL)
        }
    }

    /// RegExpExec, including a script-defined exec and its result type check.
    fn rx_exec(&mut self, o: Obj, s: Str) -> R<Value> {
        let k = self.intern("exec");
        let exec = self.get_prop(o.value(), Key::Name(k))?;
        if Realm::obj(exec).is_some_and(|f| f.is_callable()) {
            if self.rx.as_ref().is_some_and(|rx| rx.exec == exec) {
                let o = self.this_regexp(o.value(), "exec")?;
                return self.rx_builtin_exec(o, s);
            }
            let r = self.call_function(exec, o.value(), &[s.value()])?;
            return if r.is_null() || Realm::obj(r).is_some() { Ok(r) } else { self.type_error("RegExp exec returned a non-object") };
        }
        let o = self.this_regexp(o.value(), "exec")?;
        self.rx_builtin_exec(o, s)
    }
}

fn regexp_ctor(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let (p, f) = (arg(args, 0), arg(args, 1));
    let is_regexp = rt.is_regexp(p)?;
    if is_regexp && rt.nt.is_undefined() && f.is_undefined() {
        let c = rt.get_named(p, "constructor")?;
        if c == rt.callee {
            return Ok(p);
        }
    }
    let (source, flags) = if let Some(o) = Realm::obj(p).filter(|o| o.is_kind(K_REGEXP)) {
        let Some((src, bits)) = parts(rt, o.value(), "constructor")? else { unreachable!() };
        (src.value(), if f.is_undefined() { rt.string(&letters(bits)) } else { f })
    } else if is_regexp {
        let src = rt.get_named(p, "source")?;
        (src, if f.is_undefined() { rt.get_named(p, "flags")? } else { f })
    } else {
        (p, f)
    };
    let target = if rt.nt.is_undefined() { rt.callee } else { rt.nt };
    let proto = rt.get_prop(target, Key::Name(rt.k_prototype))?;
    let proto = Realm::obj(proto);
    let src = if source.is_undefined() { rt.intern("") } else { rt.to_str(source)? };
    let flags = if flags.is_undefined() { String::new() } else { rt.to_str(flags).map(|s| rt.text(s))? };
    rt.new_regexp(proto, src, &flags)
}

fn regexp_species(_rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    Ok(this)
}

fn regexp_exec(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let o = rt.this_regexp(this, "exec")?;
    let s = rt.to_str(arg(args, 0))?;
    rt.rx_builtin_exec(o, s)
}

fn regexp_test(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let Some(o) = Realm::obj(this) else { return rt.type_error("RegExp.prototype.test requires an object") };
    let s = rt.to_str(arg(args, 0))?;
    if rt.plain_rx_exec(o) {
        return Ok(Value::bool(rt.regexp_run(o, s)?));
    }
    Ok(Value::bool(!rt.rx_exec(o, s)?.is_null()))
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
    if Realm::obj(this).is_none() {
        return rt.type_error("RegExp.prototype.flags requires an object");
    }
    let mut bits = 0;
    for (i, name) in FLAG_NAMES.iter().enumerate() {
        if Realm::truthy(rt.get_named(this, name)?) {
            bits |= 1 << i;
        }
    }
    Ok(rt.string(&letters(bits)))
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

#[cfg(test)]
#[path = "regexp_setup_reuse_tests.rs"]
mod setup_reuse_tests;

#[cfg(test)]
#[path = "regexp_one_pass_tests.rs"]
mod one_pass_tests;
