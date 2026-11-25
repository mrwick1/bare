//! The `integrity` option of `fetch` and `Request` (Subresource Integrity). The Teams WebRTC worker passes it; bare stopped the
//! worker with `Unsupported("fetch option integrity")`.

use crate::browser_fetch::sri_match;
use crate::browser_fetch_tests::*;

const SHA256: &str = "sha256-LPJNul+wow4m6DsqxbninhsWHlwfp0JecwQzYpOLmCQ=";
const SHA384: &str = "sha384-WeF0h3dEjGnea4ANejO7+5/xtGPkQ1TDVTvNucZm+pASWjx5+QOXvfX2oT3oKGhP";
const SHA512: &str = "sha512-m3HSJL1i83hdltRq0+o9czGb+8KJDKra4t/3JRlnPKcjI8PZm6XBHXx6zG4UuMXaDEZjR1wuXDre9G9zvN7AQw==";
const OTHER: &str = "sha256-/ddYXgjE4q/XHcq9tGNsidVXo/QtueIEDIu9FwiqTOc=";

#[test]
fn the_strongest_algorithm_decides() {
    for m in [SHA256, SHA384, SHA512, "sha256-LPJNul-wow4m6DsqxbninhsWHlwfp0JecwQzYpOLmCQ", "sha256-LPJNul+wow4m6DsqxbninhsWHlwfp0JecwQzYpOLmCQ=?opt"] {
        assert!(sri_match(m, b"hello"), "{m}");
    }
    assert!(!sri_match(OTHER, b"hello"));
    assert!(sri_match(&format!("{OTHER} {SHA256}"), b"hello"), "one digest of the algorithm is enough");
    assert!(!sri_match(&format!("{SHA256} sha384-AAAA"), b"hello"), "only sha384 counts");
    assert!(sri_match("md5-xyz bad", b"hello"), "no known algorithm: no check");
}

#[test]
fn fetch_with_integrity_resolves_on_a_match_and_rejects_on_a_mismatch() {
    big(|| {
        let srv = serve(|_| ok("hello"));
        let mut b = at(srv.port);
        let src = format!(
            "var out = []; fetch('/a', {{ integrity: '{SHA384}' }}).then(function (r) {{ return r.text(); }}).then(function (t) {{ out.push(t);
             return fetch('/b', {{ integrity: '{OTHER}' }}); }}).then(function () {{ out.push('no'); }}, function (e) {{ out.push(e.name);
             out.push(new Request('/c', {{ integrity: '{SHA256}' }}).integrity === '{SHA256}'); }});"
        );
        assert_eq!(go(&mut b, &src), "hello|TypeError|true");
    });
}
