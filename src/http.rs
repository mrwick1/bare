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

use crate::url::Url;
// JS: like `import { Url } from "./url"`. `crate` means the root of our program.
use std::io::{Read, Write};
// JS: `import { Read, Write } from "std/io"`.
// `Read` and `Write` are traits. A trait is like a TypeScript interface: a set of methods.
// In Rust you must import a trait before you can call its methods.
use std::net::TcpStream;
// JS: like `net.Socket` in Node. It is a network connection we can read and write.

/// Fetch a URL and return the response body as text.
pub fn get(url: &Url) -> Result<String, Box<dyn std::error::Error>> {
    // JS: like `export function get(url)`. `&Url` means we borrow it. The caller keeps it.
    // It returns the page text, or an error.
    // `Box<dyn Error>` means "any kind of error". JS has the same idea: you can `throw` anything.
    // We need it because this function can fail in two ways: our own text errors and
    // network errors from the standard library. `?` converts both into this one type.

    let mut stream = TcpStream::connect((url.host.as_str(), url.port))?;
    // JS: like `net.connect(port, host)`. It also looks up the host name (DNS).
    // `(a, b)` is a pair of values. `.as_str()` lets us pass the `String` as read-only text.
    // `?` returns the error to the caller if the connection fails.
    // `mut`: in Rust, `let` is like JS `const`. `let mut` is like JS `let`. We need it
    // because reading and writing change the socket's state.

    let request = format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nUser-Agent: bare/0.1\r\n\r\n",
        url.path, url.host
    );
    // JS: a template string. Each `{}` is replaced by the next value. Like `${url.path}`.
    // `\r\n` is the line ending that HTTP requires.
    // `Host:` says which site we want. One server can host many sites.
    // `Connection: close` asks the server to close the socket when done.
    // That lets us read until the end of the stream.
    // The empty line at the end (`\r\n\r\n`) tells the server that the request is complete.

    stream.write_all(request.as_bytes())?;
    // JS: like `socket.write(request)`. Sockets send bytes, not text. `as_bytes` gives the bytes.
    // `write_all` keeps writing until all bytes are sent, or there is an error.

    let mut raw = Vec::new();
    // JS: like `const raw = []`. A `Vec` is a growable list. This one holds bytes.
    // `mut` because we add data to it.
    stream.read_to_end(&mut raw)?;
    // JS: like collecting every `data` event until the socket closes.
    // `&mut raw` means "lend it, and you may change it". Only one `&mut` can exist at a time.

    let text = String::from_utf8_lossy(&raw);
    // JS: like `new TextDecoder().decode(bytes)`. Bad bytes become "�" instead of failing.

    let (head, body) = split_response(&text)?;
    // Cut the reply into headers and body. This is our own function, below.
    // JS: like `const [head, body] = ...`. `?` returns the error if the reply is malformed.

    let status_line = head.lines().next().unwrap_or("");
    // `head.lines()` goes through the headers line by line.
    // `.next()` takes the first line, like "HTTP/1.1 200 OK". It may not exist, so it is an `Option`.
    // `unwrap_or("")` means: use the line, or "" if missing. JS: `line ?? ""`.
    if !status_line.contains(" 200 ") {
        // JS: `!status_line.includes(" 200 ")`. 200 means success.
        // For now, any other code (301, 404...) is an error.
        return Err(format!("unsupported status: {status_line}").into());
        // `.into()` converts our text into the `Box<dyn Error>` type that this function returns.
    }

    Ok(body.to_string())
    // Return the body as success. `.to_string()` copies it.
    // We must copy, because `body` points into `text`, and `text` is dropped when the function ends.
    // JS does this for you with garbage collection. Rust does not have one.
}

/// Split a raw response into headers and body at the first blank line.
fn split_response(text: &str) -> Result<(&str, &str), String> {
    // No `pub`, so only this file can use it. JS: a function that is not exported.
    // The two `&str` we return are pieces of `text`. They are not copies.
    // JS: like `slice`, but Rust checks that `text` stays alive while you use the pieces.
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
    fn rejects_garbage() {
        assert!(split_response("not http").is_err());
        // There is no blank line, so this must be an error.
    }
}
