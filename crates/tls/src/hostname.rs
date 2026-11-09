//! Server identity check: does a certificate's subjectAltName cover the host we dialed?
//!
//! Rules (RFC 9525 section 6 and RFC 5280 section 4.2.1.6):
//! - The reference identity is the URL host. It is never resolved. No network or DNS is used.
//! - A DNS host matches only a dNSName SAN. An IP host matches only an iPAddress SAN.
//!   An IP host never matches a dNSName that spells the same text, and the reverse.
//! - The subject Common Name is never used. A certificate with no matching SAN fails.
//! - dNSName comparison is exact and ASCII case-insensitive (RFC 4343).
//! - Wildcards (RFC 9525 section 6.3): `*` must be the whole leftmost label of the SAN, and at
//!   least two labels must follow it. `*.example.com` matches `www.example.com`. It does not
//!   match `example.com` or `a.b.example.com`. `*.com`, `w*.example.com` and `*.*.example.com`
//!   never match. The code does not check the public suffix list: that is the CA's job. A host
//!   that holds `*` is rejected.
//! - Trailing dot: ONE trailing dot on the reference host is removed (`example.com.` is the
//!   same name as `example.com`). A presented dNSName with a trailing dot never matches.
//! - Internationalized hosts: the caller gives the A-label (punycode) form. Non-ASCII input
//!   is rejected. Nothing is converted here.
//!
//! The API takes plain SAN values so it does not depend on the X.509 parser. The caller
//! maps parser names to `SanName`.

use std::net::{Ipv4Addr, Ipv6Addr};
use std::str::FromStr;

/// One subjectAltName entry as the caller extracted it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SanName<'a> {
    /// dNSName octets.
    Dns(&'a [u8]),
    /// iPAddress octets: 4 (IPv4) or 16 (IPv6). Other lengths never match.
    Ip(&'a [u8]),
    /// Any other name type (email, URI, directoryName...). Ignored.
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The host is empty, too long, non-ASCII, has a bad label, or is a malformed IP.
    BadHost,
    /// No SAN matched. This includes a certificate with no SAN extension.
    NoMatch,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Error::BadHost => "host name is not a valid DNS name or IP address",
            Error::NoMatch => "certificate does not match the host name",
        })
    }
}
impl std::error::Error for Error {}

enum Reference<'a> {
    Dns(&'a [u8]),
    Ip4([u8; 4]),
    Ip6([u8; 16]),
}

/// `host` is the normalized URL host without brackets: `example.com`, `192.0.2.1`, `::1`.
fn reference(host: &str) -> Result<Reference<'_>, Error> {
    let h = host.as_bytes();
    if h.contains(&b':') {
        // Zone ids (`fe80::1%eth0`) are rejected by std: they are not a URL host.
        return Ipv6Addr::from_str(host).map(|a| Reference::Ip6(a.octets())).map_err(|_| Error::BadHost);
    }
    let name = h.strip_suffix(b".").unwrap_or(h);
    if name.is_empty() || name.len() > 253 {
        return Err(Error::BadHost);
    }
    let mut last = name;
    for label in name.split(|&b| b == b'.') {
        let ldh = label.iter().all(|&b| b.is_ascii_alphanumeric() || b == b'-');
        if label.is_empty() || label.len() > 63 || !ldh || label[0] == b'-' || label[label.len() - 1] == b'-' {
            return Err(Error::BadHost);
        }
        last = label;
    }
    // A last label of digits only is an IPv4 spelling (WHATWG URL). It must parse as dotted
    // quad. `1.2.3` or `01.2.3.4` is an error, never a DNS name.
    if last.iter().all(u8::is_ascii_digit) {
        // Safe: only ASCII bytes passed the loop above.
        let s = std::str::from_utf8(name).map_err(|_| Error::BadHost)?;
        return Ipv4Addr::from_str(s).map(|a| Reference::Ip4(a.octets())).map_err(|_| Error::BadHost);
    }
    Ok(Reference::Dns(name))
}

/// `p` is `*.` plus a name of two or more labels, and `host` is one label plus that name.
/// `host` passed the LDH check, so it holds no `*`: a second `*` in `p` can never match.
fn wildcard(host: &[u8], p: &[u8]) -> bool {
    match (p.strip_prefix(b"*."), host.iter().position(|&b| b == b'.')) {
        (Some(rest), Some(dot)) => rest.contains(&b'.') && host[dot + 1..].eq_ignore_ascii_case(rest),
        _ => false,
    }
}

