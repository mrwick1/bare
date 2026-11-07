//! `fetch`, `Headers`, `Request`, `Response` and a byte reader over a buffered response, for the DOM/JS bridge.
//!
//! How it works
//! - `fetch(...)` only validates and queues. It returns a genuine pending `Promise` (`Interp::host_new_promise`) and no socket
//!   opens. The embedder calls [`Browser::run_network_tasks`] after the script. That call does the blocking I/O with the real
//!   `bare_net::http::Client` (verified TLS, pooling, a POST is never sent twice), builds the `Response`, settles the promise
//!   through the evaluator's resolve/reject machinery and runs a microtask checkpoint. Promise callbacks stay microtasks.
//! - The document origin comes from [`Browser::configure_fetch`] and [`Browser::update_fetch_base_url`]. Another part of the
//!   browser owns the real document URL and calls the hook. Without it nothing leaves the process.
//! - State of every `Headers`, `Request`, `Response`, stream and reader lives in Rust (`FetchState`), keyed by the JS object.
//!   `Host::trace` reports the JS objects each one keeps, `Host::prune` drops the entries of dead objects, and the queue roots
//!   its promises. No `RefCell` borrow is held while JS runs (getters, `toString`, callbacks).
//!
//! Policy (strict same-origin milestone)
//! - A request whose origin is not the document origin is `Unsupported` (CORS is a gate), before DNS or a socket.
//! - Every redirect is checked against the origin of the request before the next hop opens a socket. A redirect to another
//!   origin, or from https to http, ends the request with a network error (the promise rejects with `TypeError`), so a
//!   307/308 can never forward a body or an `Authorization` header elsewhere. `redirect: "error"` is supported,
//!   `"manual"` is `Unsupported`. 301/302 (POST) and 303 (all but GET and HEAD) turn the request into a GET without a body; 307/308 keep both.
//! - JS cannot set `Host`, `Content-Length`, `Cookie`, `Origin`, `Referer`, `Connection`, `Transfer-Encoding`, `Accept-Encoding`,
//!   `Sec-*`, `Proxy-*` and the other forbidden names of the Fetch Standard: they are dropped, as a browser does. `Host` and
//!   `Content-Length` come from the client. `Set-Cookie` of a response is never put in `response.headers`. There is no cookie
//!   jar, so `credentials` changes nothing and no `Cookie`, `Origin`, `Referer` is ever sent.
//! - A failed network request rejects with `TypeError: Failed to fetch`. Errors and diagnostics never hold a header value, a
//!   body byte, a path or a query. [`Browser::take_fetch_diagnostics`] gives fixed labels and the origin.
//!
//! Resource caps (`FetchLimits`) end the call with `Error::Limit(LimitKind::Host(name))`, never a JS exception: queued requests,
//! requests started, URL length, request and response body bytes, aggregate retained body bytes, live objects, live readers, header
//! count and bytes, redirects. Copies and decoding charge the step budget. Objects count until the embedder collects them
//! (`Browser::collect`): the VM never collects by itself.
//!
//! Streams: `response.body` is a facade over a response that is fully buffered in memory. `getReader().read()` hands out real
//! bytes of that response in `Uint8Array` chunks of `chunk_bytes`, then `{done: true}`. There is no live socket streaming and no
//! backpressure. `cancel`, `releaseLock`, `locked`, `bodyUsed` and the one-consumer rules work. `tee`, `pipeTo`, `pipeThrough`,
//! BYOB readers, `reader.closed` and transfer are `Unsupported`. `new ReadableStream(...)` with a script source is in
//! `browser_streams.rs`; the stream and reader methods there dispatch on which kind of object `this` is.
//!
//! Bodies ("extract a body"): a string is `text/plain;charset=UTF-8`; a `URLSearchParams` is `application/x-www-form-urlencoded;charset=UTF-8`;
//! an `ArrayBuffer` or a view has no type; a `Blob` or `File` is its bytes with its `type` (none if empty); a `FormData` is
//! `multipart/form-data` with a new random boundary (`Dom::web_body` in `browser_web.rs`). The type is set only when the caller set
//! no `Content-Type`. `blob()` and `formData()` (`application/x-www-form-urlencoded` and `multipart/form-data` only, else `TypeError`)
//! read a body of a `Request` or `Response`.
//!
//! Gates (explicit `Unsupported`, nothing is faked): token methods and a lower-case `patch`; stream bodies;
//! `Request.body` and `mode: "no-cors"`; non-empty `integrity`;
//! `cache: "only-if-cached"`; non-ASCII header values; compressed
//! responses (a `Content-Encoding` that `bare_net` cannot undo is a network error; gzip, deflate and br are undone, and the headers stay); subclassing (`class X extends Response`).
//! Headers iteration: `keys`, `values`, `entries` and `[Symbol.iterator]` (the same function as `entries`) make live iterators. Each
//! `next` sorts and combines the list again and takes the entry at its index, so a header added or deleted during the loop shows. They
//! inherit from `%IteratorPrototype%`. A `HeadersInit` with `Symbol.iterator` (read once) is a sequence of pairs read with the engine's own
//! iteration (`Interp::host_iter_*`); each name and value is converted as it is read; a pair has exactly two values (at most three
//! are read). Header validation follows conversion. Sequence errors do not close iterators. A limit, `Unsupported` or heap
//! fault runs no more JS. Any other object is a record (an enumerable symbol key is a `TypeError`). Caps are checked
//! before growth, and a byte view with more keys than `max_headers` is refused before its keys are listed. Existing Headers inputs
//! read their iterator too. A finished iterator is not sticky: it sees later headers (as in Node).
//! Deviations: `statusText` of a network response is "" (the client drops the reason phrase). `Headers.forEach` uses a
//! snapshot of sorted, combined entries. Bodies are not cached: the client cache is bypassed. Timeouts are the client's (120 s
//! a reply). A request that already started its blocking I/O cannot be interrupted; [`Browser::abort_fetches`] only rejects
//! requests that are still queued.
//! `signal` (`browser_abort.rs`): an aborted signal rejects the promise with its reason at once; a later abort rejects a request that is still queued. A request that runs its blocking I/O is not interrupted. `request.signal` is a new signal that follows `init.signal`.
//!
//! Not browser-verified except against Node for the pure parts (`tools/js/fetch_oracle.mjs`).

use crate::browser::{Browser, Dom, Kind, R, unsupported};
use crate::browser_navigation_url::form_serialize;
use crate::browser_streams as streams;
use bare_js::eval::{Error, Interp};
use bare_js::value::{ObjId, Value};
use bare_net::http::{Client, Method, Request as NetRequest};
use bare_net::parallel::{MAX_WORKERS, Task};
use bare_url::{Scheme, Url};
use std::cell::{Cell, OnceCell, RefCell};
use std::borrow::Cow;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

/// Hard bounds of the fetch layer. Each is checked before the thing grows.
#[derive(Clone, Copy, Debug)]
pub struct FetchLimits {
    /// Requests that wait for `run_network_tasks`.
    pub max_pending: usize,
    /// Requests started in the life of the browser.
    pub max_started: u64,
    pub max_url_bytes: usize,
    pub max_request_body: usize,
    pub max_response_body: usize,
    /// Body bytes held by all Request/Response objects and queued requests together.
    pub max_retained: usize,
    /// Live Headers, Request, Response, stream and reader objects.
    pub max_objects: usize,
    pub max_readers: usize,
    /// Entries and bytes of one Headers.
    pub max_headers: usize,
    pub max_header_bytes: usize,
    pub max_redirects: usize,
    /// Size of one chunk of a reader.
    pub chunk_bytes: usize,
    /// Kept diagnostics. The oldest is dropped (and counted).
    pub max_diag: usize,
}

impl Default for FetchLimits {
    fn default() -> Self {
        FetchLimits {
            max_pending: 64,
            max_started: 1 << 16,
            max_url_bytes: 8 << 10,
            max_request_body: 8 << 20,
            max_response_body: 16 << 20,
            max_retained: 64 << 20,
            max_objects: 1 << 14,
            max_readers: 256,
            max_headers: 128,
            max_header_bytes: 64 << 10,
            max_redirects: 20,
            chunk_bytes: 64 << 10,
            max_diag: 64,
        }
    }
}

/// What `configure_fetch` takes. `base_url: None` keeps the network closed.
pub struct FetchConfig {
    /// The document URL. Its origin is the only origin `fetch` may reach, and relative URLs resolve against it.
    pub base_url: Option<String>,
    /// The HTTP client: pool, TLS roots. `Client::new()` trusts the OS roots; a test passes `Client::with_roots`.
    pub client: Client,
    pub limits: FetchLimits,
}

/// Counters for tests and the load tool.
#[derive(Clone, Copy, Debug, Default)]
pub struct FetchStats {
    pub pending: usize,
    pub objects: usize,
    pub readers: usize,
    pub retained_bytes: usize,
    pub queued_bytes: usize,
    pub started: u64,
    pub dropped_diagnostics: u64,
}

// ---------------------------------------------------------------------------------------------- state

#[derive(Clone, Copy, PartialEq, Eq)]
enum Guard {
    None,
    Request,
    Response,
    Immutable,
}

#[derive(Clone)]
struct HeaderList {
    guard: Guard,
    /// Lowercase name and value, in insertion order.
    list: Vec<(String, String)>,
}

enum HErr {
    Immutable,
    Limit(&'static str),
}

const FORBIDDEN: [&str; 21] = [
    "accept-charset", "accept-encoding", "access-control-request-headers", "access-control-request-method", "connection", "content-length", "cookie", "cookie2",
    "date", "dnt", "expect", "host", "keep-alive", "origin", "referer", "set-cookie", "te", "trailer", "transfer-encoding", "upgrade", "via",
];

pub(crate) fn forbidden_request(n: &str) -> bool {
    FORBIDDEN.contains(&n) || n.starts_with("proxy-") || n.starts_with("sec-")
}

impl HeaderList {
    fn new(guard: Guard) -> HeaderList {
        HeaderList { guard, list: Vec::new() }
    }

    /// A name the guard silently ignores (as a browser does).
    fn ignored(&self, n: &str) -> bool {
        match self.guard {
            Guard::Request => forbidden_request(n),
            Guard::Response => n == "set-cookie" || n == "set-cookie2",
            _ => false,
        }
    }

    fn check(&self, lim: &FetchLimits, add: usize) -> Result<(), HErr> {
        if self.guard == Guard::Immutable {
            return Err(HErr::Immutable);
        }
        let bytes: usize = self.list.iter().map(|(n, v)| n.len() + v.len()).sum();
        if self.list.len() >= lim.max_headers {
            return Err(HErr::Limit("fetch-headers"));
        }
        if bytes + add > lim.max_header_bytes {
            return Err(HErr::Limit("fetch-header-bytes"));
        }
        Ok(())
    }

    fn append(&mut self, lim: &FetchLimits, n: String, v: String) -> Result<(), HErr> {
        if self.guard == Guard::Immutable {
            return Err(HErr::Immutable);
        }
        if self.ignored(&n) {
            return Ok(());
        }
        self.check(lim, n.len() + v.len())?;
        self.list.push((n, v));
        Ok(())
    }

