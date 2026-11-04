//! Scripted TLS 1.3 server and public fixtures for `client.rs` tests and `client_load.rs`.
//!
//! Included with `#[path]` by both, so it uses `crate::` modules of whoever includes it.
//! Everything here is public test data: the "random" values are fixed byte patterns, the
//! certificate is a synthetic self-signed P-256 leaf (`tools/fixtures/client/leaf.der`,
//! SAN `client-test.bare.invalid`) and its private key was deleted after signing.
//!
//! The server side reuses `RecordLayer` and `key_schedule`: it derives the same keys from the
//! client's deterministic ClientHello. The CertificateVerify signatures (`cv_*.der`) are
//! over the exact transcripts of the two EncryptedExtensions variants. They are valid only
//! while the ClientHello bytes stay the same. To regenerate after a ClientHello change:
//! run the `print_cv_input` test, sign each printed hex input with a throw-away key
//! (`openssl dgst -sha256 -sign`), make a new self-signed leaf, replace the three files.

use crate::hello::{self, ClientHelloParams};
use crate::key_schedule::{Client, Handshake, HandshakeSecret, MasterSecret, Server, TrafficSecret, Transcript};
use crate::record::{Record, RecordLayer};
use crate::x25519;
use std::io::{self, Read, Write};

pub const HOST: &str = "client-test.bare.invalid";
/// 2026-11-04T15:40:00Z, inside the leaf's validity.
pub const NOW: i64 = 1_793_806_800;
pub const LEAF: &[u8] = include_bytes!("../fixtures/client/leaf.der");
/// CertificateVerify signature when EncryptedExtensions holds ALPN `http/1.1`.
pub const CV_ALPN: &[u8] = include_bytes!("../fixtures/client/cv_alpn.der");
/// CertificateVerify signature when EncryptedExtensions is empty.
pub const CV_PLAIN: &[u8] = include_bytes!("../fixtures/client/cv_plain.der");

/// What `connect_inner` reads: scalar, client random, session ID. A fixed pattern.
pub fn client_random() -> [u8; 96] {
    let mut r = [0u8; 96];
    for (i, b) in r.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(7).wrapping_add(3);
    }
    r
}

const SERVER_SCALAR: [u8; 32] = [0x5a; 32];

/// A transport that reads from a script (at most `chunk` bytes per call) and collects writes.
pub struct Script {
    pub input: Vec<u8>,
    pub pos: usize,
    pub chunk: usize,
    pub output: Vec<u8>,
    /// Reads and writes done (for syscall counts).
    pub reads: usize,
    pub writes: usize,
}

impl Script {
    pub fn new(input: Vec<u8>, chunk: usize) -> Self {
        Script { input, pos: 0, chunk, output: Vec::new(), reads: 0, writes: 0 }
    }
}

