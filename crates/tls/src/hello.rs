//! TLS 1.3 ClientHello encoder, ServerHello parser and handshake message framing
//! (RFC 8446 §4, §4.1.2, §4.1.3, §4.2). Narrow on purpose.
//!
//! We offer one of everything, except signatures:
//! - version TLS 1.3 only (`supported_versions` = 0x0304; legacy_version 0x0303);
//! - cipher suite `TLS_AES_128_GCM_SHA256` (0x1301);
//! - groups X25519 (0x001d) and secp256r1 (0x0017), with one key share, for X25519. secp256r1 serves TLS 1.2 only;
//!   a TLS 1.3 server that insists on it answers with a HelloRetryRequest, which we reject;
//! - `signature_algorithms` = `ecdsa_secp256r1_sha256` (0x0403), `ecdsa_secp384r1_sha384` (0x0503),
//!   `rsa_pss_rsae_sha256` (0x0804) and `rsa_pss_rsae_sha384` (0x0805). This limits only the server's
//!   `CertificateVerify` (P-256 and P-384 leaf keys use ECDSA; RSA leaf keys of 2048 to 4096 bits use the PSS schemes);
//! - `signature_algorithms_cert` = 0x0403, `ecdsa_secp384r1_sha384` (0x0503),
//!   `rsa_pkcs1_sha256` (0x0401) and `rsa_pkcs1_sha384` (0x0501). This limits only signatures
//!   inside certificates. The staging chain is signed with P-384/SHA-384; public RSA chains are
//!   signed with PKCS#1 v1.5 (RFC 8446 §4.2.3: when this extension is present,
//!   `signature_algorithms` does not apply to certificates);
//! - SNI host name and ALPN `http/1.1`, with `h2` before it when the caller offers it.
//!
//! The same ClientHello also offers TLS 1.2 (RFC 5246): `supported_versions` lists 1.3 and 1.2, the cipher list adds
//! `TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256` (0xC02B) and `TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256` (0xC02F), and
//! `signature_algorithms` adds `rsa_pkcs1_sha256` and `rsa_pkcs1_sha384` (TLS 1.2 ServerKeyExchange may use them).
//! The hello also sends `extended_master_secret` (RFC 7627) and an empty `renegotiation_info` (RFC 5746).
//! `is_tls12_server_hello` tells the two apart and `parse_server_hello12` reads the 1.2 one.
//!
//! No PSK, no early data, no compression, no renegotiation, no session resumption.
//! `HelloRetryRequest` is detected and rejected with `HelloError::HelloRetryRequest`.
//! The caller must then abort. Continuing would need a different transcript (RFC 8446 §4.4.1)
//! and a second ClientHello. This code does neither.

use std::fmt;
use std::net::IpAddr;

pub const HANDSHAKE_HEADER_LEN: usize = 4;
/// Handshake message types (RFC 8446 §4).
pub const CLIENT_HELLO: u8 = 1;
pub const SERVER_HELLO: u8 = 2;
pub const ENCRYPTED_EXTENSIONS: u8 = 8;
pub const CERTIFICATE: u8 = 11;
pub const CERTIFICATE_VERIFY: u8 = 15;
pub const SERVER_KEY_EXCHANGE: u8 = 12;
pub const SERVER_HELLO_DONE: u8 = 14;
pub const CLIENT_KEY_EXCHANGE: u8 = 16;
pub const FINISHED: u8 = 20;
/// The largest body a `uint24` length can describe.
pub const MAX_BODY_LEN: usize = 0xFF_FFFF;

pub const LEGACY_VERSION: u16 = 0x0303;
pub const TLS13: u16 = 0x0304;
pub const TLS_AES_128_GCM_SHA256: u16 = 0x1301;
pub const TLS_ECDHE_ECDSA_AES_128_GCM_SHA256: u16 = 0xC02B;
pub const TLS_ECDHE_RSA_AES_128_GCM_SHA256: u16 = 0xC02F;
pub const GROUP_X25519: u16 = 0x001d;
pub const GROUP_SECP256R1: u16 = 0x0017;
pub const ECDSA_SECP256R1_SHA256: u16 = 0x0403;
pub const ECDSA_SECP384R1_SHA384: u16 = 0x0503;
pub const RSA_PSS_RSAE_SHA256: u16 = 0x0804;
pub const RSA_PSS_RSAE_SHA384: u16 = 0x0805;
pub const RSA_PKCS1_SHA256: u16 = 0x0401;
pub const RSA_PKCS1_SHA384: u16 = 0x0501;
pub const MAX_SESSION_ID_LEN: usize = 32;
pub const MAX_SERVER_NAME_LEN: usize = 253;
/// Size of the longest ClientHello we can write (32-byte session ID, 253-byte name).
pub const MAX_CLIENT_HELLO_LEN: usize = 465;

/// SHA-256("HelloRetryRequest"). A ServerHello with this random is a HelloRetryRequest.
pub const HELLO_RETRY_REQUEST_RANDOM: [u8; 32] = [
    0xCF, 0x21, 0xAD, 0x74, 0xE5, 0x9A, 0x61, 0x11, 0xBE, 0x1D, 0x8C, 0x02, 0x1E, 0x65, 0xB8, 0x91, 0xC2, 0xA2, 0x11, 0x16, 0x7A, 0xBB,
    0x8C, 0x5E, 0x07, 0x9E, 0x09, 0xE2, 0xC8, 0xA8, 0x33, 0x9C,
];

/// The last 8 bytes of a TLS 1.2 ServerHello random from a server that supports 1.3 (RFC 8446 §4.1.3).
pub const DOWNGRADE_TLS12: [u8; 8] = *b"DOWNGRD\x01";

