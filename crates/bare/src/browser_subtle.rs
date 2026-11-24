//! `SubtleCrypto` key methods (W3C Web Cryptography API) for AES-GCM, AES-CBC and HKDF: `generateKey`, `importKey`, `exportKey`,
//! `encrypt`, `decrypt`, `deriveBits` and `deriveKey`. `digest` is in `browser_web.rs`. The cipher code is `bare_tls::aes` and
//! `bare_tls::hkdf`. Learning code, not audited.
//!
//! A `CryptoKey` is an object of `CryptoKey.prototype` with an entry in `WebHost::keys`: the algorithm, the key bytes, `extractable`
//! and the usages. The prototype has the getters `type` ("secret"), `extractable`, `algorithm` and `usages`. `algorithm` and
//! `usages` give a new object on each read (the spec caches one). There is no `CryptoKey` global and no structured clone of a key.
//!
//! Limits: key formats "raw" only ("jwk", "spki" and "pkcs8" reject with `NotSupportedError`). AES-GCM takes a 12-byte IV only
//! (`NotSupportedError` for other lengths). HKDF takes SHA-256 only. RSA, ECDSA, ECDH, HMAC, `sign`, `verify`, `wrapKey` and
//! `unwrapKey` reject with `NotSupportedError`.

use crate::browser::*;
use crate::browser_scoped::*;
use crate::browser_web::{need, page_random, settle, subtle_this, usv, web_made_key_proto};
use crate::eng::Eng;
use bare_js_api::{Args, Attrs, Local, Type};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Alg {
    AesGcm,
    AesCbc,
    Hkdf,
}

impl Alg {
    fn name(self) -> &'static str {
        match self {
            Alg::AesGcm => "AES-GCM",
            Alg::AesCbc => "AES-CBC",
            Alg::Hkdf => "HKDF",
        }
    }
}

/// The data of one `CryptoKey`. The bytes are zeroed on drop.
#[derive(Clone)]
pub(crate) struct Key {
    alg: Alg,
    bytes: Vec<u8>,
    extractable: bool,
    /// Bits in the order of `USAGES`.
    usages: u8,
}

impl Drop for Key {
    fn drop(&mut self) {
        for b in self.bytes.iter_mut() {
            // SAFETY: a valid, aligned, exclusive reference to a byte of the vector.
            unsafe { std::ptr::write_volatile(b, 0) };
        }
    }
}

/// The `KeyUsage` values, in the order that `usages` lists them.
const USAGES: [&str; 8] = ["encrypt", "decrypt", "sign", "verify", "deriveKey", "deriveBits", "wrapKey", "unwrapKey"];
const ENCRYPT: u8 = 1;
const DECRYPT: u8 = 2;
const DERIVE_KEY: u8 = 16;
const DERIVE_BITS: u8 = 32;
const AES_USES: u8 = ENCRYPT | DECRYPT | 64 | 128;
const HKDF_USES: u8 = DERIVE_KEY | DERIVE_BITS;

/// The most keys of a page. A key holds at most 32 bytes.
const MAX_KEYS: usize = 100_000;

fn err<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, name: &str, msg: &str) -> bare_js_api::Error<'s> {
    d.dom_err_s(s, name, msg)
}

fn undef<E: Eng>(s: &mut Sc<'_, E>, v: Local<'_>) -> bool {
    s.type_of(v) == Type::Undefined
}

/// "Normalize an algorithm": a name or an object with `name`. Gives the matched algorithm and the object (if any) for its parameters.
fn alg<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, v: Local<'s>, allowed: &[Alg]) -> Res<'s, (Alg, Option<Local<'s>>)> {
    let (name, obj) = if is_object::<E>(s, v) {
        let n = s.get_str(v, "name")?;
        if undef::<E>(s, n) {
            return Err(s.type_error("Algorithm: Missing 'name'."));
        }
        (usv(d, s, n)?, Some(v))
    } else {
        (usv(d, s, v)?, None)
    };
    match allowed.iter().find(|a| a.name().eq_ignore_ascii_case(&name)) {
        Some(&a) => Ok((a, obj)),
        None => Err(err(d, s, "NotSupportedError", "Unrecognized algorithm name.")),
    }
}

/// A required member of the parameters of an algorithm.
fn member<'s, E: Eng>(s: &mut Sc<'s, E>, obj: Option<Local<'s>>, name: &str) -> Res<'s, Local<'s>> {
    let v = match obj {
        Some(o) => s.get_str(o, name)?,
        None => s.undefined(),
    };
    if undef::<E>(s, v) {
        return Err(s.type_error(&format!("Failed to read the '{name}' property: Required member is undefined.")));
    }
    Ok(v)
}

