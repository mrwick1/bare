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

use crate::cookie::{self, Jar, Party, Via};
use crate::{cause, dns, encoding, h2, parallel, replay, tcp, trace};
// JS: like `import * as dns from "./dns"`: other files in this crate.
use bare_tls::client::{self, ClientStream};
use bare_tls::trust_store::{self, TrustStore};
use bare_url::{Scheme, Url};
// JS: like `import { Url } from "bare-url"`: from another package.
use std::borrow::Cow;
use std::collections::HashMap;
// JS: like a `Map`.
use std::io::{self, ErrorKind, Read, Write};
// These give us the `read` and `write` functions on a connection.
// Rust needs this import before you can call them.
use std::net::{SocketAddr, TcpStream};
use std::sync::{Arc, Mutex, OnceLock};
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

pub(crate) const MAX_REPLY: usize = 128 * 1024 * 1024;
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
type IdleConnection = (Stream, Instant);

/// A server: the scheme, the host and the port. A connection only works for the server it was opened to, and a
/// plain connection must never serve an `https` request, so the scheme is part of the key.
// The port and route share one hash input: bit 16 identifies the proxy route.
// BARE_PROXY is fixed process-wide, so there is only one proxy route. Rotation
// must key both pools by proxy endpoint AND credentials.
type Server = (Scheme, String, u32);

/// The idle connections of a client, by server, the last one that went idle at the end. A `Body` that is read to the end gives its
/// connection back to it. A server keeps at most `MAX_IDLE_PER_SERVER`: the number of connections that one page uses at once.
type Pool = Arc<Mutex<HashMap<Server, Vec<IdleConnection>>>>;

const MAX_IDLE_PER_SERVER: usize = parallel::MAX_WORKERS;

/// The HTTP/2 session of each server that has one. A session takes many requests at once, so it is never taken out of the map.
type Sessions = Arc<Mutex<HashMap<Server, Arc<h2::Conn>>>>;

/// A cached reply is found by scheme, host, port and path.
type CacheKey = (Scheme, String, u16, String);

/// A stored reply: when we stored it, and for how long it stays fresh.
struct Cached {
    response: Arc<Response>,
    // Shared with the caller: a cache hit gives out the same reply, and does not copy the body.
    stored: Instant,
    fresh_for: Duration,
    // Until this time has passed, we use the copy without asking the server.
    owner: u64,
    // 0: the reply says `public` or `immutable`, and answers every jar. Else the id of the jar that stored it (`Jar::id`): the reply
    // can depend on its cookies, and it answers that jar only without a request. One slot for each URL: a reply of another jar replaces it.
}

/// The stored replies and their size, counted with `cache_size`. A client and its forks share one: a script that a worker
/// downloaded answers the next document of the page without a request, as in Chrome.
#[derive(Default)]
struct Cache {
    map: HashMap<CacheKey, Cached>,
    bytes: usize,
}

impl Cache {
    /// Remove a stored reply, and take its size off the count.
    fn forget(&mut self, key: &CacheKey) {
        if let Some(old) = self.map.remove(key) {
            self.bytes -= cache_size(&old.response);
        }
    }

    /// Store a reply. The cache is emptied when the reply does not fit. A reply over `MAX_CACHE` is not kept, and it leaves the rest.
    fn put(&mut self, key: CacheKey, response: Arc<Response>, fresh_for: Duration, owner: u64) {
        self.forget(&key);
        let size = cache_size(&response);
        if size > MAX_CACHE {
            return;
        }
        if self.bytes + size > MAX_CACHE {
            self.map.clear();
            self.bytes = 0;
        }
        self.bytes += size;
        self.map.insert(key, Cached { response, stored: Instant::now(), fresh_for, owner });
    }

    /// The server answered 304 to a question about the stored reply: it is right. Its headers replace the stored ones of the same name
    /// (RFC 9111 section 4.3.4), the reply is fresh again, and the stored reply comes back with its body. `None`: it is not stored (any more).
    fn revalidated(&mut self, key: &CacheKey, not_modified: &Response) -> Option<Arc<Response>> {
        let cached = self.map.get_mut(key)?;
        let news: Vec<_> = not_modified.headers.iter().filter(|(n, _)| !matches!(n.as_str(), "content-length" | "content-encoding" | "transfer-encoding" | "connection" | "keep-alive")).collect();
        // The stored `Age` is old. Only the one of the 304 counts.
        let stored = Arc::make_mut(&mut cached.response);
        stored.headers.retain(|(n, _)| n != "age" && !news.iter().any(|(m, _)| m == n));
        stored.headers.extend(news.into_iter().cloned());
        cached.stored = Instant::now();
        cached.fresh_for = freshness(stored).unwrap_or(Duration::ZERO);
        Some(cached.response.clone())
    }
}

/// The questions that ask a server whether a stored reply changed: `If-None-Match` for an `ETag`, `If-Modified-Since` for a date.
fn questions(stored: &Response) -> String {
    let mut out = String::new();
    if let Some(etag) = stored.header("etag") {
        out += &format!("If-None-Match: {etag}\r\n");
    }
    if let Some(date) = stored.header("last-modified") {
        out += &format!("If-Modified-Since: {date}\r\n");
    }
    out
}

/// May the server be asked later whether this reply changed?
fn has_validator(response: &Response) -> bool {
    response.header("etag").is_some() || response.header("last-modified").is_some()
}

/// The methods of the Fetch standard that the client sends. `PATCH` goes on the wire in upper case here: the caller decides the case
/// rules (the Fetch standard keeps `patch` in lower case, and that is a wire form this enum cannot hold).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Put,
    Patch,
    Delete,
    Head,
    Options,
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Post => "POST",
            Method::Put => "PUT",
            Method::Patch => "PATCH",
            Method::Delete => "DELETE",
            Method::Head => "HEAD",
            Method::Options => "OPTIONS",
        }
    }

    /// A method that reads only (RFC 9110 section 9.2.1, minus OPTIONS and TRACE, which we leave out). We may send it again on a new
    /// connection, and it never invalidates a cached reply.
    pub fn safe(self) -> bool {
        matches!(self, Method::Get | Method::Head)
    }
}

/// One request. It borrows everything from the caller: the request stores no owned header or body data. Small bodies are copied into the
/// wire buffer; large bodies are written directly from this slice. `Host`, `Content-Length` and the other headers that frame a message are made by the
/// client, so the caller must not set them (`Client::send` gives an error).
pub struct Request<'a> {
    pub method: Method,
    pub url: &'a Url,
    /// Extra request headers, like `("Authorization", "Bearer ...")`. Values are never put in an error message.
    pub headers: &'a [(&'a str, &'a str)],
    /// The body. GET and HEAD have none (`check_request` refuses one).
    pub body: &'a [u8],
    /// Cookies. A `Cookie` header in `headers` stops the jar from adding one.
    pub cookies: Cookies<'a>,
}

/// How a request relates to the page, for the cookie jar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ctx<'a> {
    /// A subresource, `fetch` or XHR of the document of this client (`Client::set_site`). A cross-site one gets only `SameSite=None`
    /// cookies, and its reply sets only those.
    Embedded,
    /// A top-level navigation. `None`: the user started it (every cookie goes). `Some(site)`: a page of that site started it (`cookie::site_of`).
    /// A cross-site one gets `Lax` cookies on a safe method, and never `Strict` ones. A redirect that leaves the site makes the rest of
    /// the chain cross-site (the site `""` stands for that). A reply to a navigation may set any cookie.
    Navigation(Option<&'a str>),
}

/// What a request does with cookies. The default is the cookies of the jar, in the context of the document of the client.
#[derive(Clone, Copy, Debug)]
pub struct Cookies<'a> {
    /// False: no `Cookie` header, and the `Set-Cookie` headers of the reply are dropped (`credentials: "omit"`, or a cross-origin
    /// request without credentials).
    pub on: bool,
    pub ctx: Ctx<'a>,
}

impl Default for Cookies<'_> {
    fn default() -> Self {
        Cookies { on: true, ctx: Ctx::Embedded }
    }
}

impl<'a> Request<'a> {
    pub fn get(url: &'a Url) -> Request<'a> {
        Request { cookies: Cookies::default(), method: Method::Get, url, headers: &[], body: &[] }
    }

    pub fn post(url: &'a Url, headers: &'a [(&'a str, &'a str)], body: &'a [u8]) -> Request<'a> {
        Request { cookies: Cookies::default(), method: Method::Post, url, headers, body }
    }
}

/// What a browser sends about itself on the wire (`Client::set_identity`). A request with no `User-Agent` of its own gets `user_agent`.
/// An https request gets each of `hints` (a header name and its value) that the caller did not set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    pub user_agent: &'static str,
    pub hints: Vec<(&'static str, String)>,
}

/// The `User-Agent` of a client with no `Identity`: direct use of this crate, outside a browser.
const DEFAULT_USER_AGENT: &str = "bare/0.1";

/// What to send for one hop of a redirect chain. A redirect can change the method, the body and the headers.
struct Plan<'a> {
    cookies: Cookies<'a>,
    method: Method,
    headers: Cow<'a, [(&'a str, &'a str)]>,
    body: &'a [u8],
    // The identity of the client that makes the request (`None`: `DEFAULT_USER_AGENT`, no hints).
    ident: Option<Arc<Identity>>,
}

impl Plan<'_> {
    /// Only a GET with no body and no header of the caller may use the cache. A cached reply to a request with an
    /// `Authorization` header must not answer a request without it (after a logout), so such a request bypasses the
    /// cache until the cache stores variants. A POST changes things on the server: never a cache.
    fn cacheable(&self) -> bool {
        self.method == Method::Get && self.headers.is_empty()
    }
}

/// Headers that frame the message or belong to one connection. The client writes them, so a caller must not.
const CLIENT_HEADERS: [&str; 10] =
    ["host", "content-length", "transfer-encoding", "connection", "keep-alive", "proxy-connection", "te", "trailer", "upgrade", "expect"];

/// Headers that a redirect to another origin must not carry (Fetch standard, "HTTP-redirect fetch", and Cookie).
const SECRET_HEADERS: [&str; 3] = ["authorization", "cookie", "proxy-authorization"];

/// Headers that describe a request body. A redirect that turns a POST into a GET drops them (Fetch standard).
const BODY_HEADERS: [&str; 4] = ["content-type", "content-encoding", "content-language", "content-location"];

/// A body up to this size is written together with the head: one write, one packet.
const INLINE_BODY: usize = 16 * 1024;

/// Fetches pages. It keeps idle connections so that the next request can reuse one.
#[derive(Default)]
// Gives `Client::default()`: a client with an empty list. JS: the starting values of a class.
pub struct Client {
    // JS: like a class with a private field.
    idle: Pool,
    // The HTTP/2 sessions, by server (see `h2.rs`). A fork shares them.
    sessions: Sessions,
    // The open connections that wait for a next request.
    // The key is the server: its scheme, host and port. A connection only works for the server
    // it was opened to. The value is the connection and the time it became idle.
    // Up to `MAX_IDLE_PER_SERVER` connections per server: workers (see `parallel.rs`) open more at once, as Chrome does (6).
    cache: Arc<Mutex<Cache>>,
    // Replies we may use again. The key is the scheme, the host, the port and the path.
    // It lives in memory only. `MAX_CACHE` is the size limit. A fork shares it.
    roots: Arc<OnceLock<Result<TrustStore, String>>>,
    // The trusted root certificates for https. `open` loads the OS bundle on the first https connection and keeps it.
    // The roots never come from a peer. A fork of the client shares them: the bundle is read once.
    pub(crate) workers: Arc<parallel::Workers>,
    // The worker threads of this client and its forks (see `parallel.rs`).
    pub(crate) jar: Arc<Mutex<Jar>>,
    // The cookies. In memory only. A fork shares it, so a new document of the same session keeps its cookies; a new client starts empty.
    big: bool,
    // A download client (`allow_big`): a `Content-Length` over `MAX_REPLY` does not fail the request. A fork does not copy it.
    pub(crate) site: Option<Arc<str>>,
    // The site of the document that this client serves (`cookie::site_of`), for `SameSite`. `None`: every request is first-party.
    identity: Option<Arc<Identity>>,
    // The identity that every request of this client sends (see `set_identity`). A fork shares it.
    tape: Option<Arc<replay::Tape>>,
    // Record or replay of replies (see `replay.rs`). Off unless `BARE_NET_RECORD` or `BARE_NET_REPLAY` is set, or a test sets it. A fork shares it.
}

/// Check a request before any DNS lookup or socket. The messages name no header value and no body byte.
fn check_request(req: &Request) -> Result<(), Error> {
    if req.method.safe() && !req.body.is_empty() {
        return Err("a GET or HEAD request has no body".into());
    }
    for (name, value) in req.headers {
        // A name is a "token": letters, digits and these marks. It cannot hold a space, a colon, CR, LF or NUL.
        if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b)) {
            return Err("invalid request header name".into());
        }
        if CLIENT_HEADERS.iter().any(|h| name.eq_ignore_ascii_case(h)) {
            return Err(format!("the client sets the {name} header itself").into());
        }
        // A value has no control byte (CR, LF and NUL end or split a header), and no space or tab at its ends.
        if value.bytes().any(|b| (b < b' ' && b != b'\t') || b == 0x7f) || value.starts_with([' ', '\t']) || value.ends_with([' ', '\t']) {
            return Err("invalid request header value".into());
        }
    }
    Ok(())
}

/// How many bytes of memory a stored reply takes, about.
fn cache_size(response: &Response) -> usize {
    response.body.len() + 512
    // The 512 is for the headers and the key. It stops a million tiny replies from costing nothing.
}

/// A reused connection that the server closed is dead, and we can see it before we write: the server's FIN makes a
/// read return 0. Used before a POST, which we must never send twice.
/// For TLS, we look at the socket under it (peek: nothing is read) and ask the TLS stream if it holds bytes.
/// Any byte waiting on the socket, even an alert, means "not alive": a fresh connection is safe, a guess is not.
fn alive(stream: &Stream) -> bool {
    let tcp = match stream {
        Stream::Tcp(tcp) => tcp,
        Stream::ProxyTcp(proxy) => &proxy.0,
        Stream::Tls(tls) if tls.reusable() => tls.get_ref(),
        _ => return false,
    };
    if tcp.set_nonblocking(true).is_err() {
        return false;
    }
    let alive = matches!(tcp.peek(&mut [0u8; 1]), Err(e) if e.kind() == ErrorKind::WouldBlock);
    // No bytes and no close: `WouldBlock`. Bytes that nobody asked for mean that the connection is out of step.
    tcp.set_nonblocking(false).is_ok() && alive
}

impl Client {
    pub fn new() -> Client {
        Client { tape: replay::Tape::from_env(), ..Client::default() }
    }

    /// Record or replay with this tape, for this client and its forks (a test; the environment does it for `new`).
    pub fn use_tape(&mut self, tape: Arc<replay::Tape>) {
        self.tape = Some(tape);
    }

    /// A client that trusts these roots for https, and not the OS bundle. For tests with a private CA.
    /// The roots are the only trust anchors: there is no way to turn the checks off.
    pub fn with_roots(roots: TrustStore) -> Client {
        let client = Client::new();
        let _ = client.roots.set(Ok(roots));
        client
    }

    /// A second client that shares the connections, the root certificates and the worker threads of this one. The reply cache is not shared.
    pub fn fork(&self) -> Client {
        Client { idle: self.idle.clone(), sessions: self.sessions.clone(), cache: self.cache.clone(), roots: self.roots.clone(), workers: self.workers.clone(), jar: self.jar.clone(), site: self.site.clone(), identity: self.identity.clone(), tape: self.tape.clone(), big: false }
    }

