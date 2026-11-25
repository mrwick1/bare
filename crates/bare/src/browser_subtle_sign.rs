//! `SubtleCrypto` signatures (`sign`, `verify`) and their keys: HMAC, RSASSA-PKCS1-v1_5 and ECDSA (P-256, P-384), with `generateKey`,
//! `importKey` and `exportKey` in the formats "raw" (HMAC, public EC), "jwk", "spki" and "pkcs8". The big-number and curve code is
//! `bare_tls::rsa_key` and `bare_tls::ecc`; the hashes are `bare_tls::sha*`. `browser_subtle.rs` has the key type and calls this file.
//!
//! A key keeps its bytes in the form that the formats share: HMAC the secret; RSA public the PKCS#1 `RSAPublicKey` DER; RSA private
//! the PKCS#1 `RSAPrivateKey` DER (two primes); EC public the point `04 || X || Y`; EC private the scalar `d`. Learning code, not audited.
//!
//! Limits: RSA moduli of 256 to 4096 bits, public exponent 3 or 65537 for `generateKey`, any odd exponent below 2^32 for import. A
//! private RSA key needs the CRT members (`p`, `q`, `dp`, `dq`, `qi`). HMAC keys are at most 64 KiB, with a length in whole bytes.
//! `RSA-PSS`, `RSA-OAEP`, `ECDH` and `Ed25519` are not built (`NotSupportedError`). JWK for AES and HKDF is not built.
//! Signing is not constant time (see `bare_tls::rsa_key` and `bare_tls::ecc`). The work meter (`charge`) is an estimate from
//! one run on one machine: one unit is about 64 ns.

use crate::browser::*;
use crate::browser_scoped::*;
use crate::browser_subtle::{buf, check_uses, err, member, undef, usages, Alg, Fmt, Key, Kind, SIGN, VERIFY, USAGES};
use crate::browser_web::{page_random, usv};
use crate::eng::Eng;
use bare_js_api::{Attrs, Local};
use bare_tls::{der, ecc, rsa_key};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Hash {
    Sha1,
    Sha256,
    Sha384,
    Sha512,
}

impl Hash {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Hash::Sha1 => "SHA-1",
            Hash::Sha256 => "SHA-256",
            Hash::Sha384 => "SHA-384",
            Hash::Sha512 => "SHA-512",
        }
    }

    /// The digits of the JWK `alg`: "RS256", "HS1".
    fn short(self) -> &'static str {
        &self.name()[4..]
    }

    fn block(self) -> usize {
        if matches!(self, Hash::Sha384 | Hash::Sha512) { 128 } else { 64 }
    }

    fn digest(self, data: &[u8]) -> Vec<u8> {
        match self {
            Hash::Sha1 => bare_tls::sha1::sha1(data).to_vec(),
            Hash::Sha256 => bare_tls::sha256::sha256(data).to_vec(),
            Hash::Sha384 => bare_tls::sha384::sha384(data).to_vec(),
            Hash::Sha512 => bare_tls::sha512::sha512(data).to_vec(),
        }
    }

    /// The DER `DigestInfo` up to the digest (RFC 8017 section 9.2, note 1).
    fn info(self) -> &'static [u8] {
        match self {
            Hash::Sha1 => &[0x30, 0x21, 0x30, 0x09, 6, 5, 0x2b, 0x0e, 3, 2, 0x1a, 5, 0, 4, 0x14],
            Hash::Sha256 => &[0x30, 0x31, 0x30, 0x0d, 6, 9, 0x60, 0x86, 0x48, 1, 0x65, 3, 4, 2, 1, 5, 0, 4, 0x20],
            Hash::Sha384 => &[0x30, 0x41, 0x30, 0x0d, 6, 9, 0x60, 0x86, 0x48, 1, 0x65, 3, 4, 2, 2, 5, 0, 4, 0x30],
            Hash::Sha512 => &[0x30, 0x51, 0x30, 0x0d, 6, 9, 0x60, 0x86, 0x48, 1, 0x65, 3, 4, 2, 3, 5, 0, 4, 0x40],
        }
    }
}

/// HMAC (RFC 2104) with any of the hashes.
fn hmac(h: Hash, key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut k = if key.len() > h.block() { h.digest(key) } else { key.to_vec() };
    k.resize(h.block(), 0);
    let mut inner: Vec<u8> = k.iter().map(|b| b ^ 0x36).collect();
    inner.extend_from_slice(data);
    let mut outer: Vec<u8> = k.iter().map(|b| b ^ 0x5c).collect();
    outer.extend(h.digest(&inner));
    h.digest(&outer)
}

