//! Module 1b: a minimal HTTP/1.1 client.
//!
//! HTTP is text over a TCP connection. We send a request, then read the reply.
//!
//! Request:                    Response:
//!   GET /path HTTP/1.1          HTTP/1.1 200 OK
//!   Host: example.com           Content-Type: text/html
//!   (blank line)                (blank line)
//!                               <html>...</html>
//!
//! A new TCP connection is slow: it needs a round trip before we can send anything.
//! So the `Client` keeps the connection open after a reply, and sends the next request
//! to the same server over it. This is called "keep-alive". HTTP/1.1 does it by default.
//!
//! Comments compare Rust with JavaScript, because you know JS.

use crate::{dns, tcp};
// JS: like `import * as dns from "./dns"`: other files in this crate.
use bare_url::Url;
// JS: like `import { Url } from "bare-url"`: from another package.
use std::collections::HashMap;
// JS: like a `Map`.
use std::io::{ErrorKind, Read, Write};
// These give us the `read` and `write` functions on a connection.
// Rust needs this import before you can call them.
use std::net::{SocketAddr, TcpStream};
// `SocketAddr` is an IP address plus a port. `TcpStream` is an open connection.
use std::time::{Duration, Instant};
// `Instant` is a point in time. We use it to know how long a connection has been idle.

const MAX_IDLE: Duration = Duration::from_secs(15);
// We do not reuse a connection that has been idle for more than 15 seconds.
// Servers close idle connections too: Apache after 5 seconds, nginx after 75.
// A connection that the server closed would fail on our next request.

type Error = Box<dyn std::error::Error>;
// JS: a short name for a long type. `Box<dyn Error>` means "any kind of error".
// We need it because our functions can fail in two ways: our own error text, and
// network errors. Both fit into this one type.

/// An open connection that waits for a next request, and the time it became idle.
type IdleConnection = (TcpStream, Instant);

/// Fetches pages. It keeps idle connections so that the next request can reuse one.
#[derive(Default)]
// Gives `Client::default()`: a client with an empty list. JS: the starting values of a class.
pub struct Client {
    // JS: like a class with a private field.
    idle: HashMap<(String, u16), IdleConnection>,
    // The open connections that wait for a next request.
    // The key is the server: its host and port. A connection only works for the server
    // it was opened to. The value is the connection and the time it became idle.
    // Only one connection per server. When a browser needs more at once, it opens more.
    // Chrome allows 6 for each server.
}

impl Client {
    pub fn new() -> Client {
        Client::default()
    }

    /// Fetch a URL and return the response body as text.
    pub fn get(&mut self, url: &Url) -> Result<String, Error> {
        // `&mut self` means this method may change the client: it adds and removes connections.
        // `&Url` means we borrow the URL. The caller keeps it.
        let response = self.fetch(url)?;
        // Send the request and read the reply. `?` returns the error to the caller.

        if response.status != 200 {
            // 200 means success. For now, any other code (301, 404...) is an error.
            return Err(format!("unsupported status: {}", response.status).into());
            // `.into()` converts our text into the `Error` type that this function returns.
        }

        Ok(String::from_utf8_lossy(&response.body).into_owned())
        // Turn the body bytes into text. Bad bytes become "�" instead of failing.
        // `into_owned` makes a copy that we can return.
    }

    /// Send a GET request and read the reply. Reuse an idle connection when there is one.
    pub fn fetch(&mut self, url: &Url) -> Result<Response, Error> {
        let server = (url.host.clone(), url.port);
        let (mut stream, reused) = match self.idle.remove(&server) {
            // `remove` takes the connection out of the list. While we use it, nobody else can.
            Some((stream, since)) if since.elapsed() < MAX_IDLE => (stream, true),
            // There is a connection, and it has not been idle for too long. Use it.
            _ => (open(url)?, false),
            // `_` means "anything else": no connection, or an old one. Open a new one.
            // An old connection is dropped here, and Rust closes it for us.
        };

        let mut result = exchange(&mut stream, url);
        if result.is_err() && reused {
            // A reused connection can fail when the server closed it while it was idle.
            // We cannot know that before we try. The error is not the page's fault, so we
            // try once more on a new connection. This is safe because GET only reads.
            // We must never do this for a request that changes something, like POST.
            stream = open(url)?;
            result = exchange(&mut stream, url);
        }
        let (response, reusable) = result?;

        if reusable {
            self.idle.insert(server, (stream, Instant::now()));
            // Keep the connection for the next request.
        }
        // If not, `stream` is dropped at the end of this function, and Rust closes it.
        Ok(response)
    }
}