    /// Let a reply declare more than `MAX_REPLY` bytes. The body still stops at `MAX_REPLY` and at `MAX_TIME` until the caller calls
    /// `Body::unlimited`. For a download, which goes to a file and not to memory.
    pub fn allow_big(&mut self) {
        self.big = true;
    }

    /// A fork with connections of its own: no idle connection and no HTTP/2 session is shared. A download range must not share the TCP
    /// connection of another range.
    pub fn fork_alone(&self) -> Client {
        Client { idle: Pool::default(), sessions: Sessions::default(), ..self.fork() }
    }

    /// Whom a reply answers: every jar (0) when it says `public` or `immutable`, else the jar of this client.
    fn owner_of(&self, response: &Response) -> u64 {
        let control = response.header("cache-control").unwrap_or("").to_ascii_lowercase();
        if control.split(',').map(str::trim).any(|d| d == "public" || d == "immutable") { 0 } else { self.jar.lock().unwrap_or_else(|e| e.into_inner()).id() }
    }

    /// A fresh stored reply may answer this client with no request: it is shared, or it was stored for the jar of this client. Any stored
    /// reply can still be asked about with its validators (the server answers for the cookies of this client), so this does not stop that.
    fn visible(&self, cached: &Cached) -> bool {
        cached.owner == 0 || cached.owner == self.jar.lock().unwrap_or_else(|e| e.into_inner()).id()
    }

    /// The cookie jar of this client and its forks.
    pub fn jar(&self) -> Arc<Mutex<Jar>> {
        self.jar.clone()
    }

    /// Use this jar from now on: the jar of a session, which `document.cookie` reads too.
    pub fn use_jar(&mut self, jar: Arc<Mutex<Jar>>) {
        self.jar = jar;
    }

    /// The site of the document that this client serves (`cookie::site_of`). A request of the default `Cookies` to another site gets
    /// only `SameSite=None` cookies.
    pub fn set_site(&mut self, site: Option<String>) {
        self.site = site.map(Into::into);
    }

    /// Send this identity from now on: its `user_agent` for a request with no `User-Agent`, and its hints on https requests. A browser sets it
    /// on every client it gets. Without it, a request sends `bare/0.1` and no hints.
    pub fn set_identity(&mut self, identity: Identity) {
        self.identity = Some(Arc::new(identity));
    }

    /// Fetch a URL. Follow redirects, and use the cache. Any status can come back: a 404 is still a page.
    pub fn get(&mut self, url: &Url) -> Result<Arc<Response>, Error> {
        self.send(&Request::get(url))
    }

    /// Send a POST. Same rules as `send`.
    pub fn post(&mut self, url: &Url, headers: &[(&str, &str)], body: &[u8]) -> Result<Arc<Response>, Error> {
        self.send(&Request::post(url, headers, body))
    }

    /// Send a request, follow redirects, and read the whole reply. Any status can come back.
    /// The request is checked before any DNS lookup or socket. A POST is never sent twice on its own.
    pub fn send(&mut self, req: &Request) -> Result<Arc<Response>, Error> {
        Ok(self.follow(req, false)?.0)
    }

    /// Send one request and read one reply. Do not follow a redirect (a 3xx comes back as it is, so the caller can
    /// check where it goes before any socket opens), and do not use the cache. The request check, the pool, the
    /// verified TLS and the rule that a POST is never sent twice are the same as for `send`. The reply body may have
    /// `max_body` bytes at most: a bigger `Content-Length` fails before a byte is read, and a chunked or
    /// close-ended body fails when it passes the limit. The error names no header value and no body byte.
    pub fn send_once(&mut self, req: &Request, max_body: usize) -> Result<Arc<Response>, Error> {
        check_request(req)?;
        let plan = Plan { cookies: req.cookies, method: req.method, headers: Cow::Borrowed(req.headers), body: req.body, ident: self.identity.clone() };
        let (mut response, mut body) = self.start(req.url, &plan, "")?;
        if body.size_hint() > max_body {
            body.note(&io::Error::new(ErrorKind::InvalidData, "the reply is too big"));
            return Err("the reply is too big".into());
        }
        let mut bytes = Vec::with_capacity(body.size_hint());
        match (&mut body).take((max_body as u64).saturating_add(1)).read_to_end(&mut bytes) {
            Err(e) if e.kind() == ErrorKind::UnexpectedEof && redirect_target(&response).is_some() => {}
            // A cut body of a redirect is no error: see `fetch`.
            r => {
                r?;
            }
        }
        if bytes.len() > max_body {
            body.note(&io::Error::new(ErrorKind::InvalidData, "the reply is too big"));
            return Err("the reply is too big".into());
        }
        response.body = bytes;
        Ok(Arc::new(response))
    }

    /// Send one checked request without cache or redirects. The caller reads the body and checks each redirect.
    pub fn send_stream_once(&mut self, req: &Request) -> Result<Streamed, Error> {
        check_request(req)?;
        let plan = Plan { cookies: req.cookies, method: req.method, headers: Cow::Borrowed(req.headers), body: req.body, ident: self.identity.clone() };
        let (response, body) = self.start(req.url, &plan, "")?;
        Ok(Streamed { response: Arc::new(response), body })
    }

    /// Like `send_stream_once`, but a fresh copy in the cache answers a cacheable request without a request to the server. The
    /// caller reads the body and gives it to `keep`, so that the next request can use it. For scripts and style sheets.
    pub fn send_stream_cached(&mut self, req: &Request) -> Result<Streamed, Error> {
        check_request(req)?;
        let plan = Plan { cookies: req.cookies, method: req.method, headers: Cow::Borrowed(req.headers), body: req.body, ident: self.identity.clone() };
        let (response, body) = self.hop_stream(req.url, &plan)?;
        Ok(Streamed { response, body: body.expect("a streamed hop has a body") })
    }

    /// Store a streamed reply that `send_stream_cached` gave, with its whole body as the caller read it. Only a 200 is kept, as `get` does:
    /// no `no-store`, no `Vary` but `Accept-Encoding`, and fresh for a while or with an `ETag` or `Last-Modified` to ask about when it
    /// is stale. The body is copied only when it is kept. A reply that came from the cache is not stored again.
    pub fn keep(&mut self, url: &Url, response: &Arc<Response>, body: &[u8]) {
        // With no `Cache-Control` and no validator, a reply is never fresh and cannot be asked about: most replies stop here.
        if response.status != 200 || (response.header("cache-control").is_none() && !has_validator(response)) {
            return;
        }
        let fresh_for = match freshness(response) {
            Some(f) if vary_ok(response) && (!f.is_zero() || has_validator(response)) => f,
            _ => return,
        };
        let key = (url.scheme, url.host.clone(), url.port, url.path.clone());
        let mut cache = self.cache.lock().unwrap();
        if cache.map.get(&key).is_some_and(|c| Arc::ptr_eq(&c.response, response)) {
            return;
        }
        let mut stored = Response::clone(response);
        stored.body = body.to_vec();
        let owner = self.owner_of(response);
        cache.put(key, Arc::new(stored), fresh_for, owner);
    }

    /// Like `send`, with a body that reads from the connection. See `get_stream`.
    pub fn send_stream(&mut self, req: &Request) -> Result<Streamed, Error> {
        let (response, body) = self.follow(req, true)?;
        Ok(Streamed { response, body: body.expect("a streamed hop has a body") })
    }

    /// Send the request, and follow redirects like a browser does (Fetch standard, "HTTP-redirect fetch"):
    /// - 301 and 302 turn a POST into a GET, and 303 turns anything but GET into a GET. The body goes, and so do the
    ///   headers that describe it. 307 and 308 keep the method and the body.
    /// - A redirect to another origin (another scheme, host or port) drops `Authorization`, `Cookie` and
    ///   `Proxy-Authorization`.
    fn follow(&mut self, req: &Request, stream: bool) -> Result<(Arc<Response>, Option<Body>), Error> {
        check_request(req)?;
        let mut plan = Plan { cookies: req.cookies, method: req.method, headers: Cow::Borrowed(req.headers), body: req.body, ident: self.identity.clone() };
        let mut current = req.url;
        // The URL we ask for now. It starts as the one we were given.
        let mut next;
        // Where a redirect sends us. We own this `Url`, so it must live outside the loop.
        for _ in 0..=MAX_REDIRECTS {
            // `0..=20` is 21 turns: the first request plus 20 redirects.
            let (response, body) = if stream { self.hop_stream(current, &plan)? } else { (self.load(current, &plan)?, None) };
            let Some(location) = redirect_target(&response) else {
                return Ok((response, body));
                // Not a redirect: this is the page.
            };
            let target = current.join(location)?;
            // `Location` may be relative, like "/new". `join` makes it a full URL.
            // A link to `https://` is a full URL too. The next turn opens it with verified TLS.
            if !current.same_origin(&target) && plan.headers.iter().any(|(n, _)| SECRET_HEADERS.iter().any(|s| n.eq_ignore_ascii_case(s))) {
                plan.headers = Cow::Owned(plan.headers.iter().filter(|(n, _)| !SECRET_HEADERS.iter().any(|s| n.eq_ignore_ascii_case(s))).copied().collect());
            }
            // Fetch standard, "HTTP-redirect fetch": 301 and 302 turn a POST into a GET; 303 turns every method but GET and HEAD into a GET.
            if (plan.method == Method::Post && matches!(response.status, 301 | 302)) || (response.status == 303 && !plan.method.safe()) {
                plan.method = Method::Get;
                plan.body = &[];
                if plan.headers.iter().any(|(n, _)| BODY_HEADERS.iter().any(|s| n.eq_ignore_ascii_case(s))) {
                    plan.headers = Cow::Owned(plan.headers.iter().filter(|(n, _)| !BODY_HEADERS.iter().any(|s| n.eq_ignore_ascii_case(s))).copied().collect());
                }
            }
            // The body of a redirect is not wanted. Read it (a redirect has a small one) so that the connection can be used again.
            if let Some(mut body) = body {
                match io::copy(&mut (&mut body).take(MAX_REDIRECT_BODY), &mut io::sink()) {
                    Err(e) if e.kind() != ErrorKind::UnexpectedEof => return Err(e.into()),
                    _ => {}
                    // A cut body of a redirect is no error: see `fetch`.
                }
            }
            // A chain that leaves the site of the page that started the navigation stays cross-site (6265bis, "same-site" of a redirect chain).
            if let Ctx::Navigation(Some(from)) = plan.cookies.ctx
                && !from.is_empty()
                && !cookie::same_site(from, &target)
            {
                plan.cookies.ctx = Ctx::Navigation(Some(""));
            }
            next = target;
            current = &next;
        }
        Err(format!("too many redirects (more than {MAX_REDIRECTS})").into())
        // `.into()` converts our text into the `Error` type that this function returns.
    }

    /// Get one URL from the cache, or from the server. Do not follow redirects.
    fn load(&mut self, url: &Url, plan: &Plan) -> Result<Arc<Response>, Error> {
        // A plain copy of the same host, port and path must not answer an https URL: the scheme is in the cache key.
        if !plan.cacheable() {
            return Ok(Arc::new(self.fetch(url, plan, "")?));
        }
        let key = (url.scheme, url.host.clone(), url.port, url.path.clone());
        let mut conditions = String::new();
        // Extra request headers. They stay empty unless we have an old copy to check.

        if let Some(cached) = self.cache.lock().unwrap().map.get(&key) {
            if cached.stored.elapsed() < cached.fresh_for && self.visible(cached) {
                return Ok(cached.response.clone());
                // Still fresh: use the copy. No request at all.
            }
            // Stale. The copy may still be right, so we ask the server: "has it changed?"
            conditions = questions(&cached.response);
        }

        let response = Arc::new(self.fetch(url, plan, &conditions)?);

        if response.status == 304
            && let Some(stored) = self.cache.lock().unwrap().revalidated(&key, &response)
        {
            // "Not Modified": our copy is still right. The reply has no body. The 304 may give a new `max-age`.
            return Ok(stored);
        }

        match freshness(&response) {
            Some(fresh_for)
                if response.status == 200
                    && vary_ok(&response)
                    && (!fresh_for.is_zero() || has_validator(&response)) =>
            {
                // We keep a reply only if all of these are true:
                // - It is a 200 and the server did not say `no-store`.
                // - It has no `Vary` header, or only `Vary: Accept-Encoding`. `Vary` says the reply depends on a request
                //   header. We do not handle that, except for the one header that this client always sends the same.
                // - It is useful: it is fresh for a while, or we can ask the server about it later.
                let owner = self.owner_of(&response);
                self.cache.lock().unwrap().put(key, response.clone(), fresh_for, owner);
            }
            _ => {
                self.cache.lock().unwrap().forget(&key);
                // Do not keep this reply. Throw away any older copy too: it is out of date.
            }
        }
        Ok(response)
    }

    /// Send a request and read the whole reply. Reuse an idle connection when there is one.
    fn fetch(&mut self, url: &Url, plan: &Plan, conditions: &str) -> Result<Response, Error> {
        let (mut response, mut body) = self.start(url, plan, conditions)?;
        let mut bytes = Vec::with_capacity(body.size_hint());
        // A reply with a `Content-Length` gets its room at once: a list that grows by doubling holds up to twice the body for a moment.
        match body.read_to_end(&mut bytes) {
            // A redirect with no length ends at the close. A server may close TLS with no close_notify (craigslist.org does).
            // Nobody wants the body of a redirect, so a cut body is no error here.
            Err(e) if e.kind() == ErrorKind::UnexpectedEof && redirect_target(&response).is_some() => {}
            r => {
                r?;
            }
        }
        if bytes.capacity() - bytes.len() > bytes.len() / 8 {
            bytes.shrink_to_fit();
            // Give the spare room back. For a big list, the system does this without a copy.
        }
        response.body = bytes;
        Ok(response)
    }

    /// Send a request and read the status and the headers. The body is not read: the `Body` reads it from the
    /// connection when the caller asks. Reuse an idle connection when there is one.
    /// A new connection, and a session of its own when the server chose `h2`: the session goes in the map for the next request.
    /// The gate lets only `parallel::MAX_CONNECTING` connections open at once. A request that waited at the gate looks again for a session,
    /// because one that was dialed meanwhile serves it: this is why a page of 40 assets opens one connection, not six.
    fn dial(&self, url: &Url, server: &Server, proxy: Option<&'static Proxy>, rec: &mut Option<Box<trace::Rec>>) -> Result<Dialed, Error> {
        let _inside = self.workers.connecting.enter();
        if url.scheme == Scheme::Https
            && let Some(conn) = self.sessions.lock().unwrap().get(server).filter(|c| c.usable())
        {
            return Ok(Dialed::H2(conn.clone()));
        }
        Ok(match open(url, &self.roots, proxy, rec)? {
            Stream::Tls(tls) if tls.alpn_h2() => {
                let conn = h2::Conn::start(*tls)?;
                let mut sessions = self.sessions.lock().unwrap();
                match sessions.get(server) {
                    Some(old) if old.usable() => {}
                    _ => {
                        sessions.insert(server.clone(), conn.clone());
                    }
                }
                Dialed::H2(conn)
            }
            stream => Dialed::H1(stream),
        })
    }

