//! X.509 v3 certificate parser for the TLS path (RFC 5280, RFC 5480, RFC 5758, X.690 DER).
//!
//! PARSING ONLY. A parsed certificate is NOT a verified certificate. This module does
//! not check a signature, build or validate a chain, check a trust anchor, match a host
//! name, or apply policy. It does not decide that a key usage or a critical extension is
//! acceptable. It returns facts about the bytes; a later validator must decide.
//!
//! Everything borrows from the input slice. There is no allocation and no recursion.
//! Work is linear in the input, except the duplicate-extension check, which is at most
//! [`MAX_EXTENSIONS`]^2 / 2 slice comparisons over a 1 KiB stack array of OIDs.
//!
//! Limits: a certificate is at most [`MAX_CERT_LEN`] bytes and has at most
//! [`MAX_EXTENSIONS`] extensions. A serial number is positive and at most 20 octets
//! (RFC 5280 4.1.2.2). Tags with number >= 31 are rejected. Dates must use the RFC 5280
//! forms: `YYMMDDHHMMSSZ` for years 1950-2049, `YYYYMMDDHHMMSSZ` from 2050.
//!
//! Strictness beyond RFC 5280 (each is an error here): explicit DEFAULT values
//! (`version` v1, `critical FALSE`, `cA FALSE`); `pathLenConstraint` when `cA` is not
//! true; empty DNS names or DNS names with bytes outside 0x21..=0x7E; non-canonical
//! `keyUsage` bit strings; the inner and outer signature `AlgorithmIdentifier` differing
//! by even one byte; compressed or malformed EC points.

use crate::der::{self, Reader, Tlv};

pub const MAX_CERT_LEN: usize = 64 * 1024;
pub const MAX_EXTENSIONS: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Der(der::Error),
    TooLarge,
    /// Unknown version, or an explicit v1, or a field that the version does not allow.
    Version,
    /// Serial number is zero, negative or longer than 20 octets.
    Serial,
    /// AlgorithmIdentifier is malformed, or has parameters that RFC 5758 / RFC 4055 forbid.
    Algorithm,
    /// Inner (TBS) and outer signature algorithm differ (RFC 5280 4.1.1.2).
    AlgorithmMismatch,
    /// Name is not `SEQUENCE OF SET OF SEQUENCE { OID, value }`, or the issuer is empty.
    Name,
    /// Date is malformed or out of range.
    Time,
    /// SubjectPublicKeyInfo is malformed, an EC point is not 0x04 || X || Y, or an RSA key is
    /// malformed or outside the policy of `rsa` (2048 to 4096 bits, e below 2^32).
    PublicKey,
    /// Extension encoding is malformed, empty, or repeats an OID.
    Extension,
    TooManyExtensions,
    /// A GeneralName is malformed.
    GeneralName,
}

impl From<der::Error> for Error {
    fn from(e: der::Error) -> Self {
        Error::Der(e)
    }
}

type Result<T> = core::result::Result<T, Error>;

// OBJECT IDENTIFIER content octets.
const OID_ECDSA_SHA256: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03, 0x02];
const OID_ECDSA_SHA384: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03, 0x03];
const OID_RSA_SHA256: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x0b];
const OID_RSA_SHA384: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x0c];
const OID_RSA_ENCRYPTION: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01];
const OID_EC_PUBLIC_KEY: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01];
const OID_P256: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07];
const OID_P384: &[u8] = &[0x2b, 0x81, 0x04, 0x00, 0x22];
const OID_SKI: &[u8] = &[0x55, 0x1d, 0x0e];
const OID_KEY_USAGE: &[u8] = &[0x55, 0x1d, 0x0f];
const OID_SAN: &[u8] = &[0x55, 0x1d, 0x11];
const OID_BASIC_CONSTRAINTS: &[u8] = &[0x55, 0x1d, 0x13];
const OID_AKI: &[u8] = &[0x55, 0x1d, 0x23];
const OID_EKU: &[u8] = &[0x55, 0x1d, 0x25];
const OID_EKU_ANY: &[u8] = &[0x55, 0x1d, 0x25, 0x00];
const OID_EKU_SERVER_AUTH: &[u8] = &[0x2b, 0x06, 0x01, 0x05, 0x05, 0x07, 0x03, 0x01];

/// Signature algorithm of a certificate. `Unsupported` carries the OID content octets.
/// Recognising an algorithm here does not mean that this crate can verify it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SigAlg<'a> {
    EcdsaSha256,
    EcdsaSha384,
    /// sha256WithRSAEncryption (RSASSA-PKCS1-v1_5 with SHA-256).
    RsaPkcs1Sha256,
    /// sha384WithRSAEncryption.
    RsaPkcs1Sha384,
    Unsupported(&'a [u8]),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Curve {
    P256,
    P384,
}

impl Curve {
    /// Size of a field element, and of each of r and s, in octets.
    pub fn field_len(self) -> usize {
        match self {
            Curve::P256 => 32,
            Curve::P384 => 48,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublicKey<'a> {
    /// `point` is the uncompressed SEC1 point 0x04 || X || Y (65 or 97 octets).
    /// It is NOT checked to lie on the curve.
    Ec { curve: Curve, point: &'a [u8] },
    /// rsaEncryption, 2048 to 4096 bits, odd modulus, odd exponent below 2^32.
    Rsa(crate::rsa::PublicKey<'a>),
    /// Any other algorithm or curve. `algorithm` is the OID content.
    Unsupported { algorithm: &'a [u8] },
}

/// A UTC instant as seconds since 1970-01-01T00:00:00Z. Leap seconds do not exist here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Time(pub i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BasicConstraints {
    pub critical: bool,
    pub ca: bool,
    /// `None` means no pathLenConstraint was encoded. `Some(0)` means a constraint of 0.
    pub path_len: Option<u32>,
}

/// keyUsage bit `i` of RFC 5280 4.2.1.3 is `1 << i`. Use the `KU_*` constants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyUsage {
    pub critical: bool,
    pub bits: u16,
}

pub const KU_DIGITAL_SIGNATURE: u16 = 1 << 0;
pub const KU_NON_REPUDIATION: u16 = 1 << 1;
pub const KU_KEY_ENCIPHERMENT: u16 = 1 << 2;
pub const KU_DATA_ENCIPHERMENT: u16 = 1 << 3;
pub const KU_KEY_AGREEMENT: u16 = 1 << 4;
pub const KU_KEY_CERT_SIGN: u16 = 1 << 5;
pub const KU_CRL_SIGN: u16 = 1 << 6;
pub const KU_ENCIPHER_ONLY: u16 = 1 << 7;
pub const KU_DECIPHER_ONLY: u16 = 1 << 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExtKeyUsage {
    pub critical: bool,
    pub server_auth: bool,
    pub any: bool,
    /// At least one other purpose OID is listed.
    pub other: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeneralName<'a> {
    /// dNSName: non-empty, bytes 0x21..=0x7E. Not lower-cased and not matched here.
    Dns(&'a [u8]),
    /// iPAddress: 4 or 16 octets.
    Ip(&'a [u8]),
    /// A valid name of another form. The value is the context tag.
    Other(u8),
}

/// subjectAltName. The names were validated when the certificate was parsed.
#[derive(Debug, Clone, Copy)]
pub struct San<'a> {
    pub critical: bool,
    names: &'a [u8],
}

impl<'a> San<'a> {
    pub fn names(&self) -> impl Iterator<Item = GeneralName<'a>> + 'a {
        let mut r = Reader::new(self.names);
        core::iter::from_fn(move || if r.is_empty() { None } else { general_name(&mut r).ok() })
    }
}

#[derive(Debug, Clone, Copy)]
pub struct AuthorityKeyId<'a> {
    /// keyIdentifier, if present. authorityCertIssuer and serial are checked, not exposed.
    pub key_id: Option<&'a [u8]>,
}

/// One extension as encoded.
#[derive(Debug, Clone, Copy)]
pub struct Extension<'a> {
    /// OID content octets.
    pub oid: &'a [u8],
    pub critical: bool,
    /// The extnValue OCTET STRING content (itself DER).
    pub value: &'a [u8],
}

/// Parsed extensions. `None` means the extension is absent; that is not the same as present
/// with a false or zero value.
#[derive(Debug, Clone, Copy)]
pub struct Extensions<'a> {
    raw: &'a [u8],
    pub count: usize,
    pub basic_constraints: Option<BasicConstraints>,
    pub key_usage: Option<KeyUsage>,
    pub ext_key_usage: Option<ExtKeyUsage>,
    pub san: Option<San<'a>>,
    pub subject_key_id: Option<&'a [u8]>,
    pub authority_key_id: Option<AuthorityKeyId<'a>>,
    /// OID of the first CRITICAL extension this module does not interpret (anything other
    /// than the six above). A validator MUST reject the certificate when this is `Some`
    /// (RFC 5280 4.2), unless it implements that extension itself via [`Extensions::iter`].
    pub unhandled_critical: Option<&'a [u8]>,
}

impl<'a> Extensions<'a> {
    /// All extensions in encoded order.
    pub fn iter(&self) -> impl Iterator<Item = Extension<'a>> + 'a {
        let mut r = Reader::new(self.raw);
        core::iter::from_fn(move || if r.is_empty() { None } else { read_extension(&mut r).ok() })
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Certificate<'a> {
    /// The whole certificate.
    pub der: &'a [u8],
    /// The exact signed bytes: the TBSCertificate TLV, header included.
    pub tbs: &'a [u8],
    /// 1, 2 or 3 (the encoded value plus one).
    pub version: u8,
    /// Positive INTEGER magnitude, no leading zero octet.
    pub serial: &'a [u8],
    /// Outer signatureAlgorithm. Byte-equal to the inner one.
    pub sig_alg: SigAlg<'a>,
    /// signatureValue BIT STRING octets (unused bits are zero). For ECDSA, see [`EcdsaSig::parse`].
    pub signature: &'a [u8],
    /// Whole Name TLV (header included), for byte-exact comparison with another subject.
    pub issuer: &'a [u8],
    pub subject: &'a [u8],
    pub not_before: Time,
    pub not_after: Time,
    /// The whole SubjectPublicKeyInfo TLV.
    pub spki: &'a [u8],
    pub public_key: PublicKey<'a>,
    pub extensions: Extensions<'a>,
}

impl Certificate<'_> {
    /// `not_before <= now <= not_after` for a caller-supplied Unix time. Nothing else.
    pub fn valid_at(&self, now: i64) -> bool {
        self.not_before.0 <= now && now <= self.not_after.0
    }
}

/// Parse one DER certificate. `der` must be exactly one certificate with no extra bytes.
pub fn parse(der: &[u8]) -> Result<Certificate<'_>> {
    if der.len() > MAX_CERT_LEN {
        return Err(Error::TooLarge);
    }
    let mut top = Reader::new(der);
    let cert = top.read_tag(der::SEQUENCE)?;
    top.finish()?;
    let mut r = Reader::new(cert.content);
    let tbs = r.read_tag(der::SEQUENCE)?;
    let outer = r.read_tag(der::SEQUENCE)?;
    let sig = r.read_tag(der::BIT_STRING)?;
    r.finish()?;
    let (unused, signature) = der::bit_string(sig.content)?;
    if unused != 0 {
        return Err(Error::Der(der::Error::BadBitString));
    }

