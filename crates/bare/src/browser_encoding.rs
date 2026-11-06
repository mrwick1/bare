//! The plain window functions of the HTML Standard: `structuredClone` ("structured data": the walk is in
//! `browser_navigation_state.rs`, shared with `history.state`) and `atob` / `btoa` ("forgiving-base64"). Logic of the base64
//! pair follows Boa's `core/wintertc/src/base64` (Unlicense OR MIT) for the shape and the HTML Standard for the rules.
//!
//! - `structuredClone(value, options)`: `options.transfer` must be absent, undefined or empty. A transfer list with items is
//!   `Unsupported` (a transfer moves a buffer: not done). Errors are `DataCloneError` (see `browser_navigation_state.rs`).
//! - `btoa(data)`: a code unit above 0xFF is `InvalidCharacterError`. `atob(data)`: ASCII whitespace is dropped, the length must
//!   not be 1 mod 4 after up to two trailing `=` are removed (when the length is a multiple of 4), any other character that is
//!   not in the standard alphabet is `InvalidCharacterError`. The unused bits of the last character are dropped.

use crate::browser::*;
use bare_js::eval::Error;
use bare_js::eval::Interp;
use bare_js::value::Value;

fn arg(a: &[Value], i: usize) -> Value {
    a.get(i).copied().unwrap_or(Value::Undefined)
}

fn need(it: &mut Interp<'_>, a: &[Value], f: &str) -> R<()> {
    match a.is_empty() {
        true => Err(it.type_error(&format!("Failed to execute '{f}' on 'Window': 1 argument required, but only 0 present."))),
        false => Ok(()),
    }
}

fn structured_clone(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> R<Value> {
    need(it, a, "structuredClone")?;
    match arg(a, 1) {
        Value::Undefined | Value::Null => {}
        o @ Value::Object(_) => {
            let k = d.key(it, "transfer")?;
            let t = it.get_value(o, k)?;
            if !matches!(t, Value::Undefined) {
                let Some(mut iter) = it.host_iter_open(t)? else {
                    return Err(it.type_error("Failed to execute 'structuredClone' on 'Window': The provided value cannot be converted to a sequence."));
                };
                let first = it.host_iter_next(&mut iter)?;
                if first.is_some() {
                    it.host_iter_close(&mut iter)?;
                    return Err(Error::Unsupported(unsupported("structuredClone with a transfer list")));
                }
            }
        }
        _ => return Err(it.type_error("Failed to execute 'structuredClone' on 'Window': The provided value is not of type 'StructuredSerializeOptions'.")),
    }
    crate::browser_navigation_state::clone(d, it, arg(a, 0))
}

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn units_of(d: &Dom, it: &mut Interp<'_>, v: Value) -> R<Vec<u16>> {
    let s = it.to_js_string(v)?;
    let len = it.heap.str_len(s).map_err(Error::Heap)?;
    if len > d.lim.max_string_bytes {
        return Err(d.lim("dom-string"));
    }
    it.charge((len / 32) as u64)?;
    match it.heap.str_units(s) {
        Ok(u) => Ok(u.to_vec()),
        Err(e) => Err(it.heap_error(e)),
    }
}

fn btoa(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> R<Value> {
    need(it, a, "btoa")?;
    let u = units_of(d, it, a[0])?;
    if u.iter().any(|&c| c > 0xFF) {
        return Err(d.dom_err(it, "InvalidCharacterError", "Failed to execute 'btoa' on 'Window': The string to be encoded contains characters outside of the Latin1 range."));
    }
    let mut out: Vec<u16> = Vec::with_capacity(u.len().div_ceil(3) * 4);
    for c in u.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            out.push(if i <= c.len() { ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as u16 } else { b'=' as u16 });
        }
    }
    match it.heap.intern_units(&out) {
        Ok(s) => Ok(Value::String(s)),
        Err(e) => Err(it.heap_error(e)),
    }
}

fn atob(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> R<Value> {
    need(it, a, "atob")?;
    let mut u = units_of(d, it, a[0])?;
    u.retain(|&c| !matches!(c, 9 | 10 | 12 | 13 | 32));
    if u.len() % 4 == 0 {
        for _ in 0..2 {
            if u.last() == Some(&(b'=' as u16)) {
                u.pop();
            }
        }
    }
    let bad = |d: &Dom, it: &mut Interp<'_>| d.dom_err(it, "InvalidCharacterError", "Failed to execute 'atob' on 'Window': The string to be decoded is not correctly encoded.");
    if u.len() % 4 == 1 {
        return Err(bad(d, it));
    }
    let mut out: Vec<u16> = Vec::with_capacity(u.len() / 4 * 3 + 2);
    let (mut acc, mut bits) = (0u32, 0u32);
    for &c in &u {
        let Some(v) = u8::try_from(c).ok().and_then(|c| ALPHABET.iter().position(|&x| x == c)) else { return Err(bad(d, it)) };
        acc = acc << 6 | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits & 0xFF) as u16);
        }
    }
    match it.heap.intern_units(&out) {
        Ok(s) => Ok(Value::String(s)),
        Err(e) => Err(it.heap_error(e)),
    }
}

pub(crate) fn install(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    let g = *d.global.get().expect("installed");
    for (name, h, len) in [("structuredClone", structured_clone as H, 1), ("btoa", btoa as H, 1), ("atob", atob as H, 1)] {
        let f = d.add_fn(it, h, name, len, false)?;
        let k = d.key(it, name)?;
        it.define_data(g, k, Value::Object(f), true, true, true)?;
    }
    Ok(())
}
