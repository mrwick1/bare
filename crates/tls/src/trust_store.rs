//! Trust anchors from the local machine.
//!
//! Roots come ONLY from an explicit local file or from a fixed list of OS bundle paths.
//! Nothing comes from the peer's chain or from the network. There is no fallback when a
//! file exists but is bad, and no empty store.
//!
//! PEM decoding here is NOT certificate validation. It only checks the PEM text, the base64
//! and that the bytes start like one DER SEQUENCE whose length covers all the bytes. The
//! later X.509 parser must parse each root, and the caller decides what to do with a root
//! it rejects.
//!
//! Accepted text (RFC 7468 `CERTIFICATE` blocks, as in `ca-certificates` bundles):
//! - LF or CRLF line ends. A last line may have no line end. A lone CR is an error.
//! - Outside blocks: only blank lines and `#` comment lines. Any other text is an error,
//!   so a damaged BEGIN line cannot hide a certificate. Other labels (`TRUSTED CERTIFICATE`)
//!   are errors, not skipped.
//! - Inside blocks: non-empty base64 lines (any length, trailing spaces and tabs allowed),
//!   canonical padding at the end only, no blank lines.
//! - A block that ends the file without an END line is an error (truncation).

use std::fmt;
use std::io::Read;
use std::path::Path;

pub const MAX_FILE_BYTES: usize = 4 << 20;
pub const MAX_CERTS: usize = 1024;
pub const MAX_CERT_DER_BYTES: usize = 16 << 10;

/// Checked in this order. A missing path is skipped. The first existing path is the only
/// one used. (Only `/etc/ssl/certs/ca-certificates.crt` and `/etc/ssl/cert.pem` were seen
/// on the Arch machine of 2026-11-04. The Red Hat path is the documented location. openSUSE has only `/etc/ssl/ca-bundle.pem`.)
pub const SYSTEM_PATHS: [&str; 4] =
    ["/etc/ssl/certs/ca-certificates.crt", "/etc/pki/tls/certs/ca-bundle.crt", "/etc/ssl/cert.pem", "/etc/ssl/ca-bundle.pem"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Io(std::io::ErrorKind),
    NotAFile,
    FileTooLarge,
    TooManyCerts,
    CertTooLarge { line: usize },
    /// Bad PEM text. `line` is 1-based.
    Format { line: usize, why: &'static str },
    /// The input holds no certificate.
    Empty,
    /// No path in `SYSTEM_PATHS` exists.
    NoSystemStore,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(k) => write!(f, "cannot read trust store: {k}"),
            Error::NotAFile => f.write_str("trust store is not a regular file"),
            Error::FileTooLarge => write!(f, "trust store is over {MAX_FILE_BYTES} bytes"),
            Error::TooManyCerts => write!(f, "trust store has over {MAX_CERTS} certificates"),
            Error::CertTooLarge { line } => write!(f, "line {line}: certificate is over {MAX_CERT_DER_BYTES} bytes"),
            Error::Format { line, why } => write!(f, "trust store line {line}: {why}"),
            Error::Empty => f.write_str("trust store has no certificate"),
            Error::NoSystemStore => f.write_str("no system trust store found"),
        }
    }
}
impl std::error::Error for Error {}

/// DER certificates in one buffer. Certificate `i` is `der[ends[i-1]..ends[i]]`.
#[derive(Debug)]
pub struct TrustStore {
    der: Vec<u8>,
    ends: Vec<u32>,
}

impl TrustStore {
    pub fn len(&self) -> usize {
        self.ends.len()
    }
    pub fn is_empty(&self) -> bool {
        self.ends.is_empty()
    }
    pub fn get(&self, i: usize) -> Option<&[u8]> {
        let end = *self.ends.get(i)? as usize;
        let start = if i == 0 { 0 } else { self.ends[i - 1] as usize };
        Some(&self.der[start..end])
    }
    pub fn iter(&self) -> impl Iterator<Item = &[u8]> {
        (0..self.len()).map(|i| self.get(i).unwrap())
    }
}