/// The `hash` member of the parameters: a name or `{ name }`.
pub(crate) fn hash_param<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, obj: Option<Local<'s>>) -> Res<'s, Hash> {
    let h = member::<E>(s, obj, "hash")?;
    let h = if is_object::<E>(s, h) { member::<E>(s, Some(h), "name")? } else { h };
    let name = usv(d, s, h)?;
    match [Hash::Sha1, Hash::Sha256, Hash::Sha384, Hash::Sha512].into_iter().find(|h| h.name().eq_ignore_ascii_case(&name)) {
        Some(h) => Ok(h),
        None => Err(err(d, s, "NotSupportedError", "Unrecognized hash algorithm name.")),
    }
}

// ------------------------------------------------------------------------------------------ encodings

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

fn b64(b: &[u8]) -> String {
    let mut out = String::with_capacity(b.len().div_ceil(3) * 4);
    for c in b.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..=c.len() {
            out.push(B64[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

/// Base64url without padding. `None` for another character or a length of 1 mod 4.
fn unb64(t: &str) -> Option<Vec<u8>> {
    if t.len() % 4 == 1 {
        return None;
    }
    let (mut out, mut acc, mut bits) = (Vec::with_capacity(t.len() * 3 / 4), 0u32, 0);
    for c in t.bytes() {
        acc = acc << 6 | B64.iter().position(|&x| x == c)? as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

const OID_RSA: [u8; 9] = [0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01];
const OID_EC: [u8; 7] = [0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01];
const OID_P256: [u8; 8] = [0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07];
const OID_P384: [u8; 5] = [0x2b, 0x81, 0x04, 0x00, 0x22];

/// The curve of `param` (256 or 384): the curve, the JWK name and the OID.
fn curve_of(bits: u32) -> (ecc::Curve, &'static str, &'static [u8]) {
    if bits == 256 { (ecc::Curve::P256, "P-256", &OID_P256) } else { (ecc::Curve::P384, "P-384", &OID_P384) }
}

fn seq(parts: &[Vec<u8>]) -> Vec<u8> {
    der::tlv(der::SEQUENCE, &parts.concat())
}

fn alg_id(oid: &[u8], param: Vec<u8>) -> Vec<u8> {
    seq(&[der::tlv(der::OID, oid), param])
}

fn rsa_public_der(n: &[u8], e: &[u8]) -> Vec<u8> {
    seq(&[der::uint(n), der::uint(e)])
}

fn rsa_private_der(k: &rsa_key::PrivateKey) -> Vec<u8> {
    seq(&[der::uint(&[0]), der::uint(&k.n), der::uint(&k.e), der::uint(&k.d), der::uint(&k.p), der::uint(&k.q), der::uint(&k.dp), der::uint(&k.dq), der::uint(&k.qi)])
}

/// The INTEGERs of one SEQUENCE that fills `der`, as big-endian magnitudes.
fn integers(der_: &[u8]) -> Option<Vec<Vec<u8>>> {
    let mut r = der::Reader::new(der_);
    let body = r.read_tag(der::SEQUENCE).ok()?;
    r.finish().ok()?;
    let mut r = der::Reader::new(body.content);
    let mut v = Vec::new();
    while !r.is_empty() {
        v.push(rsa_key::strip(der::unsigned(r.read_tag(der::INTEGER).ok()?.content).ok()?).to_vec());
    }
    Some(v)
}

fn parse_rsa_public(der_: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    let [n, e] = <[_; 2]>::try_from(integers(der_)?).ok()?;
    Some((n, e))
}

fn parse_rsa_private(der_: &[u8]) -> Option<rsa_key::PrivateKey> {
    let [v, n, e, d, p, q, dp, dq, qi] = <[_; 9]>::try_from(integers(der_)?).ok()?;
    (v == [0] || v.is_empty()).then_some(rsa_key::PrivateKey { n, e, d, p, q, dp, dq, qi })
}

/// A `SubjectPublicKeyInfo` or a `PrivateKeyInfo` (`inner` is the BIT STRING or the OCTET STRING): the algorithm OID, the tag and content
/// of its parameter, and the key bytes.
fn parse_info(der_: &[u8], private: bool) -> Option<(Vec<u8>, u8, Vec<u8>, Vec<u8>)> {
    let mut r = der::Reader::new(der_);
    let body = r.read_tag(der::SEQUENCE).ok()?;
    r.finish().ok()?;
    let mut r = der::Reader::new(body.content);
    if private && der::unsigned(r.read_tag(der::INTEGER).ok()?.content).ok()? != [0] {
        return None;
    }
    let mut a = der::Reader::new(r.read_tag(der::SEQUENCE).ok()?.content);
    let oid = a.read_tag(der::OID).ok()?.content.to_vec();
    let p = a.read().ok()?;
    a.finish().ok()?;
    let key = if private {
        r.read_tag(der::OCTET_STRING).ok()?.content.to_vec()
    } else {
        match der::bit_string(r.read_tag(der::BIT_STRING).ok()?.content).ok()? {
            (0, k) => k.to_vec(),
            _ => return None,
        }
    };
    (r.is_empty() || private).then_some((oid, p.tag, p.content.to_vec(), key))
}

/// The scalar `d` of an `ECPrivateKey` (RFC 5915), left-padded to `size` bytes.
fn parse_ec_private(der_: &[u8], size: usize) -> Option<Vec<u8>> {
    let mut r = der::Reader::new(der_);
    let body = r.read_tag(der::SEQUENCE).ok()?;
    r.finish().ok()?;
    let mut r = der::Reader::new(body.content);
    (der::unsigned(r.read_tag(der::INTEGER).ok()?.content).ok()? == [1]).then_some(())?;
    let d = r.read_tag(der::OCTET_STRING).ok()?.content;
    (d.len() <= size).then(|| [vec![0; size - d.len()], d.to_vec()].concat())
}

fn ec_alg_id(oid: &[u8]) -> Vec<u8> {
    alg_id(&OID_EC, der::tlv(der::OID, oid))
}

// ------------------------------------------------------------------------------------------ keys

fn bits_of(n: &[u8]) -> u32 {
    let n = rsa_key::strip(n);
    n.first().map_or(0, |b| 8 * n.len() as u32 - b.leading_zeros())
}

/// A public RSA key that `bare_tls::rsa_key::public_op` accepts: an odd modulus of 256 to 4096 bits, an odd exponent of 3 to 2^32 - 1.
fn rsa_public_ok(n: &[u8], e: &[u8]) -> bool {
    let (n, e) = (rsa_key::strip(n), rsa_key::strip(e));
    (rsa_key::MIN_BITS as u32..=rsa_key::MAX_BITS as u32).contains(&bits_of(n)) && n.last().is_some_and(|b| b & 1 == 1) && e.len() <= 4 && e.last().is_some_and(|b| b & 1 == 1) && e != [1]
}

fn rsa_key_data(hash: Hash, bytes: Vec<u8>, bits: u32, extractable: bool, uses: u8, kind: Kind) -> Key {
    Key { alg: Alg::Rsa, bytes, extractable, usages: uses, kind, hash, param: bits }
}

fn ec_key_data(bytes: Vec<u8>, bits: u32, extractable: bool, uses: u8, kind: Kind) -> Key {
    Key { alg: Alg::Ecdsa, bytes, extractable, usages: uses, kind, hash: Hash::Sha256, param: bits }
}

/// Usages of a public key: within `allowed`, and may be empty.
fn check_public<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, uses: u8, allowed: u8) -> Res<'s, ()> {
    if uses & !allowed != 0 {
        return Err(err(d, s, "SyntaxError", "Cannot create a key using the specified key usages."));
    }
    Ok(())
}

fn data_err<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, msg: &str) -> bare_js_api::Error<'s> {
    err(d, s, "DataError", msg)
}

fn random_bytes<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, n: usize) -> Res<'s, Vec<u8>> {
    let mut b = vec![0u8; n];
    if page_random(&mut b).is_err() {
        return Err(err(d, s, "OperationError", "The operating system random source failed."));
    }
    Ok(b)
}

