//! Server certificate chain validation and the TLS 1.3 server `CertificateVerify` check.
//!
//! Inputs: the peer's DER chain, an independently loaded [`TrustStore`], the host the user
//! asked for and a caller-supplied Unix time. Output: a [`VerifiedPeer`] that holds the
//! leaf key for [`verify_certificate_verify`]. There is no "insecure" mode and no callback.
//!
//! What is checked (RFC 5280 section 6, RFC 9525, RFC 8446 section 4.4):
//! - Trust comes ONLY from the store. A certificate sent by the peer is never an anchor,
//!   even when it is byte-equal to one: the path always ends at the store's copy.
//! - Path: leaf, then 0..=[`MAX_INTERMEDIATES`] peer certificates, then an anchor. The
//!   next certificate must be a LATER entry of the peer list (reordered chains fail, unused
//!   extra entries are skipped and never parsed unless needed). Linking needs the issuer's
//!   subject to be BYTE-EQUAL to the subject's issuer (a conservative limit: RFC 5280 asks
//!   for name normalisation) and, when both are present, AKI keyIdentifier = SKI. A name
//!   match alone never makes trust: every used link's signature is verified.
//! - Every certificate on the path (anchor included) must be valid at `now`.
//! - Issuers: basicConstraints cA, pathLenConstraint, keyUsage keyCertSign if present, EKU
//!   serverAuth or anyExtendedKeyUsage if present, version 3. Leaf: not a CA, keyUsage
//!   digitalSignature if present, EKU serverAuth if present (anyEKU is refused), and a SAN
//!   that matches the host ([`crate::hostname`]: SAN only, no CN, no wildcard).
//! - Unknown critical extensions fail. nameConstraints, policyMappings, policyConstraints
//!   and inhibitAnyPolicy are not implemented, so a certificate that carries one FAILS,
//!   critical or not. A critical SAN with any name that is not DNS or IP fails.
//! - Signatures: ecdsa-with-SHA256 or SHA384 (the curve is the ISSUER key's curve), and
//!   sha256WithRSAEncryption or sha384WithRSAEncryption (RSASSA-PKCS1-v1_5, issuer key 2048
//!   to 4096 bits, see [`crate::rsa`]). The hash is the one the certificate's own algorithm
//!   names. An EC issuer cannot sign with an RSA algorithm, and the reverse.
//!
//! NOT done: revocation (CRL, OCSP, stapling), certificate transparency, policy
//! processing, name normalisation, RSA-PSS or SHA-512 certificate signatures, Ed25519, trust-anchor constraints other than the
//! anchor certificate's own extensions, a leaf that is itself an anchor. An unparsable
//! UNUSED peer certificate is ignored; an unparsable leaf or anchor-store entry is not used.
//!
//! Work is bounded: at most [`MAX_CHAIN_CERTS`] certificates and [`MAX_CHAIN_BYTES`] bytes,
//! [`MAX_SIG_CHECKS`] signature verifications, recursion depth at most
//! [`MAX_INTERMEDIATES`]. The code does not allocate.

use crate::der::{self, Reader};
use crate::ecc;
use crate::hello;
use crate::hostname::{self, SanName};
use crate::rsa;
use crate::sha256::sha256;
use crate::sha384::sha384;
use crate::trust_store::TrustStore;
use crate::x509::{self, Certificate, EcdsaSig, GeneralName, PublicKey, SigAlg};

pub const MAX_CHAIN_CERTS: usize = 8;
pub const MAX_CHAIN_BYTES: usize = 64 * 1024;
pub const MAX_INTERMEDIATES: usize = 4;
pub const MAX_SIG_CHECKS: usize = 8;

/// ecdsa_secp256r1_sha256, a CertificateVerify scheme the ClientHello offers (P-256 leaf).
/// The others are `hello::RSA_PSS_RSAE_SHA256` and `hello::RSA_PSS_RSAE_SHA384` (RSA leaf).
pub const SIG_ECDSA_SECP256R1_SHA256: u16 = 0x0403;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    EmptyChain,
    TooManyCerts,
    ChainTooLarge,
    /// The leaf did not parse.
    Parse(x509::Error),
    Host(hostname::Error),
    NotYetValid,
    Expired,
    /// A critical extension this crate does not implement.
    UnhandledCritical,
    /// nameConstraints, policyMappings, policyConstraints or inhibitAnyPolicy is present.
    UnsupportedConstraint,
    /// A critical SAN holds a name type this crate does not interpret.
    BadSan,
    LeafIsCa,
    LeafKeyUsage,
    LeafEku,
    NotCa,
    CaKeyUsage,
    CaEku,
    PathLen,
    /// Not an EC P-256/P-384 key or an RSA key, or not on the curve.
    BadKey,
    /// The signature algorithm, or the issuer key type, cannot be verified here.
    UnsupportedAlgorithm,
    BadSignature,
    /// No anchor or later peer certificate issued the certificate.
    NoIssuer,
    TooManyChecks,
    /// The CertificateVerify scheme was not offered.
    UnsupportedScheme,
    /// The scheme needs another key than the leaf's: another curve, or RSA instead of EC.
    SchemeKeyMismatch,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "certificate verification failed: {self:?}")
    }
}
impl std::error::Error for Error {}

