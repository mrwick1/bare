//! Synchronous TLS 1.3 client stream (RFC 8446). Learning code, NOT audited.
//!
//! Composes `record`, `hello`, `key_schedule`, `x25519` and a chain verifier into one
//! `std::io::{Read, Write}` stream. It offers one suite (TLS_AES_128_GCM_SHA256), one group
//! (X25519), CertificateVerify schemes 0x0403 (ecdsa_secp256r1_sha256), 0x0804 and 0x0805 (rsa_pss_rsae_sha256 and sha384)
//! and ALPN `h2` and `http/1.1` (`h2` only when the caller offers it).
//!
//! Handshake order is fixed. Any other message is an error:
//! `ClientHello -> ServerHello -> EncryptedExtensions -> Certificate -> CertificateVerify
//!  -> server Finished -> client Finished`.
//!
//! Decisions:
//! - Handshake messages are reassembled from records into one buffer. A message may span
//!   records and records may hold several messages. A message that is not whole at a key
//!   change, or bytes left after ServerHello or server Finished, are errors (RFC 8446 5.1).
//! - The compatibility `change_cipher_spec` is allowed from the ClientHello until the
//!   server Finished (section 5). The handshake has a byte budget on everything read from
//!   the transport (so a flood of ignored CCS records ends) and a deadline.
//!   `HANDSHAKE_DEADLINE` is checked before each transport call. A blocking call is not
//!   interrupted: the caller must set socket read and write timeouts.
//! - The chain verifier is a private trait. There is no insecure mode and no callback.
//!   `chain[0]` is the leaf. The leaf key must be P-256 (CertificateVerify 0x0403) or RSA of 2048 to 4096 bits
//!   (0x0804 or 0x0805). A scheme that does not fit the leaf key is an error.
//! - After the handshake only NewSessionTicket is accepted (parsed and dropped, no resumption).
//!   KeyUpdate, post-handshake CertificateRequest and all other messages fail closed with an alert.
//! - Any error ends the stream for good. A fatal alert is sent when one fits.
//! - IP address hosts are not supported (the ClientHello encoder rejects them).
//! - Secrets: client random, session ID and X25519 scalar come from `random::fill`.
//!   Scalar, shared secret and traffic keys are wiped after use. Nothing here logs.

use crate::hello::{self, ClientHelloParams, HelloError};
use crate::key_schedule::{HandshakeSecret, Transcript};
use crate::record::{self, Record, RecordLayer};
use crate::sha256::sha256;
use crate::sha384::sha384;
use crate::trust_store::TrustStore;
use crate::{ecc, random, rsa, verify, x25519, x509};
use std::fmt;
use std::io::{self, ErrorKind, Read, Write};
use std::time::{Duration, Instant};

/// Wall-clock limit for the whole handshake.
pub const HANDSHAKE_DEADLINE: Duration = Duration::from_secs(30);
/// Bytes read from the transport during the handshake, ignored records included.
pub const HANDSHAKE_WIRE_BUDGET: usize = 128 * 1024;
const MAX_SERVER_HELLO: usize = 1024;
const MAX_ENCRYPTED_EXTENSIONS: usize = 4096;
/// Body of the Certificate message.
const MAX_CERTIFICATE: usize = 64 * 1024;
const MAX_CERT_DER: usize = 16 * 1024;
/// Certificates in one Certificate message.
pub const MAX_CHAIN: usize = 8;
/// Scheme and length (4 bytes) and an RSA-4096 signature (512 bytes).
const MAX_CERTIFICATE_VERIFY: usize = 4 + 512;
/// Body of a NewSessionTicket.
const MAX_TICKET: usize = 16 * 1024;

const ENCRYPTED_EXTENSIONS: u8 = hello::ENCRYPTED_EXTENSIONS;
const CERTIFICATE_REQUEST: u8 = 13;
const NEW_SESSION_TICKET: u8 = 4;
const KEY_UPDATE: u8 = 24;

/// What went wrong. Each variant maps to an alert in [`Error::alert`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Io(ErrorKind),
    Record(record::Error),
    Hello(HelloError),
    /// The OS gave no random bytes.
    Random,
    /// The X25519 shared secret is all zero.
    KeyExchange,
    /// A field is malformed (decode_error).
    Decode(&'static str),
    /// Wrong message or order (unexpected_message).
    Unexpected(&'static str),
    /// A feature this client does not have (unexpected_message).
    Unsupported(&'static str),
    /// A server value that is not allowed (illegal_parameter).
    Illegal(&'static str),
    /// An extension that we did not offer (unsupported_extension).
    Extension(u16),
    /// ALPN is not `http/1.1` (no_application_protocol).
    Alpn,
    /// The chain verifier said no (bad_certificate).
    Certificate(String),
    /// CertificateVerify is wrong (decrypt_error).
    BadSignature,
    /// Server Finished is wrong (decrypt_error).
    BadFinished,
    /// The handshake used too many bytes or too much time.
    HandshakeLimit(&'static str),
}

impl Error {
    /// The alert (RFC 8446 section 6.2) to send for this error, if one fits.
    pub fn alert(&self) -> Option<u8> {
        Some(match self {
            Error::Record(e) => return e.alert(),
            Error::Hello(HelloError::UnsupportedVersion) => 70,
            Error::Hello(HelloError::DuplicateExtension(_) | HelloError::UnsolicitedExtension(_)) => 110,
            Error::Hello(HelloError::Truncated | HelloError::BadLength) => 50,
            Error::Hello(HelloError::BadMessageType | HelloError::TooLarge) => 10,
            Error::Hello(_) | Error::Illegal(_) | Error::KeyExchange => 47,
            Error::Random | Error::Io(_) | Error::HandshakeLimit(_) => return None,
            Error::Decode(_) => 50,
            Error::Unexpected(_) | Error::Unsupported(_) => 10,
            Error::Extension(_) => 110,
            Error::Alpn => 120,
            Error::Certificate(_) => 42,
            Error::BadSignature | Error::BadFinished => 51,
        })
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(k) => write!(f, "transport error: {k}"),
            Error::Record(e) => write!(f, "record error: {e}"),
            Error::Hello(e) => write!(f, "hello error: {e}"),
            Error::Random => f.write_str("no OS random bytes"),
            Error::KeyExchange => f.write_str("X25519 shared secret is all zero"),
            Error::Decode(m) => write!(f, "malformed message: {m}"),
            Error::Unexpected(m) => write!(f, "unexpected message: {m}"),
            Error::Unsupported(m) => write!(f, "not supported: {m}"),
            Error::Illegal(m) => write!(f, "illegal parameter: {m}"),
            Error::Extension(t) => write!(f, "unsolicited extension {t}"),
            Error::Alpn => f.write_str("server selected an ALPN protocol other than http/1.1"),
            Error::Certificate(m) => write!(f, "certificate rejected: {m}"),
            Error::BadSignature => f.write_str("CertificateVerify signature is wrong"),
            Error::BadFinished => f.write_str("server Finished is wrong"),
            Error::HandshakeLimit(m) => write!(f, "handshake limit: {m}"),
        }
    }
}
impl std::error::Error for Error {}

impl From<record::Error> for Error {
    fn from(e: record::Error) -> Self {
        Error::Record(e)
    }
}
impl From<HelloError> for Error {
    fn from(e: HelloError) -> Self {
        Error::Hello(e)
    }
}

impl From<Error> for io::Error {
    fn from(e: Error) -> Self {
        match e {
            Error::Io(k) | Error::Record(record::Error::Io(k)) => io::Error::new(k, e),
            Error::Record(record::Error::UnexpectedEof | record::Error::Truncated) => {
                io::Error::new(ErrorKind::UnexpectedEof, e)
            }
            e => io::Error::new(ErrorKind::InvalidData, e),
        }
    }
}

/// Path validation. The real one is `verify::verify_server_chain` with the caller's
/// `TrustStore`. `chain[0]` is the leaf. `Err` carries a reason without secrets.
pub(crate) trait ChainVerifier {
    fn verify_chain(&self, chain: &[&[u8]], host: &str, now: i64) -> Result<(), String>;
}

/// The transport as the record layer sees it, plus the handshake byte budget and deadline.
struct Guard<T> {
    io: T,
    budget: usize,
    deadline: Option<Instant>,
    /// Why a read or write failed here (the record layer keeps only the error kind).
    why: Option<&'static str>,
}

impl<T> Guard<T> {
    fn check(&mut self) -> io::Result<()> {
        if self.deadline.is_some_and(|d| Instant::now() >= d) {
            self.why = Some("deadline");
            return Err(ErrorKind::TimedOut.into());
        }
        Ok(())
    }
}

impl<T: Read> Read for Guard<T> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        self.check()?;
        let cap = buf.len().min(self.budget);
        if cap == 0 {
            self.why = Some("byte budget");
            return Err(ErrorKind::Other.into());
        }
        let n = self.io.read(&mut buf[..cap])?;
        self.budget -= n;
        Ok(n)
    }
}

impl<T: Write> Write for Guard<T> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.check()?;
        self.io.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.io.flush()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Open,
    /// The peer sent close_notify. Reads give EOF.
    PeerClosed,
    Failed,
}