const EXT_SERVER_NAME: u16 = 0;
const EXT_EC_POINT_FORMATS: u16 = 11;
const EXT_EXTENDED_MASTER_SECRET: u16 = 23;
const EXT_RENEGOTIATION_INFO: u16 = 0xff01;
const EXT_SUPPORTED_GROUPS: u16 = 10;
const EXT_ALPN: u16 = 16;
const EXT_SIGNATURE_ALGORITHMS: u16 = 13;
const EXT_SUPPORTED_VERSIONS: u16 = 43;
const EXT_KEY_SHARE: u16 = 51;
const EXT_SIGNATURE_ALGORITHMS_CERT: u16 = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelloError {
    /// Fewer bytes than the structure needs.
    Truncated,
    /// Declared body is longer than the caller's limit.
    TooLarge,
    /// A length field does not fit: extra bytes, inner length past outer, or a body over 2^24-1.
    BadLength,
    /// The output buffer is too small.
    BufferTooSmall,
    /// Bad caller input: session ID longer than 32 bytes, or an invalid server name.
    BadInput,
    /// The message type is not the expected one.
    BadMessageType,
    BadLegacyVersion,
    /// The server sent a HelloRetryRequest. Not supported. Abort the handshake.
    HelloRetryRequest,
    SessionIdMismatch,
    /// The server chose a cipher suite we did not offer.
    UnsupportedCipher,
    /// Compression method is not 0.
    BadCompression,
    /// The same extension appears twice.
    DuplicateExtension(u16),
    /// The extension is one we did not offer (or is not allowed in a ServerHello).
    UnsolicitedExtension(u16),
    /// `supported_versions` or `key_share` is missing.
    MissingExtension(u16),
    /// The server chose a version other than TLS 1.3.
    UnsupportedVersion,
    /// The key share is not X25519 with 32 bytes.
    BadKeyShare,
    /// A TLS 1.2 ServerHello carries the downgrade marker: a server that supports 1.3 answered a 1.3 offer with 1.2.
    Downgrade,
    /// A TLS 1.2 ServerHello echoes our session ID. That means resumption, and we never offer it.
    Resumption,
}

impl fmt::Display for HelloError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HelloError::DuplicateExtension(t) => write!(f, "duplicate extension {t}"),
            HelloError::UnsolicitedExtension(t) => write!(f, "unsolicited extension {t}"),
            HelloError::MissingExtension(t) => write!(f, "missing extension {t}"),
            e => f.write_str(match e {
                HelloError::Truncated => "truncated handshake message",
                HelloError::TooLarge => "handshake message too large",
                HelloError::BadLength => "bad length field",
                HelloError::BufferTooSmall => "output buffer too small",
                HelloError::BadInput => "bad ClientHello input",
                HelloError::BadMessageType => "unexpected handshake message type",
                HelloError::BadLegacyVersion => "legacy_version is not 0x0303",
                HelloError::HelloRetryRequest => "HelloRetryRequest is not supported",
                HelloError::SessionIdMismatch => "ServerHello session ID does not match",
                HelloError::UnsupportedCipher => "server chose a cipher suite we did not offer",
                HelloError::BadCompression => "compression method is not null",
                HelloError::UnsupportedVersion => "server did not select TLS 1.3",
                HelloError::BadKeyShare => "key share is not a 32-byte X25519 share",
                HelloError::Downgrade => "TLS 1.2 ServerHello carries the downgrade marker",
                HelloError::Resumption => "TLS 1.2 ServerHello resumes a session we never offered",
                _ => unreachable!(),
            }),
        }
    }
}
impl std::error::Error for HelloError {}

// ---------------------------------------------------------------------------------------
// Handshake message framing: `uint8 type, uint24 length, body` (RFC 8446 §4).

/// A parsed handshake header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub msg_type: u8,
    pub body_len: usize,
}

/// Read the 4-byte header. `Truncated` if `buf` has fewer than 4 bytes.
pub fn parse_header(buf: &[u8]) -> Result<Header, HelloError> {
    match buf {
        [t, a, b, c, ..] => Ok(Header { msg_type: *t, body_len: (*a as usize) << 16 | (*b as usize) << 8 | *c as usize }),
        _ => Err(HelloError::Truncated),
    }
}

/// Write a 4-byte header. `BadLength` if `body_len` does not fit in 24 bits.
pub fn write_header(msg_type: u8, body_len: usize) -> Result<[u8; HANDSHAKE_HEADER_LEN], HelloError> {
    if body_len > MAX_BODY_LEN {
        return Err(HelloError::BadLength);
    }
    Ok([msg_type, (body_len >> 16) as u8, (body_len >> 8) as u8, body_len as u8])
}

/// Split the first complete message off `buf`. Returns `(message, rest)`. `message` includes
/// its header: these are the exact bytes for the transcript hash. A declared body over
/// `max_body` is `TooLarge` at once, before any wait for more bytes. An incomplete message
/// is `Truncated`; the caller should read more and retry.
pub fn next_message(buf: &[u8], max_body: usize) -> Result<(&[u8], &[u8]), HelloError> {
    let h = parse_header(buf)?;
    if h.body_len > max_body {
        return Err(HelloError::TooLarge);
    }
    let total = HANDSHAKE_HEADER_LEN + h.body_len; // <= 4 + 2^24, no overflow on 32-bit or more
    if buf.len() < total {
        return Err(HelloError::Truncated);
    }
    Ok(buf.split_at(total))
}

// ---------------------------------------------------------------------------------------
// Writer into a caller-owned buffer.

struct Writer<'a> {
    buf: &'a mut [u8],
    n: usize,
}

impl Writer<'_> {
    fn put(&mut self, d: &[u8]) -> Result<(), HelloError> {
        let end = self.n.checked_add(d.len()).filter(|&e| e <= self.buf.len()).ok_or(HelloError::BufferTooSmall)?;
        self.buf[self.n..end].copy_from_slice(d);
        self.n = end;
        Ok(())
    }
    fn u8(&mut self, v: u8) -> Result<(), HelloError> {
        self.put(&[v])
    }
    fn u16(&mut self, v: u16) -> Result<(), HelloError> {
        self.put(&v.to_be_bytes())
    }
    /// Reserve `width` length bytes. Returns the position to give to `close`.
    fn open(&mut self, width: usize) -> Result<usize, HelloError> {
        let at = self.n;
        self.put(&[0u8; 3][..width])?;
        Ok(at)
    }
    /// Fill in the length that `open(width)` reserved at `at`.
    fn close(&mut self, at: usize, width: usize) -> Result<(), HelloError> {
        let len = self.n - at - width;
        if len >> (8 * width) != 0 {
            return Err(HelloError::BadLength);
        }
        for i in 0..width {
            self.buf[at + i] = (len >> (8 * (width - 1 - i))) as u8;
        }
        Ok(())
    }
    fn ext(&mut self, ty: u16, f: impl FnOnce(&mut Self) -> Result<(), HelloError>) -> Result<(), HelloError> {
        self.u16(ty)?;
        let at = self.open(2)?;
        f(self)?;
        self.close(at, 2)
    }
}

