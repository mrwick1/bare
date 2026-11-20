//! `Array` and the `Array.prototype` methods that do not call a script function (plan step S2.7). `forEach`, `map`, `filter`, `reduce`,
//! `some`, `every`, `find`, `findIndex` and `sort` call back, so they are in `prelude.js` (see `builtins.rs`).
//!
//! A method works on an array or an arguments object (an indexed object); any other `this` throws a `TypeError` (the generic
//! array-like form is a known gap). A hole is read as `undefined`, except in `join` (empty text), `indexOf` and `includes`.
//! `shift` and `unshift` move all the elements: O(n) each.
use crate::builtins::{NativeFn, arg};
use crate::interp_rt::{ErrKind, R, Realm};
use crate::object::Obj;
use crate::string::Str;
use crate::value::Value;

/// A relative index (`slice`, `splice`, `indexOf`): `v` is `ToIntegerOrInfinity`; a negative value counts from `len` when `neg` is true,
/// else it is 0; the result is at most `len`. `default` stands for `undefined`.
pub(crate) fn rel_index(rt: &mut Realm, v: Value, default: f64, len: f64, neg: bool) -> R<usize> {
    let n = if v.is_undefined() { default } else { rt.to_number(v)? };
    let n = if n.is_nan() { 0.0 } else { n.trunc() };
    let n = if n < 0.0 && neg { (len + n).max(0.0) } else { n.max(0.0) };
    Ok(n.min(len) as usize)
}

impl Realm {
    pub(crate) fn install_array(&mut self) {
        let proto = self.array_proto;
        let c = self.ctor("Array", array_ctor, proto);
        self.method(c, "isArray", is_array);
        for (n, f) in [
            ("push", push as NativeFn),
            ("pop", pop),
            ("shift", shift),
            ("unshift", unshift),
            ("slice", slice),
            ("splice", splice),
            ("concat", concat),
            ("join", join),
            ("indexOf", index_of),
            ("lastIndexOf", last_index_of),
            ("includes", includes),
            ("reverse", reverse),
            ("toString", to_string),
        ] {
            self.method(proto, n, f);
        }
    }
}

fn this_arr(rt: &mut Realm, this: Value) -> R<Obj> {
    match Realm::obj(this).filter(|o| o.is_indexed()) {
        Some(o) => Ok(o),
        None => rt.type_error("an Array method was called on a value that is not an array"),
    }
}

fn elems(o: Obj) -> Vec<Value> {
    (0..o.len()).map(|i| o.get_element(i).unwrap_or(Value::HOLE)).collect()
}

/// Replace the elements of `o` by `vals`.
fn set_all(rt: &mut Realm, o: Obj, vals: &[Value]) -> R<()> {
    o.set_len(&mut rt.heap, 0).expect("memory");
    for (i, &v) in vals.iter().enumerate() {
        if v.is_hole() {
            continue;
        }
        o.set_element(&mut rt.heap, i as u32, v).expect("memory");
    }
    o.set_len(&mut rt.heap, vals.len() as u32).expect("memory");
    Ok(())
}