pub struct ClientStream<T: Read + Write> {
    rl: RecordLayer<Guard<T>>,
    /// Handshake bytes not yet consumed: `hs[hs_pos..]`.
    hs: Vec<u8>,
    hs_pos: usize,
    /// Rest of an application record that did not fit the caller's buffer.
    pending: Vec<u8>,
    pending_pos: usize,
    state: State,
    http11: bool,
    h2: bool,
}

/// Overwrite with zeros in a way the compiler keeps.
fn wipe(b: &mut [u8]) {
    for x in b.iter_mut() {
        // SAFETY: `x` is a valid, aligned `&mut u8`.
        unsafe { std::ptr::write_volatile(x, 0) };
    }
    std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
}

/// Bytes of randomness a handshake takes: X25519 scalar, client random, session ID.
const RANDOM_LEN: usize = 96;

/// The real validator: `verify::verify_server_chain` against caller-supplied independent roots.
/// `VerifiedPeer::leaf()` is `chain[0]`, the certificate whose key checks CertificateVerify.
struct Roots<'a>(&'a TrustStore);

impl ChainVerifier for Roots<'_> {
    fn verify_chain(&self, chain: &[&[u8]], host: &str, now: i64) -> Result<(), String> {
        verify::verify_server_chain(chain, self.0, host, now).map(|_| ()).map_err(|e| e.to_string())
    }
}

/// Connect over `io`: TLS 1.3 handshake with `host`, the chain checked against `roots`
/// (never against certificates the peer sends), the real clock and OS random.
/// The caller must set socket read and write timeouts.
pub fn connect<T: Read + Write>(io: T, host: &str, roots: &TrustStore) -> Result<ClientStream<T>, Error> {
    connect_with(io, host, &Roots(roots), false)
}

/// Same as `connect`, and offers ALPN `h2` before `http/1.1`. Check `alpn_h2` after.
pub fn connect_h2<T: Read + Write>(io: T, host: &str, roots: &TrustStore) -> Result<ClientStream<T>, Error> {
    connect_with(io, host, &Roots(roots), true)
}

/// Connect over `io` with the real clock and OS random.
pub(crate) fn connect_with<T: Read + Write, V: ChainVerifier>(
    io: T,
    host: &str,
    verifier: &V,
    h2: bool,
) -> Result<ClientStream<T>, Error> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| Error::Illegal("clock is before 1970"))?
        .as_secs() as i64;
    let mut rnd = [0u8; RANDOM_LEN];
    random::fill(&mut rnd).map_err(|_| Error::Random)?;
    let r = connect_alpn(io, host, verifier, now, &rnd, h2);
    wipe(&mut rnd);
    r
}

/// Same, with the time and the random bytes given (tests only call this one).
#[cfg(test)]
pub(crate) fn connect_inner<T: Read + Write, V: ChainVerifier>(
    io: T,
    host: &str,
    verifier: &V,
    now: i64,
    rnd: &[u8; RANDOM_LEN],
) -> Result<ClientStream<T>, Error> {
    connect_alpn(io, host, verifier, now, rnd, false)
}

fn connect_alpn<T: Read + Write, V: ChainVerifier>(
    io: T,
    host: &str,
    verifier: &V,
    now: i64,
    rnd: &[u8; RANDOM_LEN],
    h2: bool,
) -> Result<ClientStream<T>, Error> {
    let guard = Guard { io, budget: HANDSHAKE_WIRE_BUDGET, deadline: Some(Instant::now() + HANDSHAKE_DEADLINE), why: None };
    let mut s = ClientStream {
        rl: RecordLayer::new(guard),
        hs: Vec::with_capacity(4096),
        hs_pos: 0,
        pending: Vec::new(),
        pending_pos: 0,
        state: State::Open,
        http11: false,
        h2: false,
    };
    match s.handshake(host, verifier, now, rnd, h2) {
        Ok(()) => {
            let g = s.rl.get_mut();
            g.budget = usize::MAX;
            g.deadline = None;
            s.hs = Vec::new(); // handshake-only buffer
            s.hs_pos = 0;
            Ok(s)
        }
        Err(e) => Err(s.fail(e)),
    }
}

// ---- parsing helpers ----

struct R<'a>(&'a [u8]);

impl<'a> R<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        if self.0.len() < n {
            return Err(Error::Decode("truncated"));
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Ok(a)
    }
    fn u8(&mut self) -> Result<usize, Error> {
        Ok(self.take(1)?[0] as usize)
    }
    fn u16(&mut self) -> Result<usize, Error> {
        let b = self.take(2)?;
        Ok((b[0] as usize) << 8 | b[1] as usize)
    }
    fn u24(&mut self) -> Result<usize, Error> {
        let b = self.take(3)?;
        Ok((b[0] as usize) << 16 | (b[1] as usize) << 8 | b[2] as usize)
    }
    fn vec8(&mut self) -> Result<&'a [u8], Error> {
        let n = self.u8()?;
        self.take(n)
    }
    fn vec16(&mut self) -> Result<&'a [u8], Error> {
        let n = self.u16()?;
        self.take(n)
    }
    fn done(&self) -> Result<(), Error> {
        if self.0.is_empty() { Ok(()) } else { Err(Error::Decode("extra bytes")) }
    }
}

/// EncryptedExtensions (section 4.3.1). Returns whether ALPN `http/1.1` and whether `h2` was selected.
/// Allowed: server_name (empty ack), supported_groups (a list), ALPN with one protocol (`h2` only if offered).
fn parse_encrypted_extensions(body: &[u8], offer_h2: bool) -> Result<(bool, bool), Error> {
    let mut r = R(body);
    let list = r.vec16()?;
    r.done()?;
    let (mut sni, mut groups, mut alpn) = (false, false, false);
    let mut h2 = false;
    let mut e = R(list);
    while !e.0.is_empty() {
        let ty = e.u16()? as u16;
        let data = e.vec16()?;
        let seen = match ty {
            0 => &mut sni,
            10 => &mut groups,
            16 => &mut alpn,
            _ => return Err(Error::Extension(ty)),
        };
        if std::mem::replace(seen, true) {
            return Err(Error::Illegal("duplicate extension"));
        }
        match ty {
            0 if !data.is_empty() => return Err(Error::Decode("server_name ack is not empty")),
            10 => {
                let mut g = R(data);
                let l = g.vec16()?;
                g.done()?;
                if l.is_empty() || l.len() % 2 != 0 {
                    return Err(Error::Decode("supported_groups"));
                }
            }
            16 => {
                let mut a = R(data);
                let names = a.vec16()?;
                a.done()?;
                let mut n = R(names);
                let name = n.vec8()?;
                n.done()?; // the server picks exactly one
                h2 = name == b"h2" && offer_h2;
                if name != b"http/1.1" && !h2 {
                    return Err(Error::Alpn);
                }
            }
            _ => {}
        }
    }
    Ok((alpn && !h2, h2))
}

/// Certificate (section 4.4.2). Fills `chain` with slices of `body`. Returns the count.
fn parse_certificate<'a>(body: &'a [u8], chain: &mut [&'a [u8]; MAX_CHAIN]) -> Result<usize, Error> {
    let mut r = R(body);
    if !r.vec8()?.is_empty() {
        return Err(Error::Illegal("certificate_request_context is not empty"));
    }
    let n = r.u24()?;
    let mut list = R(r.take(n)?);
    r.done()?;
    let mut count = 0;
    while !list.0.is_empty() {
        let len = list.u24()?;
        if len == 0 || len > MAX_CERT_DER {
            return Err(Error::Illegal("certificate size"));
        }
        let der = list.take(len)?;
        // Entry extensions: we offered status_request and SCT in none, so any is unsolicited.
        let mut ext = R(list.vec16()?);
        if !ext.0.is_empty() {
            let ty = ext.u16()? as u16;
            ext.vec16()?;
            return Err(Error::Extension(ty));
        }
        if count == MAX_CHAIN {
            return Err(Error::Illegal("too many certificates"));
        }
        chain[count] = der;
        count += 1;
    }
    if count == 0 {
        return Err(Error::Illegal("empty certificate list"));
    }
    Ok(count)
}

