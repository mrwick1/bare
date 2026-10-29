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
//! The `Client` also follows redirects ("this page moved") and keeps a cache of replies.
//!
//! Comments compare Rust with JavaScript, because you know JS.

use crate::{dns, tcp};
// JS: like `import * as dns from "./dns"`: other files in this crate.
use bare_url::Url;
// JS: like `import { Url } from "bare-url"`: from another package.
use std::collections::HashMap;
// JS: like a `Map`.
use std::io::{self, ErrorKind, Read, Write};
// These give us the `read` and `write` functions on a connection.
// Rust needs this import before you can call them.
use std::net::{SocketAddr, TcpStream};
use std::sync::{Arc, Mutex};
// JS: like a shared reference. `Arc` lets two owners hold one value, and a copy of the `Arc` costs nothing.
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

const MAX_HEAD: usize = 256 * 1024;
// The status line and headers of a reply may be 256 KB at most. Chrome allows 256 KB too. Without a limit, a server
// that sends headers for ever fills our memory.

const MAX_REPLY: usize = 128 * 1024 * 1024;
// A reply (head and body) may be 128 MB at most. Without a limit, a server that never stops fills our memory.

const MAX_TIME: Duration = Duration::from_secs(120);
// A reply must be complete after 120 seconds. The read limit of `tcp.rs` is for each read, so a server that sends
// one byte every 10 seconds would keep us for ever.

const MAX_CACHE: usize = 64 * 1024 * 1024;
// The cache holds 64 MB at most. When it is full, we empty it.

const MAX_REDIRECT_BODY: u64 = 64 * 1024;
// We read at most 64 KB of the body of a redirect, so that the connection can be used again. A bigger one is not worth it.

const MAX_REDIRECTS: usize = 20;
// We follow at most 20 redirects. Chrome and Firefox use 20 too. Without a limit,
// two pages that point at each other would send us around forever.

/// An open connection that waits for a next request, and the time it became idle.
type IdleConnection = (TcpStream, Instant);

/// The idle connections of a client, by server. A `Body` that is read to the end gives its connection back to it.
type Pool = Arc<Mutex<HashMap<(String, u16), IdleConnection>>>;

/// A stored reply: when we stored it, and for how long it stays fresh.
struct Cached {
    response: Arc<Response>,
    // Shared with the caller: a cache hit gives out the same reply, and does not copy the body.
    stored: Instant,
    fresh_for: Duration,
    // Until this time has passed, we use the copy without asking the server.
}

/// Fetches pages. It keeps idle connections so that the next request can reuse one.
#[derive(Default)]
// Gives `Client::default()`: a client with an empty list. JS: the starting values of a class.
pub struct Client {
    // JS: like a class with a private field.
    idle: Pool,
    // The open connections that wait for a next request.
    // The key is the server: its host and port. A connection only works for the server
    // it was opened to. The value is the connection and the time it became idle.
    // Only one connection per server. When a browser needs more at once, it opens more.
    // Chrome allows 6 for each server.
    cache: HashMap<(String, u16, String), Cached>,
    // Replies we may use again. The key is the host, the port and the path.
    // It lives in memory only. `MAX_CACHE` is the size limit.
    cache_bytes: usize,
    // How many bytes the cache holds, counted with `cache_size`.
}

/// How many bytes of memory a stored reply takes, about.
fn cache_size(response: &Response) -> usize {
    response.body.len() + 512
    // The 512 is for the headers and the key. It stops a million tiny replies from costing nothing.
}

impl Client {
    pub fn new() -> Client {
        Client::default()
    }

    /// Fetch a URL. Follow redirects, and use the cache. Any status can come back: a 404 is still a page.
    pub fn get(&mut self, url: &Url) -> Result<Arc<Response>, Error> {
        // `&mut self` means this method may change the client: it adds and removes connections.
        // `&Url` means we borrow the URL. The caller keeps it.
        let mut current = url;
        // The URL we ask for now. It starts as the one we were given.
        let mut next;
        // Where a redirect sends us. We own this `Url`, so it must live outside the loop.
        for _ in 0..=MAX_REDIRECTS {
            // `0..=20` is 21 turns: the first request plus 20 redirects.
            let response = self.load(current)?;
            let Some(location) = redirect_target(&response) else {
                return Ok(response);
                // Not a redirect: this is the page.
            };
            next = current.join(location)?;
            // `Location` may be relative, like "/new". `join` makes it a full URL.
            // A link to `https://` fails here, because we do not support HTTPS yet.
            current = &next;
        }
        Err(format!("too many redirects (more than {MAX_REDIRECTS})").into())
        // `.into()` converts our text into the `Error` type that this function returns.
    }

    /// Remove a stored reply, and take its size off the count.
    fn forget(&mut self, key: &(String, u16, String)) {
        if let Some(old) = self.cache.remove(key) {
            self.cache_bytes -= cache_size(&old.response);
        }
    }

