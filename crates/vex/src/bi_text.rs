//! UTF-8 TextEncoder and TextDecoder. Decoder state holds at most three unfinished bytes.
use crate::bi_buffer::{View, instance_proto, make_ctor, plain_proto, range, tag};
use crate::bi_typed::Element;
use crate::builtins::{NativeFn, arg};
use crate::interp::STOP;
use crate::interp_rt::{Key, R, Realm};
use crate::lazy::{Ent, factory};
use crate::object::{K_TEXT_DECODER, K_TEXT_ENCODER, Obj};
use crate::shapes::{A_C, A_E, A_W};
use crate::value::Value;
use std::borrow::Cow;

pub(crate) static GLOBALS: &[Ent] = &[factory("TextEncoder", encoder), factory("TextDecoder", decoder)];

const FATAL: i32 = 1;
const IGNORE_BOM: i32 = 2;
const BOM_SEEN: i32 = 4;

fn accessor(rt: &mut Realm, proto: Obj, name: &str, display_name: &'static str, f: NativeFn) {
    let key = Key::Name(rt.intern(name));
    let get = rt.new_native(f, false, 0, display_name);
    rt.define_accessor(proto, key, get.value(), false, true).expect("define");
}

fn method(rt: &mut Realm, proto: Obj, name: &'static str, len: u8, f: NativeFn) {
    let key = Key::Name(rt.intern(name));
    let method = rt.new_native(f, false, len, name);
    rt.define_attrs(proto, key, A_W | A_E | A_C, method.value()).expect("define");
}

fn encoder(rt: &mut Realm) -> Value {
    let proto = plain_proto(rt, rt.object_proto);
    tag(rt, proto, "TextEncoder");
    accessor(rt, proto, "encoding", "get encoding", encoder_encoding);
    method(rt, proto, "encode", 0, encode);
    method(rt, proto, "encodeInto", 2, encode_into);
    make_ctor(rt, "TextEncoder", 0, encoder_ctor, proto).value()
}

fn decoder(rt: &mut Realm) -> Value {
    let proto = plain_proto(rt, rt.object_proto);
    tag(rt, proto, "TextDecoder");
    accessor(rt, proto, "encoding", "get encoding", decoder_encoding);
    accessor(rt, proto, "fatal", "get fatal", decoder_fatal);
    accessor(rt, proto, "ignoreBOM", "get ignoreBOM", decoder_ignore_bom);
    method(rt, proto, "decode", 0, decode);
    make_ctor(rt, "TextDecoder", 0, decoder_ctor, proto).value()
}

fn brand(rt: &mut Realm, this: Value, kind: u32) -> R<Obj> {
    Realm::obj(this).filter(|o| o.is_kind(kind)).ok_or_else(|| rt.error(crate::interp_rt::ErrKind::Type, "incompatible text codec receiver"))
}

fn work(rt: &mut Realm, n: usize) -> R<()> {
    rt.fuel = rt.fuel.checked_sub((n / 4) as u64).ok_or(STOP)?;
    Ok(())
}

fn encoder_ctor(rt: &mut Realm, _: Value, _: &[Value]) -> R<Value> {
    let proto = instance_proto(rt)?;
    let o = plain_proto(rt, proto);
    o.set_kind(K_TEXT_ENCODER);
    Ok(o.value())
}

fn encoder_encoding(rt: &mut Realm, this: Value, _: &[Value]) -> R<Value> {
    brand(rt, this, K_TEXT_ENCODER)?;
    Ok(rt.string("utf-8"))
}

fn encode(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    brand(rt, this, K_TEXT_ENCODER)?;
    let src = arg(args, 0);
    let text = if src.is_undefined() {
        String::new()
    } else {
        let src = rt.to_str(src)?;
        work(rt, src.len())?;
        String::from_utf16_lossy(&rt.units(src))
    };
    let proto = rt.buffer_intrinsic("Uint8Array");
    let o = rt.alloc_typed(proto, Element::U8, text.len())?;
    let view = View::of(o).expect("Uint8Array");
    rt.buffer_bytes_mut(view.buffer).expect("buffer").copy_from_slice(text.as_bytes());
    Ok(o.value())
}

fn encode_into(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    brand(rt, this, K_TEXT_ENCODER)?;
    let src = rt.to_str(arg(args, 0))?;
    let Some(dest) = Realm::obj(arg(args, 1)).filter(|&o| View::of(o).is_some_and(|v| v.elem == Some(Element::U8))) else {
        return rt.type_error("the destination must be a Uint8Array");
    };
    let (buf, span) = rt.byte_range(dest).expect("Uint8Array byte range");
    if crate::bi_buffer::immutable_buffer(buf) || crate::shared_memory::is_shared(buf) {
        return rt.type_error("destination must be a writable unshared buffer");
    }
    // Inspect at most one code point past the destination capacity.
    let count = src.len().min(span.len().saturating_add(2));
    work(rt, count)?;
    let src = rt.flat(src);
    let units = src.chars().expect("flat string");
    let data = &mut rt.buffer_bytes_mut(buf).expect("buffer")[span];
    let (mut read, mut written) = (0, 0);
    for c in char::decode_utf16(units.units().take(count)) {
        let c = c.unwrap_or(char::REPLACEMENT_CHARACTER);
        if c.len_utf8() > data.len() - written {
            break;
        }
        written += c.encode_utf8(&mut data[written..]).len();
        read += c.len_utf16();
    }
    let o = plain_proto(rt, rt.object_proto);
    rt.set_own(o, "read", Realm::num(read as f64));
    rt.set_own(o, "written", Realm::num(written as f64));
    Ok(o.value())
}

