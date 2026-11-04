//! TLS 1.3 key schedule for TLS_AES_128_GCM_SHA256 (RFC 8446 §7.1, §4.4.4, §7.2, §7.3).
//!
//! ```text
//! early secret      = Extract(0, 0)                       (no PSK)
//! handshake secret  = Extract(Derive-Secret(early, "derived", ""), ECDHE)
//! master secret     = Extract(Derive-Secret(handshake, "derived", ""), 0)
//! ```
//! Each traffic secret has a direction (`Client` or `Server`) and an epoch (`Handshake`
//! or `Application`) in its type. The compiler then rejects a server secret where a client
//! secret is needed. All buffers are fixed-size arrays on the stack. Secrets are wiped when
//! dropped (a volatile write, so the compiler cannot remove it). Secrets have no `Clone`
//! and a `Debug` that prints nothing. Nothing here logs.
//!
//! Test vectors: RFC 8448 §3 (simple 1-RTT handshake).

use crate::hkdf::{self, HkdfError};
use crate::hmac::hmac_sha256;
use crate::sha256::{DIGEST_LEN, Sha256, sha256};
use std::fmt;
use std::marker::PhantomData;

/// Bytes in a hash, a secret and a Finished value.
pub const HASH_LEN: usize = DIGEST_LEN;
/// AES-128 key bytes.
pub const KEY_LEN: usize = 16;
/// AEAD IV bytes.
pub const IV_LEN: usize = 12;
/// `HkdfLabel` is at most 2 + (1 + 255) + (1 + 255) bytes.
const MAX_HKDF_LABEL: usize = 514;
/// Longest label text. "tls13 " takes 6 of the 255 label bytes.
pub const MAX_LABEL_LEN: usize = 255 - 6;
/// Longest context.
pub const MAX_CONTEXT_LEN: usize = 255;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyScheduleError {
    /// The label is empty or longer than `MAX_LABEL_LEN`.
    BadLabel,
    /// The context is longer than `MAX_CONTEXT_LEN`.
    ContextTooLong,
    /// The output length is 0 or more than 65535 (the `uint16` length field) or HKDF's limit.
    BadOutputLen,
}

impl fmt::Display for KeyScheduleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            KeyScheduleError::BadLabel => "HKDF label is empty or too long",
            KeyScheduleError::ContextTooLong => "HKDF context is longer than 255 bytes",
            KeyScheduleError::BadOutputLen => "HKDF output length is not allowed",
        })
    }
}
impl std::error::Error for KeyScheduleError {}

impl From<HkdfError> for KeyScheduleError {
    fn from(_: HkdfError) -> Self {
        KeyScheduleError::BadOutputLen
    }
}

/// SHA-256 of the empty string. `Derive-Secret(.., "derived", "")` uses it.
pub const EMPTY_HASH: [u8; HASH_LEN] = [
    0xe3, 0xb0, 0xc4, 0x42, 0x98, 0xfc, 0x1c, 0x14, 0x9a, 0xfb, 0xf4, 0xc8, 0x99, 0x6f, 0xb9, 0x24, 0x27, 0xae, 0x41, 0xe4, 0x64, 0x9b,
    0x93, 0x4c, 0xa4, 0x95, 0x99, 0x1b, 0x78, 0x52, 0xb8, 0x55,
];

/// A 32-byte secret. No `Clone`, no `Copy`, wiped on drop, `Debug` shows nothing.
struct Secret([u8; HASH_LEN]);

impl Drop for Secret {
    fn drop(&mut self) {
        for b in self.0.iter_mut() {
            // SAFETY: `b` is a valid, aligned `&mut u8`.
            unsafe { std::ptr::write_volatile(b, 0) };
        }
        std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
    }
}