    /// Get one URL from the cache, or from the server. Do not follow redirects.
    fn load(&mut self, url: &Url) -> Result<Arc<Response>, Error> {
        let key = (url.host.clone(), url.port, url.path.clone());
        let mut conditions = String::new();
        // Extra request headers. They stay empty unless we have an old copy to check.

        if let Some(cached) = self.cache.get(&key) {
            if cached.stored.elapsed() < cached.fresh_for {
                return Ok(cached.response.clone());
                // Still fresh: use the copy. No request at all.
            }
            // Stale. The copy may still be right, so we ask the server: "has it changed?"
            if let Some(etag) = cached.response.header("etag") {
                conditions += &format!("If-None-Match: {etag}\r\n");
                // An ETag is a label for one version of the page, like a version number.
            }
            if let Some(date) = cached.response.header("last-modified") {
                conditions += &format!("If-Modified-Since: {date}\r\n");
                // The same question with a date: "changed since then?"
            }
        }

        let response = Arc::new(self.fetch(url, &conditions)?);

        if response.status == 304 {
            // "Not Modified": our copy is still right. The reply has no body.
            if let Some(cached) = self.cache.get_mut(&key) {
                cached.stored = Instant::now();
                cached.fresh_for = freshness(&response).unwrap_or(Duration::ZERO);
                // The copy is fresh again. The 304 may give a new `max-age`.
                return Ok(cached.response.clone());
            }
        }

        match freshness(&response) {
            Some(fresh_for)
                if response.status == 200
                    && response.header("vary").is_none()
                    && (!fresh_for.is_zero() || response.header("etag").is_some() || response.header("last-modified").is_some()) =>
            {
                // We keep a reply only if all of these are true:
                // - It is a 200 and the server did not say `no-store`.
                // - It has no `Vary` header. `Vary` says the reply depends on a request header.
                //   We do not handle that yet.
                // - It is useful: it is fresh for a while, or we can ask the server about it later.
                self.forget(&key);
                if self.cache_bytes + cache_size(&response) > MAX_CACHE {
                    self.cache.clear();
                    self.cache_bytes = 0;
                }
                if cache_size(&response) <= MAX_CACHE {
                    self.cache_bytes += cache_size(&response);
                    self.cache.insert(key, Cached { response: response.clone(), stored: Instant::now(), fresh_for });
                }
            }
            _ => {
                self.forget(&key);
                // Do not keep this reply. Throw away any older copy too: it is out of date.
            }
        }
        Ok(response)
    }

    /// Send a GET request and read the whole reply. Reuse an idle connection when there is one.
    fn fetch(&mut self, url: &Url, conditions: &str) -> Result<Response, Error> {
        let (mut response, mut body) = self.start(url, conditions)?;
        let mut bytes = Vec::with_capacity(body.size_hint());
        // A reply with a `Content-Length` gets its room at once: a list that grows by doubling holds up to twice the body for a moment.
        body.read_to_end(&mut bytes)?;
        if bytes.capacity() - bytes.len() > bytes.len() / 8 {
            bytes.shrink_to_fit();
            // Give the spare room back. For a big list, the system does this without a copy.
        }
        response.body = bytes;
        Ok(response)
    }

    /// Send a GET request and read the status and the headers. The body is not read: the `Body` reads it from the
    /// connection when the caller asks. Reuse an idle connection when there is one.
    fn start(&mut self, url: &Url, conditions: &str) -> Result<(Response, Body), Error> {
        let server = (url.host.clone(), url.port);
        let idle = self.idle.lock().unwrap().remove(&server);
        let (mut stream, reused) = match idle {
            // `remove` takes the connection out of the list. While we use it, nobody else can.
            Some((stream, since)) if since.elapsed() < MAX_IDLE => (stream, true),
            // There is a connection, and it has not been idle for too long. Use it.
            _ => (open(url)?, false),
            // `_` means "anything else": no connection, or an old one. Open a new one.
            // An old connection is dropped here, and Rust closes it for us.
        };

        let mut result = send(&mut stream, url, conditions);
        if result.is_err() && reused {
            // A reused connection can fail when the server closed it while it was idle.
            // We cannot know that before we try. The error is not the page's fault, so we
            // try once more on a new connection. This is safe because GET only reads.
            // We must never do this for a request that changes something, like POST.
            stream = open(url)?;
            result = send(&mut stream, url, conditions);
        }
        let (head, leftover) = result?;
        if let Framing::Length(n) = head.framing
            && n > MAX_REPLY
        {
            return Err("the reply is too big".into());
        }
        let reusable = !head.response.close && head.framing != Framing::UntilClose;
        let live = Live::new(Stream::Tcp(stream), leftover, head.framing, reusable.then(|| (self.idle.clone(), server)));
        Ok((head.response, Body(Source::Live(Box::new(live)))))
    }

    /// Fetch a URL, and give the body as a `Body` that reads from the connection while the caller reads. The caller can
    /// start to use the first bytes of a page before the last ones come, and the page is never all in memory.
    /// Redirects are followed. A fresh copy in the cache is used. A reply that comes from the server is not put in the cache.
    /// The `response` has the status and the headers. Its `body` is empty: read it from `body`.
    pub fn get_stream(&mut self, url: &Url) -> Result<Streamed, Error> {
        let mut current = url;
        let mut next;
        for _ in 0..=MAX_REDIRECTS {
            let key = (current.host.clone(), current.port, current.path.clone());
            let fresh = self.cache.get(&key).filter(|c| c.stored.elapsed() < c.fresh_for).map(|c| c.response.clone());
            let (response, mut body) = match fresh {
                Some(response) => {
                    let body = Body(Source::Memory(response.clone(), 0));
                    (response, body)
                }
                None => {
                    let (response, body) = self.start(current, "")?;
                    (Arc::new(response), body)
                }
            };
            let Some(location) = redirect_target(&response) else {
                return Ok(Streamed { response, body });
            };
            next = current.join(location)?;
            current = &next;
            // The body of a redirect is not wanted. Read it (a redirect has a small one) so that the connection can be used again.
            io::copy(&mut (&mut body).take(MAX_REDIRECT_BODY), &mut io::sink())?;
        }
        Err(format!("too many redirects (more than {MAX_REDIRECTS})").into())
    }
}

/// A reply with a body that is still on the connection.
pub struct Streamed {
    pub response: Arc<Response>,
    pub body: Body,
}

/// Open a new connection to the server of this URL.
fn open(url: &Url) -> Result<TcpStream, Error> {
    let addrs: Vec<SocketAddr> = dns::resolve(&url.host)?.into_iter().map(|ip| SocketAddr::new(ip, url.port)).collect();
    // Our own DNS lookup (see `dns.rs`) gives IP addresses. Add the port to each one.
    // JS: `ips.map(ip => ({ ip, port }))`.
    Ok(tcp::connect(&addrs)?)
    // Open a connection, with time limits (see `tcp.rs`).
}