/// A copy of the bytes of a `BufferSource`.
fn buf<'s, E: Eng>(s: &mut Sc<'s, E>, v: Local<'s>, what: &str) -> Res<'s, Vec<u8>> {
    let Some(b) = s.bytes(v).map(<[u8]>::to_vec) else { return Err(s.type_error(&format!("{what} is not of type '(ArrayBuffer or ArrayBufferView)'."))) };
    s.charge((b.len() / 64) as u64)?;
    Ok(b)
}

/// `length` of an AES key algorithm: 128, 192 or 256 bits, as bytes.
fn aes_len<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, obj: Option<Local<'s>>) -> Res<'s, usize> {
    let v = member::<E>(s, obj, "length")?;
    match s.to_number(v)? {
        n if n == 128.0 || n == 192.0 || n == 256.0 => Ok(n as usize / 8),
        _ => Err(err(d, s, "OperationError", "AES key length must be 128, 192 or 256 bits.")),
    }
}

/// A `sequence<KeyUsage>` as bits. A value that is not a `KeyUsage` is a TypeError.
fn usages<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, v: Local<'s>) -> Res<'s, u8> {
    let bad = "The provided value cannot be converted to a sequence.";
    let Some(mut iter) = (if is_object::<E>(s, v) { s.iter_open(v)? } else { None }) else { return Err(s.type_error(bad)) };
    let mut bits = 0u8;
    loop {
        let step = match s.iter_next(&mut iter) {
            Ok(Some(x)) => s.charge(1).and_then(|_| usv(d, s, x)).and_then(|u| match USAGES.iter().position(|&n| n == u) {
                Some(i) => Ok(Some(1u8 << i)),
                None => Err(s.type_error(&format!("The provided value '{u}' is not a valid enum value of type KeyUsage."))),
            }),
            Ok(None) => Ok(None),
            Err(e) => Err(e),
        };
        match step {
            Ok(Some(b)) => bits |= b,
            Ok(None) => return Ok(bits),
            Err(e) => return Err(s.iter_fail(&mut iter, e)),
        }
    }
}

/// The usages of a new secret key: within `allowed` and not empty, else a SyntaxError.
fn check_uses<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, uses: u8, allowed: u8) -> Res<'s, ()> {
    if uses & !allowed != 0 {
        return Err(err(d, s, "SyntaxError", "Cannot create a key using the specified key usages."));
    }
    if uses == 0 {
        return Err(err(d, s, "SyntaxError", "Usages cannot be empty when creating a key."));
    }
    Ok(())
}

/// The data of the key `o`, for a structured clone (WebCrypto "CryptoKey" serialization steps: all internal slots).
pub(crate) fn key_data<E: Eng>(d: &Dom<E>, o: E::Obj) -> Option<Key> {
    d.web.keys.borrow().get(&o).cloned()
}

pub(crate) fn new_key<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, k: Key) -> Res<'s, Local<'s>> {
    if d.web.keys.borrow().len() >= MAX_KEYS {
        return Err(d.lim_s(s, "crypto-keys"));
    }
    let proto = web_made_key_proto(d, s)?;
    let o = d.obj_s(s, proto)?;
    d.web.keys.borrow_mut().insert(o, k);
    d.own_obj(s.engine(), o, Owner::Web(o));
    Ok(d.obj_local(s, o))
}

/// The key `v` with `usage`, for the algorithm `alg`. Else an InvalidAccessError (a TypeError when `v` is not a key).
fn use_key<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, v: Local<'s>, alg: Alg, usage: u8) -> Res<'s, Vec<u8>> {
    let got = obj_id::<E>(s, v).and_then(|o| d.web.keys.borrow().get(&o).map(|k| (k.alg, k.usages, k.bytes.clone())));
    let Some((a, uses, bytes)) = got else { return Err(s.type_error("The provided value is not of type 'CryptoKey'.")) };
    if a != alg || uses & usage == 0 {
        return Err(err(d, s, "InvalidAccessError", "The requested operation is not valid for the provided key."));
    }
    Ok(bytes)
}

fn key_this<'s, E: Eng, T>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, f: impl FnOnce(&Key) -> T) -> Res<'s, T> {
    let o = obj_id::<E>(s, this);
    let w = d.web.keys.borrow();
    match o.and_then(|o| w.get(&o)) {
        Some(k) => Ok(f(k)),
        None => {
            drop(w);
            Err(s.type_error("Illegal invocation"))
        }
    }
}

// ------------------------------------------------------------------------------------------ CryptoKey getters

pub(crate) fn key_type<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    key_this(d, s, this, |_| ())?;
    d.js_str_s(s, "secret")
}

pub(crate) fn key_extractable<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let x = key_this(d, s, this, |k| k.extractable)?;
    Ok(s.boolean(x))
}