/// Open a new connection to the server of this URL.
fn open(url: &Url) -> Result<TcpStream, Error> {
    let addrs: Vec<SocketAddr> = dns::resolve(&url.host)?.into_iter().map(|ip| SocketAddr::new(ip, url.port)).collect();
    // Our own DNS lookup (see `dns.rs`) gives IP addresses. Add the port to each one.
    // JS: `ips.map(ip => ({ ip, port }))`.
    Ok(tcp::connect(&addrs)?)
    // Open a connection, with time limits (see `tcp.rs`).
}

/// Send one request and read one reply. Also say if the connection can be used again.
fn exchange(stream: &mut TcpStream, url: &Url) -> Result<(Response, bool), Error> {
    // `&mut TcpStream` means we borrow the connection and may change it. The caller keeps it.
    let request = format!("GET {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: bare/0.1\r\n\r\n", url.path, host_header(url));
    // JS: a template string. Each `{}` is replaced by the next value. Like `${url.path}`.
    // `\r\n` is the line ending that HTTP requires.
    // `Host:` says which site we want. One server can host many sites.
    // There is no `Connection:` header. For HTTP/1.1 "keep the connection" is the default.
    // The empty line at the end (`\r\n\r\n`) tells the server that the request is complete.

    stream.write_all(request.as_bytes())?;
    // Send the request. A connection sends bytes, not text. `as_bytes` gives the bytes.
    // `write_all` keeps writing until all bytes are sent, or there is an error.

    let mut raw = Vec::new();
    // JS: like `const raw = []`. A `Vec` is a growable list. This one holds the bytes we got so far.
    let mut piece = [0u8; 8192];
    // A box for 8192 bytes. Each read fills some of it.
    let mut closed = false;
    // Becomes true when the server closes the connection.

    loop {
        if let Some((response, used)) = parse_response(&raw, closed)? {
            // Is the reply complete? If yes, we get the response and how many bytes it used.
            // If not, we get `None` and read more bytes below.
            let reusable = !closed && used == raw.len() && !response.close;
            // We can use this connection again only if all of these are true:
            // - The server did not close it.
            // - There are no bytes left after the reply. Extra bytes would be the start of
            //   a reply we never asked for, and we could not trust the next reply.
            // - The server did not say `Connection: close`.
            return Ok((response, reusable));
        }

        let n = stream.read(&mut piece).map_err(|e| match e.kind() {
            ErrorKind::WouldBlock | ErrorKind::TimedOut => std::io::Error::new(e.kind(), "the server stopped answering"),
            _ => e,
        })?;
        // Read what the server sends. `n` is the number of bytes we got.
        // If the server sends nothing for 30 seconds, the read stops with a "would block" error.
        // We replace that confusing name with a clear message.
        // `&mut piece` means "lend it, and you may change it".
        if n == 0 {
            closed = true;
            // 0 bytes means the server closed the connection. The loop parses one more time.
        } else {
            raw.extend_from_slice(&piece[..n]);
            // Add the new bytes to the end of the list. `piece[..n]` is JS `piece.slice(0, n)`.
        }
    }
}

/// A parsed HTTP response.
pub struct Response {
    // JS: like an object `{ status, headers, body }`. `pub` lets other crates read the fields.
    pub status: u16,
    // The number in the first line, like 200 or 404.
    pub headers: Vec<(String, String)>,
    // JS: like `[["content-type", "text/html"], ...]`. Names are lowercase.
    // A list, not an object, because a server may send the same name twice.
    pub body: Vec<u8>,
    // The body as bytes. An image is not text, so we only make text at the end.
    pub close: bool,
    // True when the server will close the connection after this reply.
}

impl Response {
    // JS: like adding methods to a class.
    /// The first header with this name. Names are not case sensitive.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
        // JS: `headers.find(([n]) => n === name)?.[1]`. `Option` is like a value that may be `undefined`.
    }
}