/// A new private scalar of a curve and its public point.
fn ec_pair<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, bits: u32) -> Res<'s, (Vec<u8>, Vec<u8>)> {
    let curve = curve_of(bits).0;
    loop {
        let sk = random_bytes(d, s, curve.size())?;
        if let Some(pk) = ecc::public_from_scalar(curve, &sk).filter(|_| ecc::valid_scalar(curve, &sk)) {
            return Ok((sk, pk));
        }
    }
}

fn key_pair<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, public: Key, private: Key) -> Res<'s, Local<'s>> {
    let o = s.new_object()?;
    for (name, k) in [("publicKey", public), ("privateKey", private)] {
        let v = crate::browser_subtle::new_key(d, s, k)?;
        s.define_str(o, name, v, Attrs::ALL)?;
    }
    Ok(o)
}

fn named_curve<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, obj: Option<Local<'s>>) -> Res<'s, u32> {
    let c = member::<E>(s, obj, "namedCurve")?;
    match usv(d, s, c)?.as_str() {
        "P-256" => Ok(256),
        "P-384" => Ok(384),
        _ => Err(err(d, s, "NotSupportedError", "Unsupported named curve.")),
    }
}

/// `generateKey` of HMAC, RSASSA-PKCS1-v1_5 and ECDSA. A pair is a plain object with `publicKey` and `privateKey`.
pub(crate) fn generate<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, alg: Alg, obj: Option<Local<'s>>, extractable: bool, uses: u8) -> Res<'s, Local<'s>> {
    let sign_uses = SIGN | VERIFY;
    match alg {
        Alg::Hmac => {
            let hash = hash_param(d, s, obj)?;
            let l = obj.map(|o| s.get_str(o, "length")).transpose()?;
            let bits = match l.filter(|&l| !undef::<E>(s, l)) {
                Some(l) => s.to_number(l)?,
                None => (hash.block() * 8) as f64,
            };
            check_uses(d, s, uses, sign_uses)?;
            if bits < 8.0 || bits > 65536.0 || bits % 8.0 != 0.0 {
                return Err(err(d, s, "OperationError", "The HMAC key length must be a whole number of bytes from 8 to 65536 bits."));
            }
            let bytes = random_bytes(d, s, bits as usize / 8)?;
            crate::browser_subtle::new_key(d, s, Key { alg, bytes, extractable, usages: uses, kind: Kind::Secret, hash, param: 0 })
        }
        Alg::Rsa => {
            let hash = hash_param(d, s, obj)?;
            let (len, e) = (member::<E>(s, obj, "modulusLength")?, member::<E>(s, obj, "publicExponent")?);
            let bits = s.to_number(len)?;
            let e = buf::<E>(s, e, "publicExponent")?;
            check_uses(d, s, uses, sign_uses)?;
            if uses & SIGN == 0 {
                return Err(err(d, s, "SyntaxError", "Usages of the private key cannot be empty."));
            }
            let e = match rsa_key::strip(&e) {
                [3] => 3,
                [1, 0, 1] => 65537,
                _ => return Err(err(d, s, "OperationError", "The public exponent must be 3 or 65537.")),
            };
            if bits.fract() != 0.0 || bits < rsa_key::MIN_BITS as f64 || bits > rsa_key::MAX_BITS as f64 || bits % 8.0 != 0.0 {
                return Err(err(d, s, "OperationError", "The modulus length must be a multiple of 8 from 256 to 4096 bits."));
            }
            let bits = bits as u32;
            s.charge((bits as u64).pow(3) / 5000)?;
            let Some(k) = rsa_key::PrivateKey::generate(bits as usize, e) else { return Err(err(d, s, "OperationError", "The key generation failed.")) };
            let public = rsa_key_data(hash, rsa_public_der(&k.n, &k.e), bits, true, uses & VERIFY, Kind::Public);
            let private = rsa_key_data(hash, rsa_private_der(&k), bits, extractable, uses & SIGN, Kind::Private);
            key_pair(d, s, public, private)
        }
        _ => {
            let bits = named_curve(d, s, obj)?;
            check_uses(d, s, uses, sign_uses)?;
            if uses & SIGN == 0 {
                return Err(err(d, s, "SyntaxError", "Usages of the private key cannot be empty."));
            }
            let (sk, pk) = ec_pair(d, s, bits)?;
            key_pair(d, s, ec_key_data(pk, bits, true, uses & VERIFY, Kind::Public), ec_key_data(sk, bits, extractable, uses & SIGN, Kind::Private))
        }
    }
}