    fn set(&mut self, lim: &FetchLimits, n: String, v: String) -> Result<(), HErr> {
        if self.guard == Guard::Immutable {
            return Err(HErr::Immutable);
        }
        if self.ignored(&n) {
            return Ok(());
        }
        let first = self.list.iter().position(|(k, _)| *k == n);
        match first {
            Some(i) => {
                let kept: usize = self.list.iter().filter(|(k, _)| k != &n).map(|(k, v)| k.len() + v.len()).sum();
                if kept.saturating_add(n.len()).saturating_add(v.len()) > lim.max_header_bytes {
                    return Err(HErr::Limit("fetch-header-bytes"));
                }
                self.list[i].1 = v;
                let mut seen = false;
                self.list.retain(|(k, _)| *k != n || !std::mem::replace(&mut seen, true));
                // `retain` keeps the first match (the one we just changed) and drops the rest.
                Ok(())
            }
            None => self.append(lim, n, v),
        }
    }

    fn delete(&mut self, n: &str) -> Result<(), HErr> {
        if self.guard == Guard::Immutable {
            return Err(HErr::Immutable);
        }
        if !self.ignored(n) {
            self.list.retain(|(k, _)| k != n);
        }
        Ok(())
    }

    fn get(&self, n: &str) -> Option<String> {
        let mut it = self.list.iter().filter(|(k, _)| k == n).map(|(_, v)| v.as_str());
        let first = it.next()?;
        Some(it.fold(first.to_string(), |a, v| a + ", " + v))
    }

    /// Entry `i` of `sorted()`, without copying the others (a live iterator asks again at each step).
    fn nth_sorted(&self, i: usize) -> Option<(String, String)> {
        let mut names: Vec<&String> = self.list.iter().map(|(k, _)| k).collect();
        names.sort_unstable();
        names.dedup();
        let n = names.get(i)?;
        Some(((*n).clone(), self.get(n).unwrap_or_default()))
    }

    /// Sorted by name, one entry for each name with the values joined by ", " (the iteration order of the standard).
    fn sorted(&self) -> Vec<(String, String)> {
        let mut names: Vec<&String> = self.list.iter().map(|(k, _)| k).collect();
        names.sort_unstable();
        names.dedup();
        names.into_iter().map(|n| (n.clone(), self.get(n).unwrap_or_default())).collect()
    }
}

/// The bytes of a body init and the content type it implies.
pub(crate) type BodyInit = (Rc<Vec<u8>>, Option<Cow<'static, str>>);

/// A body that is in memory. `None` bytes is a `null` body or a body that was consumed.
struct Body {
    bytes: Option<Rc<Vec<u8>>>,
    /// Offset of the next chunk a reader hands out.
    pos: usize,
    /// Disturbed: a text/json/arrayBuffer call, a read or a cancel happened.
    used: bool,
    locked: bool,
}

impl Body {
    fn new(bytes: Option<Rc<Vec<u8>>>) -> Body {
        Body { bytes, pos: 0, used: false, locked: false }
    }

    fn len(&self) -> usize {
        self.bytes.as_ref().map_or(0, |b| b.len())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Redirect {
    Follow,
    Error,
}

struct Common {
    headers: ObjId,
    body: Body,
}

struct ReqData {
    c: Common,
    method: String,
    url: Url,
    redirect: Redirect,
    credentials: &'static str,
    mode: &'static str,
    /// The signal that follows `init.signal`, made on the first read of `request.signal` or at construction.
    signal: Option<ObjId>,
}

struct RespData {
    c: Common,
    status: u16,
    status_text: String,
    url: String,
    redirected: bool,
    ty: &'static str,
    stream: Option<ObjId>,
}

enum F {
    Headers(HeaderList),
    Request(ReqData),
    Response(RespData),
    /// The `body` of a response. Holds the response.
    Stream(ObjId),
    Reader { stream: ObjId, released: bool },
    /// A live iterator of a Headers: the position in the sorted, combined list, read again at each step.
    HeadersIter { headers: ObjId, idx: usize, kind: u8 },
}

/// A request that waits for `run_network_tasks`. Plain Rust data: the only JS value is the promise, and it is a root.
struct Pending {
    promise: Value,
    method: Method,
    url: Url,
    headers: Vec<(String, String)>,
    body: Option<Rc<Vec<u8>>>,
    redirect: Redirect,
    /// An `XMLHttpRequest` and its request generation (`browser_xhr.rs`). Then `promise` is `undefined`.
    xhr: Option<(ObjId, u64)>,
    /// When it was queued (only when `BARE_NET_TRACE` is set).
    queued: Option<std::time::Instant>,
    /// Its I/O, started on a worker thread before its turn (see `start_queued`). The outcome and the diagnostics.
    started: Option<Task<(Outcome, Vec<String>)>>,
}

/// What a worker thread needs to send a queued request. It owns its data: a `Pending` holds values of the realm.
struct Wire {
    method: Method,
    url: Url,
    headers: Vec<(String, String)>,
    body: Option<Vec<u8>>,
    redirect: Redirect,
    queued: Option<std::time::Instant>,
}

/// What `run_one` needs to send a request: borrowed from a `Pending` or from a `Wire`.
struct Parts<'a> {
    method: Method,
    url: &'a Url,
    headers: &'a [(String, String)],
    body: Option<&'a [u8]>,
    redirect: Redirect,
}

impl Wire {
    fn parts(&self) -> Parts<'_> {
        Parts { method: self.method, url: &self.url, headers: &self.headers, body: self.body.as_deref(), redirect: self.redirect }
    }
}

impl Pending {
    fn parts(&self) -> Parts<'_> {
        Parts { method: self.method, url: &self.url, headers: &self.headers, body: self.body.as_ref().map(|b| b.as_slice()), redirect: self.redirect }
    }

    fn wire(&self) -> Wire {
        Wire { method: self.method, url: dup(&self.url), headers: self.headers.clone(), body: self.body.as_deref().cloned(), redirect: self.redirect, queued: self.queued }
    }
}

#[derive(Default)]
struct FetchState {
    objs: HashMap<ObjId, F>,
    queue: VecDeque<Pending>,
    base: Option<Url>,
    retained: usize,
    queued_bytes: usize,
    readers: usize,
    started: u64,
    diag: VecDeque<String>,
    dropped_diag: u64,
    halted: Option<Error>,
}

#[derive(Clone, Copy)]
enum Pr {
    Headers,
    Request,
    Response,
    Stream,
    Reader,
    HeadersIter,
}

/// The fetch state of one `Dom`.
pub(crate) struct Fetch {
    st: RefCell<FetchState>,
    lim: Cell<FetchLimits>,
    client: RefCell<Client>,
    protos: OnceCell<[ObjId; 6]>,
    fixed: RefCell<Vec<ObjId>>,
    pub(crate) json_parse: Cell<Option<Value>>,
}

impl Default for Fetch {
    fn default() -> Fetch {
        Fetch { st: RefCell::default(), lim: Cell::new(FetchLimits::default()), client: RefCell::new(Client::new()), protos: OnceCell::new(), fixed: RefCell::default(), json_parse: Cell::new(None) }
    }
}

impl Fetch {
    pub(crate) fn roots(&self, out: &mut Vec<Value>) {
        out.extend(self.fixed.borrow().iter().map(|&o| Value::Object(o)));
        out.extend(self.json_parse.get());
        out.extend(self.st.borrow().queue.iter().flat_map(|p| [p.promise].into_iter().chain(p.xhr.map(|x| Value::Object(x.0)))));
    }

    pub(crate) fn trace(&self, owner: ObjId, out: &mut Vec<Value>) {
        let st = self.st.borrow();
        match st.objs.get(&owner) {
            Some(F::Request(r)) => out.extend([Value::Object(r.c.headers)].into_iter().chain(r.signal.map(Value::Object))),
            Some(F::Response(r)) => {
                out.push(Value::Object(r.c.headers));
                out.extend(r.stream.map(Value::Object));
            }
            Some(F::Stream(o)) | Some(F::Reader { stream: o, .. }) | Some(F::HeadersIter { headers: o, .. }) => out.push(Value::Object(*o)),
            _ => {}
        }
    }

    pub(crate) fn prune(&self, live: &dyn Fn(ObjId) -> bool) {
        let mut st = self.st.borrow_mut();
        st.objs.retain(|&o, _| live(o));
        st.recount();
    }
}

impl FetchState {
    /// Recompute the totals from the objects (after a prune).
    fn recount(&mut self) {
        self.retained = self.objs.values().map(|f| match f {
            F::Request(r) => r.c.body.len(),
            F::Response(r) => r.c.body.len(),
            _ => 0,
        }).sum();
        self.readers = self.objs.values().filter(|f| matches!(f, F::Reader { .. })).count();
    }

    fn diag(&mut self, lim: &FetchLimits, text: String) {
        if lim.max_diag == 0 {
            self.dropped_diag = self.dropped_diag.saturating_add(1);
            return;
        }
        if self.diag.len() >= lim.max_diag {
            self.diag.pop_front();
            self.dropped_diag += 1;
        }
        self.diag.push_back(text);
    }
}

fn dup(u: &Url) -> Url {
    Url { scheme: u.scheme, host: u.host.clone(), port: u.port, path: u.path.clone() }
}

/// `scheme://host:port`, no path and no query: safe for a diagnostic.
fn origin(u: &Url) -> String {
    let u = Url { scheme: u.scheme, host: u.host.clone(), port: u.port, path: String::new() };
    u.to_string()
}

// ---------------------------------------------------------------------------------------------- Dom side

impl Dom {
    /// Requests that wait for `run_network_tasks` (`fetch` and XHR).
    pub(crate) fn fetch_queued(&self) -> usize {
        self.fetch.st.borrow().queue.len()
    }

    pub(crate) fn fl(&self) -> FetchLimits {
        self.fetch.lim.get()
    }

    fn fproto(&self, p: Pr) -> ObjId {
        self.fetch.protos.get().expect("installed")[p as usize]
    }

    /// Reserve room for a new object (and `bytes` of body) before it is made.
    fn reserve(&self, it: &mut Interp<'_>, objects: usize, bytes: usize) -> R<()> {
        let lim = self.fl();
        let st = self.fetch.st.borrow();
        if st.objs.len() + objects > lim.max_objects {
            return Err(self.lim("fetch-objects"));
        }
        if st.retained.saturating_add(st.queued_bytes).saturating_add(bytes) > lim.max_retained {
            return Err(self.lim("fetch-retained"));
        }
        drop(st);
        it.charge(objects as u64 + (bytes / 64) as u64)
    }

    fn add_obj(&self, it: &mut Interp<'_>, p: Pr, f: F) -> R<ObjId> {
        let o = self.obj(it, self.fproto(p))?;
        let mut st = self.fetch.st.borrow_mut();
        match &f {
            F::Request(r) => st.retained += r.c.body.len(),
            F::Response(r) => st.retained += r.c.body.len(),
            F::Reader { .. } => st.readers += 1,
            _ => {}
        }
        st.objs.insert(o, f);
        Ok(o)
    }

    fn headers_obj(&self, it: &mut Interp<'_>, list: HeaderList) -> R<ObjId> {
        self.reserve(it, 1, 0)?;
        self.add_obj(it, Pr::Headers, F::Headers(list))
    }