/// Try to read one reply from the bytes we have. `closed` says that no more bytes will come.
/// - `Ok(Some((response, used)))`: the reply is complete. It used `used` bytes.
/// - `Ok(None)`: the reply is not complete yet. Read more bytes.
/// - `Err(..)`: the reply is broken, or the connection closed in the middle of it.
fn parse_response(raw: &[u8], closed: bool) -> Result<Option<(Response, usize)>, String> {
    // `Option` inside `Result` gives three outcomes: good, "not yet", and bad.
    let Some(head_end) = find(raw, b"\r\n\r\n") else {
        // The blank line ends the headers. Everything after it is the body.
        return if closed { Err("malformed response: no blank line after headers".to_string()) } else { Ok(None) };
        // JS: `if (headEnd === undefined) { ... return }`. `let ... else` runs the block when the pattern does not fit.
        // No blank line yet: wait for more bytes. If the server closed, there will be none.
    };
    let head = String::from_utf8_lossy(&raw[..head_end]);
    // Headers are text, so we make text from them. `raw[..head_end]` is JS `raw.slice(0, headEnd)`.
    let mut lines = head.split("\r\n");
    // JS: `head.split("\r\n")`. We read it one line at a time.

    let status_line = lines.next().unwrap_or("");
    // The first line, like "HTTP/1.1 200 OK".
    let mut parts = status_line.splitn(3, ' ');
    // Split at the first two spaces only. The reason text ("Not Found") has a space inside.
    let version = parts.next().unwrap_or("");
    if !version.starts_with("HTTP/1.") {
        return Err(format!("malformed status line: {status_line}"));
    }
    let status = parts.next().and_then(|code| code.parse::<u16>().ok()).ok_or_else(|| format!("malformed status line: {status_line}"))?;
    // `parse` turns "200" into the number 200. It can fail, so we get a `Result`.
    // `.ok()` turns that into an `Option`, and `and_then` runs it only if the code exists.

    let mut headers = Vec::new();
    for line in lines {
        let (name, value) = line.split_once(':').ok_or_else(|| format!("malformed header: {line}"))?;
        // "Content-Length: 5" becomes "Content-Length" and " 5".
        headers.push((name.to_ascii_lowercase(), value.trim().to_string()));
        // Names are not case sensitive, so we keep them lowercase. Spaces around the value are not part of it.
    }

    let connection = headers.iter().filter(|(name, _)| name == "connection").map(|(_, v)| v.to_ascii_lowercase()).collect::<Vec<_>>().join(",");
    // All `Connection` headers joined: "close", or "keep-alive", or both, or nothing.
    let close = connection.contains("close") || (version == "HTTP/1.0" && !connection.contains("keep-alive"));
    // HTTP/1.1 keeps the connection unless the server says "close".
    // HTTP/1.0 is the other way around: it closes unless the server says "keep-alive".

    let mut response = Response { status, headers, body: Vec::new(), close };
    let rest = &raw[head_end + 4..];
    // The bytes after the blank line (4 bytes: `\r\n\r\n`). They may hold the body, but maybe not all of it.
    let Some((body, used)) = read_body(&response, rest, closed)? else {
        return Ok(None);
        // The body is not complete yet.
    };
    response.body = body;
    Ok(Some((response, head_end + 4 + used)))
    // The reply used the head, the blank line and `used` bytes of the body.
}

