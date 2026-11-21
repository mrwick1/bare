//! `Array` and the `Array.prototype` methods that do not call a script function (plan step S2.7). `forEach`, `map`, `filter`, `reduce`,
//! `some`, `every`, `find`, `findIndex`, `sort` and the others that call back are in `prelude.js` (see `builtins.rs`).
//!
//! Each method follows the specification: `this` goes through `ToObject`, the length through `ToLength`, and the elements are read
//! with `get`, `has`, `set` and `delete`, so a method works on an array-like object, on holes that a prototype fills, and on a frozen
//! object (it throws). The helpers [`get_at`], [`has_at`], [`set_at`] and [`del_at`] take the direct path for an array with plain
//! elements, so a real array pays one flag test. An accessor property of an array-like is not read here (it needs a call): known gap.
//! `shift` and `unshift` move all the elements: O(n) each.
use crate::lazy::{Ent, m, script, sym_same};
use crate::bi_object::{Desc, H_SEARCH};
use crate::bi_symbol::{ITERATOR, SPECIES};
use crate::builtins::{NativeFn, arg};
use crate::interp_rt::{ErrKind, Key, R, Realm};
use crate::object::{F_ELEMS_FIXED, F_ELEMS_RO, F_LEN_RO, F_NOEXT, F_SLOWIDX, Obj};
use crate::string::Str;
use crate::value::Value;

/// The largest length of an array-like (2^53 - 1).
const MAX_LEN: f64 = 9007199254740991.0;
/// The flags that stop the direct path to the elements of an array.
const PLAIN: u32 = F_NOEXT | F_SLOWIDX | F_ELEMS_RO | F_ELEMS_FIXED | F_LEN_RO;

/// A relative index (`slice`, `splice`, `indexOf`): `v` is `ToIntegerOrInfinity`; a negative value counts from `len` when `neg` is true,
/// else it is 0; the result is at most `len`. `default` stands for `undefined`.
pub(crate) fn rel_index(rt: &mut Realm, v: Value, default: f64, len: f64, neg: bool) -> R<usize> {
    let n = if v.is_undefined() { default } else { rt.to_number(v)? };
    let n = if n.is_nan() { 0.0 } else { n.trunc() };
    let n = if n < 0.0 && neg { (len + n).max(0.0) } else { n.max(0.0) };
    Ok(n.min(len) as usize)
}


/// `ToIntegerOrInfinity` of `v` (`undefined` is `default`).
fn to_int(rt: &mut Realm, v: Value, default: f64) -> R<f64> {
    if v.is_undefined() {
        return Ok(default);
    }
    let n = rt.to_number(v)?;
    Ok(if n.is_nan() { 0.0 } else { n.trunc() + 0.0 })
}

/// A relative position (`slice`, `fill`, `at`, ...): a negative value counts from `len`; the result is in `0..=len`.
fn rel(rt: &mut Realm, v: Value, default: f64, len: f64) -> R<f64> {
    let n = to_int(rt, v, default)?;
    Ok(if n < 0.0 { (len + n).max(0.0) } else { n.min(len) })
}

fn this_val(rt: &mut Realm, this: Value) -> R<Value> {
    rt.to_object(this).map(Obj::value)
}

/// The key of the position `i` (a number that is a non-negative integer).
fn key_at(rt: &mut Realm, i: f64) -> Key {
    if i < u32::MAX as f64 { Key::Idx(i as u32) } else { Key::Name(rt.intern(&format!("{}", i as u64))) }
}

/// Can the search methods read the elements of `v` without a call? An array or an arguments object whose elements are all data.
fn direct(v: Value) -> bool {
    Realm::obj(v).is_some_and(|o| o.is_indexed() && !o.has_flag(F_SLOWIDX))
}

/// An array whose elements the helpers may read and write directly.
fn plain(o: Obj) -> bool {
    o.is_indexed() && !o.has_flag(PLAIN)
}

/// `ToLength(Get(v, "length"))`.
pub(crate) fn len_of(rt: &mut Realm, v: Value) -> R<f64> {
    if let Some(o) = Realm::obj(v)
        && rt.is_array(o)
    {
        return Ok(o.len() as f64);
    }
    let l = rt.get_prop(v, Key::Name(rt.k_length))?;
    let n = rt.to_number(l)?;
    Ok(if n.is_nan() || n <= 0.0 { 0.0 } else { n.trunc().min(MAX_LEN) })
}