/// Send one request, and read the status line and the headers of the reply. Gives them, and the bytes of the body that came with them.
fn send(stream: &mut TcpStream, url: &Url, conditions: &str) -> Result<(Head, Vec<u8>), Error> {
    // `&mut TcpStream` means we borrow the connection and may change it. The caller keeps it.
    let request = format!("GET {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: bare/0.1\r\n{conditions}\r\n", url.path, host_header(url));
    // JS: a template string. Each `{}` is replaced by the next value. Like `${url.path}`.
    // `\r\n` is the line ending that HTTP requires.
    // `Host:` says which site we want. One server can host many sites.
    // There is no `Connection:` header. For HTTP/1.1 "keep the connection" is the default.
    // `conditions` are extra headers, or nothing. Each one ends with `\r\n`.
    // The empty line at the end (`\r\n\r\n`) tells the server that the request is complete.

    stream.write_all(request.as_bytes())?;
    // Send the request. A connection sends bytes, not text. `as_bytes` gives the bytes.
    // `write_all` keeps writing until all bytes are sent, or there is an error.
    read_head(stream)
}

/// Read until the blank line after the headers, and parse what came before it.
fn read_head(stream: &mut impl Read) -> Result<(Head, Vec<u8>), Error> {
    let mut raw = Vec::new();
    // JS: like `const raw = []`. A `Vec` is a growable list. This one holds the bytes we got so far.
    let mut piece = [0u8; 8192];
    // A box for 8192 bytes. Each read fills some of it.
    let start = Instant::now();
    loop {
        if let Some(head) = parse_head(&raw)? {
            raw.drain(..head.head_len);
            // What is left are the first bytes of the body. They came in the same read as the end of the headers.
            return Ok((head, raw));
        }
        if raw.len() > MAX_HEAD {
            return Err("the headers of the reply are too long".into());
        }
        if start.elapsed() > MAX_TIME {
            return Err("the server is too slow".into());
        }
        let n = stream.read(&mut piece).map_err(slow)?;
        // Read what the server sends. `n` is the number of bytes we got.
        // `&mut piece` means "lend it, and you may change it".
        if n == 0 {
            return Err("malformed response: no blank line after headers".into());
            // 0 bytes means the server closed the connection, and there will be no more.
        }
        raw.extend_from_slice(&piece[..n]);
        // Add the new bytes to the end of the list. `piece[..n]` is JS `piece.slice(0, n)`.
    }
}

/// If the server sends nothing for 30 seconds, a read stops with a "would block" error.
/// We replace that confusing name with a clear message.
fn slow(e: io::Error) -> io::Error {
    match e.kind() {
        ErrorKind::WouldBlock | ErrorKind::TimedOut => io::Error::new(e.kind(), "the server stopped answering"),
        _ => e,
    }
}

/// A parsed HTTP response.
#[derive(Clone)]
// `Clone` lets us make a copy with `.clone()`. The cache keeps one copy and gives out another.
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

/// Where a redirect sends us, or `None` when the reply is not a redirect.
fn redirect_target(response: &Response) -> Option<&str> {
    match response.status {
        // 301, 308: moved for good. 302, 307: moved for now. 303: "now use GET".
        // We only send GET, so we treat all five the same.
        301 | 302 | 303 | 307 | 308 => response.header("location"),
        // A redirect without `Location` has nowhere to go. We show it as a normal page.
        _ => None,
    }
}

/// For how long a reply stays fresh. `None` means "never keep it" (`no-store`).
fn freshness(response: &Response) -> Option<Duration> {
    let control = response.header("cache-control").unwrap_or("").to_ascii_lowercase();
    // Like "max-age=60, public". The words are not case sensitive.
    let mut fresh_for = Duration::ZERO;
    let mut no_cache = false;
    for directive in control.split(',').map(str::trim) {
        if directive == "no-store" {
            return None;
            // The page is private, like a bank page. Never keep it.
        }
        if directive.starts_with("no-cache") {
            no_cache = true;
            // "no-cache" does not mean "do not keep it". It means "ask the server before every use".
        }
        if let Some(seconds) = directive.strip_prefix("max-age=").and_then(|n| parse_number(n.trim_matches('"'), 10)) {
            fresh_for = Duration::from_secs(seconds as u64);
            // "Use this for N seconds without asking."
        }
    }
    if no_cache {
        return Some(Duration::ZERO);
    }
    let age = response.header("age").and_then(|n| parse_number(n, 10)).unwrap_or(0);
    // `Age` is how many seconds the reply already sat in a proxy. That time counts.
    Some(fresh_for.saturating_sub(Duration::from_secs(age as u64)))
    // `saturating_sub` stops at 0 instead of going below it.
}

/// How a reply says where its body ends. HTTP has three rules, and the first one that fits wins.
#[derive(Debug, PartialEq)]
enum Framing {
    /// These codes never have a body, whatever the headers say.
    None,
    /// The body comes in chunks. Each chunk says its own size.
    Chunked,
    /// The server says the exact size of the body.
    Length(usize),
    /// Nothing says the size, so the body ends when the connection closes.
    UntilClose,
}

/// The status and headers of a reply, and the way to find the end of its body.
struct Head {
    /// The reply, with an empty body.
    response: Response,
    /// The bytes of the status line, the headers and the blank line.
    head_len: usize,
    framing: Framing,
}