    let mut t = Reader::new(tbs.content);
    let version = match t.read_opt(0xa0)? {
        None => 1,
        Some(v) => {
            let mut vr = Reader::new(v.content);
            let i = vr.read_tag(der::INTEGER)?;
            vr.finish()?;
            match i.content {
                [1] => 2,
                [2] => 3,
                _ => return Err(Error::Version), // 0 (v1) must be omitted in DER; others unknown
            }
        }
    };
    let serial_tlv = t.read_tag(der::INTEGER)?;
    let serial = der::unsigned(serial_tlv.content).map_err(|_| Error::Serial)?;
    if serial == [0] || serial_tlv.content.len() > 20 {
        return Err(Error::Serial);
    }
    let inner = t.read_tag(der::SEQUENCE)?;
    if inner.full != outer.full {
        // Parse both for a precise error before reporting the mismatch.
        sig_alg(inner)?;
        sig_alg(outer)?;
        return Err(Error::AlgorithmMismatch);
    }
    let sig_alg = sig_alg(outer)?;
    let issuer = t.read_tag(der::SEQUENCE)?;
    check_name(issuer)?;
    if issuer.content.is_empty() {
        return Err(Error::Name);
    }
    let mut v = Reader::new(t.read_tag(der::SEQUENCE)?.content);
    let not_before = time(v.read()?)?;
    let not_after = time(v.read()?)?;
    v.finish()?;
    let subject = t.read_tag(der::SEQUENCE)?;
    check_name(subject)?;
    let spki = t.read_tag(der::SEQUENCE)?;
    let public_key = public_key(spki)?;
    if version >= 2 {
        // issuerUniqueID [1] and subjectUniqueID [2]: IMPLICIT BIT STRING, checked and ignored.
        for tag in [0x81, 0x82] {
            if let Some(u) = t.read_opt(tag)? {
                der::bit_string(u.content)?;
            }
        }
    }
    let extensions = match t.read_opt(0xa3)? {
        Some(e) if version == 3 => {
            let mut er = Reader::new(e.content);
            let seq = er.read_tag(der::SEQUENCE)?;
            er.finish()?;
            extensions(seq.content)?
        }
        Some(_) => return Err(Error::Version),
        None => Extensions::empty(),
    };
    t.finish()?;

    Ok(Certificate {
        der,
        tbs: tbs.full,
        version,
        serial,
        sig_alg,
        signature,
        issuer: issuer.full,
        subject: subject.full,
        not_before,
        not_after,
        spki: spki.full,
        public_key,
        extensions,
    })
}

fn sig_alg(t: Tlv<'_>) -> Result<SigAlg<'_>> {
    let mut r = Reader::new(t.content);
    let oid = r.read_tag(der::OID)?.content;
    der::check_oid(oid)?;
    let params = if r.is_empty() { None } else { Some(r.read()?) };
    r.finish()?;
    let null = |p: Option<Tlv<'_>>| matches!(p, Some(p) if p.tag == der::NULL && p.content.is_empty());
    match oid {
        // RFC 5758 3.2: parameters MUST be absent.
        OID_ECDSA_SHA256 | OID_ECDSA_SHA384 if params.is_some() => Err(Error::Algorithm),
        OID_ECDSA_SHA256 => Ok(SigAlg::EcdsaSha256),
        OID_ECDSA_SHA384 => Ok(SigAlg::EcdsaSha384),
        // RFC 4055 5: parameters MUST be NULL.
        OID_RSA_SHA256 | OID_RSA_SHA384 if !null(params) => Err(Error::Algorithm),
        OID_RSA_SHA256 => Ok(SigAlg::RsaPkcs1Sha256),
        OID_RSA_SHA384 => Ok(SigAlg::RsaPkcs1Sha384),
        _ => Ok(SigAlg::Unsupported(oid)),
    }
}

/// Name ::= SEQUENCE OF RelativeDistinguishedName; RDN ::= SET SIZE (1..) OF { OID, ANY }.
fn check_name(name: Tlv<'_>) -> Result<()> {
    let mut rdns = Reader::new(name.content);
    while !rdns.is_empty() {
        let set = rdns.read_tag(der::SET).map_err(|_| Error::Name)?;
        if set.content.is_empty() {
            return Err(Error::Name);
        }
        let mut atvs = Reader::new(set.content);
        let mut previous: Option<&[u8]> = None;
        while !atvs.is_empty() {
            let atv = atvs.read_tag(der::SEQUENCE).map_err(|_| Error::Name)?;
            // X.690 11.6: SET OF elements are ordered by their complete DER encodings.
            if previous.is_some_and(|p| p > atv.full) {
                return Err(Error::Name);
            }
            previous = Some(atv.full);
            let mut a = Reader::new(atv.content);
            let oid = a.read_tag(der::OID).map_err(|_| Error::Name)?;
            der::check_oid(oid.content).map_err(|_| Error::Name)?;
            a.read().map_err(|_| Error::Name)?;
            a.finish().map_err(|_| Error::Name)?;
        }
    }
    Ok(())
}

/// RFC 5280 4.1.2.5: UTCTime `YYMMDDHHMMSSZ` for 1950-2049, GeneralizedTime
/// `YYYYMMDDHHMMSSZ` from 2050. No fractions, no offsets, seconds required, no leap second.
fn time(t: Tlv<'_>) -> Result<Time> {
    let c = t.content;
    let (year, rest) = match (t.tag, c.len()) {
        (der::UTC_TIME, 13) => {
            let yy = num(&c[0..2])? as i64;
            (if yy >= 50 { 1900 + yy } else { 2000 + yy }, &c[2..])
        }
        (der::GENERALIZED_TIME, 15) => {
            let y = num(&c[0..4])? as i64;
            if y < 2050 {
                return Err(Error::Time);
            }
            (y, &c[4..])
        }
        _ => return Err(Error::Time),
    };
    if rest[10] != b'Z' {
        return Err(Error::Time);
    }
    let (mo, d, h, mi, s) = (
        num(&rest[0..2])? as i64,
        num(&rest[2..4])? as i64,
        num(&rest[4..6])?,
        num(&rest[6..8])?,
        num(&rest[8..10])?,
    );
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let mdays = match mo {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return Err(Error::Time),
    };
    if d < 1 || d > mdays || h > 23 || mi > 59 || s > 59 {
        return Err(Error::Time);
    }
    Ok(Time(days_from_civil(year, mo, d) * 86400 + (h * 3600 + mi * 60 + s) as i64))
}

/// Exactly ASCII digits.
fn num(s: &[u8]) -> Result<u32> {
    s.iter().try_fold(0u32, |a, &b| {
        if b.is_ascii_digit() { Ok(a * 10 + (b - b'0') as u32) } else { Err(Error::Time) }
    })
}

/// Days from 1970-01-01 to a proleptic Gregorian date (Hinnant's civil-days algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// RFC 5480 2.1.1: id-ecPublicKey with a namedCurve OID. RFC 8017 A.1: rsaEncryption with NULL
/// parameters and an RSAPublicKey; a key outside the policy of [`crate::rsa`] is an error.
/// Other keys are `Unsupported`.
fn public_key(spki: Tlv<'_>) -> Result<PublicKey<'_>> {
    let mut r = Reader::new(spki.content);
    let alg = r.read_tag(der::SEQUENCE)?;
    let key = r.read_tag(der::BIT_STRING)?;
    r.finish()?;
    let (unused, point) = der::bit_string(key.content).map_err(|_| Error::PublicKey)?;
    if unused != 0 {
        return Err(Error::PublicKey);
    }
    let mut a = Reader::new(alg.content);
    let oid = a.read_tag(der::OID)?.content;
    der::check_oid(oid)?;
    let params = if a.is_empty() { None } else { Some(a.read()?) };
    a.finish()?;
    if oid == OID_RSA_ENCRYPTION {
        if !matches!(params, Some(p) if p.tag == der::NULL && p.content.is_empty()) {
            return Err(Error::PublicKey);
        }
        return crate::rsa::PublicKey::from_pkcs1(point).map(PublicKey::Rsa).ok_or(Error::PublicKey);
    }
    if oid != OID_EC_PUBLIC_KEY {
        return Ok(PublicKey::Unsupported { algorithm: oid });
    }
    let p = params.ok_or(Error::PublicKey)?;
    let curve = match (p.tag, p.content) {
        (der::OID, OID_P256) => Curve::P256,
        (der::OID, OID_P384) => Curve::P384,
        _ => return Ok(PublicKey::Unsupported { algorithm: oid }),
    };
    if point.len() != 1 + 2 * curve.field_len() || point[0] != 4 {
        return Err(Error::PublicKey);
    }
    Ok(PublicKey::Ec { curve, point })
}

fn read_extension<'a>(r: &mut Reader<'a>) -> Result<Extension<'a>> {
    let e = r.read_tag(der::SEQUENCE)?;
    let mut x = Reader::new(e.content);
    let oid = x.read_tag(der::OID)?.content;
    der::check_oid(oid)?;
    let critical = match x.read_opt(der::BOOLEAN)? {
        None => false,
        Some(b) if der::boolean(b.content)? => true,
        Some(_) => return Err(Error::Extension), // DEFAULT FALSE must not be encoded
    };
    let value = x.read_tag(der::OCTET_STRING)?.content;
    x.finish()?;
    Ok(Extension { oid, critical, value })
}