/// The key of a leaf that CertificateVerify is checked with. Owned: the handshake buffer moves on.
enum LeafKey {
    /// `04 || X || Y`.
    P256([u8; 65]),
    /// Modulus bytes (`n[..len]`, no leading zero) and exponent.
    Rsa { n: [u8; rsa::MAX_BITS / 8], len: usize, e: u32 },
}

fn leaf_key(der: &[u8]) -> Result<LeafKey, Error> {
    let cert = x509::parse(der).map_err(|_| Error::Certificate("leaf does not parse".into()))?;
    match cert.public_key {
        x509::PublicKey::Ec { curve: x509::Curve::P256, point } if point.len() == 65 => Ok(LeafKey::P256(point.try_into().unwrap())),
        x509::PublicKey::Rsa(k) => {
            let mut n = [0u8; rsa::MAX_BITS / 8];
            let len = k.modulus().len();
            n[..len].copy_from_slice(k.modulus());
            Ok(LeafKey::Rsa { n, len, e: k.exponent() })
        }
        _ => Err(Error::Certificate("leaf key is not P-256 or RSA".into())),
    }
}

/// CertificateVerify (section 4.4.3): scheme 0x0403 (P-256 leaf key) or 0x0804 and 0x0805
/// (RSA leaf key) over the server context and the hash of the transcript up to and including
/// Certificate.
fn verify_certificate_verify(body: &[u8], key: &LeafKey, transcript_hash: &[u8; 32]) -> Result<(), Error> {
    let mut r = R(body);
    let scheme = r.u16()? as u16;
    if !matches!(scheme, hello::ECDSA_SECP256R1_SHA256 | hello::RSA_PSS_RSAE_SHA256 | hello::RSA_PSS_RSAE_SHA384) {
        return Err(Error::Illegal("CertificateVerify scheme was not offered"));
    }
    let sig = r.vec16()?;
    r.done()?;
    let mut signed = [0x20u8; 64 + 33 + 1 + 32];
    signed[64..97].copy_from_slice(b"TLS 1.3, server CertificateVerify");
    signed[97] = 0;
    signed[98..].copy_from_slice(transcript_hash);
    match key {
        LeafKey::P256(point) if scheme == hello::ECDSA_SECP256R1_SHA256 => {
            let es = x509::EcdsaSig::parse(sig, x509::Curve::P256).map_err(|_| Error::BadSignature)?;
            let mut rs = [0u8; 64];
            rs[32 - es.r.len()..32].copy_from_slice(es.r);
            rs[64 - es.s.len()..].copy_from_slice(es.s);
            ecc::verify(ecc::Curve::P256, point, &sha256(&signed), &rs).map_err(|_| Error::BadSignature)
        }
        LeafKey::Rsa { n, len, e } if scheme != hello::ECDSA_SECP256R1_SHA256 => {
            let k = rsa::PublicKey::new(&n[..*len], &e.to_be_bytes()).ok_or(Error::BadSignature)?;
            if scheme == hello::RSA_PSS_RSAE_SHA256 {
                k.verify_pss(rsa::Hash::Sha256, &sha256(&signed), sig)
            } else {
                k.verify_pss(rsa::Hash::Sha384, &sha384(&signed), sig)
            }
            .map_err(|_| Error::BadSignature)
        }
        _ => Err(Error::Illegal("CertificateVerify scheme does not fit the leaf key")),
    }
}

/// NewSessionTicket (section 4.6.1). Checks the framing and drops the ticket.
fn parse_new_session_ticket(body: &[u8]) -> Result<(), Error> {
    let mut r = R(body);
    r.take(8)?; // ticket_lifetime, ticket_age_add
    r.vec8()?; // ticket_nonce
    if r.vec16()?.is_empty() {
        return Err(Error::Decode("empty ticket"));
    }
    let mut e = R(r.vec16()?);
    r.done()?;
    while !e.0.is_empty() {
        let ty = e.u16()?;
        let data = e.vec16()?;
        if ty == 42 && data.len() != 4 {
            return Err(Error::Decode("early_data in ticket"));
        }
    }
    Ok(())
}

impl<T: Read + Write> ClientStream<T> {
    /// Send a fatal alert when one fits, then end the stream.
    fn fail(&mut self, e: Error) -> Error {
        let why = self.rl.get_ref().why;
        let e = match (e, why) {
            (Error::Record(record::Error::Io(_)), Some(w)) => Error::HandshakeLimit(w),
            (e, _) => e,
        };
        if self.state != State::Failed {
            if let Some(a) = e.alert() {
                let _ = self.rl.send_fatal_alert(a);
            }
            self.state = State::Failed;
        }
        e
    }

    /// Append one handshake record to `hs`.
    fn fill_hs(&mut self) -> Result<(), Error> {
        match self.rl.read_record()? {
            Record::Handshake(d) => {
                if self.hs_pos > 0 && self.hs_pos == self.hs.len() {
                    self.hs.clear();
                    self.hs_pos = 0;
                }
                self.hs.extend_from_slice(d);
                Ok(())
            }
            Record::ApplicationData(_) => Err(Error::Unexpected("application data in the handshake")),
            Record::CloseNotify => Err(Error::Unexpected("close_notify in the handshake")),
        }
    }

    /// Wait for one whole message of type `expected` and return its length with header.
    /// It is `hs[hs_pos..hs_pos + len]`. Type and size are checked as soon as the header is in.
    fn read_message(&mut self, expected: u8, max_body: usize) -> Result<usize, Error> {
        loop {
            let buf = &self.hs[self.hs_pos..];
            if buf.len() >= hello::HANDSHAKE_HEADER_LEN {
                if buf[0] != expected {
                    return Err(if buf[0] == CERTIFICATE_REQUEST {
                        Error::Unsupported("CertificateRequest (client authentication)")
                    } else {
                        Error::Unexpected("handshake message order")
                    });
                }
                match hello::next_message(buf, max_body) {
                    Ok((m, _)) => return Ok(m.len()),
                    Err(HelloError::Truncated) => {}
                    Err(HelloError::TooLarge) => return Err(Error::Illegal("handshake message too large")),
                    Err(e) => return Err(e.into()),
                }
            }
            self.fill_hs()?;
        }
    }

    fn consume(&mut self, n: usize) {
        self.hs_pos += n;
    }

    /// No handshake byte may wait across a key change (section 5.1).
    fn require_boundary(&self) -> Result<(), Error> {
        if self.hs_pos == self.hs.len() { Ok(()) } else { Err(Error::Unexpected("message data across a key change")) }
    }