// ------------------------------------------------------------------------------------------ JWK

/// A text member of a JWK, `None` when it is absent.
fn jwk_text<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, o: Local<'s>, name: &str) -> Res<'s, Option<String>> {
    let v = s.get_str(o, name)?;
    if undef::<E>(s, v) { Ok(None) } else { usv(d, s, v).map(Some) }
}

/// A base64url member of a JWK, `None` when it is absent.
fn jwk_bytes<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, o: Local<'s>, name: &str) -> Res<'s, Option<Vec<u8>>> {
    match jwk_text(d, s, o, name)? {
        None => Ok(None),
        Some(t) => unb64(&t).map(Some).ok_or_else(|| data_err(d, s, &format!("The '{name}' member of the JWK is not base64url."))),
    }
}

fn jwk_need<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, o: Local<'s>, name: &str) -> Res<'s, Vec<u8>> {
    match jwk_bytes(d, s, o, name)? {
        Some(b) => Ok(b),
        None => Err(data_err(d, s, &format!("The JWK has no '{name}' member."))),
    }
}

/// The checks of "import a JWK" that every key type shares: `kty`, `use`, `key_ops`, `ext` and `alg`.
fn jwk_check<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, o: Local<'s>, kty: &str, alg: Option<&str>, extractable: bool, uses: u8) -> Res<'s, ()> {
    if jwk_text(d, s, o, "kty")?.as_deref() != Some(kty) {
        return Err(data_err(d, s, "The JWK 'kty' member does not match the algorithm."));
    }
    if jwk_text(d, s, o, "use")?.is_some_and(|u| u != "sig") {
        return Err(data_err(d, s, "The JWK 'use' member must be 'sig'."));
    }
    let ops = s.get_str(o, "key_ops")?;
    if !undef::<E>(s, ops) && uses & !usages(d, s, ops)? != 0 {
        return Err(data_err(d, s, "The JWK 'key_ops' member does not allow the usages."));
    }
    let ext = s.get_str(o, "ext")?;
    if !undef::<E>(s, ext) && !s.to_boolean(ext) && extractable {
        return Err(data_err(d, s, "The JWK 'ext' member is false, but the key must be extractable."));
    }
    if let (Some(a), Some(want)) = (jwk_text(d, s, o, "alg")?, alg) {
        if a != want {
            return Err(data_err(d, s, "The JWK 'alg' member does not match the algorithm."));
        }
    }
    Ok(())
}