/// Read the status line and the headers. `None` when the blank line has not come yet.
fn parse_head(raw: &[u8]) -> Result<Option<Head>, String> {
    // `Option` inside `Result` gives three outcomes: good, "not yet", and bad.
    let Some(head_end) = find(raw, b"\r\n\r\n") else {
        // The blank line ends the headers. Everything after it is the body.
        return Ok(None);
        // JS: `if (headEnd === undefined) { ... return }`. `let ... else` runs the block when the pattern does not fit.
        // No blank line yet: wait for more bytes.
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

    let response = Response { status, headers, body: Vec::new(), close };
    let framing = framing(&response)?;
    Ok(Some(Head { response, head_len: head_end + 4, framing }))
    // The blank line is 4 bytes: `\r\n\r\n`.
}

impl Default for Response {
    fn default() -> Response {
        Response { status: 0, headers: Vec::new(), body: Vec::new(), close: false }
    }
}

/// Choose the rule that finds the end of the body.
fn framing(response: &Response) -> Result<Framing, String> {
    if response.status < 200 || response.status == 204 || response.status == 304 {
        return Ok(Framing::None);
        // Rule 0: these codes never have a body, whatever the headers say.
    }

    if let Some(coding) = response.header("transfer-encoding") {
        // Rule 1: the body comes in chunks.
        if coding.to_ascii_lowercase().split(',').next_back().map(str::trim) != Some("chunked") {
            return Err(format!("unsupported transfer-encoding: {coding}"));
        }
        return Ok(Framing::Chunked);
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
        return Ok(Framing::Length(n));
    }

    Ok(Framing::UntilClose)
    // Rule 3: this is the oldest rule. It cannot tell a complete body from a cut one.
    // It also means the connection is gone, so it cannot be reused.
}

/// The body of a reply. It reads from the connection while the caller reads, so the body is never all in memory.
/// `Read` is the trait of everything that gives bytes: a file, a socket. JS: like a `ReadableStream`.
pub struct Body(Source);

enum Source {
    /// A reply from the cache: the bytes are in memory. The number is how far we have read.
    Memory(Arc<Response>, usize),
    /// A reply that comes from a server now.
    Live(Box<Live>),
}

/// Where the chunks of a chunked body are.
#[derive(Clone, Copy, PartialEq)]
enum Chunk {
    /// Reading the line with the size of the next chunk.
    Size,
    /// Reading the data of a chunk.
    Data,
    /// Reading the line break after the data. The number says how many of its 2 bytes we have read.
    End(u8),
    /// Reading the lines after the last chunk, up to the blank line.
    Trailers,
}

/// The connection, and what we need to read the body from it.
struct Live {
    stream: Stream,
    /// Bytes that we read from the connection and did not give out yet: `buf[at..]`.
    buf: Vec<u8>,
    at: usize,
    framing: Framing,
    /// `Length`: the bytes that are still to come. `Chunked`: the bytes that are left in this chunk.
    left: usize,
    chunk: Chunk,
    /// The line that we read now (a chunk size, or a trailer).
    line: Vec<u8>,
    given: usize,
    start: Instant,
    /// Where to put the connection when the body ends well, if it may be used again.
    pool: Option<(Pool, (String, u16))>,
    done: bool,
}

/// A connection. In tests it can also be some bytes in memory: a reply that stops, or a reply that ends.
enum Stream {
    Tcp(TcpStream),
    /// The connection went back to the pool, or was dropped. It gives no bytes.
    Closed,
    #[cfg(test)]
    Fake(io::Cursor<Vec<u8>>, bool),
}

impl Read for Stream {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        match self {
            Stream::Tcp(stream) => stream.read(out),
            Stream::Closed => Ok(0),
            #[cfg(test)]
            Stream::Fake(bytes, closed) => match bytes.read(out)? {
                0 if !*closed && !out.is_empty() => Err(ErrorKind::WouldBlock.into()),
                n => Ok(n),
            },
        }
    }
}

impl Live {
    fn new(stream: Stream, leftover: Vec<u8>, framing: Framing, pool: Option<(Pool, (String, u16))>) -> Live {
        let left = if let Framing::Length(n) = framing { n } else { 0 };
        Live { stream, buf: leftover, at: 0, framing, left, chunk: Chunk::Size, line: Vec::new(), given: 0, start: Instant::now(), pool, done: false }
    }

    /// Make sure that there are bytes in `buf[at..]`. When there are none after this, the connection has closed.
    fn fill(&mut self) -> io::Result<()> {
        if self.at < self.buf.len() {
            return Ok(());
        }
        self.buf.resize(8192, 0);
        let n = self.stream.read(&mut self.buf).map_err(slow)?;
        self.buf.truncate(n);
        self.at = 0;
        Ok(())
    }

    /// Give out bytes from `buf`, and no more than `max`.
    fn from_buf(&mut self, out: &mut [u8], max: usize) -> usize {
        let n = out.len().min(max).min(self.buf.len() - self.at);
        out[..n].copy_from_slice(&self.buf[self.at..self.at + n]);
        self.at += n;
        n
    }

    /// The body is complete. Give the connection back, if nothing is left on it and the server will keep it.
    fn finish(&mut self) {
        self.done = true;
        if self.at == self.buf.len()
            && let Some((pool, server)) = self.pool.take()
            && let Stream::Tcp(stream) = std::mem::replace(&mut self.stream, Stream::Closed)
        {
            pool.lock().unwrap().insert(server, (stream, Instant::now()));
        }
    }

    /// Read bytes up to the end of the line (`\r\n`). Gives the line without it.
    fn read_line(&mut self) -> io::Result<String> {
        loop {
            self.fill()?;
            if self.at == self.buf.len() {
                return Err(io::Error::new(ErrorKind::UnexpectedEof, "chunked body ended early"));
            }
            self.line.push(self.buf[self.at]);
            self.at += 1;
            if self.line.ends_with(b"\r\n") {
                let line = String::from_utf8_lossy(&self.line[..self.line.len() - 2]).into_owned();
                self.line.clear();
                return Ok(line);
            }
            if self.line.len() > MAX_HEAD {
                return Err(io::Error::new(ErrorKind::InvalidData, "a line of the chunked body is too long"));
            }
        }
    }

