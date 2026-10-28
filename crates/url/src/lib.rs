//! Module 1a: URL parsing.
//!
//! A URL like `http://example.com:8080/index.html` has parts:
//! scheme (`http`), host (`example.com`), port (`8080`), path (`/index.html`).
//! We split the string by hand. No library.
//!
//! Comments compare Rust with JavaScript, because you know JS.

mod idna;
// IDNA for Unicode host names, in `idna.rs`. No `pub`: only this crate uses it.
use std::net::{Ipv4Addr, Ipv6Addr};
// The standard library's IP address types. They can check and print addresses.

/// A parsed URL.
#[derive(Debug, PartialEq)]
// `#[derive(...)]` makes the compiler write code for you.
// `Debug` lets you print it, like `console.log(obj)`.
// `PartialEq` lets you compare with `==`. In JS, `==` on two objects compares references.
// In Rust, with `PartialEq`, it compares the fields.
pub struct Url {
    // JS: like a TypeScript `interface Url { host: string; port: number; path: string }`.
    // `pub` is like `export`. Other files can use it.
    pub host: String,
    // The server name, like "example.com". `String` is text that this struct owns.
    pub port: u16,
    // JS has one `number` type. Rust has many. `u16` is a whole number from 0 to 65535,
    // which is exactly the range of valid ports.
    pub path: String,
    // What to ask for, like "/index.html".
}

impl Url {
    // JS: like the body of `class Url { ... }`. These functions belong to `Url`.

    /// Parse a string into a `Url`.
    pub fn parse(input: &str) -> Result<Url, String> {
        // JS: like `static parse(input)`. You call it as `Url::parse(..)`.
        // `&str` is text we only read. `String` is text we own. JS has only one kind of string.
        // The return type is `Result<Url, String>`: a good `Url`, or an error message.
        // Rust has no `throw`. A function that can fail returns a `Result`.

        let rest = input
            .strip_prefix("http://")
            // JS: like `startsWith("http://")` and then `slice(7)`, in one step.
            // It gives `Some(the rest)` or `None`. `None` is like `undefined`.
            .ok_or_else(|| format!("only http:// is supported, got: {input}"))?;
        // `ok_or_else` turns `None` into an error with this message.
        // `format!` is a template string: JS `` `only http:// ... ${input}` ``.
        // `?` is the key idea. If the value is an error, return it from this function now.
        // JS: like `if (err) return err;`. If it is good, take the value out.
        // `rest` is the text after "http://".

        let rest = before(rest, '#');
        // Drop the fragment ("#top"). It points to a spot in the page. The server never sees it.
        // Rust lets you reuse a name with a new `let`. This is called shadowing.
        // JS would say "already declared". The old `rest` is simply hidden.

        let (host_port, path) = match rest.find(['/', '?']) {
            // Find the position of the first "/" or "?".
            // Rust gives `Some(position)`, or `None` if there is none.
            // We stop at "?" too, so "http://a.com?x=1" has host "a.com".
            Some(i) => (&rest[..i], &rest[i..]), // keep the leading "/" in the path
            // Found at position `i`. JS: `[rest.slice(0, i), rest.slice(i)]`.
            // The `&` means we borrow pieces of `rest`. We do not copy.
            None => (rest, "/"), // "http://a.com" means path "/"
            // No "/" found. All of it is host. The path is "/".
        };
        // Both arms give a pair of values. JS: `const [hostPort, path] = ...`.

        let path = if path.starts_with('?') { format!("/{path}") } else { path.to_string() };
        // "?x=1" becomes "/?x=1". The request line needs a path that starts with "/".
        // In Rust, `if` gives back a value, like the JS ternary `a ? b : c`.

        let (host, port) = if let Some(inside) = host_port.strip_prefix('[') {
            // An IPv6 host, like "[::1]:8080". The address itself contains ":",
            // so "[ ]" mark where it ends. Only then can we find the port.
            let (ip, after) = inside.split_once(']').ok_or_else(|| "missing ] in IPv6 host".to_string())?;
            let ip: Ipv6Addr = ip.parse().map_err(|_| format!("bad IPv6 address: {ip}"))?;
            // The standard library checks the address for us. JS has no built-in for this.
            // `ip: Ipv6Addr` tells `parse` which type to make. Like a TypeScript type annotation.
            let port = match after.strip_prefix(':') {
                Some(p) => parse_port(p)?,
                None if after.is_empty() => 80,
                // `if` in a match arm is a guard. This arm runs only when nothing follows "]".
                None => return Err(format!("bad text after IPv6 address: {after}")),
                // `Err(..)` is the error side of `Result`. JS: like `return { err: "..." }`.
            };
            (ip.to_string(), port)
            // `to_string()` writes the short form: "0:0::1" becomes "::1".
        } else {
            match host_port.split_once(':') {
                // JS: like `host_port.split(":")`, but it cuts only at the first ":".
                // It gives `Some((left, right))` or `None`.
                Some((h, p)) => (ascii_host(h)?, parse_port(p)?),
                // There is a ":". `h` is the host text and `p` is the port text.
                // `ascii_host` makes the host ASCII-only, for DNS. See below.
                // It makes a new `String` that the `Url` keeps for itself.
                None => (ascii_host(host_port)?, 80), // default HTTP port
                // No ":". The whole text is the host. HTTP uses port 80 by default.
            }
        };
        // An empty host, like in "http:///x", is an error from `ascii_host`.

        Ok(Url {
            // `Ok(..)` is the good side of `Result`. We put a new `Url` in it.
            host,
            // Short for `host: host`. JS has the same shorthand.
            port,
            path: encode(&path),
            // Turn spaces and non-ASCII characters into "%XX" codes.
        })
        // No `;`, so this is the return value. Rust returns the last expression.
    }