impl<'a> Extensions<'a> {
    fn empty() -> Self {
        Extensions {
            raw: &[],
            count: 0,
            basic_constraints: None,
            key_usage: None,
            ext_key_usage: None,
            san: None,
            subject_key_id: None,
            authority_key_id: None,
            unhandled_critical: None,
        }
    }
}

fn extensions(raw: &[u8]) -> Result<Extensions<'_>> {
    let mut out = Extensions { raw, ..Extensions::empty() };
    let mut seen: [&[u8]; MAX_EXTENSIONS] = [&[]; MAX_EXTENSIONS];
    let mut r = Reader::new(raw);
    while !r.is_empty() {
        if out.count == MAX_EXTENSIONS {
            return Err(Error::TooManyExtensions);
        }
        let e = read_extension(&mut r)?;
        // Duplicate check against every earlier OID (RFC 5280 4.2): at most 64 * 63 / 2 compares.
        if seen[..out.count].contains(&e.oid) {
            return Err(Error::Extension);
        }
        seen[out.count] = e.oid;
        out.count += 1;
        let mut v = Reader::new(e.value);
        match e.oid {
            OID_BASIC_CONSTRAINTS => out.basic_constraints = Some(basic_constraints(&mut v, e.critical)?),
            OID_KEY_USAGE => out.key_usage = Some(key_usage(&mut v, e.critical)?),
            OID_EKU => out.ext_key_usage = Some(ext_key_usage(&mut v, e.critical)?),
            OID_SAN => out.san = Some(san(&mut v, e.critical)?),
            OID_SKI => {
                let k = v.read_tag(der::OCTET_STRING)?.content;
                if k.is_empty() {
                    return Err(Error::Extension);
                }
                out.subject_key_id = Some(k);
            }
            OID_AKI => out.authority_key_id = Some(authority_key_id(&mut v)?),
            _ => {
                if e.critical && out.unhandled_critical.is_none() {
                    out.unhandled_critical = Some(e.oid);
                }
                continue; // contents not interpreted
            }
        }
        v.finish()?;
    }
    if out.count == 0 {
        return Err(Error::Extension); // SIZE (1..MAX)
    }
    Ok(out)
}

/// BasicConstraints ::= SEQUENCE { cA BOOLEAN DEFAULT FALSE, pathLenConstraint INTEGER (0..MAX) OPTIONAL }
fn basic_constraints(v: &mut Reader<'_>, critical: bool) -> Result<BasicConstraints> {
    let mut s = Reader::new(v.read_tag(der::SEQUENCE)?.content);
    let ca = match s.read_opt(der::BOOLEAN)? {
        None => false,
        Some(b) if der::boolean(b.content)? => true,
        Some(_) => return Err(Error::Extension),
    };
    let path_len = match s.read_opt(der::INTEGER)? {
        None => None,
        Some(i) if ca => {
            let m = der::unsigned(i.content)?;
            if m.len() > 4 {
                return Err(Error::Extension);
            }
            Some(m.iter().fold(0u32, |a, &b| (a << 8) | b as u32))
        }
        Some(_) => return Err(Error::Extension),
    };
    s.finish()?;
    Ok(BasicConstraints { critical, ca, path_len })
}

/// KeyUsage ::= BIT STRING, DER named-bit form: at least one bit, no trailing zero bits.
fn key_usage(v: &mut Reader<'_>, critical: bool) -> Result<KeyUsage> {
    let (unused, data) = der::bit_string(v.read_tag(der::BIT_STRING)?.content)?;
    let ok = match data {
        [last] => last.trailing_zeros() == unused as u32 && *last != 0,
        [_, last] => *last != 0 && last.trailing_zeros() == unused as u32 && last & 0x7f == 0,
        _ => false, // empty, or bits above decipherOnly (index 8)
    };
    if !ok {
        return Err(Error::Extension);
    }
    let mut bits = 0u16;
    for (i, &b) in data.iter().enumerate() {
        for j in 0..8 {
            if b & (0x80 >> j) != 0 {
                bits |= 1 << (i * 8 + j);
            }
        }
    }
    Ok(KeyUsage { critical, bits })
}

/// ExtKeyUsageSyntax ::= SEQUENCE SIZE (1..MAX) OF KeyPurposeId
fn ext_key_usage(v: &mut Reader<'_>, critical: bool) -> Result<ExtKeyUsage> {
    let mut s = Reader::new(v.read_tag(der::SEQUENCE)?.content);
    let mut eku = ExtKeyUsage { critical, server_auth: false, any: false, other: false };
    let mut n = 0;
    while !s.is_empty() {
        let o = s.read_tag(der::OID)?.content;
        der::check_oid(o)?;
        n += 1;
        match o {
            OID_EKU_SERVER_AUTH => eku.server_auth = true,
            OID_EKU_ANY => eku.any = true,
            _ => eku.other = true,
        }
    }
    if n == 0 {
        return Err(Error::Extension);
    }
    Ok(eku)
}

/// SubjectAltName ::= GeneralNames ::= SEQUENCE SIZE (1..MAX) OF GeneralName
fn san<'a>(v: &mut Reader<'a>, critical: bool) -> Result<San<'a>> {
    let names = v.read_tag(der::SEQUENCE)?.content;
    if names.is_empty() {
        return Err(Error::Extension);
    }
    let mut r = Reader::new(names);
    while !r.is_empty() {
        general_name(&mut r)?;
    }
    Ok(San { critical, names })
}

fn general_name<'a>(r: &mut Reader<'a>) -> Result<GeneralName<'a>> {
    let t = r.read()?;
    let c = t.content;
    match t.tag {
        0x82 if !c.is_empty() && c.iter().all(|b| (0x21..=0x7e).contains(b)) => Ok(GeneralName::Dns(c)),
        0x87 if c.len() == 4 || c.len() == 16 => Ok(GeneralName::Ip(c)),
        // rfc822Name and uniformResourceIdentifier: IA5String (ASCII).
        0x81 | 0x86 if c.is_ascii() => Ok(GeneralName::Other(t.tag)),
        0x88 if der::check_oid(c).is_ok() => Ok(GeneralName::Other(t.tag)),
        // otherName, x400Address, directoryName, ediPartyName: constructed, content not checked.
        0xa0 | 0xa3 | 0xa4 | 0xa5 => Ok(GeneralName::Other(t.tag)),
        _ => Err(Error::GeneralName),
    }
}

/// AuthorityKeyIdentifier ::= SEQUENCE { [0] keyIdentifier OPTIONAL,
///   [1] authorityCertIssuer OPTIONAL, [2] authorityCertSerialNumber OPTIONAL }
/// RFC 5280: [1] and [2] appear together or not at all.
fn authority_key_id<'a>(v: &mut Reader<'a>) -> Result<AuthorityKeyId<'a>> {
    let mut s = Reader::new(v.read_tag(der::SEQUENCE)?.content);
    let key_id = match s.read_opt(0x80)? {
        Some(k) if k.content.is_empty() => return Err(Error::Extension),
        k => k.map(|k| k.content),
    };
    let issuer = s.read_opt(0xa1)?;
    let serial = s.read_opt(0x82)?;
    s.finish()?;
    if issuer.is_some() != serial.is_some() {
        return Err(Error::Extension);
    }
    if let Some(i) = issuer {
        let mut g = Reader::new(i.content);
        if g.is_empty() {
            return Err(Error::Extension);
        }
        while !g.is_empty() {
            general_name(&mut g)?;
        }
    }
    if let Some(n) = serial {
        der::check_integer(n.content)?;
    }
    Ok(AuthorityKeyId { key_id })
}

/// A DER ECDSA-Sig-Value (RFC 3279 2.2.3): `SEQUENCE { r INTEGER, s INTEGER }`.
/// `r` and `s` are positive, minimal, big-endian, without the sign octet, and at most
/// the curve's field length. They are NOT checked against the group order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EcdsaSig<'a> {
    pub r: &'a [u8],
    pub s: &'a [u8],
}

impl<'a> EcdsaSig<'a> {
    /// The curve is that of the SIGNER's key, not of the signature algorithm OID.
    pub fn parse(der: &'a [u8], curve: Curve) -> core::result::Result<Self, der::Error> {
        let mut top = Reader::new(der);
        let seq = top.read_tag(der::SEQUENCE)?;
        top.finish()?;
        let mut r = Reader::new(seq.content);
        let mut comp = || -> core::result::Result<&'a [u8], der::Error> {
            let m = der::unsigned(r.read_tag(der::INTEGER)?.content)?;
            if m == [0] || m.len() > curve.field_len() {
                return Err(der::Error::BadInteger);
            }
            Ok(m)
        };
        let (rv, sv) = (comp()?, comp()?);
        r.finish()?;
        Ok(EcdsaSig { r: rv, s: sv })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIX: [(&str, &[u8]); 5] = [
        ("staging-0-leaf", include_bytes!("../../../tools/fixtures/x509/staging-0-leaf.der")),
        ("staging-1-le-ye2", include_bytes!("../../../tools/fixtures/x509/staging-1-le-ye2.der")),
        ("staging-2-isrg-root-ye", include_bytes!("../../../tools/fixtures/x509/staging-2-isrg-root-ye.der")),
        (
            "staging-3-isrg-x2-crosssigned",
            include_bytes!("../../../tools/fixtures/x509/staging-3-isrg-x2-crosssigned.der"),
        ),
        ("isrg-root-x2", include_bytes!("../../../tools/fixtures/x509/isrg-root-x2.der")),
    ];
    const TBS: [&[u8]; 5] = [
        include_bytes!("../../../tools/fixtures/x509/tbs/staging-0-leaf.tbs.der"),
        include_bytes!("../../../tools/fixtures/x509/tbs/staging-1-le-ye2.tbs.der"),
        include_bytes!("../../../tools/fixtures/x509/tbs/staging-2-isrg-root-ye.tbs.der"),
        include_bytes!("../../../tools/fixtures/x509/tbs/staging-3-isrg-x2-crosssigned.tbs.der"),
        include_bytes!("../../../tools/fixtures/x509/tbs/isrg-root-x2.tbs.der"),
    ];
    const EXPECT: &str = include_str!("../../../tools/fixtures/x509/expect.txt");