/// The key `data` (a JWK object, or the bytes of the other formats) of the algorithm `alg` with the parameters `obj`.
pub(crate) fn import<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, fmt: Fmt, data: Local<'s>, alg: Alg, obj: Option<Local<'s>>, extractable: bool, uses: u8) -> Res<'s, Key> {
    let bad = |d: &Dom<E>, s: &mut Sc<'s, E>| data_err(d, s, "The key data is not valid.");
    let jwk = if fmt == Fmt::Jwk {
        if !is_object::<E>(s, data) {
            return Err(s.type_error("The provided value is not of type 'JsonWebKey'."));
        }
        Some(data)
    } else {
        None
    };
    let bytes = if jwk.is_none() { buf::<E>(s, data, "keyData")? } else { Vec::new() };
    match alg {
        Alg::Hmac => {
            let hash = hash_param(d, s, obj)?;
            let bytes = match (fmt, jwk) {
                (Fmt::Raw, _) => bytes,
                (_, Some(o)) => {
                    jwk_check(d, s, o, "oct", Some(&format!("HS{}", hash.short())), extractable, uses)?;
                    jwk_need(d, s, o, "k")?
                }
                _ => return Err(err(d, s, "NotSupportedError", "HMAC keys import as 'raw' or 'jwk' only.")),
            };
            check_uses(d, s, uses, SIGN | VERIFY)?;
            let len = obj.map(|o| s.get_str(o, "length")).transpose()?.filter(|&l| !undef::<E>(s, l));
            if let Some(l) = len {
                let l = s.to_number(l)?;
                if l == 0.0 || l > (bytes.len() * 8) as f64 || l <= (bytes.len() * 8) as f64 - 8.0 {
                    return Err(data_err(d, s, "The HMAC key length does not match the key data."));
                }
            }
            if bytes.is_empty() || bytes.len() > 8192 {
                return Err(data_err(d, s, "The HMAC key data must be from 1 byte to 8192 bytes."));
            }
            Ok(Key { alg, bytes, extractable, usages: uses, kind: Kind::Secret, hash, param: 0 })
        }
        Alg::Rsa => {
            let hash = hash_param(d, s, obj)?;
            let (public, private) = match (fmt, jwk) {
                (Fmt::Jwk, Some(o)) => {
                    jwk_check(d, s, o, "RSA", Some(&format!("RS{}", hash.short())), extractable, uses)?;
                    let (n, e) = (jwk_need(d, s, o, "n")?, jwk_need(d, s, o, "e")?);
                    match jwk_bytes(d, s, o, "d")? {
                        None => (Some((n, e)), None),
                        Some(dd) => {
                            let k = rsa_key::PrivateKey { n, e, d: dd, p: jwk_need(d, s, o, "p")?, q: jwk_need(d, s, o, "q")?, dp: jwk_need(d, s, o, "dp")?, dq: jwk_need(d, s, o, "dq")?, qi: jwk_need(d, s, o, "qi")? };
                            (None, Some(k))
                        }
                    }
                }
                (Fmt::Spki, _) => match parse_info(&bytes, false) {
                    Some((oid, 5, p, key)) if oid == OID_RSA && p.is_empty() => (Some(parse_rsa_public(&key).ok_or_else(|| bad(d, s))?), None),
                    _ => return Err(bad(d, s)),
                },
                (Fmt::Pkcs8, _) => match parse_info(&bytes, true) {
                    Some((oid, 5, p, key)) if oid == OID_RSA && p.is_empty() => (None, Some(parse_rsa_private(&key).ok_or_else(|| bad(d, s))?)),
                    _ => return Err(bad(d, s)),
                },
                _ => return Err(err(d, s, "NotSupportedError", "RSA keys import as 'jwk', 'spki' or 'pkcs8' only.")),
            };
            if let Some(k) = private {
                check_uses(d, s, uses, SIGN)?;
                s.charge((bits_of(&k.n) as u64).pow(3) / 340_000)?;
                let k = rsa_key::PrivateKey { n: rsa_key::strip(&k.n).to_vec(), e: rsa_key::strip(&k.e).to_vec(), ..k };
                if !rsa_public_ok(&k.n, &k.e) || !k.check() {
                    return Err(bad(d, s));
                }
                Ok(rsa_key_data(hash, rsa_private_der(&k), bits_of(&k.n), extractable, uses, Kind::Private))
            } else {
                let (n, e) = public.expect("one of the two");
                check_public(d, s, uses, VERIFY)?;
                if !rsa_public_ok(&n, &e) {
                    return Err(bad(d, s));
                }
                Ok(rsa_key_data(hash, rsa_public_der(&n, &e), bits_of(&n), extractable, uses, Kind::Public))
            }
        }
        _ => {
            let bits = named_curve(d, s, obj)?;
            let (curve, crv, oid) = curve_of(bits);
            let size = curve.size();
            let (public, private) = match (fmt, jwk) {
                (Fmt::Jwk, Some(o)) => {
                    jwk_check(d, s, o, "EC", None, extractable, uses)?;
                    if jwk_text(d, s, o, "crv")?.as_deref() != Some(crv) {
                        return Err(data_err(d, s, "The JWK 'crv' member does not match the curve."));
                    }
                    let (x, y) = (jwk_need(d, s, o, "x")?, jwk_need(d, s, o, "y")?);
                    let sk = jwk_bytes(d, s, o, "d")?;
                    ([vec![4], x, y].concat(), sk)
                }
                (Fmt::Raw, _) => (bytes, None),
                (Fmt::Spki, _) => match parse_info(&bytes, false) {
                    Some((o, 6, p, key)) if o == OID_EC && p == oid => (key, None),
                    _ => return Err(bad(d, s)),
                },
                _ => match parse_info(&bytes, true) {
                    Some((o, 6, p, key)) if o == OID_EC && p == oid => {
                        let sk = parse_ec_private(&key, size).ok_or_else(|| bad(d, s))?;
                        (ecc::public_from_scalar(curve, &sk).ok_or_else(|| bad(d, s))?, Some(sk))
                    }
                    _ => return Err(bad(d, s)),
                },
            };
            if let Some(sk) = private {
                check_uses(d, s, uses, SIGN)?;
                if !ecc::valid_scalar(curve, &sk) || ecc::public_from_scalar(curve, &sk).is_none_or(|q| q != public) {
                    return Err(bad(d, s));
                }
                Ok(ec_key_data(sk, bits, extractable, uses, Kind::Private))
            } else {
                check_public(d, s, uses, VERIFY)?;
                if !ecc::valid_public(curve, &public) {
                    return Err(bad(d, s));
                }
                Ok(ec_key_data(public, bits, extractable, uses, Kind::Public))
            }
        }
    }
}