/// Find where the body ends. HTTP has three rules. The first one that fits wins.
/// Gives the body and the number of bytes it used, or `None` when more bytes must come.
fn read_body(response: &Response, rest: &[u8], closed: bool) -> Result<Option<(Vec<u8>, usize)>, String> {
    if response.status < 200 || response.status == 204 || response.status == 304 {
        return Ok(Some((Vec::new(), 0)));
        // Rule 0: these codes never have a body, whatever the headers say.
    }

    if let Some(coding) = response.header("transfer-encoding") {
        // Rule 1: the body comes in chunks. Each chunk says its own size.
        if coding.to_ascii_lowercase().split(',').next_back().map(str::trim) != Some("chunked") {
            return Err(format!("unsupported transfer-encoding: {coding}"));
        }
        return decode_chunked(rest, closed);
        // This rule comes before Content-Length. If both are present, a proxy and a server
        // may read them differently. That is "request smuggling". The standard says chunked wins.
    }

    if let Some(length) = response.header("content-length") {
        // Rule 2: the server says the exact size of the body.
        let n = parse_number(length, 10).ok_or_else(|| format!("bad content-length: {length}"))?;
        if response.headers.iter().any(|(name, value)| name == "content-length" && value != length) {
            return Err("two different content-length headers".to_string());
            // Two different sizes: we cannot know which one is true. We refuse.
        }
        if rest.len() < n {
            if closed {
                return Err(format!("the connection closed early: got {} of {n} bytes", rest.len()));
                // The server closed the connection before the end. The page is cut short.
                // Without this check we would show half a page as if it were complete.
            }
            return Ok(None);
            // The rest of the body is still on its way.
        }
        return Ok(Some((rest[..n].to_vec(), n)));
        // Take exactly `n` bytes. Anything after them belongs to the next reply.
    }

    if !closed {
        return Ok(None);
        // We cannot know where the body ends. We must wait for the server to close the connection.
    }
    Ok(Some((rest.to_vec(), rest.len())))
    // Rule 3: nothing says the size, so the body ends when the connection closes.
    // This is the oldest rule. It cannot tell a complete body from a cut one.
    // It also means the connection is gone, so it cannot be reused.
}

/// Join the chunks of a chunked body. A chunk is: size in hex, `\r\n`, data, `\r\n`.
/// A chunk of size 0 ends the body. After it come optional "trailers" and a blank line.
fn decode_chunked(mut rest: &[u8], closed: bool) -> Result<Option<(Vec<u8>, usize)>, String> {
    // `mut rest` lets us move `rest` forward, past each chunk we have read.
    let total = rest.len();
    // We remember the start, so we can say at the end how many bytes we used.
    let more = || if closed { Err("chunked body ended early".to_string()) } else { Ok(None) };
    // What to do when the bytes stop in the middle of the body: wait for more, or give an error.
    // JS: an arrow function that we call as `more()`.
    let mut body = Vec::new();
    loop {
        let Some(line_end) = find(rest, b"\r\n") else { return more() };
        let line = String::from_utf8_lossy(&rest[..line_end]);
        // The size line, like "241" or "1a;name=value".
        let size_text = line.split(';').next().unwrap_or("");
        // After ";" a server may add "extensions". We ignore them.
        let size = parse_number(size_text.trim(), 16).ok_or_else(|| format!("bad chunk size: {line}"))?;
        // The size is in hex: "241" means 577 bytes, not 241. This is why example.com
        // showed "241" and "0" in our output before.
        rest = &rest[line_end + 2..];
        // Move past the size line.

        if size == 0 {
            break;
            // The last chunk.
        }
        if rest.len() < size.saturating_add(2) {
            return more();
            // `saturating_add` stops at the biggest number instead of wrapping around.
            // A server could send a size like "ffffffffffffffff" to make `size + 2` overflow.
        }
        body.extend_from_slice(&rest[..size]);
        // Keep the chunk data.
        if &rest[size..size + 2] != b"\r\n" {
            return Err("chunk is not followed by a line break".to_string());
        }
        rest = &rest[size + 2..];
        // Move past the data and its line break. The loop reads the next chunk.
    }

    loop {
        // Trailers: extra header lines after the last chunk. We read them and ignore them.
        // We must read them: a keep-alive connection is only ready for the next reply
        // when all of this reply is gone.
        let Some(line_end) = find(rest, b"\r\n") else { return more() };
        rest = &rest[line_end + 2..];
        if line_end == 0 {
            return Ok(Some((body, total - rest.len())));
            // An empty line ends the reply.
        }
    }
}

/// Read a number made only of digits. `from_str_radix` also accepts "+5", which HTTP does not.
fn parse_number(text: &str, radix: u32) -> Option<usize> {
    // `radix` is the base: 10 for normal numbers, 16 for hex.
    if text.is_empty() || !text.chars().all(|c| c.is_digit(radix)) {
        return None;
    }
    usize::from_str_radix(text, radix).ok()
}

/// The position of the first `needle` inside `haystack`.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
    // `windows(n)` looks at every piece of n bytes in a row. JS: a loop with `slice(i, i + n)`.
}