/// A chain that passed [`verify_server_chain`]. Only that function builds one.
#[derive(Debug, Clone, Copy)]
pub struct VerifiedPeer<'a> {
    leaf: Certificate<'a>,
    /// `PublicKey::Ec`, checked to be on the curve, or `PublicKey::Rsa`.
    key: PublicKey<'a>,
    path_certs: usize,
}

impl<'a> VerifiedPeer<'a> {
    pub fn leaf(&self) -> &Certificate<'a> {
        &self.leaf
    }
    /// The leaf key: an EC curve and `04 || X || Y` (checked to be on the curve), or RSA.
    pub fn leaf_key(&self) -> PublicKey<'a> {
        self.key
    }
    /// Certificates on the path, leaf and anchor included.
    pub fn path_certs(&self) -> usize {
        self.path_certs
    }
}

const OID_NAME_CONSTRAINTS: &[u8] = &[0x55, 0x1d, 0x1e];
const OID_POLICY_MAPPINGS: &[u8] = &[0x55, 0x1d, 0x21];
const OID_POLICY_CONSTRAINTS: &[u8] = &[0x55, 0x1d, 0x24];
const OID_INHIBIT_ANY_POLICY: &[u8] = &[0x55, 0x1d, 0x36];

fn ecc_curve(c: x509::Curve) -> ecc::Curve {
    match c {
        x509::Curve::P256 => ecc::Curve::P256,
        x509::Curve::P384 => ecc::Curve::P384,
    }
}

/// Checks for every certificate on the path: validity, unsupported or critical extensions.
fn common(c: &Certificate<'_>, now: i64) -> Result<(), Error> {
    if now < c.not_before.0 {
        return Err(Error::NotYetValid);
    }
    if now > c.not_after.0 {
        return Err(Error::Expired);
    }
    let x = &c.extensions;
    if x.iter().any(|e| {
        matches!(e.oid, OID_NAME_CONSTRAINTS | OID_POLICY_MAPPINGS | OID_POLICY_CONSTRAINTS | OID_INHIBIT_ANY_POLICY)
    }) {
        return Err(Error::UnsupportedConstraint);
    }
    if x.unhandled_critical.is_some() {
        return Err(Error::UnhandledCritical);
    }
    if let Some(s) = x.san
        && s.critical
        && s.names().any(|n| matches!(n, GeneralName::Other(_)))
    {
        return Err(Error::BadSan);
    }
    Ok(())
}

fn leaf_checks(c: &Certificate<'_>) -> Result<(), Error> {
    let x = &c.extensions;
    if x.basic_constraints.is_some_and(|b| b.ca) {
        return Err(Error::LeafIsCa);
    }
    if x.key_usage.is_some_and(|k| k.bits & x509::KU_DIGITAL_SIGNATURE == 0) {
        return Err(Error::LeafKeyUsage);
    }
    if x.ext_key_usage.is_some_and(|e| !e.server_auth) {
        return Err(Error::LeafEku);
    }
    Ok(())
}

/// `depth` is the number of intermediates between the leaf and `c`.
fn ca_checks(c: &Certificate<'_>, depth: usize) -> Result<(), Error> {
    let x = &c.extensions;
    let bc = x.basic_constraints.filter(|b| b.ca && c.version == 3).ok_or(Error::NotCa)?;
    if bc.path_len.is_some_and(|l| depth as u64 > l as u64) {
        return Err(Error::PathLen);
    }
    if x.key_usage.is_some_and(|k| k.bits & x509::KU_KEY_CERT_SIGN == 0) {
        return Err(Error::CaKeyUsage);
    }
    if x.ext_key_usage.is_some_and(|e| !e.server_auth && !e.any) {
        return Err(Error::CaEku);
    }
    Ok(())
}

/// Checked only for a key that is to verify a CertificateVerify: key encoding, curve, point.
/// The generic verify checks the key before the signature range, and an all-zero signature
/// fails the range check. So this costs a point check, not a scalar multiplication.
fn check_key(curve: x509::Curve, point: &[u8]) -> Result<(), Error> {
    let w = curve.field_len();
    match ecc::verify(ecc_curve(curve), point, &[0u8; 32], &[0u8; 96][..2 * w]) {
        Err(ecc::Error::BadSignature) => Ok(()),
        _ => Err(Error::BadKey),
    }
}

