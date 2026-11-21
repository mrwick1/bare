//! The URI functions of the global object: `encodeURI`, `encodeURIComponent`, `decodeURI` and `decodeURIComponent` (ECMA-262 19.2.6).
//! They are lazy entries of the global table, so a realm that never names one pays nothing. A lone surrogate in an encode and a bad
//! escape in a decode throw a `URIError`. `escape` and `unescape` (Annex B) are not here yet.
use crate::builtins::arg;
use crate::interp_rt::{ErrKind, R, Realm};
use crate::lazy::{Ent, m};
use crate::string::MAX_LEN;
use crate::value::Value;

/// The characters `encodeURI` and `decodeURI` leave as they are besides those of the component forms (uriReserved and `#`).
const RESERVED: &str = ";/?:@&=+$,#";

impl Realm {
    pub(crate) fn install_uri(&mut self) {
        static URI_FUNCTIONS: &[Ent] = &[
            m("encodeURI", 1, encode_uri),
            m("encodeURIComponent", 1, encode_uri_component),
            m("decodeURI", 1, decode_uri),
            m("decodeURIComponent", 1, decode_uri_component),
        ];
        let g = self.global;
        self.lazy_table(g, URI_FUNCTIONS);
    }

    fn uri_error<T>(&mut self) -> R<T> {
        let msg = self.string("URI malformed");
        Err(self.make_error(self.error_protos[6], Some(msg)))
    }
}

/// Percent-encode the UTF-8 bytes of everything but `A-Za-z0-9-_.!~*'()` and `extra`. `None` for a lone surrogate.
fn encode(units: &[u16], extra: &str) -> Option<Vec<u16>> {
    let mut out = Vec::with_capacity(units.len());
    for c in char::decode_utf16(units.iter().copied()) {
        let c = c.ok()?;
        if c.is_ascii_alphanumeric() || c.is_ascii() && ("-_.!~*'()".contains(c) || extra.contains(c)) {
            out.push(c as u16);
        } else {
            for b in c.encode_utf8(&mut [0; 4]).bytes() {
                out.extend(format!("%{b:02X}").encode_utf16());
            }
        }
    }
    Some(out)
}

/// Decode each `%XX` run as one UTF-8 character. An escape of an ASCII character in `reserved` stays as it is. `None` for a bad escape
/// or bad UTF-8.
fn decode(units: &[u16], reserved: &str) -> Option<Vec<u16>> {
    let hex = |i: usize| -> Option<u8> {
        let h = |u: u16| char::from_u32(u as u32)?.to_digit(16);
        if *units.get(i)? != b'%' as u16 {
            return None;
        }
        Some((h(*units.get(i + 1)?)? * 16 + h(*units.get(i + 2)?)?) as u8)
    };
    let mut out = Vec::with_capacity(units.len());
    let mut k = 0;
    while k < units.len() {
        if units[k] != b'%' as u16 {
            out.push(units[k]);
            k += 1;
            continue;
        }
        let b = hex(k)?;
        if b < 0x80 {
            if reserved.contains(b as char) {
                out.extend_from_slice(&units[k..k + 3]);
            } else {
                out.push(b as u16);
            }
            k += 3;
            continue;
        }
        let n = b.leading_ones() as usize;
        if !(2..=4).contains(&n) {
            return None;
        }
        let mut bytes = [b, 0, 0, 0];
        for (j, slot) in bytes.iter_mut().enumerate().take(n).skip(1) {
            *slot = hex(k + 3 * j).filter(|c| c & 0xC0 == 0x80)?;
        }
        // `from_utf8` refuses overlong forms, surrogates and values above U+10FFFF.
        let c = std::str::from_utf8(&bytes[..n]).ok()?.chars().next()?;
        out.extend_from_slice(c.encode_utf16(&mut [0; 2]));
        k += 3 * n;
    }
    Some(out)
}

fn run(rt: &mut Realm, args: &[Value], f: fn(&[u16], &str) -> Option<Vec<u16>>, set: &str) -> R<Value> {
    let s = rt.to_str(arg(args, 0))?;
    let u = rt.units(s);
    match f(&u, set) {
        Some(out) if out.len() > MAX_LEN => Err(rt.error(ErrKind::Range, "invalid string length")),
        Some(out) => Ok(rt.make_units(&out)),
        None => rt.uri_error(),
    }
}

fn encode_uri(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    run(rt, args, encode, RESERVED)
}

fn encode_uri_component(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    run(rt, args, encode, "")
}

fn decode_uri(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    run(rt, args, decode, RESERVED)
}

fn decode_uri_component(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    run(rt, args, decode, "")
}
