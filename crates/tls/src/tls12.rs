//! TLS 1.2 client handshake (RFC 5246) for `client.rs`. Learning code, NOT audited.
//!
//! The ClientHello offers 1.3 and 1.2. When the ServerHello answers with 1.2, `ClientStream::handshake12` runs.
//! Flow: `ServerHello -> Certificate -> ServerKeyExchange -> ServerHelloDone -> ClientKeyExchange -> CCS -> Finished
//! -> (server) CCS -> Finished`.
//!
//! Decisions:
//! - Suites: `TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256` (0xC02B) and `TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256` (0xC02F).
//!   The crate has AES-128 only (no AES-256, no ChaCha20), so there is no SHA-384 PRF.
//! - Groups: X25519 and secp256r1 (P-256). Real servers need both: an OpenSSL server wants the group of an ECDSA
//!   certificate in `supported_groups`, and some servers (craigslist.org) answer with P-256 even when X25519 is first.
//!   The P-256 secret multiply is the variable-time one in `ecc.rs`. See the exception in its docs: one random secret
//!   per handshake.
//! - The chain goes through the same verifier as TLS 1.3 (trust store, host name, validity). The key of the leaf checks
//!   the ServerKeyExchange signature over `client_random || server_random || ServerECDHParams` (RFC 5246 7.4.3).
//!   The suite fixes the key type: 0xC02B needs an EC leaf, 0xC02F an RSA leaf.
//! - Signature schemes (all offered in the ClientHello): ECDSA 0x0403 and 0x0503 with a P-256 or P-384 leaf (TLS 1.2
//!   does not tie the hash to the curve), RSA-PSS 0x0804 and 0x0805, RSA PKCS#1 v1.5 0x0401 and 0x0501.
//! - `extended_master_secret` (RFC 7627) is offered. When the server agrees, the master secret uses the session hash.
//!   A server that does not agree gets the plain master secret: we have no resumption, no renegotiation and no client
//!   certificate, which are the cases the triple-handshake attack needs.
//! - No resumption (a ServerHello that echoes our session ID is an error), no renegotiation (any handshake message after
//!   the handshake is an error), no compression, no tickets (we do not offer the extension).
//! - A ServerHello random that ends in `DOWNGRD\x01` is an error: we offered 1.3 (RFC 8446 4.1.3).
//! - Secrets (pre-master, master secret, key block) are wiped when the handshake ends, also on an error.

use super::*;
use crate::hmac::Hmac;

/// Body of a ServerKeyExchange: params (4 bytes and a P-256 point of 65), scheme (2), signature length (2), RSA-4096 signature.
const MAX_SERVER_KEY_EXCHANGE: usize = 4 + 65 + 4 + 512;
const NAMED_CURVE: u8 = 3;
/// Verify data length (RFC 5246 7.4.9).
const VERIFY_DATA_LEN: usize = 12;

/// A secret that is wiped when it goes out of scope.
struct Wiped<const N: usize>([u8; N]);

impl<const N: usize> Drop for Wiped<N> {
    fn drop(&mut self) {
        wipe(&mut self.0);
    }
}

/// `P_SHA256(secret, label || seed)` (RFC 5246 5), as many bytes as `out` holds. The seed is given in parts.
pub(super) fn prf(secret: &[u8], label: &[u8], seed: &[&[u8]], out: &mut [u8]) {
    let base = Hmac::new(secret);
    let mut a = {
        let mut h = base.clone();
        h.update(label);
        seed.iter().for_each(|s| h.update(s));
        h.finalize()
    };
    for chunk in out.chunks_mut(32) {
        let mut h = base.clone();
        h.update(&a);
        h.update(label);
        seed.iter().for_each(|s| h.update(s));
        chunk.copy_from_slice(&h.finalize()[..chunk.len()]);
        let mut h = base.clone();
        h.update(&a);
        a = h.finalize();
    }
    wipe(&mut a);
}

/// Master secret (RFC 5246 8.1). With `session_hash` it is the extended one (RFC 7627 4): the seed is the hash of the
/// handshake through ClientKeyExchange.
fn master_secret(pms: &[u8], client_random: &[u8], server_random: &[u8], session_hash: Option<[u8; 32]>) -> Wiped<48> {
    let mut ms = Wiped([0u8; 48]);
    match session_hash {
        Some(h) => prf(pms, b"extended master secret", &[&h], &mut ms.0),
        None => prf(pms, b"master secret", &[client_random, server_random], &mut ms.0),
    }
    ms
}

/// The key block of an AES-128-GCM suite (RFC 5246 6.3, RFC 5288 3): both keys, then both 4-byte salts.
struct Keys {
    client_key: [u8; 16],
    server_key: [u8; 16],
    client_salt: [u8; 4],
    server_salt: [u8; 4],
}

impl Drop for Keys {
    fn drop(&mut self) {
        for k in [&mut self.client_key[..], &mut self.server_key, &mut self.client_salt, &mut self.server_salt] {
            wipe(k);
        }
    }
}

fn key_block(ms: &Wiped<48>, client_random: &[u8], server_random: &[u8]) -> Keys {
    let mut b = Wiped([0u8; 40]);
    prf(&ms.0, b"key expansion", &[server_random, client_random], &mut b.0);
    let (b, mut k) = (&b.0, Keys { client_key: [0; 16], server_key: [0; 16], client_salt: [0; 4], server_salt: [0; 4] });
    k.client_key.copy_from_slice(&b[..16]);
    k.server_key.copy_from_slice(&b[16..32]);
    k.client_salt.copy_from_slice(&b[32..36]);
    k.server_salt.copy_from_slice(&b[36..40]);
    k
}

/// `verify_data` of a Finished message (RFC 5246 7.4.9): `label` is "client finished" or "server finished".
fn verify_data(ms: &Wiped<48>, label: &[u8], transcript_hash: &[u8; 32]) -> [u8; VERIFY_DATA_LEN] {
    let mut v = [0u8; VERIFY_DATA_LEN];
    prf(&ms.0, label, &[transcript_hash], &mut v);
    v
}

