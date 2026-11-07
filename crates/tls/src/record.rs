//! TLS 1.3 record layer (RFC 8446 section 5) for TLS_AES_128_GCM_SHA256. Learning code, NOT audited.
//!
//! Sources: RFC 8446 sections 5.1 to 5.5 and 6 (records, protection, nonce, padding,
//! key limits, alerts). Test vectors: RFC 8448 section 3 (simple 1-RTT trace).
//!
//! This module only frames bytes. It does NOT run a handshake. A plaintext record
//! proves nothing about who sent it; the handshake must authenticate everything it
//! reads before the keys come from it. `bare` cannot open an HTTPS page yet.
//!
//! Contract for the handshake code (the caller):
//! - One [`RecordLayer`] wraps one stream (`Read + Write`). It starts in the plaintext
//!   state in both directions. The caller installs keys with [`RecordLayer::install_read_key`]
//!   and [`RecordLayer::install_write_key`]. Each direction has its own key, static IV and
//!   64-bit sequence number. A new key resets that direction's sequence number to 0.
//! - Records are not messages. A handshake message may be split over several records or
//!   share one record with others. The caller must reassemble. RFC 8446 section 5.1 says
//!   a message must not cross a key change; the caller must check that the buffered
//!   bytes end at a record boundary before it installs a key. This module returns one
//!   record per call, so the check is "no partial message is left".
//! - [`RecordLayer::read_record`] returns a slice that borrows the internal buffer.
//!   It is valid until the next call. It holds only authenticated content.
//! - A compatibility `change_cipher_spec` record (RFC 8446 section 5, appendix D.4) is only
//!   dropped when the caller turned it on with [`RecordLayer::allow_compat_ccs`]. The
//!   caller turns it on after the ClientHello is sent and off after the peer Finished.
//! - On any `Err` that is not [`Error::KeyLimit`], that direction is dead. Later calls
//!   return [`Error::Failed`]. The caller may send an alert (see [`Error::alert`]) and
//!   must close the connection.
//!
//! Limits and decisions:
//! - Plaintext fragment at most 2^14 bytes. Inner plaintext (content, type byte, zero
//!   padding) at most 2^14 + 1 bytes (section 5.4). So the protected record body is at
//!   most 2^14 + 1 + 16 bytes. Section 5.2 allows 2^14 + 256 on the wire. We reject
//!   anything above the section 5.4 size (`record_overflow`), because AES-GCM adds 16 bytes.
//!   A longer body can only come from a sender that breaks section 5.4. All of these
//!   checks run on the 5-byte header, before the body is read or decrypted.
//! - Record version: section 5.1 says the plaintext field "MUST be ignored for all
//!   purposes". We only check that the major byte is 3 (it must look like TLS) and do
//!   not use the minor byte. A protected record must carry 0x0303 (section 5.2 says
//!   "always"). The header is also the AEAD additional data, so it is authenticated.
//! - Key usage: section 5.5 allows 2^24.5 full-size AES-GCM records per key. We stop at
//!   floor(2^24.5) = 23,726,566 records per direction ([`MAX_RECORDS_PER_KEY`]), counted
//!   for small records too, which is conservative. At the limit the call returns
//!   [`Error::KeyLimit`] before it touches the nonce. The caller must do a KeyUpdate
//!   (section 4.6.3) BEFORE that point: check [`RecordLayer::write_sequence`]. KeyUpdate
//!   itself is not here yet. Sequence numbers never wrap: the limit is far below 2^64.
//! - Alerts: the level byte is ignored (section 6). `close_notify` ends the read side
//!   cleanly. Every other alert, also `user_canceled`, is an error (fail closed).
//!   Zero-length handshake and alert records are rejected (sections 5.1, 5.4). Zero-length
//!   application data is accepted.
//! - Not constant time beyond what `aes_gcm` gives. The buffers are not wiped when the
//!   layer is dropped; only the AES key is.
//! - A non-blocking or time-out socket is not supported: any I/O error except
//!   `Interrupted` kills the direction.

use crate::aes_gcm::{Aes128Gcm, KEY_LEN, NONCE_LEN, TAG_LEN, tls13_nonce};
use std::io::{ErrorKind, Read, Write};

pub const HEADER_LEN: usize = 5;
/// Largest plaintext fragment (RFC 8446 section 5.1).
pub const MAX_PLAINTEXT: usize = 1 << 14;
/// Largest inner plaintext: content, 1 type byte and zero padding (section 5.4).
pub const MAX_INNER: usize = MAX_PLAINTEXT + 1;
/// Largest protected record body: inner plaintext plus the 16-byte tag.
pub const MAX_CIPHERTEXT: usize = MAX_INNER + TAG_LEN;
/// Largest whole record on the wire.
pub const MAX_WIRE: usize = HEADER_LEN + MAX_CIPHERTEXT;
/// floor(2^24.5): the AES-GCM key usage limit of RFC 8446 section 5.5, per direction.
pub const MAX_RECORDS_PER_KEY: u64 = 23_726_566;

const CCS: u8 = 20;
const ALERT: u8 = 21;
const HANDSHAKE: u8 = 22;
const APPDATA: u8 = 23;
const VERSION: [u8; 2] = [3, 3];

/// What the caller gets for one record. The slices borrow the layer's buffer.
#[derive(Debug, PartialEq, Eq)]
pub enum Record<'a> {
    /// Part of one or more handshake messages (not necessarily whole ones).
    Handshake(&'a [u8]),
    /// Application bytes. May be empty.
    ApplicationData(&'a [u8]),
    /// The peer sent `close_notify`. Nothing more will be read; later calls repeat this.
    CloseNotify,
}

/// Every error is `Copy` so the layer can report it again. `Io` keeps only the kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Io(ErrorKind),
    /// The stream ended at a record boundary but no `close_notify` came.
    UnexpectedEof,
    /// The stream ended inside a record.
    Truncated,
    /// A type byte that this module does not know.
    BadContentType(u8),
    /// A known type that is not allowed now (for example plaintext after keys).
    UnexpectedRecord(u8),
    BadVersion(u16),
    RecordOverflow,
    /// A handshake or alert record with no content.
    EmptyRecord,
    /// For example an alert that is not exactly 2 bytes.
    Malformed,
    /// Tag mismatch or a body shorter than a tag.
    AuthFailed,
    /// The decrypted record had no non-zero byte (no type byte).
    AllPadding,
    PeerAlert(u8),
    /// Per-key record limit reached. Install a new key. Nothing was consumed or sent.
    KeyLimit,
    /// The new key and IV equal the current ones. That would repeat nonces.
    SameKey,
    /// Application data asked for before a write key exists.
    NotProtected,
    /// This direction failed before. It stays failed.
    Failed,
    /// This direction was closed by an alert.
    Closed,
    /// Cannot happen when the code is right.
    Internal,
}

impl Error {
    /// The alert (RFC 8446 section 6.2) to send for this error, if one fits.
    pub fn alert(&self) -> Option<u8> {
        match self {
            Error::AuthFailed => Some(20),                      // bad_record_mac
            Error::RecordOverflow => Some(22),                  // record_overflow
            Error::BadContentType(_) | Error::UnexpectedRecord(_) | Error::EmptyRecord
            | Error::AllPadding => Some(10),                    // unexpected_message
            Error::Malformed => Some(50),                       // decode_error
            Error::Internal => Some(80),                        // internal_error
            _ => None,
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "tls record error: {self:?}")
    }
}
impl std::error::Error for Error {}

/// One direction's key, static IV and sequence number. A new key makes a new `Traffic`;
/// there is no way to reset the sequence of an existing one.
struct Traffic {
    aead: Aes128Gcm,
    key: [u8; KEY_LEN],
    iv: [u8; NONCE_LEN],
    seq: u64,
    /// `MAX_RECORDS_PER_KEY`. A field so tests can use a small bound.
    limit: u64,
}

impl Traffic {
    fn new(key: &[u8; KEY_LEN], iv: &[u8; NONCE_LEN]) -> Self {
        Traffic { aead: Aes128Gcm::new(key), key: *key, iv: *iv, seq: 0, limit: MAX_RECORDS_PER_KEY }
    }
}

impl Drop for Traffic {
    fn drop(&mut self) {
        // The AEAD wipes its own round keys. Wipe our copies of the key and IV too.
        // SAFETY: valid, aligned, exclusive references to our own fields.
        unsafe {
            std::ptr::write_volatile(&mut self.key, [0; KEY_LEN]);
            std::ptr::write_volatile(&mut self.iv, [0; NONCE_LEN]);
        }
    }
}