    fn herr(&self, it: &mut Interp<'_>, e: HErr) -> Error {
        match e {
            HErr::Immutable => it.type_error("Headers are immutable"),
            HErr::Limit(n) => self.lim(n),
        }
    }

    fn header_name(&self, it: &mut Interp<'_>, v: Value) -> R<String> {
        let s = self.string_arg(it, v)?;
        self.normalize_header_name(it, s)
    }

    fn normalize_header_name(&self, it: &mut Interp<'_>, s: String) -> R<String> {
        if s.is_empty() || !s.bytes().all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b)) {
            return Err(it.type_error("Invalid header name"));
        }
        Ok(s.to_ascii_lowercase())
    }

    fn header_value(&self, it: &mut Interp<'_>, v: Value) -> R<String> {
        let s = self.string_arg(it, v)?;
        self.normalize_header_value(it, s)
    }

    fn normalize_header_value(&self, it: &mut Interp<'_>, s: String) -> R<String> {
        let t = s.trim_matches(['\t', '\n', '\r', ' ']);
        if t.chars().any(|c| matches!(c, '\0' | '\r' | '\n') || c as u32 > 0xFF) {
            return Err(it.type_error("Invalid header value"));
        }
        if !t.is_ascii() {
            return Err(Error::Unsupported(unsupported("non-ASCII header value (the wire encoding is not Latin-1 safe)")));
        }
        Ok(t.to_string())
    }

    /// One element of a sequence init: an object that is iterable and holds exactly two values, a name and a value. Each is
    /// checked when it is read. At most three values are read (a longer or endless iterable is a `TypeError`).
    fn init_pair(&self, it: &mut Interp<'_>, x: Value) -> R<(String, String)> {
        const BAD: &str = "Invalid header pair: a header is [name, value]";
        if !matches!(x, Value::Object(_)) {
            return Err(it.type_error("Invalid header pair"));
        }
        let Some(mut inner) = it.host_iter_open(x)? else { return Err(it.type_error(BAD)) };
        let (mut name, mut value, mut n) = (None, None, 0);
        loop {
            let step = match it.host_iter_next(&mut inner) {
                Ok(Some(_)) if n == 2 => {
                    let e = it.type_error(BAD);
                    return Err(e);
                }
                Ok(Some(v)) if n == 0 => self.string_arg(it, v).map(|s| {
                    name = Some(s);
                    true
                }),
                Ok(Some(v)) => self.string_arg(it, v).map(|s| {
                    value = Some(s);
                    true
                }),
                Ok(None) => Ok(false),
                Err(e) => Err(e),
            };
            match step {
                Ok(true) => n += 1,
                Ok(false) => break,
                Err(e) => return Err(e),
            }
        }
        match (name, value) {
            (Some(a), Some(b)) => Ok((a, b)),
            _ => Err(it.type_error(BAD)),
        }
    }

    /// The caps of a `HeadersInit` that is being read, before the next pair is made: entries, then bytes.
    fn init_room(&self, out: &[(String, String)], bytes: usize, add: usize) -> R<()> {
        let lim = self.fl();
        if add == 0 && out.len() >= lim.max_headers {
            return Err(self.lim("fetch-headers"));
        }
        if bytes.saturating_add(add) > lim.max_header_bytes {
            return Err(self.lim("fetch-header-bytes"));
        }
        Ok(())
    }

    /// `(name, value)` pairs of a `HeadersInit`: a Headers, a sequence of pairs (anything with `Symbol.iterator`, read once)
    /// or a record. Nothing is stored yet. Existing Headers use the same iterator path.
    fn init_pairs(&self, it: &mut Interp<'_>, init: Value) -> R<Vec<(String, String)>> {
        let Value::Object(o) = init else { return Err(it.type_error("Headers init is not an object or a sequence")) };
        let (mut out, mut bytes) = (Vec::new(), 0usize);
        if let Some(mut outer) = it.host_iter_open(init)? {
            loop {
                let step = (|| {
                    let Some(x) = it.host_iter_next(&mut outer)? else { return Ok(false) };
                    self.init_room(&out, bytes, 0)?;
                    let (n, v) = self.init_pair(it, x)?;
                    self.init_room(&out, bytes, n.len() + v.len())?;
                    bytes += n.len() + v.len();
                    out.push((n, v));
                    Ok(true)
                })();
                match step {
                    Ok(true) => {}
                    Ok(false) => return Ok(out),
                    Err(e) => return Err(e),
                }
            }
        }
        // A byte view lists one key for each byte: refuse a long one before the list is made.
        if it.heap.view_parts(o).is_some_and(|(_, _, len)| len > self.fl().max_headers) {
            return Err(self.lim("fetch-headers"));
        }
        let keys = it.heap.own_keys(o).map_err(|e| it.heap_error(e))?;
        if keys.len() > self.fl().max_headers {
            return Err(self.lim("fetch-headers"));
        }
        for k in keys {
            it.charge(1)?;
            let enumerable = it.heap.peek_own_property(o, k).map_err(|e| it.heap_error(e))?.is_some_and(|p| p.enumerable());
            if !enumerable {
                continue;
            }
            if matches!(k, bare_js::value::PropertyKey::Symbol(_)) {
                return Err(it.type_error("Cannot convert a Symbol value to a string"));
            }
            let name = it.heap.key_to_value(k).map_err(|e| it.heap_error(e))?;
            let v = it.get_value(init, k)?;
            let (n, v) = (self.header_name(it, name)?, self.string_arg(it, v)?);
            self.init_room(&out, bytes, n.len() + v.len())?;
            bytes += n.len() + v.len();
            out.push((n, v));
        }
        Ok(out)
    }

    fn fill(&self, it: &mut Interp<'_>, list: &mut HeaderList, init: Value) -> R<()> {
        if matches!(init, Value::Undefined) {
            return Ok(());
        }
        let lim = self.fl();
        for (n, v) in self.init_pairs(it, init)? {
            let n = self.normalize_header_name(it, n)?;
            let v = self.normalize_header_value(it, v)?;
            list.append(&lim, n, v).map_err(|e| self.herr(it, e))?;
        }
        Ok(())
    }

    fn brand(&self, it: &mut Interp<'_>, this: Value) -> R<ObjId> {
        match this {
            Value::Object(o) if self.fetch.st.borrow().objs.contains_key(&o) => Ok(o),
            _ => Err(it.type_error("Illegal invocation")),
        }
    }

    /// Run `f` on the header list of `this` (a Headers). `f` cannot run JS: it holds the borrow.
    fn on_headers<T>(&self, it: &mut Interp<'_>, this: Value, f: impl FnOnce(&mut HeaderList) -> Result<T, HErr>) -> R<T> {
        let o = self.brand(it, this)?;
        let r = match self.fetch.st.borrow_mut().objs.get_mut(&o) {
            Some(F::Headers(h)) => f(h),
            _ => return Err(it.type_error("Illegal invocation")),
        };
        r.map_err(|e| self.herr(it, e))
    }

    fn list_len(&self, o: ObjId) -> usize {
        match self.fetch.st.borrow().objs.get(&o) {
            Some(F::Headers(h)) => h.list.len(),
            _ => 0,
        }
    }

    fn list_of(&self, o: ObjId) -> HeaderList {
        match self.fetch.st.borrow().objs.get(&o) {
            Some(F::Headers(h)) => h.clone(),
            _ => HeaderList::new(Guard::None),
        }
    }

    pub(crate) fn fetch_base(&self) -> Option<Url> {
        let configured = self.fetch.st.borrow().base.as_ref().map(dup)?;
        let nav = self.nav.s.borrow();
        Some(nav.entries.get(nav.cur).map(|e| dup(&e.url.u)).unwrap_or(configured))
    }

    fn resolve_url(&self, it: &mut Interp<'_>, s: &str) -> R<Url> {
        if s.len() > self.fl().max_url_bytes {
            return Err(self.lim("fetch-url"));
        }
        let s = s.trim_matches(|c: char| c <= ' ');
        let r = match &self.fetch_base() {
            Some(b) => b.join(s),
            None => Url::parse(s),
        };
        match r {
            Ok(u) if u.to_string().len() <= self.fl().max_url_bytes => Ok(u),
            Ok(_) => Err(self.lim("fetch-url")),
            Err(_) => Err(it.type_error("Failed to parse URL")),
        }
    }

    /// The bytes of a body init, and its default content type. A copy is charged and bounded before it is made.
    pub(crate) fn extract_body(&self, it: &mut Interp<'_>, v: Value) -> R<Option<BodyInit>> {
        let lim = self.fl();
        let (bytes, ty): (Vec<u8>, Option<Cow<'static, str>>) = match v {
            Value::Undefined | Value::Null => return Ok(None),
            Value::Object(_) if it.bytes_of(v).is_some() => {
                let n = it.bytes_of(v).map_or(0, <[u8]>::len);
                if n > lim.max_request_body {
                    return Err(self.lim("fetch-request-body"));
                }
                self.reserve(it, 0, n)?;
                (it.bytes_of(v).map(<[u8]>::to_vec).unwrap_or_default(), None)
            }
            Value::Object(_) if let Some(n) = self.web_body_len(v) => {
                if n > lim.max_request_body {
                    return Err(self.lim("fetch-request-body"));
                }
                self.reserve(it, 0, n)?;
                let (bytes, ty) = self.web_body(it, v, lim.max_request_body)?;
                (bytes, ty.map(Cow::Owned))
            }
            Value::Object(o) if self.nav.s.borrow().params.contains_key(&o) => {
                let s = form_serialize(&self.nav.s.borrow().params[&o].list);
                if s.len() > lim.max_request_body {
                    return Err(self.lim("fetch-request-body"));
                }
                self.reserve(it, 0, s.len())?;
                (s.into_bytes(), Some(Cow::Borrowed("application/x-www-form-urlencoded;charset=UTF-8")))
            }
            Value::Object(_) => {
                let kg = self.key(it, "getReader")?;
                let g = it.get_value(v, kg)?;
                if it.is_callable_value(g) {
                    return Err(Error::Unsupported(unsupported("a ReadableStream as a body")));
                }
                self.text_body(it, v)?
            }
            _ => self.text_body(it, v)?,
        };
        Ok(Some((Rc::new(bytes), ty)))
    }

    /// A body that is converted to a string (UTF-8, `text/plain`).
    fn text_body(&self, it: &mut Interp<'_>, v: Value) -> R<(Vec<u8>, Option<Cow<'static, str>>)> {
        let s = self.string_arg(it, v)?;
        if s.len() > self.fl().max_request_body {
            return Err(self.lim("fetch-request-body"));
        }
        self.reserve(it, 0, s.len())?;
        Ok((s.into_bytes(), Some(Cow::Borrowed("text/plain;charset=UTF-8"))))
    }

    fn prop(&self, it: &mut Interp<'_>, o: Value, name: &str) -> R<Value> {
        let k = self.key(it, name)?;
        it.get_value(o, k)
    }

    /// An enum-like string option: `None` when absent, else one of `allowed` (a `TypeError` otherwise).
    fn choice(&self, it: &mut Interp<'_>, o: Value, name: &str, allowed: &[&'static str]) -> R<Option<&'static str>> {
        let v = self.prop(it, o, name)?;
        if matches!(v, Value::Undefined) {
            return Ok(None);
        }
        let s = self.string_arg(it, v)?;
        match allowed.iter().find(|a| **a == s) {
            Some(a) => Ok(Some(a)),
            None => Err(it.type_error(&format!("The provided value is not a valid enum value of type {name}"))),
        }
    }
}

