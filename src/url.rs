//! Module 1a: URL parsing.
//!
//! A URL like `http://example.com:8080/index.html` has parts:
//! scheme (`http`), host (`example.com`), port (`8080`), path (`/index.html`).
//! We split the string by hand. No library.
//!
//! Comments compare Rust with JavaScript, because you know JS.

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

        let (host_port, path) = match rest.find('/') {
            // JS: like `rest.indexOf("/")`. Rust gives `Some(position)` or `None`, not -1.
            Some(i) => (&rest[..i], &rest[i..]), // keep the leading "/" in the path
            // Found at position `i`. JS: `[rest.slice(0, i), rest.slice(i)]`.
            // The `&` means we borrow pieces of `rest`. We do not copy.
            None => (rest, "/"), // "http://a.com" means path "/"
            // No "/" found. All of it is host. The path is "/".
        };
        // Both arms give a pair of values. JS: `const [hostPort, path] = ...`.

        let (host, port) = match host_port.split_once(':') {
            // JS: like `host_port.split(":")`, but it cuts only at the first ":".
            // It gives `Some((left, right))` or `None`.
            Some((h, p)) => {
                // There is a ":". `h` is the host text and `p` is the port text.
                let port = p.parse::<u16>().map_err(|_| format!("bad port: {p}"))?;
                // JS: like `Number(p)`, but it fails for "abc" and for numbers above 65535.
                // JS would give `NaN`. Rust gives an error that we must handle.
                // `|_| ...` is `_ => ...`: an arrow function that ignores its argument.
                // `?` returns the error if the parse failed.
                (h, port)
                // No `;` here, so this is the value of the arm. JS: like `return [h, port]`.
            }
            None => (host_port, 80), // default HTTP port
            // No ":". The whole text is the host. HTTP uses port 80 by default.
        };

        if host.is_empty() {
            // JS: `host.length === 0`. This catches "http:///x".
            return Err("empty host".to_string());
            // `Err(..)` is the error side of `Result`. JS: like `return { err: "empty host" }`.
            // `.to_string()` makes a `String` we own from the text.
        }

        Ok(Url {
            // `Ok(..)` is the good side of `Result`. We put a new `Url` in it.
            host: host.to_string(),
            // `host` only borrows part of `input`. `.to_string()` copies it, so `Url`
            // does not depend on `input`. JS does this for you with garbage collection.
            port,
            // Short for `port: port`. JS has the same shorthand.
            path: path.to_string(),
            // Copy the path too.
        })
        // No `;`, so this is the return value. Rust returns the last expression.
    }
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
}