/// ALPN result for a ServerHello: `(http/1.1 chosen, h2 chosen)`. Same rule as TLS 1.3 EncryptedExtensions.
fn alpn(chosen: Option<&[u8]>, offer_h2: bool) -> Result<(bool, bool), Error> {
    match chosen {
        None => Ok((false, false)),
        Some(b"http/1.1") => Ok((true, false)),
        Some(b"h2") if offer_h2 => Ok((false, true)),
        Some(_) => Err(Error::Alpn),
    }
}

fn ecdsa(curve: ecc::Curve, point: &[u8], digest: &[u8], sig: &[u8]) -> Result<(), Error> {
    let xc = if curve == ecc::Curve::P256 { x509::Curve::P256 } else { x509::Curve::P384 };
    let es = x509::EcdsaSig::parse(sig, xc).map_err(|_| Error::BadSignature)?;
    let w = curve.size();
    let mut rs = [0u8; 96];
    rs[w - es.r.len()..w].copy_from_slice(es.r);
    rs[2 * w - es.s.len()..2 * w].copy_from_slice(es.s);
    ecc::verify(curve, point, digest, &rs[..2 * w]).map_err(|_| Error::BadSignature)
}

/// The server's ephemeral key from a ServerKeyExchange.
#[derive(Debug, PartialEq, Eq)]
enum ServerKey {
    X25519([u8; 32]),
    /// `04 || X || Y`.
    P256([u8; 65]),
}

/// ServerKeyExchange (RFC 5246 7.4.3, RFC 8422 5.4). Checks the signature and returns the server's ephemeral key.
/// `ecdsa_suite` is true for 0xC02B. The body holds `curve_type, named_curve, point, scheme, signature`.
fn verify_server_key_exchange(body: &[u8], key: &LeafKey, ecdsa_suite: bool, cr: &[u8; 32], sr: &[u8; 32]) -> Result<ServerKey, Error> {
    let mut r = R(body);
    if r.u8()? != NAMED_CURVE as usize {
        return Err(Error::Unsupported("ServerKeyExchange curve type"));
    }
    let curve = r.u16()? as u16;
    let point = r.vec8()?;
    let server_key = match curve {
        hello::GROUP_X25519 => ServerKey::X25519(point.try_into().map_err(|_| Error::Illegal("X25519 public key length"))?),
        hello::GROUP_SECP256R1 => ServerKey::P256(point.try_into().map_err(|_| Error::Illegal("P-256 public key length"))?),
        _ => return Err(Error::Unsupported("ServerKeyExchange group is not X25519 or secp256r1")),
    };
    let params = &body[..body.len() - r.0.len()];
    let scheme = r.u16()? as u16;
    let sig = r.vec16()?;
    r.done()?;
    let mut signed = [0u8; 64 + 4 + 65];
    signed[..32].copy_from_slice(cr);
    signed[32..64].copy_from_slice(sr);
    signed[64..64 + params.len()].copy_from_slice(params); // params.len() is 36 or 69: it holds a point of 32 or 65 bytes
    let signed = &signed[..64 + params.len()];
    let sha384_scheme = match scheme {
        hello::ECDSA_SECP256R1_SHA256 | hello::RSA_PSS_RSAE_SHA256 | hello::RSA_PKCS1_SHA256 => false,
        hello::ECDSA_SECP384R1_SHA384 | hello::RSA_PSS_RSAE_SHA384 | hello::RSA_PKCS1_SHA384 => true,
        _ => return Err(Error::Illegal("ServerKeyExchange scheme was not offered")),
    };
    let mut full = [0u8; 48];
    let digest = if sha384_scheme {
        full = sha384(signed);
        &full[..]
    } else {
        full[..32].copy_from_slice(&sha256(signed));
        &full[..32]
    };
    let fit = Error::Illegal("ServerKeyExchange scheme does not fit the cipher suite and leaf key");
    match key {
        LeafKey::P256(p) if ecdsa_suite && matches!(scheme, hello::ECDSA_SECP256R1_SHA256 | hello::ECDSA_SECP384R1_SHA384) => {
            ecdsa(ecc::Curve::P256, p, digest, sig)?
        }
        LeafKey::P384(p) if ecdsa_suite && matches!(scheme, hello::ECDSA_SECP256R1_SHA256 | hello::ECDSA_SECP384R1_SHA384) => {
            ecdsa(ecc::Curve::P384, p, digest, sig)?
        }
        LeafKey::Rsa { n, len, e } if !ecdsa_suite => {
            let k = rsa::PublicKey::new(&n[..*len], &e.to_be_bytes()).ok_or(Error::BadSignature)?;
            let hash = if sha384_scheme { rsa::Hash::Sha384 } else { rsa::Hash::Sha256 };
            match scheme {
                hello::RSA_PSS_RSAE_SHA256 | hello::RSA_PSS_RSAE_SHA384 => k.verify_pss(hash, digest, sig),
                hello::RSA_PKCS1_SHA256 | hello::RSA_PKCS1_SHA384 => k.verify_pkcs1(hash, digest, sig),
                _ => return Err(fit),
            }
            .map_err(|_| Error::BadSignature)?
        }
        _ => return Err(fit),
    }
    Ok(server_key)
}