// ---------------------------------------------------------------------------------------------- Request core

struct Core {
    method: String,
    url: Url,
    headers: HeaderList,
    body: Option<Rc<Vec<u8>>>,
    redirect: Redirect,
    credentials: &'static str,
    mode: &'static str,
    /// The `AbortSignal` of `init.signal`, or the one of the `Request` input.
    signal: Option<ObjId>,
}

/// Build the data of a request from `input` and `init`, the way `new Request(input, init)` and `fetch(input, init)` do.
fn make_core(d: &Dom, it: &mut Interp<'_>, input: Value, init: Value) -> R<Core> {
    let lim = d.fl();
    let mut transfer: Option<ObjId> = None;
    let mut core = match input {
        Value::Object(o) if matches!(d.fetch.st.borrow().objs.get(&o), Some(F::Request(_))) => {
            let (method, url, redirect, credentials, mode, hdr, body, used, signal) = match d.fetch.st.borrow().objs.get(&o) {
                Some(F::Request(r)) => (r.method.clone(), dup(&r.url), r.redirect, r.credentials, r.mode, r.c.headers, r.c.body.bytes.clone(), r.c.body.used || r.c.body.locked, r.signal),
                _ => unreachable!(),
            };
            if used {
                return Err(it.type_error("Cannot construct a Request with a Request object that has already been used"));
            }
            transfer = body.is_some().then_some(o);
            let mut headers = d.list_of(hdr);
            headers.guard = Guard::Request;
            Core { method, url, headers, body, redirect, credentials, mode, signal }
        }
        _ => {
            let s = d.string_arg(it, input)?;
            let url = d.resolve_url(it, &s)?;
            Core { method: "GET".into(), url, headers: HeaderList::new(Guard::Request), body: None, redirect: Redirect::Follow, credentials: "same-origin", mode: "cors", signal: None }
        }
    };
    if !matches!(init, Value::Undefined | Value::Null) {
        if !matches!(init, Value::Object(_)) {
            return Err(it.type_error("The request init is not an object"));
        }
        let m = d.prop(it, init, "method")?;
        if !matches!(m, Value::Undefined) {
            let s = d.string_arg(it, m)?;
            if s.is_empty() || !s.bytes().all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b)) {
                return Err(it.type_error("'method' is not a valid HTTP method"));
            }
            if ["CONNECT", "TRACE", "TRACK"].iter().any(|f| s.eq_ignore_ascii_case(f)) {
                return Err(it.type_error(&format!("'{s}' HTTP method is unsupported")));
            }
            core.method = if ["DELETE", "GET", "HEAD", "OPTIONS", "POST", "PUT"].iter().any(|f| s.eq_ignore_ascii_case(f)) { s.to_ascii_uppercase() } else { s };
        }
        if let Some(r) = d.choice(it, init, "redirect", &["follow", "error", "manual"])? {
            core.redirect = match r {
                "follow" => Redirect::Follow,
                "error" => Redirect::Error,
                _ => return Err(Error::Unsupported(unsupported("fetch redirect: manual (opaque redirect responses)"))),
            };
        }
        if let Some(c) = d.choice(it, init, "credentials", &["omit", "same-origin", "include"])? {
            core.credentials = c;
        }
        if let Some(m) = d.choice(it, init, "mode", &["same-origin", "cors", "no-cors", "navigate"])? {
            match m {
                "navigate" => return Err(it.type_error("Request constructor: invalid request mode navigate")),
                "no-cors" => return Err(Error::Unsupported(unsupported("fetch mode: no-cors (opaque responses)"))),
                _ => core.mode = m,
            }
        }
        if d.choice(it, init, "cache", &["default", "no-store", "reload", "no-cache", "force-cache", "only-if-cached"])? == Some("only-if-cached") {
            return Err(Error::Unsupported(unsupported("fetch cache: only-if-cached")));
        }
        match d.prop(it, init, "signal")? {
            Value::Undefined => {}
            Value::Null => core.signal = None,
            Value::Object(o) if d.is_signal(o) => core.signal = Some(o),
            _ => return Err(it.type_error("Failed to construct 'Request': Failed to read the 'signal' property from 'RequestInit': Failed to convert value to 'AbortSignal'.")),
        }
        let integrity = d.prop(it, init, "integrity")?;
        if !matches!(integrity, Value::Undefined) && !d.string_arg(it, integrity)?.is_empty() {
            return Err(Error::Unsupported(unsupported("fetch option integrity")));
        }
        let h = d.prop(it, init, "headers")?;
        if !matches!(h, Value::Undefined) {
            let mut list = HeaderList::new(Guard::Request);
            d.fill(it, &mut list, h)?;
            core.headers = list;
        }
        let b = d.prop(it, init, "body")?;
        if let Some((bytes, ty)) = d.extract_body(it, b)? {
            core.body = Some(bytes);
            transfer = None;
            if let Some(ty) = ty && core.headers.get("content-type").is_none() {
                core.headers.append(&lim, "content-type".into(), ty.into()).map_err(|e| d.herr(it, e))?;
            }
        }
    }
    if core.body.is_some() && (core.method == "GET" || core.method == "HEAD") {
        return Err(it.type_error("Request with GET/HEAD method cannot have body."));
    }
    if let Some(o) = transfer {
        // The body moves to the new request: the old one is used up.
        let mut st = d.fetch.st.borrow_mut();
        if let Some(F::Request(r)) = st.objs.get_mut(&o) {
            r.c.body.used = true;
            let n = r.c.body.len();
            r.c.body.bytes = None;
            st.retained -= n;
        }
    }
    Ok(core)
}

/// The wire method for a normalized method. A token method (`FOO`) and a lower-case `patch` have no `Method` variant: `Unsupported`.
fn wire_method(m: &str) -> R<Method> {
    match m {
        "GET" => Ok(Method::Get),
        "POST" => Ok(Method::Post),
        "PUT" => Ok(Method::Put),
        "PATCH" => Ok(Method::Patch),
        "DELETE" => Ok(Method::Delete),
        "HEAD" => Ok(Method::Head),
        "OPTIONS" => Ok(Method::Options),
        _ => Err(Error::Unsupported(unsupported("fetch method other than GET, POST, PUT, PATCH, DELETE, HEAD and OPTIONS"))),
    }
}

// ---------------------------------------------------------------------------------------------- settle helpers

/// Settle `p` with the outcome of `r`: a value fulfills, a thrown value rejects, any other error is the result.
pub(crate) fn settle(it: &mut Interp<'_>, p: Value, r: R<Value>) -> R<Value> {
    match r {
        Ok(v) => it.host_resolve_promise(p, v)?,
        Err(Error::Throw(v)) => it.host_reject_promise(p, v)?,
        Err(e) => return Err(e),
    }
    Ok(p)
}

/// The abort algorithm of a queued request: reject its promise with `reason`. A request that already left the queue is not touched.
pub(crate) fn abort_queued(d: &Dom, it: &mut Interp<'_>, promise: Value, reason: Value) -> R<()> {
    let found = {
        let mut st = d.fetch.st.borrow_mut();
        match st.queue.iter().position(|t| matches!((t.promise, promise), (Value::Object(a), Value::Object(b)) if a == b)) {
            Some(i) => {
                let t = st.queue.remove(i).expect("position");
                st.queued_bytes -= t.body.as_ref().map_or(0, |b| b.len());
                true
            }
            None => false,
        }
    };
    if found {
        it.host_reject_promise(promise, reason)?;
    }
    Ok(())
}

fn thrown_value(e: Error) -> R<Value> {
    match e {
        Error::Throw(v) => Ok(v),
        e => Err(e),
    }
}

// ---------------------------------------------------------------------------------------------- Headers methods