pub(crate) fn get_at(rt: &mut Realm, v: Value, i: f64) -> R<Value> {
    if let Some(o) = Realm::obj(v)
        && o.is_indexed()
        && i < u32::MAX as f64
        && let Some(x) = o.get_element(i as u32)
    {
        return Ok(x);
    }
    let k = key_at(rt, i);
    rt.get_prop(v, k)
}

/// `HasProperty(v, i)`.
pub(crate) fn has_at(rt: &mut Realm, v: Value, i: f64) -> R<bool> {
    if let Some(o) = Realm::obj(v)
        && o.is_indexed()
        && i < u32::MAX as f64
        && o.get_element(i as u32).is_some()
    {
        return Ok(true);
    }
    let k = key_at(rt, i);
    rt.has_prop(v, k)
}

/// `Set(v, i, x, true)`.
pub(crate) fn set_at(rt: &mut Realm, v: Value, i: f64, x: Value) -> R<()> {
    if let Some(o) = Realm::obj(v)
        && plain(o)
        && i < u32::MAX as f64
        && o.get_element(i as u32).is_some()
    {
        if o.set_element(&mut rt.heap, i as u32, x).is_none() {
            return Err(rt.error(ErrKind::Range, "invalid array length"));
        }
        return Ok(());
    }
    let k = key_at(rt, i);
    rt.set_checked(v, k, x, false)
}

/// `Set(v, "length", n, true)`.
fn set_len(rt: &mut Realm, v: Value, n: f64) -> R<()> {
    if let Some(o) = Realm::obj(v)
        && rt.is_array(o)
        && plain(o)
        && n <= u32::MAX as f64
    {
        o.set_len(&mut rt.heap, n as u32).expect("memory");
        return Ok(());
    }
    rt.set_checked(v, Key::Name(rt.k_length), Realm::num(n), false)
}

/// `DeletePropertyOrThrow(v, i)`.
fn del_at(rt: &mut Realm, v: Value, i: f64) -> R<()> {
    let k = key_at(rt, i);
    if rt.delete_prop(v, k)? { Ok(()) } else { rt.type_error("cannot delete a property that is not configurable") }
}

/// Create the result of an array method, using the original array's species when present.
fn species_create(rt: &mut Realm, o: Value, n: f64) -> R<Obj> {
    if Realm::obj(o).is_some_and(|x| rt.is_array(x)) {
        let mut c = rt.get_prop(o, Key::Name(rt.k_constructor))?;
        if Realm::obj(c).is_some() {
            c = rt.get_prop(c, Key::Name(rt.wk[SPECIES]))?;
            if c.is_null() {
                c = Value::UNDEFINED;
            }
        }
        if !c.is_undefined() && c != rt.array_ctor {
            if !rt.is_constructor(c) {
                return rt.type_error("the array species is not a constructor");
            }
            let v = rt.construct_value(c, &[Realm::num(n + 0.0)])?;
            return Realm::obj(v).ok_or_else(|| rt.error(ErrKind::Type, "the array species did not return an object"));
        }
    }
    array_create(rt, n)
}

fn array_create(rt: &mut Realm, n: f64) -> R<Obj> {
    if n > u32::MAX as f64 {
        return Err(rt.error(ErrKind::Range, "invalid array length"));
    }
    let a = Obj::array(&mut rt.heap, &rt.shapes, rt.s_array, &[]).expect("memory");
    a.set_len(&mut rt.heap, n as u32).expect("memory");
    Ok(a)
}

/// `CreateDataPropertyOrThrow(a, i, x)` on a fresh array.
fn put(rt: &mut Realm, a: Obj, i: f64, x: Value) -> R<()> {
    let k = key_at(rt, i);
    if a.is_indexed() {
        return rt.define(a, k, x);
    }
    let d = Desc { value: Some(x), w: Some(true), e: Some(true), c: Some(true), ..Desc::default() };
    if rt.define_own(a, k, &d)? { Ok(()) } else { rt.type_error("cannot define the property") }
}

pub(crate) fn prelude_species(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let n = arg(args, 1).as_f64().expect("length");
    species_create(rt, arg(args, 0), n).map(Obj::value)
}

pub(crate) fn prelude_array_put(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let a = Realm::obj(arg(args, 0)).expect("species result");
    let i = arg(args, 1).as_f64().expect("index");
    put(rt, a, i, arg(args, 2))?;
    Ok(Value::UNDEFINED)
}

