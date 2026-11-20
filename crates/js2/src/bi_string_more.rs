//! The rest of `String` and `String.prototype`: `raw`, `fromCodePoint`, `normalize`, the locale case methods, `isWellFormed`,
//! `toWellFormed`, and `match`, `matchAll` and `search`. js2 has no `RegExp`, so the last three check `this` and throw a `TypeError`.
//! `normalize` does NFC and NFD with `bare-unicode`; NFKC and NFKD need the compatibility data of ICU (`bare-js` bakes it) and throw a `TypeError`.
use crate::bi_array::len_of;
use crate::builtins::{NativeFn, arg};
use crate::interp_rt::{ErrKind, Key, R, Realm};
use crate::value::Value;

impl Realm {
    pub(crate) fn install_string_more(&mut self) {
        let proto = self.string_proto;
        let c = self.global_string_ctor();
        self.method(c, "raw", 1, raw);
        self.method(c, "fromCodePoint", 1, from_code_point);
        for (n, len, f) in [
            ("normalize", 0, normalize as NativeFn),
            ("toLocaleLowerCase", 0, to_locale_lower),
            ("toLocaleUpperCase", 0, to_locale_upper),
            ("isWellFormed", 0, is_well_formed),
            ("toWellFormed", 0, to_well_formed),
            ("match", 1, no_regexp_match),
            ("matchAll", 1, no_regexp_match),
            ("search", 1, no_regexp_search),
        ] {
            self.method(proto, n, len, f);
        }
    }

    /// The `String` constructor (the global).
    fn global_string_ctor(&mut self) -> crate::object::Obj {
        let k = self.intern("String");
        let g = self.global;
        let v = self.get_prop(g.value(), Key::Name(k)).expect("String is installed");
        Realm::obj(v).expect("a function")
    }

    /// `RequireObjectCoercible(this)` and `ToString`, for the methods below.
    fn coerce_this(&mut self, this: Value, name: &str) -> R<crate::string::Str> {
        if this.is_undefined() || this.is_null() {
            return self.type_error(&format!("String.prototype.{name} called on null or undefined"));
        }
        self.to_str(this)
    }
}

/// `String.raw(template, ...substitutions)`.
fn raw(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let cooked = arg(args, 0);
    if cooked.is_undefined() || cooked.is_null() {
        return rt.type_error("String.raw called on undefined or null");
    }
    let raw = rt.get_named(cooked, "raw")?;
    if raw.is_undefined() || raw.is_null() {
        return rt.type_error("String.raw needs a template object with a raw property");
    }
    let len = len_of(rt, raw)?;
    let mut out: Vec<u16> = Vec::new();
    let mut i = 0.0;
    while i < len {
        let seg = rt.get_prop(raw, Key::Idx(i as u32))?;
        let seg = rt.to_str(seg)?;
        rt.push_units(&mut out, seg);
        if i + 1.0 == len {
            break;
        }
        if let Some(&sub) = args.get(i as usize + 1) {
            let s = rt.to_str(sub)?;
            rt.push_units(&mut out, s);
        }
        i += 1.0;
    }
    Ok(rt.make_units(&out))
}

fn from_code_point(rt: &mut Realm, _this: Value, args: &[Value]) -> R<Value> {
    let mut out: Vec<u16> = Vec::with_capacity(args.len());
    for &a in args {
        let n = rt.to_number(a)?;
        if n.fract() != 0.0 || !(0.0..=1114111.0).contains(&n) {
            return Err(rt.error(ErrKind::Range, "invalid code point"));
        }
        let c = n as u32;
        if c < 0x10000 {
            out.push(c as u16);
        } else {
            let c = c - 0x10000;
            out.push(0xd800 + (c >> 10) as u16);
            out.push(0xdc00 + (c & 0x3ff) as u16);
        }
    }
    Ok(rt.make_units(&out))
}