/// The value of the `Host:` header, like "a.com" or "[::1]:8080".
fn host_header(url: &Url) -> String {
    let host = if url.host.contains(':') { format!("[{}]", url.host) } else { url.host.clone() };
    // Only IPv6 addresses contain ":". They need their "[ ]" back.
    if url.port == 80 { host } else { format!("{host}:{}", url.port) }
    // The port is sent only when it is not the default.
}

#[cfg(test)]
// Only compiled for `cargo test`.
mod tests {
    use super::*;
    // Import everything from this file, including the private `parse_response`.

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

    fn parse(raw: &str) -> Result<Response, String> {
        parse_response(raw.as_bytes(), true).map(|r| r.unwrap().0)
        // A shortcut: tests write the reply as text. `true` means the connection has closed,
        // so the answer is never "not yet". `unwrap` takes the response out of the `Option`.
    }

    #[test]
    fn reads_status_and_headers() {
        let r = parse("HTTP/1.1 404 Not Found\r\nContent-Type: text/html\r\nContent-Length: 2\r\n\r\nhi").unwrap();
        assert_eq!(r.status, 404);
        // The reason text "Not Found" has a space. It must not break the status.
        assert_eq!(r.header("content-type"), Some("text/html"));
        assert_eq!(r.header("CONTENT-TYPE"), None);
        // `header` expects a lowercase name. Names are stored in lowercase.
        assert_eq!(r.body, b"hi");
    }

    #[test]
    fn body_ends_at_content_length() {
        let r = parse("HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nabcdef").unwrap();
        assert_eq!(r.body, b"abc");
        // Bytes after the size are not part of this body.
    }

    #[test]
    fn short_body_is_an_error() {
        let r = parse("HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nabc");
        assert!(r.is_err());
        // The server promised 10 bytes and sent 3. We must not show it as a full page.
    }

    #[test]
    fn body_ends_when_connection_closes() {
        let r = parse("HTTP/1.1 200 OK\r\n\r\nall of it").unwrap();
        assert_eq!(r.body, b"all of it");
        // No size header: we take everything.
    }

    #[test]
    fn joins_chunks() {
        let r = parse("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n19;ext=1\r\n, and the rest of it here\r\n0\r\n\r\n").unwrap();
        assert_eq!(r.body, b"hello, and the rest of it here");
        // "19" is hex for 25. The ";ext=1" part is ignored.
    }

    #[test]
    fn chunked_wins_over_content_length() {
        let r = parse("HTTP/1.1 200 OK\r\nContent-Length: 1\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nok\r\n0\r\n\r\n").unwrap();
        assert_eq!(r.body, b"ok");
    }

    #[test]
    fn cut_chunked_body_is_an_error() {
        assert!(parse("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhel").is_err());
        // The connection closed in the middle of a chunk.
        assert!(parse("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nokXX0\r\n\r\n").is_err());
        // The chunk is not followed by a line break.
        assert!(parse("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n+5\r\nhello\r\n0\r\n\r\n").is_err());
        // "+5" is not a valid size, even though Rust's number parser accepts it.
    }