fn array_species_get(_rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    Ok(this)
}

impl Realm {
    pub(crate) fn install_array(&mut self) {
        let proto = self.array_proto;
        let c = self.ctor("Array", 1, array_ctor, proto);
        self.array_ctor = c.value();
        self.keep(c.value());
        let get = self.new_native(array_species_get, false, 0, "get [Symbol.species]");
        self.define_accessor(c, Key::Name(self.wk[SPECIES]), get.value(), false, false).expect("define");
        static ARRAY_STATICS: &[Ent] = &[m("of", 0, array_of), m("from", 1, array_from), m("isArray", 1, is_array)];
        self.lazy_table(c, ARRAY_STATICS);
        static ARRAY_PROTO: &[Ent] = &[
            script("keys", "array_keys"),
            script("values", "array_values"),
            script("entries", "array_entries"),
            script("forEach", "array_forEach"),
            script("map", "array_map"),
            script("filter", "array_filter"),
            script("reduce", "array_reduce"),
            script("reduceRight", "array_reduceRight"),
            script("some", "array_some"),
            script("every", "array_every"),
            script("find", "array_find"),
            script("findIndex", "array_findIndex"),
            script("findLast", "array_findLast"),
            script("findLastIndex", "array_findLastIndex"),
            script("flat", "array_flat"),
            script("flatMap", "array_flatMap"),
            script("sort", "array_sort"),
            script("toSorted", "array_toSorted"),
            script("toLocaleString", "array_toLocaleString"),
            sym_same("[Symbol.iterator]", crate::bi_symbol::ITERATOR, "values"),
            m("at", 1, at),
            m("concat", 1, concat),
            m("copyWithin", 2, copy_within),
            m("fill", 1, fill),
            m("includes", 1, includes),
            m("indexOf", 1, index_of),
            m("join", 1, join),
            m("lastIndexOf", 1, last_index_of),
            m("pop", 0, pop),
            m("push", 1, push),
            m("reverse", 0, reverse),
            m("shift", 0, shift),
            m("slice", 2, slice),
            m("splice", 2, splice),
            m("toReversed", 0, to_reversed),
            m("toSpliced", 2, to_spliced),
            m("toString", 0, to_string),
            m("unshift", 1, unshift),
            m("with", 2, with),
        ];
        self.lazy_table(proto, ARRAY_PROTO);
    }
}

fn array_ctor(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let proto = rt.new_proto()?;
    let shape = rt.shapes.root(crate::cell::Layout::Indexed, Some(proto.cell()), 0);
    let o = match args {
        [n] if n.is_number() => {
            let f = n.as_f64().expect("a number");
            if f < 0.0 || f.fract() != 0.0 || f > u32::MAX as f64 {
                return Err(rt.error(ErrKind::Range, "invalid array length"));
            }
            let o = Obj::array(&mut rt.heap, &rt.shapes, shape, &[]).expect("memory");
            o.set_len(&mut rt.heap, f as u32).expect("memory");
            o
        }
        _ => Obj::array(&mut rt.heap, &rt.shapes, shape, args).expect("memory"),
    };
    Ok(o.value())
}

/// Construct a result with the caller's constructor or use the intrinsic array.
fn factory_result(rt: &mut Realm, ctor: Value, len: Option<f64>) -> R<Obj> {
    if ctor != rt.array_ctor && rt.is_constructor(ctor) {
        let v = match len {
            Some(n) => rt.construct_value(ctor, &[Realm::num(n)])?,
            None => rt.construct_value(ctor, &[])?,
        };
        return Ok(Realm::obj(v).expect("a constructor returns an object"));
    }
    array_create(rt, len.unwrap_or(0.0))
}

fn array_of(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let n = args.len() as f64;
    let result = factory_result(rt, this, Some(n))?;
    for (i, &value) in args.iter().enumerate() {
        put(rt, result, i as f64, value)?;
    }
    set_len(rt, result.value(), n)?;
    Ok(result.value())
}