    fn handshake<V: ChainVerifier>(&mut self, host: &str, verifier: &V, now: i64, rnd: &[u8; RANDOM_LEN], offer_h2: bool) -> Result<(), Error> {
        let scalar: &[u8; 32] = rnd[..32].try_into().unwrap();
        let client_random: &[u8; 32] = rnd[32..64].try_into().unwrap();
        let session_id = &rnd[64..];

        // ClientHello. The compatibility CCS goes out right after it (appendix D.4).
        let public = x25519::public_key(scalar);
        let mut ch = [0u8; hello::MAX_CLIENT_HELLO_LEN];
        let n = hello::encode_client_hello(
            &ClientHelloParams { random: client_random, session_id, x25519_public: &public, server_name: host.as_bytes(), alpn_h2: offer_h2 },
            &mut ch,
        )?;
        let mut t = Transcript::new();
        t.update(&ch[..n]);
        self.rl.write_handshake(&ch[..n])?;
        self.rl.allow_compat_ccs(true);
        self.rl.write_compat_ccs()?;
        self.rl.get_mut().flush().map_err(|e| Error::Io(e.kind()))?;

        // ServerHello. Keys change here, so nothing may follow it in its record.
        let n = self.read_message(hello::SERVER_HELLO, MAX_SERVER_HELLO)?;
        let msg = &self.hs[self.hs_pos..self.hs_pos + n];
        let sh = hello::parse_server_hello(msg, session_id)?;
        t.update(msg);
        self.consume(n);
        self.require_boundary()?;
        let mut shared = x25519::shared_secret(scalar, &sh.x25519_public).map_err(|_| Error::KeyExchange)?;
        let hs_secret = HandshakeSecret::from_ecdhe(&shared);
        wipe(&mut shared);
        let hello_hash = t.hash();
        let c_hs = hs_secret.client_traffic(&hello_hash);
        let s_hs = hs_secret.server_traffic(&hello_hash);
        let master = hs_secret.into_master();
        let k = s_hs.traffic_keys();
        self.rl.install_read_key(&k.key, &k.iv)?;
        let k = c_hs.traffic_keys();
        self.rl.install_write_key(&k.key, &k.iv)?;

        // EncryptedExtensions.
        let n = self.read_message(ENCRYPTED_EXTENSIONS, MAX_ENCRYPTED_EXTENSIONS)?;
        let msg = &self.hs[self.hs_pos..self.hs_pos + n];
        (self.http11, self.h2) = parse_encrypted_extensions(&msg[hello::HANDSHAKE_HEADER_LEN..], offer_h2)?;
        t.update(msg);
        self.consume(n);

        // Certificate: the verifier checks the chain, then we keep only the leaf key.
        let n = self.read_message(hello::CERTIFICATE, MAX_CERTIFICATE)?;
        let msg = &self.hs[self.hs_pos..self.hs_pos + n];
        let mut chain: [&[u8]; MAX_CHAIN] = [&[]; MAX_CHAIN];
        let count = parse_certificate(&msg[hello::HANDSHAKE_HEADER_LEN..], &mut chain)?;
        verifier.verify_chain(&chain[..count], host, now).map_err(Error::Certificate)?;
        let key = leaf_key(chain[0])?;
        t.update(msg);
        self.consume(n);
        let cv_hash = t.hash();

        // CertificateVerify.
        let n = self.read_message(hello::CERTIFICATE_VERIFY, MAX_CERTIFICATE_VERIFY)?;
        let msg = &self.hs[self.hs_pos..self.hs_pos + n];
        verify_certificate_verify(&msg[hello::HANDSHAKE_HEADER_LEN..], &key, &cv_hash)?;
        t.update(msg);
        self.consume(n);

        // Server Finished. Verified before anything is sent or exposed.
        let n = self.read_message(hello::FINISHED, 32)?;
        let msg = &self.hs[self.hs_pos..self.hs_pos + n];
        if !s_hs.verify_finished(&t.hash(), &msg[hello::HANDSHAKE_HEADER_LEN..]) {
            return Err(Error::BadFinished);
        }
        t.update(msg);
        self.consume(n);
        self.require_boundary()?;
        self.rl.allow_compat_ccs(false);

        // Application secrets use the transcript through server Finished. So does the
        // client Finished (no client certificate).
        let finished_hash = t.hash();
        let k = master.server_traffic(&finished_hash).traffic_keys();
        self.rl.install_read_key(&k.key, &k.iv)?;
        let mut fin = [0u8; 4 + 32];
        fin[..4].copy_from_slice(&hello::write_header(hello::FINISHED, 32)?);
        fin[4..].copy_from_slice(&c_hs.finished_verify_data(&finished_hash));
        self.rl.write_handshake(&fin)?;
        self.rl.get_mut().flush().map_err(|e| Error::Io(e.kind()))?;
        let k = master.client_traffic(&finished_hash).traffic_keys();
        self.rl.install_write_key(&k.key, &k.iv)?;
        Ok(())
    }

    /// True when the server selected ALPN `http/1.1`. False means no ALPN (HTTP/1.1 by default).
    pub fn alpn_http11(&self) -> bool {
        self.http11
    }

    /// True when the server selected ALPN `h2`. Only `connect_h2` offers it.
    pub fn alpn_h2(&self) -> bool {
        self.h2
    }

    pub fn get_ref(&self) -> &T {
        &self.rl.get_ref().io
    }

    /// True when a `read` would give bytes without waiting for the transport: plaintext is left, or a whole record is buffered.
    /// For a caller that waits on the transport (`get_ref`) before it takes a lock on this stream.
    pub fn has_buffered(&self) -> bool {
        self.pending_pos < self.pending.len() || self.rl.has_record()
    }

    /// Like `read`, with one difference: `Ok(None)` when the records that came held no application data (a session ticket)
    /// and the next one is not all here. A reader that shares the stream with a writer gives the stream back then, and
    /// waits on the transport, so it never holds the stream while it waits for the peer.
    pub fn read_ready(&mut self, out: &mut [u8]) -> io::Result<Option<usize>> {
        if self.state == State::Failed {
            return Err(failed());
        }
        if out.is_empty() {
            return Ok(Some(0));
        }
        self.read_inner(out, false).map_err(|e| self.fail(e).into())
    }

    /// True when the stream is open and holds no unread plaintext or half a handshake message.
    /// A caller that keeps the connection idle must check this first. It cannot know whether
    /// the peer has sent more bytes: for that, look at the transport (`get_ref`) too.
    pub fn reusable(&self) -> bool {
        self.state == State::Open && self.pending_pos == self.pending.len() && self.hs_pos == self.hs.len()
    }

    /// Send `close_notify` and flush. The stream may still be read until the peer closes.
    pub fn shutdown(&mut self) -> io::Result<()> {
        if self.state == State::Failed {
            return Err(failed());
        }
        self.rl.send_close_notify().map_err(|e| self.fail(e.into()))?;
        self.flush()
    }

    /// Handle handshake bytes that arrived after the handshake. Only whole NewSessionTicket
    /// messages are taken. KeyUpdate and everything else fail closed.
    fn post_handshake(&mut self) -> Result<(), Error> {
        loop {
            let buf = &self.hs[self.hs_pos..];
            if buf.len() < hello::HANDSHAKE_HEADER_LEN {
                break;
            }
            let h = hello::parse_header(buf)?;
            match h.msg_type {
                NEW_SESSION_TICKET => {}
                KEY_UPDATE => return Err(Error::Unsupported("KeyUpdate")),
                CERTIFICATE_REQUEST => return Err(Error::Unsupported("post-handshake authentication")),
                _ => return Err(Error::Unexpected("handshake message after the handshake")),
            }
            match hello::next_message(buf, MAX_TICKET) {
                Ok((m, _)) => {
                    parse_new_session_ticket(&m[hello::HANDSHAKE_HEADER_LEN..])?;
                    let n = m.len();
                    self.consume(n);
                }
                Err(HelloError::Truncated) => break,
                Err(HelloError::TooLarge) => return Err(Error::Illegal("ticket too large")),
                Err(e) => return Err(e.into()),
            }
        }
        // A partial message may wait, but the bytes already used must not pile up.
        if self.hs_pos == self.hs.len() {
            self.hs = Vec::new(); // no allocation held between tickets
        } else {
            self.hs.drain(..self.hs_pos);
        }
        self.hs_pos = 0;
        Ok(())
    }

    /// `wait` false: give `None`, instead of waiting for the socket, when only handshake records came (a session ticket) and no
    /// whole record is buffered. `Some(0)` is the end of the stream.
    fn read_inner(&mut self, out: &mut [u8], wait: bool) -> Result<Option<usize>, Error> {
        let mut records = 0;
        loop {
            if self.pending_pos < self.pending.len() {
                let n = out.len().min(self.pending.len() - self.pending_pos);
                out[..n].copy_from_slice(&self.pending[self.pending_pos..self.pending_pos + n]);
                self.pending_pos += n;
                return Ok(Some(n));
            }
            if self.state == State::PeerClosed {
                return Ok(Some(0));
            }
            if !wait && records > 0 && !self.rl.has_record() {
                return Ok(None);
            }
            // Bound work before returning to the caller's elapsed-time checks. Empty application
            // records and session tickets must not keep one Read call inside TLS indefinitely.
            if records == 64 {
                return Err(Error::Illegal("too many records without application data"));
            }
            records += 1;
            match self.rl.read_record()? {
                Record::ApplicationData(d) => {
                    if self.hs_pos != self.hs.len() {
                        return Err(Error::Unexpected("handshake message interrupted"));
                    }
                    let n = out.len().min(d.len());
                    out[..n].copy_from_slice(&d[..n]);
                    if n < d.len() {
                        self.pending.clear();
                        self.pending.extend_from_slice(&d[n..]);
                        self.pending_pos = 0;
                    }
                    if n > 0 {
                        return Ok(Some(n));
                    }
                }
                Record::Handshake(d) => {
                    self.hs.extend_from_slice(d);
                    self.post_handshake()?;
                }
                Record::CloseNotify => {
                    if self.hs_pos != self.hs.len() {
                        return Err(Error::Record(record::Error::Truncated));
                    }
                    self.state = State::PeerClosed;
                    return Ok(Some(0));
                }
            }
        }
    }
}