    /// Resolve a link found on this page, like `<a href="../about">`.
    /// JS: like `new URL(link, base)`.
    pub fn join(&self, link: &str) -> Result<Url, String> {
        // `&self` is like `this` in JS. We read the page's URL. We do not change it.

        let link = before(link, '#');
        // The fragment never matters for loading. "#top" alone becomes "".

        if link.starts_with("//") {
            // "//cdn.com/x.js": a new host, same scheme as the page.
            return Url::parse(&format!("http:{link}"));
        }

        if let Some((scheme, _)) = link.split_once(':') {
            // `if let` is a `match` with one arm. It runs only if the pattern fits.
            // JS: `const parts = ...; if (parts) { const [scheme] = parts; ... }`.
            if !scheme.is_empty() && scheme.chars().all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c)) {
                // Text like "http:" or "mailto:" is a scheme. Then the link is a full URL.
                // `chars().all(..)` is JS `[...scheme].every(..)`.
                // "a/b:c" is not a scheme, because "/" is not allowed in a scheme.
                return Url::parse(link);
                // `parse` gives an error for anything but http://.
            }
        }

        let page = before(&self.path, '?');
        // The page path without its query: "/blog/post.html".
        let path = if link.is_empty() {
            self.path.clone()
            // Only a fragment: stay on the same page. `.clone()` copies the `String`.
        } else if link.starts_with('/') {
            link.to_string()
            // From the root of the site.
        } else if link.starts_with('?') {
            format!("{page}{link}")
            // Same page, new query.
        } else {
            let folder = &page[..=page.rfind('/').unwrap_or(0)];
            // The page's folder, with the last "/": "/blog/". JS: `page.slice(0, page.lastIndexOf("/") + 1)`.
            // `..=` includes the end position. Our paths always start with "/", so `rfind` finds one.
            format!("{folder}{link}")
            // Relative to that folder: "/blog/" + "cat.png".
        };

        Ok(Url { host: self.host.clone(), port: self.port, path: encode(&remove_dots(&path)) })
        // Same server as the page. `self.port` is a number, so it is copied for free.
        // Numbers are `Copy` in Rust. Strings are not, so they need `.clone()`.
    }
}