    /// Read chunks into `out`. One call fills `out` from as many chunks as the bytes that we have allow: a body of
    /// 1-byte chunks does not cost one call for each byte. We wait for the connection only when we have nothing to give.
    fn chunked(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let bad = |why: &str| io::Error::new(ErrorKind::InvalidData, why.to_string());
        let ended = || io::Error::new(ErrorKind::UnexpectedEof, "chunked body ended early");
        let mut written = 0;
        loop {
            if written == out.len() || (written > 0 && self.at == self.buf.len()) {
                return Ok(written);
            }
            match self.chunk {
                Chunk::Size => {
                    let line = self.read_line()?;
                    // The size line, like "241" or "1a;name=value". After ";" a server may add "extensions". We ignore them.
                    let size = parse_number(line.split(';').next().unwrap_or("").trim(), 16).ok_or_else(|| bad(&format!("bad chunk size: {line}")))?;
                    // The size is in hex: "241" means 577 bytes, not 241.
                    self.left = size;
                    self.chunk = if size == 0 { Chunk::Trailers } else { Chunk::Data };
                }
                Chunk::Data => {
                    self.fill()?;
                    if self.at == self.buf.len() {
                        return Err(ended());
                    }
                    let n = self.from_buf(&mut out[written..], self.left);
                    written += n;
                    self.left -= n;
                    if self.left == 0 {
                        self.chunk = Chunk::End(0);
                    }
                }
                Chunk::End(seen) => {
                    self.fill()?;
                    if self.at == self.buf.len() {
                        return Err(ended());
                    }
                    if self.buf[self.at] != b"\r\n"[seen as usize] {
                        return Err(bad("chunk is not followed by a line break"));
                    }
                    self.at += 1;
                    self.chunk = if seen == 1 { Chunk::Size } else { Chunk::End(1) };
                }
                Chunk::Trailers => {
                    // Extra header lines after the last chunk. We read them and ignore them.
                    // We must read them: a keep-alive connection is only ready for the next reply when all of this reply is gone.
                    if self.read_line()?.is_empty() {
                        self.finish();
                        return Ok(written);
                        // An empty line ends the reply.
                    }
                }
            }
        }
    }
}

impl Read for Body {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let live = match &mut self.0 {
            Source::Memory(response, at) => {
                let n = out.len().min(response.body.len() - *at);
                out[..n].copy_from_slice(&response.body[*at..*at + n]);
                *at += n;
                return Ok(n);
            }
            Source::Live(live) => live,
        };
        if live.done || out.is_empty() {
            return Ok(0);
        }
        if live.given > MAX_REPLY {
            return Err(io::Error::new(ErrorKind::InvalidData, "the reply is too big"));
        }
        if live.start.elapsed() > MAX_TIME {
            return Err(io::Error::new(ErrorKind::TimedOut, "the server is too slow"));
        }
        let n = match live.framing {
            Framing::None => {
                live.finish();
                0
            }
            Framing::Length(_) => {
                if live.left == 0 {
                    live.finish();
                    return Ok(0);
                }
                let n = if live.at < live.buf.len() {
                    live.from_buf(out, live.left)
                } else {
                    // Read from the connection straight into the buffer of the caller: no copy in between.
                    let max = out.len().min(live.left);
                    live.stream.read(&mut out[..max]).map_err(slow)?
                };
                if n == 0 {
                    return Err(io::Error::new(ErrorKind::UnexpectedEof, format!("the connection closed early: {} bytes are missing", live.left)));
                    // The server closed the connection before the end. The page is cut short.
                    // Without this check we would show half a page as if it were complete.
                }
                live.left -= n;
                n
            }
            Framing::UntilClose => {
                let n = if live.at < live.buf.len() { live.from_buf(out, usize::MAX) } else { live.stream.read(out).map_err(slow)? };
                if n == 0 {
                    live.done = true;
                    // The server closed the connection: the body ends here. This is the oldest rule. It cannot tell a
                    // complete body from a cut one. It also means the connection is gone, so it cannot be reused.
                }
                n
            }
            Framing::Chunked => live.chunked(out)?,
        };
        live.given += n;
        Ok(n)
    }
}

impl Body {
    /// How many bytes the body will have, when the server said so (up to the limit). Used to make room at once.
    fn size_hint(&self) -> usize {
        match &self.0 {
            Source::Live(live) => match live.framing {
                Framing::Length(n) => n.min(MAX_REPLY),
                _ => 0,
            },
            Source::Memory(response, _) => response.body.len(),
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
    // Import everything from this file, including the private `Body`.

    /// Parse a whole reply from bytes. `closed` says if the connection has closed after them: if not, a reply that is not
    /// complete gives `None` ("wait for more"). Gives the reply and how many bytes it used.
    fn parse_response(raw: &[u8], closed: bool) -> Result<Option<(Response, usize)>, String> {
        let mut stream = Stream::Fake(io::Cursor::new(raw.to_vec()), closed);
        let (head, leftover) = match read_head(&mut stream) {
            Ok(x) => x,
            Err(e) if e.to_string().contains("stopped answering") => return Ok(None),
            Err(e) => return Err(e.to_string()),
        };
        let mut response = head.response;
        let mut body = Body(Source::Live(Box::new(Live::new(stream, leftover, head.framing, None))));
        let mut bytes = Vec::new();
        match body.read_to_end(&mut bytes) {
            Err(e) if e.to_string().contains("stopped answering") => return Ok(None),
            Err(e) => return Err(e.to_string()),
            Ok(_) => {}
        }
        response.body = bytes;
        let Source::Live(live) = &body.0 else { unreachable!() };
        let Stream::Fake(cursor, _) = &live.stream else { unreachable!() };
        let unread = (live.buf.len() - live.at) + (raw.len() - cursor.position() as usize);
        Ok(Some((response, raw.len() - unread)))
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
    /// keeps it open. Gives the port, and a channel with the head of each request it got.
    fn serve(connections: Vec<(Vec<&'static str>, bool)>) -> (u16, std::sync::mpsc::Receiver<String>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        // Port 0 means "any free port".
        let port = listener.local_addr().unwrap().port();
        let (requests, received) = std::sync::mpsc::channel();
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
                    let _ = requests.send(String::from_utf8_lossy(&seen).into_owned());
                    // A test that does not read the requests has dropped the receiver. Then `send` fails. We do not care.
                    stream.write_all(reply.as_bytes()).unwrap();
                }
                if !close {
                    held.push(stream);
                }
            }
        });
        (port, received)
    }