// Both directions normally hold a key. Keep it inline to avoid epoch-change allocations.
#[allow(clippy::large_enum_variant)]
enum Tx {
    Plain,
    Keyed(Traffic),
    /// We sent an alert that ends our write side.
    Closed,
    Dead,
}

#[allow(clippy::large_enum_variant)]
enum Rx {
    Plain,
    Keyed(Traffic),
    /// The peer sent `close_notify`.
    Closed,
    Dead,
}

/// Result of parsing one record, as positions in `rbuf` (so no borrow is held).
enum Next {
    Handshake(usize, usize),
    App(usize, usize),
    CloseNotify,
}

/// The record layer over one stream. See the module docs for the contract.
pub struct RecordLayer<T> {
    io: T,
    tx: Tx,
    rx: Rx,
    /// One outgoing record, built and sealed in place. Allocated once.
    wbuf: Box<[u8]>,
    /// Read-ahead buffer: room for two whole records, so one `read` call can bring in
    /// several records and a partial record never needs more room than is left after
    /// the bytes are moved to the front. Allocated once.
    rbuf: Box<[u8]>,
    /// `rbuf[pos..end]` holds bytes not yet consumed.
    pos: usize,
    end: usize,
    /// Bytes of the record returned by the last `read_record`. Skipped at the next call,
    /// because the caller still borrows them until then.
    pending: usize,
    ccs_allowed: bool,
}

impl<T: Read + Write> RecordLayer<T> {
    /// Both directions start in plaintext.
    pub fn new(io: T) -> Self {
        RecordLayer {
            io,
            tx: Tx::Plain,
            rx: Rx::Plain,
            wbuf: vec![0; MAX_WIRE].into_boxed_slice(),
            rbuf: vec![0; 2 * MAX_WIRE].into_boxed_slice(),
            pos: 0,
            end: 0,
            pending: 0,
            ccs_allowed: false,
        }
    }

    pub fn get_ref(&self) -> &T {
        &self.io
    }

    /// The stream, for `flush` and timeouts. Do not read or write it directly: that breaks
    /// the record framing.
    pub fn get_mut(&mut self) -> &mut T {
        &mut self.io
    }

    /// Gives back the stream. Bytes already read ahead are lost.
    pub fn into_inner(self) -> T {
        self.io
    }

    /// Records sent under the current write key (0 when none). Compare with
    /// [`MAX_RECORDS_PER_KEY`] to decide when to start a KeyUpdate.
    pub fn write_sequence(&self) -> u64 {
        if let Tx::Keyed(t) = &self.tx { t.seq } else { 0 }
    }

    pub fn read_sequence(&self) -> u64 {
        if let Rx::Keyed(t) = &self.rx { t.seq } else { 0 }
    }

    /// Allow or forbid the compatibility `change_cipher_spec` record on the read side.
    /// When allowed, a record `14 03 03 00 01 01` is dropped without a sequence number.
    /// Any other `change_cipher_spec` record is an error. When forbidden, all are errors.
    pub fn allow_compat_ccs(&mut self, allowed: bool) {
        self.ccs_allowed = allowed;
    }

    /// Start (or replace) protection of what we send. Fails when this key and IV are
    /// the ones in use, because the sequence number would restart on the same key.
    pub fn install_write_key(&mut self, key: &[u8; KEY_LEN], iv: &[u8; NONCE_LEN]) -> Result<(), Error> {
        match &self.tx {
            Tx::Dead => return Err(Error::Failed),
            Tx::Closed => return Err(Error::Closed),
            Tx::Keyed(t) if t.key == *key && t.iv == *iv => return Err(Error::SameKey),
            _ => {}
        }
        self.tx = Tx::Keyed(Traffic::new(key, iv));
        Ok(())
    }

    /// Start (or replace) protection of what we read. Bytes already read ahead are
    /// decrypted with the new key, because they are only parsed at the next read.
    pub fn install_read_key(&mut self, key: &[u8; KEY_LEN], iv: &[u8; NONCE_LEN]) -> Result<(), Error> {
        match &self.rx {
            Rx::Dead => return Err(Error::Failed),
            Rx::Closed => return Err(Error::Closed),
            Rx::Keyed(t) if t.key == *key && t.iv == *iv => return Err(Error::SameKey),
            _ => {}
        }
        self.rx = Rx::Keyed(Traffic::new(key, iv));
        Ok(())
    }

    // ---- writing ----

    /// Sends handshake bytes, split into records of at most 2^14 bytes. The last record
    /// ends exactly at the end of `msg`, so a message that precedes a key change stays
    /// on a record boundary. Empty input is an error (section 5.1). Either all records
    /// are sent or the call fails before sending any (only for `KeyLimit`).
    pub fn write_handshake(&mut self, msg: &[u8]) -> Result<(), Error> {
        if msg.is_empty() {
            return Err(Error::EmptyRecord);
        }
        self.write_records(HANDSHAKE, msg)
    }

    /// Sends application bytes. Needs a write key (it must never go out in plaintext).
    /// Empty input sends nothing.
    pub fn write_application_data(&mut self, data: &[u8]) -> Result<(), Error> {
        if !matches!(self.tx, Tx::Keyed(_)) {
            return Err(match self.tx {
                Tx::Dead => Error::Failed,
                Tx::Closed => Error::Closed,
                _ => Error::NotProtected,
            });
        }
        if data.is_empty() {
            return Ok(());
        }
        self.write_records(APPDATA, data)
    }

    /// Sends the plaintext compatibility record `14 03 03 00 01 01`. It is never
    /// protected and never uses a sequence number. The caller decides when (RFC 8446
    /// appendix D.4): after the first ClientHello, before the write key changes.
    pub fn write_compat_ccs(&mut self) -> Result<(), Error> {
        match self.tx {
            Tx::Dead => return Err(Error::Failed),
            Tx::Closed => return Err(Error::Closed),
            _ => {}
        }
        let rec = [CCS, VERSION[0], VERSION[1], 0, 1, 1];
        self.io.write_all(&rec).map_err(|e| self.kill_tx(e.kind()))
    }

    /// Sends `close_notify` and ends our write side.
    pub fn send_close_notify(&mut self) -> Result<(), Error> {
        self.send_alert(1, 0)
    }

    /// Sends a fatal alert (description from RFC 8446 section 6.2) and ends our write side.
    pub fn send_fatal_alert(&mut self, description: u8) -> Result<(), Error> {
        self.send_alert(2, description)
    }

    fn send_alert(&mut self, level: u8, description: u8) -> Result<(), Error> {
        match self.tx {
            Tx::Dead => return Err(Error::Failed),
            Tx::Closed => return Err(Error::Closed),
            _ => {}
        }
        self.write_records(ALERT, &[level, description])?;
        self.tx = Tx::Closed;
        Ok(())
    }

    fn kill_tx(&mut self, kind: ErrorKind) -> Error {
        self.tx = Tx::Dead; // drops the key (wiped)
        Error::Io(kind)
    }

    fn write_records(&mut self, ty: u8, data: &[u8]) -> Result<(), Error> {
        let records = data.len().div_ceil(MAX_PLAINTEXT) as u64;
        match &self.tx {
            Tx::Dead => return Err(Error::Failed),
            Tx::Closed => return Err(Error::Closed),
            Tx::Plain if ty == APPDATA => return Err(Error::NotProtected),
            // `limit - seq` cannot underflow: seq <= limit always holds.
            Tx::Keyed(t) if t.limit - t.seq < records => return Err(Error::KeyLimit),
            _ => {}
        }
        for chunk in data.chunks(MAX_PLAINTEXT) {
            self.write_one(ty, chunk, 0)?;
        }
        Ok(())
    }