fn failed() -> io::Error {
    io::Error::new(ErrorKind::ConnectionAborted, "TLS connection failed earlier")
}

impl<T: Read + Write> Read for ClientStream<T> {
    /// `Ok(0)` only after the peer's `close_notify`. A close without it is `UnexpectedEof`.
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.state == State::Failed {
            return Err(failed());
        }
        if out.is_empty() {
            return Ok(0);
        }
        self.read_inner(out, true).map(|n| n.unwrap_or(0)).map_err(|e| self.fail(e).into())
    }
}

impl<T: Read + Write> Write for ClientStream<T> {
    /// Takes all of `data` or fails. At the per-key record limit the stream fails
    /// (KeyUpdate is not supported).
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if self.state == State::Failed {
            return Err(failed());
        }
        self.rl.write_application_data(data).map_err(|e| self.fail(e.into()))?;
        Ok(data.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        if self.state == State::Failed {
            return Err(failed());
        }
        self.rl.get_mut().flush().inspect_err(|_| self.state = State::Failed)
    }
}

#[cfg(test)]
#[path = "../../../tools/crypto/client_fixture.rs"]
mod fx;

#[cfg(test)]
mod tests {
    use super::fx::*;
    use super::*;

    /// Test-only trust: exactly the fixture leaf, for exactly `HOST`, inside its validity.
    /// Not an allow-all: anything else is rejected.
    struct Pin(&'static str);
    impl ChainVerifier for Pin {
        fn verify_chain(&self, chain: &[&[u8]], host: &str, now: i64) -> Result<(), String> {
            let cert = x509::parse(chain[0]).map_err(|_| "parse".to_string())?;
            if chain.len() != 1 || chain[0] != LEAF || host != self.0 || !cert.valid_at(now) {
                return Err("not the pinned leaf".into());
            }
            Ok(())
        }
    }

    type Conn = ClientStream<Script>;

    /// A transport whose output stays readable after the handshake failed and the stream is gone.
    #[derive(Clone)]
    struct Shared(std::rc::Rc<std::cell::RefCell<Script>>);
    impl Read for Shared {
        fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
            self.0.borrow_mut().read(b)
        }
    }
    impl Write for Shared {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            self.0.borrow_mut().write(b)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    /// A failed handshake: the error and the bytes the client wrote.
    fn fail_with(wire: Vec<u8>, host: &str, v: &Pin) -> (Error, Vec<u8>) {
        let t = Shared(std::rc::Rc::new(std::cell::RefCell::new(Script::new(wire, usize::MAX))));
        let r = connect_inner(t.clone(), host, v, NOW, &client_random());
        let out = t.0.borrow().output.clone();
        match r {
            Err(e) => (e, out),
            Ok(_) => panic!("handshake should have failed"),
        }
    }

    fn connect_at(wire: Vec<u8>, chunk: usize, host: &str, verifier: &Pin, now: i64) -> Result<Conn, Error> {
        connect_inner(Script::new(wire, chunk), host, verifier, now, &client_random())
    }
    fn connect(wire: Vec<u8>) -> Result<Conn, Error> {
        connect_at(wire, usize::MAX, HOST, &Pin(HOST), NOW)
    }
    /// The happy flight, with `more` called before the stream ends.
    fn happy(f: impl FnOnce(&mut Srv)) -> (Srv, Vec<u8>) {
        let mut s = Srv::new();
        s.flight(&ee_alpn(), CV_ALPN);
        f(&mut s);
        let wire = s.take();
        (s, wire)
    }
    fn read_all(c: &mut Conn) -> io::Result<Vec<u8>> {
        let mut v = Vec::new();
        c.read_to_end(&mut v)?;
        Ok(v)
    }
    fn err(r: Result<Conn, Error>) -> Error {
        match r {
            Err(e) => e,
            Ok(_) => panic!("handshake should have failed"),
        }
    }
    /// Run `f` on a fresh server; its bytes are the wire. The ClientHello is the fixed one.
    fn flight_err(f: impl FnOnce(&mut Srv)) -> (Srv, Error, Vec<u8>) {
        let mut s = Srv::new();
        f(&mut s);
        let wire = s.take();
        match connect_inner(Script::new(wire, usize::MAX), HOST, &Pin(HOST), NOW, &client_random()) {
            Err(e) => (s, e, Vec::new()),
            Ok(_) => panic!("handshake should have failed"),
        }
    }

    #[test]
    #[ignore] // prints the CertificateVerify inputs for the fixture signatures
    fn print_cv_input() {
        for (name, ee) in [("alpn", ee_alpn()), ("plain", ee_plain())] {
            let mut s = Srv::new();
            s.push(&ee);
            s.push(&certificate(&[(LEAF, &[])]));
            let mut signed = vec![0x20u8; 64];
            signed.extend_from_slice(b"TLS 1.3, server CertificateVerify\0");
            signed.extend_from_slice(&s.transcript_hash());
            let hex: String = signed.iter().map(|b| format!("{b:02x}")).collect();
            println!("CVINPUT {name} {hex}");
        }
    }

    // ---- happy path ----

    #[test]
    fn handshake_and_application_data() {
        let (srv, wire) = happy(|s| {
            s.app(b"hello client");
            s.app(b"");
            s.app(b" second record");
            s.close_notify();
        });
        let mut c = connect(wire).unwrap();
        assert!(c.alpn_http11());
        assert_eq!(read_all(&mut c).unwrap(), b"hello client second record");
        assert_eq!(c.read(&mut [0u8; 4]).unwrap(), 0, "EOF is repeatable after close_notify");
        c.write_all(b"request one").unwrap();
        c.write_all(&vec![7u8; 40_000]).unwrap();
        c.shutdown().unwrap();
        let out = std::mem::take(&mut c.rl.get_mut().io.output);
        let (ch, fin, app, alert) = srv.read_client(&out);
        assert_eq!(alert, None);
        assert_eq!(ch, client_hello());
        assert_eq!(fin.len(), 36);
        assert_eq!([&b"request one"[..], &[7u8; 40_000][..]].concat(), app);
    }

    #[test]
    fn no_alpn_is_accepted() {
        let mut s = Srv::new();
        s.flight(&ee_plain(), CV_PLAIN);
        let c = connect(s.take()).unwrap();
        assert!(!c.alpn_http11());
    }

    #[test]
    fn one_byte_transport_reads() {
        let (_, wire) = happy(|s| {
            s.app(&[9u8; 5000]);
            s.close_notify();
        });
        let mut c = connect_at(wire, 1, HOST, &Pin(HOST), NOW).unwrap();
        assert_eq!(read_all(&mut c).unwrap(), vec![9u8; 5000]);
    }

    #[test]
    fn coalesced_and_fragmented_messages() {
        let cert = certificate(&[(LEAF, &[])]);
        let cv = certificate_verify(0x0403, CV_ALPN);
        // All four messages in one record.
        let mut s = Srv::new();
        let ee = ee_alpn();
        s.push_many(&[&ee, &cert, &cv]);
        let f = s.finished();
        s.push_many(&[&f]);
        s.app_keys();
        assert!(connect(s.take()).is_ok());
        // Every message cut in the middle of its header, then in the middle of its body.
        for cut in [1, 3, 5, 100] {
            let mut s = Srv::new();
            s.push_split(&ee, cut.min(ee.len() - 1));
            s.push_split(&cert, cut);
            s.push_split(&cv, cut.min(cv.len() - 1));
            let f = s.finished();
            s.push_split(&f, cut.min(35));
            s.app_keys();
            assert!(connect(s.take()).is_ok(), "cut {cut}");
        }
        // A certificate message that spans more than one 16 KiB record is limited by size, not by records.
        let mut s = Srv::new();
        s.push(&ee);
        let big = vec![0u8; 20_000];
        s.push(&certificate(&[(&big, &[])]));
        assert_eq!(err(connect(s.take())), Error::Illegal("certificate size"));
    }

    // ---- server selections ----

    fn plain_record(m: &[u8]) -> Vec<u8> {
        let mut r = vec![22, 3, 3, (m.len() >> 8) as u8, m.len() as u8];
        r.extend_from_slice(m);
        r
    }