// ---------------------------------------------------------------------------------------
// ClientHello

/// Caller-supplied fields. Nothing is generated here, so tests are deterministic.
pub struct ClientHelloParams<'a> {
    /// 32 random bytes from `random::fill`.
    pub random: &'a [u8; 32],
    /// 0 to 32 bytes. The server must echo them. Empty is fine for TLS 1.3 alone;
    /// 32 random bytes make some middleboxes happier (RFC 8446 Appendix D.4).
    pub session_id: &'a [u8],
    /// The X25519 public key for the key share.
    pub x25519_public: &'a [u8; 32],
    /// DNS host name, 1 to 253 ASCII bytes, no trailing dot, not an IP address (RFC 6066 §3).
    pub server_name: &'a [u8],
    /// Offer `h2` before `http/1.1`.
    pub alpn_h2: bool,
}

fn valid_server_name(n: &[u8]) -> bool {
    !n.is_empty()
        && n.len() <= MAX_SERVER_NAME_LEN
        && n.split(|&b| b == b'.').all(|label| {
            (1..=63).contains(&label.len())
                && label.iter().all(|b| b.is_ascii_alphanumeric() || *b == b'-')
                && !label.starts_with(b"-") && !label.ends_with(b"-")
        })
        && std::str::from_utf8(n).ok().and_then(|s| s.parse::<IpAddr>().ok()).is_none()
}

/// Write the whole ClientHello handshake message (header included) into `out`.
/// Returns its length. These bytes go into the transcript and onto the wire unchanged.
/// A buffer of `MAX_CLIENT_HELLO_LEN` bytes always suffices.
pub fn encode_client_hello(p: &ClientHelloParams, out: &mut [u8]) -> Result<usize, HelloError> {
    if p.session_id.len() > MAX_SESSION_ID_LEN || !valid_server_name(p.server_name) {
        return Err(HelloError::BadInput);
    }
    let mut w = Writer { buf: out, n: 0 };
    w.u8(CLIENT_HELLO)?;
    let msg = w.open(3)?;
    w.u16(LEGACY_VERSION)?;
    w.put(p.random)?;
    w.u8(p.session_id.len() as u8)?;
    w.put(p.session_id)?;
    w.u16(6)?; // three cipher suites: the TLS 1.3 one, then the two TLS 1.2 ones
    w.u16(TLS_AES_128_GCM_SHA256)?;
    w.u16(TLS_ECDHE_ECDSA_AES_128_GCM_SHA256)?;
    w.u16(TLS_ECDHE_RSA_AES_128_GCM_SHA256)?;
    w.put(&[1, 0])?; // one compression method: null
    let exts = w.open(2)?;
    w.ext(EXT_SERVER_NAME, |w| {
        let list = w.open(2)?;
        w.u8(0)?; // name_type host_name
        w.u16(p.server_name.len() as u16)?;
        w.put(p.server_name)?;
        w.close(list, 2)
    })?;
    w.ext(EXT_SUPPORTED_GROUPS, |w| {
        w.u16(4)?;
        w.u16(GROUP_X25519)?;
        w.u16(GROUP_SECP256R1) // TLS 1.2 only: ECDSA P-256 certificates and ECDHE need it. The key share stays X25519.
    })?;
    w.ext(EXT_SIGNATURE_ALGORITHMS, |w| {
        w.u16(12)?;
        w.u16(ECDSA_SECP256R1_SHA256)?;
        w.u16(ECDSA_SECP384R1_SHA384)?;
        w.u16(RSA_PSS_RSAE_SHA256)?;
        w.u16(RSA_PSS_RSAE_SHA384)?;
        w.u16(RSA_PKCS1_SHA256)?; // TLS 1.2 only: a 1.3 CertificateVerify never uses it
        w.u16(RSA_PKCS1_SHA384)
    })?;
    w.ext(EXT_SIGNATURE_ALGORITHMS_CERT, |w| {
        w.u16(8)?;
        w.u16(ECDSA_SECP256R1_SHA256)?;
        w.u16(ECDSA_SECP384R1_SHA384)?;
        w.u16(RSA_PKCS1_SHA256)?;
        w.u16(RSA_PKCS1_SHA384)
    })?;
    w.ext(EXT_SUPPORTED_VERSIONS, |w| {
        w.u8(4)?;
        w.u16(TLS13)?;
        w.u16(LEGACY_VERSION)
    })?;
    w.ext(EXT_KEY_SHARE, |w| {
        let list = w.open(2)?;
        w.u16(GROUP_X25519)?;
        w.u16(32)?;
        w.put(p.x25519_public)?;
        w.close(list, 2)
    })?;
    w.ext(EXT_ALPN, |w| {
        let list = w.open(2)?;
        if p.alpn_h2 {
            w.u8(2)?;
            w.put(b"h2")?;
        }
        w.u8(8)?;
        w.put(b"http/1.1")?;
        w.close(list, 2)
    })?;
    w.ext(EXT_EXTENDED_MASTER_SECRET, |_| Ok(()))?;
    w.ext(EXT_RENEGOTIATION_INFO, |w| w.u8(0))?;
    w.close(exts, 2)?;
    w.close(msg, 3)?;
    Ok(w.n)
}

// ---------------------------------------------------------------------------------------
// ServerHello

/// What the handshake needs from a valid ServerHello.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerHello {
    /// The server's X25519 public key. Pass it to `x25519::shared_secret`.
    pub x25519_public: [u8; 32],
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], HelloError> {
        if self.0.len() < n {
            return Err(HelloError::Truncated);
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Ok(a)
    }
    fn u8(&mut self) -> Result<u8, HelloError> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, HelloError> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }
}