    const OK: &str = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok";

    fn url(port: u16) -> Url {
        Url::parse(&format!("http://127.0.0.1:{port}/")).unwrap()
    }

    #[test]
    fn reuses_the_connection() {
        let (port, _) = serve(vec![(vec![OK, OK], false)]);
        // The server accepts only one connection. A second connection would never be answered.
        let mut client = Client::new();
        assert_eq!(client.get(&url(port)).unwrap().body, b"ok");
        assert_eq!(client.get(&url(port)).unwrap().body, b"ok");
        // The second request goes over the same connection, and it works.
    }

    #[test]
    fn opens_a_new_connection_after_close() {
        let (port, _) = serve(vec![(vec!["HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok"], false), (vec![OK], false)]);
        // The first reply says "close", but this server keeps the connection open.
        // If we tried to reuse it, nobody would answer, and the test would fail.
        let mut client = Client::new();
        assert_eq!(client.get(&url(port)).unwrap().body, b"ok");
        assert_eq!(client.get(&url(port)).unwrap().body, b"ok");
    }

    #[test]
    fn retries_when_the_server_closed_an_idle_connection() {
        let (port, _) = serve(vec![(vec![OK], true), (vec![OK], false)]);
        // The server closes the first connection right after its reply.
        let mut client = Client::new();
        assert_eq!(client.get(&url(port)).unwrap().body, b"ok");
        assert_eq!(client.get(&url(port)).unwrap().body, b"ok");
        // The second request fails on the dead connection. We retry on a new one.
    }