/// `HKDF-Expand-Label(secret, label, context, out.len())` (RFC 8446 §7.1).
///
/// The info is `uint16 length || uint8 len("tls13 " + label) || "tls13 " + label ||
/// uint8 len(context) || context`. Every length is checked. No heap.
pub fn hkdf_expand_label(secret: &[u8; HASH_LEN], label: &[u8], context: &[u8], out: &mut [u8]) -> Result<(), KeyScheduleError> {
    if label.is_empty() || label.len() > MAX_LABEL_LEN {
        return Err(KeyScheduleError::BadLabel);
    }
    if context.len() > MAX_CONTEXT_LEN {
        return Err(KeyScheduleError::ContextTooLong);
    }
    if out.is_empty() || out.len() > u16::MAX as usize {
        return Err(KeyScheduleError::BadOutputLen);
    }
    let mut info = [0u8; MAX_HKDF_LABEL];
    info[..2].copy_from_slice(&(out.len() as u16).to_be_bytes());
    info[2] = (6 + label.len()) as u8;
    info[3..9].copy_from_slice(b"tls13 ");
    let mut n = 9;
    info[n..n + label.len()].copy_from_slice(label);
    n += label.len();
    info[n] = context.len() as u8;
    n += 1;
    info[n..n + context.len()].copy_from_slice(context);
    n += context.len();
    hkdf::expand(secret, &info[..n], out)?;
    Ok(())
}

/// `Derive-Secret(secret, label, transcript_hash)` = `HKDF-Expand-Label(secret, label,
/// Hash(messages), 32)`. The caller passes the hash. For an empty transcript, pass `EMPTY_HASH`.
pub fn derive_secret(secret: &[u8; HASH_LEN], label: &[u8], transcript_hash: &[u8; HASH_LEN]) -> Result<[u8; HASH_LEN], KeyScheduleError> {
    let mut out = [0u8; HASH_LEN];
    hkdf_expand_label(secret, label, transcript_hash, &mut out)?;
    Ok(out)
}

// Label and context lengths below are fixed and legal, so these cannot fail.
fn expand32(secret: &[u8; HASH_LEN], label: &[u8], ctx: &[u8]) -> Secret {
    let mut out = Secret([0; HASH_LEN]);
    hkdf_expand_label(secret, label, ctx, &mut out.0).expect("fixed legal lengths");
    out
}

/// The running hash of handshake messages. Feed it exact message bytes, header included.
#[derive(Clone, Default)]
pub struct Transcript(Sha256);

impl Transcript {
    pub fn new() -> Self {
        Self(Sha256::new())
    }
    pub fn update(&mut self, message: &[u8]) {
        self.0.update(message);
    }
    /// The hash of everything so far. The transcript stays usable. This copies 108 bytes of
    /// hash state. It does not allocate and does not rehash old messages.
    pub fn hash(&self) -> [u8; HASH_LEN] {
        self.0.clone().finalize()
    }
}

/// Direction markers.
pub enum Client {}
pub enum Server {}
/// Epoch markers.
pub enum Handshake {}
pub enum Application {}

/// A traffic secret for one direction and one epoch.
pub struct TrafficSecret<D, E> {
    secret: Secret,
    _m: PhantomData<(D, E)>,
}

impl<D, E> fmt::Debug for TrafficSecret<D, E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TrafficSecret(..)")
    }
}

/// An AES-128-GCM key and a 12-byte IV. Wiped on drop.
pub struct TrafficKeys {
    pub key: [u8; KEY_LEN],
    pub iv: [u8; IV_LEN],
}

impl Drop for TrafficKeys {
    fn drop(&mut self) {
        for b in self.key.iter_mut().chain(self.iv.iter_mut()) {
            // SAFETY: `b` is a valid, aligned `&mut u8`.
            unsafe { std::ptr::write_volatile(b, 0) };
        }
        std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
    }
}

impl fmt::Debug for TrafficKeys {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TrafficKeys(..)")
    }
}

impl<D, E> TrafficSecret<D, E> {
    fn new(secret: Secret) -> Self {
        Self { secret, _m: PhantomData }
    }

    /// `key = Expand-Label(secret, "key", "", 16)` and `iv = Expand-Label(secret, "iv", "", 12)`
    /// (RFC 8446 §7.3).
    pub fn traffic_keys(&self) -> TrafficKeys {
        let mut k = TrafficKeys { key: [0; KEY_LEN], iv: [0; IV_LEN] };
        hkdf_expand_label(&self.secret.0, b"key", b"", &mut k.key).expect("fixed legal lengths");
        hkdf_expand_label(&self.secret.0, b"iv", b"", &mut k.iv).expect("fixed legal lengths");
        k
    }
}

impl<D> TrafficSecret<D, Handshake> {
    fn finished_key(&self) -> Secret {
        expand32(&self.secret.0, b"finished", b"")
    }