impl<T: Read + Write> ClientStream<T> {
    /// The rest of the handshake after a TLS 1.2 ServerHello. `n` is the length of that message at `hs[hs_pos..]`.
    /// `t` holds the transcript through the ClientHello. `public` is our X25519 public key (sent in the ClientHello too).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn handshake12<V: ChainVerifier>(
        &mut self,
        host: &str,
        verifier: &V,
        now: i64,
        rnd: &[u8; RANDOM_LEN],
        public: &[u8; 32],
        offer_h2: bool,
        mut t: Transcript,
        n: usize,
    ) -> Result<(), Error> {
        let scalar: &[u8; 32] = rnd[..32].try_into().map_err(|_| Error::Decode("scalar"))?;
        let client_random: &[u8; 32] = rnd[32..64].try_into().map_err(|_| Error::Decode("client random"))?;
        self.v12 = true;

        // ServerHello.
        let msg = &self.hs[self.hs_pos..self.hs_pos + n];
        let sh = hello::parse_server_hello12(msg, &rnd[64..])?;
        (self.http11, self.h2) = alpn(sh.alpn, offer_h2)?;
        let (server_random, ems, ecdsa_suite) = (sh.random, sh.ems, sh.suite == hello::TLS_ECDHE_ECDSA_AES_128_GCM_SHA256);
        t.update(msg);
        self.consume(n);

        // Certificate: no context and no entry extensions in TLS 1.2.
        let n = self.read_message(hello::CERTIFICATE, MAX_CERTIFICATE)?;
        let msg = &self.hs[self.hs_pos..self.hs_pos + n];
        let mut chain: [&[u8]; MAX_CHAIN] = [&[]; MAX_CHAIN];
        let count = parse_certificate(&msg[hello::HANDSHAKE_HEADER_LEN..], &mut chain, false)?;
        verifier.verify_chain(&chain[..count], host, now).map_err(Error::Certificate)?;
        let key = leaf_key(chain[0])?;
        t.update(msg);
        self.consume(n);

        // ServerKeyExchange: the leaf key signs the server's X25519 key and both randoms.
        let n = self.read_message(hello::SERVER_KEY_EXCHANGE, MAX_SERVER_KEY_EXCHANGE)?;
        let msg = &self.hs[self.hs_pos..self.hs_pos + n];
        let server_key = verify_server_key_exchange(&msg[hello::HANDSHAKE_HEADER_LEN..], &key, ecdsa_suite, client_random, &server_random)?;
        t.update(msg);
        self.consume(n);

        // ServerHelloDone. Nothing may follow it before our reply.
        let n = self.read_message(hello::SERVER_HELLO_DONE, 0)?;
        t.update(&self.hs[self.hs_pos..self.hs_pos + n]);
        self.consume(n);
        self.require_boundary()?;

        // ClientKeyExchange, then the keys. P-256 needs a second random secret: the X25519 one went into the ClientHello.
        let mut cke = [0u8; 4 + 1 + 65];
        let (pms, cke) = match server_key {
            ServerKey::X25519(server_public) => {
                cke[..4].copy_from_slice(&hello::write_header(hello::CLIENT_KEY_EXCHANGE, 33)?);
                cke[4] = 32;
                cke[5..37].copy_from_slice(public);
                (Wiped(x25519::shared_secret(scalar, &server_public).map_err(|_| Error::KeyExchange)?), &mut cke[..37])
            }
            ServerKey::P256(server_public) => {
                let mut secret = Wiped([0u8; 32]);
                random::fill(&mut secret.0).map_err(|_| Error::Random)?;
                let mine = ecc::p256_public(&secret.0).ok_or(Error::KeyExchange)?;
                cke[..4].copy_from_slice(&hello::write_header(hello::CLIENT_KEY_EXCHANGE, 66)?);
                cke[4] = 65;
                cke[5..].copy_from_slice(&mine);
                (Wiped(ecc::p256_shared(&secret.0, &server_public).map_err(|_| Error::KeyExchange)?), &mut cke[..])
            }
        };
        t.update(cke);
        self.rl.write_handshake(cke)?;
        let ms = master_secret(&pms.0, client_random, &server_random, ems.then(|| t.hash()));
        drop(pms);
        let keys = key_block(&ms, client_random, &server_random);

        // change_cipher_spec and our Finished, now protected.
        self.rl.write_compat_ccs()?;
        self.rl.install_write_key12(&keys.client_key, &keys.client_salt)?;
        let mut fin = [0u8; 4 + VERIFY_DATA_LEN];
        fin[..4].copy_from_slice(&hello::write_header(hello::FINISHED, VERIFY_DATA_LEN)?);
        fin[4..].copy_from_slice(&verify_data(&ms, b"client finished", &t.hash()));
        t.update(&fin);
        self.rl.write_handshake(&fin)?;
        self.rl.get_mut().flush().map_err(|e| Error::Io(e.kind()))?;

        // The server's change_cipher_spec and Finished.
        self.rl.install_read_key12(&keys.server_key, &keys.server_salt)?;
        let n = self.read_message(hello::FINISHED, VERIFY_DATA_LEN)?;
        let want = verify_data(&ms, b"server finished", &t.hash());
        let got = &self.hs[self.hs_pos + hello::HANDSHAKE_HEADER_LEN..self.hs_pos + n];
        if got.iter().zip(&want).fold(got.len() ^ want.len(), |d, (a, b)| d | (a ^ b) as usize) != 0 {
            return Err(Error::BadFinished);
        }
        self.consume(n);
        self.require_boundary()
    }
}

#[cfg(test)]
mod tests {
    use super::super::fx::*;
    use super::*;
    use crate::aes_gcm::Aes128Gcm;
    use crate::hmac::hmac_sha256;
    use std::cell::RefCell;
    use std::rc::Rc;