    fn exp(name: &str, key: &str) -> &'static str {
        let p = format!("{name}.{key}=");
        EXPECT.lines().find_map(|l| l.strip_prefix(p.as_str())).unwrap_or_else(|| panic!("no {p}"))
    }

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    fn tlv(tag: u8, c: &[u8]) -> Vec<u8> {
        let mut v = vec![tag];
        match c.len() {
            n if n < 128 => v.push(n as u8),
            n if n < 256 => v.extend([0x81, n as u8]),
            n => v.extend([0x82, (n >> 8) as u8, n as u8]),
        }
        v.extend(c);
        v
    }

    fn cat(parts: &[&[u8]]) -> Vec<u8> {
        parts.concat()
    }

    // ---- fixtures against openssl's output --------------------------------------------

    #[test]
    fn fixtures_match_openssl() {
        for (i, (name, der)) in FIX.iter().enumerate() {
            let c = parse(der).unwrap_or_else(|e| panic!("{name}: {e:?}"));
            assert_eq!(c.version, 3, "{name}");
            assert_eq!(hex(c.serial), exp(name, "serial"), "{name}");
            assert_eq!(c.not_before.0.to_string(), exp(name, "not_before"), "{name}");
            assert_eq!(c.not_after.0.to_string(), exp(name, "not_after"), "{name}");
            assert_eq!(hex(c.signature), exp(name, "sig"), "{name}");
            assert_eq!(c.tbs, TBS[i], "{name}: tbs differs from openssl asn1parse");
            assert_eq!(c.tbs.as_ptr() as usize - der.as_ptr() as usize, 4, "{name}");
            let alg = exp(name, "sig_alg");
            match c.sig_alg {
                SigAlg::EcdsaSha384 => assert_eq!(alg, "ecdsa-with-SHA384"),
                SigAlg::RsaPkcs1Sha256 => assert_eq!(alg, "sha256WithRSAEncryption"),
                other => panic!("{name}: {other:?}"),
            }
            match c.public_key {
                PublicKey::Ec { curve, point } => {
                    assert_eq!(hex(point), exp(name, "key"), "{name}");
                    assert_eq!(curve == Curve::P256, exp(name, "curve") == "P-256", "{name}");
                    assert_eq!(curve == Curve::P384, exp(name, "curve") == "P-384", "{name}");
                }
                other => panic!("{name}: {other:?}"),
            }
            let e = &c.extensions;
            assert_eq!(e.count.to_string(), exp(name, "ext_count"), "{name}");
            assert!(e.unhandled_critical.is_none(), "{name}");
            let bc = e.basic_constraints.unwrap();
            assert_eq!(bc.critical as u8, exp(name, "bc_critical").parse::<u8>().unwrap());
            assert_eq!(bc.ca as u8, exp(name, "bc_ca").parse::<u8>().unwrap());
            let pl = bc.path_len.map_or("none".to_string(), |p| p.to_string());
            assert_eq!(pl, exp(name, "bc_pathlen"), "{name}");
            let ku = e.key_usage.unwrap();
            assert_eq!(ku.bits.to_string(), exp(name, "ku"), "{name}");
            assert!(ku.critical);
            assert_eq!(e.ext_key_usage.is_some(), exp(name, "eku") != "absent", "{name}");
            if let Some(k) = e.ext_key_usage {
                assert_eq!(k.server_auth, exp(name, "eku").contains("Server"), "{name}");
                assert_eq!(k.other, exp(name, "eku").contains("Client"), "{name}");
            }
            let san: Vec<String> = e.san.iter().flat_map(|s| s.names()).map(|n| match n {
                GeneralName::Dns(d) => format!("DNS:{}", String::from_utf8_lossy(d)),
                other => format!("{other:?}"),
            }).collect();
            assert_eq!(if san.is_empty() { "absent".into() } else { san.join(",") }, exp(name, "san"), "{name}");
            assert_eq!(e.subject_key_id.map_or("absent".into(), hex), exp(name, "ski"), "{name}");
            let aki = e.authority_key_id.map(|a| hex(a.key_id.unwrap())).unwrap_or("absent".into());
            assert_eq!(aki, exp(name, "aki"), "{name}");
            assert_eq!(e.iter().count(), e.count, "{name}");
        }
    }

    #[test]
    fn staging_chain_links() {
        let c: Vec<_> = FIX[..4].iter().map(|(_, d)| parse(d).unwrap()).collect();
        for i in 0..3 {
            assert_eq!(c[i].issuer, c[i + 1].subject, "issuer {i} == subject {}", i + 1);
        }
        // The cross-sign and the independently fetched root have the same name and key.
        let x2 = parse(FIX[4].1).unwrap();
        assert_eq!(c[3].subject, x2.subject);
        assert_eq!(c[3].spki, x2.spki);
        assert_ne!(c[3].der, x2.der);
        assert_eq!(c[2].issuer, x2.subject);
        // Self-signed root: issuer == subject. The cross-sign is not.
        assert_eq!(x2.issuer, x2.subject);
        assert_ne!(c[3].issuer, c[3].subject);
        assert_eq!(x2.sig_alg, SigAlg::EcdsaSha384);
        assert_eq!(c[3].sig_alg, SigAlg::RsaPkcs1Sha256);
        // Constraints: absent pathLen is not pathLen 0.
        assert_eq!(c[1].extensions.basic_constraints.unwrap().path_len, Some(0));
        assert_eq!(c[2].extensions.basic_constraints.unwrap().path_len, None);
        assert!(!c[0].extensions.basic_constraints.unwrap().ca);
        assert!(c[1].extensions.key_usage.unwrap().bits & KU_KEY_CERT_SIGN != 0);
        assert!(c[0].extensions.key_usage.unwrap().bits & KU_KEY_CERT_SIGN == 0);
        assert!(c[0].extensions.ext_key_usage.unwrap().server_auth);
        assert_eq!(c[0].extensions.authority_key_id.unwrap().key_id, c[1].extensions.subject_key_id);
        // The leaf has an SAN, a public-key curve of P-256 and 10 extensions; the SCT list is not interpreted.
        let names: Vec<_> = c[0].extensions.san.unwrap().names().collect();
        assert_eq!(names, [GeneralName::Dns(b"staging.8vx.in"), GeneralName::Dns(b"staging.app.8vox.ai")]);
        assert!(matches!(c[0].public_key, PublicKey::Ec { curve: Curve::P256, .. }));
    }

    #[test]
    fn fixture_ecdsa_signatures() {
        // Certificates 0..2 and the root are signed by P-384 keys.
        for (name, der) in [FIX[0], FIX[1], FIX[2], FIX[4]] {
            let c = parse(der).unwrap();
            let s = EcdsaSig::parse(c.signature, Curve::P384).unwrap_or_else(|e| panic!("{name}: {e:?}"));
            assert!(s.r.len() <= 48 && s.s.len() <= 48 && s.r[0] != 0 && s.s[0] != 0, "{name}");
        }
        // The RSA cross-sign signature is not a DER ECDSA value.
        assert!(EcdsaSig::parse(parse(FIX[3].1).unwrap().signature, Curve::P384).is_err());
    }

    #[test]
    fn fixed_time_validity() {
        let leaf = parse(FIX[0].1).unwrap();
        let (nb, na) = (leaf.not_before.0, leaf.not_after.0);
        assert_eq!((nb, na), (1792055227, 1799831226)); // 2026-10-15 09:07:07Z .. 2027-01-13 09:07:06Z
        assert!(leaf.valid_at(nb) && leaf.valid_at(na) && leaf.valid_at(1795000000));
        assert!(!leaf.valid_at(nb - 1) && !leaf.valid_at(na + 1));
        // A root parses the same way after the leaf would have expired.
        let x2 = parse(FIX[4].1).unwrap();
        assert!(x2.valid_at(na + 10_000_000) && !x2.valid_at(2231510401));
    }

    // ---- dates ---------------------------------------------------------------------------

    fn tm(tag: u8, s: &str) -> Result<i64> {
        let b = tlv(tag, s.as_bytes());
        time(Reader::new(&b).read().unwrap()).map(|t| t.0)
    }

    #[test]
    fn dates() {
        let (u, g) = (der::UTC_TIME, der::GENERALIZED_TIME);
        assert_eq!(tm(u, "700101000000Z"), Ok(0));
        assert_eq!(tm(u, "500101000000Z"), Ok(-631152000)); // 50..99 is 19xx
        assert_eq!(tm(u, "491231235959Z"), Ok(2524607999)); // 00..49 is 20xx
        assert_eq!(tm(u, "000101000000Z"), Ok(946684800));
        assert_eq!(tm(u, "991231235959Z"), Ok(946684799));
        assert_eq!(tm(g, "20500101000000Z"), Ok(2524608000));
        assert_eq!(tm(u, "240229120000Z"), Ok(1709208000)); // leap day
        assert_eq!(tm(g, "20520229000000Z"), Ok(2592777600));
        assert_eq!(tm(g, "24000229000000Z"), Ok(13574563200)); // divisible by 400: leap
        assert!(tm(u, "230229000000Z").is_err());
        assert!(tm(g, "21000229000000Z").is_err()); // divisible by 100, not 400: not leap
        assert!(tm(u, "250431000000Z").is_err());
        assert!(tm(u, "250631000000Z").is_err());
        assert!(tm(u, "250430000000Z").is_ok());
        // Form errors: years < 2050 must be UTCTime; no fractions, offsets, missing seconds, leap second.
        assert!(tm(g, "20491231235959Z").is_err());
        assert!(tm(g, "19700101000000Z").is_err());
        assert!(tm(u, "2601010000Z").is_err());
        assert!(tm(u, "260101000000+0000").is_err());
        assert!(tm(u, "260101000000z").is_err());
        assert!(tm(u, "260101000000").is_err());
        assert!(tm(g, "20500101000000.0Z").is_err());
        assert!(tm(g, "205001010000Z").is_err());
        assert!(tm(u, "261301000000Z").is_err());
        assert!(tm(u, "260001000000Z").is_err());
        assert!(tm(u, "260100000000Z").is_err());
        assert!(tm(u, "260132000000Z").is_err());
        assert!(tm(u, "260101240000Z").is_err());
        assert!(tm(u, "260101006000Z").is_err());
        assert!(tm(u, "260101000060Z").is_err());
        assert!(tm(u, "26010100000aZ").is_err());
        assert!(tm(u, "+60101000000Z").is_err());
        assert!(tm(0x0c, "260101000000Z").is_err()); // UTF8String tag
        assert!(tm(u, "").is_err());
    }

    // ---- synthetic certificates ----------------------------------------------------------

    const NAME_OID_CN: &[u8] = &[0x55, 0x04, 0x03];
    fn name(cn: &str) -> Vec<u8> {
        let atv = tlv(0x30, &cat(&[&tlv(6, NAME_OID_CN), &tlv(0x0c, cn.as_bytes())]));
        tlv(0x30, &tlv(0x31, &atv))
    }
    fn alg(oid: &[u8]) -> Vec<u8> {
        tlv(0x30, &tlv(6, oid))
    }
    fn ec_spki(oid_curve: &[u8], point: &[u8]) -> Vec<u8> {
        let a = tlv(0x30, &cat(&[&tlv(6, OID_EC_PUBLIC_KEY), &tlv(6, oid_curve)]));
        tlv(0x30, &cat(&[&a, &tlv(3, &cat(&[&[0], point]))]))
    }
    fn p256_point() -> Vec<u8> {
        let mut p = vec![4];
        p.extend((0..64).map(|i| i as u8));
        p
    }
    fn ext(oid: &[u8], critical: bool, value: &[u8]) -> Vec<u8> {
        let crit: &[u8] = if critical { &[1, 1, 0xff] } else { &[] };
        tlv(0x30, &cat(&[&tlv(6, oid), crit, &tlv(4, value)]))
    }
    fn exts(list: &[Vec<u8>]) -> Vec<u8> {
        let all: Vec<u8> = list.concat();
        tlv(0xa3, &tlv(0x30, &all))
    }

    #[derive(Clone)]
    struct Parts {
        version: Vec<u8>,
        serial: Vec<u8>,
        inner: Vec<u8>,
        outer: Vec<u8>,
        issuer: Vec<u8>,
        validity: Vec<u8>,
        subject: Vec<u8>,
        spki: Vec<u8>,
        tail: Vec<u8>,
        sig: Vec<u8>,
    }
    impl Parts {
        fn ok() -> Parts {
            let t = |s: &str| tlv(der::UTC_TIME, s.as_bytes());
            Parts {
                version: tlv(0xa0, &tlv(2, &[2])),
                serial: tlv(2, &[1]),
                inner: alg(OID_ECDSA_SHA256),
                outer: alg(OID_ECDSA_SHA256),
                issuer: name("CA"),
                validity: tlv(0x30, &cat(&[&t("260101000000Z"), &t("270101000000Z")])),
                subject: name("leaf"),
                spki: ec_spki(OID_P256, &p256_point()),
                tail: vec![],
                sig: vec![0x30, 6, 2, 1, 1, 2, 1, 1],
            }
        }
        fn build(&self) -> Vec<u8> {
            let tbs = tlv(
                0x30,
                &cat(&[&self.version, &self.serial, &self.inner, &self.issuer, &self.validity, &self.subject, &self.spki, &self.tail]),
            );
            tlv(0x30, &cat(&[&tbs, &self.outer, &tlv(3, &cat(&[&[0], &self.sig]))]))
        }
        fn with_ext(mut self, list: &[Vec<u8>]) -> Parts {
            self.tail = exts(list);
            self
        }
        fn parse(&self) -> Result<Vec<u8>> {
            let b = self.build();
            let r = parse(&b).map(|_| ());
            r.map(|_| b)
        }
        fn err(&self) -> Error {
            self.parse().unwrap_err()
        }
    }
    fn parse_with(list: &[Vec<u8>]) -> core::result::Result<Vec<u8>, Error> {
        Parts::ok().with_ext(list).parse()
    }
    fn ext_err(list: &[Vec<u8>]) -> Error {
        parse_with(list).unwrap_err()
    }

    #[test]
    fn synthetic_baseline() {
        let b = Parts::ok().build();
        let c = parse(&b).unwrap();
        assert_eq!(c.version, 3);
        assert_eq!(c.serial, &[1]);
        assert_eq!(c.sig_alg, SigAlg::EcdsaSha256);
        assert_eq!(c.signature, &[0x30, 6, 2, 1, 1, 2, 1, 1]);
        assert_eq!(c.extensions.count, 0);
        assert!(c.extensions.basic_constraints.is_none() && c.extensions.san.is_none());
        assert_eq!(c.extensions.iter().count(), 0);
        assert_eq!(c.not_before, Time(1767225600));
        // v1: no version, no extensions.
        let mut p = Parts::ok();
        p.version = vec![];
        assert_eq!(parse(&p.build()).unwrap().version, 1);
        // v2 with a unique ID.
        let mut p = Parts::ok();
        p.version = tlv(0xa0, &tlv(2, &[1]));
        p.tail = tlv(0x81, &[0, 0xaa]);
        assert_eq!(parse(&p.build()).unwrap().version, 2);
    }

    #[test]
    fn version_rules() {
        let mut p = Parts::ok();
        p.version = tlv(0xa0, &tlv(2, &[0])); // explicit v1 is not DER
        assert_eq!(p.err(), Error::Version);
        p.version = tlv(0xa0, &tlv(2, &[3]));
        assert_eq!(p.err(), Error::Version);
        p.version = tlv(0xa0, &tlv(2, &[0, 2]));
        assert!(matches!(p.err(), Error::Version));
        let mut p = Parts::ok().with_ext(&[ext(OID_SKI, false, &tlv(4, &[1]))]);
        p.version = vec![]; // v1 with extensions
        assert_eq!(p.err(), Error::Version);
        p.version = tlv(0xa0, &tlv(2, &[1])); // v2 with extensions
        assert_eq!(p.err(), Error::Version);
        let mut p = Parts::ok();
        p.version = vec![];
        p.tail = tlv(0x81, &[0, 1]); // v1 with a unique ID
        assert!(p.parse().is_err());
    }

    #[test]
    fn serial_rules() {
        let mut p = Parts::ok();
        for (bytes, ok) in [
            (vec![0x7f], true),
            (vec![0x00, 0x80], true),
            ([vec![0x7f], vec![0xff; 19]].concat(), true), // 20 octets
            ([vec![0x00, 0x80], vec![0xff; 18]].concat(), true), // 20 octets with sign padding
            ([vec![0x00, 0x80], vec![0xff; 19]].concat(), false), // 21 octets
            (vec![0x00], false),                           // zero
            (vec![0x80], false),                           // negative
            (vec![0xff], false),
            (vec![0x00, 0x01], false), // padded
            (vec![], false),
        ] {
            p.serial = tlv(2, &bytes);
            assert_eq!(p.parse().is_ok(), ok, "{bytes:02x?}");
        }
        p.serial = tlv(4, &[1]);
        assert_eq!(p.err(), Error::Der(der::Error::WrongTag));
    }

    #[test]
    fn algorithm_rules() {
        let mut p = Parts::ok();
        p.outer = alg(OID_ECDSA_SHA384);
        assert_eq!(p.err(), Error::AlgorithmMismatch);
        // Same OID, different parameter encoding: bytes differ.
        p.inner = tlv(0x30, &cat(&[&tlv(6, OID_RSA_SHA256), &[5, 0]]));
        p.outer = tlv(0x30, &tlv(6, OID_RSA_SHA256));
        assert_eq!(p.err(), Error::Algorithm);
        // ECDSA must not carry NULL parameters (RFC 5758).
        let nulled = tlv(0x30, &cat(&[&tlv(6, OID_ECDSA_SHA256), &[5, 0]]));
        p.inner = nulled.clone();
        p.outer = nulled;
        assert_eq!(p.err(), Error::Algorithm);
        // RSA must carry NULL (RFC 4055). The ignored cross-sign form parses.
        let rsa = tlv(0x30, &cat(&[&tlv(6, OID_RSA_SHA256), &[5, 0]]));
        p.inner = rsa.clone();
        p.outer = rsa;
        assert_eq!(parse(&p.build()).unwrap().sig_alg, SigAlg::RsaPkcs1Sha256);
        // Unknown algorithm: parses as Unsupported with its OID; the parameters are kept opaque.
        let odd = tlv(0x30, &cat(&[&tlv(6, &[0x2a, 3, 4]), &tlv(0x30, &[5, 0])]));
        p.inner = odd.clone();
        p.outer = odd;
        assert_eq!(parse(&p.build()).unwrap().sig_alg, SigAlg::Unsupported(&[0x2a, 3, 4]));
        // Malformed: empty AlgorithmIdentifier, bad OID, trailing junk.
        for bad in [vec![0x30, 0], tlv(0x30, &tlv(6, &[0x2a, 0x86])), tlv(0x30, &cat(&[&tlv(6, OID_ECDSA_SHA256), &[5, 0, 5, 0]]))] {
            p.inner = bad.clone();
            p.outer = bad;
            assert!(p.parse().is_err());
        }
    }

    #[test]
    fn signature_bit_string_rules() {
        let b = Parts::ok().build();
        let n = b.len();
        // unused-bits octet is the 9th byte from the end (after the 8-byte signature).
        let mut m = b.clone();
        m[n - 9] = 1;
        assert_eq!(parse(&m).unwrap_err(), Error::Der(der::Error::BadBitString));
        let mut m = b.clone();
        m[n - 9] = 8;
        assert!(parse(&m).is_err());
    }

    #[test]
    fn rdn_set_order_is_canonical() {
        let a = tlv(0x30, &cat(&[&tlv(6, NAME_OID_CN), &tlv(0x0c, b"A")]));
        let b = tlv(0x30, &cat(&[&tlv(6, NAME_OID_CN), &tlv(0x0c, b"B")]));
        let mut p = Parts::ok();
        p.issuer = tlv(0x30, &tlv(0x31, &cat(&[&a, &b])));
        assert!(p.parse().is_ok());
        p.issuer = tlv(0x30, &tlv(0x31, &cat(&[&b, &a])));
        assert_eq!(p.err(), Error::Name);
    }

    #[test]
    fn name_rules() {
        let mut p = Parts::ok();
        p.issuer = vec![0x30, 0];
        assert_eq!(p.err(), Error::Name);
        p.issuer = name("CA");
        p.subject = vec![0x30, 0]; // empty subject is allowed
        assert!(p.parse().is_ok());
        for bad in [
            tlv(0x30, &[0x31, 0]),                                     // empty RDN
            tlv(0x30, &tlv(0x30, &[])),                                // RDN not a SET
            tlv(0x30, &tlv(0x31, &tlv(0x30, &tlv(6, NAME_OID_CN)))),   // attribute without value
            tlv(0x30, &tlv(0x31, &tlv(0x30, &cat(&[&tlv(2, &[1]), &tlv(0x0c, b"x")])))), // type not OID
            tlv(0x30, &tlv(0x31, &tlv(0x30, &cat(&[&tlv(6, NAME_OID_CN), &tlv(0x0c, b"x"), &[5, 0]])))), // extra
            tlv(0x31, &[]),
        ] {
            p.subject = bad;
            assert!(p.parse().is_err());
        }
    }

    /// An RSA SubjectPublicKeyInfo for `n` and `e` (big-endian, no sign octet added by the caller).
    fn rsa_spki(n: &[u8], e: &[u8], params: &[u8]) -> Vec<u8> {
        let int = |v: &[u8]| tlv(2, &if v[0] & 0x80 != 0 { cat(&[&[0], v]) } else { v.to_vec() });
        let key = tlv(0x30, &cat(&[&int(n), &int(e)]));
        let a = tlv(0x30, &cat(&[&tlv(6, OID_RSA_ENCRYPTION), params]));
        tlv(0x30, &cat(&[&a, &tlv(3, &cat(&[&[0], &key]))]))
    }

    #[test]
    fn rsa_keys_and_algorithms() {
        let mut n = vec![0xc5u8; 256];
        n[255] = 0xc7;
        let mut p = Parts::ok();
        p.spki = rsa_spki(&n, &[1, 0, 1], &[5, 0]);
        match parse(&p.build()).unwrap().public_key {
            PublicKey::Rsa(k) => assert_eq!((k.modulus(), k.exponent(), k.bits()), (&n[..], 65537, 2048)),
            other => panic!("{other:?}"),
        }
        // NULL parameters are required (RFC 3279 2.3.1).
        for params in [&[][..], &[5, 1, 0], &[6, 1, 0x2a], &[0x30, 0]] {
            p.spki = rsa_spki(&n, &[1, 0, 1], params);
            assert_eq!(p.err(), Error::PublicKey);
        }
        // Key policy: 1024 bits, 4097 bits, even n, e of 33 bits, even e, e = 1.
        let mut big = vec![0xc5u8; 513];
        big[0] = 1;
        big[512] = 7;
        for (n, e) in [(&n[..128], &[1u8, 0, 1][..]), (&big[..], &[1, 0, 1]), (&n[..255], &[1, 0, 1]), (&n[..], &[1, 0, 0, 0, 1]), (&n[..], &[1, 0, 0]), (&n[..], &[1])] {
            p.spki = rsa_spki(n, e, &[5, 0]);
            assert_eq!(p.err(), Error::PublicKey, "{} {e:?}", n.len());
        }
        let mut even = n.clone();
        even[255] = 0xc6;
        p.spki = rsa_spki(&even, &[1, 0, 1], &[5, 0]);
        assert_eq!(p.err(), Error::PublicKey);
        let mut max = big.clone();
        max.truncate(512);
        max[0] = 0xc5;
        p.spki = rsa_spki(&max, &[0xff, 0xff, 0xff, 0xff], &[5, 0]);
        assert!(matches!(parse(&p.build()).unwrap().public_key, PublicKey::Rsa(_)), "4096 bits and e = 2^32 - 1");
        // Signature algorithms: sha384WithRSAEncryption needs NULL too.
        let rsa384 = tlv(0x30, &cat(&[&tlv(6, OID_RSA_SHA384), &[5, 0]]));
        p.inner = rsa384.clone();
        p.outer = rsa384;
        assert_eq!(parse(&p.build()).unwrap().sig_alg, SigAlg::RsaPkcs1Sha384);
        let bare = alg(OID_RSA_SHA384);
        p.inner = bare.clone();
        p.outer = bare;
        assert_eq!(p.err(), Error::Algorithm);
    }

    #[test]
    fn validity_rules() {
        let mut p = Parts::ok();
        let t = |s: &str| tlv(der::UTC_TIME, s.as_bytes());
        p.validity = tlv(0x30, &t("260101000000Z")); // one time only
        assert!(p.parse().is_err());
        p.validity = tlv(0x30, &cat(&[&t("260101000000Z"), &t("270101000000Z"), &t("270101000000Z")]));
        assert!(p.parse().is_err());
        p.validity = tlv(0x30, &cat(&[&t("260101000000Z"), &t("270230000000Z")]));
        assert_eq!(p.err(), Error::Time);
        // Not-before after not-after parses; valid_at is then never true.
        p.validity = tlv(0x30, &cat(&[&t("270101000000Z"), &t("260101000000Z")]));
        let b = p.build();
        let c = parse(&b).unwrap();
        assert!(!c.valid_at(c.not_before.0) && !c.valid_at(c.not_after.0));
    }

    #[test]
    fn public_key_rules() {
        let mut p = Parts::ok();
        let mut compressed = vec![2];
        compressed.extend([7; 32]);
        for bad in [
            ec_spki(OID_P256, &compressed),
            ec_spki(OID_P256, &p256_point()[..64]),
            ec_spki(OID_P256, &[p256_point(), vec![0]].concat()),
            ec_spki(OID_P384, &p256_point()), // curve and point length disagree
            ec_spki(OID_P256, &[vec![5], vec![0; 64]].concat()),
        ] {
            p.spki = bad;
            assert_eq!(p.err(), Error::PublicKey);
        }
        // Unused bits in the key.
        let mut s = ec_spki(OID_P256, &p256_point());
        let n = s.len();
        s[n - 66] = 1;
        p.spki = s;
        assert!(p.parse().is_err());
        // EC key without parameters: error. Other curve, specified curve, RSA: Unsupported.
        let noparams = tlv(0x30, &cat(&[&tlv(0x30, &tlv(6, OID_EC_PUBLIC_KEY)), &tlv(3, &[0, 4])]));
        p.spki = noparams;
        assert_eq!(p.err(), Error::PublicKey);
        let k1 = [0x2b, 0x81, 0x04, 0x00, 0x0a];
        p.spki = ec_spki(&k1, &p256_point());
        assert!(matches!(parse(&p.build()).unwrap().public_key, PublicKey::Unsupported { .. }));
        let rsa_oid = [0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01];
        let rsa = tlv(0x30, &cat(&[&tlv(0x30, &cat(&[&tlv(6, &rsa_oid), &[5, 0]])), &tlv(3, &[0, 0x30, 0])]));
        p.spki = rsa;
        assert_eq!(p.err(), Error::PublicKey, "an empty RSAPublicKey is an error, not Unsupported");
        // Wrong structure.
        p.spki = tlv(0x30, &tlv(3, &[0]));
        assert!(p.parse().is_err());
        p.spki = tlv(0x30, &[]);
        assert!(p.parse().is_err());
    }

    #[test]
    fn extension_rules() {
        let skv = tlv(4, &[1, 2, 3]);
        let c = parse_with(&[ext(OID_SKI, false, &skv)]).unwrap();
        let c = parse(&c).unwrap();
        assert_eq!(c.extensions.subject_key_id, Some(&[1u8, 2, 3][..]));
        assert_eq!(c.extensions.count, 1);
        // Duplicates, including non-adjacent and unknown ones, and critical/non-critical mixes.
        let unk = [0x2a, 9, 9];
        assert_eq!(ext_err(&[ext(OID_SKI, false, &skv), ext(OID_SKI, false, &skv)]), Error::Extension);
        assert_eq!(
            ext_err(&[ext(OID_SKI, false, &skv), ext(&unk, false, &[5, 0]), ext(OID_SKI, true, &skv)]),
            Error::Extension
        );
        assert_eq!(ext_err(&[ext(&unk, false, &[5, 0]), ext(&unk, true, &[0xff])]), Error::Extension);
        // Unknown critical is recorded, unknown non-critical is not; contents are not looked at.
        let c = parse_with(&[ext(&unk, true, &[0xde, 0xad])]).unwrap();
        assert_eq!(parse(&c).unwrap().extensions.unhandled_critical, Some(&unk[..]));
        let c = parse_with(&[ext(&unk, false, &[0xde, 0xad])]).unwrap();
        assert_eq!(parse(&c).unwrap().extensions.unhandled_critical, None);
        let c = parse_with(&[ext(&unk, true, &[]), ext(&[0x2a, 9, 8], true, &[])]).unwrap();
        assert_eq!(parse(&c).unwrap().extensions.unhandled_critical, Some(&unk[..]));
        // A critical flag on a known extension is kept.
        let c = parse_with(&[ext(OID_BASIC_CONSTRAINTS, true, &[0x30, 0])]).unwrap();
        let bc = parse(&c).unwrap().extensions.basic_constraints.unwrap();
        assert_eq!(bc, BasicConstraints { critical: true, ca: false, path_len: None });
        // Encoding errors: explicit FALSE, BOOLEAN 0x01, missing value, wrong order, junk, bad OID.
        let o = tlv(6, OID_SKI);
        for bad in [
            tlv(0x30, &cat(&[&o, &[1, 1, 0], &tlv(4, &skv)])),
            tlv(0x30, &cat(&[&o, &[1, 1, 1], &tlv(4, &skv)])),
            tlv(0x30, &cat(&[&o, &[1, 2, 0xff, 0xff], &tlv(4, &skv)])),
            tlv(0x30, &o),
            tlv(0x30, &cat(&[&tlv(4, &skv), &o])),
            tlv(0x30, &cat(&[&o, &tlv(4, &skv), &[5, 0]])),
            tlv(0x30, &cat(&[&tlv(6, &[0x55, 0x80, 1]), &tlv(4, &skv)])),
            tlv(0x30, &cat(&[&o, &[1, 1, 0xff, 1, 1, 0xff], &tlv(4, &skv)])),
            tlv(0x31, &cat(&[&o, &tlv(4, &skv)])),
        ] {
            let mut p = Parts::ok();
            p.tail = tlv(0xa3, &tlv(0x30, &bad));
            assert!(p.parse().is_err(), "{bad:02x?}");
        }
        // Empty extension list, wrong wrapper, trailing bytes after the list.
        let mut p = Parts::ok();
        p.tail = tlv(0xa3, &tlv(0x30, &[]));
        assert_eq!(p.err(), Error::Extension);
        p.tail = tlv(0xa3, &[]);
        assert!(p.parse().is_err());
        p.tail = tlv(0xa3, &cat(&[&tlv(0x30, &ext(OID_SKI, false, &skv)), &[5, 0]]));
        assert!(p.parse().is_err());
        p.tail = cat(&[&exts(&[ext(OID_SKI, false, &skv)]), &[5, 0]]);
        assert!(p.parse().is_err());
        // Count limit: 64 distinct OIDs are fine, 65 are not.
        let many = |n: usize| -> Vec<Vec<u8>> { (0..n).map(|i| ext(&[0x2a, 3, i as u8], false, &[5, 0])).collect() };
        let c = parse_with(&many(MAX_EXTENSIONS)).unwrap();
        assert_eq!(parse(&c).unwrap().extensions.count, MAX_EXTENSIONS);
        assert_eq!(ext_err(&many(MAX_EXTENSIONS + 1)), Error::TooManyExtensions);
    }

    #[test]
    fn basic_constraints_rules() {
        let bc = |v: &[u8]| ext(OID_BASIC_CONSTRAINTS, true, &tlv(0x30, v));
        let get = |v: &[u8]| {
            let b = parse_with(&[bc(v)])?;
            Ok::<_, Error>(parse(&b).unwrap().extensions.basic_constraints.unwrap())
        };
        assert_eq!(get(&[]).unwrap(), BasicConstraints { critical: true, ca: false, path_len: None });
        assert!(get(&[1, 1, 0xff]).unwrap().ca);
        assert_eq!(get(&[1, 1, 0xff, 2, 1, 0]).unwrap().path_len, Some(0));
        assert_eq!(get(&[1, 1, 0xff, 2, 1, 5]).unwrap().path_len, Some(5));
        assert_eq!(get(&[1, 1, 0xff, 2, 4, 0x7f, 0xff, 0xff, 0xff]).unwrap().path_len, Some(0x7fff_ffff));
        assert_eq!(get(&[1, 1, 0xff, 2, 5, 0, 0xff, 0xff, 0xff, 0xff]).unwrap().path_len, Some(u32::MAX));
        assert!(get(&[1, 1, 0xff, 2, 5, 1, 0, 0, 0, 0]).is_err()); // > u32
        assert!(get(&[1, 1, 0xff, 2, 1, 0x80]).is_err()); // negative
        assert!(get(&[1, 1, 0xff, 2, 2, 0, 1]).is_err()); // padded
        assert!(get(&[1, 1, 0xff, 2, 0]).is_err()); // empty integer
        assert!(get(&[1, 1, 0]).is_err()); // explicit DEFAULT
        assert!(get(&[2, 1, 1]).is_err()); // pathLen without cA
        assert!(get(&[1, 1, 1]).is_err()); // BOOLEAN not 0xff
        assert!(get(&[2, 1, 1, 1, 1, 0xff]).is_err()); // wrong order
        assert!(get(&[1, 1, 0xff, 5, 0]).is_err()); // extra element
        // Not a SEQUENCE, or bytes after it.
        assert!(parse_with(&[ext(OID_BASIC_CONSTRAINTS, true, &[5, 0])]).is_err());
        assert!(parse_with(&[ext(OID_BASIC_CONSTRAINTS, true, &[0x30, 0, 0])]).is_err());
        assert!(parse_with(&[ext(OID_BASIC_CONSTRAINTS, true, &[])]).is_err());
    }

    #[test]
    fn key_usage_rules() {
        let ku = |unused: u8, bytes: &[u8]| {
            let v = tlv(3, &[&[unused][..], bytes].concat());
            parse_with(&[ext(OID_KEY_USAGE, true, &v)]).map(|b| parse(&b).unwrap().extensions.key_usage.unwrap().bits)
        };
        assert_eq!(ku(7, &[0x80]), Ok(KU_DIGITAL_SIGNATURE));
        assert_eq!(ku(1, &[0x86]), Ok(KU_DIGITAL_SIGNATURE | KU_KEY_CERT_SIGN | KU_CRL_SIGN));
        assert_eq!(ku(2, &[0x84]), Ok(KU_DIGITAL_SIGNATURE | KU_KEY_CERT_SIGN)); // 0x84 = bits 0 and 5
        assert_eq!(ku(7, &[0x00, 0x80]), Ok(KU_DECIPHER_ONLY));
        assert_eq!(ku(7, &[0x01, 0x80]), Ok(KU_ENCIPHER_ONLY | KU_DECIPHER_ONLY));
        // Not canonical: trailing zero bits encoded, all-zero value, no data, wrong unused count.
        assert!(ku(0, &[0x80]).is_err());
        assert!(ku(6, &[0x80]).is_err());
        assert!(ku(7, &[0x00]).is_err());
        assert!(ku(0, &[]).is_err());
        assert!(ku(7, &[0x80, 0x00]).is_err());
        assert!(ku(0, &[0x80, 0x01]).is_err()); // bit 15 is undefined
        assert!(ku(0, &[0x80, 0x01, 0x01]).is_err());
        assert!(ku(8, &[0x80]).is_err());
        // Not a BIT STRING.
        assert!(parse_with(&[ext(OID_KEY_USAGE, true, &[4, 1, 0x80])]).is_err());
    }

    #[test]
    fn ext_key_usage_rules() {
        let eku = |oids: &[&[u8]]| {
            let body: Vec<u8> = oids.iter().flat_map(|o| tlv(6, o)).collect();
            parse_with(&[ext(OID_EKU, false, &tlv(0x30, &body))]).map(|b| parse(&b).unwrap().extensions.ext_key_usage.unwrap())
        };
        let u = eku(&[OID_EKU_SERVER_AUTH]).unwrap();
        assert_eq!((u.server_auth, u.any, u.other, u.critical), (true, false, false, false));
        let u = eku(&[OID_EKU_ANY, &[0x2a, 1]]).unwrap();
        assert_eq!((u.server_auth, u.any, u.other), (false, true, true));
        assert!(eku(&[]).is_err());
        assert!(eku(&[&[0x2a, 0x80, 1]]).is_err());
        assert!(parse_with(&[ext(OID_EKU, false, &tlv(0x30, &[5, 0]))]).is_err());
    }

    #[test]
    fn san_rules() {
        let san = |names: &[Vec<u8>]| {
            let body: Vec<u8> = names.concat();
            parse_with(&[ext(OID_SAN, false, &tlv(0x30, &body))])
        };
        let b = san(&[tlv(0x82, b"a.example"), tlv(0x87, &[10, 0, 0, 1]), tlv(0x87, &[0; 16]), tlv(0x86, b"http://x"), tlv(0x81, b"a@b"), tlv(0xa4, &[0x30, 0]), tlv(0x88, &[0x2a, 3])]).unwrap();
        let c = parse(&b).unwrap();
        let n: Vec<_> = c.extensions.san.unwrap().names().collect();
        assert_eq!(n.len(), 7);
        assert_eq!(n[0], GeneralName::Dns(b"a.example"));
        assert_eq!(n[1], GeneralName::Ip(&[10, 0, 0, 1]));
        assert_eq!(n[3], GeneralName::Other(0x86));
        assert!(san(&[tlv(0x82, b"*.example.com")]).is_ok());
        for bad in [
            vec![],                                    // SIZE (1..)
            vec![tlv(0x82, b"")],                      // empty DNS
            vec![tlv(0x82, b"a\0b")],                  // NUL
            vec![tlv(0x82, b"a b")],                   // space
            vec![tlv(0x82, "é".as_bytes())],           // non-ASCII
            vec![tlv(0x87, &[1, 2, 3, 4, 5])],         // bad IP length
            vec![tlv(0x87, &[])],
            vec![tlv(0x87, &[0; 8])],                  // that is a name-constraint form, not a SAN
            vec![tlv(0x83, b"x")],                     // unknown choice
            vec![tlv(0x86, &[0xff])],                  // non-ASCII URI
            vec![tlv(0x88, &[0x2a, 0x80])],            // bad registered ID
            vec![tlv(0x82, b"ok"), vec![0x82, 5, 1]],  // truncated
            vec![tlv(0x02, &[1])],                     // not a context tag
        ] {
            assert!(san(&bad).is_err(), "{bad:02x?}");
        }
        assert!(parse_with(&[ext(OID_SAN, false, &[5, 0])]).is_err());
        // Critical flag is kept.
        let b = parse_with(&[ext(OID_SAN, true, &tlv(0x30, &tlv(0x82, b"a")))]).unwrap();
        assert!(parse(&b).unwrap().extensions.san.unwrap().critical);
    }

    #[test]
    fn key_id_rules() {
        let aki = |body: &[u8]| parse_with(&[ext(OID_AKI, false, &tlv(0x30, body))]);
        fn id(b: &[u8]) -> Option<&[u8]> {
            parse(b).unwrap().extensions.authority_key_id.unwrap().key_id
        }
        let b = aki(&tlv(0x80, &[9, 9])).unwrap();
        assert_eq!(id(&b), Some(&[9u8, 9][..]));
        let b = aki(&[]).unwrap();
        assert_eq!(id(&b), None);
        let full = cat(&[&tlv(0x80, &[1]), &tlv(0xa1, &tlv(0x82, b"x")), &tlv(0x82, &[5])]);
        assert!(aki(&full).is_ok());
        assert!(aki(&cat(&[&tlv(0xa1, &tlv(0x82, b"x"))])).is_err()); // issuer without serial
        assert!(aki(&cat(&[&tlv(0x82, &[5])])).is_err()); // serial without issuer
        assert!(aki(&tlv(0x80, &[])).is_err());
        assert!(aki(&cat(&[&tlv(0x82, &[5]), &tlv(0x80, &[1])])).is_err()); // wrong order
        assert!(aki(&cat(&[&tlv(0xa1, &[]), &tlv(0x82, &[5])])).is_err());
        assert!(aki(&cat(&[&tlv(0xa1, &tlv(0x82, b"x")), &tlv(0x82, &[0, 0])])).is_err());
        assert!(aki(&[5, 0]).is_err());
        assert!(parse_with(&[ext(OID_AKI, false, &[0x04, 1, 1])]).is_err());
        assert!(parse_with(&[ext(OID_SKI, false, &tlv(4, &[]))]).is_err());
        assert!(parse_with(&[ext(OID_SKI, false, &[5, 0])]).is_err());
        assert!(parse_with(&[ext(OID_SKI, false, &cat(&[&tlv(4, &[1]), &[0]]))]).is_err());
    }

    #[test]
    fn top_level_rules() {
        let b = Parts::ok().build();
        assert!(parse(&b).is_ok());
        let mut t = b.clone();
        t.push(0);
        assert_eq!(parse(&t).unwrap_err(), Error::Der(der::Error::TrailingData));
        assert!(parse(&[]).is_err());
        assert!(parse(&[0x30, 0x80, 0, 0]).is_err()); // indefinite length
        assert!(parse(&[0x31, 0]).is_err());
        // Extra element inside the certificate.
        let mut x = b.clone();
        x.extend([5, 0]);
        let n = x.len() - 2;
        x[2] = (n >> 8) as u8;
        x[3] = n as u8;
        assert!(parse(&x).is_err());
        // Non-minimal outer length (0x82 form with a value that fits in 0x81).
        assert_eq!(b[1], 0x81);
        // A certificate over the size limit.
        assert_eq!(parse(&vec![0x30; MAX_CERT_LEN + 1]).unwrap_err(), Error::TooLarge);
    }

    #[test]
    fn non_minimal_certificate_length() {
        let b = Parts::ok().build();
        let (hdr, body) = if b[1] == 0x81 { (3, &b[3..]) } else { (2, &b[2..]) };
        let _ = hdr;
        let mut w = vec![0x30, 0x82, 0, body.len() as u8];
        w.extend(body);
        assert!(parse(&b).is_ok());
        assert_eq!(parse(&w).unwrap_err(), Error::Der(der::Error::NonMinimalLength));
    }

    // ---- ECDSA signature values ------------------------------------------------------------

    fn sig(r: &[u8], s: &[u8]) -> Vec<u8> {
        tlv(0x30, &cat(&[&tlv(2, r), &tlv(2, s)]))
    }

    #[test]
    fn ecdsa_signature_rules() {
        let d = sig(&[1], &[2]);
        let ok = EcdsaSig::parse(&d, Curve::P256).unwrap();
        assert_eq!((ok.r, ok.s), (&[1u8][..], &[2u8][..]));
        let hi = [vec![0], vec![0x80], vec![0x11; 31]].concat(); // 33 content octets, 32 magnitude
        let d = sig(&hi, &[0, 0xff]);
        let ok = EcdsaSig::parse(&d, Curve::P256).unwrap();
        assert_eq!(ok.r.len(), 32);
        assert_eq!(ok.s, &[0xff]);
        let big = [vec![0], vec![0x80], vec![0x11; 47]].concat(); // 48-octet magnitude
        assert!(EcdsaSig::parse(&sig(&big, &[1]), Curve::P384).is_ok());
        assert!(EcdsaSig::parse(&sig(&big, &[1]), Curve::P256).is_err()); // too long for P-256
        let too_big = [vec![0], vec![0x80], vec![0x11; 48]].concat(); // 49-octet magnitude
        assert!(EcdsaSig::parse(&sig(&too_big, &[1]), Curve::P384).is_err());
        for (r, s) in [
            (&[0u8][..], &[1u8][..]),        // r == 0
            (&[1], &[0]),                    // s == 0
            (&[0x80], &[1]),                 // negative r
            (&[1], &[0xff]),                 // negative s
            (&[0, 1], &[1]),                 // padded r
            (&[1], &[0, 0x7f]),              // padded s
            (&[], &[1]),                     // empty
            (&[0xff, 0x80], &[1]),           // negative padded
        ] {
            assert!(EcdsaSig::parse(&sig(r, s), Curve::P256).is_err(), "{r:02x?} {s:02x?}");
        }
        let good = sig(&[1], &[2]);
        let mut t = good.clone();
        t.push(0);
        assert!(EcdsaSig::parse(&t, Curve::P256).is_err());
        assert!(EcdsaSig::parse(&tlv(0x30, &tlv(2, &[1])), Curve::P256).is_err()); // no s
        assert!(EcdsaSig::parse(&tlv(0x30, &cat(&[&tlv(2, &[1]), &tlv(2, &[2]), &tlv(2, &[3])])), Curve::P256).is_err());
        assert!(EcdsaSig::parse(&tlv(0x31, &cat(&[&tlv(2, &[1]), &tlv(2, &[2])])), Curve::P256).is_err());
        assert!(EcdsaSig::parse(&[0x30, 0x80, 2, 1, 1, 2, 1, 1, 0, 0], Curve::P256).is_err());
        assert!(EcdsaSig::parse(&[0x30, 0x81, 6, 2, 1, 1, 2, 1, 1], Curve::P256).is_err()); // non-minimal length
        assert!(EcdsaSig::parse(&[], Curve::P256).is_err());
        for n in 0..good.len() {
            assert!(EcdsaSig::parse(&good[..n], Curve::P256).is_err());
        }
    }

    // ---- truncation and mutation -----------------------------------------------------------

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
    }

    fn inside(outer: &[u8], inner: &[u8]) -> bool {
        let (o, i) = (outer.as_ptr() as usize, inner.as_ptr() as usize);
        i >= o && i + inner.len() <= o + outer.len()
    }

    /// Every borrowed slice of a parsed certificate must lie inside the input.
    fn check_borrows(der: &[u8], c: &Certificate<'_>) {
        for s in [c.der, c.tbs, c.serial, c.signature, c.issuer, c.subject, c.spki] {
            assert!(inside(der, s));
        }
        if let PublicKey::Ec { point, .. } = c.public_key {
            assert!(inside(der, point));
        }
        for e in c.extensions.iter() {
            assert!(inside(der, e.oid) && inside(der, e.value));
        }
        if let Some(san) = c.extensions.san {
            for n in san.names() {
                if let GeneralName::Dns(d) | GeneralName::Ip(d) = n {
                    assert!(inside(der, d));
                }
            }
        }
    }

    #[test]
    fn truncation_at_every_byte() {
        for (name, der) in FIX {
            for n in 0..der.len() {
                assert!(parse(&der[..n]).is_err(), "{name}: prefix {n} parsed");
            }
            for extra in 1..4 {
                let mut v = der.to_vec();
                v.extend(vec![0; extra]);
                assert_eq!(parse(&v).unwrap_err(), Error::Der(der::Error::TrailingData), "{name}");
            }
        }
        // Same for a synthetic certificate with many extensions.
        let b = Parts::ok().with_ext(&[ext(OID_SKI, false, &tlv(4, &[1])), ext(OID_SAN, false, &tlv(0x30, &tlv(0x82, b"a")))]).build();
        for n in 0..b.len() {
            assert!(parse(&b[..n]).is_err());
        }
    }

    #[test]
    fn mutated_input_never_panics() {
        let mut rng = Rng(0x9e3779b97f4a7c15);
        let mut ok = 0u32;
        let mut bad = 0u32;
        for (_, der) in FIX {
            // Every byte: flip all bits, flip one bit, zero, 0xff.
            for i in 0..der.len() {
                for m in [0xffu8, 0x01, 0x80] {
                    let mut v = der.to_vec();
                    v[i] ^= m;
                    match parse(&v) {
                        Ok(c) => {
                            ok += 1;
                            check_borrows(&v, &c)
                        }
                        Err(_) => bad += 1,
                    }
                }
                for val in [0u8, 0xff] {
                    let mut v = der.to_vec();
                    v[i] = val;
                    if let Ok(c) = parse(&v) {
                        check_borrows(&v, &c);
                    }
                }
            }
            // Random multi-byte mutations and random cut-and-splice.
            for _ in 0..3000 {
                let mut v = der.to_vec();
                for _ in 0..1 + rng.next() % 4 {
                    let i = (rng.next() % v.len() as u64) as usize;
                    v[i] = rng.next() as u8;
                }
                if rng.next().is_multiple_of(4) {
                    let cut = (rng.next() % v.len() as u64) as usize;
                    v.drain(cut..cut + (rng.next() % 8) as usize % (v.len() - cut));
                }
                if let Ok(c) = parse(&v) {
                    check_borrows(&v, &c);
                }
            }
        }
        // Mutated bytes in a signature value or key can still parse; most structure bytes cannot.
        assert!(bad > 0 && ok > 0, "ok={ok} bad={bad}");
        // Random bytes.
        for _ in 0..20000 {
            let n = (rng.next() % 64) as usize;
            let mut v: Vec<u8> = (0..n).map(|_| rng.next() as u8).collect();
            if n > 2 && rng.next().is_multiple_of(2) {
                v[0] = 0x30;
            }
            assert!(parse(&v).is_err());
        }
    }

    /// A single changed byte inside a length field must be caught as truncated, trailing or non-minimal.
    #[test]
    fn length_byte_mutations_fail() {
        let b = Parts::ok().build();
        for delta in [1i16, -1, 100, -100] {
            let mut v = b.clone();
            let l = v[2] as i16 + delta;
            if !(0..256).contains(&l) {
                continue;
            }
            v[2] = l as u8; // the outer 0x81 length octet (or the outer length high byte)
            assert!(parse(&v).is_err());
        }
    }
}