    /// One request on an HTTP/2 session. `Unprocessed` says that the server did nothing with it.
    fn h2_start(&mut self, conn: &Arc<h2::Conn>, url: &Url, plan: &Plan, conditions: &str, rec: &mut Option<Box<trace::Rec>>, reused: bool) -> Result<(Response, Body), Error> {
        let (authority, length) = (host_header(url), plan.body.len().to_string());
        let headers = h2_headers(url, plan, conditions, &authority, &length);
        let mut clock = trace::Clock::new();
        let sent = Instant::now();
        let (head, mut body) = conn.request(&headers, plan.body, plan.method == Method::Head, MAX_REPLY, sent + MAX_TIME)?;
        self.note_head(sent.elapsed());
        if let Some(r) = rec {
            r.ttfb = clock.lap();
            r.reused = reused;
            r.status = head.status;
            r.h2 = true;
        }
        if !plan.method.safe() && (200..400).contains(&head.status) {
            self.cache.lock().unwrap().forget(&(url.scheme, url.host.clone(), url.port, url.path.clone()));
        }
        let response = Response { status: head.status, headers: head.headers, body: Vec::new(), close: false };
        if !self.big && body.declared.is_some_and(|n| n > MAX_REPLY) {
            return Err("the reply is too big".into());
        }
        let undo = response.header("content-encoding").and_then(encoding::codings).filter(|c| !c.is_empty()).filter(|_| !matches!(body.declared, Some(0)));
        // The clock of the body starts where the head ended.
        body.start = Instant::now();
        body.rec = rec.take();
        let mut live = self.tapped(url, plan, &response, Body(Source::H2(Box::new(body))));
        if let Some(codings) = undo {
            live = Body(Source::Decoded(Box::new(encoding::Decoded::new(live, codings))));
        }
        Ok((response, live))
    }

    fn start(&mut self, url: &Url, plan: &Plan, conditions: &str) -> Result<(Response, Body), Error> {
        let with_cookies;
        let conditions = match self.cookie_line(url, plan) {
            Some(line) => {
                with_cookies = format!("{conditions}Cookie: {line}\r\n");
                &with_cookies
            }
            None => conditions,
        };
        let tape = self.tape.clone();
        let found = tape.as_ref().filter(|t| t.reads()).map(|t| t.find(&Self::label(url, plan)));
        let replayed = matches!(found, Some(Some(_)));
        let result = match found {
            Some(Some(entry)) => self.replayed(entry, url, plan),
            Some(None) if !tape.as_ref().is_some_and(|t| t.writes()) => Err(format!("replay: no recorded reply for {}", Self::label(url, plan)).into()),
            _ => match self.attempt(url, plan, conditions) {
                // The server did not start the request (`GOAWAY`, a refused stream): any method may run again, once, on a new connection.
                Err(e) if e.is::<h2::Unprocessed>() => self.attempt(url, plan, conditions),
                result => result,
            },
        };
        let (response, body) = match result {
            Ok(ok) => ok,
            Err(e) => {
                if let Some(tape) = tape.as_ref().filter(|t| t.writes() && !replayed) {
                    tape.slot(Self::label(url, plan)).save(&replay::Entry { error: Some(e.to_string()), ..replay::Entry::default() });
                }
                return Err(e);
            }
        };
        self.store_cookies(url, plan, &response);
        Ok((response, body))
    }

    /// The label of a request on the tape (see `replay.rs`).
    fn label(url: &Url, plan: &Plan) -> String {
        replay::Tape::label(plan.method.as_str(), &format!("{}://{}:{}{}", url.scheme, url.host, url.port, url.path), plan.body)
    }

    /// Answer a request from the tape: no DNS, no socket. The reply is as `attempt` gives it, with its body in memory.
    fn replayed(&mut self, entry: replay::Entry, url: &Url, plan: &Plan) -> Result<(Response, Body), Error> {
        if let Some(error) = entry.error {
            return Err(error.into());
        }
        if !plan.method.safe() && (200..400).contains(&entry.status) {
            self.cache.lock().unwrap().forget(&(url.scheme, url.host.clone(), url.port, url.path.clone()));
        }
        let response = Response { status: entry.status, headers: entry.headers, body: Vec::new(), close: entry.close };
        let undo = response.header("content-encoding").and_then(encoding::codings).filter(|c| !c.is_empty()).filter(|_| !entry.body.is_empty());
        let live = if entry.complete {
            Body(Source::Memory(Arc::new(Response { body: entry.body, ..response.clone() }), 0))
        } else {
            Body(Source::Cut(io::Cursor::new(entry.body)))
        };
        Ok((response, match undo {
            Some(codings) => Body(Source::Decoded(Box::new(encoding::Decoded::new(live, codings)))),
            None => live,
        }))
    }

    /// While recording, give the body a tap that writes the reply to the tape when the body ends. The body is the raw one, before `Content-Encoding` is undone.
    fn tapped(&self, url: &Url, plan: &Plan, response: &Response, body: Body) -> Body {
        match &self.tape {
            Some(tape) if tape.writes() => {
                let entry = replay::Entry { status: response.status, headers: response.headers.clone(), close: response.close, ..replay::Entry::default() };
                Body(Source::Tap(Box::new(Tap { inner: body, slot: Some(tape.slot(Self::label(url, plan))), entry })))
            }
            _ => body,
        }
    }

    /// How the request relates to the document, for `SameSite`.
    fn party(&self, url: &Url, plan: &Plan) -> Party {
        let same = |doc: &str| cookie::same_site(doc, url);
        match plan.cookies.ctx {
            Ctx::Embedded if self.site.as_deref().is_none_or(same) => Party::First,
            Ctx::Navigation(from) if from.is_none_or(same) => Party::First,
            Ctx::Navigation(_) if plan.method.safe() => Party::Nav,
            _ => Party::Third,
        }
    }

    /// The `Cookie` header for this request, or `None`: credentials are off, the caller sent its own, or no cookie matches. A request to a
    /// host with no cookies costs one lock and one hash lookup.
    fn cookie_line(&self, url: &Url, plan: &Plan) -> Option<String> {
        if !plan.cookies.on {
            return None;
        }
        let mut jar = self.jar.lock().unwrap_or_else(|e| e.into_inner());
        if jar.is_empty() || plan.headers.iter().any(|(n, _)| n.eq_ignore_ascii_case("cookie")) {
            return None;
        }
        let now = jar.now();
        jar.header(&cookie::At::of(url), self.party(url, plan), now)
    }

    /// Store the `Set-Cookie` headers of a reply (not when credentials are off).
    fn store_cookies(&self, url: &Url, plan: &Plan, response: &Response) {
        if !plan.cookies.on || !response.headers.iter().any(|(n, _)| n == "set-cookie") {
            return;
        }
        let third = matches!(plan.cookies.ctx, Ctx::Embedded) && self.party(url, plan) == Party::Third;
        let mut jar = self.jar.lock().unwrap_or_else(|e| e.into_inner());
        let now = jar.now();
        for (_, v) in response.headers.iter().filter(|(n, _)| n == "set-cookie") {
            jar.set(&cookie::At::of(url), v, Via::Http { third }, now);
        }
    }

    /// One try. A try that fails leaves a record with its cause (only when tracing is on): no body exists to carry it.
    fn attempt(&mut self, url: &Url, plan: &Plan, conditions: &str) -> Result<(Response, Body), Error> {
        let mut rec = trace::begin(|| format!("{}://{}:{}{}", url.scheme, url.host, url.port, url.path));
        let result = self.attempt_on(url, plan, conditions, &mut rec);
        if let (Err(e), Some(mut r)) = (&result, rec) {
            r.error = Some(cause::describe(&**e));
            trace::push(*r);
        }
        result
    }

    fn attempt_on(&mut self, url: &Url, plan: &Plan, conditions: &str, rec: &mut Option<Box<trace::Rec>>) -> Result<(Response, Body), Error> {
        // The idle list is keyed by scheme too, so a plain connection never serves https.
        let configured = proxy_config()?;
        let proxy = configured.filter(|_| !loopback(&url.host));
        let server = (url.scheme, url.host.clone(), u32::from(url.port) | (u32::from(proxy.is_some()) << 16));
        // An empty list stays in the map: the next connection that goes idle needs it, and the map has one key for each server.
        let idle = self.idle.lock().unwrap().get_mut(&server).and_then(Vec::pop);
        let get = plan.method.safe();
        let session = if url.scheme == Scheme::Https { self.sessions.lock().unwrap().get(&server).filter(|c| c.usable()).cloned() } else { None };
        if let Some(conn) = session {
            // A session takes one more request at once: no idle connection and no new one. A request that the server did not start, or a GET
            // that failed on an old session (as on an old connection), runs again below on a new connection.
            match self.h2_start(&conn, url, plan, conditions, rec, true) {
                Err(e) if e.is::<h2::Unprocessed>() || get => {}
                result => return result,
            }
        }
        let (mut stream, reused) = match idle {
            // `pop` takes the connection out of the list. While we use it, nobody else can.
            Some((stream, since)) if since.elapsed() < MAX_IDLE && (get || alive(&stream)) => (stream, true),
            // There is a connection, and it has not been idle for too long. Use it. A POST also needs the
            // server to have kept it: we cannot retry a POST after a write, so we look first.
            _ => match self.dial(url, &server, proxy, rec)? {
                Dialed::H1(stream) => (stream, false),
                Dialed::H2(conn) => return self.h2_start(&conn, url, plan, conditions, rec, false),
            },
            // `_` means "anything else": no connection, or an old one. Open a new one.
            // An old connection is dropped here, and Rust closes it for us.
        };

        let mut clock = trace::Clock::new();
        let mut sent = Instant::now();
        let mut result = send(&mut stream, url, plan, conditions);
        if result.is_err() && reused && get {
            // A reused connection can fail when the server closed it while it was idle.
            // We cannot know that before we try. The error is not the page's fault, so we
            // try once more on a new connection. This is safe because GET only reads.
            // Never for a POST: the server may have got the request and done what it says, and a second one would do it twice.
            stream = match self.dial(url, &server, proxy, rec)? {
                Dialed::H1(stream) => stream,
                Dialed::H2(conn) => return self.h2_start(&conn, url, plan, conditions, rec, false),
            };
            clock = trace::Clock::new();
            sent = Instant::now();
            result = send(&mut stream, url, plan, conditions);
        }
        let (head, leftover) = result?;
        // The clock of the body starts where the head ended.
        let head_end = Instant::now();
        self.note_head(head_end - sent);
        if let Some(r) = rec {
            r.ttfb = clock.lap();
            r.reused = reused;
            r.status = head.response.status;
        }
        if !get && (200..400).contains(&head.response.status) {
            // RFC 9111 section 4.4: a successful unsafe request invalidates the cached target.
            self.cache.lock().unwrap().forget(&(url.scheme, url.host.clone(), url.port, url.path.clone()));
        }
        if let Framing::Length(n) = head.framing
            && n > MAX_REPLY
            && !self.big
        {
            return Err("the reply is too big".into());
        }
        let reusable = !head.response.close && head.framing != Framing::UntilClose;
        let undo = if head.framing == Framing::None { None } else { head.response.header("content-encoding").and_then(encoding::codings).filter(|c| !c.is_empty()) };
        let mut live = Live::new(stream, leftover, head.framing, reusable.then(|| (self.idle.clone(), server)), head_end);
        live.rec = rec.take();
        let live = self.tapped(url, plan, &head.response, Body(Source::Live(Box::new(live))));
        Ok((head.response, match undo {
            Some(codings) => Body(Source::Decoded(Box::new(encoding::Decoded::new(live, codings)))),
            None => live,
        }))
    }

    /// One hop of `send_stream`: a fresh copy from the cache, or a reply from the server whose body is still on the connection. A stale
    /// copy is asked about (`If-None-Match`, `If-Modified-Since`): on a 304 the stored body answers, with the headers of the 304.
    fn hop_stream(&mut self, url: &Url, plan: &Plan) -> Result<(Arc<Response>, Option<Body>), Error> {
        let memory = |response: Arc<Response>| (response.clone(), Some(Body(Source::Memory(response, 0))));
        // An empty cache needs no key: most requests of a page that sends no `max-age` pay one lock and no allocation.
        let mut stale = None;
        if plan.cacheable() {
            let cache = self.cache.lock().unwrap();
            if !cache.map.is_empty() {
                let key = (url.scheme, url.host.clone(), url.port, url.path.clone());
                match cache.map.get(&key) {
                    Some(c) if c.stored.elapsed() < c.fresh_for && self.visible(c) => return Ok(memory(c.response.clone())),
                    Some(c) => stale = Some((key, questions(&c.response))),
                    None => {}
                }
            }
        }
        let Some((key, ask)) = stale else {
            let (response, body) = self.start(url, plan, "")?;
            return Ok((Arc::new(response), Some(body)));
        };
        let (response, mut body) = self.start(url, plan, &ask)?;
        if response.status == 304 {
            // No body follows a 304: read the end of it, so that the connection can go back to the pool.
            let _ = io::copy(&mut body, &mut io::sink());
            if let Some(stored) = self.cache.lock().unwrap().revalidated(&key, &response) {
                return Ok(memory(stored));
            }
            // Another fork dropped the copy meanwhile: ask again, with no question.
            let (response, body) = self.start(url, plan, "")?;
            return Ok((Arc::new(response), Some(body)));
        }
        // A new reply. The old copy is out of date; `keep` stores the new one if it can.
        self.cache.lock().unwrap().forget(&key);
        Ok((Arc::new(response), Some(body)))
    }

    /// Fetch a URL, and give the body as a `Body` that reads from the connection while the caller reads. The caller can
    /// start to use the first bytes of a page before the last ones come, and the page is never all in memory.
    /// Redirects are followed. A fresh copy in the cache is used. A reply that comes from the server is not put in the cache.
    /// The `response` has the status and the headers. Its `body` is empty: read it from `body`.
    pub fn get_stream(&mut self, url: &Url) -> Result<Streamed, Error> {
        self.send_stream(&Request::get(url))
    }
}

/// A new connection: plain or TLS with HTTP/1.1, or an HTTP/2 session.
enum Dialed {
    H1(Stream),
    H2(Arc<h2::Conn>),
}

/// A reply with a body that is still on the connection.
pub struct Streamed {
    pub response: Arc<Response>,
    pub body: Body,
}

/// Open a new connection to the server of this URL. The only place that makes a `Stream`.
/// `https`: TCP, then a TLS 1.3 handshake with the host name checked against `roots` (loaded from the OS on first use).
/// The handshake runs under the read and write time limits that `tcp::connect` sets.
fn open(url: &Url, roots: &OnceLock<Result<TrustStore, String>>, proxy: Option<&'static Proxy>, rec: &mut Option<Box<trace::Rec>>) -> Result<Stream, Error> {
    let mut clock = trace::Clock::new();
    let (host, port) = proxy.map_or((url.host.as_str(), url.port), |p| (p.host.as_str(), p.port));
    let addrs: Vec<SocketAddr> = dns::resolve(host).map_err(|e| cause::Phase("dns", e.into()))?.into_iter().map(|ip| SocketAddr::new(ip, port)).collect();
    if let Some(r) = rec {
        r.dns = clock.lap();
    }
    // Our own DNS lookup (see `dns.rs`) gives IP addresses. Add the port to each one.
    // JS: `ips.map(ip => ({ ip, port }))`.
    let mut tcp = tcp::connect(&addrs).map_err(|e| cause::Phase("connect", e.into()))?;
    // Open a connection, with time limits (see `tcp.rs`).
    if let Some(r) = rec {
        r.tcp = clock.lap();
    }
    if url.scheme == Scheme::Http {
        return Ok(match proxy { Some(p) => Stream::ProxyTcp(Box::new((tcp, p))), None => Stream::Tcp(tcp) });
    }
    if let Some(p) = proxy {
        p.tunnel(&mut tcp, url).map_err(|e| cause::Phase("proxy", e))?;
    }
    let roots = roots.get_or_init(|| trust_store::load_system().map(|r| r.0).map_err(|e| e.to_string())).as_ref().map_err(|e| e.clone())?;
    let tls = if h1_only() { client::connect(tcp, &url.host, roots)? } else { client::connect_h2(tcp, &url.host, roots)? };
    if let Some(r) = rec {
        r.tls = clock.lap();
    }
    Ok(Stream::Tls(Box::new(tls)))
}