    /// `verify_data = HMAC(finished_key, Transcript-Hash(..))` (RFC 8446 §4.4.4). The
    /// caller passes the hash of the messages up to, but not including, this Finished.
    pub fn finished_verify_data(&self, transcript_hash: &[u8; HASH_LEN]) -> [u8; HASH_LEN] {
        hmac_sha256(&self.finished_key().0, transcript_hash)
    }

    /// Check a received `Finished` body. Wrong length is false. The comparison does not stop
    /// at the first difference.
    pub fn verify_finished(&self, transcript_hash: &[u8; HASH_LEN], received: &[u8]) -> bool {
        let want = self.finished_verify_data(transcript_hash);
        received.len() == HASH_LEN && want.iter().zip(received).fold(0u8, |a, (x, y)| a | (x ^ y)) == 0
    }
}

impl<D> TrafficSecret<D, Application> {
    /// `application_traffic_secret_N+1 = Expand-Label(secret_N, "traffic upd", "", 32)`
    /// (RFC 8446 §7.2). It consumes the old secret, so it cannot be used again.
    pub fn update(self) -> Self {
        Self::new(expand32(&self.secret.0, b"traffic upd", b""))
    }
}

/// The handshake secret. Gives the two handshake traffic secrets and the master secret.
pub struct HandshakeSecret(Secret);

/// The master secret. Gives the two application traffic secrets.
pub struct MasterSecret(Secret);

impl HandshakeSecret {
    /// Start from the X25519 shared secret (32 bytes, already checked as non-zero by
    /// `x25519::shared_secret`). There is no PSK, so the early secret is `Extract(0, 0)`.
    pub fn from_ecdhe(ecdhe: &[u8; HASH_LEN]) -> Self {
        let early = Secret(hkdf::extract(&[0u8; HASH_LEN], &[0u8; HASH_LEN]));
        let derived = expand32(&early.0, b"derived", &EMPTY_HASH);
        Self(Secret(hkdf::extract(&derived.0, ecdhe)))
    }

    /// Hash is over ClientHello..ServerHello.
    pub fn client_traffic(&self, hello_hash: &[u8; HASH_LEN]) -> TrafficSecret<Client, Handshake> {
        TrafficSecret::new(expand32(&self.0.0, b"c hs traffic", hello_hash))
    }

    /// Hash is over ClientHello..ServerHello.
    pub fn server_traffic(&self, hello_hash: &[u8; HASH_LEN]) -> TrafficSecret<Server, Handshake> {
        TrafficSecret::new(expand32(&self.0.0, b"s hs traffic", hello_hash))
    }

    /// Consumes the handshake secret, so it cannot be used after the master secret exists.
    pub fn into_master(self) -> MasterSecret {
        let derived = expand32(&self.0.0, b"derived", &EMPTY_HASH);
        MasterSecret(Secret(hkdf::extract(&derived.0, &[0u8; HASH_LEN])))
    }
}

impl MasterSecret {
    /// Hash is over ClientHello..server Finished.
    pub fn client_traffic(&self, finished_hash: &[u8; HASH_LEN]) -> TrafficSecret<Client, Application> {
        TrafficSecret::new(expand32(&self.0.0, b"c ap traffic", finished_hash))
    }

    /// Hash is over ClientHello..server Finished.
    pub fn server_traffic(&self, finished_hash: &[u8; HASH_LEN]) -> TrafficSecret<Server, Application> {
        TrafficSecret::new(expand32(&self.0.0, b"s ap traffic", finished_hash))
    }
}