    /// Builds one record in `wbuf` and writes it. `pad` zero bytes follow the type byte
    /// (only tests use a non-zero `pad`; this crate does not pad what it sends).
    fn write_one(&mut self, ty: u8, content: &[u8], pad: usize) -> Result<(), Error> {
        let n = content.len();
        if n > MAX_PLAINTEXT || n + 1 + pad > MAX_INNER {
            return Err(Error::RecordOverflow); // before anything is built or encrypted
        }
        let total = match &mut self.tx {
            Tx::Plain => {
                let b = &mut self.wbuf;
                b[..HEADER_LEN].copy_from_slice(&[ty, VERSION[0], VERSION[1], (n >> 8) as u8, n as u8]);
                b[HEADER_LEN..HEADER_LEN + n].copy_from_slice(content);
                HEADER_LEN + n
            }
            Tx::Keyed(t) => {
                // seq < limit <= u64::MAX, so `seq + 1` cannot wrap.
                if t.seq >= t.limit {
                    return Err(Error::KeyLimit);
                }
                let inner = n + 1 + pad;
                let clen = inner + TAG_LEN;
                let header = [APPDATA, VERSION[0], VERSION[1], (clen >> 8) as u8, clen as u8];
                let b = &mut self.wbuf;
                b[..HEADER_LEN].copy_from_slice(&header);
                b[HEADER_LEN..HEADER_LEN + n].copy_from_slice(content);
                b[HEADER_LEN + n] = ty;
                b[HEADER_LEN + n + 1..HEADER_LEN + inner].fill(0);
                let nonce = tls13_nonce(&t.iv, t.seq);
                // The nonce counts as used from here, even if the write below fails.
                t.seq += 1;
                let tag = match t.aead.seal_in_place_detached(&nonce, &header, &mut b[HEADER_LEN..HEADER_LEN + inner]) {
                    Ok(tag) => tag,
                    Err(_) => {
                        self.tx = Tx::Dead;
                        return Err(Error::Internal);
                    }
                };
                b[HEADER_LEN + inner..HEADER_LEN + clen].copy_from_slice(&tag);
                HEADER_LEN + clen
            }
            Tx::Closed => return Err(Error::Closed),
            Tx::Dead => return Err(Error::Failed),
        };
        // `write_all` retries short writes and `Interrupted`. Any other error may have
        // left half a record on the wire, so the direction is dead.
        match self.io.write_all(&self.wbuf[..total]) {
            Ok(()) => Ok(()),
            Err(e) => Err(self.kill_tx(e.kind())),
        }
    }

    // ---- reading ----