/// DER ECDSA-Sig-Value -> fixed-width `r || s` (left-padded) for the generic verifier.
fn verify_ecdsa_der(curve: x509::Curve, point: &[u8], digest: &[u8], sig_der: &[u8]) -> Result<(), Error> {
    let sig = EcdsaSig::parse(sig_der, curve).map_err(|_| Error::BadSignature)?;
    let w = curve.field_len();
    let mut rs = [0u8; 96];
    rs[w - sig.r.len()..w].copy_from_slice(sig.r);
    rs[2 * w - sig.s.len()..2 * w].copy_from_slice(sig.s);
    match ecc::verify(ecc_curve(curve), point, digest, &rs[..2 * w]) {
        Ok(()) => Ok(()),
        Err(ecc::Error::BadPublicKey) => Err(Error::BadKey),
        Err(_) => Err(Error::BadSignature),
    }
}

/// Is `c`'s signature made by `issuer`'s key, with the hash that `c`'s algorithm names?
fn check_sig(c: &Certificate<'_>, issuer: &Certificate<'_>) -> Result<(), Error> {
    let rsa = |k: rsa::PublicKey<'_>, hash: rsa::Hash, digest: &[u8]| {
        k.verify_pkcs1(hash, digest, c.signature).map_err(|_| Error::BadSignature)
    };
    match (issuer.public_key, c.sig_alg) {
        (PublicKey::Ec { curve, point }, SigAlg::EcdsaSha256) => verify_ecdsa_der(curve, point, &sha256(c.tbs), c.signature),
        (PublicKey::Ec { curve, point }, SigAlg::EcdsaSha384) => verify_ecdsa_der(curve, point, &sha384(c.tbs), c.signature),
        (PublicKey::Rsa(k), SigAlg::RsaPkcs1Sha256) => rsa(k, rsa::Hash::Sha256, &sha256(c.tbs)),
        (PublicKey::Rsa(k), SigAlg::RsaPkcs1Sha384) => rsa(k, rsa::Hash::Sha384, &sha384(c.tbs)),
        _ => Err(Error::UnsupportedAlgorithm),
    }
}

/// The subject Name TLV of a certificate without a full parse. It only skips fields, so it
/// is a cheap filter for the store scan. A wrong answer cannot grant trust: the full parse
/// and the same name comparison follow.
fn subject_of(der: &[u8]) -> Option<&[u8]> {
    let cert = Reader::new(der).read_tag(der::SEQUENCE).ok()?;
    let tbs = Reader::new(cert.content).read_tag(der::SEQUENCE).ok()?;
    let mut t = Reader::new(tbs.content);
    t.read_opt(0xa0).ok()?; // version
    for _ in 0..4 {
        t.read().ok()?; // serial, signature, issuer, validity
    }
    Some(t.read_tag(der::SEQUENCE).ok()?.full)
}

struct Search<'a, 'r> {
    chain: &'r [&'a [u8]],
    /// `None`: not parsed yet. `Some(None)`: did not parse.
    parsed: [Option<Option<Certificate<'a>>>; MAX_CHAIN_CERTS],
    roots: &'r TrustStore,
    now: i64,
    sigs: usize,
    /// The most recent reason a candidate was rejected.
    why: Error,
}

impl<'a> Search<'a, '_> {
    fn peer(&mut self, i: usize) -> Option<Certificate<'a>> {
        *self.parsed[i].get_or_insert_with(|| x509::parse(self.chain[i]).ok())
    }

    fn try_link(&mut self, cur: &Certificate<'_>, cand: &Certificate<'_>, depth: usize) -> Result<(), Error> {
        let aki = cur.extensions.authority_key_id.and_then(|a| a.key_id);
        if cand.subject != cur.issuer || aki.zip(cand.extensions.subject_key_id).is_some_and(|(a, s)| a != s) {
            return Err(Error::NoIssuer);
        }
        common(cand, self.now)?;
        ca_checks(cand, depth)?;
        if self.sigs == MAX_SIG_CHECKS {
            return Err(Error::TooManyChecks);
        }
        self.sigs += 1;
        check_sig(cur, cand)
    }

    fn link(&mut self, cur: &Certificate<'_>, cand: &Certificate<'_>, depth: usize) -> bool {
        match self.try_link(cur, cand, depth) {
            Ok(()) => true,
            Err(Error::NoIssuer) => false, // a different name is not a failure to report
            Err(e) => {
                self.why = e;
                false
            }
        }
    }

    /// Returns the number of certificates on the path, anchor included.
    fn find(&mut self, cur: &Certificate<'a>, idx: usize, depth: usize) -> Option<usize> {
        for i in 0..self.roots.len() {
            let der = self.roots.get(i)?;
            if subject_of(der) != Some(cur.issuer) {
                continue;
            }
            if let Ok(a) = x509::parse(der)
                && self.link(cur, &a, depth)
            {
                return Some(depth + 2);
            }
        }
        if depth == MAX_INTERMEDIATES {
            return None;
        }
        for j in idx + 1..self.chain.len() {
            if let Some(c) = self.peer(j)
                && self.link(cur, &c, depth)
                && let Some(n) = self.find(&c, j, depth + 1)
            {
                return Some(n);
            }
        }
        None
    }
}