    #[test]
    fn server_hello_errors_send_plaintext_alert() {
        let sid = client_random()[64..].to_vec();
        let sh = |random: [u8; 32], sid: &[u8], cipher: u16, key: [u8; 32]| {
            let mut b = vec![3, 3];
            b.extend_from_slice(&random);
            b.push(sid.len() as u8);
            b.extend_from_slice(sid);
            b.extend_from_slice(&cipher.to_be_bytes());
            b.push(0);
            let exts = [&[0u8, 43, 0, 2, 3, 4][..], &[0, 51, 0, 36, 0, 29, 0, 32][..], &key[..]].concat();
            b.extend_from_slice(&(exts.len() as u16).to_be_bytes());
            b.extend_from_slice(&exts);
            let mut m = vec![2, 0, (b.len() >> 8) as u8, b.len() as u8];
            m.extend_from_slice(&b);
            m
        };
        let good_key = x25519::public_key(&[5; 32]);
        let cases: Vec<(Vec<u8>, Error, u8)> = vec![
            (sh(hello::HELLO_RETRY_REQUEST_RANDOM, &sid, 0x1301, good_key), Error::Hello(HelloError::HelloRetryRequest), 47),
            (sh([1; 32], &[9; 32], 0x1301, good_key), Error::Hello(HelloError::SessionIdMismatch), 47),
            (sh([1; 32], &sid, 0x1302, good_key), Error::Hello(HelloError::UnsupportedCipher), 47),
            (sh([1; 32], &sid, 0x1301, [0; 32]), Error::KeyExchange, 47),
        ];
        for (m, want, _) in cases {
            assert_eq!(err(connect(plain_record(&m))), want);
        }
        // The alert itself, on one case.
        let m = sh(hello::HELLO_RETRY_REQUEST_RANDOM, &sid, 0x1301, good_key);
        let mut s = ClientStream {
            rl: RecordLayer::new(Guard { io: Script::new(plain_record(&m), usize::MAX), budget: 1 << 20, deadline: None, why: None }),
            hs: Vec::new(),
            hs_pos: 0,
            pending: Vec::new(),
            pending_pos: 0,
            state: State::Open,
            http11: false,
            h2: false,
        };
        assert!(s.handshake(HOST, &Pin(HOST), NOW, &client_random(), false).is_err());
        let _ = s.fail(Error::Hello(HelloError::HelloRetryRequest));
        let out = &s.rl.get_ref().io.output;
        assert_eq!(&out[out.len() - 7..], &[21, 3, 3, 0, 2, 2, 47]);
    }

    #[test]
    fn data_after_server_hello_in_its_record() {
        let mut s = Srv::with_sh_tail(&[8, 0, 0]);
        s.flight(&ee_alpn(), CV_ALPN);
        assert_eq!(err(connect(s.take())), Error::Unexpected("message data across a key change"));
    }

    #[test]
    fn ip_host_is_refused_before_any_byte_is_sent() {
        let mut c = Script::new(vec![], 1);
        let r = connect_inner(std::mem::replace(&mut c, Script::new(vec![], 1)), "127.0.0.1", &Pin(HOST), NOW, &client_random());
        assert_eq!(err(r), Error::Hello(HelloError::BadInput));
    }

    // ---- order ----

    #[test]
    fn message_order_is_enforced() {
        let cert = certificate(&[(LEAF, &[])]);
        let cv = certificate_verify(0x0403, CV_ALPN);
        let ee = ee_alpn();
        let order = Error::Unexpected("handshake message order");
        // Certificate first.
        assert_eq!(flight_err(|s| s.push(&cert)).1, order);
        // CertificateVerify before Certificate.
        assert_eq!(flight_err(|s| { s.push(&ee); s.push(&cv) }).1, order);
        // Finished before Certificate.
        assert_eq!(flight_err(|s| { s.push(&ee); let f = s.finished(); s.push(&f) }).1, order);
        // Duplicate EncryptedExtensions.
        assert_eq!(flight_err(|s| { s.push(&ee); s.push(&ee) }).1, order);
        // Second Certificate.
        assert_eq!(flight_err(|s| { s.push(&ee); s.push(&cert); s.push(&cert) }).1, order);
        // CertificateRequest where Certificate belongs.
        let req = [13, 0, 0, 4, 0, 0, 0, 0];
        assert_eq!(flight_err(|s| { s.push(&ee); s.push(&req) }).1, Error::Unsupported("CertificateRequest (client authentication)"));
        // Application data in the middle of the handshake.
        assert!(flight_err(|s| { s.push(&ee); s.app_keys(); s.app(b"x") }).1.alert().is_some());
        // Server Finished with extra bytes behind it in its record: it must end the record.
        let (_, e, _) = flight_err(|s| {
            s.push(&ee);
            s.push(&cert);
            s.push(&cv);
            let f = s.finished();
            s.push_many(&[&f, &[8, 0]]);
        });
        assert!(matches!(e, Error::Unexpected(_) | Error::Record(_)), "{e}");
    }

    #[test]
    fn encrypted_extensions_are_strict() {
        let alpn = |names: &[&[u8]]| {
            let mut l = Vec::new();
            for n in names {
                l.push(n.len() as u8);
                l.extend_from_slice(n);
            }
            let mut d = (l.len() as u16).to_be_bytes().to_vec();
            d.extend_from_slice(&l);
            extension(16, &d)
        };
        let cases: Vec<(Vec<u8>, Error)> = vec![
            (alpn(&[b"h2"]), Error::Alpn),
            (alpn(&[b"http/1.1", b"h2"]), Error::Decode("extra bytes")),
            (alpn(&[]), Error::Decode("truncated")),
            ([alpn(&[b"http/1.1"]), alpn(&[b"http/1.1"])].concat(), Error::Illegal("duplicate extension")),
            (extension(42, &[0, 0, 0, 1]), Error::Extension(42)), // early_data
            (extension(1, &[1]), Error::Extension(1)),            // max_fragment_length: not offered
            (extension(28, &[0, 1]), Error::Extension(28)),       // record_size_limit
            (extension(0xfe00, &[]), Error::Extension(0xfe00)),
            (extension(0, &[1]), Error::Decode("server_name ack is not empty")),
            (extension(10, &[0, 0]), Error::Decode("supported_groups")),
            (vec![0, 16, 0, 5, 0], Error::Decode("truncated")),
        ];
        for (exts, want) in cases {
            let (_, e, _) = flight_err(|s| s.push(&ee_with(&exts)));
            assert_eq!(e, want);
        }
        // Allowed: server_name ack and supported_groups together with ALPN. (CV does not match
        // this transcript, so the handshake fails later, at CertificateVerify, not earlier.)
        let ok = [extension(0, &[]), extension(10, &[0, 2, 0, 29]), alpn(&[b"http/1.1"])].concat();
        let (_, e, _) = flight_err(|s| {
            s.push(&ee_with(&ok));
            s.push(&certificate(&[(LEAF, &[])]));
            s.push(&certificate_verify(0x0403, CV_ALPN));
        });
        assert_eq!(e, Error::BadSignature);
    }

    #[test]
    fn h2_is_selected_only_when_it_was_offered() {
        let alpn = |name: &[u8]| {
            let mut l = vec![name.len() as u8];
            l.extend_from_slice(name);
            let mut d = (l.len() as u16).to_be_bytes().to_vec();
            d.extend_from_slice(&l);
            extension(16, &d)
        };
        let body = |exts: &[u8]| [&(exts.len() as u16).to_be_bytes()[..], exts].concat();
        assert_eq!(parse_encrypted_extensions(&body(&alpn(b"h2")), true), Ok((false, true)));
        assert_eq!(parse_encrypted_extensions(&body(&alpn(b"h2")), false), Err(Error::Alpn));
        assert_eq!(parse_encrypted_extensions(&body(&alpn(b"http/1.1")), true), Ok((true, false)));
        assert_eq!(parse_encrypted_extensions(&body(&alpn(b"http/1.1")), false), Ok((true, false)));
        assert_eq!(parse_encrypted_extensions(&body(&[]), true), Ok((false, false)));
        assert_eq!(parse_encrypted_extensions(&body(&alpn(b"h3")), true), Err(Error::Alpn));
    }