impl Read for Script {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.reads += 1;
        let n = buf.len().min(self.chunk).min(self.input.len() - self.pos);
        buf[..n].copy_from_slice(&self.input[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

impl Write for Script {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.writes += 1;
        self.output.extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn msg(ty: u8, body: &[u8]) -> Vec<u8> {
    let mut m = hello::write_header(ty, body.len()).unwrap().to_vec();
    m.extend_from_slice(body);
    m
}

/// The ClientHello the client sends for `HOST` with `client_random()`.
pub fn client_hello() -> Vec<u8> {
    let r = client_random();
    let public = x25519::public_key(r[..32].try_into().unwrap());
    let mut ch = [0u8; hello::MAX_CLIENT_HELLO_LEN];
    let n = hello::encode_client_hello(
        &ClientHelloParams { random: r[32..64].try_into().unwrap(), session_id: &r[64..], x25519_public: &public, server_name: HOST.as_bytes() },
        &mut ch,
    )
    .unwrap();
    ch[..n].to_vec()
}

pub fn server_hello(group_public: &[u8; 32]) -> Vec<u8> {
    let r = client_random();
    let mut b = vec![3, 3];
    b.extend_from_slice(&[0x11; 32]); // server random
    b.push(32);
    b.extend_from_slice(&r[64..]); // echoed session ID
    b.extend_from_slice(&[0x13, 0x01, 0]);
    let exts = [&[0u8, 43, 0, 2, 3, 4][..], &[0, 51, 0, 36, 0, 29, 0, 32][..], &group_public[..]].concat();
    b.extend_from_slice(&(exts.len() as u16).to_be_bytes());
    b.extend_from_slice(&exts);
    msg(hello::SERVER_HELLO, &b)
}

/// EncryptedExtensions that the fixture signature `CV_ALPN` covers.
pub fn ee_alpn() -> Vec<u8> {
    msg(hello::ENCRYPTED_EXTENSIONS, &[0, 15, 0, 16, 0, 11, 0, 9, 8, b'h', b't', b't', b'p', b'/', b'1', b'.', b'1'])
}
/// Empty EncryptedExtensions, covered by `CV_PLAIN`.
pub fn ee_plain() -> Vec<u8> {
    msg(hello::ENCRYPTED_EXTENSIONS, &[0, 0])
}
pub fn extension(ty: u16, data: &[u8]) -> Vec<u8> {
    let mut e = ty.to_be_bytes().to_vec();
    e.extend_from_slice(&(data.len() as u16).to_be_bytes());
    e.extend_from_slice(data);
    e
}
pub fn ee_with(exts: &[u8]) -> Vec<u8> {
    let mut b = (exts.len() as u16).to_be_bytes().to_vec();
    b.extend_from_slice(exts);
    msg(hello::ENCRYPTED_EXTENSIONS, &b)
}

/// A Certificate message. Each entry is `(der, extensions)`.
pub fn certificate(entries: &[(&[u8], &[u8])]) -> Vec<u8> {
    let mut list = Vec::new();
    for (der, ext) in entries {
        list.extend_from_slice(&(der.len() as u32).to_be_bytes()[1..]);
        list.extend_from_slice(der);
        list.extend_from_slice(&(ext.len() as u16).to_be_bytes());
        list.extend_from_slice(ext);
    }
    let mut b = vec![0];
    b.extend_from_slice(&(list.len() as u32).to_be_bytes()[1..]);
    b.extend_from_slice(&list);
    msg(hello::CERTIFICATE, &b)
}

pub fn certificate_verify(scheme: u16, sig: &[u8]) -> Vec<u8> {
    let mut b = scheme.to_be_bytes().to_vec();
    b.extend_from_slice(&(sig.len() as u16).to_be_bytes());
    b.extend_from_slice(sig);
    msg(hello::CERTIFICATE_VERIFY, &b)
}

pub fn new_session_ticket(ticket: &[u8], exts: &[u8]) -> Vec<u8> {
    let mut b = vec![0, 0, 0x1c, 0x20, 1, 2, 3, 4, 2, 9, 9];
    b.extend_from_slice(&(ticket.len() as u16).to_be_bytes());
    b.extend_from_slice(ticket);
    b.extend_from_slice(&(exts.len() as u16).to_be_bytes());
    b.extend_from_slice(exts);
    msg(4, &b)
}

/// The server side of one connection, writing into a byte buffer.
pub struct Srv {
    rl: RecordLayer<Script>,
    pub t: Transcript,
    s_hs: TrafficSecret<Server, Handshake>,
    c_hs: TrafficSecret<Client, Handshake>,
    master: MasterSecret,
}

impl Srv {
    /// Starts from the fixed ClientHello: writes the compat CCS and the plaintext ServerHello.
    pub fn new() -> Srv {
        Srv::with_sh_tail(&[])
    }

    /// Same, but `tail` follows the ServerHello in its record (not in the transcript).
    pub fn with_sh_tail(tail: &[u8]) -> Srv {
        let r = client_random();
        let client_pub = x25519::public_key(r[..32].try_into().unwrap());
        let shared = x25519::shared_secret(&SERVER_SCALAR, &client_pub).unwrap();
        let mut t = Transcript::new();
        t.update(&client_hello());
        let sh = server_hello(&x25519::public_key(&SERVER_SCALAR));
        t.update(&sh);
        let hs = HandshakeSecret::from_ecdhe(&shared);
        let h = t.hash();
        let (s_hs, c_hs) = (hs.server_traffic(&h), hs.client_traffic(&h));
        let mut rl = RecordLayer::new(Script::new(Vec::new(), usize::MAX));
        rl.write_handshake(&[&sh[..], tail].concat()).unwrap();
        rl.get_mut().output.extend_from_slice(&[20, 3, 3, 0, 1, 1]); // compat CCS
        let k = s_hs.traffic_keys();
        rl.install_write_key(&k.key, &k.iv).unwrap();
        Srv { rl, t, s_hs, c_hs, master: hs.into_master() }
    }

    /// Send a handshake message under the handshake key and add it to the transcript.
    pub fn push(&mut self, m: &[u8]) {
        self.t.update(m);
        self.rl.write_handshake(m).unwrap();
    }
    /// Several messages in one record.
    pub fn push_many(&mut self, ms: &[&[u8]]) {
        for m in ms {
            self.t.update(m);
        }
        self.rl.write_handshake(&ms.concat()).unwrap();
    }
    /// One message over two records, cut at `at`.
    pub fn push_split(&mut self, m: &[u8], at: usize) {
        self.t.update(m);
        self.rl.write_handshake(&m[..at]).unwrap();
        self.rl.write_handshake(&m[at..]).unwrap();
    }
    /// Send bytes under the handshake key and leave the transcript alone.
    pub fn push_raw(&mut self, m: &[u8]) {
        self.rl.write_handshake(m).unwrap();
    }
    pub fn finished(&self) -> Vec<u8> {
        msg(hello::FINISHED, &self.s_hs.finished_verify_data(&self.t.hash()))
    }
    /// Send server Finished, then switch to the server application key.
    pub fn push_finished(&mut self) {
        let f = self.finished();
        self.push(&f);
        self.app_keys();
    }
    pub fn app_keys(&mut self) {
        let h = self.t.hash();
        let k = self.master.server_traffic(&h).traffic_keys();
        self.rl.install_write_key(&k.key, &k.iv).unwrap();
    }
    /// The whole happy flight. `cv` must match `ee`.
    pub fn flight(&mut self, ee: &[u8], cv: &[u8]) {
        self.push(ee);
        self.push(&certificate(&[(LEAF, &[])]));
        self.push(&certificate_verify(0x0403, cv));
        self.push_finished();
    }
    pub fn app(&mut self, data: &[u8]) {
        self.rl.write_application_data(data).unwrap();
    }
    pub fn post_handshake(&mut self, m: &[u8]) {
        self.rl.write_handshake(m).unwrap();
    }
    pub fn close_notify(&mut self) {
        self.rl.send_close_notify().unwrap();
    }
    /// Bytes written so far, taken out.
    pub fn take(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.rl.get_mut().output)
    }
    /// A compatibility CCS record on the wire (plaintext, never protected).
    pub fn rl_output_ccs(&mut self) {
        self.rl.get_mut().output.extend_from_slice(&[20, 3, 3, 0, 1, 1]);
    }
    pub fn transcript_hash(&self) -> [u8; 32] {
        self.t.hash()
    }

    /// The alert the client sent right after its ClientHello and CCS, under the handshake key.
    pub fn client_alert(&self, out: &[u8]) -> Option<u8> {
        let mut rl = RecordLayer::new(Script::new(out.to_vec(), usize::MAX));
        rl.allow_compat_ccs(true);
        assert!(matches!(rl.read_record().unwrap(), Record::Handshake(_)));
        let k = self.c_hs.traffic_keys();
        rl.install_read_key(&k.key, &k.iv).unwrap();
        match rl.read_record() {
            Err(crate::record::Error::PeerAlert(d)) => Some(d),
            _ => None,
        }
    }

    /// Decode what the client wrote: `(ClientHello, client Finished, application bytes, alert after them)`.
    /// Checks the CCS and the keys on the way.
    pub fn read_client(&self, out: &[u8]) -> (Vec<u8>, Vec<u8>, Vec<u8>, Option<u8>) {
        let mut rl = RecordLayer::new(Script::new(out.to_vec(), usize::MAX));
        rl.allow_compat_ccs(true);
        let ch = match rl.read_record().unwrap() {
            Record::Handshake(d) => d.to_vec(),
            r => panic!("expected ClientHello, got {r:?}"),
        };
        let k = self.c_hs.traffic_keys();
        rl.install_read_key(&k.key, &k.iv).unwrap();
        let fin = match rl.read_record().unwrap() {
            Record::Handshake(d) => d.to_vec(),
            r => panic!("expected Finished, got {r:?}"),
        };
        let h = self.t.hash();
        assert_eq!(fin, msg(hello::FINISHED, &self.c_hs.finished_verify_data(&h)), "client Finished");
        let k = self.master.client_traffic(&h).traffic_keys();
        rl.install_read_key(&k.key, &k.iv).unwrap();
        let mut app = Vec::new();
        let mut alert = None;
        loop {
            match rl.read_record() {
                Ok(Record::ApplicationData(d)) => app.extend_from_slice(d),
                Ok(Record::CloseNotify) => break,
                Ok(r) => panic!("unexpected {r:?}"),
                Err(crate::record::Error::UnexpectedEof) => break,
                Err(crate::record::Error::PeerAlert(d)) => {
                    alert = Some(d);
                    break;
                }
                Err(e) => panic!("{e}"),
            }
        }
        (ch, fin, app, alert)
    }
}
