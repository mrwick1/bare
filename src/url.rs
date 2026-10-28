//! Module 1a: URL parsing.
//!
//! A URL like `http://example.com:8080/index.html` has parts:
//! scheme (`http`), host (`example.com`), port (`8080`), path (`/index.html`).
//! We split the string by hand. No library.
//!
//! Comments compare Rust with JavaScript, because you know JS.

use std::net::Ipv6Addr;
// The standard library's IPv6 address type. It can check and print addresses.

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
            // JS: like `rest.search(/[\/?]/)`: the position of the first "/" or "?".
            // Rust gives `Some(position)` or `None`, not -1.
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
            };
            (ip.to_string(), port)
            // `to_string()` writes the short form: "0:0::1" becomes "::1".
        } else {
            match host_port.split_once(':') {
                // JS: like `host_port.split(":")`, but it cuts only at the first ":".
                // It gives `Some((left, right))` or `None`.
                Some((h, p)) => (ascii_host(h)?, parse_port(p)?),
                // There is a ":". `h` is the host text and `p` is the port text.
                // `ascii_host` makes the host lowercase and ASCII-only. See below.
                // It makes a new `String` we own, so `Url` does not depend on `input`.
                // JS does this for you with garbage collection.
                None => (ascii_host(host_port)?, 80), // default HTTP port
                // No ":". The whole text is the host. HTTP uses port 80 by default.
            }
        };

        if host.is_empty() {
            // JS: `host.length === 0`. This catches "http:///x".
            return Err("empty host".to_string());
            // `Err(..)` is the error side of `Result`. JS: like `return { err: "empty host" }`.
            // `.to_string()` makes a `String` we own from the text.
        }

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
    #[allow(dead_code)] // links are followed in module 6; tests use it now
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

/// Read a port number, like "8080".
fn parse_port(text: &str) -> Result<u16, String> {
    text.parse::<u16>().map_err(|_| format!("bad port: {text}"))
    // JS: like `Number(text)`, but it fails for "abc" and for numbers above 65535.
    // JS would give `NaN`. Rust gives an error that we must handle.
    // `|_| ...` is `_ => ...`: an arrow function that ignores its argument.
}

/// Make a host lowercase and ASCII-only: "BÜCHER.de" becomes "xn--bcher-kva.de".
/// DNS only accepts ASCII, so each non-ASCII label is written in Punycode with an "xn--" prefix.
/// Real browsers also normalize Unicode first (UTS #46). We only lowercase.
fn ascii_host(host: &str) -> Result<String, String> {
    let mut labels = Vec::new();
    for label in host.to_lowercase().split('.') {
        // `to_lowercase` works on all of Unicode: "Ü" becomes "ü".
        // A label is one part between dots: "bücher" and "de".
        if label.is_ascii() {
            labels.push(label.to_string());
            // Plain ASCII labels stay as they are.
        } else if label.chars().count() > 63 {
            return Err(format!("host label too long: {label}"));
            // DNS allows at most 63 characters per label. This also keeps the numbers
            // in `punycode` small, so they cannot overflow.
        } else {
            labels.push(format!("xn--{}", punycode(label)));
        }
    }
    Ok(labels.join("."))
}