const BEGIN: &[u8] = b"-----BEGIN CERTIFICATE-----";
const END: &[u8] = b"-----END CERTIFICATE-----";

const fn table() -> [u8; 256] {
    let mut t = [255u8; 256];
    let a = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut i = 0;
    while i < 64 {
        t[a[i] as usize] = i as u8;
        i += 1;
    }
    t
}
static B64: [u8; 256] = table();

/// Base64 state across lines: `n` characters of the current 4-character group.
#[derive(Default)]
struct B64State {
    acc: u32,
    n: u8,
    pad: u8,
}

impl B64State {
    fn push(&mut self, c: u8, out: &mut Vec<u8>) -> Result<(), &'static str> {
        if c == b'=' {
            if self.n < 2 {
                return Err("misplaced base64 padding");
            }
            self.pad += 1;
            self.acc <<= 6;
        } else {
            let v = B64[c as usize];
            if self.pad > 0 {
                return Err("base64 data after padding");
            }
            if v == 255 {
                return Err("bad base64 character");
            }
            self.acc = self.acc << 6 | v as u32;
        }
        self.n += 1;
        if self.n == 4 {
            // The padded bits must be zero, or two texts would decode to the same bytes.
            let unused = [0, 0xFF, 0xFFFF][self.pad as usize];
            if self.acc & unused != 0 {
                return Err("non-canonical base64 padding bits");
            }
            let b = self.acc.to_be_bytes();
            out.extend_from_slice(&b[1..4 - self.pad as usize]);
            self.acc = 0;
            self.n = 0;
        }
        Ok(())
    }
}

/// One DER SEQUENCE header whose length covers exactly all of `d` (minimal length bytes).
fn der_frame_ok(d: &[u8]) -> bool {
    let (hdr, len) = match d {
        [0x30, l, ..] if *l < 0x80 => (2, *l as usize),
        [0x30, 0x81, l, ..] if *l >= 0x80 => (3, *l as usize),
        [0x30, 0x82, h, l, ..] if *h != 0 => (4, (*h as usize) << 8 | *l as usize),
        _ => return false,
    };
    hdr + len == d.len()
}

/// Decodes every CERTIFICATE block. See the module text for the accepted format.
pub fn decode_pem(pem: &[u8]) -> Result<TrustStore, Error> {
    if pem.len() > MAX_FILE_BYTES {
        return Err(Error::FileTooLarge);
    }
    let mut der = Vec::with_capacity(pem.len() / 4 * 3);
    let mut ends: Vec<u32> = Vec::new();
    // Some(start of the current certificate in `der`, base64 state) while inside a block.
    let mut open: Option<(usize, B64State)> = None;
    let mut line_no = 0;
    for raw in pem.strip_suffix(b"\n").unwrap_or(pem).split(|&b| b == b'\n') {
        line_no += 1;
        let fmt_err = |why| Error::Format { line: line_no, why };
        let mut l = raw.strip_suffix(b"\r").unwrap_or(raw);
        while let [rest @ .., b' ' | b'\t'] = l {
            l = rest;
        }
        match &mut open {
            None => {
                if l == BEGIN {
                    open = Some((der.len(), B64State::default()));
                } else if l == END {
                    return Err(fmt_err("END without BEGIN"));
                } else if !(l.is_empty() || l[0] == b'#') {
                    return Err(fmt_err("text outside a CERTIFICATE block"));
                } else if l.contains(&b'\r') {
                    return Err(fmt_err("lone CR"));
                }
            }
            Some((start, st)) => {
                if l == END {
                    if st.n != 0 {
                        return Err(fmt_err("base64 ends inside a group"));
                    }
                    if !der_frame_ok(&der[*start..]) {
                        return Err(fmt_err("not one DER SEQUENCE of the right length"));
                    }
                    if ends.len() == MAX_CERTS {
                        return Err(Error::TooManyCerts);
                    }
                    ends.push(der.len() as u32);
                    open = None;
                } else if l == BEGIN {
                    return Err(fmt_err("BEGIN inside a block"));
                } else if l.is_empty() {
                    return Err(fmt_err("blank line inside a block"));
                } else {
                    let mut l = l;
                    // Fast path: whole groups of 4 with no padding, when a group starts here.
                    if st.n == 0 && st.pad == 0 {
                        while let [a, b, c, d, rest @ ..] = l {
                            let v = [B64[*a as usize], B64[*b as usize], B64[*c as usize], B64[*d as usize]];
                            if v.contains(&255) {
                                break;
                            }
                            let x = (v[0] as u32) << 18 | (v[1] as u32) << 12 | (v[2] as u32) << 6 | v[3] as u32;
                            der.extend_from_slice(&x.to_be_bytes()[1..]);
                            l = rest;
                        }
                    }
                    for &c in l {
                        st.push(c, &mut der).map_err(fmt_err)?;
                    }
                    if der.len() - *start > MAX_CERT_DER_BYTES {
                        return Err(Error::CertTooLarge { line: line_no });
                    }
                }
            }
        }
    }
    if open.is_some() {
        return Err(Error::Format { line: line_no, why: "file ends inside a block" });
    }
    if ends.is_empty() {
        return Err(Error::Empty);
    }
    Ok(TrustStore { der, ends })
}