pub(crate) fn key_algorithm<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let (alg, len) = key_this(d, s, this, |k| (k.alg, k.bytes.len()))?;
    let o = s.new_object()?;
    let n = d.js_str_s(s, alg.name())?;
    s.define_str(o, "name", n, Attrs::ALL)?;
    if alg != Alg::Hkdf {
        let l = s.number((len * 8) as f64);
        s.define_str(o, "length", l, Attrs::ALL)?;
    }
    Ok(o)
}

pub(crate) fn key_usages<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, _: Args<'s>) -> Res<'s, Local<'s>> {
    let bits = key_this(d, s, this, |k| k.usages)?;
    let mut items = Vec::new();
    for (i, n) in USAGES.iter().enumerate() {
        if bits & (1 << i) != 0 {
            items.push(d.js_str_s(s, n)?);
        }
    }
    s.new_array(&items)
}

// ------------------------------------------------------------------------------------------ SubtleCrypto methods

/// Run `f` and give a Promise of its result: an exception rejects it.
fn promise<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>, n: usize, what: &str, f: impl FnOnce(&mut Sc<'s, E>, [Local<'s>; 5]) -> Res<'s, Local<'s>>) -> Res<'s, Local<'s>> {
    let p = s.new_promise()?;
    let r = (|| -> Res<'s, Local<'s>> {
        subtle_this(d, s, this)?;
        need::<E>(s, a, n, &format!("SubtleCrypto.{what}"))?;
        let v: [Local<'s>; 5] = std::array::from_fn(|i| arg_s::<E>(s, a, i));
        f(s, v)
    })();
    settle::<E>(s, p, r)
}

pub(crate) fn generate_key<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    promise(d, s, this, a, 3, "generateKey", |s, v| {
        let (alg, obj) = alg(d, s, v[0], &[Alg::AesGcm, Alg::AesCbc])?;
        let extractable = s.to_boolean(v[1]);
        let uses = usages(d, s, v[2])?;
        let len = aes_len(d, s, obj)?;
        check_uses(d, s, uses, AES_USES)?;
        let mut bytes = vec![0u8; len];
        if page_random(&mut bytes).is_err() {
            return Err(err(d, s, "OperationError", "The operating system random source failed."));
        }
        new_key(d, s, Key { alg, bytes, extractable, usages: uses })
    })
}

/// The key format: "raw" works; the other formats of the spec are not built.
fn format<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, v: Local<'s>) -> Res<'s, ()> {
    match usv(d, s, v)?.as_str() {
        "raw" => Ok(()),
        "jwk" | "spki" | "pkcs8" => Err(err(d, s, "NotSupportedError", "Only the 'raw' key format is supported.")),
        f => Err(s.type_error(&format!("The provided value '{f}' is not a valid enum value of type KeyFormat."))),
    }
}

pub(crate) fn import_key<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    promise(d, s, this, a, 5, "importKey", |s, v| {
        format(d, s, v[0])?;
        let bytes = buf::<E>(s, v[1], "keyData")?;
        let (alg, _) = alg(d, s, v[2], &[Alg::AesGcm, Alg::AesCbc, Alg::Hkdf])?;
        let extractable = s.to_boolean(v[3]);
        let uses = usages(d, s, v[4])?;
        if alg == Alg::Hkdf {
            if extractable {
                return Err(err(d, s, "SyntaxError", "HKDF keys cannot be extractable."));
            }
            check_uses(d, s, uses, HKDF_USES)?;
        } else {
            if !matches!(bytes.len(), 16 | 24 | 32) {
                return Err(err(d, s, "DataError", "AES key data must be 128, 192 or 256 bits."));
            }
            check_uses(d, s, uses, AES_USES)?;
        }
        new_key(d, s, Key { alg, bytes, extractable, usages: uses })
    })
}

pub(crate) fn export_key<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    promise(d, s, this, a, 2, "exportKey", |s, v| {
        format(d, s, v[0])?;
        let k = v[1];
        let got = obj_id::<E>(s, k).and_then(|o| d.web.keys.borrow().get(&o).map(|k| (k.alg, k.extractable, k.bytes.clone())));
        let Some((alg, extractable, bytes)) = got else { return Err(s.type_error("The provided value is not of type 'CryptoKey'.")) };
        if alg == Alg::Hkdf {
            return Err(err(d, s, "NotSupportedError", "HKDF keys cannot be exported."));
        }
        if !extractable {
            return Err(err(d, s, "InvalidAccessError", "The key is not extractable."));
        }
        s.new_array_buffer(bytes)
    })
}