// ------------------------------------------------------------------------------------------ export

fn put<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, o: Local<'s>, name: &str, v: &str) -> Res<'s, ()> {
    let v = d.js_str_s(s, v)?;
    s.define_str(o, name, v, Attrs::ALL)
}

/// The public exponent of an RSA key.
pub(crate) fn rsa_exponent(k: &Key) -> Vec<u8> {
    match k.kind {
        Kind::Private => parse_rsa_private(&k.bytes).map(|p| p.e),
        _ => parse_rsa_public(&k.bytes).map(|p| p.1),
    }
    .unwrap_or_default()
}

/// The members of `key.algorithm` that come after `name`.
pub(crate) fn describe<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, o: Local<'s>, k: &Key) -> Res<'s, ()> {
    let hash = |d: &Dom<E>, s: &mut Sc<'s, E>| -> Res<'s, Local<'s>> {
        let h = s.new_object()?;
        put(d, s, h, "name", k.hash.name())?;
        Ok(h)
    };
    match k.alg {
        Alg::Hmac => {
            let h = hash(d, s)?;
            s.define_str(o, "hash", h, Attrs::ALL)?;
            let l = s.number((k.bytes.len() * 8) as f64);
            s.define_str(o, "length", l, Attrs::ALL)
        }
        Alg::Rsa => {
            let l = s.number(k.param as f64);
            s.define_str(o, "modulusLength", l, Attrs::ALL)?;
            let e = s.new_uint8_array(rsa_exponent(k))?;
            s.define_str(o, "publicExponent", e, Attrs::ALL)?;
            let h = hash(d, s)?;
            s.define_str(o, "hash", h, Attrs::ALL)
        }
        _ => put(d, s, o, "namedCurve", curve_of(k.param).1),
    }
}