/// Validate a ServerHello against our offer. `msg` is the whole handshake message with its
/// header, exactly as framed by `next_message`. `offered_session_id` is what we sent.
///
/// Accepts only: type 2, legacy_version 0x0303, echoed session ID, cipher 0x1301,
/// compression 0, and exactly the extensions `supported_versions` (= TLS 1.3) and `key_share`
/// (X25519, 32 bytes), each once. Everything else is an error. A HelloRetryRequest random gives
/// `HelloError::HelloRetryRequest` before any extension is read.
pub fn parse_server_hello(msg: &[u8], offered_session_id: &[u8]) -> Result<ServerHello, HelloError> {
    let h = parse_header(msg)?;
    if h.msg_type != SERVER_HELLO {
        return Err(HelloError::BadMessageType);
    }
    let body = &msg[HANDSHAKE_HEADER_LEN..];
    if body.len() < h.body_len {
        return Err(HelloError::Truncated);
    }
    if body.len() > h.body_len {
        return Err(HelloError::BadLength);
    }
    let mut r = Reader(body);
    if r.u16()? != LEGACY_VERSION {
        return Err(HelloError::BadLegacyVersion);
    }
    if r.take(32)? == HELLO_RETRY_REQUEST_RANDOM {
        return Err(HelloError::HelloRetryRequest);
    }
    let sid_len = r.u8()? as usize;
    if sid_len > MAX_SESSION_ID_LEN {
        return Err(HelloError::BadLength);
    }
    if r.take(sid_len)? != offered_session_id {
        return Err(HelloError::SessionIdMismatch);
    }
    if r.u16()? != TLS_AES_128_GCM_SHA256 {
        return Err(HelloError::UnsupportedCipher);
    }
    if r.u8()? != 0 {
        return Err(HelloError::BadCompression);
    }
    let ext_len = r.u16()? as usize;
    if r.0.len() != ext_len {
        return Err(if r.0.len() < ext_len { HelloError::Truncated } else { HelloError::BadLength });
    }
    let (mut version_seen, mut share) = (false, None);
    while !r.0.is_empty() {
        let ty = r.u16()?;
        let len = r.u16()? as usize;
        let mut d = Reader(r.take(len)?);
        match ty {
            EXT_SUPPORTED_VERSIONS => {
                if std::mem::replace(&mut version_seen, true) {
                    return Err(HelloError::DuplicateExtension(ty));
                }
                if len != 2 {
                    return Err(HelloError::BadLength);
                }
                if d.u16()? != TLS13 {
                    return Err(HelloError::UnsupportedVersion);
                }
            }
            EXT_KEY_SHARE => {
                if share.is_some() {
                    return Err(HelloError::DuplicateExtension(ty));
                }
                if d.u16()? != GROUP_X25519 {
                    return Err(HelloError::BadKeyShare);
                }
                let klen = d.u16()? as usize;
                if klen != 32 || d.0.len() != 32 {
                    return Err(HelloError::BadKeyShare);
                }
                share = Some(<[u8; 32]>::try_from(d.0).unwrap());
            }
            _ => return Err(HelloError::UnsolicitedExtension(ty)),
        }
    }
    if !version_seen {
        return Err(HelloError::MissingExtension(EXT_SUPPORTED_VERSIONS));
    }
    Ok(ServerHello { x25519_public: share.ok_or(HelloError::MissingExtension(EXT_KEY_SHARE))? })
}

/// True when `msg` (a whole ServerHello) answers with TLS 1.2: no `supported_versions` extension and no HelloRetryRequest
/// random. A message that does not parse gives false. The TLS 1.3 parser then reports the error.
pub fn is_tls12_server_hello(msg: &[u8]) -> bool {
    fn scan(body: &[u8]) -> Result<bool, HelloError> {
        let mut r = Reader(body);
        r.take(2)?; // legacy_version
        if r.take(32)? == HELLO_RETRY_REQUEST_RANDOM {
            return Ok(false);
        }
        let sid = r.u8()? as usize;
        r.take(sid + 3)?; // session ID, cipher suite, compression
        if r.0.is_empty() {
            return Ok(true); // a TLS 1.2 ServerHello may have no extensions
        }
        let len = r.u16()? as usize;
        let mut e = Reader(r.take(len)?);
        while !e.0.is_empty() {
            let ty = e.u16()?;
            let l = e.u16()? as usize;
            e.take(l)?;
            if ty == EXT_SUPPORTED_VERSIONS {
                return Ok(false);
            }
        }
        Ok(true)
    }
    msg.get(HANDSHAKE_HEADER_LEN..).is_some_and(|b| scan(b).unwrap_or(false))
}

/// What the handshake needs from a valid TLS 1.2 ServerHello.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerHello12<'a> {
    pub random: [u8; 32],
    /// 0xC02B or 0xC02F.
    pub suite: u16,
    /// The server agreed to `extended_master_secret`.
    pub ems: bool,
    /// The ALPN protocol the server chose, if it sent the extension.
    pub alpn: Option<&'a [u8]>,
}