    #[test]
    fn some_codes_have_no_body() {
        let r = parse("HTTP/1.1 304 Not Modified\r\nContent-Length: 50\r\n\r\n").unwrap();
        assert!(r.body.is_empty());
        // A 304 has no body, even when it names a size. We must not wait for 50 bytes.
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse("not http").is_err());
        // There is no blank line.
        assert!(parse("FTP/1.0 200 OK\r\n\r\n").is_err());
        assert!(parse("HTTP/1.1 abc OK\r\n\r\n").is_err());
        assert!(parse("HTTP/1.1 200 OK\r\nno colon here\r\n\r\n").is_err());
        assert!(parse("HTTP/1.1 200 OK\r\nContent-Length: 1\r\nContent-Length: 2\r\n\r\nab").is_err());
        // Each of these is broken in its own way. Each must be an error, not a crash.
    }

    #[test]
    fn waits_for_more_bytes() {
        let wait = |raw: &str| parse_response(raw.as_bytes(), false).unwrap().is_none();
        // `false` means the connection is still open, so more bytes may come.
        assert!(wait("HTTP/1.1 200 OK\r\nContent-Le"));
        // The headers are not complete.
        assert!(wait("HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhel"));
        // The body is not complete.
        assert!(wait("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n"));
        // The last chunk has not come.
        assert!(wait("HTTP/1.1 200 OK\r\n\r\nno size, so we wait for the close"));
    }

    #[test]
    fn reports_how_many_bytes_a_reply_used() {
        let raw = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nokHTTP/1.1 200 OK\r\n";
        let (_, used) = parse_response(raw.as_bytes(), false).unwrap().unwrap();
        assert_eq!(&raw[used..], "HTTP/1.1 200 OK\r\n");
        // The bytes after `used` belong to the next reply. We must not lose them or mix them in.
    }

    #[test]
    fn reads_trailers() {
        let raw = "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nok\r\n0\r\nX-Sum: 1\r\n\r\n";
        let (r, used) = parse_response(raw.as_bytes(), false).unwrap().unwrap();
        assert_eq!(r.body, b"ok");
        assert_eq!(used, raw.len());
        // The trailer line is part of this reply, so it is counted as used.
    }

    #[test]
    fn huge_chunk_size_is_an_error_not_a_crash() {
        assert!(parse("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nffffffffffffffff\r\nx").is_err());
        // The size plus 2 would overflow. The reply is cut, so it must be an error.
    }

    #[test]
    fn knows_when_the_server_will_close() {
        assert!(!parse("HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n").unwrap().close);
        assert!(parse("HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap().close);
        assert!(parse("HTTP/1.0 200 OK\r\nContent-Length: 0\r\n\r\n").unwrap().close);
        // HTTP/1.0 closes by default.
        assert!(!parse("HTTP/1.0 200 OK\r\nContent-Length: 0\r\nConnection: Keep-Alive\r\n\r\n").unwrap().close);
    }

    /// Start a fake server on this machine. It answers each connection with its replies, one for
    /// each request. When the flag is `true`, it closes the connection afterwards. If not, it
    /// keeps it open. Gives the port.
    fn serve(connections: Vec<(Vec<&'static str>, bool)>) -> u16 {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        // Port 0 means "any free port".
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let mut held = Vec::new();
            // Connections we keep open, so that Rust does not close them.
            for (replies, close) in connections {
                let (mut stream, _) = listener.accept().unwrap();
                for reply in replies {
                    let mut seen = Vec::new();
                    let mut byte = [0u8; 1];
                    while !seen.ends_with(b"\r\n\r\n") {
                        stream.read_exact(&mut byte).unwrap();
                        seen.push(byte[0]);
                    }
                    // Read one request, up to its blank line.
                    stream.write_all(reply.as_bytes()).unwrap();
                }
                if !close {
                    held.push(stream);
                }
            }
        });
        port
    }

    const OK: &str = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok";

    fn url(port: u16) -> Url {
        Url::parse(&format!("http://127.0.0.1:{port}/")).unwrap()
    }

    #[test]
    fn reuses_the_connection() {
        let port = serve(vec![(vec![OK, OK], false)]);
        // The server accepts only one connection. A second connection would never be answered.
        let mut client = Client::new();
        assert_eq!(client.get(&url(port)).unwrap(), "ok");
        assert_eq!(client.get(&url(port)).unwrap(), "ok");
        // The second request goes over the same connection, and it works.
    }

    #[test]
    fn opens_a_new_connection_after_close() {
        let port = serve(vec![(vec!["HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok"], false), (vec![OK], false)]);
        // The first reply says "close", but this server keeps the connection open.
        // If we tried to reuse it, nobody would answer, and the test would fail.
        let mut client = Client::new();
        assert_eq!(client.get(&url(port)).unwrap(), "ok");
        assert_eq!(client.get(&url(port)).unwrap(), "ok");
    }

    #[test]
    fn retries_when_the_server_closed_an_idle_connection() {
        let port = serve(vec![(vec![OK], true), (vec![OK], false)]);
        // The server closes the first connection right after its reply.
        let mut client = Client::new();
        assert_eq!(client.get(&url(port)).unwrap(), "ok");
        assert_eq!(client.get(&url(port)).unwrap(), "ok");
        // The second request fails on the dead connection. We retry on a new one.
    }
}