/// One-shot hash, for tests and callers that hold the whole transcript.
pub fn transcript_hash(messages: &[u8]) -> [u8; HASH_LEN] {
    sha256(messages)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sha256::hex;

    // RFC 8448 §3: ClientHello and ServerHello (handshake messages with headers).
    pub(crate) const CH: &str = "010000c00303cb34ecb1e78163ba1c38c6dacb196a6dffa21a8d9912ec18a2ef6283024dece7000006130113031302010000910000000b0009000006736572766572ff01000100000a00140012001d0017001800190100010101020103010400230000003300260024001d002099381de560e4bd43d23d8e435a7dbafeb3c06e51c13cae4d5413691e529aaf2c002b0003020304000d0020001e040305030603020308040805080604010501060102010402050206020202002d00020101001c00024001";
    pub(crate) const SH: &str = "020000560303a6af06a4121860dc5e6e60249cd34c95930c8ac5cb1434dac155772ed3e2692800130100002e00330024001d0020c9828876112095fe66762bdbf7c672e156d6cc253b833df1dd69b1b04e751f0f002b00020304";
    const SHARED: &str = "8bd4054fb55b9d63fdfbacf9f04b9f0d35e6d63f537563efd46272900f89492d";

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap()).collect()
    }
    fn arr(s: &str) -> [u8; 32] {
        unhex(s).try_into().unwrap()
    }

    #[test]
    fn empty_hash_constant() {
        assert_eq!(EMPTY_HASH, sha256(b""));
    }

    #[test]
    fn rfc8448_early_and_derived() {
        // Early secret and "derived" are internal. Check them through HKDF directly.
        let early = hkdf::extract(&[0; 32], &[0; 32]);
        assert_eq!(hex(&early), "33ad0a1c607ec03b09e6cd9893680ce210adf300aa1f2660e1b22e10f170f92a");
        let derived = derive_secret(&early, b"derived", &EMPTY_HASH).unwrap();
        assert_eq!(hex(&derived), "6f2615a108c702c5678f54fc9dbab69716c076189c48250cebeac3576c3611ba");
        assert_eq!(hex(&hkdf::extract(&derived, &arr(SHARED))), "1dc826e93606aa6fdc0aadc12f741b01046aa6b99f691ed221a9f0ca043fbeac");
    }

    #[test]
    fn rfc8448_handshake_traffic_keys_and_finished_keys() {
        let mut t = Transcript::new();
        t.update(&unhex(CH));
        t.update(&unhex(SH));
        let h = t.hash();
        let hs = HandshakeSecret::from_ecdhe(&arr(SHARED));
        let c = hs.client_traffic(&h);
        let s = hs.server_traffic(&h);
        assert_eq!(hex(&c.secret.0), "b3eddb126e067f35a780b3abf45e2d8f3b1a950738f52e9600746a0e27a55a21");
        assert_eq!(hex(&s.secret.0), "b67b7d690cc16c4e75e54213cb2d37b4e9c912bcded9105d42befd59d391ad38");
        let sk = s.traffic_keys();
        assert_eq!(hex(&sk.key), "3fce516009c21727d0f2e4e86ee403bc");
        assert_eq!(hex(&sk.iv), "5d313eb2671276ee13000b30");
        let ck = c.traffic_keys();
        assert_eq!(hex(&ck.key), "dbfaa693d1762c5b666af5d950258d01");
        assert_eq!(hex(&ck.iv), "5bd3c71b836e0b76bb73265f");
        assert_eq!(hex(&s.finished_key().0), "008d3b66f816ea559f96b537e885c31fc068bf492c652f01f288a1d8cdc19fc8");
        assert_eq!(hex(&c.finished_key().0), "b80ad01015fb2f0bd65ff7d4da5d6bf83f84821d1f87fdc7d3c75b5a7b42d9c4");
    }

    #[test]
    fn rfc8448_server_finished_and_application_secrets() {
        // Transcript hash through server CertificateVerify, and through server Finished.
        let cv_hash = arr("edb7725fa7a3473b031ec8ef65a2485493900138a2b91291407d7951a06110ed");
        let sf_hash = arr("9608102a0f1ccc6db6250b7b7e417b1a000eaada3daae4777a7686c9ff83df13");
        let hs = HandshakeSecret::from_ecdhe(&arr(SHARED));
        let h = transcript_hash(&[unhex(CH), unhex(SH)].concat());
        let s = hs.server_traffic(&h);
        let c = hs.client_traffic(&h);
        let vd = s.finished_verify_data(&cv_hash);
        assert_eq!(hex(&vd), "9b9b141d906337fbd2cbdce71df4deda4ab42c309572cb7fffee5454b78f0718");
        assert!(s.verify_finished(&cv_hash, &vd));
        assert!(!s.verify_finished(&sf_hash, &vd));
        assert!(!s.verify_finished(&cv_hash, &vd[..31]));
        let mut bad = vd;
        bad[31] ^= 1;
        assert!(!s.verify_finished(&cv_hash, &bad));
        // Client Finished covers the hash through server Finished.
        assert_eq!(hex(&c.finished_verify_data(&sf_hash)), "a8ec436d677634ae525ac1fcebe11a039ec17694fac6e98527b642f2edd5ce61");

        let m = hs.into_master();
        let ca = m.client_traffic(&sf_hash);
        let sa = m.server_traffic(&sf_hash);
        assert_eq!(hex(&ca.secret.0), "9e40646ce79a7f9dc05af8889bce6552875afa0b06df0087f792ebb7c17504a5");
        assert_eq!(hex(&sa.secret.0), "a11af9f05531f856ad47116b45a950328204b4f44bfb6b3a4b4f1f3fcb631643");
    }

    #[test]
    fn master_secret_matches_rfc8448() {
        let hs = HandshakeSecret::from_ecdhe(&arr(SHARED));
        let m = hs.into_master();
        assert_eq!(hex(&m.0.0), "18df06843d13a08bf2a449844c5f8a478001bc4d4c627984d5a41da8d0402919");
    }

    #[test]
    fn traffic_update_matches_independent_computation() {
        // Computed with Python hashlib/hmac: Expand-Label(secret, "traffic upd", "", 32), twice.
        let m = HandshakeSecret::from_ecdhe(&arr(SHARED)).into_master();
        let sf = arr("9608102a0f1ccc6db6250b7b7e417b1a000eaada3daae4777a7686c9ff83df13");
        let n1 = m.client_traffic(&sf).update();
        assert_eq!(hex(&n1.secret.0), "fcdfcc72725aaee48bf64e4fd8b749cdbdbab39d90da0b26e2245ca6ea167207");
        let n2 = n1.update();
        assert_eq!(hex(&n2.secret.0), "68474fe58c172143d0a009a76edd231b0d934f3058588ea0852662279e54a573");
        assert_eq!(hex(&n2.traffic_keys().key), "215f086ece93c88043a7d492b236c774");
    }

    #[test]
    fn expand_label_checks_lengths() {
        let s = [1u8; 32];
        let mut o = [0u8; 8];
        assert!(hkdf_expand_label(&s, b"", b"", &mut o).is_err());
        assert_eq!(hkdf_expand_label(&s, &[b'a'; 250], b"", &mut o), Err(KeyScheduleError::BadLabel));
        assert!(hkdf_expand_label(&s, &[b'a'; 249], &[0; 255], &mut o).is_ok());
        assert_eq!(hkdf_expand_label(&s, b"x", &[0; 256], &mut o), Err(KeyScheduleError::ContextTooLong));
        assert_eq!(hkdf_expand_label(&s, b"x", b"", &mut []), Err(KeyScheduleError::BadOutputLen));
        let mut big = vec![0u8; 8161];
        assert_eq!(hkdf_expand_label(&s, b"x", b"", &mut big), Err(KeyScheduleError::BadOutputLen));
        let mut huge = vec![0u8; 65536];
        assert_eq!(hkdf_expand_label(&s, b"x", b"", &mut huge), Err(KeyScheduleError::BadOutputLen));
    }

    #[test]
    fn expand_label_matches_manual_info() {
        let s = [7u8; 32];
        let mut got = [0u8; 12];
        hkdf_expand_label(&s, b"iv", b"ctx", &mut got).unwrap();
        let mut info = vec![0, 12, 8];
        info.extend_from_slice(b"tls13 iv");
        info.push(3);
        info.extend_from_slice(b"ctx");
        let mut want = [0u8; 12];
        hkdf::expand(&s, &info, &mut want).unwrap();
        assert_eq!(got, want);
    }

    #[test]
    fn transcript_hash_is_repeatable_and_streaming() {
        let mut t = Transcript::new();
        t.update(b"abc");
        assert_eq!(t.hash(), t.hash());
        t.update(b"def");
        assert_eq!(t.hash(), sha256(b"abcdef"));
    }

    #[test]
    fn debug_does_not_print_secrets() {
        let hs = HandshakeSecret::from_ecdhe(&[5; 32]);
        let c = hs.client_traffic(&[0; 32]);
        assert_eq!(format!("{:?} {:?}", c, c.traffic_keys()), "TrafficSecret(..) TrafficKeys(..)");
    }
}
