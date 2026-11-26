//! `URL.protocol` between the special schemes (`http`, `https`, `ws`, `wss`, `ftp`). WHATWG URL allows a switch from one special
//! scheme to another. T3 Code sets `url.protocol = 'ws:'` on an `http:` URL to make its socket address. The expected values are those of Node's `URL`.

use crate::browser_navigation_url::{parse, parse_any, Part};

fn set(href: &str, proto: &str) -> String {
    let mut r = parse_any(href, None).unwrap();
    match Part::Protocol.set(&mut r, proto) {
        Ok(()) => r.href(),
        Err(e) => format!("{e:?}"),
    }
}

#[test]
fn a_special_scheme_switches_to_another_special_scheme() {
    assert_eq!(set("http://localhost:3773/a?b=1#c", "ws:"), "ws://localhost:3773/a?b=1#c");
    assert_eq!(set("https://h.test/x", "wss"), "wss://h.test/x");
    assert_eq!(set("http://h.test:80/x", "ws"), "ws://h.test/x");
    assert_eq!(set("http://h.test:81/x", "wss"), "wss://h.test:81/x");
    assert_eq!(set("ws://h.test/x", "http:"), "http://h.test/x");
    assert_eq!(set("wss://h.test:8443/x", "https"), "https://h.test:8443/x");
    assert_eq!(set("ws://h.test/x", "wss"), "wss://h.test/x");
    assert_eq!(set("ftp://h.test/x", "http"), "http://h.test/x");
}

#[test]
fn other_switches_change_nothing() {
    assert_eq!(set("http://h.test/x", "mailto"), "http://h.test/x");
    assert_eq!(set("mailto:a@b.c", "http"), "mailto:a@b.c");
    assert_eq!(set("foo://h.test/x", "http"), "foo://h.test/x");
    assert!(parse("http://h.test/", None).is_ok());
}