/// A single immutable HTTP proxy. Do not derive Debug: credentials must not reach diagnostics.
struct Proxy {
    host: String,
    port: u16,
    auth: String,
}

fn proxy_config() -> Result<Option<&'static Proxy>, Error> {
    static PROXY: OnceLock<Result<Option<Proxy>, &'static str>> = OnceLock::new();
    PROXY
        .get_or_init(|| match std::env::var("BARE_PROXY") {
            Ok(v) => Proxy::parse(&v).map(Some),
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(_) => Err("invalid BARE_PROXY (expected http://[user:pass@]host:port)"),
        })
        .as_ref()
        .map(Option::as_ref)
        .map_err(|e| cause::Phase("proxy", (*e).into()).into())
}

impl Proxy {
    fn parse(value: &str) -> Result<Self, &'static str> {
        let bad = "invalid BARE_PROXY (expected http://[user:pass@]host:port)";
        if !value.is_ascii() || value.bytes().any(|b| b <= b' ' || b == 127) {
            return Err(bad);
        }
        let authority = value.strip_prefix("http://").ok_or(bad)?;
        if authority.contains(['/', '?', '#', '\\']) {
            return Err(bad);
        }
        let (authority, auth) = match authority.rsplit_once('@') {
            Some((login, host)) => {
                if login.contains('@') || !login.contains(':') {
                    return Err(bad);
                }
                (host, format!("Proxy-Authorization: Basic {}\r\n", proxy_base64(login.as_bytes())))
            }
            None => (authority, String::new()),
        };
        let (host, port) = authority.rsplit_once(':').ok_or(bad)?;
        let port = port.parse::<u16>().ok().filter(|p| *p != 0).ok_or(bad)?;
        let host = if let Some(v6) = host.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            v6.parse::<std::net::Ipv6Addr>().map_err(|_| bad)?;
            v6
        } else {
            if host.is_empty() || !host.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-') {
                return Err(bad);
            }
            host
        };
        Ok(Self { host: host.into(), port, auth })
    }

    fn tunnel(&self, tcp: &mut TcpStream, url: &Url) -> Result<(), Error> {
        let host = if url.host.contains(':') {
            format!("[{}]", url.host)
        } else {
            url.host.clone()
        };
        write!(tcp, "CONNECT {host}:{} HTTP/1.1\r\nHost: {host}:{}\r\n{}\r\n", url.port, url.port, self.auth)?;
        tcp.flush()?;
        proxy_reply(tcp)
    }

    /// Change origin form to absolute form, and add credentials only on the proxy hop.
    fn request(&self, out: &mut Vec<u8>, url: &Url) {
        let space = out.iter().position(|b| *b == b' ').unwrap() + 1;
        let prefix = format!("http://{}", host_header(url));
        out.splice(space..space, prefix.bytes());
        let end = out.len() - 2;
        out.splice(end..end, self.auth.bytes());
    }
}

fn proxy_base64(input: &[u8]) -> String {
    const DIGITS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for c in input.chunks(3) {
        let n = ((c[0] as u32) << 16) | ((c.get(1).copied().unwrap_or(0) as u32) << 8) | c.get(2).copied().unwrap_or(0) as u32;
        out.push(DIGITS[(n >> 18) as usize] as char);
        out.push(DIGITS[((n >> 12) & 63) as usize] as char);
        out.push(if c.len() > 1 { DIGITS[((n >> 6) & 63) as usize] as char } else { '=' });
        out.push(if c.len() > 2 { DIGITS[(n & 63) as usize] as char } else { '=' });
    }
    out
}

/// Read exactly the head: bytes after the blank line belong to TLS. Socket read
/// limits still apply, with the same total limit as an origin response head.
fn proxy_reply(stream: &mut impl Read) -> Result<(), Error> {
    let mut raw = Vec::new();
    let start = Instant::now();
    while !raw.ends_with(b"\r\n\r\n") {
        if raw.len() == MAX_HEAD {
            return Err("the headers of the proxy reply are too long".into());
        }
        if start.elapsed() > MAX_TIME {
            return Err("the proxy is too slow".into());
        }
        let mut byte = [0];
        stream.read_exact(&mut byte).map_err(slow)?;
        raw.push(byte[0]);
    }
    // CONNECT framing is independent of Content-Length and Transfer-Encoding.
    // Do not reflect arbitrary proxy text, which can echo credentials.
    let line = raw.split(|b| *b == b'\r').next().unwrap_or_default();
    if !(line.starts_with(b"HTTP/1.1 ") || line.starts_with(b"HTTP/1.0 "))
        || line.len() < 12
        || !line[9..12].iter().all(u8::is_ascii_digit)
        || (line.len() > 12 && line[12] != b' ')
    {
        return Err("malformed CONNECT response".into());
    }
    let status = std::str::from_utf8(&line[9..12]).unwrap().parse::<u16>().unwrap();
    if !(200..300).contains(&status) {
        return Err(format!("{status} CONNECT refused").into());
    }
    Ok(())
}

#[cfg(test)]
mod proxy_tests {
    use super::*;

    #[test]
    fn connect_stops_at_the_head_and_ignores_body_framing() {
        for status in [200, 204, 299] {
            let head = format!("HTTP/1.1 {status} OK\r\nContent-Length: 999999\r\n\r\n");
            let mut wire = io::Cursor::new([head.as_bytes(), b"TLS bytes"].concat());
            proxy_reply(&mut wire).unwrap();
            assert_eq!(wire.position(), head.len() as u64);
        }
        assert!(proxy_reply(&mut io::Cursor::new(b"HTTP/1.1 2000 bad\r\n\r\n")).is_err());
        assert!(proxy_reply(&mut io::Cursor::new(b"HTTP/1.1 200 OK\r\n")).is_err());
        let mut cap = io::Cursor::new(vec![b'x'; MAX_HEAD + 20]);
        assert!(proxy_reply(&mut cap).is_err());
        assert_eq!(cap.position(), MAX_HEAD as u64);
        let timeout = io::Error::from(ErrorKind::TimedOut);
        struct Stalled(Option<io::Error>);
        impl Read for Stalled {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(self.0.take().unwrap())
            }
        }
        assert!(cause::describe(&*proxy_reply(&mut Stalled(Some(timeout))).unwrap_err()).contains("read timeout"));
    }

    #[test]
    fn proxy_credentials_and_ipv6_parse_without_dependencies() {
        for (raw, encoded) in [("", ""), ("f", "Zg=="), ("fo", "Zm8="), ("foo", "Zm9v"), ("user:pass", "dXNlcjpwYXNz")] {
            assert_eq!(proxy_base64(raw.as_bytes()), encoded);
        }
        let p = Proxy::parse("http://u:p@[::1]:8080").ok().unwrap();
        assert_eq!((&*p.host, p.port), ("::1", 8080));
        for bad in [
            "http://:80",
            "http://::1:80",
            "http://[bad]:80",
            "https://host:80",
            "http://a@host:80",
            "http://a:b@c@host:80",
        ] {
            assert!(Proxy::parse(bad).is_err());
        }
    }

    #[test]
    fn direct_and_proxy_pool_routes_are_separate() {
        let direct = (Scheme::Https, "example.test".to_string(), 443u32);
        let proxy = (Scheme::Https, "example.test".to_string(), 443u32 | (1 << 16));
        let mut pool = HashMap::new();
        pool.insert(direct.clone(), 1);
        pool.insert(proxy.clone(), 2);
        assert_eq!(pool.len(), 2);
        assert_eq!(pool[&direct], 1);
        assert_eq!(pool[&proxy], 2);
    }
}

/// `BARE_NET_H1` (any value) offers `http/1.1` only, so a run over HTTP/1.1 and one over HTTP/2 can be compared on one server.
fn h1_only() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("BARE_NET_H1").is_some())
}

/// The default `Accept-Language`: the same languages as `navigator.languages` (`en-US`, `en`), with the quality that Chrome sends.
const ACCEPT_LANGUAGE: &str = "en-US,en;q=0.9";

/// A host that is this machine. A compressed body costs the CPU of both ends and saves no byte on a loopback link, so we do not ask for one
/// (`Accept-Encoding`). A caller that sets the header gets it sent. A reply that has a coding is decoded in any case.
fn loopback(host: &str) -> bool {
    // The URL parser gives an IPv4 host as digits and dots, and an IPv6 host in its short form. `parse::<IpAddr>` would cost about 380 instructions for each request.
    host == "localhost" || host == "::1" || host.strip_prefix("127.").is_some_and(|rest| rest.bytes().all(|b| b.is_ascii_digit() || b == b'.'))
}

/// The client hints of an https request: each hint of the identity that the caller did not set (`sec-ch-ua` and the other low-entropy hints).
fn hints<'a>(url: &Url, plan: &'a Plan) -> Vec<(&'static str, &'a str)> {
    let Some(ident) = plan.ident.as_deref().filter(|_| url.scheme == Scheme::Https) else { return Vec::new() };
    ident.hints.iter().filter(|(name, _)| !plan.headers.iter().any(|(n, _)| n.eq_ignore_ascii_case(name))).map(|(name, value)| (*name, value.as_str())).collect()
}

/// The headers of an HTTP/2 request: the four pseudo headers first, then what `request_head` writes for HTTP/1.1, with no `Host`.
/// `authority` is `host_header(url)`, and `length` is the length of the body in digits.
fn h2_headers<'a>(url: &'a Url, plan: &'a Plan, conditions: &'a str, authority: &'a str, length: &'a str) -> Vec<(&'a str, &'a str)> {
    let mut headers = vec![(":method", plan.method.as_str()), (":scheme", "https"), (":authority", authority), (":path", url.path.as_str())];
    if !plan.headers.iter().any(|(n, _)| n.eq_ignore_ascii_case("user-agent")) {
        headers.push(("user-agent", plan.ident.as_deref().map_or(DEFAULT_USER_AGENT, |i| i.user_agent)));
    }
    for (name, value) in hints(url, plan) {
        headers.push((name, value));
    }
    if !loopback(&url.host) && !plan.headers.iter().any(|(n, _)| n.eq_ignore_ascii_case("accept-encoding")) {
        headers.push(("accept-encoding", "gzip, deflate, br"));
    }
    if !loopback(&url.host) && !plan.headers.iter().any(|(n, _)| n.eq_ignore_ascii_case("accept-language")) {
        headers.push(("accept-language", ACCEPT_LANGUAGE));
    }
    headers.extend(plan.headers.iter().copied());
    if matches!(plan.method, Method::Post | Method::Put) || !plan.body.is_empty() {
        headers.push(("content-length", length));
    }
    // `conditions` is `Name: value\r\n` for each header.
    headers.extend(conditions.split("\r\n").filter_map(|line| line.split_once(": ")));
    headers
}