/// `encrypt` and `decrypt` with AES-GCM or AES-CBC.
fn cipher<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>, enc: bool) -> Res<'s, Local<'s>> {
    promise(d, s, this, a, 3, if enc { "encrypt" } else { "decrypt" }, |s, v| {
        let (alg, obj) = alg(d, s, v[0], &[Alg::AesGcm, Alg::AesCbc])?;
        let iv = member::<E>(s, obj, "iv")?;
        let iv = buf::<E>(s, iv, "iv")?;
        let (aad, tag) = if alg == Alg::AesGcm {
            let o = obj.expect("member checked it");
            let aad = s.get_str(o, "additionalData")?;
            let aad = if undef::<E>(s, aad) { Vec::new() } else { buf::<E>(s, aad, "additionalData")? };
            let t = s.get_str(o, "tagLength")?;
            let t = if undef::<E>(s, t) { 128.0 } else { s.to_number(t)? };
            if ![32.0, 64.0, 96.0, 104.0, 112.0, 120.0, 128.0].contains(&t) {
                return Err(err(d, s, "OperationError", "The tag length is not supported."));
            }
            (aad, t as usize / 8)
        } else {
            (Vec::new(), 0)
        };
        let key = use_key(d, s, v[1], alg, if enc { ENCRYPT } else { DECRYPT })?;
        let data = buf::<E>(s, v[2], "data")?;
        let out = match alg {
            Alg::AesGcm if iv.len() != 12 => return Err(err(d, s, "NotSupportedError", "AES-GCM takes a 12-byte IV only.")),
            Alg::AesGcm if enc => bare_tls::aes::gcm_seal(&key, &iv, &aad, &data, tag),
            Alg::AesGcm => bare_tls::aes::gcm_open(&key, &iv, &aad, &data, tag),
            _ if iv.len() != 16 => return Err(err(d, s, "OperationError", "The AES-CBC IV must be 16 bytes.")),
            _ if enc => bare_tls::aes::cbc_encrypt(&key, &iv, &data),
            _ => bare_tls::aes::cbc_decrypt(&key, &iv, &data),
        };
        match out {
            Ok(b) => s.new_array_buffer(b),
            Err(_) => Err(err(d, s, "OperationError", "The operation failed for an operation-specific reason.")),
        }
    })
}

pub(crate) fn encrypt<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    cipher(d, s, this, a, true)
}

pub(crate) fn decrypt<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    cipher(d, s, this, a, false)
}

/// HKDF-SHA256 of the key `key` with the parameters `obj`: `bits` bits.
fn hkdf<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, obj: Option<Local<'s>>, key: &[u8], bits: f64) -> Res<'s, Vec<u8>> {
    let hash = member::<E>(s, obj, "hash")?;
    let hash = if is_object::<E>(s, hash) { member::<E>(s, Some(hash), "name")? } else { hash };
    let hash = usv(d, s, hash)?;
    let salt = member::<E>(s, obj, "salt")?;
    let salt = buf::<E>(s, salt, "salt")?;
    let info = member::<E>(s, obj, "info")?;
    let info = buf::<E>(s, info, "info")?;
    if !hash.eq_ignore_ascii_case("SHA-256") {
        return Err(err(d, s, "NotSupportedError", "HKDF takes SHA-256 only."));
    }
    if bits == 0.0 || bits % 8.0 != 0.0 || bits > (255 * 32 * 8) as f64 {
        return Err(err(d, s, "OperationError", "The HKDF length is not valid."));
    }
    let prk = bare_tls::hkdf::extract(&salt, key);
    let mut out = vec![0u8; bits as usize / 8];
    if bare_tls::hkdf::expand(&prk, &info, &mut out).is_err() {
        return Err(err(d, s, "OperationError", "The HKDF length is not valid."));
    }
    Ok(out)
}

pub(crate) fn derive_bits<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    promise(d, s, this, a, 2, "deriveBits", |s, v| {
        let (_, obj) = alg(d, s, v[0], &[Alg::Hkdf])?;
        let len = v[2];
        let bits = if s.type_of(len) == Type::Null || undef::<E>(s, len) { 0.0 } else { s.to_number(len)? };
        let key = use_key(d, s, v[1], Alg::Hkdf, DERIVE_BITS)?;
        let out = hkdf(d, s, obj, &key, bits)?;
        s.new_array_buffer(out)
    })
}

pub(crate) fn derive_key<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    promise(d, s, this, a, 5, "deriveKey", |s, v| {
        let (_, obj) = alg(d, s, v[0], &[Alg::Hkdf])?;
        let (to, to_obj) = alg(d, s, v[2], &[Alg::AesGcm, Alg::AesCbc])?;
        let extractable = s.to_boolean(v[3]);
        let uses = usages(d, s, v[4])?;
        let len = aes_len(d, s, to_obj)?;
        let key = use_key(d, s, v[1], Alg::Hkdf, DERIVE_KEY)?;
        let bytes = hkdf(d, s, obj, &key, (len * 8) as f64)?;
        check_uses(d, s, uses, AES_USES)?;
        new_key(d, s, Key { alg: to, bytes, extractable, usages: uses })
    })
}