/// Read a port number, like "8080". Empty text means the default port, 80.
fn parse_port(text: &str) -> Result<u16, String> {
    if text.is_empty() {
        return Ok(80);
    }
    if !text.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("bad port: {text}"));
        // Only digits. Rust's `parse` would also accept "+80".
    }
    text.parse::<u16>().map_err(|_| format!("bad port: {text}"))
    // JS: like `Number(text)`, but it fails for "abc" and for numbers above 65535.
    // JS would give `NaN`. Rust gives an error that we must handle.
    // `|_| ...` is `_ => ...`: an arrow function that ignores its argument.
}

/// Turn the host text from a URL into the ASCII host that DNS needs.
/// "BÜCHER.de" becomes "xn--bcher-kva.de" (see `idna.rs`).
fn ascii_host(host: &str) -> Result<String, String> {
    let bytes = percent_decode(host);
    // Browsers decode "%XX" in the host first: "exam%70le.com" is "example.com".
    let host = String::from_utf8_lossy(&bytes);
    // Bytes that are not valid UTF-8 become "\u{FFFD}". IDNA then rejects the host.
    let ascii = idna::domain_to_ascii(&host)?;
    if ends_in_number(&ascii) {
        // A host that ends in a number must be an IPv4 address. "example.255" is an error.
        let ip = parse_ipv4(&ascii).ok_or_else(|| format!("bad IPv4 address: {ascii}"))?;
        return Ok(Ipv4Addr::from(ip).to_string());
        // Write it the normal way: "0x7f.1" becomes "127.0.0.1".
    }
    Ok(ascii)
}

/// True if the last label is a number: "1.2.3.4", "a.0x1f".
fn ends_in_number(host: &str) -> bool {
    let host = host.strip_suffix('.').unwrap_or(host);
    // One trailing dot is ignored: "1.2.3.4." ends in "4".
    let last = host.rsplit('.').next().unwrap_or("");
    // `rsplit` splits from the end, so `.next()` gives the last label.
    let hex = last.strip_prefix("0x").or_else(|| last.strip_prefix("0X"));
    (!last.is_empty() && last.bytes().all(|b| b.is_ascii_digit())) || hex.is_some_and(|h| h.bytes().all(|b| b.is_ascii_hexdigit()))
    // All digits counts, even "09", which is not a valid number. "0x" plus hex digits counts too,
    // even when the number is too big. The IPv4 parser rejects those later.
}

/// Read an IPv4 address the way browsers do, and give it as one number.
/// Browsers accept some old short forms too: "127.1", "0x7f.1", "0300.0250.0.1".
fn parse_ipv4(host: &str) -> Option<u32> {
    let host = host.strip_suffix('.').unwrap_or(host);
    let mut numbers = Vec::new();
    for part in host.split('.') {
        numbers.push(ipv4_number(part)?);
    }
    if numbers.len() > 4 {
        return None;
    }
    let last = numbers.pop()?;
    // `pop` takes the last number off the list. JS: `numbers.pop()`.
    if numbers.iter().any(|&n| n > 255) || last >= 1 << (8 * (4 - numbers.len())) {
        return None;
        // Each number before the last must fit in one part (0 to 255).
        // The last number may fill all the parts that are left, so it may be bigger.
    }
    let mut ip = last;
    for (i, n) in numbers.iter().enumerate() {
        ip += n << (8 * (3 - i));
        // Put each number in its place: the first number goes first.
    }
    u32::try_from(ip).ok()
    // `try_from` converts to a smaller number type. It fails if the number does not fit.
}

/// Parse one IPv4 part: decimal ("10"), hex ("0xa") or octal ("012"). `None` if it is not a number.
fn ipv4_number(part: &str) -> Option<u64> {
    if part.is_empty() {
        return None;
    }
    let (digits, radix) = if let Some(hex) = part.strip_prefix("0x").or_else(|| part.strip_prefix("0X")) {
        (hex, 16)
    } else if part.len() > 1 && part.starts_with('0') {
        (&part[1..], 8)
    } else {
        (part, 10)
    };
    if digits.is_empty() {
        return Some(0); // "0x" alone is 0
    }
    let mut value: u64 = 0;
    for c in digits.chars() {
        let d = c.to_digit(radix)?;
        // `to_digit(16)` turns "f" into 15. It gives `None` for a character that is not a digit.
        value = value.checked_mul(radix as u64)?.checked_add(d as u64)?;
        // Add the digit. If the number gets far too big, stop with `None`.
    }
    Some(value)
}