fn array_ctor(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let proto = rt.new_proto()?;
    let shape = rt.shapes.root(crate::cell::Layout::Indexed, Some(proto.cell()), 0);
    let o = match args {
        [n] if n.is_number() => {
            let f = n.as_f64().expect("a number");
            if f < 0.0 || f.fract() != 0.0 || f >= u32::MAX as f64 {
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

fn is_array(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    Ok(Value::bool(Realm::obj(arg(args, 0)).is_some_and(|o| rt.is_array(o))))
}

fn push(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let o = this_arr(rt, this)?;
    let mut n = o.len();
    for &v in args {
        if n >= u32::MAX - 1 || o.set_element(&mut rt.heap, n, v).is_none() {
            return Err(rt.error(ErrKind::Range, "invalid array length"));
        }
        n += 1;
    }
    Ok(Realm::num(n as f64))
}

fn pop(rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    let o = this_arr(rt, this)?;
    let n = o.len();
    if n == 0 {
        return Ok(Value::UNDEFINED);
    }
    let v = o.get_element(n - 1).unwrap_or(Value::UNDEFINED);
    o.set_len(&mut rt.heap, n - 1).expect("memory");
    Ok(v)
}

fn shift(rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    let o = this_arr(rt, this)?;
    let v = elems(o);
    let Some((first, rest)) = v.split_first() else { return Ok(Value::UNDEFINED) };
    set_all(rt, o, rest)?;
    Ok(if first.is_hole() { Value::UNDEFINED } else { *first })
}

fn unshift(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let o = this_arr(rt, this)?;
    let all = [args, &elems(o)].concat();
    set_all(rt, o, &all)?;
    Ok(Realm::num(all.len() as f64))
}

fn slice(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let o = this_arr(rt, this)?;
    let len = o.len() as f64;
    let a = rel_index(rt, arg(args, 0), 0.0, len, true)?;
    let b = rel_index(rt, arg(args, 1), len, len, true)?;
    let v = elems(o);
    Ok(rt.new_array(if a < b { &v[a..b] } else { &[] }))
}

fn splice(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let o = this_arr(rt, this)?;
    let len = o.len() as f64;
    let a = rel_index(rt, arg(args, 0), 0.0, len, true)?;
    let count = match args.len() {
        0 => 0,
        1 => o.len() as usize - a,
        _ => {
            let n = rt.to_number(args[1])?;
            let n = if n.is_nan() { 0.0 } else { n.trunc() };
            n.max(0.0).min(len - a as f64) as usize
        }
    };
    let mut v = elems(o);
    let removed: Vec<Value> = v.splice(a..a + count, args.iter().skip(2).copied()).collect();
    set_all(rt, o, &v)?;
    Ok(rt.new_array(&removed))
}

fn concat(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let o = this_arr(rt, this)?;
    let mut v = elems(o);
    for &a in args {
        match Realm::obj(a).filter(|x| rt.is_array(*x)) {
            Some(x) => v.extend(elems(x)),
            None => v.push(a),
        }
    }
    Ok(rt.new_array(&v))
}

fn join_with(rt: &mut Realm, o: Obj, sep: &[u16]) -> R<Value> {
    let mut out: Vec<u16> = Vec::new();
    for i in 0..o.len() {
        if i > 0 {
            out.extend(sep);
        }
        match o.get_element(i) {
            None => {}
            Some(v) if v.is_undefined() || v.is_null() => {}
            Some(v) => {
                let s: Str = rt.to_str(v)?;
                rt.push_units(&mut out, s);
            }
        }
    }
    Ok(rt.make_units(&out))
}

fn join(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let o = this_arr(rt, this)?;
    let sep = if arg(args, 0).is_undefined() {
        vec![b',' as u16]
    } else {
        let s = rt.to_str(args[0])?;
        rt.units(s)
    };
    join_with(rt, o, &sep)
}

fn to_string(rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    match Realm::obj(this).filter(|o| o.is_indexed()) {
        Some(o) => join_with(rt, o, &[b',' as u16]),
        None => crate::builtins::object_to_string(rt, this, &[]),
    }
}

fn index_of(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let o = this_arr(rt, this)?;
    let len = o.len() as f64;
    let from = rel_index(rt, arg(args, 1), 0.0, len, true)?;
    for i in from..o.len() as usize {
        if let Some(v) = o.get_element(i as u32)
            && rt.strict_eq(v, arg(args, 0))
        {
            return Ok(Value::int(i as i32));
        }
    }
    Ok(Value::int(-1))
}

fn last_index_of(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let o = this_arr(rt, this)?;
    if o.is_empty() {
        return Ok(Value::int(-1));
    }
    let len = o.len() as f64;
    let from = if args.len() > 1 { rel_index(rt, args[1], len - 1.0, len - 1.0, true)? } else { o.len() as usize - 1 };
    for i in (0..=from.min(o.len() as usize - 1)).rev() {
        if let Some(v) = o.get_element(i as u32)
            && rt.strict_eq(v, arg(args, 0))
        {
            return Ok(Value::int(i as i32));
        }
    }
    Ok(Value::int(-1))
}

fn includes(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let o = this_arr(rt, this)?;
    let len = o.len() as f64;
    let from = rel_index(rt, arg(args, 1), 0.0, len, true)?;
    let x = arg(args, 0);
    let nan = x.as_f64().is_some_and(f64::is_nan);
    for i in from..o.len() as usize {
        // `includes` reads a hole as `undefined` and counts `NaN` as equal to itself.
        let v = o.get_element(i as u32).unwrap_or(Value::UNDEFINED);
        if rt.strict_eq(v, x) || (nan && v.as_f64().is_some_and(f64::is_nan)) {
            return Ok(Value::TRUE);
        }
    }
    Ok(Value::FALSE)
}

fn reverse(rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    let o = this_arr(rt, this)?;
    let mut v = elems(o);
    v.reverse();
    set_all(rt, o, &v)?;
    Ok(this)
}