/// Encode one label in Punycode (RFC 3492): "bücher" becomes "bcher-kva".
///
/// The idea: first copy the ASCII letters ("bcher"), then add "-". After that, each
/// non-ASCII character is stored as a number: which character it is, and where it goes.
/// These numbers are written with the digits a-z and 0-9.
fn punycode(label: &str) -> String {
    let chars: Vec<u32> = label.chars().map(|c| c as u32).collect();
    // Each character as its Unicode number. "ü" is 252. JS: `[...label].map(c => c.codePointAt(0))`.
    let mut out: String = label.chars().filter(|c| c.is_ascii()).collect();
    // Start with the ASCII characters, in order: "bcher".
    let ascii_count = out.len() as u32;
    // `as u32` converts the number type. Rust never converts number types on its own.
    if ascii_count > 0 {
        out.push('-');
        // The "-" separates the ASCII part from the encoded part.
    }

    let mut done = ascii_count;
    // How many characters are already placed.
    let mut n = 128;
    // The character we look for next. We go from small Unicode numbers to big ones.
    let mut delta: u32 = 0;
    // One running number that holds both "which character" and "where it goes".
    let mut bias = 72;
    // Tunes how many digits each number needs. It adapts as we go.

    while (done as usize) < chars.len() {
        let next = *chars.iter().filter(|&&c| c >= n).min().unwrap();
        // The smallest character not yet placed. There is one, because `done` < all.
        // `&&c` unpacks two layers of borrowing. JS just has `c`.
        delta += (next - n) * (done + 1);
        // Skipping from `n` to `next` costs one step for each slot at each skipped value.
        n = next;
        for &c in &chars {
            if c < n {
                delta += 1;
                // A character placed earlier: one more slot to skip.
            }
            if c == n {
                // Write `delta` as a variable-length number.
                let mut q = delta;
                let mut k = 36;
                loop {
                    // JS: `while (true)`.
                    let t = if k <= bias { 1 } else if k >= bias + 26 { 26 } else { k - bias };
                    // The threshold for this digit. A digit below it ends the number.
                    if q < t {
                        break;
                    }
                    out.push(digit(t + (q - t) % (36 - t)));
                    q = (q - t) / (36 - t);
                    k += 36;
                }
                out.push(digit(q));
                // The last digit. It is below the threshold, so the reader knows the number ends.
                bias = adapt(delta, done + 1, done == ascii_count);
                delta = 0;
                done += 1;
            }
        }
        delta += 1;
        n += 1;
    }
    out
}

/// Tune `bias` after each number, so the next numbers use few digits (RFC 3492, section 6.1).
fn adapt(delta: u32, count: u32, first: bool) -> u32 {
    let mut delta = if first { delta / 700 } else { delta / 2 };
    delta += delta / count;
    let mut k = 0;
    while delta > 455 {
        // 455 is ((36 - 1) * 26) / 2 from the RFC.
        delta /= 35;
        k += 36;
    }
    k + (36 * delta) / (delta + 38)
}

/// One Punycode digit: 0-25 are "a"-"z", 26-35 are "0"-"9".
fn digit(d: u32) -> char {
    if d < 26 { (b'a' + d as u8) as char } else { (b'0' + (d - 26) as u8) as char }
}

/// Replace unsafe bytes with "%XX" codes: "my file" becomes "my%20file".
/// The byte sets follow the WHATWG URL Standard for paths and queries.
fn encode(path: &str) -> String {
    let mut out = String::new();
    let mut in_query = false;
    // The path and the query encode slightly different characters.
    for byte in path.bytes() {
        // `.bytes()` gives the UTF-8 bytes. "é" is two bytes: 0xC3 0xA9.
        // JS: like looping over `new TextEncoder().encode(path)`.
        let escape = byte <= b' ' || byte > b'~' || b"\"<>".contains(&byte)
            // Control bytes, space, non-ASCII bytes, and `"`, `<`, `>`, everywhere.
            // `b' '` is the byte value of a character. `b"..."` is a list of bytes.
            || if in_query { byte == b'\'' } else { b"`{}".contains(&byte) };
            // `'` only in the query. The backtick, `{` and `}` only in the path.
        if escape {
            out.push_str(&format!("%{byte:02X}"));
            // `:02X` writes two uppercase hex digits. JS: `byte.toString(16).toUpperCase().padStart(2, "0")`.
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
    // The returned `&str` is a piece of `text`. Rust knows this, because `text` is the
    // only borrowed input. So the piece cannot outlive `text`.
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
            // `*part` reads the value that `part` points to.
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
    fn rejects_bad_port() {
        assert!(Url::parse("http://a.com:abc/").is_err());
        // "abc" is not a number, so this must fail.
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
    fn punycode_labels() {
        // Expected values come from Python's built-in "punycode" codec.
        let cases = [("bücher", "bcher-kva"), ("münchen", "mnchen-3ya"), ("中文", "fiq228c"), ("faß", "fa-hia"), ("ñ", "ida")];
        for (label, expected) in cases {
            assert_eq!(punycode(label), expected, "label {label}");
        }
    }

    #[test]
    fn non_ascii_host() {
        assert_eq!(Url::parse("http://BÜCHER.de/").unwrap().host, "xn--bcher-kva.de");
        // Lowercase first, then Punycode. Only the non-ASCII label changes.
        assert_eq!(Url::parse("http://例え.jp:8080/").unwrap().port, 8080);
        assert!(Url::parse(&format!("http://{}.com/", "é".repeat(64))).is_err());
        // DNS allows at most 63 characters in one label.
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