/// Validate a TLS 1.2 ServerHello against our offer (RFC 5246 §7.4.1.3, RFC 8446 §4.1.3). `msg` is the whole message.
///
/// Accepts only: type 2, version 0x0303, no downgrade marker in the random, a session ID that is not ours, cipher
/// 0xC02B or 0xC02F, compression 0 and at most one each of `server_name` (empty), `ec_point_formats` (with
/// uncompressed), `extended_master_secret` (empty), `renegotiation_info` (empty) and ALPN (one name). Any other
/// extension is an error: we did not offer it.
pub fn parse_server_hello12<'a>(msg: &'a [u8], offered_session_id: &[u8]) -> Result<ServerHello12<'a>, HelloError> {
    let h = parse_header(msg)?;
    if h.msg_type != SERVER_HELLO {
        return Err(HelloError::BadMessageType);
    }
    let body = &msg[HANDSHAKE_HEADER_LEN..];
    if body.len() != h.body_len {
        return Err(if body.len() < h.body_len { HelloError::Truncated } else { HelloError::BadLength });
    }
    let mut r = Reader(body);
    if r.u16()? != LEGACY_VERSION {
        return Err(HelloError::UnsupportedVersion);
    }
    let random: [u8; 32] = r.take(32)?.try_into().map_err(|_| HelloError::Truncated)?;
    if random[24..] == DOWNGRADE_TLS12 {
        return Err(HelloError::Downgrade);
    }
    let sid_len = r.u8()? as usize;
    if sid_len > MAX_SESSION_ID_LEN {
        return Err(HelloError::BadLength);
    }
    if sid_len > 0 && r.take(sid_len)? == offered_session_id {
        return Err(HelloError::Resumption);
    }
    let suite = r.u16()?;
    if suite != TLS_ECDHE_ECDSA_AES_128_GCM_SHA256 && suite != TLS_ECDHE_RSA_AES_128_GCM_SHA256 {
        return Err(HelloError::UnsupportedCipher);
    }
    if r.u8()? != 0 {
        return Err(HelloError::BadCompression);
    }
    let (mut ems, mut alpn, mut seen) = (false, None, [false; 5]);
    if !r.0.is_empty() {
        let ext_len = r.u16()? as usize;
        if r.0.len() != ext_len {
            return Err(if r.0.len() < ext_len { HelloError::Truncated } else { HelloError::BadLength });
        }
        while !r.0.is_empty() {
            let ty = r.u16()?;
            let len = r.u16()? as usize;
            let mut d = Reader(r.take(len)?);
            let slot = match ty {
                EXT_SERVER_NAME => 0,
                EXT_EC_POINT_FORMATS => 1,
                EXT_EXTENDED_MASTER_SECRET => 2,
                EXT_RENEGOTIATION_INFO => 3,
                EXT_ALPN => 4,
                _ => return Err(HelloError::UnsolicitedExtension(ty)),
            };
            if std::mem::replace(&mut seen[slot], true) {
                return Err(HelloError::DuplicateExtension(ty));
            }
            match ty {
                EXT_SERVER_NAME | EXT_EXTENDED_MASTER_SECRET if len != 0 => return Err(HelloError::BadLength),
                EXT_EXTENDED_MASTER_SECRET => ems = true,
                EXT_EC_POINT_FORMATS => {
                    let n = d.u8()? as usize;
                    if !d.take(n)?.contains(&0) || !d.0.is_empty() {
                        return Err(HelloError::BadLength); // uncompressed must be listed
                    }
                }
                EXT_RENEGOTIATION_INFO => {
                    // An empty renegotiated_connection: this is the first handshake (RFC 5746 §3.6).
                    if d.0 != [0u8] {
                        return Err(HelloError::BadLength);
                    }
                }
                EXT_ALPN => {
                    let list = d.u16()? as usize;
                    let mut names = Reader(d.take(list)?);
                    if !d.0.is_empty() {
                        return Err(HelloError::BadLength);
                    }
                    let n = names.u8()? as usize;
                    let name = names.take(n)?;
                    if !names.0.is_empty() {
                        return Err(HelloError::BadLength);
                    }
                    alpn = Some(name);
                }
                _ => {}
            }
        }
    }
    Ok(ServerHello12 { random, suite, ems, alpn })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sha256::{hex, sha256};

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap()).collect()
    }

    // Expected bytes were built by an independent Python script (struct packing, no TLS library).
    const CH_FULL: &str = "010000d80303111111111111111111111111111111111111111111111111111111111111111120222222222222222222222222222222222222222222222222222222222222222200061301c02bc02f0100008900000010000e00000b6578616d706c652e636f6d000a00060004001d0017000d000e000c0403050308040805040105010032000a00080403050304010501002b00050403040303003300260024001d002033333333333333333333333333333333333333333333333333333333333333330010000b000908687474702f312e3100170000ff01000100";
    const CH_NOSID: &str = "010000ae030344444444444444444444444444444444444444444444444444444444444444440000061301c02bc02f0100007f00000006000400000161000a00060004001d0017000d000e000c0403050308040805040105010032000a00080403050304010501002b00050403040303003300260024001d002055555555555555555555555555555555555555555555555555555555555555550010000b000908687474702f312e3100170000ff01000100";
    // RFC 8448 §3 ServerHello (empty session ID in that trace).
    const RFC8448_SH: &str = "020000560303a6af06a4121860dc5e6e60249cd34c95930c8ac5cb1434dac155772ed3e2692800130100002e00330024001d0020c9828876112095fe66762bdbf7c672e156d6cc253b833df1dd69b1b04e751f0f002b00020304";
    const RFC8448_SERVER_PUB: &str = "c9828876112095fe66762bdbf7c672e156d6cc253b833df1dd69b1b04e751f0f";

    fn enc(random: u8, sid: &[u8], pubk: u8, name: &[u8]) -> Result<Vec<u8>, HelloError> {
        enc_alpn(random, sid, pubk, name, false)
    }

    fn enc_alpn(random: u8, sid: &[u8], pubk: u8, name: &[u8], alpn_h2: bool) -> Result<Vec<u8>, HelloError> {
        let mut out = [0u8; MAX_CLIENT_HELLO_LEN];
        let n = encode_client_hello(
            &ClientHelloParams { random: &[random; 32], session_id: sid, x25519_public: &[pubk; 32], server_name: name, alpn_h2 },
            &mut out,
        )?;
        Ok(out[..n].to_vec())
    }

    #[test]
    fn client_hello_known_encodings() {
        assert_eq!(hex(&enc(0x11, &[0x22; 32], 0x33, b"example.com").unwrap()), CH_FULL);
        assert_eq!(hex(&enc(0x44, b"", 0x55, b"a").unwrap()), CH_NOSID);
    }

    #[test]
    fn client_hello_offers_h2_before_http11() {
        let (plain, h2) = (hex(&enc(1, b"", 2, b"x.test").unwrap()), hex(&enc_alpn(1, b"", 2, b"x.test", true).unwrap()));
        assert!(plain.contains("0010000b000908687474702f312e31"));
        assert!(h2.contains("0010000e000c02683208687474702f312e31"));
        // Only the lengths of the ClientHello (3 bytes), its extensions (2 bytes at 0x00..) and the ALPN extension change.
        assert_eq!(h2.len(), plain.len() + 6);
    }

    #[test]
    fn client_hello_signature_algorithms_are_split() {
        // signature_algorithms (13) holds 0x0403, 0x0503, 0x0804 and 0x0805, then 0x0401 and 0x0501 for TLS 1.2
        // ServerKeyExchange. The cert list (50) holds 0x0403, 0x0503, 0x0401 and 0x0501.
        let m = enc(1, b"", 2, b"x.test").unwrap();
        let h = hex(&m);
        assert!(h.contains("000d000e000c040305030804080504010501"));
        assert!(h.contains("0032000a00080403050304010501"));
    }

    #[test]
    fn client_hello_max_size_is_exact_and_framing_is_valid() {
        let name = ["a".repeat(63), "a".repeat(63), "a".repeat(63), "a".repeat(61)].join(".").into_bytes();
        let m = enc_alpn(1, &[9; 32], 2, &name, true).unwrap();
        assert_eq!(m.len(), MAX_CLIENT_HELLO_LEN);
        assert_eq!(enc(1, &[9; 32], 2, &name).unwrap().len(), MAX_CLIENT_HELLO_LEN - 3);
        let (whole, rest) = next_message(&m, MAX_BODY_LEN).unwrap();
        assert_eq!((whole.len(), rest.len()), (m.len(), 0));
    }

    #[test]
    fn client_hello_rejects_invalid_dns_labels() {
        for bad in [&b"a..b"[..], b"-a.example", b"a-.example", b"a/b", b"a@b", b"*.example", b"a_b.example", &[b'a'; 64]] {
            assert_eq!(enc(1, b"", 2, bad), Err(HelloError::BadInput));
        }
    }

    #[test]
    fn client_hello_input_and_buffer_errors() {
        for bad in [&b""[..], b".a", b"a.", b"a b", b"a\x00b", "é.com".as_bytes(), b"127.0.0.1", b"::1", &[b'a'; 254]] {
            assert_eq!(enc(1, b"", 2, bad), Err(HelloError::BadInput), "{bad:?}");
        }
        assert_eq!(enc(1, &[0; 33], 2, b"a"), Err(HelloError::BadInput));
        let full = enc(1, b"", 2, b"a.b").unwrap();
        for cap in 0..full.len() {
            let mut out = vec![0u8; cap];
            let p = ClientHelloParams { random: &[1; 32], session_id: b"", x25519_public: &[2; 32], server_name: b"a.b", alpn_h2: false };
            assert_eq!(encode_client_hello(&p, &mut out), Err(HelloError::BufferTooSmall), "cap {cap}");
        }
    }

    #[test]
    fn framing() {
        assert_eq!(parse_header(&[]), Err(HelloError::Truncated));
        assert_eq!(parse_header(&[1, 0, 0]), Err(HelloError::Truncated));
        assert_eq!(parse_header(&[20, 1, 2, 3, 9]), Ok(Header { msg_type: 20, body_len: 0x010203 }));
        assert_eq!(write_header(2, 0x010203), Ok([2, 1, 2, 3]));
        assert_eq!(write_header(2, MAX_BODY_LEN), Ok([2, 255, 255, 255]));
        assert_eq!(write_header(2, MAX_BODY_LEN + 1), Err(HelloError::BadLength));
        let buf = [20, 0, 0, 2, 0xaa, 0xbb, 8, 0, 0, 0];
        let (m, rest) = next_message(&buf, 16).unwrap();
        assert_eq!((m, rest), (&buf[..6], &buf[6..]));
        let (m2, rest2) = next_message(rest, 16).unwrap();
        assert_eq!((m2, rest2), (&buf[6..], &b""[..]));
        assert_eq!(next_message(&buf[..5], 16), Err(HelloError::Truncated));
        assert_eq!(next_message(&[20, 0, 1, 0], 16), Err(HelloError::TooLarge)); // before waiting for body
        assert_eq!(next_message(&[20, 0xff, 0xff, 0xff], 16), Err(HelloError::TooLarge));
    }

    // Test-side ServerHello builder (separate from the parser under test).
    fn sh(ver: u16, random: &[u8], sid: &[u8], cipher: u16, comp: u8, exts: &[u8]) -> Vec<u8> {
        let mut b = ver.to_be_bytes().to_vec();
        b.extend_from_slice(random);
        b.push(sid.len() as u8);
        b.extend_from_slice(sid);
        b.extend_from_slice(&cipher.to_be_bytes());
        b.push(comp);
        b.extend_from_slice(&(exts.len() as u16).to_be_bytes());
        b.extend_from_slice(exts);
        let mut m = vec![2, (b.len() >> 16) as u8, (b.len() >> 8) as u8, b.len() as u8];
        m.extend(b);
        m
    }
    fn ext(t: u16, d: &[u8]) -> Vec<u8> {
        let mut v = t.to_be_bytes().to_vec();
        v.extend_from_slice(&(d.len() as u16).to_be_bytes());
        v.extend_from_slice(d);
        v
    }
    fn sv13() -> Vec<u8> {
        ext(43, &[3, 4])
    }
    fn ks(group: u16, key: &[u8]) -> Vec<u8> {
        let mut d = group.to_be_bytes().to_vec();
        d.extend_from_slice(&(key.len() as u16).to_be_bytes());
        d.extend_from_slice(key);
        ext(51, &d)
    }
    fn good_exts() -> Vec<u8> {
        [ks(0x1d, &[7; 32]), sv13()].concat()
    }
    const SID: [u8; 32] = [0x22; 32];
    fn good() -> Vec<u8> {
        sh(0x0303, &[5; 32], &SID, 0x1301, 0, &good_exts())
    }

    #[test]
    fn server_hello_rfc8448() {
        let m = unhex(RFC8448_SH);
        assert_eq!(hex(&parse_server_hello(&m, b"").unwrap().x25519_public), RFC8448_SERVER_PUB);
        assert_eq!(parse_server_hello(&m, &SID), Err(HelloError::SessionIdMismatch));
    }

    #[test]
    fn server_hello_valid() {
        assert_eq!(parse_server_hello(&good(), &SID).unwrap().x25519_public, [7; 32]);
        // Extension order does not matter.
        let m = sh(0x0303, &[5; 32], &SID, 0x1301, 0, &[sv13(), ks(0x1d, &[7; 32])].concat());
        assert!(parse_server_hello(&m, &SID).is_ok());
    }

    #[test]
    fn server_hello_rejects_bad_fields() {
        let e = good_exts();
        let p = |m: Vec<u8>| parse_server_hello(&m, &SID);
        assert_eq!(p(sh(0x0301, &[5; 32], &SID, 0x1301, 0, &e)), Err(HelloError::BadLegacyVersion));
        assert_eq!(p(sh(0x0304, &[5; 32], &SID, 0x1301, 0, &e)), Err(HelloError::BadLegacyVersion));
        assert_eq!(p(sh(0x0303, &[5; 32], &[0x23; 32], 0x1301, 0, &e)), Err(HelloError::SessionIdMismatch));
        assert_eq!(p(sh(0x0303, &[5; 32], b"", 0x1301, 0, &e)), Err(HelloError::SessionIdMismatch));
        assert_eq!(p(sh(0x0303, &[5; 32], &[0x22; 33], 0x1301, 0, &e)), Err(HelloError::BadLength));
        for c in [0x1302, 0x1303, 0x1304, 0xc02b, 0x0000] {
            assert_eq!(p(sh(0x0303, &[5; 32], &SID, c, 0, &e)), Err(HelloError::UnsupportedCipher));
        }
        assert_eq!(p(sh(0x0303, &[5; 32], &SID, 0x1301, 1, &e)), Err(HelloError::BadCompression));
        let mut wrong_type = good();
        wrong_type[0] = 1;
        assert_eq!(p(wrong_type), Err(HelloError::BadMessageType));
    }

    #[test]
    fn server_hello_rejects_bad_extensions() {
        let p = |exts: Vec<u8>| parse_server_hello(&sh(0x0303, &[5; 32], &SID, 0x1301, 0, &exts), &SID);
        let (k, v) = (ks(0x1d, &[7; 32]), sv13());
        assert_eq!(p(vec![]), Err(HelloError::MissingExtension(43)));
        assert_eq!(p(v.clone()), Err(HelloError::MissingExtension(51)));
        assert_eq!(p(k.clone()), Err(HelloError::MissingExtension(43)));
        assert_eq!(p([k.clone(), v.clone(), v.clone()].concat()), Err(HelloError::DuplicateExtension(43)));
        assert_eq!(p([k.clone(), k.clone(), v.clone()].concat()), Err(HelloError::DuplicateExtension(51)));
        // Unsolicited: pre_shared_key, cookie, server_name, ALPN, supported_groups, unknown.
        for t in [41u16, 44, 0, 16, 10, 0xfe0d, 0x0a0a] {
            assert_eq!(p([k.clone(), v.clone(), ext(t, &[])].concat()), Err(HelloError::UnsolicitedExtension(t)));
        }
        // Duplicate unsolicited extension: the first one already fails.
        assert_eq!(p([k.clone(), v.clone(), ext(16, b""), ext(16, b"")].concat()), Err(HelloError::UnsolicitedExtension(16)));
        // supported_versions: wrong version, wrong length.
        assert_eq!(p([k.clone(), ext(43, &[3, 3])].concat()), Err(HelloError::UnsupportedVersion));
        assert_eq!(p([k.clone(), ext(43, &[3, 4, 3, 3])].concat()), Err(HelloError::BadLength));
        assert_eq!(p([k.clone(), ext(43, &[3])].concat()), Err(HelloError::BadLength));
        // key_share: wrong group, wrong length, trailing bytes, truncated.
        assert_eq!(p([ks(0x17, &[7; 65]), v.clone()].concat()), Err(HelloError::BadKeyShare));
        assert_eq!(p([ks(0x1d, &[7; 31]), v.clone()].concat()), Err(HelloError::BadKeyShare));
        assert_eq!(p([ks(0x1d, &[7; 33]), v.clone()].concat()), Err(HelloError::BadKeyShare));
        assert_eq!(p([ext(51, &[0, 0x1d, 0, 32, 1, 2, 3]), v.clone()].concat()), Err(HelloError::BadKeyShare));
        assert_eq!(p([ext(51, &[0, 0x1d]), v.clone()].concat()), Err(HelloError::Truncated));
        assert_eq!(p([ext(51, &[]), v.clone()].concat()), Err(HelloError::Truncated));
    }

    #[test]
    fn server_hello_rejects_bad_lengths() {
        let g = good();
        // Every proper prefix is Truncated.
        for n in 0..g.len() {
            assert_eq!(parse_server_hello(&g[..n], &SID), Err(HelloError::Truncated), "prefix {n}");
        }
        // Trailing bytes after the message.
        let mut t = g.clone();
        t.push(0);
        assert_eq!(parse_server_hello(&t, &SID), Err(HelloError::BadLength));
        // Extension block length too long / too short while the message length is consistent.
        for delta in [1i32, -1, 4] {
            let mut m = g.clone();
            let at = 4 + 2 + 32 + 1 + 32 + 2 + 1;
            let v = (u16::from_be_bytes([m[at], m[at + 1]]) as i32 + delta) as u16;
            m[at..at + 2].copy_from_slice(&v.to_be_bytes());
            assert!(parse_server_hello(&m, &SID).is_err(), "delta {delta}");
        }
        // Inner extension length past the end of the block.
        let m = sh(0x0303, &[5; 32], &SID, 0x1301, 0, &[0, 43, 0, 9, 3, 4]);
        assert_eq!(parse_server_hello(&m, &SID), Err(HelloError::Truncated));
        // Extension header cut in the middle.
        let m = sh(0x0303, &[5; 32], &SID, 0x1301, 0, &[0, 43, 0]);
        assert_eq!(parse_server_hello(&m, &SID), Err(HelloError::Truncated));
    }

    #[test]
    fn hello_retry_request_is_explicit() {
        assert_eq!(sha256(b"HelloRetryRequest"), HELLO_RETRY_REQUEST_RANDOM);
        // Real HRR shape: supported_versions, key_share with only the selected group.
        let exts = [sv13(), ext(51, &[0, 0x17])].concat();
        let m = sh(0x0303, &HELLO_RETRY_REQUEST_RANDOM, &SID, 0x1301, 0, &exts);
        assert_eq!(parse_server_hello(&m, &SID), Err(HelloError::HelloRetryRequest));
        // Detected before the rest is read, even when the rest is garbage-free but cookie-laden.
        let m = sh(0x0303, &HELLO_RETRY_REQUEST_RANDOM, &SID, 0x1301, 0, &[sv13(), ext(44, &[1, 2, 3])].concat());
        assert_eq!(parse_server_hello(&m, &SID), Err(HelloError::HelloRetryRequest));
        // One flipped random bit is a normal ServerHello.
        let mut r = HELLO_RETRY_REQUEST_RANDOM;
        r[31] ^= 1;
        assert!(parse_server_hello(&sh(0x0303, &r, &SID, 0x1301, 0, &good_exts()), &SID).is_ok());
    }

    #[test]
    fn client_hello_offers_tls12() {
        let h = hex(&enc(1, b"", 2, b"x.test").unwrap());
        // supported_versions: 1.3 then 1.2. Suites: 0x1301, 0xC02B, 0xC02F. Then EMS and an empty renegotiation_info.
        assert!(h.contains("002b00050403040303"));
        assert!(h.contains("00061301c02bc02f"));
        assert!(h.ends_with("00170000ff01000100"));
    }

    // ---- TLS 1.2 ServerHello ----

    const SUITE_ECDSA: u16 = 0xC02B;
    fn sh12(random: &[u8; 32], sid: &[u8], suite: u16, exts: &[u8]) -> Vec<u8> {
        sh(0x0303, random, sid, suite, 0, exts)
    }
    /// The same message without its (empty) extension block: a TLS 1.2 ServerHello may end after the compression byte.
    fn no_ext_block(m: &[u8]) -> Vec<u8> {
        let mut v = m[..m.len() - 2].to_vec();
        v[3] -= 2;
        v
    }
    fn good12_exts() -> Vec<u8> {
        [ext(0xff01, &[0]), ext(11, &[1, 0]), ext(23, &[]), ext(16, &[0, 9, 8, b'h', b't', b't', b'p', b'/', b'1', b'.', b'1'])].concat()
    }

    #[test]
    fn tells_tls12_from_tls13() {
        assert!(is_tls12_server_hello(&sh12(&[5; 32], b"", SUITE_ECDSA, &good12_exts())));
        assert!(!is_tls12_server_hello(&good()));
        assert!(!is_tls12_server_hello(&sh(0x0303, &HELLO_RETRY_REQUEST_RANDOM, &SID, 0x1301, 0, &good_exts())));
        // No extension block at all is still TLS 1.2. A cut message is not.
        let m = sh12(&[5; 32], b"", SUITE_ECDSA, &[]);
        assert!(is_tls12_server_hello(&no_ext_block(&m)));
        assert!(!is_tls12_server_hello(&m[..m.len() - 1]));
        assert!(!is_tls12_server_hello(&[]));
        for cut in 0..good().len() {
            let _ = is_tls12_server_hello(&good()[..cut]); // no panic
        }
    }

    #[test]
    fn server_hello12_valid() {
        let m = sh12(&[5; 32], &[0x77; 32], SUITE_ECDSA, &good12_exts());
        let h = parse_server_hello12(&m, &SID).unwrap();
        assert_eq!((h.random, h.suite, h.ems, h.alpn), ([5; 32], 0xC02B, true, Some(&b"http/1.1"[..])));
        let m = sh12(&[5; 32], b"", 0xC02F, &[]);
        let h = parse_server_hello12(&m, &SID).unwrap();
        assert_eq!((h.suite, h.ems, h.alpn), (0xC02F, false, None));
        assert!(parse_server_hello12(&no_ext_block(&m), &SID).is_ok());
        // The 1.1 marker is not the 1.3 downgrade marker.
        let mut r = [5u8; 32];
        r[24..].copy_from_slice(b"DOWNGRD\x00");
        assert!(parse_server_hello12(&sh12(&r, b"", 0xC02F, &[]), &SID).is_ok());
    }

    #[test]
    fn server_hello12_rejects() {
        let p = |m: Vec<u8>| parse_server_hello12(&m, &SID).map(|_| ());
        let g = good12_exts();
        let mut r = [5u8; 32];
        r[24..].copy_from_slice(b"DOWNGRD\x01");
        assert_eq!(p(sh12(&r, b"", 0xC02F, &g)), Err(HelloError::Downgrade));
        assert_eq!(p(sh12(&[5; 32], &SID, 0xC02F, &g)), Err(HelloError::Resumption));
        assert_eq!(p(sh12(&[5; 32], &[0x22; 33], 0xC02F, &g)), Err(HelloError::BadLength));
        for c in [0x1301, 0xC030, 0xC02C, 0xCCA8, 0x009C, 0x0000] {
            assert_eq!(p(sh12(&[5; 32], b"", c, &g)), Err(HelloError::UnsupportedCipher));
        }
        for v in [0x0300, 0x0301, 0x0302, 0x0304] {
            assert_eq!(p(sh(v, &[5; 32], b"", 0xC02F, 0, &g)), Err(HelloError::UnsupportedVersion));
        }
        assert_eq!(p(sh(0x0303, &[5; 32], b"", 0xC02F, 1, &g)), Err(HelloError::BadCompression));
        assert_eq!(p(sh12(&[5; 32], b"", 0xC02F, &ext(35, &[]))), Err(HelloError::UnsolicitedExtension(35)));
        assert_eq!(p(sh12(&[5; 32], b"", 0xC02F, &ext(43, &[3, 3]))), Err(HelloError::UnsolicitedExtension(43)));
        assert_eq!(p(sh12(&[5; 32], b"", 0xC02F, &[ext(23, &[]), ext(23, &[])].concat())), Err(HelloError::DuplicateExtension(23)));
        assert_eq!(p(sh12(&[5; 32], b"", 0xC02F, &ext(23, &[1]))), Err(HelloError::BadLength));
        assert_eq!(p(sh12(&[5; 32], b"", 0xC02F, &ext(0xff01, &[1, 2]))), Err(HelloError::BadLength)); // a renegotiation
        assert_eq!(p(sh12(&[5; 32], b"", 0xC02F, &ext(11, &[1, 1]))), Err(HelloError::BadLength)); // no uncompressed
        assert_eq!(p(sh12(&[5; 32], b"", 0xC02F, &ext(16, &[0, 4, 1, b'a', 1, b'b']))), Err(HelloError::BadLength)); // two names
        let mut m = sh12(&[5; 32], b"", 0xC02F, &g);
        m.push(0);
        assert_eq!(p(m), Err(HelloError::BadLength));
        let m = sh12(&[5; 32], b"", 0xC02F, &g);
        for cut in 0..m.len() {
            assert!(p(m[..cut].to_vec()).is_err(), "cut {cut}");
        }
        assert_eq!(parse_server_hello12(&good_cert_msg(), &SID), Err(HelloError::BadMessageType));
    }
    fn good_cert_msg() -> Vec<u8> {
        vec![11, 0, 0, 0]
    }
}