fn array_from(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let (items, mapper, receiver) = (arg(args, 0), arg(args, 1), arg(args, 2));
    let mapping = !mapper.is_undefined();
    if mapping && !Realm::obj(mapper).is_some_and(|f| f.is_callable()) {
        return rt.type_error("the mapper is not a function");
    }
    let method = rt.get_prop(items, Key::Name(rt.wk[ITERATOR]))?;
    let map = |rt: &mut Realm, value, i| {
        if mapping { rt.call_function(mapper, receiver, &[value, Realm::num(i)]) } else { Ok(value) }
    };
    let result;
    let mut i = 0.0;
    if method.is_null() || method.is_undefined() {
        let source = rt.to_object(items)?.value();
        let n = len_of(rt, source)?;
        result = factory_result(rt, this, Some(n))?;
        while i < n {
            rt.fuel = rt.fuel.checked_sub(1).ok_or(crate::interp::STOP)?;
            let value = get_at(rt, source, i)?;
            let value = map(rt, value, i)?;
            put(rt, result, i, value)?;
            i += 1.0;
        }
    } else {
        if !Realm::obj(method).is_some_and(|f| f.is_callable()) {
            return rt.type_error("the iterator method is not a function");
        }
        result = factory_result(rt, this, None)?;
        let array = Realm::obj(items).filter(|o| rt.plain_array_iterator(*o) && method == rt.args_iter);
        let (iter, next) = if array.is_some() { (items, Value::UNDEFINED) } else { rt.iter_open_method(items, method)? };
        loop {
            rt.fuel = rt.fuel.checked_sub(1).ok_or(crate::interp::STOP)?;
            if i == MAX_LEN {
                let e = rt.error(ErrKind::Type, "the result has too many elements");
                rt.iter_close(iter, next, true)?;
                return Err(e);
            }
            let value = if let Some(array) = array {
                if i >= array.len() as f64 {
                    break;
                }
                get_at(rt, items, i)?
            } else {
                let Some(value) = rt.iter_step(iter, next)? else { break };
                value
            };
            if let Err(e) = map(rt, value, i).and_then(|value| put(rt, result, i, value)) {
                if e != crate::interp::STOP {
                    if array.is_some() {
                        rt.close_array_iterator(items, i as u32 + 1, true)?;
                    } else {
                        rt.iter_close(iter, next, true)?;
                    }
                }
                return Err(e);
            }
            i += 1.0;
        }
    }
    set_len(rt, result.value(), i)?;
    Ok(result.value())
}

fn is_array(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    Ok(Value::bool(Realm::obj(arg(args, 0)).is_some_and(|o| rt.is_array(o))))
}

fn at(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let o = this_val(rt, this)?;
    let len = len_of(rt, o)?;
    let n = to_int(rt, arg(args, 0), 0.0)?;
    let k = if n >= 0.0 { n } else { len + n };
    if k < 0.0 || k >= len { Ok(Value::UNDEFINED) } else { get_at(rt, o, k) }
}

fn push(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let o = this_val(rt, this)?;
    let len = len_of(rt, o)?;
    if len + args.len() as f64 > MAX_LEN {
        return rt.type_error("the length of the array-like would be above 2^53 - 1");
    }
    for (j, &v) in args.iter().enumerate() {
        set_at(rt, o, len + j as f64, v)?;
    }
    let n = len + args.len() as f64;
    set_len(rt, o, n)?;
    Ok(Realm::num(n))
}

fn pop(rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    let o = this_val(rt, this)?;
    let len = len_of(rt, o)?;
    if len == 0.0 {
        set_len(rt, o, 0.0)?;
        return Ok(Value::UNDEFINED);
    }
    let v = get_at(rt, o, len - 1.0)?;
    del_at(rt, o, len - 1.0)?;
    set_len(rt, o, len - 1.0)?;
    Ok(v)
}

fn shift(rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    let o = this_val(rt, this)?;
    let len = len_of(rt, o)?;
    if len == 0.0 {
        set_len(rt, o, 0.0)?;
        return Ok(Value::UNDEFINED);
    }
    let first = get_at(rt, o, 0.0)?;
    let mut k = 1.0;
    while k < len {
        if has_at(rt, o, k)? {
            let v = get_at(rt, o, k)?;
            set_at(rt, o, k - 1.0, v)?;
        } else {
            del_at(rt, o, k - 1.0)?;
        }
        k += 1.0;
    }
    del_at(rt, o, len - 1.0)?;
    set_len(rt, o, len - 1.0)?;
    Ok(first)
}