/// Succeeds if some SAN matches `host`. Does not look at the subject Common Name.
pub fn verify<'a>(host: &str, names: impl IntoIterator<Item = SanName<'a>>) -> Result<(), Error> {
    let r = reference(host)?;
    for n in names {
        let hit = match (&r, n) {
            // The reference passed the LDH check, so equality also rules out NUL, `*`,
            // non-ASCII bytes, empty labels and a trailing dot in the presented name.
            (Reference::Dns(d), SanName::Dns(p)) => d.eq_ignore_ascii_case(p) || wildcard(d, p),
            (Reference::Ip4(a), SanName::Ip(p)) => a == p,
            (Reference::Ip6(a), SanName::Ip(p)) => a == p,
            _ => false,
        };
        if hit {
            return Ok(());
        }
    }
    Err(Error::NoMatch)
}

#[cfg(test)]
mod tests {
    use super::*;
    use SanName::*;

    fn ok(host: &str, names: &[SanName]) -> bool {
        verify(host, names.iter().copied()) == Ok(())
    }

    #[test]
    fn exact_dns_target() {
        let san = [Dns(b"staging.8vx.in"), Dns(b"staging.app.8vox.ai")];
        assert!(ok("staging.app.8vox.ai", &san));
        assert!(ok("staging.8vx.in", &san));
        assert_eq!(verify("app.8vox.ai", san), Err(Error::NoMatch));
        assert_eq!(verify("evil.staging.app.8vox.ai", san), Err(Error::NoMatch));
        assert_eq!(verify("staging.app.8vox.ai.evil.com", san), Err(Error::NoMatch));
        assert_eq!(verify("taging.app.8vox.ai", san), Err(Error::NoMatch));
    }

    #[test]
    fn case_insensitive_ascii_only() {
        assert!(ok("Example.COM", &[Dns(b"eXample.com")]));
        // Kelvin sign and dotless i are not ASCII: never folded.
        assert_eq!(verify("example.com", [Dns("exampl\u{212A}.com".as_bytes())]), Err(Error::NoMatch));
        assert_eq!(verify("\u{212A}.com", []), Err(Error::BadHost));
    }

    #[test]
    fn trailing_dot_policy() {
        assert!(ok("example.com.", &[Dns(b"example.com")]));
        assert_eq!(verify("example.com", [Dns(b"example.com.")]), Err(Error::NoMatch));
        assert_eq!(verify("example.com.", [Dns(b"example.com.")]), Err(Error::NoMatch));
        for bad in ["example.com..", ".", ".example.com", "a..b"] {
            assert_eq!(verify(bad, [Dns(b"example.com")]), Err(Error::BadHost), "{bad}");
        }
    }

    #[test]
    fn malformed_hosts() {
        let long_label = "a".repeat(64);
        let long_name = format!("{}.com", ["a".repeat(63).as_str(); 4].join("."));
        for bad in [
            "", " ", "a b.com", "exa\0mple.com", "-a.com", "a-.com", "a_b.com", "*.example.com", "ex*mple.com",
            "caf\u{e9}.com", "a/b", "a@b", "a?b", "[::1]", &long_label, &long_name,
        ] {
            assert_eq!(verify(bad, [Dns(b"example.com")]), Err(Error::BadHost), "{bad:?}");
        }
        assert!(ok(&"a".repeat(63), &[Dns("a".repeat(63).as_bytes())]));
        // 253 bytes is the longest name.
        let max = ["a".repeat(63), "a".repeat(63), "a".repeat(63), "a".repeat(61)].join(".");
        assert_eq!(max.len(), 253);
        assert!(ok(&max, &[Dns(max.as_bytes())]));
    }

    #[test]
    fn punycode_labels_match_as_a_labels() {
        assert!(ok("xn--bcher-kva.example", &[Dns(b"xn--bcher-kva.example")]));
        assert_eq!(verify("xn--bcher-kva.example", [Dns("bücher.example".as_bytes())]), Err(Error::NoMatch));
    }

    #[test]
    fn presented_dns_garbage_never_matches() {
        for p in [&b"example.com\0"[..], b"example.com\0.evil", b"exampl\xc3\xa9.com", b"*.com", b"*", b"", b" example.com", b"example.com "] {
            assert_eq!(verify("example.com", [Dns(p)]), Err(Error::NoMatch), "{p:?}");
        }
        // NUL attack: certificate for `evil.com\0.good.com` must not match `evil.com`.
        assert_eq!(verify("evil.com", [Dns(b"evil.com\0.good.com")]), Err(Error::NoMatch));
    }