fn normalize(rt: &mut Realm, this: Value, args: &[Value]) -> R<Value> {
    let s = rt.coerce_this(this, "normalize")?;
    let form = if arg(args, 0).is_undefined() {
        "NFC".to_string()
    } else {
        let f = rt.to_str(args[0])?;
        rt.text(f)
    };
    let nfc = match form.as_str() {
        "NFC" => true,
        "NFD" => false,
        "NFKC" | "NFKD" => return rt.type_error("normalize: NFKC and NFKD are not supported yet in js2 (no compatibility data)"),
        _ => return Err(rt.error(ErrKind::Range, "the normalization form must be one of NFC, NFD, NFKC, NFKD")),
    };
    let src = rt.units(s);
    let out = normalize_units(&src, nfc);
    if out == src { Ok(s.value()) } else { Ok(rt.make_units(&out)) }
}

/// NFC or NFD of the units `src`. A run of well-formed text is normalized; a lone surrogate stays as it is.
pub(crate) fn normalize_units(src: &[u16], nfc: bool) -> Vec<u16> {
    // Below U+00C0 (U+0300 for NFC) no unit changes in NFC or NFD.
    if src.iter().all(|&c| c < if nfc { 0x300 } else { 0xC0 }) {
        return src.to_vec();
    }
    let (mut out, mut seg): (Vec<u16>, String) = (Vec::with_capacity(src.len()), String::new());
    let flush = |seg: &mut String, out: &mut Vec<u16>| {
        let m = if nfc { bare_unicode::nfc(seg) } else { bare_unicode::nfd(seg) };
        out.extend(m.encode_utf16());
        seg.clear();
    };
    for r in char::decode_utf16(src.iter().copied()) {
        match r {
            Ok(c) => seg.push(c),
            Err(e) => {
                flush(&mut seg, &mut out);
                out.push(e.unpaired_surrogate());
            }
        }
    }
    flush(&mut seg, &mut out);
    out
}

fn locale_case(rt: &mut Realm, this: Value, name: &str, f: fn(&str) -> String) -> R<Value> {
    let s = rt.coerce_this(this, name)?;
    let u = rt.units(s);
    // A lone surrogate has no case: map each well-formed run, keep the others.
    let mut out: Vec<u16> = Vec::with_capacity(u.len());
    let mut seg = String::new();
    for r in char::decode_utf16(u.iter().copied()) {
        match r {
            Ok(c) => seg.push(c),
            Err(e) => {
                out.extend(f(&seg).encode_utf16());
                seg.clear();
                out.push(e.unpaired_surrogate());
            }
        }
    }
    out.extend(f(&seg).encode_utf16());
    Ok(rt.make_units(&out))
}

fn to_locale_lower(rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    locale_case(rt, this, "toLocaleLowerCase", |s| s.to_lowercase())
}

fn to_locale_upper(rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    locale_case(rt, this, "toLocaleUpperCase", |s| s.to_uppercase())
}

fn is_well_formed(rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    let s = rt.coerce_this(this, "isWellFormed")?;
    let u = rt.units(s);
    Ok(Value::bool(char::decode_utf16(u.iter().copied()).all(|r| r.is_ok())))
}

fn to_well_formed(rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    let s = rt.coerce_this(this, "toWellFormed")?;
    let u = rt.units(s);
    let out: Vec<u16> = char::decode_utf16(u.iter().copied()).flat_map(|r| r.unwrap_or('\u{fffd}').encode_utf16(&mut [0; 2]).to_vec()).collect();
    Ok(rt.make_units(&out))
}

fn no_regexp_match(rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    rt.coerce_this(this, "match")?;
    rt.type_error("match and matchAll need a RegExp, and js2 has none yet")
}

fn no_regexp_search(rt: &mut Realm, this: Value, _args: &[Value]) -> R<Value> {
    rt.coerce_this(this, "search")?;
    rt.type_error("search needs a RegExp, and js2 has none yet")
}