fn unshift(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let o = this_val(rt, this)?;
    let len = len_of(rt, o)?;
    let n = args.len() as f64;
    if n > 0.0 {
        if len + n > MAX_LEN {
            return rt.type_error("the length of the array-like would be above 2^53 - 1");
        }
        let mut k = len;
        while k > 0.0 {
            if has_at(rt, o, k - 1.0)? {
                let v = get_at(rt, o, k - 1.0)?;
                set_at(rt, o, k + n - 1.0, v)?;
            } else {
                del_at(rt, o, k + n - 1.0)?;
            }
            k -= 1.0;
        }
        for (j, &v) in args.iter().enumerate() {
            set_at(rt, o, j as f64, v)?;
        }
    }
    set_len(rt, o, len + n)?;
    Ok(Realm::num(len + n))
}

fn slice(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let o = this_val(rt, this)?;
    let len = len_of(rt, o)?;
    let (mut k, fin) = (rel(rt, arg(args, 0), 0.0, len)?, rel(rt, arg(args, 1), len, len)?);
    let a = species_create(rt, o, (fin - k).max(0.0))?;
    let mut n = 0.0;
    while k < fin {
        if has_at(rt, o, k)? {
            let v = get_at(rt, o, k)?;
            put(rt, a, n, v)?;
        }
        k += 1.0;
        n += 1.0;
    }
    set_len(rt, a.value(), n)?;
    Ok(a.value())
}

fn splice(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let o = this_val(rt, this)?;
    let len = len_of(rt, o)?;
    let start = rel(rt, arg(args, 0), 0.0, len)?;
    let items = args.get(2..).unwrap_or(&[]);
    let count = match args.len() {
        0 => 0.0,
        1 => len - start,
        _ => to_int(rt, args[1], 0.0)?.max(0.0).min(len - start),
    };
    let n = items.len() as f64;
    if len + n - count > MAX_LEN {
        return rt.type_error("the length of the array-like would be above 2^53 - 1");
    }
    let a = species_create(rt, o, count)?;
    let mut k = 0.0;
    while k < count {
        if has_at(rt, o, start + k)? {
            let v = get_at(rt, o, start + k)?;
            put(rt, a, k, v)?;
        }
        k += 1.0;
    }
    set_len(rt, a.value(), count)?;
    if n < count {
        let mut k = start;
        while k < len - count {
            if has_at(rt, o, k + count)? {
                let v = get_at(rt, o, k + count)?;
                set_at(rt, o, k + n, v)?;
            } else {
                del_at(rt, o, k + n)?;
            }
            k += 1.0;
        }
        let mut k = len;
        while k > len - count + n {
            del_at(rt, o, k - 1.0)?;
            k -= 1.0;
        }
    } else if n > count {
        let mut k = len - count;
        while k > start {
            if has_at(rt, o, k + count - 1.0)? {
                let v = get_at(rt, o, k + count - 1.0)?;
                set_at(rt, o, k + n - 1.0, v)?;
            } else {
                del_at(rt, o, k + n - 1.0)?;
            }
            k -= 1.0;
        }
    }
    for (j, &v) in items.iter().enumerate() {
        set_at(rt, o, start + j as f64, v)?;
    }
    set_len(rt, o, len - count + n)?;
    Ok(a.value())
}

fn concat(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let o = this_val(rt, this)?;
    let a = species_create(rt, o, 0.0)?;
    let mut n = 0.0;
    for e in std::iter::once(o).chain(args.iter().copied()) {
        if Realm::obj(e).is_some_and(|x| rt.is_array(x)) {
            let len = len_of(rt, e)?;
            if n + len > MAX_LEN {
                return rt.type_error("the length of the result would be above 2^53 - 1");
            }
            let mut k = 0.0;
            while k < len {
                if has_at(rt, e, k)? {
                    let v = get_at(rt, e, k)?;
                    put(rt, a, n, v)?;
                }
                k += 1.0;
                n += 1.0;
            }
        } else {
            if n >= MAX_LEN {
                return rt.type_error("the length of the result would be above 2^53 - 1");
            }
            put(rt, a, n, e)?;
            n += 1.0;
        }
    }
    set_len(rt, a.value(), n)?;
    Ok(a.value())
}

fn join_with(rt: &mut Realm, o: Value, sep: &[u16]) -> R<Value> {
    let len = len_of(rt, o)?;
    let mut out: Vec<u16> = Vec::new();
    let mut k = 0.0;
    while k < len {
        if k > 0.0 {
            out.extend(sep);
        }
        let v = get_at(rt, o, k)?;
        if !v.is_undefined() && !v.is_null() {
            let s: Str = rt.to_str(v)?;
            rt.push_units(&mut out, s);
        }
        k += 1.0;
    }
    Ok(rt.make_units(&out))
}

