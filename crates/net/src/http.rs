//! Module 1b: a minimal HTTP/1.1 client.
//!
//! HTTP is text over a TCP connection. We send a request, then read the reply.
//!
//! Request:                    Response:
//!   GET /path HTTP/1.1          HTTP/1.1 200 OK
//!   Host: example.com           Content-Type: text/html
//!   Connection: close
//!   (blank line)                (blank line)
//!                               <html>...</html>
//!
//! Comments compare Rust with JavaScript, because you know JS.

use crate::{dns, tcp};
// JS: like `import * as dns from "./dns"`: other files in this crate.
use bare_url::Url;
// JS: like `import { Url } from "bare-url"`: from another package.
use std::io::{ErrorKind, Read, Write};
// These give us the `read` and `write` functions on a connection.
// Rust needs this import before you can call them.
use std::net::SocketAddr;
// `SocketAddr` is an IP address plus a port.

/// Fetch a URL and return the response body as text.
pub fn get(url: &Url) -> Result<String, Box<dyn std::error::Error>> {
    // JS: like `export function get(url)`. `&Url` means we borrow it. The caller keeps it.
    // It returns the page text, or an error.
    // `Box<dyn Error>` means "any kind of error".
    // We need it because this function can fail in two ways: our own error text, and
    // network errors. Both fit into this one type.

    let addrs: Vec<SocketAddr> = dns::resolve(&url.host)?.into_iter().map(|ip| SocketAddr::new(ip, url.port)).collect();
    // Our own DNS lookup (see `dns.rs`) gives IP addresses. Add the port to each one.
    // JS: `ips.map(ip => ({ ip, port }))`.
    let mut stream = tcp::connect(&addrs)?;
    // Open a connection to the server, with time limits (see `tcp.rs`).
    // `?` returns the error to the caller if the connection fails.
    // `mut`: in Rust, `let` is like JS `const`. `let mut` is like JS `let`. We need it
    // because sending and reading change the connection.

    let request = format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nUser-Agent: bare/0.1\r\n\r\n",
        url.path,
        host_header(url)
    );
    // JS: a template string. Each `{}` is replaced by the next value. Like `${url.path}`.
    // `\r\n` is the line ending that HTTP requires.
    // `Host:` says which site we want. One server can host many sites.
    // `Connection: close` asks the server to close the connection when it is done.
    // Then we know the page is complete when the connection closes.
    // The empty line at the end (`\r\n\r\n`) tells the server that the request is complete.

    stream.write_all(request.as_bytes())?;
    // Send the request. A connection sends bytes, not text. `as_bytes` gives the bytes.
    // `write_all` keeps writing until all bytes are sent, or there is an error.

    let mut raw = Vec::new();
    // JS: like `const raw = []`. A `Vec` is a growable list. This one holds bytes.
    // `mut` because we add data to it.
    stream.read_to_end(&mut raw).map_err(|e| match e.kind() {
        ErrorKind::WouldBlock | ErrorKind::TimedOut => std::io::Error::new(e.kind(), "the server stopped answering"),
        _ => e,
    })?;
    // Read everything the server sends, until it closes the connection.
    // If the server sends nothing for 30 seconds, the read stops with a "would block" error.
    // We replace that confusing name with a clear message.
    // `&mut raw` means "lend it, and you may change it".

    let text = String::from_utf8_lossy(&raw);
    // Turn the bytes into text. Bad bytes become "�" instead of failing.

    let (head, body) = split_response(&text)?;
    // Cut the reply into headers and body. This is our own function, below.
    // JS: like `const [head, body] = ...`. `?` returns the error if the reply is malformed.

    let status_line = head.lines().next().unwrap_or("");
    // `head.lines()` goes through the headers line by line.
    // `.next()` takes the first line, like "HTTP/1.1 200 OK". It may not exist, so it is an `Option`.
    // `unwrap_or("")` means: use the line, or "" if it is missing.
    if !status_line.contains(" 200 ") {
        // JS: `!status_line.includes(" 200 ")`. 200 means success.
        // For now, any other code (301, 404...) is an error.
        return Err(format!("unsupported status: {status_line}").into());
        // `.into()` converts our text into the `Box<dyn Error>` type that this function returns.
    }

    Ok(body.to_string())
    // Return the body as success. `.to_string()` copies it.
    // We must copy, because `body` is a piece of `text`, and Rust deletes `text` when
    // the function ends. JS cleans up for you. Rust needs us to keep what we return.
}

/// The value of the `Host:` header, like "a.com" or "[::1]:8080".
fn host_header(url: &Url) -> String {
    let host = if url.host.contains(':') { format!("[{}]", url.host) } else { url.host.clone() };
    // Only IPv6 addresses contain ":". They need their "[ ]" back.
    if url.port == 80 { host } else { format!("{host}:{}", url.port) }
    // The port is sent only when it is not the default.
}

/// Split a raw response into headers and body at the first blank line.
fn split_response(text: &str) -> Result<(&str, &str), String> {
    // No `pub`, so only this file can use it. JS: a function that is not exported.
    // The two `&str` we return are pieces of `text`. They are not copies.
    // JS: like `slice`.
    text.split_once("\r\n\r\n")
        // JS: like splitting at the first "\r\n\r\n" only. It gives `Some((head, body))` or `None`.
        .ok_or_else(|| "malformed response: no blank line after headers".to_string())
    // `None` becomes an error with this message. No `;`, so this is the returned value.
}

#[cfg(test)]
// Only compiled for `cargo test`.
mod tests {
    use super::*;
    // Import everything from this file, including the private `split_response`.

    #[test]
    // JS: like `test(...)` in Jest.
    fn splits_head_and_body() {
        let (head, body) = split_response("HTTP/1.1 200 OK\r\nA: b\r\n\r\nhello").unwrap();
        // A fake reply. `unwrap()` means "expect success, or crash". Fine in tests.
        assert_eq!(head, "HTTP/1.1 200 OK\r\nA: b");
        // JS: `expect(head).toBe(...)`. The head is everything before the blank line.
        assert_eq!(body, "hello");
        // The body is everything after it.
    }

    #[test]
    fn sends_host_header() {
        let header = |input| host_header(&Url::parse(input).unwrap());
        // A small arrow function, so each check below is one line.
        assert_eq!(header("http://a.com/"), "a.com");
        // Port 80 is the default, so it is left out.
        assert_eq!(header("http://a.com:8080/"), "a.com:8080");
        // Any other port must be sent. One server can run different sites on different ports.
        assert_eq!(header("http://[::1]:8080/"), "[::1]:8080");
        // An IPv6 address gets its "[ ]" back, so its ":" do not mix with the port's ":".
    }

    #[test]
    fn rejects_garbage() {
        assert!(split_response("not http").is_err());
        // There is no blank line, so this must be an error.
    }
}