    fn page(headers: &str, body: &str) -> &'static str {
        Box::leak(format!("HTTP/1.1 200 OK\r\n{headers}Content-Length: {}\r\n\r\n{body}", body.len()).into_boxed_str())
        // Makes a reply for the fake server. `Box::leak` keeps the text for the whole run,
        // because `serve` wants text that never goes away. Fine in a test.
    }

    const MOVED: &str = "HTTP/1.1 301 Moved Permanently\r\nLocation: /next\r\nContent-Length: 0\r\n\r\n";

    #[test]
    fn knows_which_replies_are_redirects() {
        for code in [301, 302, 303, 307, 308] {
            let r = parse(&format!("HTTP/1.1 {code} X\r\nLocation: /x\r\nContent-Length: 0\r\n\r\n")).unwrap();
            assert_eq!(redirect_target(&r), Some("/x"));
        }
        let r = parse("HTTP/1.1 300 X\r\nLocation: /x\r\nContent-Length: 0\r\n\r\n").unwrap();
        assert_eq!(redirect_target(&r), None);
        // 300 is not a redirect we follow.
        let r = parse("HTTP/1.1 301 X\r\nContent-Length: 0\r\n\r\n").unwrap();
        assert_eq!(redirect_target(&r), None);
        // A redirect without a place to go.
    }

    #[test]
    fn follows_a_redirect() {
        let (port, requests) = serve(vec![(vec![MOVED, OK], false)]);
        let mut client = Client::new();
        assert_eq!(client.get(&url(port)).unwrap().body, b"ok");
        let seen: Vec<String> = requests.try_iter().collect();
        assert!(seen[1].starts_with("GET /next HTTP/1.1"));
        // The second request went to the new place. "/next" was relative to the first URL.
    }

    #[test]
    fn stops_a_redirect_loop() {
        let looping = "HTTP/1.1 302 Found\r\nLocation: /\r\nContent-Length: 0\r\n\r\n";
        let (port, _) = serve(vec![(vec![looping; 21], false)]);
        // 21 replies: the first request plus 20 redirects.
        let error = Client::new().get(&url(port)).err().unwrap();
        assert!(error.to_string().contains("too many redirects"));
    }

    #[test]
    fn does_not_follow_a_redirect_to_https() {
        let (port, _) = serve(vec![(vec!["HTTP/1.1 302 Found\r\nLocation: https://a.com/\r\nContent-Length: 0\r\n\r\n"], false)]);
        let error = Client::new().get(&url(port)).err().unwrap();
        assert!(error.to_string().contains("only http://"));
    }

    #[test]
    fn gives_back_error_pages() {
        let (port, _) = serve(vec![(vec!["HTTP/1.1 404 Not Found\r\nContent-Length: 4\r\n\r\ngone"], false)]);
        let response = Client::new().get(&url(port)).unwrap();
        assert_eq!(response.status, 404);
        assert_eq!(response.body, b"gone");
        // A browser shows the body of a 404 page. It is not an error for the user.
    }

    #[test]
    fn counts_freshness() {
        let fresh = |headers: &str| freshness(&parse(&format!("HTTP/1.1 200 OK\r\n{headers}Content-Length: 0\r\n\r\n")).unwrap());
        assert_eq!(fresh("Cache-Control: max-age=60\r\n"), Some(Duration::from_secs(60)));
        assert_eq!(fresh("Cache-Control: public, MAX-AGE=60\r\n"), Some(Duration::from_secs(60)));
        assert_eq!(fresh("Cache-Control: max-age=60\r\nAge: 20\r\n"), Some(Duration::from_secs(40)));
        // The reply already spent 20 seconds in a proxy.
        assert_eq!(fresh("Cache-Control: max-age=60\r\nAge: 100\r\n"), Some(Duration::ZERO));
        // Too old. It stops at 0.
        assert_eq!(fresh("Cache-Control: max-age=60, no-cache\r\n"), Some(Duration::ZERO));
        assert_eq!(fresh("Cache-Control: no-store\r\n"), None);
        assert_eq!(fresh(""), Some(Duration::ZERO));
        // No information: never fresh.
    }

    #[test]
    fn uses_a_fresh_copy_without_asking() {
        let (port, requests) = serve(vec![(vec![page("Cache-Control: max-age=60\r\n", "a")], false)]);
        let mut client = Client::new();
        assert_eq!(client.get(&url(port)).unwrap().body, b"a");
        assert_eq!(client.get(&url(port)).unwrap().body, b"a");
        assert_eq!(requests.try_iter().count(), 1);
        // The server got one request. The server has only one reply, so a second request would hang.
    }

    #[test]
    fn does_not_keep_no_store() {
        let (port, _) = serve(vec![(vec![page("Cache-Control: no-store, max-age=60\r\n", "a"), page("", "b")], false)]);
        let mut client = Client::new();
        assert_eq!(client.get(&url(port)).unwrap().body, b"a");
        assert_eq!(client.get(&url(port)).unwrap().body, b"b");
        // The second answer is new, so the first one was not kept.
    }

    #[test]
    fn asks_if_a_stale_copy_changed() {
        let first = page("ETag: \"x\"\r\nLast-Modified: Mon, 01 Jan 2024 00:00:00 GMT\r\n", "a");
        let (port, requests) = serve(vec![(vec![first, "HTTP/1.1 304 Not Modified\r\nETag: \"x\"\r\n\r\n"], false)]);
        let mut client = Client::new();
        client.get(&url(port)).unwrap();
        let response = client.get(&url(port)).unwrap();
        assert_eq!((response.status, response.body.as_slice()), (200, b"a".as_slice()));
        // The server said "not modified" and sent no body. We still give the old page, with status 200.
        let seen: Vec<String> = requests.try_iter().collect();
        assert!(seen[1].contains("If-None-Match: \"x\"\r\n"));
        assert!(seen[1].contains("If-Modified-Since: Mon, 01 Jan 2024 00:00:00 GMT\r\n"));
        assert!(!seen[0].contains("If-None-Match"));
        // The first request had no old copy, so it asked no question.
    }

    #[test]
    fn replaces_a_changed_copy() {
        let (port, _) = serve(vec![(vec![page("ETag: \"x\"\r\n", "a"), page("ETag: \"y\"\r\n", "b"), "HTTP/1.1 304 Not Modified\r\n\r\n"], false)]);
        let mut client = Client::new();
        assert_eq!(client.get(&url(port)).unwrap().body, b"a");
        assert_eq!(client.get(&url(port)).unwrap().body, b"b");
        // The page changed, so we got a full new reply, and we keep that one now.
        assert_eq!(client.get(&url(port)).unwrap().body, b"b");
    }

    #[test]
    fn does_not_keep_pages_that_vary() {
        let (port, _) = serve(vec![(vec![page("Cache-Control: max-age=60\r\nVary: Accept-Language\r\n", "a"), page("", "b")], false)]);
        let mut client = Client::new();
        assert_eq!(client.get(&url(port)).unwrap().body, b"a");
        assert_eq!(client.get(&url(port)).unwrap().body, b"b");
        // We cannot handle `Vary` yet, so we ask again every time.
    }

    #[test]
    fn stops_a_reply_with_endless_headers() {
        let long: &'static str = Box::leak(format!("HTTP/1.1 200 OK\r\n{}", "X-A: bbbbbbbbbbbbbbbb\r\n".repeat(20_000)).into_boxed_str());
        let (port, _requests) = serve(vec![(vec![long], false)]);
        let error = Client::new().get(&url(port)).err().unwrap().to_string();
        assert!(error.contains("too long"), "{error}");
        // The server sent 480 KB of headers and no blank line. We gave up after 256 KB.
    }

    #[test]
    fn reads_a_long_chunked_body() {
        // More than 64 KB, in 1000 chunks of 100 bytes: the path that waits for the blank line at the end.
        let mut reply = String::from("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n");
        for _ in 0..1000 {
            reply += &format!("64\r\n{}\r\n", "x".repeat(100));
        }
        reply += "0\r\n\r\n";
        let (port, _requests) = serve(vec![(vec![Box::leak(reply.into_boxed_str())], false)]);
        assert_eq!(Client::new().get(&url(port)).unwrap().body.len(), 100_000);
    }

    #[test]
    fn empties_the_cache_when_it_is_full() {
        let mut client = Client::new();
        client.cache_bytes = MAX_CACHE - 100;
        // The cache says it is nearly full. The next reply does not fit.
        let (port, _requests) = serve(vec![(vec![page("Cache-Control: max-age=60\r\n", "a")], false)]);
        client.get(&url(port)).unwrap();
        assert_eq!(client.cache.len(), 1);
        assert_eq!(client.cache_bytes, 1 + 512);
    }

    #[test]
    fn random_replies_never_crash() {
        // Break good replies in random ways. Any result is fine, but not a panic or a hang.
        let goods: [&[u8]; 3] = [
            b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nX-A: b\r\n\r\nhello",
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n3;x=y\r\nabc\r\n0\r\nT: v\r\n\r\n",
            b"HTTP/1.0 404 Not Found\r\nConnection: close\r\n\r\nbody",
        ];
        let words: [&[u8]; 8] = [b"\r\n", b"\r\n\r\n", b":", b"ffffffffffffffff", b"-1", b"0", b" ", b"chunked"];
        let mut state = 0x2545F4914F6CDD1Du64;
        let mut next = move || {
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            state.wrapping_mul(0x2545F4914F6CDD1D)
        };
        for _ in 0..50_000 {
            let mut p = goods[(next() % 3) as usize].to_vec();
            for _ in 0..1 + next() % 6 {
                let at = (next() % (p.len() as u64 + 1)) as usize;
                match next() % 4 {
                    0 if at < p.len() => p[at] = next() as u8,
                    1 => {
                        let w = words[(next() % 8) as usize];
                        p.splice(at..at, w.iter().copied());
                    }
                    2 => p.truncate(at),
                    _ => p.insert(at, next() as u8),
                }
            }
            let _ = parse_response(&p, next() % 2 == 0);
        }
    }

    #[test]
    fn a_cache_hit_shares_the_body() {
        let (port, _requests) = serve(vec![(vec![page("Cache-Control: max-age=60\r\n", "a body")], false)]);
        let mut client = Client::new();
        let (first, second) = (client.get(&url(port)).unwrap(), client.get(&url(port)).unwrap());
        assert!(Arc::ptr_eq(&first, &second));
        // The same reply, not a copy of it. The server has one reply, so a second request would hang.
    }

    #[test]
    fn streams_a_body_without_holding_it() {
        // 3 MB in 3 pieces of 1 MB. A reader with a 64 KB buffer gets it all, and the connection is used again.
        let reply: &'static str = Box::leak(format!("HTTP/1.1 200 OK\r\nContent-Length: 3000000\r\n\r\n{}", "x".repeat(3_000_000)).into_boxed_str());
        let (port, requests) = serve(vec![(vec![reply, OK], false)]);
        let mut client = Client::new();
        let streamed = client.get_stream(&url(port)).unwrap();
        assert_eq!(streamed.response.status, 200);
        assert!(streamed.response.body.is_empty());
        let (mut body, mut buffer, mut total) = (streamed.body, vec![0u8; 65536], 0);
        loop {
            let n = body.read(&mut buffer).unwrap();
            if n == 0 {
                break;
            }
            assert!(buffer[..n].iter().all(|&b| b == b'x'));
            total += n;
        }
        assert_eq!(total, 3_000_000);
        // The body was read to the end, so the connection went back to the pool: the second request uses it.
        assert_eq!(client.get(&url(port)).unwrap().body, b"ok");
        assert_eq!(requests.try_iter().count(), 2);
    }

    #[test]
    fn a_body_that_is_not_read_to_the_end_does_not_give_back_the_connection() {
        let (port, _requests) = serve(vec![(vec![page("", "abcdef")], true), (vec![OK], false)]);
        let mut client = Client::new();
        let mut streamed = client.get_stream(&url(port)).unwrap();
        streamed.body.read_exact(&mut [0u8; 2]).unwrap();
        drop(streamed);
        // Four bytes are still on the connection, so it cannot be used for a new reply. The next request opens a new one.
        assert!(client.idle.lock().unwrap().is_empty());
        assert_eq!(client.get(&url(port)).unwrap().body, b"ok");
    }

    #[test]
    fn streams_a_chunked_body_and_follows_a_redirect() {
        let moved = "HTTP/1.1 301 Moved\r\nLocation: /next\r\nContent-Length: 4\r\n\r\nmove";
        let chunked = "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n1;x=y\r\n,\r\n0\r\nT: v\r\n\r\n";
        let (port, _requests) = serve(vec![(vec![moved, chunked], false)]);
        let mut client = Client::new();
        let mut text = String::new();
        client.get_stream(&url(port)).unwrap().body.read_to_string(&mut text).unwrap();
        assert_eq!(text, "hello,");
        assert_eq!(client.idle.lock().unwrap().len(), 1);
        // Both replies used one connection. The body of the redirect was read, and the chunked body was read to its end.
    }

    #[test]
    fn a_fresh_copy_in_the_cache_streams_from_memory() {
        let (port, requests) = serve(vec![(vec![page("Cache-Control: max-age=60\r\n", "cached")], false)]);
        let mut client = Client::new();
        assert_eq!(client.get(&url(port)).unwrap().body, b"cached");
        let mut text = String::new();
        client.get_stream(&url(port)).unwrap().body.read_to_string(&mut text).unwrap();
        assert_eq!(text, "cached");
        assert_eq!(requests.try_iter().count(), 1);
    }

    #[test]
    fn a_stream_of_chunks_equals_the_whole_reply() {
        // Random chunked replies. The body that comes out must be the bytes that went in, and the reply must use all the bytes.
        let mut state = 0x9E3779B97F4A7C15u64;
        let mut next = move || {
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            state.wrapping_mul(0x2545F4914F6CDD1D)
        };
        for _ in 0..300 {
            let (mut raw, mut want) = (b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec(), Vec::new());
            for _ in 0..next() % 20 {
                let len = 1 + (next() % 300) as usize;
                let data: Vec<u8> = (0..len).map(|_| b'a' + (next() % 26) as u8).collect();
                raw.extend_from_slice(format!("{len:x}{}\r\n", if next() % 3 == 0 { ";e=1" } else { "" }).as_bytes());
                raw.extend_from_slice(&data);
                raw.extend_from_slice(b"\r\n");
                want.extend_from_slice(&data);
            }
            raw.extend_from_slice(b"0\r\n\r\n");
            let (response, used) = parse_response(&raw, false).unwrap().unwrap();
            assert_eq!((response.body, used), (want, raw.len()));
        }
    }
}