/// Loads roots from one explicit file.
pub fn load_file(path: &Path) -> Result<TrustStore, Error> {
    let io = |e: std::io::Error| Error::Io(e.kind());
    // Check before open: opening a FIFO would block.
    if !std::fs::metadata(path).map_err(io)?.is_file() {
        return Err(Error::NotAFile);
    }
    let mut buf = Vec::new();
    std::fs::File::open(path).map_err(io)?.take(MAX_FILE_BYTES as u64 + 1).read_to_end(&mut buf).map_err(io)?;
    decode_pem(&buf)
}

/// Loads the first existing file of `paths`. A file that exists and fails is an error.
pub fn load_first(paths: &[&'static str]) -> Result<(TrustStore, &'static str), Error> {
    for &p in paths {
        match load_file(Path::new(p)) {
            Err(Error::Io(std::io::ErrorKind::NotFound)) => continue,
            r => return r.map(|s| (s, p)),
        }
    }
    Err(Error::NoSystemStore)
}

/// Loads the OS bundle from `SYSTEM_PATHS`. Returns the path used.
pub fn load_system() -> Result<(TrustStore, &'static str), Error> {
    load_first(&SYSTEM_PATHS)
}

#[cfg(test)]
mod tests {
    use super::*;

    // A tiny valid DER SEQUENCE: 30 03 02 01 05 (SEQUENCE { INTEGER 5 }). Not a certificate.
    const D: [u8; 5] = [0x30, 3, 2, 1, 5];
    const P: &str = "-----BEGIN CERTIFICATE-----\nMAMCAQU=\n-----END CERTIFICATE-----\n";

    fn pem(body: &str) -> String {
        format!("-----BEGIN CERTIFICATE-----\n{body}\n-----END CERTIFICATE-----\n")
    }
    fn fmt(r: Result<TrustStore, Error>) -> &'static str {
        match r {
            Err(Error::Format { why, .. }) => why,
            Err(e) => panic!("other error {e:?}"),
            Ok(_) => panic!("accepted"),
        }
    }

    #[test]
    fn decodes_one_and_many() {
        let s = decode_pem(P.as_bytes()).unwrap();
        assert_eq!((s.len(), s.get(0).unwrap()), (1, &D[..]));
        let two = format!("# one\n{P}\n# two\n\n{P}");
        let s = decode_pem(two.as_bytes()).unwrap();
        assert_eq!(s.iter().collect::<Vec<_>>(), [&D[..], &D[..]]);
        assert!(s.get(2).is_none());
    }

    #[test]
    fn crlf_missing_last_newline_and_trailing_blanks() {
        let crlf = P.replace('\n', "\r\n");
        assert_eq!(decode_pem(crlf.as_bytes()).unwrap().len(), 1);
        assert_eq!(decode_pem(P.trim_end().as_bytes()).unwrap().len(), 1);
        assert_eq!(decode_pem(P.replace('\n', " \t\n").as_bytes()).unwrap().len(), 1);
        assert!(decode_pem(P.replace('\n', "\r\r\n").as_bytes()).is_err());
        assert_eq!(fmt(decode_pem(b"# a\rb\n")), "lone CR");
        assert_eq!(fmt(decode_pem(pem("MAMC\rAQU=").as_bytes())), "bad base64 character");
    }

    #[test]
    fn base64_padding_rules() {
        // 30 03 02 01 05 + 1 extra byte makes 6 bytes: no padding; test padding with other lengths via line splits.
        // "MAMCAQU=" has one pad. Two-pad and no-pad forms need other DER lengths.
        let two_pad = pem("MAA="); // 30 00 : 2 bytes -> "MAA=" one pad
        assert_eq!(decode_pem(two_pad.as_bytes()).unwrap().get(0).unwrap(), &[0x30, 0]);
        let one = pem("MAMCAQUAAA=="); // 30 03 02 01 05 00 00 : length byte 3 but 7 bytes
        assert_eq!(fmt(decode_pem(one.as_bytes())), "not one DER SEQUENCE of the right length");
        let np = pem("MAQCAQUA"); // 30 04 02 01 05 00 : 6 bytes, no pad
        assert_eq!(decode_pem(np.as_bytes()).unwrap().get(0).unwrap(), &[0x30, 4, 2, 1, 5, 0]);
        assert_eq!(fmt(decode_pem(pem("MAMCAQU").as_bytes())), "base64 ends inside a group");
        assert_eq!(fmt(decode_pem(pem("MAMCAQU==").as_bytes())), "misplaced base64 padding");
        assert_eq!(fmt(decode_pem(pem("MAMCAQV=").as_bytes())), "non-canonical base64 padding bits");
        assert_eq!(fmt(decode_pem(pem("MAA=MAA=").as_bytes())), "base64 data after padding");
        assert_eq!(fmt(decode_pem(pem("MA=A").as_bytes())), "base64 data after padding");
        assert_eq!(fmt(decode_pem(pem("M===").as_bytes())), "misplaced base64 padding");
        assert_eq!(fmt(decode_pem(pem("====").as_bytes())), "misplaced base64 padding");
        assert_eq!(fmt(decode_pem(pem("MAM CAQU=").as_bytes())), "bad base64 character");
        assert_eq!(fmt(decode_pem(pem("MAMC-AQU").as_bytes())), "bad base64 character");
        assert_eq!(fmt(decode_pem(pem("MAMCAQU\u{e9}").as_bytes())), "bad base64 character");
        assert_eq!(fmt(decode_pem(pem("MAMC\0AQU").as_bytes())), "bad base64 character");
    }

    #[test]
    fn groups_may_span_lines() {
        // Split inside a 4-character group and between groups.
        assert_eq!(decode_pem(pem("MAMC\nAQU=").as_bytes()).unwrap().len(), 1);
        assert_eq!(decode_pem(pem("MA\nMCAQU=").as_bytes()).unwrap().len(), 1);
    }

    #[test]
    fn boundaries_and_truncation() {
        assert_eq!(fmt(decode_pem(b"-----BEGIN CERTIFICATE-----\nMAMCAQU=\n")), "file ends inside a block");
        assert_eq!(fmt(decode_pem(b"-----BEGIN CERTIFICATE-----\n")), "file ends inside a block");
        assert_eq!(fmt(decode_pem(b"-----END CERTIFICATE-----\n")), "END without BEGIN");
        assert_eq!(fmt(decode_pem(format!("{P}-----END CERTIFICATE-----\n").as_bytes())), "END without BEGIN");
        let nested = "-----BEGIN CERTIFICATE-----\nMAMC\n-----BEGIN CERTIFICATE-----\nAQU=\n-----END CERTIFICATE-----\n";
        assert_eq!(fmt(decode_pem(nested.as_bytes())), "BEGIN inside a block");
        assert_eq!(fmt(decode_pem(b"-----BEGIN CERTIFICATE----\nMAMCAQU=\n-----END CERTIFICATE-----\n")), "text outside a CERTIFICATE block");
        assert_eq!(fmt(decode_pem(b"-----BEGIN TRUSTED CERTIFICATE-----\nMAMCAQU=\n-----END TRUSTED CERTIFICATE-----\n")), "text outside a CERTIFICATE block");
        assert_eq!(fmt(decode_pem(b"-----BEGIN OTHER THING-----\nMAMCAQU=\n-----END OTHER THING-----\n")), "text outside a CERTIFICATE block");
        assert_eq!(fmt(decode_pem(b"junk\n")), "text outside a CERTIFICATE block");
        assert_eq!(fmt(decode_pem(format!("{P}junk\n").as_bytes())), "text outside a CERTIFICATE block");
        assert_eq!(fmt(decode_pem(b"-----BEGIN CERTIFICATE-----\n\nMAMCAQU=\n-----END CERTIFICATE-----\n")), "blank line inside a block");
        // Truncated base64 in the middle of a bundle must not drop the rest silently.
        let cut = format!("{P}-----BEGIN CERTIFICATE-----\nMAMC\n{P}");
        assert_eq!(fmt(decode_pem(cut.as_bytes())), "BEGIN inside a block");
        // The error names the line.
        assert_eq!(
            decode_pem(format!("{P}x\n").as_bytes()).unwrap_err(),
            Error::Format { line: 4, why: "text outside a CERTIFICATE block" }
        );
    }

    #[test]
    fn der_frame_checks() {
        assert_eq!(fmt(decode_pem(pem("MAQCAQU=").as_bytes())), "not one DER SEQUENCE of the right length"); // length too big
        assert_eq!(fmt(decode_pem(pem("MAICAQU=").as_bytes())), "not one DER SEQUENCE of the right length"); // trailing byte
        assert_eq!(fmt(decode_pem(pem("MQMCAQU=").as_bytes())), "not one DER SEQUENCE of the right length"); // 31 = SET
        assert_eq!(fmt(decode_pem(pem("MIADAgEF").as_bytes())), "not one DER SEQUENCE of the right length"); // 30 80 indefinite
        assert_eq!(fmt(decode_pem(pem("MIEDAgEF").as_bytes())), "not one DER SEQUENCE of the right length"); // 30 81 03: non-minimal
        assert_eq!(fmt(decode_pem(pem("MA==").as_bytes())), "not one DER SEQUENCE of the right length");
        // Long forms that are minimal.
        let mut d = vec![0x30, 0x81, 200];
        d.extend(vec![0u8; 200]);
        assert!(der_frame_ok(&d));
        let mut d = vec![0x30, 0x82, 0x01, 0x00];
        d.extend(vec![0u8; 256]);
        assert!(der_frame_ok(&d));
        let mut d = vec![0x30, 0x82, 0x00, 0xFF];
        d.extend(vec![0u8; 255]);
        assert!(!der_frame_ok(&d));
    }

    #[test]
    fn empty_and_limits() {
        assert_eq!(decode_pem(b"").unwrap_err(), Error::Empty);
        assert_eq!(decode_pem(b"# only a comment\n\n").unwrap_err(), Error::Empty);
        assert_eq!(decode_pem(&vec![b'#'; MAX_FILE_BYTES + 1]).unwrap_err(), Error::FileTooLarge);
        let many = P.repeat(MAX_CERTS);
        assert_eq!(decode_pem(many.as_bytes()).unwrap().len(), MAX_CERTS);
        let over = P.repeat(MAX_CERTS + 1);
        assert_eq!(decode_pem(over.as_bytes()).unwrap_err(), Error::TooManyCerts);
        // One certificate of exactly the largest size passes; one byte more fails.
        let block = |n: usize| {
            let body = n - 4;
            let mut d = vec![0x30, 0x82, (body >> 8) as u8, body as u8];
            d.extend(vec![0u8; body]);
            let a = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
            let mut text = String::from("-----BEGIN CERTIFICATE-----\n");
            for (i, c) in d.chunks(3).enumerate() {
                let v = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
                for k in 0..4 {
                    text.push(if k <= c.len() { a[(v >> (18 - 6 * k) & 63) as usize] as char } else { '=' });
                }
                if i % 16 == 15 {
                    text.push('\n');
                }
            }
            text + "\n-----END CERTIFICATE-----\n"
        };
        assert_eq!(decode_pem(block(MAX_CERT_DER_BYTES).as_bytes()).unwrap().get(0).unwrap().len(), MAX_CERT_DER_BYTES);
        assert!(matches!(decode_pem(block(MAX_CERT_DER_BYTES + 3).as_bytes()), Err(Error::CertTooLarge { .. })));
    }

    // Test-owned temp files.
    struct Tmp(std::path::PathBuf);
    impl Tmp {
        fn new(tag: &str, data: &[u8]) -> Tmp {
            let p = std::env::temp_dir().join(format!("bare-trust-{}-{tag}", std::process::id()));
            std::fs::write(&p, data).unwrap();
            Tmp(p)
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn file_errors() {
        let ok = Tmp::new("ok", P.as_bytes());
        assert_eq!(load_file(&ok.0).unwrap().len(), 1);
        let bad = Tmp::new("bad", b"junk");
        assert!(matches!(load_file(&bad.0), Err(Error::Format { line: 1, .. })));
        let missing = std::env::temp_dir().join(format!("bare-trust-{}-missing", std::process::id()));
        assert_eq!(load_file(&missing).unwrap_err(), Error::Io(std::io::ErrorKind::NotFound));
        assert_eq!(load_file(&std::env::temp_dir()).unwrap_err(), Error::NotAFile);
        let huge = Tmp::new("huge", &vec![b'#'; MAX_FILE_BYTES + 1]);
        assert_eq!(load_file(&huge.0).unwrap_err(), Error::FileTooLarge);
        let empty = Tmp::new("empty", b"");
        assert_eq!(load_file(&empty.0).unwrap_err(), Error::Empty);
    }

    #[test]
    fn first_existing_path_wins_and_bad_file_stops_search() {
        let a = Tmp::new("a", b"junk");
        let b = Tmp::new("b", P.as_bytes());
        let pa: &'static str = Box::leak(a.0.to_str().unwrap().to_string().into_boxed_str());
        let pb: &'static str = Box::leak(b.0.to_str().unwrap().to_string().into_boxed_str());
        let gone: &'static str = "/nonexistent/bare-trust-test";
        let (s, used) = load_first(&[gone, pb, pa]).unwrap();
        assert_eq!((s.len(), used), (1, pb));
        // A bad file is an error even when a later path is good.
        assert!(matches!(load_first(&[gone, pa, pb]), Err(Error::Format { .. })));
        assert_eq!(load_first(&[gone]).unwrap_err(), Error::NoSystemStore);
        assert_eq!(load_first(&[]).unwrap_err(), Error::NoSystemStore);
    }

    /// Optional: reads the real OS bundle. Run with `--ignored`.
    #[test]
    #[ignore]
    fn system_store() {
        let (s, p) = load_system().unwrap();
        println!("{p}: {} roots", s.len());
        assert!(s.len() > 50);
    }
}