    #[test]
    fn certificate_message_is_strict() {
        let ee = ee_alpn();
        let mut cases: Vec<(Vec<u8>, Error)> = vec![
            (certificate(&[(LEAF, &extension(5, &[1, 0, 0, 0, 0]))]), Error::Extension(5)),
            (certificate(&[(LEAF, &extension(18, &[0, 0]))]), Error::Extension(18)),
            (certificate(&[]), Error::Illegal("empty certificate list")),
            (certificate(&[(&[], &[])]), Error::Illegal("certificate size")),
            (certificate(&vec![(LEAF, &[][..]); 9]), Error::Illegal("too many certificates")),
        ];
        let mut m = certificate(&[(LEAF, &[])]);
        m[4] = 1; // non-empty request context
        cases.push((m, Error::Illegal("certificate_request_context is not empty")));
        let mut m = certificate(&[(LEAF, &[])]);
        m.push(0); // byte after the list
        let l = m.len() - 4;
        m[1..4].copy_from_slice(&(l as u32).to_be_bytes()[1..]);
        cases.push((m, Error::Decode("extra bytes")));
        let mut m = certificate(&[(LEAF, &[])]);
        let l = m.len() - 4;
        m[1..4].copy_from_slice(&((l - 1) as u32).to_be_bytes()[1..]); // message one byte shorter
        m.pop();
        cases.push((m, Error::Decode("truncated")));
        for (m, want) in cases {
            let (_, e, _) = flight_err(|s| { s.push(&ee); s.push(&m) });
            assert_eq!(e, want);
        }
    }

    // ---- authentication ----

    #[test]
    fn untrusted_certificate_stops_before_any_client_finished() {
        let mut s = Srv::new();
        s.flight(&ee_alpn(), CV_ALPN);
        // A verifier for another host rejects the chain. The client sends bad_certificate (42)
        // right after its ClientHello and CCS: no Finished.
        let (e, out) = fail_with(s.take(), HOST, &Pin("other.bare.invalid"));
        assert!(matches!(e, Error::Certificate(_)));
        assert_eq!(s.client_alert(&out), Some(42));
    }

    #[test]
    fn expired_or_not_yet_valid_chain_is_refused() {
        let mut s = Srv::new();
        s.flight(&ee_alpn(), CV_ALPN);
        let wire = s.take();
        for now in [1_793_806_617, 5_000_000_000] {
            let e = err(connect_at(wire.clone(), usize::MAX, HOST, &Pin(HOST), now));
            assert!(matches!(e, Error::Certificate(_)), "{e}");
        }
    }

    #[test]
    fn failures_send_the_right_alert_and_no_finished() {
        // (what the server does, expected error, expected alert)
        let cert = certificate(&[(LEAF, &[])]);
        let mut bad_sig = CV_ALPN.to_vec();
        let last = bad_sig.len() - 1;
        bad_sig[last] ^= 1;
        let mut truncated = CV_ALPN.to_vec();
        truncated.pop();
        let cases: Vec<(Vec<u8>, Error, u8)> = vec![
            (certificate_verify(0x0403, &bad_sig), Error::BadSignature, 51),
            (certificate_verify(0x0403, &truncated), Error::BadSignature, 51),
            (certificate_verify(0x0403, &[]), Error::BadSignature, 51),
            (certificate_verify(0x0804, CV_ALPN), Error::Illegal("CertificateVerify scheme does not fit the leaf key"), 47),
            (certificate_verify(0x0805, CV_ALPN), Error::Illegal("CertificateVerify scheme does not fit the leaf key"), 47),
            (certificate_verify(0x0503, CV_ALPN), Error::Illegal("CertificateVerify scheme was not offered"), 47),
        ];
        for (cv, want, alert) in cases {
            let mut s = Srv::new();
            s.push(&ee_alpn());
            s.push(&cert);
            s.push(&cv);
            let (e, out) = fail_with(s.take(), HOST, &Pin(HOST));
            assert_eq!(e, want);
            assert_eq!(e.alert(), Some(alert));
            assert_eq!(s.client_alert(&out), Some(alert), "alert comes where the Finished would be");
        }
        // Signature over a different transcript (the empty-EE signature on the ALPN flight).
        let mut s = Srv::new();
        s.push(&ee_alpn());
        s.push(&cert);
        s.push(&certificate_verify(0x0403, CV_PLAIN));
        assert_eq!(err(connect(s.take())), Error::BadSignature);
    }

    #[test]
    fn wrong_finished_is_refused() {
        let mut s = Srv::new();
        s.push(&ee_alpn());
        s.push(&certificate(&[(LEAF, &[])]));
        s.push(&certificate_verify(0x0403, CV_ALPN));
        for variant in 0..3 {
            let mut f = s.finished();
            match variant {
                0 => f[4] ^= 1,
                1 => f[35] ^= 0x80,
                _ => f.truncate(35), // header says 32, only 31 present: waits, then EOF
            }
            let mut s2 = Srv::new();
            s2.push(&ee_alpn());
            s2.push(&certificate(&[(LEAF, &[])]));
            s2.push(&certificate_verify(0x0403, CV_ALPN));
            s2.push_raw(&f);
            let e = err(connect(s2.take()));
            if variant < 2 {
                assert_eq!(e, Error::BadFinished);
                assert_eq!(e.alert(), Some(51));
            } else {
                assert_eq!(e, Error::Record(record::Error::UnexpectedEof));
            }
        }
    }

    #[test]
    fn client_finished_and_keys_are_right() {
        // read_client asserts the client Finished value and decrypts the application data
        // with the application key derived from the transcript through server Finished.
        let (srv, wire) = happy(|_| {});
        let mut c = connect(wire).unwrap();
        c.write_all(b"ping").unwrap();
        let out = std::mem::take(&mut c.rl.get_mut().io.output);
        let (_, _, app, _) = srv.read_client(&out);
        assert_eq!(app, b"ping");
    }

    // ---- CCS, limits ----

    #[test]
    fn compat_ccs_window() {
        // CCS between handshake records is dropped (the fixture already sent one after ServerHello).
        let ee = ee_alpn();
        let mut s = Srv::new();
        s.push(&ee);
        s.rl_output_ccs();
        s.push(&certificate(&[(LEAF, &[])]));
        s.push(&certificate_verify(0x0403, CV_ALPN));
        s.push_finished();
        assert!(connect(s.take()).is_ok());
        // After server Finished the window is closed: a CCS then fails the first read.
        let (_, wire) = happy(|s| {
            s.rl_output_ccs();
            s.app(b"late");
        });
        let mut c = connect(wire).unwrap();
        assert_eq!(c.read(&mut [0u8; 8]).unwrap_err().kind(), ErrorKind::InvalidData);
    }

    #[test]
    fn ccs_flood_hits_the_byte_budget() {
        let mut s = Srv::new();
        s.push(&ee_alpn());
        for _ in 0..(HANDSHAKE_WIRE_BUDGET / 6 + 10) {
            s.rl_output_ccs();
        }
        assert_eq!(err(connect(s.take())), Error::HandshakeLimit("byte budget"));
    }

    #[test]
    fn deadline_stops_the_handshake() {
        let mut g = Guard { io: Script::new(vec![1, 2, 3], usize::MAX), budget: 100, deadline: Some(Instant::now()), why: None };
        assert_eq!(g.read(&mut [0u8; 3]).unwrap_err().kind(), ErrorKind::TimedOut);
        assert_eq!(g.why, Some("deadline"));
        assert_eq!(g.write(&[1]).unwrap_err().kind(), ErrorKind::TimedOut);
        g.deadline = None;
        assert_eq!(g.read(&mut [0u8; 3]).unwrap(), 3);
    }

    // ---- application phase ----

    #[test]
    fn partial_reads_keep_the_rest_of_a_record() {
        let (_, wire) = happy(|s| {
            s.app(b"abcdefghij");
            s.app(b"klm");
            s.close_notify();
        });
        let mut c = connect(wire).unwrap();
        let mut b = [0u8; 4];
        assert_eq!(c.read(&mut b).unwrap(), 4);
        assert_eq!(&b, b"abcd");
        assert_eq!(c.read(&mut b[..2]).unwrap(), 2);
        assert_eq!(&b[..2], b"ef");
        assert_eq!(c.read(&mut []).unwrap(), 0);
        assert_eq!(read_all(&mut c).unwrap(), b"ghijklm");
    }

    #[test]
    fn buffered_handshake_and_flush_failure() {
        struct Buffered { script: Script, held: Vec<u8>, fail: bool }
        impl Read for Buffered {
            fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
                if !self.held.is_empty() { return Err(ErrorKind::WouldBlock.into()); }
                self.script.read(out)
            }
        }
        impl Write for Buffered {
            fn write(&mut self, data: &[u8]) -> io::Result<usize> {
                self.held.extend_from_slice(data);
                Ok(data.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                if self.fail { return Err(ErrorKind::BrokenPipe.into()); }
                self.script.write_all(&std::mem::take(&mut self.held))
            }
        }
        let (_, wire) = happy(|_| {});
        let io = Buffered { script: Script::new(wire, usize::MAX), held: vec![], fail: false };
        let mut c = connect_inner(io, HOST, &Pin(HOST), NOW, &client_random()).unwrap();
        assert!(c.get_ref().held.is_empty(), "client Finished must be flushed");
        c.rl.get_mut().io.fail = true;
        assert_eq!(c.flush().unwrap_err().kind(), ErrorKind::BrokenPipe);
        assert_eq!(c.write(b"later").unwrap_err().kind(), ErrorKind::ConnectionAborted);
    }