/// Validate a server chain (`chain[0]` is the leaf) against `roots` for `host` at `now`.
/// `host` is the URL host: a DNS name in A-label form, or an IP literal.
pub fn verify_server_chain<'a>(
    chain: &[&'a [u8]],
    roots: &TrustStore,
    host: &str,
    now: i64,
) -> Result<VerifiedPeer<'a>, Error> {
    if chain.is_empty() {
        return Err(Error::EmptyChain);
    }
    if chain.len() > MAX_CHAIN_CERTS {
        return Err(Error::TooManyCerts);
    }
    if chain.iter().map(|c| c.len()).sum::<usize>() > MAX_CHAIN_BYTES {
        return Err(Error::ChainTooLarge);
    }
    let leaf = x509::parse(chain[0]).map_err(Error::Parse)?;
    let san = leaf.extensions.san;
    let names = san.as_ref().map(|s| s.names()).into_iter().flatten().map(|n| match n {
        GeneralName::Dns(d) => SanName::Dns(d),
        GeneralName::Ip(i) => SanName::Ip(i),
        GeneralName::Other(_) => SanName::Other,
    });
    hostname::verify(host, names).map_err(Error::Host)?;
    common(&leaf, now)?;
    leaf_checks(&leaf)?;
    match leaf.public_key {
        PublicKey::Ec { curve, point } => check_key(curve, point)?,
        PublicKey::Rsa(_) => {}
        PublicKey::Unsupported { .. } => return Err(Error::BadKey),
    }
    let mut s = Search {
        chain,
        parsed: [None; MAX_CHAIN_CERTS],
        roots,
        now,
        sigs: 0,
        why: Error::NoIssuer,
    };
    s.parsed[0] = Some(Some(leaf));
    match s.find(&leaf, 0, 0) {
        Some(path_certs) => Ok(VerifiedPeer { leaf, key: leaf.public_key, path_certs }),
        None => Err(s.why),
    }
}

/// The signed content of RFC 8446 section 4.4.3 for a server: 64 spaces, the context string,
/// a zero byte, the transcript hash.
fn certificate_verify_content(transcript_hash: &[u8; 32]) -> [u8; 130] {
    const CONTEXT: &[u8; 33] = b"TLS 1.3, server CertificateVerify";
    let mut m = [b' '; 130];
    m[64..97].copy_from_slice(CONTEXT);
    m[97] = 0;
    m[98..].copy_from_slice(transcript_hash);
    m
}