/// `exportKey` of the keys of this file. The caller checked `extractable`.
pub(crate) fn export<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, fmt: Fmt, k: &Key) -> Res<'s, Local<'s>> {
    let no = |d: &Dom<E>, s: &mut Sc<'s, E>| err(d, s, "NotSupportedError", "This key cannot be exported in this format.");
    let bytes = match (k.alg, k.kind, fmt) {
        (Alg::Hmac, _, Fmt::Raw) | (Alg::Ecdsa, Kind::Public, Fmt::Raw) => k.bytes.clone(),
        (Alg::Rsa, Kind::Public, Fmt::Spki) => {
            let (n, e) = parse_rsa_public(&k.bytes).unwrap_or_default();
            seq(&[alg_id(&OID_RSA, der::tlv(der::NULL, &[])), der::tlv(der::BIT_STRING, &[vec![0], rsa_public_der(&n, &e)].concat())])
        }
        (Alg::Rsa, Kind::Private, Fmt::Pkcs8) => seq(&[der::uint(&[0]), alg_id(&OID_RSA, der::tlv(der::NULL, &[])), der::tlv(der::OCTET_STRING, &k.bytes)]),
        (Alg::Ecdsa, Kind::Public, Fmt::Spki) => seq(&[ec_alg_id(curve_of(k.param).2), der::tlv(der::BIT_STRING, &[vec![0], k.bytes.clone()].concat())]),
        (Alg::Ecdsa, Kind::Private, Fmt::Pkcs8) => {
            let q = ecc::public_from_scalar(curve_of(k.param).0, &k.bytes).unwrap_or_default();
            let inner = seq(&[der::uint(&[1]), der::tlv(der::OCTET_STRING, &k.bytes), der::tlv(0xa1, &der::tlv(der::BIT_STRING, &[vec![0], q].concat()))]);
            seq(&[der::uint(&[0]), ec_alg_id(curve_of(k.param).2), der::tlv(der::OCTET_STRING, &inner)])
        }
        (_, _, Fmt::Jwk) => return export_jwk(d, s, k),
        _ => return Err(no(d, s)),
    };
    s.new_array_buffer(bytes)
}

fn export_jwk<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, k: &Key) -> Res<'s, Local<'s>> {
    let o = s.new_object()?;
    let mut ops = Vec::new();
    for (i, n) in USAGES.iter().enumerate() {
        if k.usages & (1 << i) != 0 {
            ops.push(d.js_str_s(s, n)?);
        }
    }
    let ops = s.new_array(&ops)?;
    s.define_str(o, "key_ops", ops, Attrs::ALL)?;
    let ext = s.boolean(k.extractable);
    s.define_str(o, "ext", ext, Attrs::ALL)?;
    match (k.alg, k.kind) {
        (Alg::Hmac, _) => {
            put(d, s, o, "kty", "oct")?;
            put(d, s, o, "k", &b64(&k.bytes))?;
            put(d, s, o, "alg", &format!("HS{}", k.hash.short()))?;
        }
        (Alg::Rsa, kind) => {
            put(d, s, o, "alg", &format!("RS{}", k.hash.short()))?;
            put(d, s, o, "kty", "RSA")?;
            if kind == Kind::Private {
                let Some(p) = parse_rsa_private(&k.bytes) else { return Err(err(d, s, "OperationError", "The key data is not valid.")) };
                for (name, v) in [("n", &p.n), ("e", &p.e), ("d", &p.d), ("p", &p.p), ("q", &p.q), ("dp", &p.dp), ("dq", &p.dq), ("qi", &p.qi)] {
                    put(d, s, o, name, &b64(v))?;
                }
            } else {
                let (n, e) = parse_rsa_public(&k.bytes).unwrap_or_default();
                put(d, s, o, "n", &b64(&n))?;
                put(d, s, o, "e", &b64(&e))?;
            }
        }
        (_, kind) => {
            let (curve, crv, _) = curve_of(k.param);
            let q = if kind == Kind::Private { ecc::public_from_scalar(curve, &k.bytes).unwrap_or_default() } else { k.bytes.clone() };
            put(d, s, o, "kty", "EC")?;
            put(d, s, o, "x", &b64(q.get(1..=curve.size()).unwrap_or_default()))?;
            put(d, s, o, "y", &b64(q.get(1 + curve.size()..).unwrap_or_default()))?;
            put(d, s, o, "crv", crv)?;
            if kind == Kind::Private {
                put(d, s, o, "d", &b64(&k.bytes))?;
            }
        }
    }
    Ok(o)
}