    /// Reads and checks the next record. The returned slice is valid until the next
    /// call on this layer (Rust: it borrows `self`, so the compiler enforces that).
    pub fn read_record(&mut self) -> Result<Record<'_>, Error> {
        match self.next() {
            Ok(Next::Handshake(a, b)) => Ok(Record::Handshake(&self.rbuf[a..b])),
            Ok(Next::App(a, b)) => Ok(Record::ApplicationData(&self.rbuf[a..b])),
            Ok(Next::CloseNotify) => Ok(Record::CloseNotify),
            Err(e) => {
                // `KeyLimit` leaves the stream intact: nothing was consumed.
                if e != Error::KeyLimit && !matches!(self.rx, Rx::Closed) {
                    self.rx = Rx::Dead;
                }
                Err(e)
            }
        }
    }

    /// True when the bytes that the socket already gave hold a whole next record: `read_record` will not block.
    pub fn has_record(&self) -> bool {
        let (at, unread) = (self.pos + self.pending, self.end - self.pos - self.pending);
        unread >= HEADER_LEN && unread >= HEADER_LEN + u16::from_be_bytes([self.rbuf[at + 3], self.rbuf[at + 4]]) as usize
    }

    /// Makes sure `rbuf[pos..end]` has at least `need` bytes (`need <= MAX_WIRE`).
    fn fill(&mut self, need: usize) -> Result<(), Error> {
        if self.end - self.pos >= need {
            return Ok(());
        }
        if self.pos + need > self.rbuf.len() {
            // Move the unread bytes to the front. They are less than `need <= MAX_WIRE`,
            // which is half the buffer, so the rest of the room is enough.
            self.rbuf.copy_within(self.pos..self.end, 0);
            self.end -= self.pos;
            self.pos = 0;
        }
        while self.end - self.pos < need {
            match self.io.read(&mut self.rbuf[self.end..]) {
                // No bytes of a record at all: the peer closed without `close_notify`.
                Ok(0) if self.end == self.pos => return Err(Error::UnexpectedEof),
                Ok(0) => return Err(Error::Truncated),
                Ok(n) => self.end += n,
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) => return Err(Error::Io(e.kind())),
            }
        }
        Ok(())
    }

    fn next(&mut self) -> Result<Next, Error> {
        match self.rx {
            Rx::Dead => return Err(Error::Failed),
            Rx::Closed => return Ok(Next::CloseNotify), // data after close_notify is ignored
            _ => {}
        }
        // Skip the record the caller got last time.
        self.pos += self.pending;
        self.pending = 0;
        if self.pos == self.end {
            self.pos = 0;
            self.end = 0;
        }
        loop {
            self.fill(HEADER_LEN)?;
            let h: [u8; HEADER_LEN] = self.rbuf[self.pos..self.pos + HEADER_LEN].try_into().unwrap();
            let (ty, version, len) = (h[0], u16::from_be_bytes([h[1], h[2]]), u16::from_be_bytes([h[3], h[4]]) as usize);
            let keyed = matches!(self.rx, Rx::Keyed(_));
            // 1. Is this type allowed in this state?
            match (ty, keyed) {
                (CCS, _) if !self.ccs_allowed => return Err(Error::UnexpectedRecord(ty)),
                (CCS, _) => {}
                (APPDATA, true) | (ALERT | HANDSHAKE, false) => {}
                (ALERT | HANDSHAKE | APPDATA, _) => return Err(Error::UnexpectedRecord(ty)),
                _ => return Err(Error::BadContentType(ty)),
            }
            // 2. Version.
            let version_ok = if ty == APPDATA { version == 0x0303 } else { version >> 8 == 3 };
            if !version_ok {
                return Err(Error::BadVersion(version));
            }
            // 3. Length, from the header alone.
            let max = if ty == APPDATA { MAX_CIPHERTEXT } else { MAX_PLAINTEXT };
            if len > max {
                return Err(Error::RecordOverflow);
            }
            if ty == CCS {
                // Only the one-byte value 1 may be dropped (section 5).
                if len != 1 {
                    return Err(Error::UnexpectedRecord(ty));
                }
                self.fill(HEADER_LEN + 1)?;
                if self.rbuf[self.pos + HEADER_LEN] != 1 {
                    return Err(Error::UnexpectedRecord(ty));
                }
                self.pos += HEADER_LEN + 1;
                continue;
            }
            if ty == APPDATA {
                if let Rx::Keyed(t) = &self.rx
                    && t.seq >= t.limit {
                    return Err(Error::KeyLimit);
                }
                if len < TAG_LEN {
                    return Err(Error::AuthFailed); // no room for a tag
                }
            }
            self.fill(HEADER_LEN + len)?;
            let start = self.pos + HEADER_LEN;
            self.pending = HEADER_LEN + len;
            let (inner_ty, a, b) = if ty == APPDATA {
                let Rx::Keyed(t) = &mut self.rx else { return Err(Error::Internal) };
                let (body, tag) = self.rbuf[start..start + len].split_at_mut(len - TAG_LEN);
                let tag: [u8; TAG_LEN] = (&*tag).try_into().unwrap();
                let nonce = tls13_nonce(&t.iv, t.seq);
                // On failure `body` is left as ciphertext: no plaintext is made.
                t.aead.open_in_place_detached(&nonce, &h, body, &tag).map_err(|_| Error::AuthFailed)?;
                t.seq += 1;
                // Scan back from the end for the type byte, only inside the plaintext.
                let i = body.iter().rposition(|&x| x != 0).ok_or(Error::AllPadding)?;
                (body[i], start, start + i)
            } else {
                (ty, start, start + len)
            };
            return match inner_ty {
                HANDSHAKE if a == b => Err(Error::EmptyRecord),
                HANDSHAKE => Ok(Next::Handshake(a, b)),
                APPDATA if keyed => Ok(Next::App(a, b)),
                ALERT => self.alert(a, b),
                CCS => Err(Error::UnexpectedRecord(CCS)), // a protected change_cipher_spec
                APPDATA => Err(Error::UnexpectedRecord(APPDATA)),
                t => Err(Error::BadContentType(t)),
            };
        }
    }

    fn alert(&mut self, a: usize, b: usize) -> Result<Next, Error> {
        match b - a {
            0 => Err(Error::EmptyRecord),
            2 => match self.rbuf[a + 1] {
                0 => {
                    self.rx = Rx::Closed; // drops the key
                    Ok(Next::CloseNotify)
                }
                d => Err(Error::PeerAlert(d)),
            },
            _ => Err(Error::Malformed), // one alert per record, exactly 2 bytes
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aes_gcm::Aes128Gcm;
    use std::io;

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap()).collect()
    }
    fn arr<const N: usize>(s: &str) -> [u8; N] {
        unhex(s).try_into().unwrap()
    }

    // RFC 8448 section 3 "Simple 1-RTT Handshake" (https://www.rfc-editor.org/rfc/rfc8448.txt,
    // sha256 6564d1376d1ec744fc7a9993da15ebc1b9be361908b166091f47ef605c537fba).
    // Every value below is copied from the RFC text by script; the line numbers are in that file.
    // Server handshake traffic key and IV (lines 367, 372):
    const S_HS_KEY: &str = "3fce516009c21727d0f2e4e86ee403bc";
    const S_HS_IV: &str = "5d313eb2671276ee13000b30";
    // Client handshake traffic key and IV (606, 611):
    const C_HS_KEY: &str = "dbfaa693d1762c5b666af5d950258d01";
    const C_HS_IV: &str = "5bd3c71b836e0b76bb73265f";
    // Server application traffic key and IV (592, 597):
    const S_AP_KEY: &str = "9f02283b6c9c07efc26bb9f2ac92e356";
    const S_AP_IV: &str = "cf782b88dd83549aadf1e984";
    // Client application traffic key and IV (713, 718):
    const C_AP_KEY: &str = "17422dda596ed5d9acd890e3c63f5051";
    const C_AP_IV: &str = "5b78923dee08579033e523d9";
    // Plaintext records: ClientHello (line 201, record version 0x0301) and ServerHello (354).
    const CH_REC: &str = concat!(
        "16030100c4010000c00303cb34ecb1e78163ba1c38c6dacb196a6dffa21a8d99",
        "12ec18a2ef6283024dece7000006130113031302010000910000000b00090000",
        "06736572766572ff01000100000a00140012001d001700180019010001010102",
        "0103010400230000003300260024001d002099381de560e4bd43d23d8e435a7d",
        "bafeb3c06e51c13cae4d5413691e529aaf2c002b0003020304000d0020001e04",
        "0305030603020308040805080604010501060102010402050206020202002d00",
        "020101001c00024001",
    );
    const SH_REC: &str = concat!(
        "160303005a020000560303a6af06a4121860dc5e6e60249cd34c95930c8ac5cb",
        "1434dac155772ed3e2692800130100002e00330024001d0020c9828876112095",
        "fe66762bdbf7c672e156d6cc253b833df1dd69b1b04e751f0f002b00020304",
    );
    // Server flight (EE, Certificate, CertificateVerify, Finished): payload 657 bytes (455), record 679 (490). Seq 0.
    const SFLIGHT_PAY: &str = concat!(
        "080000240022000a00140012001d00170018001901000101010201030104001c",
        "00024001000000000b0001b9000001b50001b0308201ac30820115a003020102",
        "020102300d06092a864886f70d01010b0500300e310c300a0603550403130372",
        "7361301e170d3136303733303031323335395a170d3236303733303031323335",
        "395a300e310c300a0603550403130372736130819f300d06092a864886f70d01",
        "0101050003818d0030818902818100b4bb498f8279303d980836399b36c6988c",
        "0c68de55e1bdb826d3901a2461eafd2de49a91d015abbc9a95137ace6c1af19e",
        "aa6af98c7ced43120998e187a80ee0ccb0524b1b018c3e0b63264d449a6d38e2",
        "2a5fda430846748030530ef0461c8ca9d9efbfae8ea6d1d03e2bd193eff0ab9a",
        "8002c47428a6d35a8d88d79f7f1e3f0203010001a31a301830090603551d1304",
        "023000300b0603551d0f0404030205a0300d06092a864886f70d01010b050003",
        "81810085aad2a0e5b9276b908c65f73a7267170618a54c5f8a7b337d2df7a594",
        "365417f2eae8f8a58c8f8172f9319cf36b7fd6c55b80f21a03015156726096fd",
        "335e5e67f2dbf102702e608ccae6bec1fc63a42a99be5c3eb7107c3c54e9b9eb",
        "2bd5203b1c3b84e0a8b2f759409ba3eac9d91d402dcc0cc8f8961229ac9187b4",
        "2b4de100000f000084080400805a747c5d88fa9bd2e55ab085a61015b7211f82",
        "4cd484145ab3ff52f1fda8477b0b7abc90db78e2d33a5c141a078653fa6bef78",
        "0c5ea248eeaaa785c4f394cab6d30bbe8d4859ee511f602957b15411ac027671",
        "459e46445c9ea58c181e818e95b8c3fb0bf3278409d3be152a3da5043e063dda",
        "65cdf5aea20d53dfacd42f74f3140000209b9b141d906337fbd2cbdce71df4de",
        "da4ab42c309572cb7fffee5454b78f0718",
    );
    const SFLIGHT_REC: &str = concat!(
        "17030302a2d1ff334a56f5bff6594a07cc87b580233f500f45e489e7f33af35e",
        "df7869fcf40aa40aa2b8ea73f848a7ca07612ef9f945cb960b4068905123ea78",
        "b111b429ba9191cd05d2a389280f526134aadc7fc78c4b729df828b5ecf7b13b",
        "d9aefb0e57f271585b8ea9bb355c7c79020716cfb9b1183ef3ab20e37d57a6b9",
        "d7477609aee6e122a4cf51427325250c7d0e509289444c9b3a648f1d71035d2e",
        "d65b0e3cdd0cbae8bf2d0b227812cbb360987255cc744110c453baa4fcd61092",
        "8d809810e4b7ed1a8fd991f06aa6248204797e36a6a73b70a2559c09ead68694",
        "5ba246ab66e5edd8044b4c6de3fcf2a89441ac66272fd8fb330ef8190579b368",
        "4596c960bd596eea520a56a8d650f563aad27409960dca63d3e688611ea5e22f",
        "4415cf9538d51a200c27034272968a264ed6540c84838d89f72c24461aad6d26",
        "f59ecaba9acbbb317b66d902f4f292a36ac1b639c637ce343117b65962224531",
        "7b49eeda0c6258f100d7d961ffb138647e92ea330faeea6dfa31c7a84dc3bd7e",
        "1b7a6c7178af36879018e3f252107f243d243dc7339d5684c8b0378bf30244da",
        "8c87c843f5e56eb4c5e8280a2b48052cf93b16499a66db7cca71e4599426f7d4",
        "61e66f99882bd89fc50800becca62d6c74116dbd2972fda1fa80f85df881edbe",
        "5a37668936b335583b599186dc5c6918a396fa48a181d6b6fa4f9d62d513afbb",
        "992f2b992f67f8afe67f76913fa388cb5630c8ca01e0c65d11c66a1e2ac4c859",
        "77b7c7a6999bbf10dc35ae69f5515614636c0b9b68c19ed2e31c0b3b66763038",
        "ebba42f3b38edc0399f3a9f23faa63978c317fc9fa66a73f60f0504de93b5b84",
        "5e275592c12335ee340bbc4fddd502784016e4b3be7ef04dda49f4b440a30cb5",
        "d2af939828fd4ae3794e44f94df5a631ede42c1719bfdabf0253fe5175be898e",
        "750edc53370d2b",
    );
    // Client Finished under the client handshake key (696, 701). Seq 0.
    const CFIN_PAY: &str = "14000020a8ec436d677634ae525ac1fcebe11a039ec17694fac6e98527b642f2edd5ce61";
    const CFIN_REC: &str = concat!(
        "170303003575ec4dc238cce60b298044a71e219c56cc77b0517fe9b93c7a4bfc",
        "44d87f38f80338ac98fc46deb384bd1caeacab6867d726c40546",
    );
    // Server NewSessionTicket under the server application key (776, 798). Seq 0, so server app data is seq 1.
    const S_NST_REC: &str = concat!(
        "17030300de3a6b8f90414a97d6959c3487680de5134a2b240e6cffac116e95d4",
        "1d6af8f6b580dcf3d11d63c758db289a015940252f55713e061dc13e078891a3",
        "8efbcf5753ad8ef170ad3c7353d16d9da773b9ca7f2b9fa1b6c0d4a3d03f75e0",
        "9c30ba1e62972ac46f75f7b981be63439b2999ce13064615139891d5e4c5b406",
        "f16e3fc181a77ca475840025db2f0a77f81b5ab05b94c01346755f69232c8651",
        "9d86cbeeac87aac347d143f9605d64f650db4d023e70e952ca49fe5137121c74",
        "bc2697687e248746d6df353005f3bce18696129c8153556b3b6c6779b37bf159",
        "85684f",
    );
    // Application data 00..31 (50 bytes), client (814, 820, seq 0) and server (825, 831, seq 1).
    const APP_PAY: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f202122232425262728292a2b2c2d2e2f3031";
    const C_APP_REC: &str = concat!(
        "1703030043a23f7054b62c94d0affafe8228ba55cbefacea42f914aa66bcab3f",
        "2b9819a8a5b46b395bd54a9a20441e2b62974e1f5a6292a2977014bd1e3deae6",
        "3aeebb21694915e4",
    );
    const S_APP_REC: &str = concat!(
        "17030300432e937e11ef4ac740e538ad36005fc4a46932fc3225d05f82aa1b36",
        "e30efaf97d90e6dffc602dcb501a59a8fcc49c4bf2e5f0a21c0047c2abf33254",
        "0dd032e167c2955d",
    );
    // close_notify (payload 01 00) under the application keys, seq 1: client (836, 847), server (850, 854).
    const C_ALERT_REC: &str = "1703030013c9872760655666b74d7ff1153efd6db6d0b0e3";
    const S_ALERT_REC: &str = "1703030013b58fd67166ebf599d24720cfbe7efa7a8864a9";

    const KEY: [u8; 16] = *b"record-test-key1";
    const IV: [u8; 12] = *b"record-iv-12";

    /// A stream double: reads from `rd` in chunks of `rchunk`, optionally `Interrupted`
    /// on every other call or failing at a byte count; writes to `wr` in chunks.
    struct Mock {
        rd: Vec<u8>,
        rpos: usize,
        rchunk: usize,
        interrupt: bool,
        tick: bool,
        rfail_at: Option<usize>,
        wr: Vec<u8>,
        wchunk: usize,
        wfail_at: Option<usize>,
    }
    impl Mock {
        fn new(rd: Vec<u8>) -> Self {
            Mock { rd, rpos: 0, rchunk: usize::MAX, interrupt: false, tick: false, rfail_at: None, wr: vec![], wchunk: usize::MAX, wfail_at: None }
        }
    }
    impl Read for Mock {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.interrupt {
                self.tick = !self.tick;
                if self.tick {
                    return Err(ErrorKind::Interrupted.into());
                }
            }
            let mut limit = self.rd.len();
            if let Some(f) = self.rfail_at {
                if self.rpos >= f {
                    return Err(ErrorKind::ConnectionReset.into());
                }
                limit = limit.min(f);
            }
            let n = buf.len().min(self.rchunk).min(limit - self.rpos);
            buf[..n].copy_from_slice(&self.rd[self.rpos..self.rpos + n]);
            self.rpos += n;
            Ok(n)
        }
    }
    impl Write for Mock {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let mut n = buf.len().min(self.wchunk);
            if let Some(f) = self.wfail_at {
                if self.wr.len() >= f {
                    return Err(ErrorKind::BrokenPipe.into());
                }
                n = n.min(f - self.wr.len());
            }
            self.wr.extend_from_slice(&buf[..n]);
            Ok(n)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn layer(rd: Vec<u8>) -> RecordLayer<Mock> {
        RecordLayer::new(Mock::new(rd))
    }
    fn keyed_rx(rd: Vec<u8>) -> RecordLayer<Mock> {
        let mut l = layer(rd);
        l.install_read_key(&KEY, &IV).unwrap();
        l
    }

    /// A reference encoder written separately from `write_one`: builds a protected record.
    fn wire(key: &[u8; 16], iv: &[u8; 12], seq: u64, ty: u8, content: &[u8], pad: usize) -> Vec<u8> {
        let mut inner = content.to_vec();
        inner.push(ty);
        inner.resize(inner.len() + pad, 0);
        let clen = inner.len() + TAG_LEN;
        let h = [23, 3, 3, (clen >> 8) as u8, clen as u8];
        let tag = Aes128Gcm::new_portable(key).seal_in_place_detached(&tls13_nonce(iv, seq), &h, &mut inner).unwrap();
        [&h[..], &inner, &tag].concat()
    }
    fn w(seq: u64, ty: u8, content: &[u8]) -> Vec<u8> {
        wire(&KEY, &IV, seq, ty, content, 0)
    }
    fn app(r: Result<Record<'_>, Error>) -> Vec<u8> {
        match r.unwrap() {
            Record::ApplicationData(d) => d.to_vec(),
            o => panic!("not app data: {o:?}"),
        }
    }
    fn hs(r: Result<Record<'_>, Error>) -> Vec<u8> {
        match r.unwrap() {
            Record::Handshake(d) => d.to_vec(),
            o => panic!("not handshake: {o:?}"),
        }
    }

    // ---- RFC 8448 known answers (independent of our own seal) ----

    #[test]
    fn rfc8448_plaintext_records() {
        let (ch, sh) = (unhex(CH_REC), unhex(SH_REC));
        // The ClientHello record has version 0x0301; the reader ignores the minor byte.
        let mut l = layer([ch.clone(), sh.clone()].concat());
        assert_eq!(hs(l.read_record()), ch[5..]);
        assert_eq!(hs(l.read_record()), sh[5..]);
        assert_eq!(l.read_record(), Err(Error::UnexpectedEof));
        // We write 0x0303 (allowed for every record, section 5.1); the rest matches.
        let mut l = layer(vec![]);
        l.write_handshake(&ch[5..]).unwrap();
        let mut expect = ch.clone();
        expect[2] = 3;
        assert_eq!(l.get_ref().wr, expect);
        let mut l = layer(vec![]);
        l.write_handshake(&sh[5..]).unwrap();
        assert_eq!(l.get_ref().wr, sh);
    }

    #[test]
    fn rfc8448_server_flight_read_and_write() {
        let rec = unhex(SFLIGHT_REC);
        assert_eq!((rec.len(), unhex(SFLIGHT_PAY).len()), (679, 657));
        // Read, one byte per read call (the worst fragmentation) and with Interrupted.
        let mut m = Mock::new(rec.clone());
        m.rchunk = 1;
        m.interrupt = true;
        let mut l = RecordLayer::new(m);
        l.install_read_key(&arr(S_HS_KEY), &arr(S_HS_IV)).unwrap();
        assert_eq!(hs(l.read_record()), unhex(SFLIGHT_PAY));
        assert_eq!(l.read_sequence(), 1);
        // Write: the exact bytes of the RFC come out (one record: 657 <= 2^14).
        let mut l = layer(vec![]);
        l.install_write_key(&arr(S_HS_KEY), &arr(S_HS_IV)).unwrap();
        l.write_handshake(&unhex(SFLIGHT_PAY)).unwrap();
        assert_eq!(l.get_ref().wr, rec);
        assert_eq!(l.write_sequence(), 1);
    }

    #[test]
    fn rfc8448_client_finished_and_app_data() {
        let mut l = layer(vec![]);
        l.install_write_key(&arr(C_HS_KEY), &arr(C_HS_IV)).unwrap();
        l.write_handshake(&unhex(CFIN_PAY)).unwrap();
        assert_eq!(l.get_ref().wr, unhex(CFIN_REC));
        // Client app data (seq 0) then close_notify (seq 1) under the application key.
        let mut l = layer(vec![]);
        l.install_write_key(&arr(C_AP_KEY), &arr(C_AP_IV)).unwrap();
        l.write_application_data(&unhex(APP_PAY)).unwrap();
        l.send_close_notify().unwrap();
        let expect = [unhex(C_APP_REC), unhex(C_ALERT_REC)].concat();
        assert_eq!(l.get_ref().wr, expect);
        // After close_notify our write side is closed.
        assert_eq!(l.write_application_data(b"x"), Err(Error::Closed));
        assert_eq!(l.send_close_notify(), Err(Error::Closed));
        // Server NewSessionTicket (seq 0), app data (seq 1), close_notify (seq 2), read.
        let mut l = layer([unhex(S_NST_REC), unhex(S_APP_REC), unhex(S_ALERT_REC)].concat());
        l.install_read_key(&arr(S_AP_KEY), &arr(S_AP_IV)).unwrap();
        assert!(matches!(l.read_record(), Ok(Record::Handshake(d)) if d[0] == 4)); // NewSessionTicket
        assert_eq!(app(l.read_record()), unhex(APP_PAY));
        assert_eq!(l.read_record(), Ok(Record::CloseNotify));
        assert_eq!(l.read_record(), Ok(Record::CloseNotify)); // repeats
        assert_eq!(l.read_sequence(), 0); // key dropped by close
    }

    #[test]
    fn read_ahead_across_key_change() {
        // ServerHello (plaintext) and the encrypted flight arrive in ONE read call.
        let data = [unhex(SH_REC), unhex(SFLIGHT_REC)].concat();
        let mut l = layer(data);
        assert_eq!(hs(l.read_record()), unhex(SH_REC)[5..]);
        l.install_read_key(&arr(S_HS_KEY), &arr(S_HS_IV)).unwrap();
        assert_eq!(hs(l.read_record()), unhex(SFLIGHT_PAY));
    }

    // ---- plaintext boundaries ----

    #[test]
    fn plaintext_limits() {
        // 2^14 bytes ok.
        let rec = [&[22u8, 3, 3, 0x40, 0x00][..], &vec![7u8; 16384]].concat();
        assert_eq!(hs(layer(rec).read_record()).len(), 16384);
        // 2^14 + 1: refused from the header alone (the body is never sent here).
        let mut l = layer(vec![22, 3, 3, 0x40, 0x01]);
        assert_eq!(l.read_record(), Err(Error::RecordOverflow));
        assert_eq!(l.get_ref().rpos, 5);
        assert_eq!(l.read_record(), Err(Error::Failed));
        // Empty handshake and alert records.
        assert_eq!(layer(vec![22, 3, 3, 0, 0]).read_record(), Err(Error::EmptyRecord));
        assert_eq!(layer(vec![21, 3, 3, 0, 0]).read_record(), Err(Error::EmptyRecord));
        // Plaintext application data is never allowed.
        assert_eq!(layer(vec![23, 3, 3, 0, 1, 0]).read_record(), Err(Error::UnexpectedRecord(23)));
        // Unknown types, including 24 (heartbeat) and 0.
        for t in [0u8, 1, 19, 24, 255] {
            assert_eq!(layer(vec![t, 3, 3, 0, 1, 0]).read_record(), Err(Error::BadContentType(t)));
        }
        // Version: major byte must be 3; the minor byte is ignored.
        assert_eq!(layer(vec![22, 2, 0, 0, 1, 0]).read_record(), Err(Error::BadVersion(0x0200)));
        assert_eq!(layer(vec![22, 3, 1, 0, 1, 9]).read_record(), Ok(Record::Handshake(&[9])));
        assert_eq!(layer(vec![22, 3, 0xff, 0, 1, 9]).read_record(), Ok(Record::Handshake(&[9])));
    }

    #[test]
    fn plaintext_alerts() {
        let mut l = layer(vec![21, 3, 3, 0, 2, 2, 40, 22, 3, 3, 0, 1, 1]);
        assert_eq!(l.read_record(), Err(Error::PeerAlert(40)));
        assert_eq!(l.read_record(), Err(Error::Failed)); // sticky; the next record is never read
        assert_eq!(layer(vec![21, 3, 3, 0, 1, 0]).read_record(), Err(Error::Malformed));
        assert_eq!(layer(vec![21, 3, 3, 0, 3, 1, 0, 0]).read_record(), Err(Error::Malformed));
        // The level byte is ignored: fatal-level close_notify is a close; warning-level user_canceled is an error.
        assert_eq!(layer(vec![21, 3, 3, 0, 2, 2, 0]).read_record(), Ok(Record::CloseNotify));
        assert_eq!(layer(vec![21, 3, 3, 0, 2, 1, 90]).read_record(), Err(Error::PeerAlert(90)));
        assert_eq!(layer(vec![21, 3, 3, 0, 2, 1, 200]).read_record(), Err(Error::PeerAlert(200)));
    }

    #[test]
    fn plaintext_write_rules() {
        let mut l = layer(vec![]);
        assert_eq!(l.write_application_data(b"x"), Err(Error::NotProtected));
        assert_eq!(l.write_handshake(b""), Err(Error::EmptyRecord));
        assert!(l.get_ref().wr.is_empty());
        l.write_handshake(&vec![1; 16385]).unwrap(); // two records: 16384 then 1
        let wr = &l.get_ref().wr;
        assert_eq!(&wr[..5], &[22, 3, 3, 0x40, 0]);
        assert_eq!(&wr[5 + 16384..5 + 16384 + 5], &[22, 3, 3, 0, 1]);
        assert_eq!(wr.len(), 5 + 16384 + 5 + 1);
        // A fatal alert in plaintext, then no more writes.
        let mut l = layer(vec![]);
        l.send_fatal_alert(40).unwrap();
        assert_eq!(l.get_ref().wr, [21, 3, 3, 0, 2, 2, 40]);
        assert_eq!(l.write_handshake(b"x"), Err(Error::Closed));
        // The compat CCS record.
        let mut l = layer(vec![]);
        l.write_compat_ccs().unwrap();
        assert_eq!(l.get_ref().wr, [20, 3, 3, 0, 1, 1]);
    }

    // ---- protected records ----

    #[test]
    fn protected_sizes_and_padding() {
        // Max content (2^14): body 2^14 + 1 + 16 = 16401 accepted.
        let big = vec![0xabu8; 16384];
        assert_eq!(app(keyed_rx(w(0, 23, &big)).read_record()), big);
        // Zero-length app data, and padded ones.
        assert_eq!(app(keyed_rx(w(0, 23, b"")).read_record()), b"");
        let rec = wire(&KEY, &IV, 0, 23, b"hello", 100);
        assert_eq!(rec.len(), 5 + 5 + 1 + 100 + 16);
        assert_eq!(app(keyed_rx(rec).read_record()), b"hello");
        // Max padding: content 10 + type + padding fills exactly 2^14 + 1 bytes.
        let rec = wire(&KEY, &IV, 0, 22, &[5; 10], MAX_INNER - 11);
        assert_eq!(rec.len(), HEADER_LEN + MAX_CIPHERTEXT);
        assert_eq!(hs(keyed_rx(rec).read_record()), [5; 10]);
        // One byte more: refused from the header (body not sent).
        let mut hdr = vec![23, 3, 3, 0x40, 0x12]; // 16402
        let mut l = keyed_rx(hdr.clone());
        assert_eq!(l.read_record(), Err(Error::RecordOverflow));
        hdr[3..5].copy_from_slice(&0x4100u16.to_be_bytes()); // 2^14 + 256 (the RFC wire maximum) and above
        assert_eq!(keyed_rx(hdr).read_record(), Err(Error::RecordOverflow));
        // A zero-only inner (also empty inner) has no type byte.
        assert_eq!(keyed_rx(wire(&KEY, &IV, 0, 0, b"", 15)).read_record(), Err(Error::AllPadding));
        let mut empty_inner = vec![];
        let h = [23u8, 3, 3, 0, 16];
        let tag = Aes128Gcm::new_portable(&KEY).seal_in_place_detached(&tls13_nonce(&IV, 0), &h, &mut empty_inner).unwrap();
        assert_eq!(keyed_rx([&h[..], &tag].concat()).read_record(), Err(Error::AllPadding));
        // Body shorter than a tag.
        assert_eq!(keyed_rx(vec![23, 3, 3, 0, 15, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]).read_record(), Err(Error::AuthFailed));
        // Types inside: change_cipher_spec, unknown, empty handshake, empty alert, alert of 3 bytes.
        assert_eq!(keyed_rx(w(0, 20, &[1])).read_record(), Err(Error::UnexpectedRecord(20)));
        assert_eq!(keyed_rx(w(0, 99, b"x")).read_record(), Err(Error::BadContentType(99)));
        assert_eq!(keyed_rx(w(0, 22, b"")).read_record(), Err(Error::EmptyRecord));
        assert_eq!(keyed_rx(w(0, 21, b"")).read_record(), Err(Error::EmptyRecord));
        assert_eq!(keyed_rx(w(0, 21, &[1, 0, 0])).read_record(), Err(Error::Malformed));
        assert_eq!(keyed_rx(w(0, 21, &[2, 40])).read_record(), Err(Error::PeerAlert(40)));
        assert_eq!(keyed_rx(w(0, 21, &[1, 0])).read_record(), Ok(Record::CloseNotify));
    }

    #[test]
    fn protected_state_rejects_plaintext_and_bad_headers() {
        // Plaintext handshake or alert once the read key is installed.
        assert_eq!(keyed_rx(vec![22, 3, 3, 0, 1, 0]).read_record(), Err(Error::UnexpectedRecord(22)));
        assert_eq!(keyed_rx(vec![21, 3, 3, 0, 2, 1, 0]).read_record(), Err(Error::UnexpectedRecord(21)));
        assert_eq!(keyed_rx(vec![9, 3, 3, 0, 1, 0]).read_record(), Err(Error::BadContentType(9)));
        // Protected record version must be exactly 0x0303.
        for v in [[3u8, 1], [3, 4], [2, 3]] {
            let mut r = w(0, 23, b"x");
            r[1..3].copy_from_slice(&v);
            assert_eq!(keyed_rx(r).read_record(), Err(Error::BadVersion(u16::from_be_bytes(v))));
        }
    }

    #[test]
    fn tampering_fails_closed() {
        let good = w(0, 23, b"secret payload");
        // Flip every single bit of the record: header (type, version, length) and body and tag.
        for i in 0..good.len() {
            for bit in 0..8 {
                let mut r = good.clone();
                r[i] ^= 1 << bit;
                r.extend_from_slice(&[0; 64]); // so a longer length is still readable
                let mut l = keyed_rx(r);
                assert!(l.read_record().is_err(), "byte {i} bit {bit} accepted");
                // Dead afterwards, and no plaintext was produced in the buffer.
                assert_eq!(l.read_record(), Err(Error::Failed));
                assert_eq!(l.read_sequence(), 0);
            }
        }
        // No plaintext on a tag failure: the body bytes stay ciphertext.
        let mut r = good.clone();
        *r.last_mut().unwrap() ^= 1;
        let mut l = keyed_rx(r.clone());
        assert_eq!(l.read_record(), Err(Error::AuthFailed));
        assert_eq!(&l.rbuf[..r.len()], &r[..]);
        // Wrong sequence number, wrong key, wrong IV.
        assert_eq!(keyed_rx(w(1, 23, b"x")).read_record(), Err(Error::AuthFailed));
        let mut l = layer(wire(&[9; 16], &IV, 0, 23, b"x", 0));
        l.install_read_key(&KEY, &IV).unwrap();
        assert_eq!(l.read_record(), Err(Error::AuthFailed));
        let mut l = layer(wire(&KEY, &[9; 12], 0, 23, b"x", 0));
        l.install_read_key(&KEY, &IV).unwrap();
        assert_eq!(l.read_record(), Err(Error::AuthFailed));
        // A replayed record fails (its nonce is old).
        let mut l = keyed_rx([w(0, 23, b"a"), w(0, 23, b"a")].concat());
        assert_eq!(app(l.read_record()), b"a");
        assert_eq!(l.read_record(), Err(Error::AuthFailed));
    }

    #[test]
    fn directions_are_independent() {
        let k2: [u8; 16] = *b"other-key-123456";
        let iv2: [u8; 12] = *b"other-iv-123";
        // Peer sends 3 records under (KEY, IV). We send 2 under (k2, iv2). Sequences differ.
        let rd = [w(0, 23, b"r0"), w(1, 23, b"r1"), w(2, 23, b"r2")].concat();
        let mut l = layer(rd);
        l.install_read_key(&KEY, &IV).unwrap();
        l.install_write_key(&k2, &iv2).unwrap();
        l.write_application_data(b"w0").unwrap();
        assert_eq!(app(l.read_record()), b"r0");
        l.write_application_data(b"w1").unwrap();
        assert_eq!((l.write_sequence(), l.read_sequence()), (2, 1));
        assert_eq!(app(l.read_record()), b"r1");
        assert_eq!(app(l.read_record()), b"r2");
        assert_eq!((l.write_sequence(), l.read_sequence()), (2, 3));
        let expect = [wire(&k2, &iv2, 0, 23, b"w0", 0), wire(&k2, &iv2, 1, 23, b"w1", 0)].concat();
        assert_eq!(l.get_ref().wr, expect);
    }

    #[test]
    fn write_matches_reference_for_many_sizes() {
        for n in [0usize, 1, 15, 16, 17, 255, 256, 1000, 16383, 16384, 16385, 40000] {
            let data: Vec<u8> = (0..n).map(|i| (i * 7) as u8).collect();
            let mut l = layer(vec![]);
            l.install_write_key(&KEY, &IV).unwrap();
            l.write_application_data(&data).unwrap();
            let expect: Vec<u8> = data.chunks(MAX_PLAINTEXT).enumerate().flat_map(|(s, c)| w(s as u64, 23, c)).collect();
            assert_eq!(l.get_ref().wr, expect, "n={n}");
        }
    }

    #[test]
    fn write_one_checks_size_before_sealing() {
        let mut l = layer(vec![]);
        l.install_write_key(&KEY, &IV).unwrap();
        assert_eq!(l.write_one(23, &vec![0; 16385], 0), Err(Error::RecordOverflow));
        assert_eq!(l.write_one(23, &[0; 10], MAX_INNER - 10), Err(Error::RecordOverflow));
        assert_eq!(l.write_sequence(), 0); // no nonce used
        assert!(l.get_ref().wr.is_empty());
        l.write_one(22, &[1; 10], MAX_INNER - 11).unwrap(); // exactly the maximum
        assert_eq!(l.get_ref().wr, wire(&KEY, &IV, 0, 22, &[1; 10], MAX_INNER - 11));
    }

    // ---- key limits ----

    #[test]
    fn write_key_limit_and_new_key() {
        let mut l = layer(vec![]);
        l.install_write_key(&KEY, &IV).unwrap();
        if let Tx::Keyed(t) = &mut l.tx {
            t.limit = 2;
        }
        l.write_application_data(b"a").unwrap();
        l.write_application_data(b"b").unwrap();
        let before = l.get_ref().wr.clone();
        assert_eq!(l.write_application_data(b"c"), Err(Error::KeyLimit));
        assert_eq!(l.send_close_notify(), Err(Error::KeyLimit));
        assert_eq!(l.get_ref().wr, before); // nothing sent, no nonce used
        // Not dead: a new key works, with sequence 0 again.
        assert_eq!(l.install_write_key(&KEY, &IV), Err(Error::SameKey));
        let k2 = [2u8; 16];
        l.install_write_key(&k2, &IV).unwrap();
        l.write_application_data(b"c").unwrap();
        assert_eq!(&l.get_ref().wr[before.len()..], &wire(&k2, &IV, 0, 23, b"c", 0)[..]);
        // Same key, different IV is a different nonce space: allowed.
        l.install_write_key(&k2, &[3; 12]).unwrap();
        // A multi-record write that does not fit sends nothing.
        if let Tx::Keyed(t) = &mut l.tx {
            t.limit = 2;
        }
        let n = l.get_ref().wr.len();
        assert_eq!(l.write_application_data(&vec![0; 3 * MAX_PLAINTEXT]), Err(Error::KeyLimit));
        assert_eq!(l.get_ref().wr.len(), n);
        l.write_application_data(&vec![0; 2 * MAX_PLAINTEXT]).unwrap();
    }

    #[test]
    fn sequence_never_wraps() {
        let mut l = layer(vec![]);
        l.install_write_key(&KEY, &IV).unwrap();
        if let Tx::Keyed(t) = &mut l.tx {
            t.limit = u64::MAX;
            t.seq = u64::MAX - 1;
        }
        l.write_application_data(b"last").unwrap();
        assert_eq!(l.write_sequence(), u64::MAX);
        assert_eq!(l.write_application_data(b"x"), Err(Error::KeyLimit));
        // Read side.
        let mut l = keyed_rx([w(u64::MAX - 1, 23, b"last"), vec![23, 3, 3, 0, 17, 0]].concat());
        if let Rx::Keyed(t) = &mut l.rx {
            t.limit = u64::MAX;
            t.seq = u64::MAX - 1;
        }
        assert_eq!(app(l.read_record()), b"last");
        assert_eq!(l.read_record(), Err(Error::KeyLimit));
        assert_eq!(l.read_record(), Err(Error::KeyLimit)); // not dead, nothing consumed
    }

    #[test]
    fn read_key_limit_and_new_key() {
        let k2 = [2u8; 16];
        let mut l = keyed_rx([w(0, 23, b"a"), w(1, 23, b"b"), wire(&k2, &IV, 0, 23, b"c", 0)].concat());
        if let Rx::Keyed(t) = &mut l.rx {
            t.limit = 2;
        }
        assert_eq!(app(l.read_record()), b"a");
        assert_eq!(app(l.read_record()), b"b");
        assert_eq!(l.read_record(), Err(Error::KeyLimit));
        l.install_read_key(&k2, &IV).unwrap();
        assert_eq!(app(l.read_record()), b"c");
        assert_eq!(MAX_RECORDS_PER_KEY, (2f64.powf(24.5)).floor() as u64);
    }

    // ---- I/O behavior ----

    #[test]
    fn fragmented_stream_with_compaction() {
        // Many records of mixed sizes, delivered in odd chunk sizes, some padded. The
        // read-ahead buffer wraps around many times.
        let mut data = vec![];
        let mut expect: Vec<(u8, Vec<u8>)> = vec![];
        for i in 0..300u64 {
            let n = ((i * 2654435761) % 16385) as usize;
            let body: Vec<u8> = (0..n).map(|j| (j as u64 + i) as u8).collect();
            let ty = if i % 3 == 0 && n > 0 { 22 } else { 23 };
            data.extend(wire(&KEY, &IV, i, ty, &body, (i % 5) as usize));
            expect.push((ty, body));
        }
        for chunk in [1usize, 7, 4096, 20000, 40000] {
            let mut m = Mock::new(data.clone());
            m.rchunk = chunk;
            let mut l = RecordLayer::new(m);
            l.install_read_key(&KEY, &IV).unwrap();
            for (ty, body) in &expect {
                match l.read_record().unwrap() {
                    Record::Handshake(d) => assert!(*ty == 22 && d == body),
                    Record::ApplicationData(d) => assert!(*ty == 23 && d == body),
                    o => panic!("{o:?}"),
                }
            }
            assert_eq!(l.read_record(), Err(Error::UnexpectedEof));
        }
    }

    #[test]
    fn short_writes_and_write_errors() {
        let mut m = Mock::new(vec![]);
        m.wchunk = 3;
        let mut l = RecordLayer::new(m);
        l.install_write_key(&KEY, &IV).unwrap();
        l.write_application_data(&vec![5; 20000]).unwrap();
        let expect: Vec<u8> = [vec![5u8; 16384], vec![5; 3616]].iter().enumerate().flat_map(|(s, c)| w(s as u64, 23, c)).collect();
        assert_eq!(l.get_ref().wr, expect);
        // A failure in the middle of a record kills the direction. The nonce is not reused.
        let mut m = Mock::new(vec![]);
        m.wfail_at = Some(10);
        let mut l = RecordLayer::new(m);
        l.install_write_key(&KEY, &IV).unwrap();
        assert_eq!(l.write_application_data(b"hello world"), Err(Error::Io(ErrorKind::BrokenPipe)));
        assert_eq!(l.write_application_data(b"again"), Err(Error::Failed));
        assert_eq!(l.send_close_notify(), Err(Error::Failed));
        assert_eq!(l.write_compat_ccs(), Err(Error::Failed));
        assert_eq!(l.install_write_key(&[1; 16], &IV), Err(Error::Failed)); // no revival
        assert_eq!(l.get_ref().wr.len(), 10);
        // Plaintext write failure too.
        let mut m = Mock::new(vec![]);
        m.wfail_at = Some(0);
        let mut l = RecordLayer::new(m);
        assert_eq!(l.write_handshake(b"x"), Err(Error::Io(ErrorKind::BrokenPipe)));
        assert_eq!(l.write_handshake(b"x"), Err(Error::Failed));
    }

    #[test]
    fn read_io_errors_are_terminal() {
        let mut m = Mock::new(w(0, 23, b"0123456789"));
        m.rfail_at = Some(8);
        let mut l = RecordLayer::new(m);
        l.install_read_key(&KEY, &IV).unwrap();
        assert_eq!(l.read_record(), Err(Error::Io(ErrorKind::ConnectionReset)));
        assert_eq!(l.read_record(), Err(Error::Failed));
        assert_eq!(l.install_read_key(&[1; 16], &IV), Err(Error::Failed));
    }

    #[test]
    fn eof_versus_close_notify() {
        // No bytes at all: raw EOF.
        assert_eq!(layer(vec![]).read_record(), Err(Error::UnexpectedEof));
        // Cut inside the header, inside the body, one byte short of the end: truncation.
        let rec = w(0, 23, b"data!");
        for cut in [1, 4, 5, 6, rec.len() - 1] {
            assert_eq!(keyed_rx(rec[..cut].to_vec()).read_record(), Err(Error::Truncated), "cut {cut}");
        }
        // Data, then raw EOF before close_notify.
        let mut l = keyed_rx(rec.clone());
        assert_eq!(app(l.read_record()), b"data!");
        assert_eq!(l.read_record(), Err(Error::UnexpectedEof));
        assert_eq!(l.read_record(), Err(Error::Failed));
        // Data, close_notify, garbage after it: the garbage is ignored; repeated reads say CloseNotify.
        let mut l = keyed_rx([rec, w(1, 21, &[1, 0]), vec![0xff; 9]].concat());
        assert_eq!(app(l.read_record()), b"data!");
        assert_eq!(l.read_record(), Ok(Record::CloseNotify));
        assert_eq!(l.read_record(), Ok(Record::CloseNotify));
        assert_eq!(l.install_read_key(&[1; 16], &IV), Err(Error::Closed));
        // Half-close: we can still write after the peer's close_notify.
        l.install_write_key(&KEY, &IV).unwrap();
        l.write_application_data(b"bye").unwrap();
    }

    // ---- change_cipher_spec ----

    #[test]
    fn compat_ccs_only_when_allowed() {
        let ccs = vec![20, 3, 3, 0, 1, 1];
        // Not allowed: error in plaintext and in protected state.
        assert_eq!(layer(ccs.clone()).read_record(), Err(Error::UnexpectedRecord(20)));
        assert_eq!(keyed_rx(ccs.clone()).read_record(), Err(Error::UnexpectedRecord(20)));
        // Allowed: dropped, and it does not use a sequence number. Several in a row too.
        let mut l = layer([ccs.clone(), unhex(SH_REC), ccs.clone(), ccs.clone(), w(0, 23, b"x")].concat());
        l.allow_compat_ccs(true);
        assert_eq!(hs(l.read_record()), unhex(SH_REC)[5..]);
        l.install_read_key(&KEY, &IV).unwrap();
        assert_eq!(app(l.read_record()), b"x"); // seq 0
        // Wrong body or length is refused even when allowed.
        for (bad, err) in [
            (vec![20, 3, 3, 0, 1, 2], Error::UnexpectedRecord(20)),
            (vec![20, 3, 3, 0, 2, 1, 1], Error::UnexpectedRecord(20)),
            (vec![20, 3, 3, 0, 0], Error::UnexpectedRecord(20)),
            (vec![20, 2, 0, 0, 1, 1], Error::BadVersion(0x0200)),
        ] {
            let mut l = layer(bad);
            l.allow_compat_ccs(true);
            assert_eq!(l.read_record(), Err(err));
        }
        // Allowed, then turned off after the peer Finished.
        let mut l = layer([ccs.clone(), ccs].concat());
        l.allow_compat_ccs(true);
        l.allow_compat_ccs(false);
        assert_eq!(l.read_record(), Err(Error::UnexpectedRecord(20)));
        // A protected CCS is refused even when allowed (tested in protected_sizes_and_padding too).
        let mut l = keyed_rx(w(0, 20, &[1]));
        l.allow_compat_ccs(true);
        assert_eq!(l.read_record(), Err(Error::UnexpectedRecord(20)));
    }

    #[test]
    fn alert_mapping() {
        assert_eq!(Error::AuthFailed.alert(), Some(20));
        assert_eq!(Error::RecordOverflow.alert(), Some(22));
        assert_eq!(Error::AllPadding.alert(), Some(10));
        assert_eq!(Error::Malformed.alert(), Some(50));
        assert_eq!(Error::UnexpectedEof.alert(), None);
        assert_eq!(Error::PeerAlert(40).alert(), None); // never answer an alert with an alert
    }
}
