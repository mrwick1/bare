//! `SubtleCrypto` key methods (W3C Web Cryptography API) for AES-GCM, AES-CBC, AES-CTR, HMAC and HKDF: `generateKey`, `importKey`,
//! `exportKey`, `encrypt`, `decrypt`, `sign`, `verify`, `deriveBits` and `deriveKey`. `digest` is in `browser_web.rs`. The cipher code is `bare_tls::aes` and
//! `bare_tls::hkdf`. Learning code, not audited.
//!
//! A `CryptoKey` is an object of `CryptoKey.prototype` with an entry in `WebHost::keys`: the algorithm, the key bytes, `extractable`
//! and the usages. The prototype has the getters `type` ("secret"), `extractable`, `algorithm` and `usages`. `algorithm` and
//! `usages` give a new object on each read (the spec caches one). There is no `CryptoKey` global and no structured clone of a key.
//!
//! Limits: key formats "raw" only ("jwk", "spki" and "pkcs8" reject with `NotSupportedError`). AES-GCM takes a 12-byte IV only
//! (`NotSupportedError` for other lengths). HKDF takes SHA-256 only; `deriveKey` gives an AES-GCM, AES-CBC or HMAC key. HMAC takes SHA-1, SHA-256, SHA-384 and SHA-512. RSA, ECDSA, ECDH, `wrapKey` and `unwrapKey` reject
//! with `NotSupportedError`. AES-CTR counts in the low `length` bits of the 16-byte counter block, as the spec says, and rejects
//! data that would reuse a counter value. `verify` of HMAC compares in constant time.

use crate::browser::*;
use crate::browser_scoped::*;
use crate::browser_web::{need, page_random, settle, subtle_this, usv, web_made_key_proto};
use crate::eng::Eng;
use bare_js_api::{Args, Attrs, Local, Type};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Alg {
    AesGcm,
    AesCbc,
    AesCtr,
    /// HMAC with the hash of the key.
    Hmac(Hash),
    Hkdf,
}

/// A hash of HMAC.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Hash {
    Sha1,
    Sha256,
    Sha384,
    Sha512,
}

impl Hash {
    fn name(self) -> &'static str {
        match self {
            Hash::Sha1 => "SHA-1",
            Hash::Sha256 => "SHA-256",
            Hash::Sha384 => "SHA-384",
            Hash::Sha512 => "SHA-512",
        }
    }

    /// The block size in bytes: the HMAC key length, and the length of a generated key with no `length`.
    fn block(self) -> usize {
        match self {
            Hash::Sha1 | Hash::Sha256 => 64,
            Hash::Sha384 | Hash::Sha512 => 128,
        }
    }

    fn of(self, data: &[u8]) -> Vec<u8> {
        match self {
            Hash::Sha1 => bare_tls::sha1::sha1(data).to_vec(),
            Hash::Sha256 => bare_tls::sha256::sha256(data).to_vec(),
            Hash::Sha384 => bare_tls::sha384::sha384(data).to_vec(),
            Hash::Sha512 => bare_tls::sha512::sha512(data).to_vec(),
        }
    }

    /// HMAC (RFC 2104) of `data` with `key`.
    fn hmac(self, key: &[u8], data: &[u8]) -> Vec<u8> {
        if self == Hash::Sha256 {
            return bare_tls::hmac::hmac_sha256(key, data).to_vec();
        }
        let mut k = if key.len() > self.block() { self.of(key) } else { key.to_vec() };
        k.resize(self.block(), 0);
        let mut inner: Vec<u8> = k.iter().map(|b| b ^ 0x36).collect();
        inner.extend_from_slice(data);
        let mut outer: Vec<u8> = k.iter().map(|b| b ^ 0x5c).collect();
        outer.extend(self.of(&inner));
        self.of(&outer)
    }
}