/// The request line, the headers and the blank line. A small body is added to the end by the caller.
fn request_head(url: &Url, plan: &Plan, conditions: &str, extra: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(256 + extra + conditions.len() + plan.headers.iter().map(|(n, v)| n.len() + v.len() + 4).sum::<usize>());
    // One list with all the room that it needs: no growing while we write.
    let _ = write!(out, "{} {} HTTP/1.1\r\nHost: {}\r\n", plan.method.as_str(), url.path, host_header(url));
    // `\r\n` is the line ending that HTTP requires.
    // `Host:` says which site we want. One server can host many sites.
    // There is no `Connection:` header. For HTTP/1.1 "keep the connection" is the default.
    if !plan.headers.iter().any(|(n, _)| n.eq_ignore_ascii_case("user-agent")) {
        let _ = write!(out, "User-Agent: {}\r\n", plan.ident.as_deref().map_or(DEFAULT_USER_AGENT, |i| i.user_agent));
    }
    for (name, value) in hints(url, plan) {
        let _ = write!(out, "{name}: {value}\r\n");
    }
    if !loopback(&url.host) && !plan.headers.iter().any(|(n, _)| n.eq_ignore_ascii_case("accept-encoding")) {
        // `br` only over https, as Chrome does: some plain proxies break it.
        out.extend_from_slice(if url.scheme == Scheme::Https { b"Accept-Encoding: gzip, deflate, br\r\n" } else { b"Accept-Encoding: gzip, deflate\r\n" });
    }
    if !loopback(&url.host) && !plan.headers.iter().any(|(n, _)| n.eq_ignore_ascii_case("accept-language")) {
        out.extend_from_slice(b"Accept-Language: ");
        out.extend_from_slice(ACCEPT_LANGUAGE.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    for (name, value) in plan.headers.iter() {
        let _ = write!(out, "{name}: {value}\r\n");
    }
    if matches!(plan.method, Method::Post | Method::Put) || !plan.body.is_empty() {
        let _ = write!(out, "Content-Length: {}\r\n", plan.body.len());
        // The exact size of the body, made here from the slice, never from the caller.
    }
    // `conditions` are extra headers, or nothing. Each one ends with `\r\n`.
    out.extend_from_slice(conditions.as_bytes());
    out.extend_from_slice(b"\r\n");
    // The empty line at the end (`\r\n\r\n`) tells the server that the head is complete.
    out
}

/// Send one request, and read the status line and the headers of the reply. Gives them, and the bytes of the body that came with them.
fn send(stream: &mut Stream, url: &Url, plan: &Plan, conditions: &str) -> Result<(Head, Vec<u8>), Error> {
    // `&mut Stream` means we borrow the connection and may change it. The caller keeps it.
    let inline = plan.body.len() <= INLINE_BODY;
    let mut out = request_head(url, plan, conditions, if inline { plan.body.len() } else { 0 });
    if let Stream::ProxyTcp(proxy) = stream {
        proxy.1.request(&mut out, url);
    }
    if inline {
        out.extend_from_slice(plan.body);
        // A small body goes in the same write as the head: one packet, no wait for an acknowledgement between two.
    }
    stream.write_all(&out)?;
    // Send the request (over TLS: as encrypted records). A connection sends bytes, not text. `write_all` keeps writing until all bytes are sent, or there is an error.
    if !inline && let Err(e) = stream.write_all(plan.body) {
        // A big body goes from the slice of the caller: no copy. A server may answer early ("413 too big") and close.
        // Then the write fails, but the answer is on the connection. It is not reusable: the server did not read the body.
        return match read_head(stream) {
            Ok((mut head, leftover)) => {
                head.response.close = true;
                Ok((head, leftover))
            }
            Err(_) => Err(e.into()),
        };
    }
    stream.flush()?;
    let (mut head, leftover) = read_head(stream)?;
    if plan.method == Method::Head {
        // A reply to HEAD has the headers of a GET reply, and no body, whatever `Content-Length` says (RFC 9110 section 9.3.2).
        head.framing = Framing::None;
    }
    Ok((head, leftover))
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

/// The reply does not depend on a request header that can change: it has no `Vary`, or only `Accept-Encoding`, which this client sends the same for its whole life.
fn vary_ok(response: &Response) -> bool {
    response.headers.iter().filter(|(n, _)| n == "vary").all(|(_, v)| v.split(',').all(|t| t.trim().eq_ignore_ascii_case("accept-encoding")))
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
    /// A reply on an HTTP/2 stream (see `h2.rs`).
    H2(Box<h2::Body>),
    /// The same, with a `Content-Encoding` that is undone as it is read (see `encoding.rs`).
    Decoded(Box<encoding::Decoded>),
    /// A recorded reply that was cut: the bytes that came, then a read error (see `replay.rs`).
    Cut(io::Cursor<Vec<u8>>),
    /// A body that is being recorded (see `replay.rs`).
    Tap(Box<Tap>),
}

/// Reads the body below it, and keeps the bytes. At the end of the body, or an error, or when the body is dropped, they go to the tape.
struct Tap {
    inner: Body,
    slot: Option<replay::Slot>,
    entry: replay::Entry,
}

impl Tap {
    fn save(&mut self) {
        if let Some(slot) = self.slot.take() {
            slot.save(&self.entry);
        }
    }

    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let r = self.inner.read(out);
        match r {
            Ok(0) if !out.is_empty() => {
                self.entry.complete = true;
                self.save();
            }
            Ok(n) => self.entry.body.extend_from_slice(&out[..n]),
            Err(_) => self.save(),
        }
        r
    }
}

impl Drop for Tap {
    fn drop(&mut self) {
        self.save();
    }
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
    /// False after `Body::unlimited`: no `MAX_REPLY` and no `MAX_TIME`.
    limited: bool,
    /// Where to put the connection when the body ends well, if it may be used again.
    pool: Option<(Pool, Server)>,
    done: bool,
    /// The timing record of this request (only when `BARE_NET_TRACE` is set). It goes out when the body is dropped.
    rec: Option<Box<trace::Rec>>,
}

impl Drop for Live {
    fn drop(&mut self) {
        if let Some(mut r) = self.rec.take() {
            r.body = self.start.elapsed().as_micros() as u64;
            r.bytes = self.given as u64;
            trace::push(*r);
        }
    }
}

/// A connection. In tests it can also be some bytes in memory: a reply that stops, or a reply that ends.
enum Stream {
    Tcp(TcpStream),
    ProxyTcp(Box<(TcpStream, &'static Proxy)>),
    /// TCP under verified TLS. Boxed: the record layer holds big buffers. Reads and writes always go through the TLS
    /// layer, so its read-ahead and record sequence numbers stay right.
    Tls(Box<ClientStream<TcpStream>>),
    /// The connection went back to the pool, or was dropped. It gives no bytes.
    Closed,
    #[cfg(test)]
    Fake(io::Cursor<Vec<u8>>, bool),
}

impl Read for Stream {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        match self {
            Stream::Tcp(stream) => stream.read(out),
            Stream::ProxyTcp(proxy) => proxy.0.read(out),
            Stream::Tls(stream) => stream.read(out),
            Stream::Closed => Ok(0),
            #[cfg(test)]
            Stream::Fake(bytes, closed) => match bytes.read(out)? {
                0 if !*closed && !out.is_empty() => Err(ErrorKind::WouldBlock.into()),
                n => Ok(n),
            },
        }
    }
}

impl Write for Stream {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        match self {
            Stream::Tcp(stream) => stream.write(data),
            Stream::ProxyTcp(proxy) => proxy.0.write(data),
            Stream::Tls(stream) => stream.write(data),
            _ => Err(ErrorKind::NotConnected.into()),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Stream::Tcp(stream) => stream.flush(),
            Stream::ProxyTcp(proxy) => proxy.0.flush(),
            Stream::Tls(stream) => stream.flush(),
            _ => Err(ErrorKind::NotConnected.into()),
        }
    }
}

impl Live {
    fn new(stream: Stream, leftover: Vec<u8>, framing: Framing, pool: Option<(Pool, Server)>, start: Instant) -> Live {
        let left = if let Framing::Length(n) = framing { n } else { 0 };
        Live { stream, buf: leftover, at: 0, framing, left, chunk: Chunk::Size, line: Vec::new(), given: 0, start, limited: true, pool, done: false, rec: None }
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
        // A TLS stream with bytes of its own (the rest of a record, half a handshake message) or a close is not given back.
        let clean = match &self.stream {
            Stream::Tcp(_) | Stream::ProxyTcp(_) => true,
            Stream::Tls(tls) => tls.reusable(),
            _ => false,
        };
        if clean
            && self.at == self.buf.len()
            && let Some((pool, server)) = self.pool.take()
        {
            let stream = std::mem::replace(&mut self.stream, Stream::Closed);
            let mut pool = pool.lock().unwrap();
            let list = pool.entry(server).or_default();
            if list.len() == MAX_IDLE_PER_SERVER {
                list.remove(0);
            }
            list.push((stream, Instant::now()));
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
        let r = self.read_body(out);
        if let Err(e) = &r {
            self.note(e);
        }
        r
    }
}

impl Body {
    /// Keep the cause of a failed read in the timing record (only when tracing is on). A body that is decoded has its record inside, and its raw body notes the error.
    fn note(&mut self, e: &(dyn std::error::Error + 'static)) {
        let rec = match &mut self.0 {
            Source::Live(live) => &mut live.rec,
            Source::H2(stream) => &mut stream.rec,
            Source::Tap(tap) => return tap.inner.note(e),
            _ => return,
        };
        if let Some(r) = rec {
            r.error.get_or_insert_with(|| cause::describe(e));
        }
    }

    fn read_body(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let live = match &mut self.0 {
            Source::Memory(response, at) => {
                let n = out.len().min(response.body.len() - *at);
                out[..n].copy_from_slice(&response.body[*at..*at + n]);
                *at += n;
                return Ok(n);
            }
            Source::Live(live) => live,
            Source::Decoded(decoded) => return decoded.read(out),
            Source::H2(stream) => return stream.read(out),
            Source::Tap(tap) => return tap.read(out),
            Source::Cut(bytes) => {
                return match bytes.read(out)? {
                    0 if !out.is_empty() => Err(io::Error::new(ErrorKind::TimedOut, "the recorded reply stops here")),
                    n => Ok(n),
                };
            }
        };
        if live.done || out.is_empty() {
            return Ok(0);
        }
        if live.limited && live.given > MAX_REPLY {
            return Err(io::Error::new(ErrorKind::InvalidData, "the reply is too big"));
        }
        if live.limited && live.start.elapsed() > MAX_TIME {
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
    /// The whole decoded body of a gzip or deflate reply, in one step (see `encoding::Decoded::read_all`). `None` for any other body, or one that
    /// was read from already: read it with `Read`.
    pub fn decoded_whole(&mut self, max: usize, deadline: Instant) -> Option<io::Result<Vec<u8>>> {
        match &mut self.0 {
            Source::Decoded(decoded) => decoded.read_all(max, deadline),
            _ => None,
        }
    }

    /// Lift `MAX_REPLY` and `MAX_TIME` from this body: the caller writes it to a file and sets its own cap. The 30 s limit of each read
    /// stays. A decoded body keeps its caps: they guard against a compression bomb.
    pub fn unlimited(&mut self) {
        match &mut self.0 {
            Source::Live(live) => live.limited = false,
            Source::H2(stream) => stream.unlimited(),
            Source::Memory(..) | Source::Decoded(_) => {}
        }
    }

    /// How many bytes the body will have, when the server said so (up to the limit). Used to make room at once.
    fn size_hint(&self) -> usize {
        match &self.0 {
            Source::Live(live) => match live.framing {
                Framing::Length(n) => n.min(MAX_REPLY),
                _ => 0,
            },
            Source::Memory(response, _) => response.body.len(),
            Source::H2(stream) => stream.declared.unwrap_or(0).min(MAX_REPLY),
            // The length on the wire is not the length of the decoded body.
            Source::Decoded(_) | Source::Cut(_) => 0,
            Source::Tap(tap) => tap.inner.size_hint(),
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
    if url.port == url.scheme.default_port() { host } else { format!("{host}:{}", url.port) }
    // The port is sent only when it is not the default for the scheme: 80 for http, 443 for https.
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
        let mut body = Body(Source::Live(Box::new(Live::new(stream, leftover, head.framing, None, Instant::now()))));
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
    fn asks_for_compression_except_on_this_machine_and_br_only_over_https() {
        let ask = |input, headers: &[(&str, &str)]| {
            let plan = Plan { method: Method::Get, headers: Cow::Borrowed(headers), body: b"", cookies: Default::default(), ident: None };
            String::from_utf8(request_head(&Url::parse(input).unwrap(), &plan, "", 0)).unwrap()
        };
        assert!(ask("https://a.com/", &[]).contains("\r\nAccept-Encoding: gzip, deflate, br\r\n"));
        assert!(ask("http://a.com/", &[]).contains("\r\nAccept-Encoding: gzip, deflate\r\n"));
        // A name that only starts with 127. is a remote host.
        assert!(ask("http://127.example.com/", &[]).contains("ccept-Encoding"));
        for local in ["http://127.0.0.1:80/", "https://127.9.9.9/", "http://localhost/", "http://[::1]:8080/", "http://[0:0:0:0:0:0:0:1]/", "http://0x7f.1/"] {
            assert!(!ask(local, &[]).contains("ccept-"), "{local}");
        }
        // The caller's header is sent once, as it is, on any host, and ours is not added.
        for input in ["http://a.com/", "http://127.0.0.1/"] {
            let head = ask(input, &[("accept-ENCODING", "identity")]);
            let lower = head.to_ascii_lowercase();
            assert_eq!(lower.matches("accept-encoding").count(), 1);
            assert!(head.contains("\r\naccept-ENCODING: identity\r\n"));
        }
    }

    #[test]
    fn asks_for_english_except_on_this_machine_or_when_the_caller_sets_it() {
        let plan = |headers: &'static [(&'static str, &'static str)]| Plan { method: Method::Get, headers: Cow::Borrowed(headers), body: b"", cookies: Default::default(), ident: None };
        let ask = |input, headers: &'static [(&'static str, &'static str)]| String::from_utf8(request_head(&Url::parse(input).unwrap(), &plan(headers), "", 0)).unwrap();
        for remote in ["https://a.com/", "http://a.com/"] {
            assert!(ask(remote, &[]).contains("\r\nAccept-Language: en-US,en;q=0.9\r\n"), "{remote}");
        }
        assert!(!ask("http://127.0.0.1/", &[]).contains("ccept-Language"));
        let head = ask("https://a.com/", &[("accept-LANGUAGE", "fr")]);
        assert_eq!(head.to_ascii_lowercase().matches("accept-language").count(), 1);
        assert!(head.contains("\r\naccept-LANGUAGE: fr\r\n"));
        let url = Url::parse("https://a.com/").unwrap();
        let own = plan(&[]);
        let h2 = h2_headers(&url, &own, "", "a.com", "0");
        assert!(h2.contains(&("accept-language", "en-US,en;q=0.9")), "{h2:?}");
        let theirs = plan(&[("Accept-Language", "fr")]);
        let h2 = h2_headers(&url, &theirs, "", "a.com", "0");
        assert_eq!(h2.iter().filter(|(n, _)| n.eq_ignore_ascii_case("accept-language")).count(), 1);
    }

    #[test]
    fn the_headers_of_an_h2_request_have_the_pseudo_headers_first_and_no_host() {
        let url = Url::parse("https://example.com:8443/a/b?q=1").unwrap();
        let plan = Plan { method: Method::Post, headers: Cow::Owned(vec![("X-Token", "t")]), body: b"abc", cookies: Default::default(), ident: None };
        let h = h2_headers(&url, &plan, "If-None-Match: \"v1\"\r\nIf-Modified-Since: yesterday\r\n", "example.com:8443", "3");
        assert_eq!(
            h,
            [
                (":method", "POST"),
                (":scheme", "https"),
                (":authority", "example.com:8443"),
                (":path", "/a/b?q=1"),
                ("user-agent", "bare/0.1"),
                ("accept-encoding", "gzip, deflate, br"),
                ("accept-language", "en-US,en;q=0.9"),
                ("X-Token", "t"),
                ("content-length", "3"),
                ("If-None-Match", "\"v1\""),
                ("If-Modified-Since", "yesterday"),
            ]
        );
        // A header of the caller replaces the default. A GET has no content-length. A loopback host asks for no coding.
        let url = Url::parse("https://localhost/").unwrap();
        let plan = Plan { method: Method::Get, headers: Cow::Owned(vec![("User-Agent", "mine")]), body: b"", cookies: Default::default(), ident: None };
        assert_eq!(h2_headers(&url, &plan, "", "localhost", "0")[4..], [("User-Agent", "mine")]);
    }

    #[test]
    fn a_browser_identity_gives_the_user_agent_and_the_hints_of_https_requests() {
        let ident = Some(Arc::new(Identity { user_agent: "UA/1", hints: vec![("sec-ch-ua", "\"A\";v=\"1\"".into()), ("sec-ch-ua-mobile", "?0".into())] }));
        let plan = |headers: &'static [(&'static str, &'static str)], ident: Option<Arc<Identity>>| Plan { method: Method::Get, headers: Cow::Borrowed(headers), body: b"", cookies: Default::default(), ident };
        let text = |url: &str, plan: &Plan| String::from_utf8(request_head(&Url::parse(url).unwrap(), plan, "", 0)).unwrap();
        // No identity: `bare/0.1` and no hints.
        assert!(text("https://a.com/", &plan(&[], None)).contains("User-Agent: bare/0.1\r\n"));
        // The identity: its user agent on http and https; its hints on https only.
        assert!(text("http://a.com/", &plan(&[], ident.clone())).contains("User-Agent: UA/1\r\n"));
        assert!(!text("http://a.com/", &plan(&[], ident.clone())).contains("sec-ch-ua"));
        assert!(text("https://a.com/", &plan(&[], ident.clone())).contains("\r\nsec-ch-ua: \"A\";v=\"1\"\r\nsec-ch-ua-mobile: ?0\r\n"));
        // A header of the caller replaces its hint, and its own user agent replaces ours.
        let theirs = text("https://a.com/", &plan(&[("sec-ch-ua-mobile", "?1"), ("User-Agent", "mine")], ident.clone()));
        assert!(theirs.contains("\r\nsec-ch-ua: \"A\";v=\"1\"\r\n") && theirs.contains("sec-ch-ua-mobile: ?1\r\n") && theirs.contains("User-Agent: mine\r\n"));
        assert!(!theirs.contains("UA/1") && !theirs.contains("?0"));
        // HTTP/2 sends the same values, with lower-case names.
        let (https, http) = (Url::parse("https://a.com/").unwrap(), Url::parse("http://a.com/").unwrap());
        let (https_plan, http_plan) = (plan(&[], ident.clone()), plan(&[], ident.clone()));
        let h2 = h2_headers(&https, &https_plan, "", "a.com", "0");
        assert!(h2.contains(&("user-agent", "UA/1")) && h2.contains(&("sec-ch-ua-mobile", "?0")), "{h2:?}");
        assert!(!h2_headers(&http, &http_plan, "", "a.com", "0").iter().any(|(n, _)| n.starts_with("sec-")));
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
        assert_eq!(header("https://a.com/"), "a.com");
        assert_eq!(header("https://a.com:443/"), "a.com");
        // The default port of https is 443, so it is left out. Port 80 is sent for https.
        assert_eq!(header("https://a.com:8443/"), "a.com:8443");
        assert_eq!(header("http://a.com:443/"), "a.com:443");
        assert_eq!(header("https://a.com:80/"), "a.com:80");
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
                    // Read one request, up to its blank line. A request with `Content-Length` has a body: read it too, so that
                    // the next request on this connection starts in the right place.
                    let head = String::from_utf8_lossy(&seen).to_ascii_lowercase();
                    if let Some(n) = head.split("content-length: ").nth(1).and_then(|r| r.split("\r\n").next()).and_then(|n| n.parse::<usize>().ok()) {
                        let mut body = vec![0u8; n];
                        stream.read_exact(&mut body).unwrap();
                        seen.extend_from_slice(&body);
                    }
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

    /// True if nothing connected to this listener. A client connects before it sends anything, and
    /// the connection waits in the listener's queue, so a check right after the call is enough.
    fn nothing_connects(listener: &std::net::TcpListener) -> bool {
        listener.set_nonblocking(true).unwrap();
        listener.accept().is_err()
    }

    /// A peer that is not trusted: it takes one connection, keeps what the client sends until the client closes
    /// (or 1 s passes), answers a plain text reply that a real server would never send as its first bytes, and
    /// closes. Gives the port and a channel with the bytes it received.
    fn untrusted() -> (u16, std::sync::mpsc::Receiver<Vec<u8>>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
            let mut got = Vec::new();
            let mut buf = [0u8; 4096];
            let n = stream.read(&mut buf).unwrap_or(0);
            got.extend_from_slice(&buf[..n]);
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\nSECRET");
            while let Ok(n) = stream.read(&mut buf) {
                if n == 0 {
                    break;
                }
                got.extend_from_slice(&buf[..n]);
            }
            let _ = tx.send(got);
        });
        (port, rx)
    }

    fn tls_url(port: u16, path: &str) -> Url {
        // `localhost`, not an address: the TLS client sends a host name and does not support IP hosts.
        Url::parse(&format!("https://localhost:{port}{path}")).unwrap()
    }

    /// The client sent a TLS handshake record (type 22, version 3.x) and nothing else readable: no request line, no header, no body.
    fn only_a_client_hello(wire: &[u8]) {
        assert!(wire.len() > 5 && wire[0] == 22 && wire[1] == 3, "not a TLS record: {:?}", &wire[..wire.len().min(16)]);
        let text = String::from_utf8_lossy(wire);
        for secret in ["GET ", "POST ", "HTTP/1.1", "Authorization", "s3cret", "hunter2", "Cookie"] {
            assert!(!text.contains(secret), "plain text {secret:?} reached an untrusted peer");
        }
    }

    #[test]
    fn https_sends_tls_records_and_no_plain_request_to_an_untrusted_peer() {
        let secret = [("Authorization", "Bearer s3cret")];
        let (port, wire) = untrusted();
        let error = Client::new().get(&tls_url(port, "/")).err().unwrap();
        // The peer's plain text reply is not a TLS record: the handshake fails, and the reply is not a page.
        assert!(!error.to_string().contains("SECRET"));
        only_a_client_hello(&wire.recv().unwrap());
        let (port, wire) = untrusted();
        assert!(Client::new().get_stream(&tls_url(port, "/")).is_err());
        only_a_client_hello(&wire.recv().unwrap());
        let (port, wire) = untrusted();
        let error = Client::new().post(&tls_url(port, "/login"), &secret, b"pw=hunter2").err().unwrap().to_string();
        assert!(!error.contains("s3cret") && !error.contains("hunter2"), "{error}");
        only_a_client_hello(&wire.recv().unwrap());
        let (port, wire) = untrusted();
        assert!(Client::new().send_stream(&Request::post(&tls_url(port, "/login"), &secret, b"pw=hunter2")).is_err());
        only_a_client_hello(&wire.recv().unwrap());
    }

    #[test]
    fn an_https_redirect_from_a_plain_server_opens_tls_and_sends_no_plain_text() {
        let (target, wire) = untrusted();
        let moved = format!("HTTP/1.1 302 Found\r\nLocation: https://localhost:{target}/\r\nContent-Length: 0\r\n\r\n");
        let (server, _) = serve(vec![(vec![Box::leak(moved.into_boxed_str())], false)]);
        assert!(Client::new().get(&url(server)).is_err());
        only_a_client_hello(&wire.recv().unwrap());
    }

    #[test]
    fn https_never_reuses_a_plain_connection() {
        // A plain connection to this host and port waits in the pool. The pool is keyed by scheme, so https opens its own
        // connection and starts a handshake. The idle plain connection gets no byte.
        let idle = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let (port, wire) = untrusted();
        let mut client = Client::new();
        let plain = TcpStream::connect(idle.local_addr().unwrap()).unwrap();
        client.idle.lock().unwrap().insert((Scheme::Http, "localhost".to_string(), u32::from(port)), vec![(Stream::Tcp(plain), Instant::now())]);
        assert!(client.get(&tls_url(port, "/")).is_err());
        only_a_client_hello(&wire.recv().unwrap());
        let (mut held, _) = idle.accept().unwrap();
        held.set_read_timeout(Some(Duration::from_millis(100))).unwrap();
        assert!(held.read(&mut [0u8; 1]).is_err(), "bytes reached the idle plain connection");
        assert!(client.idle.lock().unwrap().contains_key(&(Scheme::Http, "localhost".to_string(), u32::from(port))));
    }

    #[test]
    fn https_never_gets_a_cached_plain_reply() {
        // A fresh plain copy must not answer an https request for the same host, port and path.
        for stream in [false, true] {
            let (port, wire) = untrusted();
            let mut client = Client::new();
            let plain = Arc::new(Response { status: 200, body: b"plain".to_vec(), ..Response::default() });
            client.cache.lock().unwrap().map.insert((Scheme::Http, "localhost".to_string(), port, "/".to_string()), Cached { response: plain, stored: Instant::now(), fresh_for: Duration::from_secs(60), owner: 0 });
            let url = tls_url(port, "/");
            assert!(if stream { client.get_stream(&url).is_err() } else { client.get(&url).is_err() });
            only_a_client_hello(&wire.recv().unwrap());
        }
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
    fn send_once_accepts_the_maximum_body_cap() {
        let (port, _) = serve(vec![(vec!["HTTP/1.1 200 OK\r\nContent-Length: 1\r\n\r\nx"], false)]);
        assert_eq!(Client::new().send_once(&Request::get(&url(port)), usize::MAX).unwrap().body, b"x");
    }

    #[test]
    fn send_once_returns_a_redirect_and_sends_no_second_request() {
        let (port, requests) = serve(vec![(vec!["HTTP/1.1 307 Temporary Redirect\r\nLocation: http://127.0.0.1:9/x\r\nContent-Length: 0\r\n\r\n"], false)]);
        let body = b"{}";
        let response = Client::new().send_once(&Request::post(&url(port), &[("Authorization", "Bearer s3cret")], body), 1024).unwrap();
        assert_eq!((response.status, response.header("location")), (307, Some("http://127.0.0.1:9/x")));
        assert!(requests.recv().unwrap().starts_with("POST / HTTP/1.1"));
        assert!(requests.recv_timeout(Duration::from_millis(100)).is_err(), "a second request was sent");
    }

    #[test]
    fn send_once_caps_the_body_before_and_while_it_reads() {
        let (port, _) = serve(vec![(vec!["HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\n0123456789"], false)]);
        let r = Client::new().send_once(&Request::get(&url(port)), 10).unwrap();
        assert_eq!(r.body, b"0123456789");
        let (port, _) = serve(vec![(vec!["HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\n0123456789"], false)]);
        assert_eq!(Client::new().send_once(&Request::get(&url(port)), 9).err().unwrap().to_string(), "the reply is too big");
        let chunked = "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n5\r\nworld\r\n0\r\n\r\n";
        let (port, _) = serve(vec![(vec![chunked], false)]);
        assert_eq!(Client::new().send_once(&Request::get(&url(port)), 9).err().unwrap().to_string(), "the reply is too big");
        let (port, _) = serve(vec![(vec![chunked], false)]);
        assert_eq!(Client::new().send_once(&Request::get(&url(port)), 10).unwrap().body, b"helloworld");
    }

    #[test]
    fn send_once_checks_the_request_before_any_socket() {
        let u = Url::parse("http://127.0.0.1:9/").unwrap();
        let e = Client::new().send_once(&Request::post(&u, &[("Host", "evil")], b"x"), 10).err().unwrap().to_string();
        assert!(e.contains("sets the Host header itself"), "{e}");
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

    /// A script that a worker streamed and kept answers the next document of the session: the fork that asks again sends nothing.
    #[test]
    fn a_kept_stream_answers_a_fork_without_a_request() {
        let (port, requests) = serve(vec![(vec![page("Cache-Control: public, max-age=31536000, immutable\r\n", "js"), page("", "b")], false)]);
        let (client, u) = (Client::new(), url(port));
        let mut worker = client.fork();
        let mut s = worker.send_stream_cached(&Request::get(&u)).unwrap();
        let mut body = Vec::new();
        s.body.read_to_end(&mut body).unwrap();
        worker.keep(&u, &s.response, &body);
        let mut again = client.fork().send_stream_cached(&Request::get(&u)).unwrap();
        let mut body = Vec::new();
        again.body.read_to_end(&mut body).unwrap();
        assert_eq!(body, b"js");
        assert_eq!(requests.try_iter().count(), 1);
        // A copy that is not fresh is not kept: the next request goes to the server.
        let (port, requests) = serve(vec![(vec![page("Cache-Control: no-cache\r\nETag: \"1\"\r\n", "a"), page("", "b")], false)]);
        let u = url(port);
        let mut c = Client::new();
        let mut s = c.send_stream_cached(&Request::get(&u)).unwrap();
        let mut body = Vec::new();
        s.body.read_to_end(&mut body).unwrap();
        c.keep(&u, &s.response, &body);
        let mut s = c.send_stream_cached(&Request::get(&u)).unwrap();
        let mut body = Vec::new();
        s.body.read_to_end(&mut body).unwrap();
        assert_eq!(body, b"b");
        assert_eq!(requests.try_iter().count(), 2);
    }

    /// Read a streamed reply to its end, give it to `keep`, and return the body and the reply.
    fn stream_kept(client: &mut Client, u: &Url) -> (Vec<u8>, Arc<Response>) {
        let mut s = client.send_stream_cached(&Request::get(u)).unwrap();
        let mut body = Vec::new();
        s.body.read_to_end(&mut body).unwrap();
        client.keep(u, &s.response, &body);
        (body, s.response)
    }

    /// A second client on the same connections with a cookie jar of its own: another profile.
    fn other_profile(client: &Client) -> Client {
        let mut other = client.fork();
        other.use_jar(Arc::new(Mutex::new(Jar::default())));
        other
    }

    /// A reply that is not `public` or `immutable` can depend on the cookies of the jar that asked: it answers that jar only.
    /// A reply that says `public` or `immutable` answers every jar.
    #[test]
    fn a_reply_that_is_not_public_answers_only_the_jar_that_stored_it() {
        let private = ["Cache-Control: private, max-age=60\r\n", "Cache-Control: max-age=60\r\n"];
        for headers in private {
            // Stream path (`keep`): jar A stores, jar B asks, then jar A asks again.
            let (port, requests) = serve(vec![(vec![page(headers, "for a"), page(headers, "for b")], false)]);
            let (mut a, u) = (Client::new(), url(port));
            let mut b = other_profile(&a);
            assert_eq!(stream_kept(&mut a, &u).0, b"for a");
            assert_eq!(stream_kept(&mut a.fork(), &u).0, b"for a", "the same jar uses its copy");
            assert_eq!(requests.try_iter().count(), 1);
            assert_eq!(stream_kept(&mut b, &u).0, b"for b", "another jar must not get the copy of jar A");
            assert_eq!(requests.try_iter().count(), 1);
            // The same through `get`.
            let (port, requests) = serve(vec![(vec![page(headers, "for a"), page(headers, "for b")], false)]);
            let (mut a, u) = (Client::new(), url(port));
            let mut b = other_profile(&a);
            assert_eq!(a.get(&u).unwrap().body, b"for a");
            assert_eq!(a.get(&u).unwrap().body, b"for a");
            assert_eq!(b.get(&u).unwrap().body, b"for b");
            assert_eq!(requests.try_iter().count(), 2);
        }
        // A private reply with a validator is asked about, not used: the server decides for the cookies of the jar that asks.
        let not_modified = "HTTP/1.1 304 Not Modified\r\nETag: \"v1\"\r\nCache-Control: private, max-age=60\r\n\r\n";
        let (port, requests) = serve(vec![(vec![page("Cache-Control: private, max-age=60\r\nETag: \"v1\"\r\n", "for a"), not_modified], false)]);
        let (mut a, u) = (Client::new(), url(port));
        let mut b = other_profile(&a);
        assert_eq!(stream_kept(&mut a, &u).0, b"for a");
        assert_eq!(stream_kept(&mut b, &u).0, b"for a", "the server said 304 to the jar of b");
        let seen = seen(&requests);
        assert!(seen.len() == 2 && seen[1].contains("If-None-Match: \"v1\"\r\n"), "{seen:?}");
        for headers in ["Cache-Control: public, max-age=60\r\n", "Cache-Control: max-age=31536000, immutable\r\n"] {
            let (port, requests) = serve(vec![(vec![page(headers, "shared"), page(headers, "again")], false)]);
            let (mut a, u) = (Client::new(), url(port));
            let mut b = other_profile(&a);
            assert_eq!(stream_kept(&mut a, &u).0, b"shared");
            assert_eq!(stream_kept(&mut b, &u).0, b"shared", "a public reply is shared");
            assert_eq!(b.get(&u).unwrap().body, b"shared");
            assert_eq!(requests.try_iter().count(), 1);
        }
    }

    /// A stale script with an `ETag` and a `Last-Modified` is asked about with a conditional request. A 304 gives the stored body,
    /// and its headers replace the stored ones (a new `max-age`), so the next use is fresh and sends nothing.
    #[test]
    fn a_stale_stream_copy_is_revalidated_and_a_304_keeps_the_body() {
        let first = page("Cache-Control: max-age=0\r\nETag: \"v1\"\r\nLast-Modified: Mon, 01 Jan 2024 00:00:00 GMT\r\n", "script");
        let not_modified = "HTTP/1.1 304 Not Modified\r\nETag: \"v1\"\r\nCache-Control: max-age=3600\r\n\r\n";
        let (port, requests) = serve(vec![(vec![first, not_modified], false)]);
        let (mut client, u) = (Client::new(), url(port));
        assert_eq!(stream_kept(&mut client, &u).0, b"script");
        let (body, response) = stream_kept(&mut client.fork(), &u);
        assert_eq!((body.as_slice(), response.status, response.header("cache-control")), (b"script".as_slice(), 200, Some("max-age=3600")));
        let seen = seen(&requests);
        assert_eq!(seen.len(), 2);
        assert!(!seen[0].contains("If-None-Match"));
        assert!(seen[1].contains("If-None-Match: \"v1\"\r\n") && seen[1].contains("If-Modified-Since: Mon, 01 Jan 2024 00:00:00 GMT\r\n"), "{}", seen[1]);
        // The 304 made the copy fresh for an hour: no third request.
        assert_eq!(stream_kept(&mut client, &u).0, b"script");
        assert_eq!(requests.try_iter().count(), 0);
    }

    /// A 200 after a conditional request is a new copy: it replaces the stored one, and the next question names its `ETag`.
    #[test]
    fn a_changed_stream_copy_replaces_the_stored_one() {
        let (port, requests) = serve(vec![(vec![page("ETag: \"v1\"\r\n", "a"), page("ETag: \"v2\"\r\n", "b"), "HTTP/1.1 304 Not Modified\r\n\r\n"], false)]);
        let (mut client, u) = (Client::new(), url(port));
        assert_eq!(stream_kept(&mut client, &u).0, b"a");
        assert_eq!(stream_kept(&mut client, &u).0, b"b");
        assert_eq!(stream_kept(&mut client, &u).0, b"b");
        let seen = seen(&requests);
        assert!(seen[1].contains("If-None-Match: \"v1\"\r\n") && seen[2].contains("If-None-Match: \"v2\"\r\n"), "{seen:?}");
    }

    /// A stale copy with no validator cannot be asked about: it is requested whole, and a reply that is not cacheable removes it.
    #[test]
    fn a_stream_reply_that_cannot_be_kept_removes_the_stale_copy() {
        let (port, requests) = serve(vec![(vec![page("ETag: \"v1\"\r\n", "a"), page("Cache-Control: no-store\r\n", "b"), page("", "c")], false)]);
        let (mut client, u) = (Client::new(), url(port));
        assert_eq!(stream_kept(&mut client, &u).0, b"a");
        assert_eq!(stream_kept(&mut client, &u).0, b"b");
        assert_eq!(stream_kept(&mut client, &u).0, b"c");
        assert!(!seen(&requests)[2].contains("If-None-Match"));
    }

    /// The 304 of a whole-body request updates the headers of the stored copy too (RFC 9111 section 4.3.4).
    #[test]
    fn a_304_updates_the_stored_headers_of_a_whole_reply() {
        let first = page("Cache-Control: max-age=0\r\nETag: \"v1\"\r\n", "a");
        let (port, requests) = serve(vec![(vec![first, "HTTP/1.1 304 Not Modified\r\nETag: \"v1\"\r\nCache-Control: max-age=3600\r\n\r\n"], false)]);
        let (mut client, u) = (Client::new(), url(port));
        client.get(&u).unwrap();
        let again = client.get(&u).unwrap();
        assert_eq!((again.body.as_slice(), again.header("cache-control")), (b"a".as_slice(), Some("max-age=3600")));
        assert_eq!(client.get(&u).unwrap().header("cache-control"), Some("max-age=3600"));
        assert_eq!(requests.try_iter().count(), 2);
    }

    /// The cache holds `MAX_CACHE` at most. A reply that does not fit empties it, and an evicted reply is freed.
    #[test]
    fn the_cache_stays_under_its_cap_and_frees_what_it_drops() {
        let key = |n: usize| (Scheme::Http, "h".to_string(), 80, format!("/{n}"));
        let reply = |n: usize| Arc::new(Response { status: 200, body: vec![0; n], ..Response::default() });
        let mut cache = Cache::default();
        let first = reply(MAX_CACHE / 2);
        let weak = Arc::downgrade(&first);
        cache.put(key(1), first, Duration::from_secs(60), 0);
        cache.put(key(2), reply(MAX_CACHE / 4), Duration::from_secs(60), 0);
        assert!(cache.bytes <= MAX_CACHE && cache.map.len() == 2);
        assert!(weak.upgrade().is_some());
        // 1/2 + 1/4 + 1/2 does not fit: the cache empties and holds the new reply alone. Nothing else holds the first one.
        cache.put(key(3), reply(MAX_CACHE / 2), Duration::from_secs(60), 0);
        assert_eq!((cache.map.len(), cache.bytes), (1, MAX_CACHE / 2 + 512));
        assert!(weak.upgrade().is_none(), "the evicted reply is still in memory");
        // A reply over the cap is not kept, and it does not empty the cache.
        cache.put(key(4), reply(MAX_CACHE), Duration::from_secs(60), 0);
        assert_eq!((cache.map.len(), cache.bytes), (1, MAX_CACHE / 2 + 512));
        // The same key twice counts once.
        cache.put(key(3), reply(10), Duration::from_secs(60), 0);
        assert_eq!((cache.map.len(), cache.bytes), (1, 10 + 512));
        cache.forget(&key(3));
        assert_eq!((cache.map.len(), cache.bytes), (0, 0));
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
        client.cache.lock().unwrap().bytes = MAX_CACHE - 100;
        // The cache says it is nearly full. The next reply does not fit.
        let (port, _requests) = serve(vec![(vec![page("Cache-Control: max-age=60\r\n", "a")], false)]);
        client.get(&url(port)).unwrap();
        assert_eq!(client.cache.lock().unwrap().map.len(), 1);
        assert_eq!(client.cache.lock().unwrap().bytes, 1 + 512);
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

    // ---- Requests with headers and a body: POST, redirects, no retry, cache and pool keys ----

    const JSON: &[(&str, &str)] = &[("Content-Type", "application/json"), ("Authorization", "Bearer secret-token")];

    fn leak(text: String) -> &'static str {
        Box::leak(text.into_boxed_str())
    }

    fn redirect(code: u16, location: &str) -> &'static str {
        leak(format!("HTTP/1.1 {code} Moved\r\nLocation: {location}\r\nContent-Length: 0\r\n\r\n"))
    }

    fn post_url(port: u16, path: &str) -> Url {
        Url::parse(&format!("http://127.0.0.1:{port}{path}")).unwrap()
    }

    /// Take what the server saw. No wait is needed: `serve` sends each request before it writes the reply, so every
    /// request that a client has an answer for is already in the channel.
    fn seen(requests: &std::sync::mpsc::Receiver<String>) -> Vec<String> {
        requests.try_iter().collect()
    }

    #[test]
    fn posts_exact_bytes() {
        let (port, requests) = serve(vec![(vec![OK], false)]);
        let body = br#"{"email":"a@b.c","password":"p"}"#;
        let r = Client::new().post(&post_url(port, "/login?x=1"), JSON, body).unwrap();
        assert_eq!((r.status, r.body.as_slice()), (200, b"ok".as_slice()));
        let want = format!(
            "POST /login?x=1 HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nUser-Agent: bare/0.1\r\nContent-Type: application/json\r\nAuthorization: Bearer secret-token\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            String::from_utf8_lossy(body)
        );
        assert_eq!(seen(&requests), vec![want]);
    }

    #[test]
    fn a_post_without_a_body_says_content_length_0() {
        let (port, requests) = serve(vec![(vec![OK], false)]);
        Client::new().post(&post_url(port, "/"), &[], b"").unwrap();
        assert!(seen(&requests)[0].ends_with("Content-Length: 0\r\n\r\n"));
    }

    #[test]
    fn a_get_with_headers_has_no_content_length() {
        let (port, requests) = serve(vec![(vec![OK], false)]);
        let url = post_url(port, "/me");
        let req = Request { method: Method::Get, url: &url, headers: &[("Authorization", "Bearer t"), ("User-Agent", "other/1")], body: b"", cookies: Default::default() };
        Client::new().send(&req).unwrap();
        let want = format!("GET /me HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer t\r\nUser-Agent: other/1\r\n\r\n");
        assert_eq!(seen(&requests), vec![want]);
        // The caller's `User-Agent` replaces ours. It does not come twice.
    }

    #[test]
    fn a_body_at_the_inline_limit_and_past_it_arrives_whole() {
        for size in [INLINE_BODY - 1, INLINE_BODY, INLINE_BODY + 1, 3 * 1024 * 1024] {
            let (port, requests) = serve(vec![(vec![OK], false)]);
            let body: Vec<u8> = (0..size).map(|i| b'a' + (i % 26) as u8).collect();
            Client::new().post(&post_url(port, "/"), &[], &body).unwrap();
            let got = seen(&requests).remove(0);
            let (head, sent) = got.split_once("\r\n\r\n").unwrap();
            assert!(head.ends_with(&format!("Content-Length: {size}")));
            assert_eq!(sent.as_bytes(), body.as_slice(), "size {size}");
        }
    }

    #[test]
    fn bad_headers_never_reach_a_connection() {
        let target = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = post_url(target.local_addr().unwrap().port(), "/");
        let bad_names = ["", "a b", "a:b", "a\r\nX: y", "a\nb", "a\0b", "\u{e9}", "a(b", "a\"b"];
        let bad_values = ["a\r\nX-Injected: 1", "a\nb", "a\rb", "a\0b", "a\x01", "a\x7f", " lead", "trail ", "\tx"];
        let managed = ["Host", "content-length", "Transfer-Encoding", "CONNECTION", "Keep-Alive", "Proxy-Connection", "TE", "Trailer", "Upgrade", "Expect"];
        let mut client = Client::new();
        for name in bad_names {
            assert!(client.post(&url, &[(name, "v")], b"x").is_err(), "name {name:?}");
        }
        for value in bad_values {
            let error = client.post(&url, &[("X-A", value)], b"x").err().unwrap().to_string();
            assert!(!error.contains(value.trim()) || value.trim().is_empty(), "{error}");
            assert!(client.send_stream(&Request::post(&url, &[("X-A", value)], b"x")).is_err());
        }
        for name in managed {
            assert!(client.post(&url, &[(name, "1")], b"x").is_err(), "{name}");
        }
        assert!(client.send(&Request { method: Method::Get, url: &url, headers: &[], body: b"x", cookies: Default::default() }).is_err());
        assert!(nothing_connects(&target));
        // Each check failed before DNS and before a socket. Valid names with all the token marks pass:
        assert!(check_request(&Request::post(&url, &[("X-A_b.c~`|^$%&'*+!#-9", "tab\tinside and \u{e9}")], b"")).is_ok());
    }

    #[test]
    fn an_error_does_not_show_a_secret_value() {
        let url = post_url(1, "/");
        let error = Client::new().post(&url, &[("Authorization", "Bearer s3cret\r\nX: y")], b"pw=hunter2").err().unwrap().to_string();
        assert!(!error.contains("s3cret") && !error.contains("hunter2"), "{error}");
        let (port, _) = serve(vec![(vec!["HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nabc"], true)]);
        let error = Client::new().post(&post_url(port, "/"), &[("Authorization", "Bearer s3cret")], b"pw=hunter2").err().unwrap().to_string();
        assert!(!error.contains("s3cret") && !error.contains("hunter2"), "{error}");
    }

    #[test]
    fn redirects_turn_a_post_into_a_get_like_fetch() {
        // 301 and 302 (POST only) and 303: GET, no body, no body headers. The `Authorization` of the same origin stays.
        for code in [301, 302, 303] {
            let (port, requests) = serve(vec![(vec![redirect(code, "/next"), OK], false)]);
            let hdrs = [("Content-Type", "application/json"), ("authorization", "A"), ("X-Keep", "1"), ("content-LANGUAGE", "en")];
            assert_eq!(Client::new().post(&post_url(port, "/login"), &hdrs, b"{\"p\":1}").unwrap().body, b"ok");
            let seen = seen(&requests);
            assert!(seen[0].starts_with("POST /login ") && seen[0].ends_with("{\"p\":1}"), "{code}");
            let want = format!("GET /next HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nUser-Agent: bare/0.1\r\nauthorization: A\r\nX-Keep: 1\r\n\r\n");
            assert_eq!(seen[1], want, "status {code}");
        }
    }

    #[test]
    fn redirects_307_and_308_keep_the_method_and_the_body() {
        for code in [307, 308] {
            let (port, requests) = serve(vec![(vec![redirect(code, "/next"), OK], false)]);
            Client::new().post(&post_url(port, "/login"), JSON, b"{\"p\":1}").unwrap();
            let seen = seen(&requests);
            let want = format!(
                "POST /next HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nUser-Agent: bare/0.1\r\nContent-Type: application/json\r\nAuthorization: Bearer secret-token\r\nContent-Length: 7\r\n\r\n{{\"p\":1}}"
            );
            assert_eq!(seen[1], want, "status {code}");
            // The body is sent again from the same slice, and the connection is used again.
        }
    }

    fn with<'a>(method: Method, url: &'a Url, body: &'a [u8]) -> Request<'a> {
        Request { method, url, headers: &[("X-A", "1")], body, cookies: Default::default() }
    }

    #[test]
    fn each_method_sends_its_exact_bytes() {
        // (method, body, the wire line, whether Content-Length is sent). Chrome sends the same: a length for POST and PUT even with
        // no body, a length for any method with a body, none for the rest (tools/fixtures/ui/methods-expected.txt).
        let cases: [(Method, &[u8], &str, Option<usize>); 11] = [
            (Method::Put, b"", "PUT", Some(0)),
            (Method::Put, b"abc", "PUT", Some(3)),
            (Method::Post, b"", "POST", Some(0)),
            (Method::Patch, b"", "PATCH", None),
            (Method::Patch, b"abc", "PATCH", Some(3)),
            (Method::Delete, b"", "DELETE", None),
            (Method::Delete, b"abc", "DELETE", Some(3)),
            (Method::Options, b"", "OPTIONS", None),
            (Method::Options, b"abc", "OPTIONS", Some(3)),
            (Method::Head, b"", "HEAD", None),
            (Method::Get, b"", "GET", None),
        ];
        for (method, body, line, length) in cases {
            let (port, requests) = serve(vec![(vec![OK], false)]);
            let url = post_url(port, "/r");
            Client::new().send(&with(method, &url, body)).unwrap();
            let cl = length.map(|n| format!("Content-Length: {n}\r\n")).unwrap_or_default();
            let want = format!("{line} /r HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nUser-Agent: bare/0.1\r\nX-A: 1\r\n{cl}\r\n{}", String::from_utf8_lossy(body));
            assert_eq!(seen(&requests)[0], want, "{line}");
        }
    }

    #[test]
    fn a_get_or_head_with_a_body_is_refused() {
        for method in [Method::Get, Method::Head] {
            assert!(Client::new().send(&with(method, &post_url(1, "/"), b"x")).is_err());
        }
    }

    #[test]
    fn a_head_reply_has_no_body_and_the_connection_stays_in_step() {
        // The server says `Content-Length: 5` and sends no body. One connection only: a second one would never be answered.
        // The 204 and 304 replies carry a `Content-Length` that must not be read either.
        let head = "HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\n";
        let (port, requests) = serve(vec![(
            vec![head, OK, "HTTP/1.1 204 No Content\r\nContent-Length: 5\r\n\r\n", OK, "HTTP/1.1 304 Not Modified\r\nContent-Length: 5\r\n\r\n", OK, head, OK],
            false,
        )]);
        let url = post_url(port, "/r");
        let mut client = Client::new();
        let reply = client.send(&with(Method::Head, &url, b"")).unwrap();
        assert_eq!((reply.status, reply.body.len(), reply.header("content-length")), (200, 0, Some("5")));
        assert_eq!(client.get(&url).unwrap().body, b"ok");
        assert_eq!(client.send(&with(Method::Delete, &url, b"")).unwrap().body, b"");
        assert_eq!(client.get(&url).unwrap().body, b"ok");
        assert_eq!(client.send(&with(Method::Put, &url, b"")).unwrap().status, 304);
        assert_eq!(client.get(&url).unwrap().body, b"ok");
        // Streaming path.
        let mut streamed = client.send_stream(&with(Method::Head, &url, b"")).unwrap();
        let mut rest = Vec::new();
        streamed.body.read_to_end(&mut rest).unwrap();
        assert!(rest.is_empty());
        assert_eq!(client.get(&url).unwrap().body, b"ok");
        assert_eq!(seen(&requests).len(), 8);
    }

    #[test]
    fn every_method_follows_every_redirect_code_like_fetch() {
        // (method, body) -> the method after 301, 302, 303, 307, 308. Chrome gives the same (tools/fixtures/ui/methods-expected.txt).
        let want = |m: Method, code: u16| match (m, code) {
            (Method::Post, 301 | 302) | (Method::Put | Method::Patch | Method::Delete | Method::Options | Method::Post, 303) => Method::Get,
            (m, _) => m,
        };
        for method in [Method::Post, Method::Put, Method::Patch, Method::Delete, Method::Options, Method::Head, Method::Get] {
            for code in [301, 302, 303, 307, 308] {
                let (port, requests) = serve(vec![(vec![redirect(code, "/next"), OK], false)]);
                let body: &[u8] = if method.safe() { b"" } else { b"abc" };
                Client::new().send(&Request { method, url: &post_url(port, "/old"), headers: &[("Content-Type", "text/x")], body, cookies: Default::default() }).unwrap();
                let second = seen(&requests).remove(1);
                let after = want(method, code);
                assert!(second.starts_with(&format!("{} /next ", after.as_str())), "{method:?} {code}: {second}");
                let kept = after == method && !body.is_empty();
                assert_eq!(second.ends_with("abc"), kept, "{method:?} {code}: {second}");
                assert_eq!(second.contains("Content-Type"), after == method, "{method:?} {code}: {second}");
            }
        }
    }

    #[test]
    fn a_get_stays_a_get_through_every_redirect_code() {
        for code in [301, 302, 303, 307, 308] {
            let (port, requests) = serve(vec![(vec![redirect(code, "/n"), OK], false)]);
            Client::new().send(&Request { method: Method::Get, url: &post_url(port, "/"), headers: &[("X-A", "1")], body: b"", cookies: Default::default() }).unwrap();
            let seen = seen(&requests);
            assert!(seen[1].starts_with("GET /n HTTP/1.1") && seen[1].contains("X-A: 1\r\n"), "{code}");
        }
    }

    #[test]
    fn a_redirect_to_another_origin_drops_secret_headers_only() {
        // Another port is another origin.
        for code in [301, 303, 307] {
            let (other, requests) = serve(vec![(vec![OK], false)]);
            let to = leak(format!("http://127.0.0.1:{other}/landing"));
            let (port, first) = serve(vec![(vec![redirect(code, to)], false)]);
            let hdrs = [("Authorization", "Bearer t"), ("COOKIE", "s=1"), ("Proxy-Authorization", "p"), ("X-Keep", "1")];
            Client::new().post(&post_url(port, "/login"), &hdrs, b"{}").unwrap();
            let got = &seen(&requests)[0];
            for gone in ["Authorization", "authorization", "COOKIE", "Cookie", "Bearer", "s=1"] {
                assert!(!got.contains(gone), "{code}: {gone} leaked: {got}");
            }
            assert!(got.contains("X-Keep: 1\r\n"), "{got}");
            assert!(seen(&first)[0].contains("Authorization: Bearer t"));
        }
    }

    #[test]
    fn a_redirect_chain_keeps_the_stripping() {
        // A to B (cross origin: stripped), B to B/x (same origin): the header does not come back.
        let (b, requests) = serve(vec![(vec![redirect(302, "/x"), OK], false)]);
        let (a, _) = serve(vec![(vec![redirect(302, leak(format!("http://127.0.0.1:{b}/y")))], false)]);
        Client::new().send(&Request { method: Method::Get, url: &post_url(a, "/"), headers: &[("Authorization", "t")], body: b"", cookies: Default::default() }).unwrap();
        assert!(seen(&requests).iter().all(|r| !r.contains("Authorization")));
    }

    #[test]
    fn a_post_redirect_loop_stops() {
        let looping = redirect(307, "/");
        let (port, requests) = serve(vec![(vec![looping; 21], false)]);
        let error = Client::new().post(&post_url(port, "/"), &[], b"x").err().unwrap();
        assert!(error.to_string().contains("too many redirects"));
        assert_eq!(seen(&requests).len(), 21);
    }

    #[test]
    fn a_post_is_never_sent_twice_when_the_reply_fails() {
        // The server reads the POST on the reused connection and closes without a reply. A GET would try again on a
        // new connection (and the second server connection would see it). A POST must not.
        let (port, requests) = serve(vec![(vec![OK, ""], true), (vec![OK], false)]);
        let mut client = Client::new();
        client.get(&post_url(port, "/")).unwrap();
        assert!(client.post(&post_url(port, "/pay"), &[], b"amount=1").is_err());
        let seen = seen(&requests);
        assert_eq!(seen.len(), 2, "{seen:?}");
        assert!(seen[1].starts_with("POST /pay"));
    }

    #[test]
    fn a_get_is_still_retried_when_the_reply_fails() {
        let (port, requests) = serve(vec![(vec![OK, ""], true), (vec![OK], false)]);
        let mut client = Client::new();
        client.get(&post_url(port, "/")).unwrap();
        assert_eq!(client.get(&post_url(port, "/again")).unwrap().body, b"ok");
        assert_eq!(seen(&requests).len(), 3);
    }

    #[test]
    fn a_post_does_not_use_a_connection_that_the_server_closed() {
        // The server closed the idle connection. We see the close before we write, so the POST goes out once, on a new connection.
        let (port, requests) = serve(vec![(vec![OK], true), (vec![OK], false)]);
        let mut client = Client::new();
        client.get(&post_url(port, "/")).unwrap();
        assert_eq!(client.idle.lock().unwrap().len(), 1);
        // Wait until the client can see the close (no sleep: a loaded machine delays the server thread).
        while client.idle.lock().unwrap().values().flatten().all(|(stream, _)| alive(stream)) {
            std::thread::yield_now();
        }
        assert_eq!(client.post(&post_url(port, "/pay"), &[], b"amount=1").unwrap().body, b"ok");
        let seen = seen(&requests);
        assert_eq!(seen.iter().filter(|r| r.starts_with("POST")).count(), 1);
    }

    #[test]
    fn posts_reuse_the_connection() {
        let (port, requests) = serve(vec![(vec![OK, OK, OK], false)]);
        let mut client = Client::new();
        client.post(&post_url(port, "/a"), &[], b"1").unwrap();
        client.post(&post_url(port, "/b"), JSON, b"22").unwrap();
        client.get(&post_url(port, "/c")).unwrap();
        assert_eq!(seen(&requests).len(), 3);
        // One connection: the server accepts only one.
    }

    #[test]
    fn a_post_reply_can_be_an_error_or_cut_short() {
        let (port, _) = serve(vec![(vec!["HTTP/1.1 401 Unauthorized\r\nContent-Length: 3\r\n\r\nbad"], false)]);
        let r = Client::new().post(&post_url(port, "/"), &[], b"x").unwrap();
        assert_eq!((r.status, r.body.as_slice()), (401, b"bad".as_slice()));
        let (port, _) = serve(vec![(vec!["HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nabc"], true)]);
        assert!(Client::new().post(&post_url(port, "/"), &[], b"x").is_err());
        let (port, _) = serve(vec![(vec!["HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhel"], true)]);
        assert!(Client::new().post(&post_url(port, "/"), &[], b"x").is_err());
        let (port, _) = serve(vec![(vec!["garbage\r\n\r\n"], true)]);
        assert!(Client::new().post(&post_url(port, "/"), &[], b"x").is_err());
    }

    #[test]
    fn a_server_that_answers_early_and_closes_gives_an_answer_or_an_error_never_a_reused_connection() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut piece = [0u8; 4096];
            let _ = s.read(&mut piece);
            let _ = s.write_all(b"HTTP/1.1 413 Too Large\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        });
        let body = vec![b'x'; 8 * 1024 * 1024];
        let mut client = Client::new();
        if let Ok(r) = client.post(&post_url(port, "/"), &[], &body) {
            assert_eq!(r.status, 413);
        }
        assert!(client.idle.lock().unwrap().is_empty());
    }

    #[test]
    fn a_post_can_stream_its_reply() {
        let (port, requests) = serve(vec![(vec![redirect(303, "/done"), page("", "streamed")], false)]);
        let mut text = String::new();
        let url = post_url(port, "/go");
        Client::new().send_stream(&Request::post(&url, JSON, b"{}")).unwrap().body.read_to_string(&mut text).unwrap();
        assert_eq!(text, "streamed");
        assert!(seen(&requests)[1].starts_with("GET /done"));
    }

    #[test]
    fn the_cache_and_the_pool_are_keyed_by_scheme() {
        // A reply and a connection stored for https must not serve http, and the other way round.
        let (port, requests) = serve(vec![(vec![page("Cache-Control: max-age=60\r\n", "plain")], false)]);
        let mut client = Client::new();
        let fake = Arc::new(Response { status: 200, body: b"tls".to_vec(), ..Response::default() });
        let key = |scheme| (scheme, "127.0.0.1".to_string(), port, "/".to_string());
        client.cache.lock().unwrap().map.insert(key(Scheme::Https), Cached { response: fake, stored: Instant::now(), fresh_for: Duration::from_secs(60), owner: 0 });
        assert_eq!(client.get(&url(port)).unwrap().body, b"plain");
        let mut text = String::new();
        client.get_stream(&url(port)).unwrap().body.read_to_string(&mut text).unwrap();
        assert_eq!(text, "plain");
        assert!(client.cache.lock().unwrap().map.contains_key(&key(Scheme::Http)) && client.cache.lock().unwrap().map.contains_key(&key(Scheme::Https)));
        assert_eq!(client.cache.lock().unwrap().map[&key(Scheme::Https)].response.body, b"tls");
        assert_eq!(seen(&requests).len(), 1);
        // The plain reply is in the pool under `Http`, and a stream that the https key holds is not touched.
        assert!(client.idle.lock().unwrap().contains_key(&(Scheme::Http, "127.0.0.1".to_string(), u32::from(port))));
        assert!(!client.idle.lock().unwrap().contains_key(&(Scheme::Https, "127.0.0.1".to_string(), u32::from(port))));
    }

    #[test]
    fn a_plain_request_never_takes_an_https_idle_connection() {
        let dummy = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let (port, requests) = serve(vec![(vec![OK], false)]);
        let mut client = Client::new();
        let stale = TcpStream::connect(dummy.local_addr().unwrap()).unwrap();
        client.idle.lock().unwrap().insert((Scheme::Https, "127.0.0.1".to_string(), u32::from(port)), vec![(Stream::Tcp(stale), Instant::now())]);
        assert_eq!(client.get(&url(port)).unwrap().body, b"ok");
        assert_eq!(seen(&requests).len(), 1);
        assert!(client.idle.lock().unwrap().contains_key(&(Scheme::Https, "127.0.0.1".to_string(), u32::from(port))));
        // The request opened its own connection. The https entry is still there.
    }

    #[test]
    fn requests_with_headers_or_bodies_bypass_the_cache() {
        let (port, requests) = serve(vec![(vec![page("Cache-Control: max-age=60\r\n", "a"), page("", "b"), "HTTP/1.1 400 Bad Request\r\nContent-Length: 1\r\n\r\nc", page("", "d")], false)]);
        let mut client = Client::new();
        let url = url(port);
        assert_eq!(client.get(&url).unwrap().body, b"a");
        // An authenticated GET gets neither the cached copy nor a place in the cache, and a POST to the same URL too.
        let auth = Request { method: Method::Get, url: &url, headers: &[("Authorization", "t")], body: b"", cookies: Default::default() };
        assert_eq!(client.send(&auth).unwrap().body, b"b");
        assert_eq!(client.post(&url, &[], b"x").unwrap().body, b"c");
        assert_eq!(client.send_stream(&auth).unwrap().response.status, 200);
        // A failed POST leaves the plain cached copy; authenticated replies never replace it.
        assert_eq!(client.get(&url).unwrap().body, b"a");
        assert_eq!(seen(&requests).len(), 4);
        assert_eq!(client.cache.lock().unwrap().map.len(), 1);
    }

    #[test]
    fn successful_posts_invalidate_the_cached_target() {
        for stream in [false, true] {
            for code in [200, 204, 302, 400] {
                let reply = leak(format!("HTTP/1.1 {code} Reply\r\nContent-Length: 0\r\n\r\n"));
                let (port, requests) = serve(vec![(vec![page("Cache-Control: max-age=60\r\n", "old"), reply, page("", "new")], false)]);
                let mut client = Client::new();
                let url = url(port);
                assert_eq!(client.get(&url).unwrap().body, b"old");
                let req = Request::post(&url, &[], b"change");
                if stream {
                    let mut reply = client.send_stream(&req).unwrap();
                    io::copy(&mut reply.body, &mut io::sink()).unwrap();
                } else {
                    client.send(&req).unwrap();
                }
                let expected = if code < 400 { b"new" } else { b"old" };
                assert_eq!(client.get(&url).unwrap().body, expected, "stream={stream}, status={code}");
                assert_eq!(seen(&requests).len(), if code < 400 { 3 } else { 2 });
            }
        }
    }

    #[test]
    fn random_headers_never_split_the_head() {
        // Random names and values from an alphabet full of specials. A request that passes the check must give a head
        // with exactly one line for each header: no value can add a line, and nothing panics.
        let alphabet: Vec<&str> = vec!["a", "Z", "0", "-", "_", ":", " ", "\t", "\r", "\n", "\0", "\x7f", "\u{e9}", "(", "\"", ",", ";", "\r\n", "Host", "content-length"];
        let mut state = 0x1234_5678_9abc_def1u64;
        let mut next = move || {
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            state.wrapping_mul(0x2545F4914F6CDD1D)
        };
        let url = Url::parse("http://h/p?q").unwrap();
        let mut accepted = 0;
        for _ in 0..200_000 {
            let mut make = |max: u64| (0..next() % max).map(|_| alphabet[(next() % alphabet.len() as u64) as usize]).collect::<String>();
            let (n1, v1, n2, v2) = (make(5), make(6), make(5), make(6));
            let headers = [(n1.as_str(), v1.as_str()), (n2.as_str(), v2.as_str())];
            let method = if next() % 2 == 0 { Method::Get } else { Method::Post };
            let req = Request { method, url: &url, headers: &headers, body: b"", cookies: Default::default() };
            if check_request(&req).is_err() {
                continue;
            }
            accepted += 1;
            let plan = Plan { method, headers: Cow::Borrowed(&headers), body: b"", cookies: Default::default(), ident: None };
            let head = request_head(&url, &plan, "", 0);
            let lines = head.split(|&b| b == b'\n').count() - 1;
            // Each line ends with "\n": the pieces are the lines and one empty rest. Request line, Host, User-Agent and
            // Accept-Encoding and Accept-Language (unless the caller sets them), the two headers, Content-Length (POST), and the blank line.
            let ua = headers.iter().any(|(n, _)| n.eq_ignore_ascii_case("user-agent"));
            let ae = headers.iter().any(|(n, _)| n.eq_ignore_ascii_case("accept-encoding"));
            let al = headers.iter().any(|(n, _)| n.eq_ignore_ascii_case("accept-language"));
            assert_eq!(lines, 2 + usize::from(!ua) + usize::from(!ae) + usize::from(!al) + 2 + usize::from(method == Method::Post) + 1, "{:?}", String::from_utf8_lossy(&head));
            assert!(head.ends_with(b"\r\n\r\n") && !head.windows(2).any(|w| w == b"\n\n" || w == b"\r\r"));
            assert!(head.iter().filter(|&&b| b == b'\r').count() == lines && !head.contains(&0));
        }
        assert!(accepted > 100, "the generator is too strict: {accepted}");
    }
    #[test]
    fn a_single_streamed_request_does_not_follow_or_cache_a_redirect() {
        let (port, requests) = serve(vec![(vec![redirect(302, "/next"), OK], false)]);
        let mut client = Client::new();
        let mut response = client.send_stream_once(&Request::get(&url(port))).unwrap();
        assert_eq!(response.response.status, 302);
        let mut body = Vec::new();
        response.body.read_to_end(&mut body).unwrap();
        assert_eq!(requests.try_iter().count(), 1);
        assert!(client.cache.lock().unwrap().map.is_empty());
    }

}