/// Replace each "%XX" with the byte it stands for. A "%" without two hex digits stays.
fn percent_decode(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes.get(i + 1..i + 3).filter(|h| h.iter().all(u8::is_ascii_hexdigit));
        // `.get(range)` gives `None` instead of crashing when the range is past the end.
        match (bytes[i], hex) {
            (b'%', Some(h)) => {
                let value = |b: u8| (b as char).to_digit(16).unwrap_or(0) as u8;
                // One hex digit as a number: "a" gives 10. Both digits were checked above.
                out.push(value(h[0]) * 16 + value(h[1]));
                i += 3;
            }
            (b, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    out
}

/// Replace unsafe bytes with "%XX" codes: "my file" becomes "my%20file".
/// The list of unsafe characters comes from the URL standard that browsers follow.
fn encode(path: &str) -> String {
    let mut out = String::new();
    let mut in_query = false;
    // The path and the query encode slightly different characters.
    for byte in path.bytes() {
        // Look at the text one byte at a time. Plain letters are one byte. "é" is two bytes.
        let escape = byte <= b' ' || byte > b'~' || b"\"<>".contains(&byte)
            // Control bytes, space, non-ASCII bytes, and `"`, `<`, `>`, everywhere.
            // `b' '` is the byte value of a character. `b"..."` is a list of bytes.
            || if in_query { byte == b'\'' } else { b"`{}".contains(&byte) };
            // `'` only in the query. The backtick, `{` and `}` only in the path.
        if escape {
            out.push_str(&format!("%{byte:02X}"));
            // Write the byte as "%" and two hex digits: a space becomes "%20".
        } else {
            out.push(byte as char);
            // `as char` turns the byte into a character. Safe here, because it is plain ASCII.
        }
        if byte == b'?' {
            in_query = true;
            // The first "?" ends the path. Everything after it is the query.
        }
    }
    out
}

/// The text before the first `ch`, or all of `text` if there is no `ch`.
fn before(text: &str, ch: char) -> &str {
    // The answer is a piece of `text`, not a copy.
    match text.split_once(ch) {
        Some((head, _)) => head,
        None => text,
    }
}

/// Remove "." and ".." parts: "/a/./b/../c" becomes "/a/c".
fn remove_dots(path: &str) -> String {
    let (path, query) = match path.split_once('?') {
        Some((p, q)) => (p, format!("?{q}")),
        None => (path, String::new()),
    };
    // Dots only matter in the path. Keep the query aside and add it back at the end.

    let parts: Vec<&str> = path.split('/').skip(1).collect();
    // "/a/./b" gives ["a", ".", "b"]. `skip(1)` drops the empty text before the first "/".
    // `collect()` turns the parts into a `Vec`. JS: `path.split("/").slice(1)`.

    let mut out: Vec<&str> = Vec::new();
    // Works like a stack. JS: `const out = []`.
    for part in &parts {
        match *part {
            // `*part` reads the text itself.
            "." => {}
            // "." means "this folder". Skip it.
            ".." => {
                out.pop();
                // ".." means "one folder up". Remove the last part. On an empty stack it does nothing.
            }
            other => out.push(other),
            // Any other part: keep it.
        }
    }
    if matches!(parts.last(), Some(&".") | Some(&"..")) {
        out.push("");
        // A path that ends in "." or ".." means a folder, so it keeps its last "/".
        // `matches!` is a `match` that gives true or false.
    }

    format!("/{}{query}", out.join("/"))
    // JS: `"/" + out.join("/") + query`.
}

#[cfg(test)]
// This code is only compiled for `cargo test`. It is not in the real program.
mod tests {
    use super::*;
    // JS: like `import * from "./url"`. `super` is the file above, this file.

    #[test]
    // JS: like `test("host only", () => { ... })` in Jest. A panic means the test failed.
    fn host_only() {
        let u = Url::parse("http://example.com").unwrap();
        // `unwrap()` means "I expect this to work. If not, crash." Fine in tests.
        assert_eq!(u, Url { host: "example.com".into(), port: 80, path: "/".into() });
        // JS: like `expect(u).toEqual({...})`. `.into()` turns `&str` into `String`.
    }

    #[test]
    fn host_port_path() {
        let u = Url::parse("http://localhost:8080/a/b.html").unwrap();
        // A full URL with a port and a path.
        assert_eq!(u, Url { host: "localhost".into(), port: 8080, path: "/a/b.html".into() });
        // Checks that all three parts are split correctly.
    }

    #[test]
    fn rejects_other_schemes() {
        assert!(Url::parse("https://example.com").is_err());
        // JS: like `expect(...).toBe(true)`. `is_err()` is true for an error.
        // We do not support HTTPS yet, so this must fail.
    }

    #[test]
    fn empty_port() {
        assert_eq!(Url::parse("http://f:/c").unwrap().port, 80);
        assert_eq!(Url::parse("http://[::1]:/").unwrap().port, 80);
        // A ":" with no number after it means the default port.
    }

    #[test]
    fn rejects_bad_port() {
        assert!(Url::parse("http://a.com:abc/").is_err());
        // "abc" is not a number, so this must fail.
        assert!(Url::parse("http://a.com:+80/").is_err());
        // A port has digits only. No sign.
    }

    #[test]
    fn rejects_empty_host() {
        assert!(Url::parse("http:///x").is_err());
        // Nothing stands between "http://" and "/x", so there is no host.
    }

    #[test]
    fn drops_fragment() {
        let u = Url::parse("http://a.com/page#top").unwrap();
        assert_eq!(u.path, "/page");
        // The "#top" part is for the browser only. It must not reach the server.
    }

    #[test]
    fn lowercases_host() {
        assert_eq!(Url::parse("http://EXAMPLE.com/").unwrap().host, "example.com");
        // Host names ignore case. "EXAMPLE.com" and "example.com" are the same server.
    }

    #[test]
    fn query_without_path() {
        let u = Url::parse("http://a.com?x=1").unwrap();
        assert_eq!(u.host, "a.com");
        assert_eq!(u.path, "/?x=1");
        // The "?" ends the host, even with no "/" before it.
    }

    #[test]
    fn joins_relative_links() {
        let base = Url::parse("http://a.com/blog/post.html?p=1").unwrap();
        // A page at this address. Its links are resolved against it.
        let cases = [
            // JS: an array of [link, expected] pairs.
            ("/s.js", "/s.js"),                     // from the root of the site
            ("cat.png", "/blog/cat.png"),           // same folder as the page
            ("../about", "/about"),                 // one folder up
            ("./a/./b/../c", "/blog/a/c"),          // "." means here, ".." means up
            ("?p=2", "/blog/post.html?p=2"),        // same page, new query
            ("#top", "/blog/post.html?p=1"),        // same page, fragment dropped
            ("..", "/"),                            // up from /blog/ gives the root
        ];
        for (link, path) in cases {
            // JS: `for (const [link, path] of cases)`.
            let u = base.join(link).unwrap();
            assert_eq!(u.host, "a.com", "link {link}");
            assert_eq!(u.path, path, "link {link}");
            // The text after the values is printed if the check fails.
        }
    }

    #[test]
    fn ipv6_host() {
        let u = Url::parse("http://[::1]:8080/x").unwrap();
        assert_eq!(u, Url { host: "::1".into(), port: 8080, path: "/x".into() });
        // The "[ ]" only separate the address from the port. We store the address without them.
        assert_eq!(Url::parse("http://[0:0::1]/").unwrap().host, "::1");
        // One address can be written in many ways. We store the short form.
        assert!(Url::parse("http://[::1/").is_err()); // no closing "]"
        assert!(Url::parse("http://[zz]/").is_err()); // not an IPv6 address
    }

    #[test]
    fn non_ascii_host() {
        assert_eq!(Url::parse("http://BÜCHER.de/").unwrap().host, "xn--bcher-kva.de");
        // Lowercase first, then Punycode. Only the non-ASCII label changes.
        assert_eq!(Url::parse("http://例え.jp:8080/").unwrap().port, 8080);
        assert!(Url::parse("http://a\u{200D}b.com/").is_err());
        // An invisible joiner in the wrong place. More cases are in `idna.rs`.
    }

    #[test]
    fn ipv4_hosts() {
        let host = |h: &str| Url::parse(&format!("http://{h}/")).map(|u| u.host);
        // Returns the parsed host, or the error.
        let cases = [
            ("127.0.0.1", "127.0.0.1"),
            ("127.1", "127.0.0.1"),             // the last number fills the remaining bytes
            ("0x7f.1", "127.0.0.1"),            // "0x" means hexadecimal
            ("0300.0250.0.1", "192.168.0.1"),   // a leading "0" means octal
            ("4294967295", "255.255.255.255"),  // one number for all four bytes
            ("0x", "0.0.0.0"),                  // "0x" with no digits is 0
            ("1.2.3.4.", "1.2.3.4"),            // one trailing dot is allowed
            ("１２７.0.0.1", "127.0.0.1"),      // fullwidth digits are mapped first (IDNA)
            ("1.2.3.4x", "1.2.3.4x"),           // does not end in a number, so it is a domain
            ("1.2.3.0x1g", "1.2.3.0x1g"),       // "0x1g" is not a hex number, so this is a domain too
        ];
        for (input, expected) in cases {
            assert_eq!(host(input).as_deref(), Ok(expected), "host {input}");
        }
        for bad in ["4294967296", "256.0.0.1", "1.2.3.4.5", "1.2.3.09", "example.255", "foo.0xffffffffffffffffface123"] {
            assert!(host(bad).is_err(), "host {bad} should fail");
            // Each one ends in a number, so it must be a valid IPv4 address. It is not.
        }
    }

    #[test]
    fn percent_encoded_host() {
        assert_eq!(Url::parse("http://exam%70le.COM/").unwrap().host, "example.com");
        // "%70" is "p". Decode first, then lowercase.
        assert_eq!(Url::parse("http://b%C3%BCcher.de/").unwrap().host, "xn--bcher-kva.de");
        // "%C3%BC" is the UTF-8 bytes of "ü".
        assert!(Url::parse("http://a%20b.com/").is_err());
        // "%20" is a space, which is not allowed in a host.
        assert!(Url::parse("http://a%zz.com/").is_err());
        // Not a valid escape, so "%" stays, and "%" is not allowed in a host.
    }

    #[test]
    fn encodes_unsafe_characters() {
        let u = Url::parse("http://a.com/my file.html?q=héllo").unwrap();
        assert_eq!(u.path, "/my%20file.html?q=h%C3%A9llo");
        // A space becomes "%20". "é" is two bytes in UTF-8, so it becomes two codes.
        assert_eq!(Url::parse("http://a.com/a%20b").unwrap().path, "/a%20b");
        // Text that is already encoded stays the same. "%" is never encoded again.
        let base = Url::parse("http://a.com/").unwrap();
        assert_eq!(base.join("my page.html").unwrap().path, "/my%20page.html");
        // Links from a page are encoded too.
    }

    #[test]
    fn joins_absolute_links() {
        let base = Url::parse("http://a.com/x").unwrap();
        assert_eq!(base.join("http://b.com/y").unwrap().host, "b.com");
        // A full URL ignores the base.
        assert_eq!(base.join("//cdn.com/z.js").unwrap().host, "cdn.com");
        // "//host/path" keeps the base scheme (http) and replaces the rest.
        assert!(base.join("mailto:me@a.com").is_err());
        // Other schemes are not supported yet.
    }
}
