//! The plain window functions of the HTML Standard: `structuredClone` ("structured data": the walk is in
//! `browser_navigation_state.rs`, shared with `history.state`) and `atob` / `btoa` ("forgiving-base64"). Logic of the base64
//! pair follows Boa's `core/wintertc/src/base64` (Unlicense OR MIT) for the shape and the HTML Standard for the rules.
//!
//! - `structuredClone(value, options)`: `options.transfer` is any iterable of `ArrayBuffer` and `MessagePort` objects (`Dom::transfer_list`;
//!   else `DataCloneError`). A listed buffer is moved: the result holds its bytes and the source is detached. A listed port moves to a
//!   new object. Errors are `DataCloneError` (see `browser_navigation_state.rs`).
//! - `btoa(data)`: a code unit above 0xFF is `InvalidCharacterError`. `atob(data)`: ASCII whitespace is dropped, the length must
//!   not be 1 mod 4 after up to two trailing `=` are removed (when the length is a multiple of 4), any other character that is
//!   not in the standard alphabet is `InvalidCharacterError`. The unused bits of the last character are dropped.

use crate::eng::Eng;
use crate::browser::*;
use crate::browser_scoped::*;
use bare_js_api::{Args, Attrs, HostFn, Local, Str, Type};

fn need<'s, E: Eng>(s: &mut Sc<'s, E>, a: Args<'s>, f: &str) -> Res<'s, ()> {
    match a.is_empty() {
        true => Err(s.type_error(&format!("Failed to execute '{f}' on 'Window': 1 argument required, but only 0 present."))),
        false => Ok(()),
    }
}

fn structured_clone<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    need::<E>(s, a, "structuredClone")?;
    let (value, options) = (arg_s::<E>(s, a, 0), arg_s::<E>(s, a, 1));
    let list = match s.type_of(options) {
        Type::Undefined | Type::Null => Vec::new(),
        Type::Object | Type::Function => {
            let t = s.get_str(options, "transfer")?;
            d.transfer_list(s, t, None, "Failed to execute 'structuredClone' on 'Window'")?
        }
        _ => return Err(s.type_error("Failed to execute 'structuredClone' on 'Window': The provided value is not of type 'StructuredSerializeOptions'.")),
    };
    crate::browser_navigation_state::clone_with(d, s, value, &list).map(|r| r.0)
}

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// The UTF-16 units of `ToString(v)`.
fn units_of<'s, E: Eng>(s: &mut Sc<'s, E>, v: Local<'s>) -> Res<'s, Vec<u16>> {
    let v = if s.type_of(v) == Type::String { v } else { s.to_string(v)? };
    let len = s.as_str(v).map_or(0, |t| t.len());
    s.charge((len / 32) as u64)?;
    Ok(match s.as_str(v) {
        Some(Str::Latin1(b)) => b.iter().map(|&c| c as u16).collect(),
        Some(Str::Utf16(u)) => u.to_vec(),
        None => Vec::new(),
    })
}

/// Standard base64 with padding.
pub(crate) fn base64(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            out.push(if i <= c.len() { ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

fn btoa<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    need::<E>(s, a, "btoa")?;
    let a0 = arg_s::<E>(s, a, 0);
    let u = units_of::<E>(s, a0)?;
    if u.iter().any(|&c| c > 0xFF) {
        return Err(d.dom_err_s(s, "InvalidCharacterError", "Failed to execute 'btoa' on 'Window': The string to be encoded contains characters outside of the Latin1 range."));
    }
    let mut out: Vec<u16> = Vec::with_capacity(u.len().div_ceil(3) * 4);
    for c in u.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            out.push(if i <= c.len() { ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as u16 } else { b'=' as u16 });
        }
    }
    s.string_utf16(&out)
}

fn atob<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, _: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    need::<E>(s, a, "atob")?;
    let a0 = arg_s::<E>(s, a, 0);
    let mut u = units_of::<E>(s, a0)?;
    u.retain(|&c| !matches!(c, 9 | 10 | 12 | 13 | 32));
    if u.len() % 4 == 0 {
        for _ in 0..2 {
            if u.last() == Some(&(b'=' as u16)) {
                u.pop();
            }
        }
    }
    let bad = "Failed to execute 'atob' on 'Window': The string to be decoded is not correctly encoded.";
    if u.len() % 4 == 1 {
        return Err(d.dom_err_s(s, "InvalidCharacterError", bad));
    }
    let mut out: Vec<u16> = Vec::with_capacity(u.len() / 4 * 3 + 2);
    let (mut acc, mut bits) = (0u32, 0u32);
    for &c in &u {
        let Some(v) = u8::try_from(c).ok().and_then(|c| ALPHABET.iter().position(|&x| x == c)) else { return Err(d.dom_err_s(s, "InvalidCharacterError", bad)) };
        acc = acc << 6 | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits & 0xFF) as u16);
        }
    }
    s.string_utf16(&out)
}

pub(crate) fn install<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>) -> Res<'s, ()> {
    let g = s.global();
    for (name, h, len) in [("structuredClone", structured_clone as HS<E>, 1), ("btoa", btoa as HS<E>, 1), ("atob", atob as HS<E>, 1)] {
        let f = d.add_fn_s(s, h, name, len, HostFn::Method)?;
        s.define_str(g, name, f, Attrs::ALL)?;
    }
    Ok(())
}