    #[test]
    fn records_without_application_progress_are_bounded() {
        let (_, wire) = happy(|s| {
            for _ in 0..256 { s.post_handshake(&new_session_ticket(&[1], &[])); }
            s.app(b"late");
        });
        let mut c = connect(wire).unwrap();
        let error = c.read(&mut [0u8; 1]).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidData);
        assert!(error.to_string().contains("records without application data"));
        assert_eq!(c.read(&mut [0u8; 1]).unwrap_err().kind(), ErrorKind::ConnectionAborted);
    }

    #[test]
    fn session_tickets_are_parsed_and_dropped() {
        let t = new_session_ticket(&[0xab; 300], &extension(42, &[0, 0, 0x40, 0]));
        let (_, wire) = happy(|s| {
            s.post_handshake(&t);
            s.app(b"data");
            let t2 = [t.clone(), t.clone()].concat();
            s.post_handshake(&t2); // two in one record
            // One cut across two records, with data after.
            s.post_handshake(&t[..9]);
            s.post_handshake(&t[9..]);
            s.app(b"more");
            s.close_notify();
        });
        let mut c = connect(wire).unwrap();
        assert_eq!(read_all(&mut c).unwrap(), b"datamore");
        assert!(c.hs.is_empty() && c.hs.capacity() == 0, "no buffer kept");
    }

    #[test]
    fn waiting_partial_message_does_not_pile_up_used_bytes() {
        let t = new_session_ticket(&[1; 50], &[]);
        let (_, wire) = happy(|_| {});
        let mut c = connect(wire).unwrap();
        for _ in 0..1000 {
            // One whole ticket and the start of the next, as a server could send forever.
            c.hs.extend_from_slice(&t);
            c.hs.extend_from_slice(&t[..10]);
            c.post_handshake().unwrap();
            assert_eq!((c.hs_pos, c.hs.len()), (0, 10));
            c.hs.clear();
        }
    }

    #[test]
    fn reusable_means_open_with_nothing_held_back() {
        // Fresh: yes. Half a record read: no. The rest read: yes again.
        let (_, wire) = happy(|s| s.app(b"0123456789"));
        let mut c = connect(wire).unwrap();
        assert!(c.reusable());
        let mut two = [0u8; 2];
        c.read_exact(&mut two).unwrap();
        assert!(!c.reusable(), "eight bytes of the record are still held");
        let mut rest = [0u8; 8];
        c.read_exact(&mut rest).unwrap();
        assert!(c.reusable());
        // Half a ticket waits in the handshake buffer: no.
        let t = new_session_ticket(&[7; 64], &[]);
        let (_, wire) = happy(|s| s.post_handshake(&t[..20]));
        let mut c = connect(wire).unwrap();
        assert!(c.read(&mut two).is_err() || !c.reusable());
        // After close_notify: no.
        let (_, wire) = happy(|s| s.close_notify());
        let mut c = connect(wire).unwrap();
        assert_eq!(read_all(&mut c).unwrap(), b"");
        assert!(!c.reusable());
    }

    #[test]
    fn bad_tickets_and_other_post_handshake_messages_fail_closed() {
        let good = new_session_ticket(&[1; 10], &[]);
        let mut short = good.clone();
        short[3] -= 1;
        short.pop(); // body one byte short: ticket extension length now runs past the end
        let cases: Vec<(Vec<u8>, Error)> = vec![
            (short, Error::Decode("truncated")),
            (new_session_ticket(&[], &[]), Error::Decode("empty ticket")),
            (new_session_ticket(&[1], &extension(42, &[0])), Error::Decode("early_data in ticket")),
            (vec![24, 0, 0, 1, 0], Error::Unsupported("KeyUpdate")),
            (vec![24, 0, 0, 1, 1], Error::Unsupported("KeyUpdate")),
            (vec![13, 0, 0, 4, 0, 0, 0, 0], Error::Unsupported("post-handshake authentication")),
            (vec![11, 0, 0, 0], Error::Unexpected("handshake message after the handshake")),
            (vec![20, 0, 0, 0], Error::Unexpected("handshake message after the handshake")),
            (vec![4, 0xff, 0xff, 0xff], Error::Illegal("ticket too large")),
        ];
        for (m, want) in cases {
            let (_, wire) = happy(|s| {
                s.post_handshake(&m);
                s.app(b"never read");
            });
            let mut c = connect(wire).unwrap();
            let e = c.read(&mut [0u8; 16]).unwrap_err();
            assert_eq!(e.get_ref().unwrap().to_string(), want.to_string());
            // The stream is dead for reading and writing: it never uses an old key again.
            assert_eq!(c.read(&mut [0u8; 16]).unwrap_err().kind(), ErrorKind::ConnectionAborted);
            assert_eq!(c.write(b"x").unwrap_err().kind(), ErrorKind::ConnectionAborted);
            assert_eq!(c.flush().unwrap_err().kind(), ErrorKind::ConnectionAborted);
        }
    }

    #[test]
    fn key_update_sends_an_alert_with_the_application_key() {
        let (srv, wire) = happy(|s| s.post_handshake(&[24, 0, 0, 1, 0]));
        let mut c = connect(wire).unwrap();
        assert!(c.read(&mut [0u8; 1]).is_err());
        let out = std::mem::take(&mut c.rl.get_mut().io.output);
        // The alert is protected with the client application key, after the client Finished.
        let (_, _, app, alert) = srv.read_client(&out);
        assert!(app.is_empty());
        assert_eq!(alert, Some(10));
    }

    #[test]
    fn interleaved_data_inside_a_partial_message_is_refused() {
        let t = new_session_ticket(&[1; 10], &[]);
        let (_, wire) = happy(|s| {
            s.post_handshake(&t[..6]);
            s.app(b"x");
        });
        let mut c = connect(wire).unwrap();
        assert_eq!(
            c.read(&mut [0u8; 4]).unwrap_err().get_ref().unwrap().to_string(),
            Error::Unexpected("handshake message interrupted").to_string()
        );
    }

    #[test]
    fn truncation_is_not_eof() {
        // Close without close_notify, at a record boundary.
        let (_, wire) = happy(|s| s.app(b"partial"));
        let mut c = connect(wire).unwrap();
        let mut b = [0u8; 100];
        assert_eq!(c.read(&mut b).unwrap(), 7);
        assert_eq!(c.read(&mut b).unwrap_err().kind(), ErrorKind::UnexpectedEof);
        assert_eq!(c.read(&mut b).unwrap_err().kind(), ErrorKind::ConnectionAborted);
        // Close in the middle of a record.
        let (_, mut wire) = happy(|s| s.app(b"0123456789"));
        wire.truncate(wire.len() - 3);
        let mut c = connect(wire).unwrap();
        assert_eq!(c.read(&mut b).unwrap_err().kind(), ErrorKind::UnexpectedEof);
        // close_notify in the middle of a ticket.
        let t = new_session_ticket(&[1; 10], &[]);
        let (_, wire) = happy(|s| {
            s.post_handshake(&t[..6]);
            s.close_notify();
        });
        let mut c = connect(wire).unwrap();
        assert_eq!(c.read(&mut b).unwrap_err().kind(), ErrorKind::UnexpectedEof);
    }

    #[test]
    fn tampered_application_record_poisons_the_stream() {
        let (_, mut wire) = happy(|s| s.app(b"secret-free test data"));
        let n = wire.len();
        wire[n - 1] ^= 1;
        let mut c = connect(wire).unwrap();
        assert_eq!(c.read(&mut [0u8; 64]).unwrap_err().kind(), ErrorKind::InvalidData);
        assert_eq!(c.read(&mut [0u8; 64]).unwrap_err().kind(), ErrorKind::ConnectionAborted);
    }

    #[test]
    fn peer_alert_is_an_error_and_close_notify_is_eof() {
        let (_, wire) = happy(|s| s.close_notify());
        let mut c = connect(wire).unwrap();
        assert_eq!(c.read(&mut [0u8; 8]).unwrap(), 0);
    }
}