fn join(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let o = this_val(rt, this)?;
    let len = len_of(rt, o)?;
    let _ = len;
    let sep = if arg(args, 0).is_undefined() {
        vec![b',' as u16]
    } else {
        let s = rt.to_str(args[0])?;
        rt.units(s)
    };
    join_with(rt, o, &sep)
}

/// `Array.prototype.toString`: `this.join()` when `join` is a function, else `Object.prototype.toString`.
fn to_string(rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    let o = this_val(rt, this)?;
    let j = rt.get_named(o, "join")?;
    if Realm::obj(j).is_some_and(|f| f.is_callable()) {
        if rt.native_id(j).is_some_and(|id| rt.natives[id].f as usize == join as NativeFn as usize) {
            return join_with(rt, o, &[b',' as u16]);
        }
        rt.tail_args.clear();
        rt.tail = Some((j, o));
        return Ok(Value::UNDEFINED);
    }
    crate::bi_object::object_to_string(rt, this, &[])
}

fn index_of(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let o = this_val(rt, this)?;
    if !direct(o) {
        return rt.helper(H_SEARCH, &[Value::int(0), o, arg(args, 0), arg(args, 1), Value::bool(args.len() > 1)]);
    }
    let len = len_of(rt, o)?;
    if len == 0.0 {
        return Ok(Value::int(-1));
    }
    let n = to_int(rt, arg(args, 1), 0.0)?;
    if n == f64::INFINITY {
        return Ok(Value::int(-1));
    }
    let mut k = if n >= 0.0 { n } else { (len + n).max(0.0) };
    while k < len {
        if has_at(rt, o, k)? {
            let v = get_at(rt, o, k)?;
            if rt.strict_eq(v, arg(args, 0)) {
                return Ok(Realm::num(k));
            }
        }
        k += 1.0;
    }
    Ok(Value::int(-1))
}

fn last_index_of(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let o = this_val(rt, this)?;
    if !direct(o) {
        return rt.helper(H_SEARCH, &[Value::int(1), o, arg(args, 0), arg(args, 1), Value::bool(args.len() > 1)]);
    }
    let len = len_of(rt, o)?;
    if len == 0.0 {
        return Ok(Value::int(-1));
    }
    let n = if args.len() > 1 { to_int(rt, args[1], 0.0)? } else { len - 1.0 };
    if n == f64::NEG_INFINITY {
        return Ok(Value::int(-1));
    }
    let mut k = if n >= 0.0 { n.min(len - 1.0) } else { len + n };
    while k >= 0.0 {
        if has_at(rt, o, k)? {
            let v = get_at(rt, o, k)?;
            if rt.strict_eq(v, arg(args, 0)) {
                return Ok(Realm::num(k));
            }
        }
        k -= 1.0;
    }
    Ok(Value::int(-1))
}

fn includes(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let o = this_val(rt, this)?;
    if !direct(o) {
        return rt.helper(H_SEARCH, &[Value::int(2), o, arg(args, 0), arg(args, 1), Value::bool(args.len() > 1)]);
    }
    let len = len_of(rt, o)?;
    if len == 0.0 {
        return Ok(Value::FALSE);
    }
    let n = to_int(rt, arg(args, 1), 0.0)?;
    if n == f64::INFINITY {
        return Ok(Value::FALSE);
    }
    let x = arg(args, 0);
    let nan = x.as_f64().is_some_and(f64::is_nan);
    let mut k = if n >= 0.0 { n } else { (len + n).max(0.0) };
    while k < len {
        // `includes` reads a hole as `undefined` and counts `NaN` as equal to itself.
        let v = get_at(rt, o, k)?;
        if rt.strict_eq(v, x) || (nan && v.as_f64().is_some_and(f64::is_nan)) {
            return Ok(Value::TRUE);
        }
        k += 1.0;
    }
    Ok(Value::FALSE)
}