fn option(rt: &mut Realm, o: Value, name: &str) -> R<bool> {
    if o.is_undefined() || o.is_null() {
        return Ok(false);
    }
    if Realm::obj(o).is_none() {
        return rt.type_error("text decoder options must be an object");
    }
    Ok(Realm::truthy(rt.get_named(o, name)?))
}

fn decoder_ctor(rt: &mut Realm, _: Value, args: &[Value]) -> R<Value> {
    let label = arg(args, 0);
    let label = if label.is_undefined() {
        "utf-8".into()
    } else {
        let label = rt.to_str(label)?;
        work(rt, label.len())?;
        rt.text(label)
    };
    let options = arg(args, 1);
    let fatal = option(rt, options, "fatal")?;
    let ignore_bom = option(rt, options, "ignoreBOM")?;
    if !matches!(label.trim_ascii().to_ascii_lowercase().as_str(),
        "utf-8" | "utf8" | "unicode-1-1-utf-8" | "unicode11utf8" | "unicode20utf8" | "x-unicode20utf8")
    {
        return range(rt, &format!("The \"{label}\" encoding is not supported"));
    }
    let proto = instance_proto(rt)?;
    let flags = (i32::from(fatal) * FATAL) | (i32::from(ignore_bom) * IGNORE_BOM);
    rt.buffer_object(proto, K_TEXT_DECODER, &[Value::int(flags), Value::int(0)]).map(Obj::value)
}

fn decoder_encoding(rt: &mut Realm, this: Value, _: &[Value]) -> R<Value> {
    brand(rt, this, K_TEXT_DECODER)?;
    Ok(rt.string("utf-8"))
}

fn decoder_fatal(rt: &mut Realm, this: Value, _: &[Value]) -> R<Value> {
    let o = brand(rt, this, K_TEXT_DECODER)?;
    Ok(Value::bool(flags(rt, o) & FATAL != 0))
}

fn decoder_ignore_bom(rt: &mut Realm, this: Value, _: &[Value]) -> R<Value> {
    let o = brand(rt, this, K_TEXT_DECODER)?;
    Ok(Value::bool(flags(rt, o) & IGNORE_BOM != 0))
}

fn flags(rt: &Realm, o: Obj) -> i32 {
    o.slot(&rt.shapes, 0).as_int().expect("decoder flags")
}

fn decode(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let o = brand(rt, this, K_TEXT_DECODER)?;
    let src = arg(args, 0);
    let buffer = if src.is_undefined() {
        None
    } else {
        let Some(b) = Realm::obj(src).filter(|&b| rt.byte_range(b).is_some()) else {
            return rt.type_error("the input must be an ArrayBuffer or an ArrayBuffer view");
        };
        Some(b)
    };
    let stream = option(rt, arg(args, 1), "stream")?;
    let mut flags = flags(rt, o);
    let pending = o.slot(&rt.shapes, 1).as_int().expect("unfinished bytes").to_le_bytes();
    let pending = &pending[..pending.iter().position(|&b| b == 0).expect("at most three bytes")];
    let span = buffer.and_then(|b| rt.byte_range(b));
    work(rt, span.as_ref().map_or(0, |(_, span)| span.len()))?;
    let data = span.map_or(&[][..], |(buf, span)| &rt.buffer_bytes(buf).expect("buffer")[span]);
    let data = if pending.is_empty() {
        Cow::Borrowed(data)
    } else {
        Cow::Owned([pending, data].concat())
    };
    let mut text = String::new();
    let mut tail = [0; 4];
    let mut chunks = data.utf8_chunks().peekable();
    let mut malformed = false;
    while let Some(chunk) = chunks.next() {
        text.push_str(chunk.valid());
        let bad = chunk.invalid();
        if bad.is_empty() {
            continue;
        }
        if stream && chunks.peek().is_none() && std::str::from_utf8(bad).is_err_and(|e| e.error_len().is_none()) {
            tail[..bad.len()].copy_from_slice(bad);
        } else if flags & FATAL != 0 {
            malformed = true;
            break;
        } else {
            text.push('\u{fffd}');
        }
    }
    o.set_slot(&rt.shapes, 1, Value::int(i32::from_le_bytes(tail)));
    let strip_bom = flags & (IGNORE_BOM | BOM_SEEN) == 0;
    if !malformed && !text.is_empty() {
        flags |= BOM_SEEN;
    }
    o.set_slot(&rt.shapes, 0, Value::int(if stream { flags } else { flags & !BOM_SEEN }));
    if malformed {
        return rt.type_error("the encoded data is not valid UTF-8");
    }
    let out = if strip_bom { text.strip_prefix('\u{feff}').unwrap_or(&text) } else { &text };
    if out.len() > crate::string::MAX_LEN && out.encode_utf16().count() > crate::string::MAX_LEN {
        return range(rt, "decoded string is too long");
    }
    Ok(rt.string(out))
}

#[cfg(test)]
#[path = "bi_text_tests.rs"]
mod tests;