// ------------------------------------------------------------------------------------------ sign and verify

/// The EMSA-PKCS1-v1_5 encoding of a digest in `k` bytes (RFC 8017 section 9.2). `None` if `k` is too short.
fn emsa(h: Hash, digest: &[u8], k: usize) -> Option<Vec<u8>> {
    let t = [h.info(), digest].concat();
    if k < t.len() + 11 {
        return None;
    }
    let mut em = vec![0xff; k];
    em[0] = 0;
    em[1] = 1;
    em[k - t.len() - 1] = 0;
    em[k - t.len()..].copy_from_slice(&t);
    Some(em)
}

/// A digest as `size` bytes for ECDSA: the leftmost bytes, or padded with zeros on the left.
fn ec_digest(digest: Vec<u8>, size: usize) -> Vec<u8> {
    if digest.len() >= size { digest[..size].to_vec() } else { [vec![0; size - digest.len()], digest].concat() }
}

/// The signature of `data` with the key `k` (`verify` is false), or whether `sig` is a signature of `data` (`verify` is true).
/// Gives the signature bytes, or `[1]` or `[0]` for a check.
pub(crate) fn sign_verify<'s, E: Eng>(d: &Dom<E>, s: &mut Sc<'s, E>, k: &Key, ec_hash: Hash, data: &[u8], sig: Option<&[u8]>) -> Res<'s, Vec<u8>> {
    let op_failed = |d: &Dom<E>, s: &mut Sc<'s, E>| err(d, s, "OperationError", "The operation failed for an operation-specific reason.");
    let ok = match (k.alg, sig) {
        (Alg::Hmac, None) => return Ok(hmac(k.hash, &k.bytes, data)),
        (Alg::Hmac, Some(sig)) => hmac(k.hash, &k.bytes, data) == sig,
        (Alg::Rsa, None) => {
            let Some(pk) = parse_rsa_private(&k.bytes) else { return Err(op_failed(d, s)) };
            s.charge((k.param as u64).pow(3) / 340_000)?;
            return match emsa(k.hash, &k.hash.digest(data), pk.n.len()).and_then(|em| pk.sign_raw(&em)) {
                Some(sig) => Ok(sig),
                None => Err(op_failed(d, s)),
            };
        }
        (Alg::Rsa, Some(sig)) => {
            let (n, e) = parse_rsa_public(&k.bytes).unwrap_or_default();
            s.charge(1000)?;
            sig.len() == n.len() && emsa(k.hash, &k.hash.digest(data), n.len()).is_some_and(|em| rsa_key::public_op(&n, &e, sig).is_some_and(|m| m == em))
        }
        (_, None) => {
            let (curve, size) = (curve_of(k.param).0, curve_of(k.param).0.size());
            let dg = ec_digest(ec_hash.digest(data), size);
            s.charge(4000)?;
            for _ in 0..8 {
                let nonce = random_bytes(d, s, size)?;
                if let Some(sig) = ecc::sign(curve, &k.bytes, &dg, &nonce) {
                    return Ok(sig);
                }
            }
            return Err(op_failed(d, s));
        }
        (_, Some(sig)) => {
            let curve = curve_of(k.param).0;
            s.charge(5000)?;
            sig.len() == 2 * curve.size() && ecc::verify(curve, &k.bytes, &ec_digest(ec_hash.digest(data), curve.size()), sig).is_ok()
        }
    };
    Ok(vec![ok as u8])
}