fn reverse(rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    let o = this_val(rt, this)?;
    let len = len_of(rt, o)?;
    let mid = (len / 2.0).floor();
    let mut lo = 0.0;
    while lo != mid {
        let hi = len - lo - 1.0;
        let le = has_at(rt, o, lo)?;
        let lv = if le { get_at(rt, o, lo)? } else { Value::UNDEFINED };
        let he = has_at(rt, o, hi)?;
        let hv = if he { get_at(rt, o, hi)? } else { Value::UNDEFINED };
        match (le, he) {
            (true, true) => {
                set_at(rt, o, lo, hv)?;
                set_at(rt, o, hi, lv)?;
            }
            (false, true) => {
                set_at(rt, o, lo, hv)?;
                del_at(rt, o, hi)?;
            }
            (true, false) => {
                del_at(rt, o, lo)?;
                set_at(rt, o, hi, lv)?;
            }
            (false, false) => {}
        }
        lo += 1.0;
    }
    Ok(o)
}

fn fill(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let o = this_val(rt, this)?;
    let len = len_of(rt, o)?;
    let (mut k, fin) = (rel(rt, arg(args, 1), 0.0, len)?, rel(rt, arg(args, 2), len, len)?);
    while k < fin {
        set_at(rt, o, k, arg(args, 0))?;
        k += 1.0;
    }
    Ok(o)
}

fn copy_within(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let o = this_val(rt, this)?;
    let len = len_of(rt, o)?;
    let (mut to, mut from) = (rel(rt, arg(args, 0), 0.0, len)?, rel(rt, arg(args, 1), 0.0, len)?);
    let fin = rel(rt, arg(args, 2), len, len)?;
    let mut count = (fin - from).min(len - to);
    let dir = if from < to && to < from + count {
        from += count - 1.0;
        to += count - 1.0;
        -1.0
    } else {
        1.0
    };
    while count > 0.0 {
        if has_at(rt, o, from)? {
            let v = get_at(rt, o, from)?;
            set_at(rt, o, to, v)?;
        } else {
            del_at(rt, o, to)?;
        }
        from += dir;
        to += dir;
        count -= 1.0;
    }
    Ok(o)
}

fn check_array_len(rt: &mut Realm, len: f64) -> R<()> {
    if len > crate::object::MAX_DENSE as f64 {
        return Err(rt.error(ErrKind::Range, "array length exceeds the dense array limit"));
    }
    Ok(())
}

/// The elements of an array-like as values, a hole as `undefined` (the `toX` copies).
fn read_all(rt: &mut Realm, o: Value, len: f64) -> R<Vec<Value>> {
    check_array_len(rt, len)?;
    let mut v = Vec::with_capacity(len as usize);
    let mut k = 0.0;
    while k < len {
        v.push(get_at(rt, o, k)?);
        k += 1.0;
    }
    Ok(v)
}

fn to_reversed(rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    let o = this_val(rt, this)?;
    let len = len_of(rt, o)?;
    check_array_len(rt, len)?;
    let mut v = Vec::with_capacity(len as usize);
    for i in (0..len as u32).rev() {
        v.push(get_at(rt, o, i as f64)?);
    }
    Ok(rt.new_array(&v))
}

fn to_spliced(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let o = this_val(rt, this)?;
    let len = len_of(rt, o)?;
    let start = rel(rt, arg(args, 0), 0.0, len)?;
    let skip = match args.len() {
        0 => 0.0,
        1 => len - start,
        _ => to_int(rt, args[1], 0.0)?.max(0.0).min(len - start),
    };
    let items = args.get(2..).unwrap_or(&[]);
    let new_len = len + items.len() as f64 - skip;
    if new_len > MAX_LEN {
        return rt.type_error("the length of the result would be above 2^53 - 1");
    }
    check_array_len(rt, new_len)?;
    let mut v = read_all(rt, o, start)?;
    v.extend_from_slice(items);
    let mut k = start + skip;
    while k < len {
        v.push(get_at(rt, o, k)?);
        k += 1.0;
    }
    Ok(rt.new_array(&v))
}

fn with(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let o = this_val(rt, this)?;
    let len = len_of(rt, o)?;
    let n = to_int(rt, arg(args, 0), 0.0)?;
    let at = if n >= 0.0 { n } else { len + n };
    if at >= len || at < 0.0 {
        return Err(rt.error(ErrKind::Range, "invalid index"));
    }
    check_array_len(rt, len)?;
    let mut v = Vec::with_capacity(len as usize);
    for i in 0..len as u32 {
        v.push(if i as f64 == at { arg(args, 1) } else { get_at(rt, o, i as f64)? });
    }
    Ok(rt.new_array(&v))
}