fn headers_ctor(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> R<Value> {
    let mut list = HeaderList::new(Guard::None);
    let init = a.first().copied().unwrap_or(Value::Undefined);
    if matches!(init, Value::Null) {
        return Err(it.type_error("Failed to construct 'Headers': The provided value is not of type 'HeadersInit'"));
    }
    d.fill(it, &mut list, init)?;
    Ok(Value::Object(d.headers_obj(it, list)?))
}

fn arg(a: &[Value], i: usize) -> Value {
    a.get(i).copied().unwrap_or(Value::Undefined)
}

fn headers_append(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> R<Value> {
    let (n, v) = (d.header_name(it, arg(a, 0))?, d.header_value(it, arg(a, 1))?);
    let lim = d.fl();
    d.on_headers(it, this, |h| h.append(&lim, n, v))?;
    Ok(Value::Undefined)
}

fn headers_set(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> R<Value> {
    let (n, v) = (d.header_name(it, arg(a, 0))?, d.header_value(it, arg(a, 1))?);
    let lim = d.fl();
    d.on_headers(it, this, |h| h.set(&lim, n, v))?;
    Ok(Value::Undefined)
}

fn headers_delete(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> R<Value> {
    let n = d.header_name(it, arg(a, 0))?;
    d.on_headers(it, this, |h| h.delete(&n))?;
    Ok(Value::Undefined)
}

fn headers_get(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> R<Value> {
    let n = d.header_name(it, arg(a, 0))?;
    match d.on_headers(it, this, |h| Ok(h.get(&n)))? {
        Some(v) => d.js_str(it, &v),
        None => Ok(Value::Null),
    }
}

fn headers_has(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> R<Value> {
    let n = d.header_name(it, arg(a, 0))?;
    Ok(Value::Bool(d.on_headers(it, this, |h| Ok(h.get(&n).is_some()))?))
}

fn headers_get_set_cookie(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> R<Value> {
    d.on_headers(it, this, |_| Ok(()))?;
    it.array_from(&[])
}

fn headers_for_each(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> R<Value> {
    let cb = arg(a, 0);
    if !it.is_callable_value(cb) {
        return Err(it.type_error("Failed to execute 'forEach' on 'Headers': parameter 1 is not of type 'Function'."));
    }
    let snapshot = d.on_headers(it, this, |h| Ok(h.sorted()))?;
    it.charge(snapshot.len() as u64)?;
    for (n, v) in snapshot {
        let (jv, jn) = (d.js_str(it, &v)?, d.js_str(it, &n)?);
        it.call(cb, arg(a, 1), &[jv, jn, this])?;
    }
    Ok(Value::Undefined)
}

/// `keys()`, `values()` and `entries()` (kind 0, 1, 2): a live iterator. Each `next` reads the sorted, combined list again,
/// so a header added or deleted during the loop shows (the index goes on, as the standard says).
fn make_iter(d: &Dom, it: &mut Interp<'_>, this: Value, kind: u8) -> R<Value> {
    d.on_headers(it, this, |_| Ok(()))?;
    let Value::Object(headers) = this else { return Err(it.type_error("Illegal invocation")) };
    d.reserve(it, 1, 0)?;
    Ok(Value::Object(d.add_obj(it, Pr::HeadersIter, F::HeadersIter { headers, idx: 0, kind })?))
}

fn headers_keys(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> R<Value> {
    make_iter(d, it, this, 0)
}
fn headers_values(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> R<Value> {
    make_iter(d, it, this, 1)
}
fn headers_entries(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> R<Value> {
    make_iter(d, it, this, 2)
}

fn headers_iter_next(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> R<Value> {
    let o = d.brand(it, this)?;
    let (headers, idx, kind) = match d.fetch.st.borrow().objs.get(&o) {
        Some(&F::HeadersIter { headers, idx, kind }) => (headers, idx, kind),
        _ => return Err(it.type_error("Illegal invocation")),
    };
    it.charge(1 + d.list_len(headers) as u64)?;
    let item = match d.fetch.st.borrow().objs.get(&headers) {
        Some(F::Headers(h)) => h.nth_sorted(idx),
        _ => None,
    };
    let (value, done) = match item {
        None => (Value::Undefined, true),
        Some((n, v)) => {
            if let Some(F::HeadersIter { idx, .. }) = d.fetch.st.borrow_mut().objs.get_mut(&o) {
                *idx += 1;
            }
            match kind {
                0 => (d.js_str(it, &n)?, false),
                1 => (d.js_str(it, &v)?, false),
                _ => {
                    let (jn, jv) = (d.js_str(it, &n)?, d.js_str(it, &v)?);
                    (it.array_from(&[jn, jv])?, false)
                }
            }
        }
    };
    let r = d.obj(it, it.object_prototype())?;
    let (kv, kd) = (d.key(it, "value")?, d.key(it, "done")?);
    it.define_data(r, kv, value, true, true, true)?;
    it.define_data(r, kd, Value::Bool(done), true, true, true)?;
    Ok(Value::Object(r))
}

// ---------------------------------------------------------------------------------------------- Request and Response

fn common(st: &mut FetchState, o: ObjId) -> Option<&mut Common> {
    match st.objs.get_mut(&o)? {
        F::Request(r) => Some(&mut r.c),
        F::Response(r) => Some(&mut r.c),
        _ => None,
    }
}

fn new_request_obj(d: &Dom, it: &mut Interp<'_>, core: Core) -> R<ObjId> {
    let n = core.body.as_ref().map_or(0, |b| b.len());
    d.reserve(it, 2, n)?;
    let h = d.headers_obj(it, core.headers)?;
    let signal = match core.signal {
        Some(s) => Some(d.follow_signal(it, s)?),
        None => None,
    };
    d.add_obj(it, Pr::Request, F::Request(ReqData { c: Common { headers: h, body: Body::new(core.body) }, method: core.method, url: core.url, redirect: core.redirect, credentials: core.credentials, mode: core.mode, signal }))
}

fn request_ctor(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> R<Value> {
    if a.is_empty() {
        return Err(it.type_error("Failed to construct 'Request': 1 argument required, but only 0 present."));
    }
    let core = make_core(d, it, arg(a, 0), arg(a, 1))?;
    Ok(Value::Object(new_request_obj(d, it, core)?))
}

fn status_null_body(s: u16) -> bool {
    matches!(s, 101 | 103 | 204 | 205 | 304)
}

fn response_ctor(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> R<Value> {
    let lim = d.fl();
    let init = arg(a, 1);
    let (mut status, mut text, mut list) = (200u16, String::new(), HeaderList::new(Guard::Response));
    if !matches!(init, Value::Undefined | Value::Null) {
        if !matches!(init, Value::Object(_)) {
            return Err(it.type_error("The response init is not an object"));
        }
        let s = d.prop(it, init, "status")?;
        if !matches!(s, Value::Undefined) {
            let n = it.to_js_number(s)?;
            if !(200.0..=599.0).contains(&n) {
                return Err(it.range_error("init[\"status\"] must be in the range of 200 to 599, inclusive."));
            }
            status = n as u16;
        }
        let t = d.prop(it, init, "statusText")?;
        if !matches!(t, Value::Undefined) {
            text = d.string_arg(it, t)?;
            if text.chars().any(|c| matches!(c, '\r' | '\n') || c as u32 > 0xFF) {
                return Err(it.type_error("Invalid statusText"));
            }
        }
        let h = d.prop(it, init, "headers")?;
        d.fill(it, &mut list, h)?;
    }
    let body = match d.extract_body(it, arg(a, 0))? {
        Some((bytes, ty)) => {
            if status_null_body(status) {
                return Err(it.type_error("Response with null body status cannot have body"));
            }
            if let Some(ty) = ty && list.get("content-type").is_none() {
                list.append(&lim, "content-type".into(), ty.into()).map_err(|e| d.herr(it, e))?;
            }
            Some(bytes)
        }
        None => None,
    };
    d.reserve(it, 2, body.as_ref().map_or(0, |b| b.len()))?;
    let h = d.headers_obj(it, list)?;
    let r = RespData { c: Common { headers: h, body: Body::new(body) }, status, status_text: text, url: String::new(), redirected: false, ty: "default", stream: None };
    Ok(Value::Object(d.add_obj(it, Pr::Response, F::Response(r))?))
}

fn with_req<T>(d: &Dom, it: &mut Interp<'_>, this: Value, f: impl FnOnce(&ReqData) -> T) -> R<T> {
    let o = d.brand(it, this)?;
    match d.fetch.st.borrow().objs.get(&o) {
        Some(F::Request(r)) => Ok(f(r)),
        _ => Err(it.type_error("Illegal invocation")),
    }
}

fn with_resp<T>(d: &Dom, it: &mut Interp<'_>, this: Value, f: impl FnOnce(&RespData) -> T) -> R<T> {
    let o = d.brand(it, this)?;
    match d.fetch.st.borrow().objs.get(&o) {
        Some(F::Response(r)) => Ok(f(r)),
        _ => Err(it.type_error("Illegal invocation")),
    }
}

fn str_val(d: &Dom, it: &mut Interp<'_>, s: String) -> R<Value> {
    d.js_str(it, &s)
}

fn req_method(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> R<Value> {
    let s = with_req(d, it, this, |r| r.method.clone())?;
    str_val(d, it, s)
}
fn req_url(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> R<Value> {
    let s = with_req(d, it, this, |r| r.url.to_string())?;
    str_val(d, it, s)
}
fn req_headers(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> R<Value> {
    Ok(Value::Object(with_req(d, it, this, |r| r.c.headers)?))
}
fn req_redirect(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> R<Value> {
    let s = with_req(d, it, this, |r| if r.redirect == Redirect::Follow { "follow" } else { "error" }.to_string())?;
    str_val(d, it, s)
}
fn req_credentials(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> R<Value> {
    let s = with_req(d, it, this, |r| r.credentials.to_string())?;
    str_val(d, it, s)
}
fn req_mode(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> R<Value> {
    let s = with_req(d, it, this, |r| r.mode.to_string())?;
    str_val(d, it, s)
}
fn req_body_used(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> R<Value> {
    Ok(Value::Bool(with_req(d, it, this, |r| r.c.body.used)?))
}
fn req_clone(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> R<Value> {
    let (method, url, redirect, credentials, mode, hdr, body, used, signal) = with_req(d, it, this, |r| (r.method.clone(), dup(&r.url), r.redirect, r.credentials, r.mode, r.c.headers, r.c.body.bytes.clone(), r.c.body.used || r.c.body.locked, r.signal))?;
    if used {
        return Err(it.type_error("Failed to execute 'clone' on 'Request': Request body is already used"));
    }
    let mut headers = d.list_of(hdr);
    headers.guard = Guard::Request;
    Ok(Value::Object(new_request_obj(d, it, Core { method, url, headers, body, redirect, credentials, mode, signal })?))
}
fn req_signal(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> R<Value> {
    let o = d.brand(it, this)?;
    if let Some(F::Request(ReqData { signal: Some(s), .. })) = d.fetch.st.borrow().objs.get(&o) {
        return Ok(Value::Object(*s));
    }
    let s = d.fresh_signal(it)?;
    if let Some(F::Request(r)) = d.fetch.st.borrow_mut().objs.get_mut(&o) {
        r.signal = Some(s);
    }
    Ok(Value::Object(s))
}
fn gate_request_body(_: &Dom, _: &mut Interp<'_>, _: Value, _: &[Value]) -> R<Value> {
    Err(Error::Unsupported(unsupported("Request.body (a request body stream)")))
}

fn resp_status(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> R<Value> {
    Ok(Value::Number(with_resp(d, it, this, |r| r.status)? as f64))
}
fn resp_ok(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> R<Value> {
    Ok(Value::Bool(with_resp(d, it, this, |r| (200..300).contains(&r.status))?))
}
fn resp_status_text(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> R<Value> {
    let s = with_resp(d, it, this, |r| r.status_text.clone())?;
    str_val(d, it, s)
}
fn resp_url(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> R<Value> {
    let s = with_resp(d, it, this, |r| r.url.clone())?;
    str_val(d, it, s)
}
fn resp_redirected(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> R<Value> {
    Ok(Value::Bool(with_resp(d, it, this, |r| r.redirected)?))
}
fn resp_type(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> R<Value> {
    let s = with_resp(d, it, this, |r| r.ty.to_string())?;
    str_val(d, it, s)
}
fn resp_headers(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> R<Value> {
    Ok(Value::Object(with_resp(d, it, this, |r| r.c.headers)?))
}
fn resp_body_used(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> R<Value> {
    Ok(Value::Bool(with_resp(d, it, this, |r| r.c.body.used)?))
}
fn resp_clone(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> R<Value> {
    let (c_hdr, body, used, status, text, url, redirected, ty) =
        with_resp(d, it, this, |r| (r.c.headers, r.c.body.bytes.clone(), r.c.body.used || r.c.body.locked, r.status, r.status_text.clone(), r.url.clone(), r.redirected, r.ty))?;
    if used {
        return Err(it.type_error("Failed to execute 'clone' on 'Response': Response body is already used"));
    }
    let mut list = d.list_of(c_hdr);
    list.guard = if ty == "basic" { Guard::Immutable } else { Guard::Response };
    d.reserve(it, 2, body.as_ref().map_or(0, |b| b.len()))?;
    let h = d.headers_obj(it, list)?;
    let r = RespData { c: Common { headers: h, body: Body::new(body) }, status, status_text: text, url, redirected, ty, stream: None };
    Ok(Value::Object(d.add_obj(it, Pr::Response, F::Response(r))?))
}

enum As {
    Blob,
    Form,
    Text,
    Json,
    Buffer,
}

/// Take the whole body (the one consumer). `TypeError` if it is used or locked.
fn take_body(d: &Dom, it: &mut Interp<'_>, this: Value) -> R<Option<Rc<Vec<u8>>>> {
    let o = d.brand(it, this)?;
    let mut st = d.fetch.st.borrow_mut();
    let Some(c) = common(&mut st, o) else { return Err(it.type_error("Illegal invocation")) };
    if c.body.used || c.body.locked {
        let msg = if c.body.used { "Body has already been consumed." } else { "Body is locked." };
        return Err(it.type_error(msg));
    }
    // A `null` body is never "used".
    c.body.used = c.body.bytes.is_some();
    let bytes = c.body.bytes.take();
    let n = bytes.as_ref().map_or(0, |b| b.len());
    st.retained -= n;
    Ok(bytes)
}

fn consume(d: &Dom, it: &mut Interp<'_>, this: Value, how: As) -> R<Value> {
    let p = it.host_new_promise()?;
    let r = (|| -> R<Value> {
        let ct = match how {
            As::Blob | As::Form => {
                let o = d.brand(it, this)?;
                let h = common(&mut d.fetch.st.borrow_mut(), o).map(|c| c.headers);
                h.and_then(|h| d.list_of(h).get("content-type")).unwrap_or_default()
            }
            _ => String::new(),
        };
        let bytes = take_body(d, it, this)?;
        let bytes = bytes.unwrap_or_default();
        let n = bytes.len();
        match how {
            As::Blob => {
                it.charge((n / 64) as u64)?;
                crate::browser_web::blob_from_bytes(d, it, Rc::try_unwrap(bytes).unwrap_or_else(|rc| (*rc).clone()), ct)
            }
            As::Form => {
                it.charge((n / 16) as u64)?;
                crate::browser_web::form_from(d, it, &bytes, &ct)?.ok_or_else(|| it.type_error("Failed to fetch"))
            }
            As::Buffer => {
                it.charge((n / 64) as u64)?;
                let v = Rc::try_unwrap(bytes).unwrap_or_else(|rc| (*rc).clone());
                it.new_array_buffer(v)
            }
            As::Text | As::Json => {
                it.charge((n / 16) as u64)?;
                let body = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&bytes);
                let text = String::from_utf8_lossy(body);
                let s = d.js_str(it, &text)?;
                if let As::Text = how {
                    return Ok(s);
                }
                let Some(parse) = d.fetch.json_parse.get() else { return Err(Error::Unsupported(unsupported("Body.json without JSON.parse"))) };
                it.charge((n / 16) as u64)?;
                it.call(parse, Value::Undefined, &[s])
            }
        }
    })();
    settle(it, p, r)
}

fn body_text(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> R<Value> {
    consume(d, it, this, As::Text)
}
fn body_json(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> R<Value> {
    consume(d, it, this, As::Json)
}
fn body_array_buffer(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> R<Value> {
    consume(d, it, this, As::Buffer)
}
fn body_blob(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> R<Value> {
    consume(d, it, this, As::Blob)
}
fn body_form_data(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> R<Value> {
    consume(d, it, this, As::Form)
}

// ---------------------------------------------------------------------------------------------- streams

fn resp_body(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> R<Value> {
    let o = d.brand(it, this)?;
    let (stream, has_body) = match d.fetch.st.borrow().objs.get(&o) {
        Some(F::Response(r)) => (r.stream, r.c.body.bytes.is_some() || r.c.body.used),
        _ => return Err(it.type_error("Illegal invocation")),
    };
    if let Some(s) = stream {
        return Ok(Value::Object(s));
    }
    if !has_body {
        return Ok(Value::Null);
    }
    d.reserve(it, 1, 0)?;
    let s = d.add_obj(it, Pr::Stream, F::Stream(o))?;
    if let Some(F::Response(r)) = d.fetch.st.borrow_mut().objs.get_mut(&o) {
        r.stream = Some(s);
    }
    Ok(Value::Object(s))
}

fn illegal(_: &Dom, it: &mut Interp<'_>, _: Value, _: &[Value]) -> R<Value> {
    Err(it.type_error("Illegal constructor"))
}

fn stream_owner(d: &Dom, it: &mut Interp<'_>, this: Value) -> R<(ObjId, ObjId)> {
    let o = d.brand(it, this)?;
    match d.fetch.st.borrow().objs.get(&o) {
        Some(F::Stream(r)) => Ok((o, *r)),
        _ => Err(it.type_error("Illegal invocation")),
    }
}

pub(crate) fn stream_locked(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> R<Value> {
    let (_, r) = stream_owner(d, it, this)?;
    let locked = match d.fetch.st.borrow().objs.get(&r) {
        Some(F::Response(r)) => r.c.body.locked,
        _ => false,
    };
    Ok(Value::Bool(locked))
}

pub(crate) fn stream_get_reader(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> R<Value> {
    let (s, r) = stream_owner(d, it, this)?;
    if !matches!(arg(a, 0), Value::Undefined) {
        let mode = d.prop(it, arg(a, 0), "mode")?;
        if !matches!(mode, Value::Undefined) {
            return Err(Error::Unsupported(unsupported("ReadableStream BYOB reader")));
        }
    }
    if d.fetch.st.borrow().readers >= d.fl().max_readers {
        return Err(d.lim("fetch-readers"));
    }
    d.reserve(it, 1, 0)?;
    {
        let mut st = d.fetch.st.borrow_mut();
        match st.objs.get_mut(&r) {
            Some(F::Response(resp)) if !resp.c.body.locked => resp.c.body.locked = true,
            _ => return Err(it.type_error("ReadableStream is locked")),
        }
    }
    Ok(Value::Object(d.add_obj(it, Pr::Reader, F::Reader { stream: s, released: false })?))
}

pub(crate) fn stream_cancel(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> R<Value> {
    let p = it.host_new_promise()?;
    let r = (|| -> R<Value> {
        let (_, resp) = stream_owner(d, it, this)?;
        let mut st = d.fetch.st.borrow_mut();
        let Some(c) = common(&mut st, resp) else { return Err(it.type_error("Illegal invocation")) };
        if c.body.locked {
            return Err(it.type_error("Cannot cancel a stream that already has a reader"));
        }
        drop_body(&mut st, resp);
        Ok(Value::Undefined)
    })();
    settle(it, p, r)
}

/// Mark the body used and free its bytes.
fn drop_body(st: &mut FetchState, o: ObjId) {
    if let Some(c) = common(st, o) {
        c.body.used = true;
        let n = c.body.len();
        c.body.bytes = None;
        st.retained -= n;
    }
}

fn reader_parts(d: &Dom, it: &mut Interp<'_>, this: Value) -> R<(ObjId, ObjId, bool)> {
    let o = d.brand(it, this)?;
    let (stream, released) = match d.fetch.st.borrow().objs.get(&o) {
        Some(F::Reader { stream, released }) => (*stream, *released),
        _ => return Err(it.type_error("Illegal invocation")),
    };
    let resp = match d.fetch.st.borrow().objs.get(&stream) {
        Some(F::Stream(r)) => *r,
        _ => return Err(it.type_error("Illegal invocation")),
    };
    Ok((o, resp, released))
}

pub(crate) fn reader_read(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> R<Value> {
    let p = it.host_new_promise()?;
    let r = (|| -> R<Value> {
        let (_, resp, released) = reader_parts(d, it, this)?;
        if released {
            return Err(it.type_error("This readable stream reader has been released and cannot be used to read from its previous owner stream"));
        }
        let chunk_max = d.fl().chunk_bytes.max(1);
        let chunk = {
            let mut st = d.fetch.st.borrow_mut();
            let Some(c) = common(&mut st, resp) else { return Err(it.type_error("Illegal invocation")) };
            c.body.used = true;
            let total = c.body.len();
            if c.body.pos >= total {
                None
            } else {
                let n = chunk_max.min(total - c.body.pos);
                let bytes = c.body.bytes.as_ref().expect("pos < len").clone();
                Some((bytes, c.body.pos, n))
            }
        };
        let (value, done) = match chunk {
            None => {
                drop_body(&mut d.fetch.st.borrow_mut(), resp);
                (Value::Undefined, true)
            }
            Some((bytes, pos, n)) => {
                it.charge((n / 64) as u64)?;
                let v = it.new_uint8_array(bytes[pos..pos + n].to_vec())?;
                let mut st = d.fetch.st.borrow_mut();
                let finished = match common(&mut st, resp) {
                    Some(c) => {
                        c.body.pos = pos + n;
                        c.body.pos >= c.body.len()
                    }
                    None => false,
                };
                if finished {
                    // Every byte was handed out: free the copy now. The next read says `done`.
                    let keep = common(&mut st, resp).map(|c| c.body.len()).unwrap_or(0);
                    if let Some(c) = common(&mut st, resp) {
                        c.body.bytes = Some(Rc::new(Vec::new()));
                        c.body.pos = 0;
                    }
                    st.retained -= keep;
                }
                (v, false)
            }
        };
        let res = d.obj(it, it.object_prototype())?;
        let (kv, kd) = (d.key(it, "value")?, d.key(it, "done")?);
        it.define_data(res, kv, value, true, true, true)?;
        it.define_data(res, kd, Value::Bool(done), true, true, true)?;
        Ok(Value::Object(res))
    })();
    settle(it, p, r)
}

pub(crate) fn reader_cancel(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> R<Value> {
    let p = it.host_new_promise()?;
    let r = (|| -> R<Value> {
        let (_, resp, released) = reader_parts(d, it, this)?;
        if released {
            return Err(it.type_error("This readable stream reader has been released and cannot be used to cancel its previous owner stream"));
        }
        drop_body(&mut d.fetch.st.borrow_mut(), resp);
        Ok(Value::Undefined)
    })();
    settle(it, p, r)
}

pub(crate) fn reader_release(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> R<Value> {
    let (o, resp, released) = reader_parts(d, it, this)?;
    if !released {
        let mut st = d.fetch.st.borrow_mut();
        if let Some(F::Reader { released, .. }) = st.objs.get_mut(&o) {
            *released = true;
        }
        if let Some(c) = common(&mut st, resp) {
            c.body.locked = false;
        }
    }
    Ok(Value::Undefined)
}

pub(crate) fn gate_reader_closed(_: &Dom, _: &mut Interp<'_>, _: Value, _: &[Value]) -> R<Value> {
    Err(Error::Unsupported(unsupported("ReadableStreamDefaultReader.closed")))
}

// ---------------------------------------------------------------------------------------------- fetch

fn fetch(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> R<Value> {
    let p = it.host_new_promise()?;
    match prepare(d, it, p, a) {
        Ok(()) => Ok(p),
        Err(e) => settle(it, p, Err(e)),
    }
}

/// Validate, apply the policy and queue. A `TypeError` rejects the promise; every other error ends the call.
fn prepare(d: &Dom, it: &mut Interp<'_>, promise: Value, a: &[Value]) -> R<()> {
    let lim = d.fl();
    {
        let st = d.fetch.st.borrow();
        if st.queue.len() >= lim.max_pending {
            return Err(d.lim("fetch-queue"));
        }
        if st.started >= lim.max_started {
            return Err(d.lim("fetch-requests"));
        }
    }
    let core = make_core(d, it, arg(a, 0), arg(a, 1))?;
    if let Some((true, reason)) = core.signal.and_then(|s| d.signal_state(s)) {
        return Err(Error::Throw(reason));
    }
    // Policy: no DNS and no socket before this passes.
    let method = wire_method(&core.method)?;
    policy(d, &lim, &core.url)?;
    let n = core.body.as_ref().map_or(0, |b| b.len());
    {
        let mut st = d.fetch.st.borrow_mut();
        if st.retained.saturating_add(st.queued_bytes).saturating_add(n) > lim.max_retained {
            return Err(d.lim("fetch-retained"));
        }
        if let Some(s) = core.signal {
            drop(st);
            d.signal_add_algo(s, crate::browser_abort::Algo::Fetch(promise))?;
            st = d.fetch.st.borrow_mut();
        }
        st.queued_bytes += n;
        st.started += 1;
        // Wire headers: combined by name. `Accept` has a default. The client adds Host, Content-Length and User-Agent.
        let mut headers: Vec<(String, String)> = Vec::new();
        for (name, _) in &core.headers.list {
            if !headers.iter().any(|(k, _)| k == name) {
                headers.push((name.clone(), core.headers.get(name).unwrap_or_default()));
            }
        }
        if !headers.iter().any(|(k, _)| k == "accept") {
            headers.push(("accept".into(), "*/*".into()));
        }
        st.queue.push_back(Pending { promise, method, url: core.url, headers, body: core.body, redirect: core.redirect, xhr: None, queued: bare_net::trace::on().then(std::time::Instant::now), started: None });
    }
    Ok(())
}

/// The origin policy: a configured document origin, and the same origin for the request.
fn policy(d: &Dom, lim: &FetchLimits, url: &Url) -> R<()> {
    let base = d.fetch_base();
    let mut st = d.fetch.st.borrow_mut();
    let Some(base) = base else {
        st.diag(lim, "blocked: no document origin is configured".into());
        return Err(Error::Unsupported(unsupported("fetch without a configured document origin (Browser::configure_fetch)")));
    };
    if !base.same_origin(url) {
        let text = format!("blocked: cross-origin request to {} (CORS is a gate)", origin(url));
        st.diag(lim, text);
        return Err(Error::Unsupported(unsupported("fetch to another origin (CORS is not implemented)")));
    }
    Ok(())
}

/// Queue the request of an `XMLHttpRequest` with the policy, the caps and the queue of `fetch`. `headers` are combined by name.
pub(crate) fn queue_xhr(d: &Dom, x: (ObjId, u64), method: &str, url: Url, mut headers: Vec<(String, String)>, body: Option<Rc<Vec<u8>>>) -> R<()> {
    let lim = d.fl();
    {
        let st = d.fetch.st.borrow();
        if st.queue.len() >= lim.max_pending {
            return Err(d.lim("fetch-queue"));
        }
        if st.started >= lim.max_started {
            return Err(d.lim("fetch-requests"));
        }
    }
    let method = wire_method(method)?;
    policy(d, &lim, &url)?;
    let n = body.as_ref().map_or(0, |b| b.len());
    let mut st = d.fetch.st.borrow_mut();
    if st.retained.saturating_add(st.queued_bytes).saturating_add(n) > lim.max_retained {
        return Err(d.lim("fetch-retained"));
    }
    st.queued_bytes += n;
    st.started += 1;
    if !headers.iter().any(|(k, _)| k == "accept") {
        headers.push(("accept".into(), "*/*".into()));
    }
    st.queue.push_back(Pending { promise: Value::Undefined, method, url, headers, body, redirect: Redirect::Follow, xhr: Some(x), queued: bare_net::trace::on().then(std::time::Instant::now), started: None });
    Ok(())
}

/// Take the request of an `XMLHttpRequest` out of the queue, if it is still there.
pub(crate) fn dequeue_xhr(d: &Dom, x: ObjId) {
    let mut st = d.fetch.st.borrow_mut();
    if let Some(i) = st.queue.iter().position(|t| t.xhr.is_some_and(|(o, _)| o == x)) {
        let t = st.queue.remove(i).expect("position");
        st.queued_bytes -= t.body.as_ref().map_or(0, |b| b.len());
    }
}

// ---------------------------------------------------------------------------------------------- the network driver

pub(crate) enum Outcome {
    Done { status: u16, url: Url, redirected: bool, headers: Vec<(String, String)>, body: Vec<u8> },
    /// A network error or a policy stop. The label holds no header value, path or query.
    Failed(&'static str),
    /// The reply is bigger than the cap: a resource error, not a JS error.
    TooBig,
}

fn run_one(client: &mut Client, lim: &FetchLimits, first: Parts<'_>, diag: &mut Vec<String>) -> Outcome {
    let first_origin = dup(first.url);
    let (mut method, mut url, mut headers, mut body) = (first.method, dup(first.url), first.headers.to_vec(), first.body);
    let mut redirected = false;
    for hop in 0..=lim.max_redirects {
        let wire: Vec<(&str, &str)> = headers.iter().map(|(n, v)| (n.as_str(), v.as_str())).collect();
        let req = NetRequest { method, url: &url, headers: &wire, body: body.unwrap_or(&[]) };
        let resp = match client.send_once(&req, lim.max_response_body) {
            Ok(r) => r,
            Err(e) if e.to_string() == "the reply is too big" => return Outcome::TooBig,
            Err(_) => return Outcome::Failed("network error"),
        };
        let resp = unwrap_arc(resp);
        if matches!(resp.status, 301 | 302 | 303 | 307 | 308)
            && let Some(location) = resp.headers.iter().find(|(n, _)| n == "location").map(|(_, v)| v.clone())
        {
            if first.redirect == Redirect::Error {
                return Outcome::Failed("redirect refused (redirect: error)");
            }
            let Ok(target) = url.join(&location) else { return Outcome::Failed("redirect to an invalid URL") };
            // Checked before any DNS or socket for the target.
            if url.scheme == Scheme::Https && target.scheme == Scheme::Http {
                diag.push(format!("blocked: https to http redirect from {}", origin(&url)));
                return Outcome::Failed("https to http redirect refused");
            }
            if !first_origin.same_origin(&target) {
                diag.push(format!("blocked: cross-origin redirect from {} to {} (CORS is a gate)", origin(&url), origin(&target)));
                return Outcome::Failed("cross-origin redirect refused");
            }
            if hop == lim.max_redirects {
                return Outcome::Failed("too many redirects");
            }
            // Fetch standard, "HTTP-redirect fetch": 301/302 turn a POST into a GET; 303 turns all but GET and HEAD into a GET; 307/308 keep both.
            if (method == Method::Post && matches!(resp.status, 301 | 302)) || (resp.status == 303 && !matches!(method, Method::Get | Method::Head)) {
                method = Method::Get;
                body = None;
                headers.retain(|(n, _)| !["content-type", "content-encoding", "content-language", "content-location"].contains(&n.as_str()));
            }
            url = target;
            redirected = true;
            continue;
        }
        if resp.encoded() {
            diag.push(format!("blocked: compressed response from {} (not supported)", origin(&url)));
            return Outcome::Failed("compressed response");
        }
        return Outcome::Done { status: resp.status, url, redirected, headers: resp.headers, body: resp.body };
    }
    Outcome::Failed("too many redirects")
}

fn unwrap_arc(a: std::sync::Arc<bare_net::http::Response>) -> bare_net::http::Response {
    std::sync::Arc::try_unwrap(a).unwrap_or_else(|a| (*a).clone())
}

fn finish(d: &Dom, it: &mut Interp<'_>, promise: Value, outcome: Outcome) -> R<()> {
    let r = match outcome {
        Outcome::Failed(_) => {
            let e = it.type_error("Failed to fetch");
            Err(e)
        }
        Outcome::TooBig => return Err(d.lim("fetch-response-body")),
        Outcome::Done { status, url, redirected, headers, body } => build_response(d, it, status, url, redirected, headers, body),
    };
    settle(it, promise, r).map(|_| ())
}

fn build_response(d: &Dom, it: &mut Interp<'_>, status: u16, url: Url, redirected: bool, headers: Vec<(String, String)>, body: Vec<u8>) -> R<Value> {
    let lim = d.fl();
    let mut list = HeaderList::new(Guard::Response);
    for (n, v) in headers {
        let v = v.trim_matches(['\t', ' ']).to_string();
        list.append(&lim, n.to_ascii_lowercase(), v).map_err(|e| d.herr(it, e))?;
    }
    list.guard = Guard::Immutable;
    let body = (!status_null_body(status)).then(|| Rc::new(body));
    d.reserve(it, 2, body.as_ref().map_or(0, |b| b.len()))?;
    let h = d.headers_obj(it, list)?;
    let r = RespData { c: Common { headers: h, body: Body::new(body) }, status, status_text: String::new(), url: url.to_string(), redirected, ty: "basic", stream: None };
    Ok(Value::Object(d.add_obj(it, Pr::Response, F::Response(r))?))
}

impl Browser {
    /// Set the document origin and the HTTP client. Call it before scripts run. Without it, `fetch` is `Unsupported`.
    pub fn configure_fetch(&mut self, cfg: FetchConfig) -> Result<(), String> {
        let base = cfg.base_url.as_deref().map(Url::parse).transpose()?;
        let f = &self.dom.fetch;
        f.lim.set(cfg.limits);
        *f.client.borrow_mut() = cfg.client;
        f.st.borrow_mut().base = base;
        Ok(())
    }

    /// The hook for the document URL (a navigation or `history.pushState` changes it). It sets the origin that `fetch` may
    /// reach and resolves relative URLs. Requests already queued keep the origin they were made for.
    pub fn update_fetch_base_url(&mut self, url: &str) -> Result<(), String> {
        let base = Url::parse(url)?;
        self.dom.fetch.st.borrow_mut().base = Some(base);
        Ok(())
    }

    /// Requests that wait for [`Browser::run_network_tasks`].
    pub fn pending_fetches(&self) -> usize {
        self.dom.fetch.st.borrow().queue.len()
    }

    /// Run up to `max` queued requests, oldest first. Each one does its blocking I/O, settles its promise and runs a microtask
    /// checkpoint, so a callback may queue the next request: it runs in this call if `max` allows. Returns how many ran.
    /// A `Limit` ends the call (the VM is then unusable, as for any other limit).
    pub fn run_network_tasks(&mut self, max: usize) -> R<usize> {
        let dom = self.dom.clone();
        if let Some(e) = dom.fetch.st.borrow().halted { return Err(e); }
        let result = self.run_network_tasks_inner(max);
        if let Err(e) = result { dom.fetch.st.borrow_mut().halted = Some(e); }
        result
    }

    /// Start the I/O of the first queued requests on worker threads, so that they run together. They settle in the order of the queue,
    /// as before: only the waiting is shared. At most `MAX_WORKERS` run at once, and the queue is not scanned further.
    /// A request that is not a GET or a HEAD may change the server. It runs when all before it are done, on the thread that settles
    /// it (a worker gains nothing: nothing after it starts before it is done), so the server sees these requests in the order of the script.
    fn start_queued(&self) {
        if self.dom.fetch.st.borrow().queue.len() < 2 {
            // One request alone gains nothing from a worker thread: this thread makes it.
            return;
        }
        let (lim, client) = (self.dom.fl(), self.dom.fetch.client.borrow());
        if !client.parallel_pays() {
            return;
        }
        for p in self.dom.fetch.st.borrow_mut().queue.iter_mut().take(MAX_WORKERS) {
            if !p.method.safe() {
                break;
            }
            if p.started.is_none() {
                let wire = p.wire();
                p.started = Some(client.spawn(move |client| {
                    if let Some(t) = wire.queued {
                        bare_net::trace::wanted(t);
                    }
                    let mut diag = Vec::new();
                    (run_one(client, &lim, wire.parts(), &mut diag), diag)
                }));
            }
        }
    }

    fn run_network_tasks_inner(&mut self, max: usize) -> R<usize> {
        let dom = self.dom.clone();
        self.it.host_scope(|it| it.host_checkpoint().map(|_| ()))?;
        let mut n = 0;
        while n < max {
            let lim = dom.fl();
            self.start_queued();
            let Some(mut task) = ({
                let mut st = dom.fetch.st.borrow_mut();
                let t = st.queue.pop_front();
                if let Some(t) = &t {
                    st.queued_bytes -= t.body.as_ref().map_or(0, |b| b.len());
                }
                t
            }) else {
                break;
            };
            n += 1;
            let (outcome, diag) = bare_net::trace::blocked(|| match task.started.take() {
                // A lost worker is a network error.
                Some(t) => t.wait().unwrap_or((Outcome::Failed("network error"), Vec::new())),
                None => {
                    let mut diag = Vec::new();
                    if let Some(t) = task.queued {
                        bare_net::trace::wanted(t);
                    }
                    (run_one(&mut dom.fetch.client.borrow_mut(), &lim, task.parts(), &mut diag), diag)
                }
            });
            {
                let mut st = dom.fetch.st.borrow_mut();
                if let Outcome::Failed(label) = &outcome {
                    let text = format!("failed: {label} ({})", origin(&task.url));
                    st.diag(&lim, text);
                }
                for t in diag {
                    st.diag(&lim, t);
                }
            }
            if let Some((x, seq)) = task.xhr {
                self.xhr_deliver(x, seq, outcome)?;
                continue;
            }
            let promise = task.promise;
            self.it.host_scope(|it| {
                finish(&dom, it, promise, outcome)?;
                it.host_checkpoint().map(|_| ())
            })?;
        }
        Ok(n)
    }

    /// Reject every queued request with an `AbortError`. A request that already started its I/O cannot be interrupted (it runs
    /// to its end inside `run_network_tasks`), so this only affects requests that have not begun. Returns how many were aborted.
    pub fn abort_fetches(&mut self) -> R<usize> {
        self.run_network_tasks(0)?;
        let result = self.abort_fetches_inner();
        if let Err(e) = result { self.dom.fetch.st.borrow_mut().halted = Some(e); }
        result
    }

    fn abort_fetches_inner(&mut self) -> R<usize> {
        let dom = self.dom.clone();
        let tasks: Vec<Pending> = {
            let mut st = dom.fetch.st.borrow_mut();
            let t: Vec<Pending> = st.queue.drain(..).collect();
            st.queued_bytes = 0;
            t
        };
        let n = tasks.len();
        self.it.host_scope(|it| {
            for t in tasks {
                if let Some((x, seq)) = t.xhr {
                    dom.xhr_abort_queued(it, x, seq)?;
                    continue;
                }
                let e = dom.dom_err(it, "AbortError", "The operation was aborted.");
                let v = thrown_value(e)?;
                it.host_reject_promise(t.promise, v)?;
            }
            it.host_checkpoint().map(|_| ())
        })?;
        Ok(n)
    }

    /// Fixed-label notes about requests that were blocked or failed (origin only, never a path, query, header or body). Clears them.
    pub fn take_fetch_diagnostics(&mut self) -> Vec<String> {
        self.dom.fetch.st.borrow_mut().diag.drain(..).collect()
    }

    pub fn fetch_stats(&self) -> FetchStats {
        let st = self.dom.fetch.st.borrow();
        FetchStats { pending: st.queue.len(), objects: st.objs.len(), readers: st.readers, retained_bytes: st.retained, queued_bytes: st.queued_bytes, started: st.started, dropped_diagnostics: st.dropped_diag }
    }
}

// ---------------------------------------------------------------------------------------------- install

use Kind::{Get as G, Method as M};
use Pr::*;

struct Def {
    on: Pr,
    name: &'static str,
    len: u32,
    kind: Kind,
}

static DEFS: &[Def] = &[
    Def { on: Headers, name: "append", len: 2, kind: M(headers_append) },
    Def { on: Headers, name: "delete", len: 1, kind: M(headers_delete) },
    Def { on: Headers, name: "get", len: 1, kind: M(headers_get) },
    Def { on: Headers, name: "getSetCookie", len: 0, kind: M(headers_get_set_cookie) },
    Def { on: Headers, name: "has", len: 1, kind: M(headers_has) },
    Def { on: Headers, name: "set", len: 2, kind: M(headers_set) },
    Def { on: Headers, name: "forEach", len: 1, kind: M(headers_for_each) },
    Def { on: Headers, name: "entries", len: 0, kind: M(headers_entries) },
    Def { on: Headers, name: "keys", len: 0, kind: M(headers_keys) },
    Def { on: Headers, name: "values", len: 0, kind: M(headers_values) },
    Def { on: HeadersIter, name: "next", len: 0, kind: M(headers_iter_next) },
    Def { on: Request, name: "method", len: 0, kind: G(req_method) },
    Def { on: Request, name: "url", len: 0, kind: G(req_url) },
    Def { on: Request, name: "headers", len: 0, kind: G(req_headers) },
    Def { on: Request, name: "redirect", len: 0, kind: G(req_redirect) },
    Def { on: Request, name: "credentials", len: 0, kind: G(req_credentials) },
    Def { on: Request, name: "mode", len: 0, kind: G(req_mode) },
    Def { on: Request, name: "bodyUsed", len: 0, kind: G(req_body_used) },
    Def { on: Request, name: "body", len: 0, kind: G(gate_request_body) },
    Def { on: Request, name: "signal", len: 0, kind: G(req_signal) },
    Def { on: Request, name: "clone", len: 0, kind: M(req_clone) },
    Def { on: Request, name: "text", len: 0, kind: M(body_text) },
    Def { on: Request, name: "json", len: 0, kind: M(body_json) },
    Def { on: Request, name: "arrayBuffer", len: 0, kind: M(body_array_buffer) },
    Def { on: Request, name: "blob", len: 0, kind: M(body_blob) },
    Def { on: Request, name: "formData", len: 0, kind: M(body_form_data) },
    Def { on: Response, name: "status", len: 0, kind: G(resp_status) },
    Def { on: Response, name: "ok", len: 0, kind: G(resp_ok) },
    Def { on: Response, name: "statusText", len: 0, kind: G(resp_status_text) },
    Def { on: Response, name: "url", len: 0, kind: G(resp_url) },
    Def { on: Response, name: "redirected", len: 0, kind: G(resp_redirected) },
    Def { on: Response, name: "type", len: 0, kind: G(resp_type) },
    Def { on: Response, name: "headers", len: 0, kind: G(resp_headers) },
    Def { on: Response, name: "bodyUsed", len: 0, kind: G(resp_body_used) },
    Def { on: Response, name: "body", len: 0, kind: G(resp_body) },
    Def { on: Response, name: "clone", len: 0, kind: M(resp_clone) },
    Def { on: Response, name: "text", len: 0, kind: M(body_text) },
    Def { on: Response, name: "json", len: 0, kind: M(body_json) },
    Def { on: Response, name: "arrayBuffer", len: 0, kind: M(body_array_buffer) },
    Def { on: Response, name: "blob", len: 0, kind: M(body_blob) },
    Def { on: Response, name: "formData", len: 0, kind: M(body_form_data) },
    Def { on: Stream, name: "locked", len: 0, kind: G(streams::locked) },
    Def { on: Stream, name: "getReader", len: 0, kind: M(streams::get_reader) },
    Def { on: Stream, name: "cancel", len: 1, kind: M(streams::cancel) },
    Def { on: Stream, name: "tee", len: 0, kind: M(streams::gate) },
    Def { on: Stream, name: "pipeTo", len: 1, kind: M(streams::gate) },
    Def { on: Stream, name: "pipeThrough", len: 1, kind: M(streams::gate) },
    Def { on: Reader, name: "read", len: 0, kind: M(streams::read) },
    Def { on: Reader, name: "cancel", len: 1, kind: M(streams::reader_cancel) },
    Def { on: Reader, name: "releaseLock", len: 0, kind: M(streams::release) },
    Def { on: Reader, name: "closed", len: 0, kind: G(streams::reader_closed) },
];

/// Define the globals `fetch`, `Headers`, `Request`, `Response`, `ReadableStream`, `ReadableStreamDefaultReader`.
pub(crate) fn install(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    let g = it.global_object();
    let op = it.object_prototype();
    // The iterator prototype inherits from %IteratorPrototype%, so `iterator[Symbol.iterator]() === iterator`.
    let protos: [ObjId; 6] = [d.obj(it, op)?, d.obj(it, op)?, d.obj(it, op)?, d.obj(it, op)?, d.obj(it, op)?, d.obj(it, it.iterator_prototype())?];
    let _ = d.fetch.protos.set(protos);
    let mut fixed = protos.to_vec();
    for def in DEFS {
        let target = protos[def.on as usize];
        let key = d.key(it, def.name)?;
        match def.kind {
            Kind::Method(h) => {
                let f = d.add_fn(it, h, def.name, def.len, false)?;
                it.define_data(target, key, Value::Object(f), true, false, true)?;
                // `Headers.prototype[Symbol.iterator]` is the same function object as `entries`.
                if def.name == "entries" {
                    it.define_data(target, it.symbol_iterator(), Value::Object(f), true, false, true)?;
                }
            }
            Kind::Get(h) => {
                let f = d.add_fn(it, h, &format!("get {}", def.name), 0, false)?;
                d.define_accessor(it, target, key, f, None)?;
            }
            Kind::Accessor(gh, sh) => {
                let f = d.add_fn(it, gh, &format!("get {}", def.name), 0, false)?;
                let s = d.add_fn(it, sh, &format!("set {}", def.name), 1, false)?;
                d.define_accessor(it, target, key, f, Some(s))?;
            }
        }
    }
    let (kp, kc) = (d.key(it, "prototype")?, d.key(it, "constructor")?);
    let ifaces: [(&str, Pr, crate::browser::H, bool); 5] = [
        ("Headers", Headers, headers_ctor, true),
        ("Request", Request, request_ctor, true),
        ("Response", Response, response_ctor, true),
        ("ReadableStream", Stream, streams::ctor, true),
        ("ReadableStreamDefaultReader", Reader, illegal, false),
    ];
    for (name, p, h, ctor) in ifaces {
        let f = d.add_fn(it, h, name, if name == "Headers" || name == "ReadableStream" { 0 } else { 1 }, ctor)?;
        let proto = protos[p as usize];
        it.define_data(f, kp, Value::Object(proto), false, false, false)?;
        it.define_data(proto, kc, Value::Object(f), true, false, true)?;
        let k = d.key(it, name)?;
        it.define_data(g, k, Value::Object(f), true, false, true)?;
        fixed.push(f);
    }
    let f = d.add_fn(it, fetch, "fetch", 1, false)?;
    let k = d.key(it, "fetch")?;
    it.define_data(g, k, Value::Object(f), true, false, true)?;
    fixed.push(f);
    streams::install(d, it, protos[Stream as usize], protos[Reader as usize], &mut fixed)?;
    *d.fetch.fixed.borrow_mut() = fixed;
    // The intrinsic JSON.parse, so a script that replaces it does not change what `json()` runs.
    let json = d.prop(it, Value::Object(g), "JSON")?;
    if let Value::Object(_) = json {
        let parse = d.prop(it, json, "parse")?;
        if it.is_callable_value(parse) {
            d.fetch.json_parse.set(Some(parse));
        }
    }
    Ok(())
}