/// Check the server's CertificateVerify. `scheme` is the 16-bit SignatureScheme from the
/// message. Three pass, the ones the ClientHello offers: 0x0403 (ecdsa_secp256r1_sha256, P-256
/// leaf key, `sig` is a DER ECDSA signature), 0x0804 and 0x0805 (rsa_pss_rsae_sha256 and
/// sha384, RSA leaf key, `sig` is the raw RSA signature).
/// `transcript_hash` is SHA-256 over the handshake up to, not including, CertificateVerify.
pub fn verify_certificate_verify(
    peer: &VerifiedPeer<'_>,
    scheme: u16,
    transcript_hash: &[u8; 32],
    sig: &[u8],
) -> Result<(), Error> {
    let content = certificate_verify_content(transcript_hash);
    match (scheme, peer.key) {
        (SIG_ECDSA_SECP256R1_SHA256, PublicKey::Ec { curve: x509::Curve::P256, point }) => {
            verify_ecdsa_der(x509::Curve::P256, point, &sha256(&content), sig)
        }
        (hello::RSA_PSS_RSAE_SHA256, PublicKey::Rsa(k)) => {
            k.verify_pss(rsa::Hash::Sha256, &sha256(&content), sig).map_err(|_| Error::BadSignature)
        }
        (hello::RSA_PSS_RSAE_SHA384, PublicKey::Rsa(k)) => {
            k.verify_pss(rsa::Hash::Sha384, &sha384(&content), sig).map_err(|_| Error::BadSignature)
        }
        (SIG_ECDSA_SECP256R1_SHA256 | hello::RSA_PSS_RSAE_SHA256 | hello::RSA_PSS_RSAE_SHA384, _) => {
            Err(Error::SchemeKeyMismatch)
        }
        _ => Err(Error::UnsupportedScheme),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trust_store::decode_pem;

    const PACK: &str = include_str!("../../../tools/fixtures/verify/pack.pem");
    const CV: &str = include_str!("../../../tools/fixtures/verify/cv.txt");
    const ORACLE: &str = include_str!("../../../tools/fixtures/verify/oracle.txt");
    const STAGING: [&[u8]; 4] = [
        include_bytes!("../../../tools/fixtures/x509/staging-0-leaf.der"),
        include_bytes!("../../../tools/fixtures/x509/staging-1-le-ye2.der"),
        include_bytes!("../../../tools/fixtures/x509/staging-2-isrg-root-ye.der"),
        include_bytes!("../../../tools/fixtures/x509/staging-3-isrg-x2-crosssigned.der"),
    ];
    const NOW: i64 = 1_800_000_000;
    /// Inside the staging leaf's validity (1792055227..1799831226).
    const STAGING_NOW: i64 = 1_795_000_000;
    const HOST: &str = "staging.app.8vox.ai";

    /// `openssl verify` accepts these, and this validator rejects them on purpose.
    const STRICTER: [&str; 10] = [
        "deep-5",
        "ok-any-eku",
        "leaf-ku-keyenc",
        "leaf-ca-true",
        "leaf-no-san",
        "leaf-san-critical-email",
        "inter-nc",
        "inter-nc-critical",
        "inter-policy-constraints",
        "leaf-nc",
    ];

    #[test]
    #[ignore = "requires the host OS root bundle"]
    fn staging_chain_with_system_roots() {
        let (roots, _) = crate::trust_store::load_system().unwrap();
        let peer = verify_server_chain(&STAGING, &roots, HOST, STAGING_NOW).unwrap();
        assert_eq!(peer.path_certs(), 4);
        assert!(verify_server_chain(&STAGING, &roots, "wrong.example", STAGING_NOW).is_err());
    }

    fn section(prefix: &str) -> Vec<(&'static str, &'static str)> {
        let (mut out, mut cur, mut pos) = (vec![], None::<(&str, usize)>, 0);
        for line in PACK.split_inclusive('\n') {
            if line.starts_with("# ") {
                if let Some((h, s)) = cur.take() {
                    out.push((h, &PACK[s..pos]));
                }
                cur = line.strip_prefix(prefix).map(|h| (h.trim_end(), pos + line.len()));
            }
            pos += line.len();
        }
        if let Some((h, s)) = cur {
            out.push((h, &PACK[s..]));
        }
        out
    }

    fn store(name: &str) -> TrustStore {
        let (_, pem) = *section("# store: ").iter().find(|(n, _)| *n == name).unwrap();
        decode_pem(pem.as_bytes()).unwrap()
    }

    struct Case {
        name: &'static str,
        chain: Vec<Vec<u8>>,
        host: &'static str,
        expect: &'static str,
        now: i64,
        store: &'static str,
    }

    fn cases() -> Vec<Case> {
        section("# case: ")
            .into_iter()
            .map(|(h, pem)| {
                let f: Vec<&str> = h.split('|').collect();
                let ts = decode_pem(pem.as_bytes()).unwrap();
                Case {
                    name: f[0],
                    chain: ts.iter().map(|d| d.to_vec()).collect(),
                    host: f[1],
                    expect: f[2],
                    now: f[3].parse().unwrap(),
                    store: f[4],
                }
            })
            .collect()
    }

    fn case(name: &str) -> Case {
        cases().into_iter().find(|c| c.name == name).unwrap()
    }

    fn run<'a>(chain: &'a [Vec<u8>], st: &TrustStore, host: &str, now: i64) -> Result<VerifiedPeer<'a>, Error> {
        let refs: Vec<&[u8]> = chain.iter().map(|c| c.as_slice()).collect();
        verify_server_chain(&refs, st, host, now)
    }

    fn label(r: &Result<VerifiedPeer<'_>, Error>) -> String {
        match r {
            Ok(_) => "ok".into(),
            Err(Error::Host(hostname::Error::NoMatch)) => "HostNoMatch".into(),
            Err(e) => format!("{e:?}"),
        }
    }

    // ----- the public staging chain -----

    fn staging(chain: &[&[u8]], st: &TrustStore, host: &str, now: i64) -> Result<usize, Error> {
        verify_server_chain(chain, st, host, now).map(|p| p.path_certs())
    }

    #[test]
    fn staging_chain_verifies_with_independent_x2() {
        let st = store("x2");
        let p = verify_server_chain(&STAGING, &st, HOST, STAGING_NOW).unwrap();
        assert_eq!(p.path_certs(), 4); // leaf, YE2, Root YE, X2 (the RSA cross-sign is unused)
        assert!(matches!(p.leaf_key(), PublicKey::Ec { curve: x509::Curve::P256, .. }));
        // Also without the unneeded cross-sign.
        assert_eq!(staging(&STAGING[..3], &st, HOST, STAGING_NOW), Ok(4));
        assert_eq!(staging(&STAGING[..2], &st, HOST, STAGING_NOW), Err(Error::NoIssuer));
        // The other name in the SAN, and case-insensitive.
        assert_eq!(staging(&STAGING, &st, "STAGING.8vx.in", STAGING_NOW), Ok(4));
    }

    #[test]
    fn staging_negative() {
        let x2 = store("x2");
        let wrong_host = |h| matches!(staging(&STAGING, &x2, h, STAGING_NOW), Err(Error::Host(_)));
        assert!(wrong_host("staging.app.8vox.com"));
        assert!(wrong_host("app.8vox.ai"));
        assert!(wrong_host("x.staging.app.8vox.ai"));
        assert!(wrong_host("192.0.2.1"));
        // Time: before the leaf, after the leaf, after YE2 (2028-?).
        assert_eq!(staging(&STAGING, &x2, HOST, 1_792_055_226), Err(Error::NotYetValid));
        assert_eq!(staging(&STAGING, &x2, HOST, 1_799_831_227), Err(Error::Expired));
        assert_eq!(staging(&STAGING, &x2, HOST, 1_792_055_227), Ok(4));
        assert_eq!(staging(&STAGING, &x2, HOST, 1_799_831_226), Ok(4));
        // Wrong roots: a store without X2, impostors with X2's name.
        for s in ["root", "other", "impostor-aki", "impostor-ski"] {
            assert!(staging(&STAGING, &store(s), HOST, STAGING_NOW).is_err(), "{s}");
        }
        // Missing/peer-only: the chain carries a self-signed root; the store has another one.
        let c = case("ok-with-root");
        assert_eq!(label(&run(&c.chain, &store("other"), c.host, NOW)), "NoIssuer");
        // Reordered and extraneous.
        let st = &x2;
        assert!(staging(&[STAGING[0], STAGING[2], STAGING[1]], st, HOST, STAGING_NOW).is_err());
        assert!(staging(&[STAGING[1], STAGING[0], STAGING[2]], st, HOST, STAGING_NOW).is_err());
        // An extra entry in the middle is skipped, never trusted.
        assert!(staging(&[STAGING[0], STAGING[3], STAGING[1], STAGING[2]], st, HOST, STAGING_NOW).is_ok());
        assert!(staging(&[STAGING[0], STAGING[0], STAGING[1], STAGING[2]], st, HOST, STAGING_NOW).is_ok());
        assert_eq!(staging(&[], st, HOST, STAGING_NOW), Err(Error::EmptyChain));
        assert!(matches!(staging(&[STAGING[1]], st, HOST, STAGING_NOW), Err(Error::Host(_))));
        assert!(matches!(staging(&[b"junk"], st, HOST, STAGING_NOW), Err(Error::Parse(_))));
    }

    /// Flip one bit at a time over the signed bytes and the signature of every used cert.
    #[test]
    fn tampering_any_used_cert_fails() {
        let st = store("x2");
        for used in 0..3 {
            let der = STAGING[used];
            let c = x509::parse(der).unwrap();
            let off = |s: &[u8]| s.as_ptr() as usize - der.as_ptr() as usize;
            let (t0, t1) = (off(c.tbs), off(c.tbs) + c.tbs.len());
            let (s0, s1) = (off(c.signature), off(c.signature) + c.signature.len());
            let mut ok_after = 0;
            for i in (t0..t1).chain(s0..s1) {
                let mut m = der.to_vec();
                m[i] ^= 0x01;
                let mut chain: Vec<&[u8]> = STAGING.to_vec();
                chain[used] = &m;
                if staging(&chain, &st, HOST, STAGING_NOW).is_ok() {
                    ok_after += 1;
                }
            }
            assert_eq!(ok_after, 0, "cert {used}: a changed signed byte still verified");
        }
        // Swap a signature between two certs.
        let (a, b) = (x509::parse(STAGING[1]).unwrap(), x509::parse(STAGING[2]).unwrap());
        let mut m = STAGING[1].to_vec();
        let at = a.signature.as_ptr() as usize - STAGING[1].as_ptr() as usize;
        if a.signature.len() == b.signature.len() {
            m[at..at + a.signature.len()].copy_from_slice(b.signature);
            let chain = [STAGING[0], &m[..], STAGING[2]];
            assert!(staging(&chain, &st, HOST, STAGING_NOW).is_err());
        }
    }

    // ----- synthetic cases, with openssl as the oracle -----

    #[test]
    fn synthetic_cases_match_expectation_and_oracle() {
        let all = cases();
        assert!(all.len() >= 34);
        for c in &all {
            let r = run(&c.chain, &store(c.store), c.host, c.now);
            assert_eq!(label(&r), c.expect, "{}", c.name);
            let o = ORACLE.lines().find_map(|l| l.strip_prefix(c.name).and_then(|x| x.strip_prefix('|'))).unwrap();
            match (o, r.is_ok()) {
                ("accept", true) | ("reject", false) => {}
                ("reject", true) => panic!("{}: accepted, but openssl rejects", c.name),
                _ => assert!(STRICTER.contains(&c.name), "{}: stricter than openssl, not listed", c.name),
            }
        }
        for n in STRICTER {
            assert!(all.iter().any(|c| c.name == n && c.expect != "ok"), "{n}");
        }
    }

    #[test]
    fn synthetic_time_edges() {
        let c = case("ok");
        let st = store("root");
        assert_eq!(label(&run(&c.chain, &st, c.host, 1_000_000_000)), "NotYetValid");
        assert_eq!(label(&run(&c.chain, &st, c.host, 5_000_000_000)), "Expired");
        let l = x509::parse(&c.chain[0]).unwrap();
        assert!(run(&c.chain, &st, c.host, l.not_after.0).is_ok());
        assert_eq!(label(&run(&c.chain, &st, c.host, l.not_after.0 + 1)), "Expired");
    }

    #[test]
    fn impostor_roots() {
        let c = case("ok");
        // Same subject, other key, other SKI: AKI does not match, so no candidate.
        assert_eq!(label(&run(&c.chain, &store("impostor-aki"), c.host, NOW)), "NoIssuer");
        // Same subject and same SKI, other key: the signature check is what stops it.
        assert_eq!(label(&run(&c.chain, &store("impostor-ski"), c.host, NOW)), "BadSignature");
    }

    #[test]
    fn peer_root_is_never_an_anchor() {
        let c = case("ok-with-root");
        assert!(run(&c.chain, &store("root"), c.host, NOW).is_ok());
        for s in ["other", "x2", "impostor-aki"] {
            assert!(run(&c.chain, &store(s), c.host, NOW).is_err(), "{s}");
        }
    }

    #[test]
    fn limits() {
        let c = case("ok");
        let st = store("root");
        let one = &c.chain[0];
        let nine: Vec<Vec<u8>> = (0..MAX_CHAIN_CERTS + 1).map(|_| one.clone()).collect();
        assert_eq!(run(&nine, &st, c.host, NOW).err(), Some(Error::TooManyCerts));
        let big: Vec<Vec<u8>> = vec![one.clone(), vec![0; MAX_CHAIN_BYTES]];
        assert_eq!(run(&big, &st, c.host, NOW).err(), Some(Error::ChainTooLarge));
        // Eight copies of the leaf: bounded work, no path.
        let eight: Vec<Vec<u8>> = (0..MAX_CHAIN_CERTS).map(|_| one.clone()).collect();
        assert_eq!(run(&eight, &st, c.host, NOW).err(), Some(Error::NoIssuer));
        // Self-issued repeated CA, untrusted: bounded and fails.
        let s = case("self-ca");
        assert_eq!(label(&run(&s.chain, &st, s.host, NOW)), "NoIssuer");
    }

    /// The RSA signature is on the intermediate in `rsa-root` and on the leaf in `rsa-inter`.
    #[test]
    fn rsa_certificate_signatures_are_checked() {
        for (name, signed) in [("rsa-root", 1), ("rsa-inter", 0)] {
            let c = case(name);
            let st = store(c.store);
            assert!(run(&c.chain, &st, c.host, c.now).is_ok(), "{name}");
            let cert = x509::parse(&c.chain[signed]).unwrap();
            assert!(matches!(cert.sig_alg, SigAlg::RsaPkcs1Sha256), "{name}");
            let at = cert.signature.as_ptr() as usize - c.chain[signed].as_ptr() as usize;
            for off in [0, 1, 100, cert.signature.len() - 1] {
                let mut chain = c.chain.clone();
                chain[signed][at + off] ^= 0x04;
                assert_eq!(run(&chain, &st, c.host, c.now).err(), Some(Error::BadSignature), "{name} {off}");
            }
            // The signed bytes: one changed byte in the validity period still parses and fails.
            let mut chain = c.chain.clone();
            let tbs = cert.tbs.as_ptr() as usize - c.chain[signed].as_ptr() as usize;
            chain[signed][tbs + cert.tbs.len() - 1] ^= 1;
            assert!(run(&chain, &st, c.host, c.now).is_err(), "{name} tbs");
        }
    }

    #[test]
    fn depth_limit() {
        let st = store("root");
        let c = case("deep-4");
        assert_eq!(run(&c.chain, &st, c.host, NOW).unwrap().path_certs(), 2 + MAX_INTERMEDIATES);
        let c = case("deep-5");
        assert_eq!(c.chain.len(), 6);
        assert_eq!(label(&run(&c.chain, &st, c.host, NOW)), "NoIssuer");
    }

    #[test]
    fn leaf_key_check_is_cheap_and_correct() {
        let c = case("ok");
        let l = x509::parse(&c.chain[0]).unwrap();
        let PublicKey::Ec { curve, point } = l.public_key else { panic!() };
        assert_eq!(check_key(curve, point), Ok(()));
        let mut bad = point.to_vec();
        bad[40] ^= 1; // off the curve
        assert_eq!(check_key(curve, &bad), Err(Error::BadKey));
        bad.truncate(64);
        assert_eq!(check_key(curve, &bad), Err(Error::BadKey));
        let mut zero_x = point.to_vec();
        zero_x[1..33].fill(0xff); // X >= p
        assert_eq!(check_key(curve, &zero_x), Err(Error::BadKey));
        assert_eq!(check_key(x509::Curve::P384, point), Err(Error::BadKey));
    }

    // ----- CertificateVerify -----

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap()).collect()
    }

    fn cv(case_name: &str, variant: &str) -> Vec<u8> {
        let l = CV.lines().find(|l| l.starts_with(&format!("{case_name}|{variant}|"))).unwrap();
        unhex(l.rsplit('|').next().unwrap())
    }

    fn th(s: &[u8]) -> [u8; 32] {
        sha256(s)
    }

    #[test]
    fn certificate_verify_vectors() {
        let c = case("ok");
        let st = store("root");
        let p = run(&c.chain, &st, c.host, NOW).unwrap();
        let h = th(b"bare transcript");
        let s = SIG_ECDSA_SECP256R1_SHA256;
        assert_eq!(verify_certificate_verify(&p, s, &h, &cv("ok", "ok")), Ok(()));
        for v in ["client-ctx", "no-pad", "no-zero", "other-hash", "sha384"] {
            assert_eq!(verify_certificate_verify(&p, s, &h, &cv("ok", v)), Err(Error::BadSignature), "{v}");
        }
        // Another transcript with the good signature.
        assert!(verify_certificate_verify(&p, s, &th(b"other transcript"), &cv("ok", "ok")).is_err());
        // Unoffered schemes, even with the right signature bytes.
        for bad in [0x0503, 0x0401, 0x0807, 0x0000, 0x0403 ^ 0x0100] {
            assert_eq!(verify_certificate_verify(&p, bad, &h, &cv("ok", "ok")), Err(Error::UnsupportedScheme));
        }
        // Offered RSA-PSS schemes need an RSA leaf key.
        for rsa_scheme in [0x0804, 0x0805] {
            assert_eq!(verify_certificate_verify(&p, rsa_scheme, &h, &cv("ok", "ok")), Err(Error::SchemeKeyMismatch));
        }
        // Malformed signatures.
        let good = cv("ok", "ok");
        assert!(verify_certificate_verify(&p, s, &h, &[]).is_err());
        assert!(verify_certificate_verify(&p, s, &h, &good[..good.len() - 1]).is_err());
        let mut t = good.clone();
        t.push(0);
        assert!(verify_certificate_verify(&p, s, &h, &t).is_err());
        for i in 0..good.len() {
            let mut m = good.clone();
            m[i] ^= 0x80;
            assert!(verify_certificate_verify(&p, s, &h, &m).is_err(), "flip at {i}");
        }
        // Raw r||s is not DER.
        assert!(verify_certificate_verify(&p, s, &h, &[0x11; 64]).is_err());
    }

    #[test]
    fn certificate_verify_key_pairing() {
        // A P-384 leaf chain is valid, but 0x0403 needs P-256.
        let c = case("ok-leaf-p384");
        let p = run(&c.chain, &store("root"), c.host, NOW).unwrap();
        assert!(matches!(p.leaf_key(), PublicKey::Ec { curve: x509::Curve::P384, .. }));
        let h = th(b"bare transcript");
        for v in ["ok", "sha384"] {
            assert_eq!(
                verify_certificate_verify(&p, 0x0403, &h, &cv("ok-leaf-p384", v)),
                Err(Error::SchemeKeyMismatch)
            );
        }
        assert_eq!(
            verify_certificate_verify(&p, 0x0503, &h, &cv("ok-leaf-p384", "sha384")),
            Err(Error::UnsupportedScheme)
        );
        // A signature made with another leaf key over the same content fails.
        let c2 = case("ok");
        let p2 = run(&c2.chain, &store("root"), c2.host, NOW).unwrap();
        assert_eq!(
            verify_certificate_verify(&p2, 0x0403, &h, &cv("ok-leaf-p384", "ok")),
            Err(Error::BadSignature)
        );
    }

    #[test]
    fn content_layout() {
        let m = certificate_verify_content(&[7; 32]);
        assert_eq!(&m[..64], &[0x20; 64]);
        assert_eq!(&m[64..97], b"TLS 1.3, server CertificateVerify");
        assert_eq!(m[97], 0);
        assert_eq!(&m[98..], &[7; 32]);
    }
}