impl Alg {
    fn name(self) -> &'static str {
        match self {
            Alg::AesGcm => "AES-GCM",
            Alg::AesCbc => "AES-CBC",
            Alg::AesCtr => "AES-CTR",
            Alg::Hmac(_) => "HMAC",
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

/// The algorithms in their disk order (`profile_record.rs`). Append only: a profile on disk names an algorithm by its index.
const DISK: [Alg; 8] = [Alg::AesGcm, Alg::AesCbc, Alg::AesCtr, Alg::Hmac(Hash::Sha256), Alg::Hkdf, Alg::Hmac(Hash::Sha1), Alg::Hmac(Hash::Sha384), Alg::Hmac(Hash::Sha512)];

impl Key {
    /// The disk form: algorithm index, key bytes, extractable, usage bits.
    pub(crate) fn parts(&self) -> (u8, &[u8], bool, u8) {
        (DISK.iter().position(|&a| a == self.alg).expect("listed") as u8, &self.bytes, self.extractable, self.usages)
    }

    /// The key of `parts`. `None` for an unknown algorithm index.
    pub(crate) fn from_parts(alg: u8, bytes: Vec<u8>, extractable: bool, usages: u8) -> Option<Key> {
        Some(Key { alg: *DISK.get(alg as usize)?, bytes, extractable, usages })
    }
}

/// The `KeyUsage` values, in the order that `usages` lists them.
const USAGES: [&str; 8] = ["encrypt", "decrypt", "sign", "verify", "deriveKey", "deriveBits", "wrapKey", "unwrapKey"];
const ENCRYPT: u8 = 1;
const DECRYPT: u8 = 2;
const SIGN: u8 = 4;
const VERIFY: u8 = 8;
const DERIVE_KEY: u8 = 16;
const DERIVE_BITS: u8 = 32;
const AES_USES: u8 = ENCRYPT | DECRYPT | 64 | 128;
const HKDF_USES: u8 = DERIVE_KEY | DERIVE_BITS;
const HMAC_USES: u8 = SIGN | VERIFY;
/// HMAC in an `allowed` list of `alg`: the hash comes from the parameters (`hmac_hash`).
const HMAC: Alg = Alg::Hmac(Hash::Sha256);

/// The most keys of a page. An AES key holds at most 32 bytes; an HMAC key is as long as the page gives.
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

/// The `hash` member of HMAC parameters. An unknown hash is `NotSupportedError`.
fn hmac_hash<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, obj: Option<Local<'s>>) -> Res<'s, Hash> {
    let hash = member::<E>(s, obj, "hash")?;
    let hash = if is_object::<E>(s, hash) { member::<E>(s, Some(hash), "name")? } else { hash };
    let name = usv(d, s, hash)?;
    match [Hash::Sha1, Hash::Sha256, Hash::Sha384, Hash::Sha512].into_iter().find(|h| h.name().eq_ignore_ascii_case(&name)) {
        Some(h) => Ok(h),
        None => Err(err(d, s, "NotSupportedError", "Unrecognized algorithm name.")),
    }
}

/// The optional `length` of HMAC parameters, in bits. A length that is not a whole number of bytes is not built.
fn hmac_len<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, obj: Option<Local<'s>>) -> Res<'s, Option<usize>> {
    let Some(o) = obj else { return Ok(None) };
    let v = s.get_str(o, "length")?;
    if undef::<E>(s, v) {
        return Ok(None);
    }
    match s.to_number(v)? {
        n if n > 0.0 && n % 8.0 == 0.0 && n <= (1u32 << 16) as f64 => Ok(Some(n as usize / 8)),
        n if n == 0.0 => Err(err(d, s, "DataError", "The HMAC key length must not be 0.")),
        _ => Err(err(d, s, "NotSupportedError", "The HMAC key length must be a whole number of bytes.")),
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
    let same = a == alg || matches!((a, alg), (Alg::Hmac(_), Alg::Hmac(_)));
    if !same || uses & usage == 0 {
        return Err(err(d, s, "InvalidAccessError", "The requested operation is not valid for the provided key."));
    }
    Ok(bytes)
}

/// The hash and the bytes of the HMAC key `v` with `usage` (`use_key`).
fn hmac_key<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, v: Local<'s>, usage: u8) -> Res<'s, (Hash, Vec<u8>)> {
    let bytes = use_key(d, s, v, HMAC, usage)?;
    let hash = obj_id::<E>(s, v).and_then(|o| match d.web.keys.borrow().get(&o)?.alg { Alg::Hmac(h) => Some(h), _ => None });
    Ok((hash.unwrap_or(Hash::Sha256), bytes))
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
    if let Alg::Hmac(hash) = alg {
        let h = s.new_object()?;
        let hn = d.js_str_s(s, hash.name())?;
        s.define_str(h, "name", hn, Attrs::ALL)?;
        s.define_str(o, "hash", h, Attrs::ALL)?;
    }
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
        let (mut alg, obj) = alg(d, s, v[0], &[Alg::AesGcm, Alg::AesCbc, Alg::AesCtr, HMAC])?;
        let extractable = s.to_boolean(v[1]);
        let uses = usages(d, s, v[2])?;
        let len = if alg == HMAC {
            let hash = hmac_hash(d, s, obj)?;
            alg = Alg::Hmac(hash);
            let len = hmac_len(d, s, obj)?.unwrap_or(hash.block());
            check_uses(d, s, uses, HMAC_USES)?;
            len
        } else {
            let len = aes_len(d, s, obj)?;
            check_uses(d, s, uses, AES_USES)?;
            len
        };
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
        let (mut alg, obj) = alg(d, s, v[2], &[Alg::AesGcm, Alg::AesCbc, Alg::AesCtr, HMAC, Alg::Hkdf])?;
        let extractable = s.to_boolean(v[3]);
        let uses = usages(d, s, v[4])?;
        if alg == HMAC {
            alg = Alg::Hmac(hmac_hash(d, s, obj)?);
            check_uses(d, s, uses, HMAC_USES)?;
            if bytes.is_empty() {
                return Err(err(d, s, "DataError", "The HMAC key data must not be empty."));
            }
            if hmac_len(d, s, obj)?.is_some_and(|l| l != bytes.len()) {
                return Err(err(d, s, "DataError", "The HMAC key length does not match the key data."));
            }
        } else if alg == Alg::Hkdf {
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
        let (alg, obj) = alg(d, s, v[0], &[Alg::AesGcm, Alg::AesCbc, Alg::AesCtr])?;
        if alg == Alg::AesCtr {
            let counter = member::<E>(s, obj, "counter")?;
            let counter = buf::<E>(s, counter, "counter")?;
            let len = member::<E>(s, obj, "length")?;
            let len = s.to_number(len)?;
            let key = use_key(d, s, v[1], alg, if enc { ENCRYPT } else { DECRYPT })?;
            let data = buf::<E>(s, v[2], "data")?;
            let Ok(counter) = <[u8; 16]>::try_from(counter.as_slice()) else { return Err(err(d, s, "OperationError", "The AES-CTR counter must be 16 bytes.")) };
            if !(1.0..=128.0).contains(&len) || len.fract() != 0.0 {
                return Err(err(d, s, "OperationError", "The AES-CTR length must be between 1 and 128."));
            }
            return match ctr(&key, counter, len as u32, data) {
                Some(b) => s.new_array_buffer(b),
                None => Err(err(d, s, "OperationError", "The data is too long for the AES-CTR counter length.")),
            };
        }
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

/// AES-CTR: XOR `data` with the AES of the counter block. The low `bits` bits of the block count up and wrap; the rest stays. `None`
/// when the data needs more blocks than the counter has values (a counter value would repeat).
fn ctr(key: &[u8], counter: [u8; 16], bits: u32, mut data: Vec<u8>) -> Option<Vec<u8>> {
    let aes = bare_tls::aes::Aes::new(key)?;
    let blocks = data.len().div_ceil(16) as u128;
    if bits < 128 && blocks > 1u128 << bits {
        return None;
    }
    let mask = if bits == 128 { u128::MAX } else { (1u128 << bits) - 1 };
    let mut c = u128::from_be_bytes(counter);
    for chunk in data.chunks_mut(16) {
        let mut b = c.to_be_bytes();
        aes.encrypt_block(&mut b);
        chunk.iter_mut().zip(b).for_each(|(x, k)| *x ^= k);
        c = (c & !mask) | (c.wrapping_add(1) & mask);
    }
    Some(data)
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
        let (mut to, to_obj) = alg(d, s, v[2], &[Alg::AesGcm, Alg::AesCbc, HMAC])?;
        let extractable = s.to_boolean(v[3]);
        let uses = usages(d, s, v[4])?;
        // An HMAC key without `length` is one block of its hash long ("get key length" of HMAC).
        let (len, allowed) = if to == HMAC {
            let hash = hmac_hash(d, s, to_obj)?;
            to = Alg::Hmac(hash);
            (hmac_len(d, s, to_obj)?.unwrap_or(hash.block()), HMAC_USES)
        } else {
            (aes_len(d, s, to_obj)?, AES_USES)
        };
        let key = use_key(d, s, v[1], Alg::Hkdf, DERIVE_KEY)?;
        let bytes = hkdf(d, s, obj, &key, (len * 8) as f64)?;
        check_uses(d, s, uses, allowed)?;
        new_key(d, s, Key { alg: to, bytes, extractable, usages: uses })
    })
}

/// `sign` with HMAC.
pub(crate) fn sign<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    promise(d, s, this, a, 3, "sign", |s, v| {
        alg(d, s, v[0], &[HMAC])?;
        let (hash, key) = hmac_key(d, s, v[1], SIGN)?;
        let data = buf::<E>(s, v[2], "data")?;
        s.new_array_buffer(hash.hmac(&key, &data))
    })
}

/// `verify` with HMAC: the MAC of the data against the signature, compared in constant time.
pub(crate) fn verify<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, this: Local<'s>, a: Args<'s>) -> Res<'s, Local<'s>> {
    promise(d, s, this, a, 4, "verify", |s, v| {
        alg(d, s, v[0], &[HMAC])?;
        let (hash, key) = hmac_key(d, s, v[1], VERIFY)?;
        let sig = buf::<E>(s, v[2], "signature")?;
        let data = buf::<E>(s, v[3], "data")?;
        let mac = hash.hmac(&key, &data);
        let same = sig.len() == mac.len() && sig.iter().zip(mac).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0;
        Ok(s.boolean(same))
    })
}