    #[test]
    fn wildcards() {
        let w = [Dns(b"*.example.com")];
        assert!(ok("www.example.com", &w));
        assert!(ok("WWW.Example.COM", &w));
        assert!(ok("www.example.com.", &w));
        // The wildcard covers one label, and not the bare domain.
        for h in ["example.com", "a.b.example.com", "www.example.org", "www.xexample.com"] {
            assert_eq!(verify(h, w), Err(Error::NoMatch), "{h}");
        }
        // Partial, double and too-short wildcards never match.
        for p in [&b"w*.example.com"[..], b"*w.example.com", b"*.*.example.com", b"*example.com", b"*.com", b"*.", b"*"] {
            assert_eq!(verify("www.example.com", [Dns(p)]), Err(Error::NoMatch), "{p:?}");
            assert_eq!(verify("example.com", [Dns(p)]), Err(Error::NoMatch), "{p:?}");
        }
        // A wildcard never matches an IP host.
        assert_eq!(verify("192.0.2.1", [Dns(b"*.0.2.1")]), Err(Error::NoMatch));
        // A host that holds `*` is still rejected.
        assert_eq!(verify("*.example.com", w), Err(Error::BadHost));
    }

    #[test]
    fn ipv4() {
        assert!(ok("192.0.2.1", &[Ip(&[192, 0, 2, 1])]));
        assert_eq!(verify("192.0.2.1", [Ip(&[192, 0, 2, 2])]), Err(Error::NoMatch));
        // Wrong SAN type: a dNSName that spells the address does not match an IP host.
        assert_eq!(verify("192.0.2.1", [Dns(b"192.0.2.1")]), Err(Error::NoMatch));
        // Wrong lengths.
        assert_eq!(verify("192.0.2.1", [Ip(&[192, 0, 2]), Ip(&[192, 0, 2, 1, 0]), Ip(&[])]), Err(Error::NoMatch));
        // Not dotted-quad: error, not a DNS fallback.
        for bad in ["1.2.3", "01.2.3.4", "256.1.1.1", "1.2.3.4.5", "1.2.3.4.", "0x7f.0.0.1", "127.1", "1234"] {
            let r = verify(bad, [Dns(bad.as_bytes())]);
            // "1.2.3.4." strips the dot and is then a valid quad that has no matching SAN.
            if bad == "1.2.3.4." {
                assert_eq!(r, Err(Error::NoMatch));
            } else {
                assert_eq!(r, Err(Error::BadHost), "{bad}");
            }
        }
        assert!(ok("1.2.3.4.", &[Ip(&[1, 2, 3, 4])]));
    }

    #[test]
    fn ipv6() {
        let mut a = [0u8; 16];
        a[15] = 1;
        assert!(ok("::1", &[Ip(&a)]));
        assert!(ok("0:0:0:0:0:0:0:1", &[Ip(&a)]));
        assert_eq!(verify("::1", [Dns(b"::1")]), Err(Error::NoMatch));
        assert_eq!(verify("::1", [Ip(&[0, 0, 0, 1])]), Err(Error::NoMatch));
        assert_eq!(verify("::1", [Ip(&[0; 16])]), Err(Error::NoMatch));
        // IPv4-mapped is a 16-byte identity; it does not match a 4-byte SAN.
        assert_eq!(verify("::ffff:1.2.3.4", [Ip(&[1, 2, 3, 4])]), Err(Error::NoMatch));
        let mut m = [0u8; 16];
        m[10] = 0xff;
        m[11] = 0xff;
        m[12..].copy_from_slice(&[1, 2, 3, 4]);
        assert!(ok("::ffff:1.2.3.4", &[Ip(&m)]));
        for bad in ["::1%eth0", ":::1", "1::2::3", "::g", "[::1]", "::1.2.3", "1:2:3:4:5:6:7:8:9", ":1"] {
            assert_eq!(verify(bad, [Ip(&a)]), Err(Error::BadHost), "{bad}");
        }
    }

    #[test]
    fn no_san_no_match_and_other_ignored() {
        assert_eq!(verify("example.com", []), Err(Error::NoMatch));
        assert_eq!(verify("example.com", [Other, Other]), Err(Error::NoMatch));
        assert!(ok("example.com", &[Other, Dns(b"example.com")]));
    }
}