    const V: &str = include_str!("../../../tools/fixtures/tls12/vectors.txt");
    /// ECDSA-SHA256 over the ServerKeyExchange of the loopback server, by the key of `LEAF`.
    const SKE_SIG: &[u8] = include_bytes!("../../../tools/fixtures/client/ske_ecdsa.der");
    /// The same for a P-256 ephemeral key (`ecc::p256_public` of `SERVER_SCALAR`).
    const SKE_SIG_P256: &[u8] = include_bytes!("../../../tools/fixtures/client/ske_ecdsa_p256.der");
    const SERVER_RANDOM: [u8; 32] = [0x22; 32];

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap()).collect()
    }
    fn vectors(kind: &str) -> Vec<Vec<&'static str>> {
        V.lines().map(|l| l.split(' ').collect::<Vec<_>>()).filter(|f| f[0] == kind).collect()
    }
    /// The PRF again, written apart from the code under test (plain HMAC calls).
    fn ref_prf(secret: &[u8], label: &[u8], seed: &[u8], n: usize) -> Vec<u8> {
        let s = [label, seed].concat();
        let (mut a, mut out) = (s.clone(), Vec::new());
        while out.len() < n {
            a = hmac_sha256(secret, &a).to_vec();
            out.extend_from_slice(&hmac_sha256(secret, &[&a[..], &s].concat()));
        }
        out.truncate(n);
        out
    }

    // ---- key derivation ----

    #[test]
    fn prf_known_answer() {
        let v = &vectors("prf")[0];
        let (secret, label, seed, want) = (unhex(v[1]), unhex(v[2]), unhex(v[3]), unhex(v[4]));
        let mut out = vec![0u8; want.len()];
        prf(&secret, &label, &[&seed], &mut out);
        assert_eq!(out, want);
        // The seed in parts gives the same bytes, and a shorter output is a prefix.
        let mut split = vec![0u8; 33];
        prf(&secret, &label, &[&seed[..5], &seed[5..]], &mut split);
        assert_eq!(split, want[..33]);
        assert_eq!(ref_prf(&secret, &label, &seed, 100), want);
    }

    #[test]
    fn master_secret_and_key_block_known_answer() {
        let v = &vectors("kdf")[0];
        let f: Vec<Vec<u8>> = v[1..].iter().map(|s| unhex(s)).collect();
        let (pms, cr, sr, sh) = (&f[0], &f[1], &f[2], <[u8; 32]>::try_from(&f[3][..]).unwrap());
        let ems = master_secret(pms, cr, sr, Some(sh));
        assert_eq!(ems.0[..], f[4][..]);
        assert_eq!(master_secret(pms, cr, sr, None).0[..], f[5][..]);
        let k = key_block(&ems, cr, sr);
        assert_eq!([&k.client_key[..], &k.server_key, &k.client_salt, &k.server_salt].concat(), f[6]);
        assert_eq!(verify_data(&ems, b"client finished", &[0x66; 32])[..], f[7][..]);
    }

    // ---- record protection ----

    fn keyed_pair(chunk: usize, input: Vec<u8>) -> RecordLayer<Script> {
        let mut rl = RecordLayer::new(Script::new(input, chunk));
        rl.install_write_key12(&[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15], &[0xA0, 0xA1, 0xA2, 0xA3]).unwrap();
        rl
    }

    #[test]
    fn record_framing_known_answer() {
        for v in vectors("rec") {
            let (seq, ty, plain, rec) = (v[3].parse::<u64>().unwrap(), v[4].parse::<u8>().unwrap(), unhex(v[5]), unhex(v[6]));
            let mut rl = keyed_pair(usize::MAX, Vec::new());
            // Reach the sequence number of the vector with records of the same type.
            for _ in 0..seq {
                if ty == 22 { rl.write_handshake(&[1]).unwrap() } else { rl.write_application_data(&[1]).unwrap() }
            }
            rl.get_mut().output.clear();
            if ty == 22 { rl.write_handshake(&plain).unwrap() } else { rl.write_application_data(&plain).unwrap() }
            assert_eq!(rl.get_ref().output, rec, "seq {seq} type {ty}");
            assert_eq!(rl.write_sequence(), seq + 1);
        }
    }

    /// Server's view: the CCS, then the first vector (a protected handshake record under seq 0).
    fn read_layer(rec: &[u8], chunk: usize) -> RecordLayer<Script> {
        let mut rl = RecordLayer::new(Script::new([&[20, 3, 3, 0, 1, 1][..], rec].concat(), chunk));
        rl.install_read_key12(&[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15], &[0xA0, 0xA1, 0xA2, 0xA3]).unwrap();
        rl
    }

    #[test]
    fn record_reads_the_known_record_and_rejects_every_tamper() {
        let v = &vectors("rec")[0];
        let (plain, rec) = (unhex(v[5]), unhex(v[6]));
        for chunk in [usize::MAX, 1, 7] {
            let mut rl = read_layer(&rec, chunk);
            assert_eq!(rl.read_record().unwrap(), Record::Handshake(&plain));
        }
        // One flipped bit anywhere after the length field: header type or version, explicit nonce, body, tag.
        for i in (0..rec.len()).filter(|&i| i != 3 && i != 4) {
            let mut bad = rec.clone();
            bad[i] ^= 1;
            let mut rl = read_layer(&bad, usize::MAX);
            assert!(rl.read_record().is_err(), "byte {i}");
        }
        // The explicit nonce is part of the nonce: another one fails too, even with the right bytes otherwise.
        let mut rl = read_layer(&rec[..rec.len() - 1], usize::MAX);
        assert!(rl.read_record().is_err()); // truncated
        // A record before the change_cipher_spec is refused: the first record must be the CCS.
        let mut rl = RecordLayer::new(Script::new(rec.clone(), usize::MAX));
        rl.install_read_key12(&[0; 16], &[0; 4]).unwrap();
        assert_eq!(rl.read_record().unwrap_err(), record::Error::UnexpectedRecord(22));
        // A CCS with another value is refused.
        let mut rl = RecordLayer::new(Script::new(vec![20, 3, 3, 0, 1, 2], usize::MAX));
        rl.install_read_key12(&[0; 16], &[0; 4]).unwrap();
        assert_eq!(rl.read_record().unwrap_err(), record::Error::UnexpectedRecord(20));
        // A plaintext alert before the CCS shows its code.
        let mut rl = RecordLayer::new(Script::new(vec![21, 3, 3, 0, 2, 2, 40], usize::MAX));
        rl.install_read_key12(&[0; 16], &[0; 4]).unwrap();
        assert_eq!(rl.read_record().unwrap_err(), record::Error::PeerAlert(40));
        // A body shorter than nonce plus tag, and one over the limit.
        for (len, want) in [(23u16, record::Error::AuthFailed), (16384 + 25, record::Error::RecordOverflow)] {
            let mut input = vec![20, 3, 3, 0, 1, 1, 23, 3, 3];
            input.extend_from_slice(&len.to_be_bytes());
            let mut rl = RecordLayer::new(Script::new(input, usize::MAX));
            rl.install_read_key12(&[0; 16], &[0; 4]).unwrap();
            assert_eq!(rl.read_record().unwrap_err(), want);
        }
    }

    #[test]
    fn record_round_trip_at_the_size_limits() {
        // A writer and a reader that share the key: a full 16384-byte record is 16408 bytes of body.
        let data: Vec<u8> = (0..40_000u32).map(|i| (i * 31) as u8).collect();
        let mut w = keyed_pair(usize::MAX, Vec::new());
        w.write_application_data(&data).unwrap();
        let wire = w.get_ref().output.clone();
        assert_eq!(wire.len(), data.len() + 3 * (5 + 8 + 16));
        assert_eq!(u16::from_be_bytes([wire[3], wire[4]]), 16384 + 8 + 16);
        let mut r = read_layer(&[], usize::MAX);
        r.get_mut().input.extend_from_slice(&wire);
        let mut got = Vec::new();
        while got.len() < data.len() {
            match r.read_record().unwrap() {
                Record::ApplicationData(d) => got.extend_from_slice(d),
                x => panic!("{x:?}"),
            }
        }
        assert_eq!(got, data);
    }

    // ---- ServerKeyExchange ----

    fn leaf_for(kind: &str, key: &[u8]) -> LeafKey {
        match kind {
            "p256" => LeafKey::P256(key.try_into().unwrap()),
            "p384" => LeafKey::P384(key.try_into().unwrap()),
            _ => {
                let k = rsa::PublicKey::from_pkcs1(key).unwrap();
                let mut n = [0u8; rsa::MAX_BITS / 8];
                n[..k.modulus().len()].copy_from_slice(k.modulus());
                LeafKey::Rsa { n, len: k.modulus().len(), e: k.exponent() }
            }
        }
    }
    fn ske_body(scheme: u16, sig: &[u8]) -> Vec<u8> {
        ske_body_for(0x001d, &[0x33; 32], scheme, sig)
    }
    fn ske_body_for(curve: u16, point: &[u8], scheme: u16, sig: &[u8]) -> Vec<u8> {
        let mut b = vec![3];
        b.extend_from_slice(&curve.to_be_bytes());
        b.push(point.len() as u8);
        b.extend_from_slice(point);
        b.extend_from_slice(&scheme.to_be_bytes());
        b.extend_from_slice(&(sig.len() as u16).to_be_bytes());
        b.extend_from_slice(sig);
        b
    }

    #[test]
    fn server_key_exchange_signatures_of_every_scheme() {
        let (rows, p256_rows) = (vectors("ske"), vectors("skp"));
        assert_eq!((rows.len(), p256_rows.len()), (8, 8));
        // The same keys sign the same randoms with a P-256 point of 65 bytes (04 || 0x33 * 64).
        let p256_point = [&[4u8][..], &[0x33; 64]].concat();
        for v in p256_rows {
            let (scheme, key, sig) = (u16::from_str_radix(v[2], 16).unwrap(), leaf_for(v[3], &unhex(v[4])), unhex(v[5]));
            let body = ske_body_for(0x0017, &p256_point, scheme, &sig);
            let got = verify_server_key_exchange(&body, &key, v[3] != "rsa", &[0x11; 32], &[0x22; 32]);
            assert_eq!(got, Ok(ServerKey::P256(p256_point.clone().try_into().unwrap())), "{}", v[1]);
            let mut bad = body.clone();
            bad[20] ^= 1; // a bit of the point
            assert_eq!(verify_server_key_exchange(&bad, &key, v[3] != "rsa", &[0x11; 32], &[0x22; 32]), Err(Error::BadSignature));
            // A 32-byte point with the P-256 group is the wrong length.
            let short = ske_body_for(0x0017, &[0x33; 32], scheme, &sig);
            assert!(matches!(verify_server_key_exchange(&short, &key, v[3] != "rsa", &[0x11; 32], &[0x22; 32]), Err(Error::Illegal(_))));
        }
        for v in rows {
            let (scheme, kind, key, sig) = (u16::from_str_radix(v[2], 16).unwrap(), v[3], leaf_for(v[3], &unhex(v[4])), unhex(v[5]));
            let ecdsa_suite = kind != "rsa";
            let ok = |body: &[u8], key: &LeafKey, suite: bool| verify_server_key_exchange(body, key, suite, &[0x11; 32], &[0x22; 32]);
            assert_eq!(ok(&ske_body(scheme, &sig), &key, ecdsa_suite), Ok(ServerKey::X25519([0x33; 32])), "{}", v[1]);
            // The suite fixes the key type.
            assert!(matches!(ok(&ske_body(scheme, &sig), &key, !ecdsa_suite), Err(Error::Illegal(_))), "{}", v[1]);
            // A flipped bit in the signature, in the signed point, in the scheme and in either random.
            let mut bad = sig.clone();
            let last = bad.len() - 1;
            bad[last] ^= 1;
            assert!(ok(&ske_body(scheme, &bad), &key, ecdsa_suite).is_err(), "{}", v[1]);
            let mut body = ske_body(scheme, &sig);
            body[10] ^= 1;
            assert_eq!(ok(&body, &key, ecdsa_suite), Err(Error::BadSignature), "{}", v[1]);
            assert_eq!(verify_server_key_exchange(&ske_body(scheme, &sig), &key, ecdsa_suite, &[0x12; 32], &[0x22; 32]), Err(Error::BadSignature));
            assert_eq!(verify_server_key_exchange(&ske_body(scheme, &sig), &key, ecdsa_suite, &[0x11; 32], &[0x23; 32]), Err(Error::BadSignature));
            // A scheme that was not offered, and one that does not fit the key type.
            assert!(matches!(ok(&ske_body(0x0201, &sig), &key, ecdsa_suite), Err(Error::Illegal(_))));
            let other = if ecdsa_suite { hello::RSA_PSS_RSAE_SHA256 } else { hello::ECDSA_SECP256R1_SHA256 };
            assert!(ok(&ske_body(other, &sig), &key, ecdsa_suite).is_err());
            // Another group (secp384r1), a short point, trailing bytes and a cut body.
            let mut body = ske_body(scheme, &sig);
            body[2] = 0x18;
            assert!(matches!(ok(&body, &key, ecdsa_suite), Err(Error::Unsupported(_))));
            let mut body = ske_body(scheme, &sig);
            body[3] = 31;
            assert!(ok(&body, &key, ecdsa_suite).is_err());
            let mut body = ske_body(scheme, &sig);
            body.push(0);
            assert!(ok(&body, &key, ecdsa_suite).is_err());
            for cut in 0..body.len() - 1 {
                assert!(ok(&body[..cut], &key, ecdsa_suite).is_err());
            }
        }
    }

    // ---- loopback against a TLS 1.2 server written here ----

    #[derive(Clone)]
    struct Cfg {
        suite: u16,
        version: u16,
        ems: bool,
        tail: Option<[u8; 8]>,
        echo_sid: bool,
        compression: u8,
        extra_ext: Vec<u8>,
        alpn: Option<&'static [u8]>,
        curve: u16,
        flip_sig: bool,
        bad_finished: bool,
        skip_ccs: bool,
        legacy_ms: bool,
    }
    fn good() -> Cfg {
        Cfg {
            suite: 0xC02B, version: 0x0303, ems: true, tail: None, echo_sid: false, compression: 0, extra_ext: vec![], alpn: Some(b"http/1.1"),
            curve: 0x001d, flip_sig: false, bad_finished: false, skip_ccs: false, legacy_ms: false,
        }
    }

    #[derive(PartialEq)]
    enum Stage { Hello, Kx, Ccs, Fin, App }

    /// A minimal TLS 1.2 server. It runs when the client reads and finds nothing to read. It uses `ref_prf` and
    /// `Aes128Gcm` directly, not the record layer or the PRF of the client.
    struct Server {
        cfg: Cfg,
        stage: Stage,
        inbuf: Vec<u8>,
        pos: usize,
        out: Vec<u8>,
        t: Transcript,
        ms: Vec<u8>,
        keys: Vec<u8>,
        rseq: u64,
        wseq: u64,
        pub app: Vec<u8>,
        pub alerts: Vec<(u8, u8)>,
        pub client_hello: Vec<u8>,
        pub finished_ok: bool,
    }

    fn m(ty: u8, body: &[u8]) -> Vec<u8> {
        msg(ty, body)
    }
    fn aad(seq: u64, ty: u8, n: usize) -> Vec<u8> {
        [&seq.to_be_bytes()[..], &[ty, 3, 3], &(n as u16).to_be_bytes()].concat()
    }

    impl Server {
        fn new(cfg: Cfg) -> Rc<RefCell<Server>> {
            Rc::new(RefCell::new(Server {
                cfg, stage: Stage::Hello, inbuf: vec![], pos: 0, out: vec![], t: Transcript::new(), ms: vec![], keys: vec![], rseq: 0, wseq: 0,
                app: vec![], alerts: vec![], client_hello: vec![], finished_ok: false,
            }))
        }
        fn plain(&mut self, ty: u8, body: &[u8]) {
            self.out.extend_from_slice(&[ty, 3, 3]);
            self.out.extend_from_slice(&(body.len() as u16).to_be_bytes());
            self.out.extend_from_slice(body);
        }
        fn send_enc(&mut self, ty: u8, body: &[u8]) {
            let (key, salt) = (&self.keys[16..32], &self.keys[36..40]);
            let aead = Aes128Gcm::new(key.try_into().unwrap());
            let explicit = self.wseq.to_be_bytes();
            let nonce: [u8; 12] = [salt, &explicit[..]].concat().try_into().unwrap();
            let mut buf = body.to_vec();
            let tag = aead.seal_in_place_detached(&nonce, &aad(self.wseq, ty, body.len()), &mut buf).unwrap();
            self.wseq += 1;
            let rec = [&explicit[..], &buf, &tag].concat();
            self.plain(ty, &rec);
        }
        fn decrypt(&mut self, ty: u8, body: &[u8]) -> Option<Vec<u8>> {
            let (key, salt) = (&self.keys[..16], &self.keys[32..36]);
            let aead = Aes128Gcm::new(key.try_into().unwrap());
            let n = body.len() - 8 - 16;
            let nonce: [u8; 12] = [salt, &body[..8]].concat().try_into().unwrap();
            let mut buf = body[8..8 + n].to_vec();
            aead.open_in_place_detached(&nonce, &aad(self.rseq, ty, n), &mut buf, body[8 + n..].try_into().unwrap()).ok()?;
            self.rseq += 1;
            Some(buf)
        }
        fn hello_flight(&mut self, ch: &[u8]) {
            let c = self.cfg.clone();
            // The ClientHello offers 1.3 and 1.2, both suites, EMS and secure renegotiation. It was the only record.
            assert_eq!(self.pos, self.inbuf.len(), "nothing may follow the ClientHello (no early CCS)");
            let has = |needle: &[u8]| ch.windows(needle.len()).any(|w| w == needle);
            assert!(has(&[0, 43, 0, 5, 4, 3, 4, 3, 3]) && has(&[0, 6, 0x13, 1, 0xC0, 0x2B, 0xC0, 0x2F]) && has(&[0, 23, 0, 0, 0xff, 1, 0, 1, 0]));
            self.client_hello = ch.to_vec();
            self.t.update(ch);
            let mut random = SERVER_RANDOM;
            if let Some(t) = c.tail {
                random[24..].copy_from_slice(&t);
            }
            let mut b = c.version.to_be_bytes().to_vec();
            b.extend_from_slice(&random);
            let sid = if c.echo_sid { client_random()[64..].to_vec() } else { vec![0x77; 32] };
            b.push(32);
            b.extend_from_slice(&sid);
            b.extend_from_slice(&c.suite.to_be_bytes());
            b.push(c.compression);
            let mut exts = extension(0xff01, &[0]);
            exts.extend(extension(11, &[1, 0]));
            if c.ems {
                exts.extend(extension(23, &[]));
            }
            if let Some(a) = c.alpn {
                let mut l = vec![a.len() as u8];
                l.extend_from_slice(a);
                let mut d = (l.len() as u16).to_be_bytes().to_vec();
                d.extend(l);
                exts.extend(extension(16, &d));
            }
            exts.extend_from_slice(&c.extra_ext);
            b.extend_from_slice(&(exts.len() as u16).to_be_bytes());
            b.extend(exts);
            let sh = m(hello::SERVER_HELLO, &b);
            let cert = {
                let mut l = (LEAF.len() as u32).to_be_bytes()[1..].to_vec();
                l.extend_from_slice(LEAF);
                let mut b = (l.len() as u32).to_be_bytes()[1..].to_vec();
                b.extend(l);
                m(hello::CERTIFICATE, &b)
            };
            let (mut sig, point) = match c.curve {
                0x0017 => (SKE_SIG_P256.to_vec(), ecc::p256_public(&SERVER_SCALAR).unwrap().to_vec()),
                0x001d => (SKE_SIG.to_vec(), x25519::public_key(&SERVER_SCALAR).to_vec()),
                _ => (SKE_SIG.to_vec(), vec![4; 97]),
            };
            if c.flip_sig {
                let last = sig.len() - 1;
                sig[last] ^= 1;
            }
            let mut params = vec![3];
            params.extend_from_slice(&c.curve.to_be_bytes());
            params.push(point.len() as u8);
            params.extend_from_slice(&point);
            let ske = m(hello::SERVER_KEY_EXCHANGE, &[&params[..], &[4, 3], &(sig.len() as u16).to_be_bytes(), &sig].concat());
            let shd = m(hello::SERVER_HELLO_DONE, &[]);
            for x in [&sh, &cert, &ske, &shd] {
                self.t.update(x);
                self.plain(22, x);
            }
        }
        fn step(&mut self) {
            while let Some(len) = self.inbuf[self.pos..].get(3..5).map(|l| u16::from_be_bytes([l[0], l[1]]) as usize) {
                if self.inbuf.len() - self.pos < 5 + len {
                    return;
                }
                let ty = self.inbuf[self.pos];
                let body = self.inbuf[self.pos + 5..self.pos + 5 + len].to_vec();
                self.pos += 5 + len;
                match self.stage {
                    Stage::Hello => {
                        assert_eq!(ty, 22);
                        self.hello_flight(&body);
                        self.stage = Stage::Kx;
                    }
                    Stage::Kx => {
                        let point_len = if self.cfg.curve == 0x0017 { 65 } else { 32 };
                        assert_eq!((ty, body[0], body[4] as usize, body.len()), (22, hello::CLIENT_KEY_EXCHANGE, point_len, 5 + point_len), "ClientKeyExchange");
                        self.t.update(&body);
                        let shared = if point_len == 65 {
                            ecc::p256_shared(&SERVER_SCALAR, &body[5..]).unwrap()
                        } else {
                            x25519::shared_secret(&SERVER_SCALAR, body[5..37].try_into().unwrap()).unwrap()
                        };
                        let cr = &client_random()[32..64];
                        self.ms = if self.cfg.ems && !self.cfg.legacy_ms {
                            ref_prf(&shared, b"extended master secret", &self.t.hash(), 48)
                        } else {
                            ref_prf(&shared, b"master secret", &[cr, &SERVER_RANDOM[..]].concat(), 48)
                        };
                        self.keys = ref_prf(&self.ms, b"key expansion", &[&SERVER_RANDOM[..], cr].concat(), 40);
                        self.stage = Stage::Ccs;
                    }
                    Stage::Ccs => {
                        assert_eq!((ty, &body[..]), (20, &[1u8][..]));
                        self.stage = Stage::Fin;
                    }
                    Stage::Fin => {
                        assert_eq!(ty, 22);
                        // A client with another master secret fails here. The server stops, like a real one would.
                        let Some(fin) = self.decrypt(ty, &body) else { return };
                        let want = [&[hello::FINISHED, 0, 0, 12][..], &ref_prf(&self.ms, b"client finished", &self.t.hash(), 12)].concat();
                        assert_eq!(fin, want, "client Finished");
                        self.finished_ok = true;
                        self.t.update(&fin);
                        if !self.cfg.skip_ccs {
                            self.plain(20, &[1]);
                        }
                        let mut vd = ref_prf(&self.ms, b"server finished", &self.t.hash(), 12);
                        if self.cfg.bad_finished {
                            vd[0] ^= 1;
                        }
                        self.send_enc(22, &[&[hello::FINISHED, 0, 0, 12][..], &vd].concat());
                        self.stage = Stage::App;
                    }
                    Stage::App => {
                        let plain = self.decrypt(ty, &body).expect("client record");
                        match ty {
                            23 => {
                                self.app.extend_from_slice(&plain);
                                self.send_enc(23, &plain.to_ascii_uppercase());
                            }
                            21 => self.alerts.push((plain[0], plain[1])),
                            t => panic!("record type {t}"),
                        }
                    }
                }
            }
        }
    }

    struct Loop(Rc<RefCell<Server>>);
    impl Read for Loop {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let mut s = self.0.borrow_mut();
            if s.out.is_empty() {
                s.step();
            }
            let n = buf.len().min(s.out.len());
            buf[..n].copy_from_slice(&s.out[..n]);
            s.out.drain(..n);
            Ok(n)
        }
    }
    impl Write for Loop {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            self.0.borrow_mut().inbuf.extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

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

    fn run(cfg: Cfg) -> (Result<ClientStream<Loop>, Error>, Rc<RefCell<Server>>) {
        let srv = Server::new(cfg);
        let r = super::super::connect_inner(Loop(srv.clone()), HOST, &Pin(HOST), NOW, &client_random());
        (r, srv)
    }
    fn fails(cfg: Cfg) -> Error {
        match run(cfg).0 {
            Err(e) => e,
            Ok(_) => panic!("handshake should have failed"),
        }
    }

    #[test]
    fn loopback_handshake_and_data() {
        for (ems, curve) in [(true, 0x001d), (false, 0x001d), (true, 0x0017), (false, 0x0017)] {
            let (c, srv) = run(Cfg { ems, curve, ..good() });
            let mut c = c.unwrap_or_else(|e| panic!("{e}"));
            assert!(srv.borrow().finished_ok && c.alpn_http11() && !c.alpn_h2());
            c.write_all(b"ping").unwrap();
            let mut got = [0u8; 4];
            c.read_exact(&mut got).unwrap();
            assert_eq!(&got, b"PING");
            // A write over one record and a read of a full-size record.
            let big: Vec<u8> = (0..40_000u32).map(|i| b'a' + (i % 26) as u8).collect();
            c.write_all(&big).unwrap();
            let mut back = vec![0u8; big.len()];
            c.read_exact(&mut back).unwrap();
            assert_eq!(back, big.to_ascii_uppercase());
            assert_eq!(&srv.borrow().app[4..], &big[..]);
            // close_notify is a protected alert.
            c.shutdown().unwrap();
            srv.borrow_mut().step();
            assert_eq!(srv.borrow().alerts, vec![(1, 0)]);
        }
    }

    #[test]
    fn loopback_without_alpn_or_with_h2_unoffered() {
        let (c, _) = run(Cfg { alpn: None, ..good() });
        let c = c.unwrap();
        assert!(!c.alpn_http11() && !c.alpn_h2());
        assert_eq!(fails(Cfg { alpn: Some(b"h2"), ..good() }), Error::Alpn);
    }

    #[test]
    fn loopback_rejects_what_it_must() {
        use HelloError::*;
        let bad = |c: Cfg| fails(c);
        // A suite we did not offer: AES-256 and the TLS 1.3 suite.
        assert_eq!(bad(Cfg { suite: 0xC030, ..good() }), Error::Hello(UnsupportedCipher));
        assert_eq!(bad(Cfg { suite: 0x1301, ..good() }), Error::Hello(UnsupportedCipher));
        // A wrong signature.
        assert_eq!(bad(Cfg { flip_sig: true, ..good() }), Error::BadSignature);
        // The RSA suite with an EC leaf.
        assert!(matches!(bad(Cfg { suite: 0xC02F, ..good() }), Error::Illegal(_)));
        // Downgrade marker, resumption, version, compression, unsolicited and other groups.
        assert_eq!(bad(Cfg { tail: Some(*b"DOWNGRD\x01"), ..good() }), Error::Hello(Downgrade));
        assert_eq!(bad(Cfg { echo_sid: true, ..good() }), Error::Hello(Resumption));
        assert_eq!(bad(Cfg { version: 0x0302, ..good() }), Error::Hello(UnsupportedVersion));
        assert_eq!(bad(Cfg { compression: 1, ..good() }), Error::Hello(BadCompression));
        assert_eq!(bad(Cfg { extra_ext: extension(35, &[]), ..good() }), Error::Hello(UnsolicitedExtension(35)));
        assert!(matches!(bad(Cfg { curve: 0x0018, ..good() }), Error::Unsupported(_)));
        // The 1.1 marker is not the 1.3 downgrade marker: only "DOWNGRD\x01" aborts. (The signature covers
        // the random, so this server gets as far as the signature check.)
        assert_eq!(bad(Cfg { tail: Some(*b"DOWNGRD\x00"), ..good() }), Error::BadSignature);
        // A wrong server Finished, a missing change_cipher_spec.
        assert_eq!(bad(Cfg { bad_finished: true, ..good() }), Error::BadFinished);
        assert_eq!(bad(Cfg { skip_ccs: true, ..good() }), Error::Record(record::Error::UnexpectedRecord(22)));
        // A server that sends the EMS extension but derives the plain master secret: the client Finished does not
        // decrypt, the server hangs up, the client sees the end of the stream.
        assert_eq!(bad(Cfg { legacy_ms: true, ..good() }), Error::Record(record::Error::UnexpectedEof));
    }

    #[test]
    fn loopback_wrong_host_is_rejected_by_the_verifier() {
        let srv = Server::new(good());
        let r = super::super::connect_inner(Loop(srv), "other.invalid", &Pin(HOST), NOW, &client_random());
        assert!(matches!(r, Err(Error::Certificate(_))));
    }

    #[test]
    fn renegotiation_request_is_refused() {
        let (c, srv) = run(good());
        let mut c = c.unwrap();
        srv.borrow_mut().send_enc(22, &[0, 0, 0, 0]); // HelloRequest
        let e = c.read(&mut [0u8; 8]).unwrap_err();
        assert!(e.to_string().contains("renegotiation"), "{e}");
    }

    #[test]
    #[ignore] // prints the ServerKeyExchange input that SKE_SIG covers
    fn print_ske_input() {
        let mut signed = client_random()[32..64].to_vec();
        signed.extend_from_slice(&SERVER_RANDOM);
        signed.extend_from_slice(&[3, 0, 0x1d, 32]);
        signed.extend_from_slice(&x25519::public_key(&SERVER_SCALAR));
        let hex: String = signed.iter().map(|b| format!("{b:02x}")).collect();
        println!("SKEINPUT {hex}");
        let mut signed = client_random()[32..64].to_vec();
        signed.extend_from_slice(&SERVER_RANDOM);
        signed.extend_from_slice(&[3, 0, 0x17, 65]);
        signed.extend_from_slice(&ecc::p256_public(&SERVER_SCALAR).unwrap());
        let hex: String = signed.iter().map(|b| format!("{b:02x}")).collect();
        println!("SKEINPUTP256 {hex}");
    }
}
