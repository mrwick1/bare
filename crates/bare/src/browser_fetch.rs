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
//! Policy: same origin, and CORS for another origin
//! - A request to the document origin (one origin compare at queue time) runs as before: no `Origin`, no check, no preflight.
//! - A request to another origin follows the CORS protocol of the Fetch Standard in `run_one`, on the worker thread:
//!   - `mode: "cors"` (the default, and all of XHR): sends `Origin`, then checks `Access-Control-Allow-Origin` (the exact origin or `*`;
//!     with `credentials: "include"` or `withCredentials` the exact origin and `Access-Control-Allow-Credentials: true`). The page sees
//!     the safelisted response headers and those of `Access-Control-Expose-Headers`. `response.type` is `"cors"`.
//!   - A request that is not simple (a method other than GET, HEAD, POST, or a header that is not safelisted, or a `Content-Type`
//!     other than form, multipart and text) sends a preflight `OPTIONS` first, with `Access-Control-Request-Method` and `-Headers`, and
//!     no body or credentials. `Access-Control-Allow-Methods`, `-Headers` and `-Max-Age` (5 s by default, 600 s at most) decide. The
//!     passed preflights are kept for the process, by (origin, credentials, URL, method, header names).
//!   - `mode: "no-cors"`: only GET, HEAD and POST, only safelisted headers, no check, and an opaque response (`type` `"opaque"`, status 0,
//!     no header, no body, empty `url`). A POST sends `Origin`.
//!   - `mode: "same-origin"` rejects before any socket.
//!   - A failed check rejects with `TypeError: Failed to fetch` (XHR: an `error` event, status 0) and puts one line in the console, as
//!     Chrome does. The line names the origins and the reason; it has no path or query.
//!   - A `http:` URL from an `https:` page is blocked (mixed content), except loopback.
//! - Every redirect is checked before the next hop opens a socket. A redirect from https to http ends the request. A redirect to another
//!   origin is followed in `cors` and `no-cors` mode, with the CORS check of each reply (also the redirect reply) and a preflight
//!   for each cross-origin hop that needs one. `Origin` is `null` once a redirect leaves a hop that is not the document origin.
//!   `Authorization` is dropped when a redirect changes the origin. `redirect: "error"` is supported,
//!   `"manual"` is `Unsupported`. 301/302 (POST) and 303 (all but GET and HEAD) turn the request into a GET without a body; 307/308 keep both.
//! - JS cannot set `Host`, `Content-Length`, `Cookie`, `Origin`, `Referer`, `Connection`, `Transfer-Encoding`, `Accept-Encoding`,
//!   `Sec-*`, `Proxy-*` and the other forbidden names of the Fetch Standard: they are dropped, as a browser does. `Host` and
//!   `Content-Length` come from the client. `Set-Cookie` of a response is never put in `response.headers`. Cookies: the
//!   jar of the client (`bare_net::cookie`, shared with `document.cookie`) fills from every reply and gives the `Cookie` header of every hop. `credentials`
//!   decides: `omit` sends and stores none, `same-origin` (the default, and XHR) only for the origin of the document, `include` (and `withCredentials`)
//!   for any origin. A cross-site request sends only `SameSite=None` cookies, and its reply sets only those. No `Referer` is sent. `include` also needs the
//!   exact origin and `Allow-Credentials` for the CORS check, as in Chrome. `Origin` is sent only to another origin. A same-origin POST sends none (Chrome does).
//! - A failed network request rejects with `TypeError: Failed to fetch`. Errors and diagnostics never hold a header value, a
//!   body byte, a path or a query. [`Browser::take_fetch_diagnostics`] gives fixed labels and the origin.
//!
//! Resource caps (`FetchLimits`) end the call with `Error::Limit(LimitKind::Host(name))`, never a JS exception: queued requests (1024, and 8 MiB of URL and header text),
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
//! `Request.body`; non-empty `integrity`;
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

use crate::browser::{Browser, Dom, IdMap, Kind, R, unsupported};
use crate::browser_navigation_url::form_serialize;
use crate::browser_streams as streams;
use bare_js::eval::{Error, Interp};
use bare_js::value::{ObjId, Value};
use bare_net::http::{Client, Cookies, Ctx, Method, Request as NetRequest};
use bare_net::parallel::Task;
use bare_url::{Scheme, Url};
use std::cell::{Cell, OnceCell, RefCell};
use std::borrow::Cow;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A queue shorter than this takes a request without a sum of the bytes of its requests (`FetchLimits::max_pending_bytes`).
const FREE_PENDING: usize = 64;

/// Bytes of the URL and the headers of a request (`FetchLimits::max_pending_bytes`).
fn meta_bytes(url: &Url, headers: &[(String, String)]) -> usize {
    url.host.len() + url.path.len() + headers.iter().map(|(k, v)| k.len() + v.len()).sum::<usize>()
}

/// Does one more request fit the byte bound of the queue? Only a queue of `FREE_PENDING` requests or more sums its bytes.
fn pending_fits(st: &FetchState, lim: &FetchLimits, add: usize) -> bool {
    st.queue.len() < FREE_PENDING || st.queue.iter().map(Pending::meta_bytes).sum::<usize>().saturating_add(add) <= lim.max_pending_bytes
}

/// Hard bounds of the fetch layer. Each is checked before the thing grows.
#[derive(Clone, Copy, Debug)]
pub struct FetchLimits {
    /// Requests that wait for `run_network_tasks`. A page that calls `fetch` many times in one task (gitlab: 64) needs all of them queued,
    /// as in Chrome. The queue is also bounded by `max_pending_bytes`.
    pub max_pending: usize,
    /// Bytes of the URLs and headers of the queued requests. Their bodies count in `max_retained`. Checked once the queue holds `FREE_PENDING`
    /// requests (fewer hold at most `FREE_PENDING` x (`max_url_bytes` + `max_header_bytes`)).
    pub max_pending_bytes: usize,
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
            max_pending: 1024,
            max_pending_bytes: 8 << 20,
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

/// The request mode of the Fetch Standard.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Cors,
    NoCors,
    SameOrigin,
}

impl Mode {
    fn of(s: &str) -> Mode {
        match s {
            "no-cors" => Mode::NoCors,
            "same-origin" => Mode::SameOrigin,
            _ => Mode::Cors,
        }
    }
}

/// What `run_one` needs for CORS. A request to the origin of the document has `doc: None` and costs one origin compare, at queue time.
struct Cors {
    mode: Mode,
    /// `credentials: "include"` or `withCredentials`.
    include: bool,
    /// `credentials: "omit"`: no cookie goes and no cookie is stored, also for the document origin.
    omit: bool,
    /// The origin of the document, for a request to another origin. `None`: the first URL is the document origin.
    doc: Option<Url>,
    /// The request is not sent: the text for the console.
    blocked: Option<String>,
    /// For the console text: `fetch` or `XMLHttpRequest`.
    api: &'static str,
}

impl Clone for Cors {
    fn clone(&self) -> Cors {
        Cors { mode: self.mode, include: self.include, omit: self.omit, doc: self.doc.as_ref().map(dup), blocked: self.blocked.clone(), api: self.api }
    }
}

impl Cors {
    fn same(api: &'static str) -> Cors {
        Cors { mode: Mode::Cors, include: false, omit: false, doc: None, blocked: None, api }
    }
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
    cors: Cors,
    /// An `XMLHttpRequest` and its request generation (`browser_xhr.rs`). Then `promise` is `undefined`.
    xhr: Option<(ObjId, u64)>,
    /// When it was queued (only when `BARE_NET_TRACE` is set).
    queued: Option<std::time::Instant>,
    /// Its I/O, started on a worker thread before its turn (see `start_queued`). The outcome and the diagnostics. `None`: the job
    /// did not send it (the bodies that wait were at the cap, or the page is gone); the page thread sends it at its turn.
    started: Option<Task<Option<(Outcome, Vec<String>)>>>,
}

/// What a worker thread needs to send a queued request. It owns its data: a `Pending` holds values of the realm.
struct Wire {
    method: Method,
    url: Url,
    headers: Vec<(String, String)>,
    body: Option<Vec<u8>>,
    redirect: Redirect,
    cors: Cors,
    queued: Option<std::time::Instant>,
}

/// What `run_one` needs to send a request: borrowed from a `Pending` or from a `Wire`.
struct Parts<'a> {
    method: Method,
    url: &'a Url,
    headers: &'a [(String, String)],
    body: Option<&'a [u8]>,
    redirect: Redirect,
    cors: &'a Cors,
}

impl Wire {
    fn parts(&self) -> Parts<'_> {
        Parts { method: self.method, url: &self.url, headers: &self.headers, body: self.body.as_deref(), redirect: self.redirect, cors: &self.cors }
    }
}

impl Pending {
    /// Bytes of the URL and the headers (the body counts in `FetchState::queued_bytes`).
    fn meta_bytes(&self) -> usize {
        meta_bytes(&self.url, &self.headers)
    }

    fn parts(&self) -> Parts<'_> {
        Parts { method: self.method, url: &self.url, headers: &self.headers, body: self.body.as_ref().map(|b| b.as_slice()), redirect: self.redirect, cors: &self.cors }
    }

    fn wire(&self) -> Wire {
        Wire { method: self.method, url: dup(&self.url), headers: self.headers.clone(), body: self.body.as_deref().cloned(), redirect: self.redirect, cors: self.cors.clone(), queued: self.queued }
    }
}

#[derive(Default)]
struct FetchState {
    objs: IdMap<ObjId, F>,
    queue: VecDeque<Pending>,
    base: Option<Url>,
    retained: usize,
    queued_bytes: usize,
    readers: usize,
    started: u64,
    diag: VecDeque<String>,
    dropped_diag: u64,
    halted: Option<Error>,
    /// Body bytes of the replies that workers read and that wait for their turn. `usize::MAX` once the page is gone.
    done: Arc<AtomicUsize>,
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
    pub(crate) client: RefCell<Client>,
    protos: OnceCell<[ObjId; 6]>,
    fixed: RefCell<Vec<ObjId>>,
    pub(crate) json_parse: Cell<Option<Value>>,
    /// Live (the window and the control interface): each request starts on a worker at once, and `run_network_tasks` takes only the
    /// replies that are in. Off: a request that is not in is waited for (a test or a headless run settles the page).
    pub(crate) live: Cell<bool>,
}

impl Default for Fetch {
    fn default() -> Fetch {
        Fetch { st: RefCell::default(), lim: Cell::new(FetchLimits::default()), client: RefCell::new(Client::new()), protos: OnceCell::new(), fixed: RefCell::default(), json_parse: Cell::new(None), live: Cell::new(false) }
    }
}

impl Drop for Fetch {
    /// A queued job that has not begun does not send its request: the page is gone, and the workers serve the next document.
    fn drop(&mut self) {
        self.st.borrow().done.store(usize::MAX, Ordering::Relaxed);
    }
}

/// Queued GET and HEAD requests that may run on the workers at once. The workers run `MAX_WORKERS` of them at a time, in order.
const AHEAD: usize = 32;

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
    /// `Browser::survive`: the network tasks stopped on a script budget; let them run again.
    pub(crate) fn clear_budget_halt_fetch(&self) {
        let mut s = self.fetch.st.borrow_mut();
        if s.halted.as_ref().is_some_and(crate::browser::is_budget) {
            s.halted = None;
        }
    }

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
        self.add_obj_in(it, self.fproto(p), f)
    }

    /// Like `add_obj` with the prototype `proto` (a subclass instance).
    fn add_obj_in(&self, it: &mut Interp<'_>, proto: ObjId, f: F) -> R<ObjId> {
        let o = self.obj(it, proto)?;
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
        self.headers_obj_in(it, self.fproto(Pr::Headers), list)
    }

    fn headers_obj_in(&self, it: &mut Interp<'_>, proto: ObjId, list: HeaderList) -> R<ObjId> {
        self.reserve(it, 1, 0)?;
        self.add_obj_in(it, proto, F::Headers(list))
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
            // Another scheme (`ftp:`, `mailto:`, `blob:`) is a network error for this request: the promise rejects, the script goes on.
            Err(_) if crate::browser_navigation_url::scheme_of(s).is_some_and(|(sch, _)| !sch.eq_ignore_ascii_case("http") && !sch.eq_ignore_ascii_case("https")) => Err(it.type_error("Failed to fetch")),
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
                    return Err(it.type_error("ReadableStream request bodies are not supported"));
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
    if core.mode == "no-cors" {
        if !matches!(core.method.as_str(), "GET" | "HEAD" | "POST") {
            return Err(it.type_error(&format!("'{}' is unsupported in no-cors mode.", core.method)));
        }
        // The headers of a no-cors request: only the safelisted ones stay (the guard "request-no-cors").
        core.headers.list.retain(|(n, v)| safelisted_request_header(n, v));
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
    let proto = it.host_proto_from_new_target(d.fproto(Pr::Headers))?;
    let mut list = HeaderList::new(Guard::None);
    let init = a.first().copied().unwrap_or(Value::Undefined);
    if matches!(init, Value::Null) {
        return Err(it.type_error("Failed to construct 'Headers': The provided value is not of type 'HeadersInit'"));
    }
    d.fill(it, &mut list, init)?;
    Ok(Value::Object(d.headers_obj_in(it, proto, list)?))
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

fn new_request_obj(d: &Dom, it: &mut Interp<'_>, core: Core, proto: ObjId) -> R<ObjId> {
    let n = core.body.as_ref().map_or(0, |b| b.len());
    d.reserve(it, 2, n)?;
    let h = d.headers_obj(it, core.headers)?;
    let signal = match core.signal {
        Some(s) => Some(d.follow_signal(it, s)?),
        None => None,
    };
    d.add_obj_in(it, proto, F::Request(ReqData { c: Common { headers: h, body: Body::new(core.body) }, method: core.method, url: core.url, redirect: core.redirect, credentials: core.credentials, mode: core.mode, signal }))
}

fn request_ctor(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> R<Value> {
    let proto = it.host_proto_from_new_target(d.fproto(Pr::Request))?;
    if a.is_empty() {
        return Err(it.type_error("Failed to construct 'Request': 1 argument required, but only 0 present."));
    }
    let core = make_core(d, it, arg(a, 0), arg(a, 1))?;
    Ok(Value::Object(new_request_obj(d, it, core, proto)?))
}

fn status_null_body(s: u16) -> bool {
    matches!(s, 101 | 103 | 204 | 205 | 304)
}

fn response_ctor(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> R<Value> {
    let proto = it.host_proto_from_new_target(d.fproto(Pr::Response))?;
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
    Ok(Value::Object(d.add_obj_in(it, proto, F::Response(r))?))
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
    Ok(Value::Object(new_request_obj(d, it, Core { method, url, headers, body, redirect, credentials, mode, signal }, d.fproto(Pr::Request))?))
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
    if let Some(url) = data_input(d, it, arg(a, 0))? {
        let r = if crate::browser_web::is_blob_url(&url) { blob_response(d, it, &url)? } else { data_response(d, it, &url)? };
        return settle(it, promise, Ok(r)).map(|_| ());
    }
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
    let mut cors = policy(d, &lim, &core.url, Mode::of(core.mode), "fetch")?;
    cors.include = core.credentials == "include";
    cors.omit = core.credentials == "omit";
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
        if !pending_fits(&st, &lim, meta_bytes(&core.url, &headers)) {
            return Err(d.lim("fetch-queue"));
        }
        st.queue.push_back(Pending { promise, method, url: core.url, headers, body: core.body, redirect: core.redirect, cors, xhr: None, queued: bare_net::trace::on().then(std::time::Instant::now), started: None });
    }
    Ok(())
}

/// The origin policy: a configured document origin. A request to the document origin returns at once (`Cors::same`). A request to another
/// origin keeps the document origin, for the CORS protocol in `run_one`, or a text that blocks it before DNS or a socket.
fn policy(d: &Dom, lim: &FetchLimits, url: &Url, mode: Mode, api: &'static str) -> R<Cors> {
    let base = d.fetch_base();
    let mut st = d.fetch.st.borrow_mut();
    let Some(base) = base else {
        st.diag(lim, "blocked: no document origin is configured".into());
        return Err(Error::Unsupported(unsupported("fetch without a configured document origin (Browser::configure_fetch)")));
    };
    let mut cors = Cors { mode, ..Cors::same(api) };
    if base.same_origin(url) {
        return Ok(cors);
    }
    let (doc, to) = (origin(&base), origin(url));
    if mode == Mode::SameOrigin {
        cors.blocked = Some(format!("Fetch API cannot load {to}. Request mode is \"same-origin\" but the URL's origin is not the same as the request origin {doc}."));
    } else if base.scheme == Scheme::Https && url.scheme == Scheme::Http && !potentially_trustworthy(url) {
        cors.blocked = Some(format!("Mixed Content: The page at '{doc}' was loaded over HTTPS, but requested an insecure resource '{to}'. This request has been blocked; the content must be served over HTTPS."));
    }
    match &cors.blocked {
        Some(_) => st.diag(lim, format!("blocked: request to {to} (mode or mixed content)")),
        None => cors.doc = Some(dup(&base)),
    }
    Ok(cors)
}

/// `http:` to a loopback host is allowed from an `https:` page, as in Chrome.
fn potentially_trustworthy(u: &Url) -> bool {
    u.host == "localhost" || u.host.ends_with(".localhost") || u.host == "[::1]" || u.host.starts_with("127.")
}

/// Queue the request of an `XMLHttpRequest` with the policy, the caps and the queue of `fetch`. `headers` are combined by name.
pub(crate) fn queue_xhr(d: &Dom, x: (ObjId, u64), method: &str, url: Url, mut headers: Vec<(String, String)>, body: Option<Rc<Vec<u8>>>, with_credentials: bool) -> R<()> {
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
    let mut cors = policy(d, &lim, &url, Mode::Cors, "XMLHttpRequest")?;
    cors.include = with_credentials;
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
    if !pending_fits(&st, &lim, meta_bytes(&url, &headers)) {
        return Err(d.lim("fetch-queue"));
    }
    st.queue.push_back(Pending { promise: Value::Undefined, method, url, headers, body, redirect: Redirect::Follow, cors, xhr: Some(x), queued: bare_net::trace::on().then(std::time::Instant::now), started: None });
    Ok(())
}

/// Queue the reply of an `XMLHttpRequest` to a `blob:` URL: no I/O. `found` is the bytes and type of the Blob, or `None` for a URL that is not in
/// the store (a network error). `url` is a stand-in for `responseURL` (the document URL). The reply settles in its turn, as any other.
#[cold]
#[inline(never)]
pub(crate) fn queue_xhr_blob(d: &Dom, x: (ObjId, u64), url: Url, found: Option<(Vec<u8>, String)>) -> R<()> {
    let lim = d.fl();
    let mut st = d.fetch.st.borrow_mut();
    if st.queue.len() >= lim.max_pending {
        return Err(d.lim("fetch-queue"));
    }
    if st.started >= lim.max_started {
        return Err(d.lim("fetch-requests"));
    }
    let out = match found {
        Some((body, ty)) => {
            if st.retained.saturating_add(st.queued_bytes).saturating_add(body.len()) > lim.max_retained {
                return Err(d.lim("fetch-retained"));
            }
            let mut headers = vec![("content-length".to_string(), body.len().to_string())];
            if !ty.is_empty() {
                headers.push(("content-type".into(), ty));
            }
            // The turn of the reply subtracts what a worker added (`run_network_tasks_inner`).
            st.done.fetch_add(body.len(), Ordering::Relaxed);
            Outcome::Done { status: 200, url: dup(&url), redirected: false, headers, body, taint: Taint::Basic }
        }
        None => Outcome::Failed("blob URL not found"),
    };
    st.started += 1;
    let started = Some(d.fetch.client.borrow().spawn(move |_| Some((out, Vec::new()))));
    st.queue.push_back(Pending { promise: Value::Undefined, method: Method::Get, url, headers: Vec::new(), body: None, redirect: Redirect::Follow, cors: Cors::same("XMLHttpRequest"), xhr: Some(x), queued: None, started });
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

/// What the page may see of a reply (the response tainting of the Fetch Standard).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Taint {
    /// The document origin, or the same origin all the way: everything.
    Basic,
    /// Another origin that passed the CORS check: the safelisted and the exposed headers.
    Cors,
    /// `no-cors` to another origin: status 0, no header, no body.
    Opaque,
}

pub(crate) enum Outcome {
    Done { status: u16, url: Url, redirected: bool, headers: Vec<(String, String)>, body: Vec<u8>, taint: Taint },
    /// The CORS check or a policy refused the request (or its reply). The text goes to the console of the page. It holds no path, query or request header value.
    Blocked(String),
    /// A network error or a policy stop. The label holds no header value, path or query.
    Failed(&'static str),
    /// A network error, with its cause (`bare_net::cause::describe`). The text goes to diagnostics only: the page sees `Failed to fetch`.
    Net(String),
    /// The reply is bigger than the cap: a resource error, not a JS error.
    TooBig,
}

fn run_one(client: &mut Client, lim: &FetchLimits, first: Parts<'_>, diag: &mut Vec<String>) -> Outcome {
    let cors = first.cors;
    if let Some(t) = &cors.blocked {
        return Outcome::Blocked(t.clone());
    }
    let first_origin = dup(first.url);
    // A request to the document origin has `doc: None`: its taint is `Basic` until a redirect leaves the origin.
    let doc = cors.doc.as_ref().unwrap_or(&first_origin);
    let mut taint = match (&cors.doc, cors.mode) {
        (None, _) => Taint::Basic,
        (_, Mode::NoCors) => Taint::Opaque,
        _ => Taint::Cors,
    };
    // Fetch standard: set when a redirect leaves a hop that is not the origin of the document. The `Origin` header is then `null`.
    let mut null_origin = false;
    let (mut method, mut url, mut headers, mut body) = (first.method, dup(first.url), first.headers.to_vec(), first.body);
    let mut redirected = false;
    for hop in 0..=lim.max_redirects {
        let origin_s = (taint == Taint::Cors || (taint == Taint::Opaque && !method.safe())).then(|| if null_origin { "null".to_string() } else { origin(doc) });
        if taint == Taint::Cors && needs_preflight(method, &headers) {
            let o = origin_s.as_deref().unwrap_or("null");
            if let Err(out) = preflight(client, lim, &url, method, &headers, o, cors, doc) {
                return out;
            }
        }
        let mut wire: Vec<(&str, &str)> = headers.iter().map(|(n, v)| (n.as_str(), v.as_str())).collect();
        if let Some(o) = &origin_s {
            wire.push(("origin", o));
        }
        // Cookies: `include` sends them to any origin, `same-origin` (the default, and XHR without `withCredentials`) only to the origin of the
        // document, `omit` never. The same rule decides if the reply may set one.
        let cookies = Cookies { on: cors.include || !cors.omit && doc.same_origin(&url), ctx: Ctx::Embedded };
        let req = NetRequest { cookies, method, url: &url, headers: &wire, body: body.unwrap_or(&[]) };
        let resp = match client.send_once(&req, lim.max_response_body) {
            Ok(r) => r,
            Err(e) if e.to_string() == "the reply is too big" => return Outcome::TooBig,
            Err(e) => return Outcome::Net(bare_net::cause::describe(&*e)),
        };
        let resp = unwrap_arc(resp);
        // Every reply of a CORS request is checked, also a redirect.
        if taint == Taint::Cors
            && let Err(reason) = cors_check(&resp.headers, origin_s.as_deref().unwrap_or("null"), cors.include)
        {
            return Outcome::Blocked(blocked_text(cors, &url, doc, &reason));
        }
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
            if !doc.same_origin(&target) {
                if cors.mode == Mode::SameOrigin {
                    diag.push(format!("blocked: cross-origin redirect from {} to {} (mode same-origin)", origin(&url), origin(&target)));
                    return Outcome::Failed("cross-origin redirect refused");
                }
                if taint == Taint::Basic {
                    taint = if cors.mode == Mode::NoCors { Taint::Opaque } else { Taint::Cors };
                }
            }
            if taint == Taint::Cors && !target.same_origin(&url) && !doc.same_origin(&url) {
                null_origin = true;
            }
            if !target.same_origin(&url) {
                headers.retain(|(n, _)| n != "authorization");
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
        return match taint {
            Taint::Basic => Outcome::Done { status: resp.status, url, redirected, headers: resp.headers, body: resp.body, taint },
            Taint::Cors => Outcome::Done { status: resp.status, url, redirected, headers: expose(resp.headers, cors.include), body: resp.body, taint },
            Taint::Opaque => Outcome::Done { status: 0, url, redirected: false, headers: Vec::new(), body: Vec::new(), taint },
        };
    }
    Outcome::Failed("too many redirects")
}

// ---------------------------------------------------------------------------------------------- CORS

fn blocked_text(c: &Cors, url: &Url, doc: &Url, reason: &str) -> String {
    format!("Access to {} at '{}' from origin '{}' has been blocked by CORS policy: {reason}", c.api, origin(url), origin(doc))
}

/// The value of a response header with the same name given more than once joined with ", " (the Fetch Standard "get").
fn combined(headers: &[(String, String)], name: &str) -> Option<String> {
    let mut it = headers.iter().filter(|(n, _)| n == name).map(|(_, v)| v.trim_matches(['\t', ' ']));
    let mut out = it.next()?.to_string();
    for v in it {
        out.push_str(", ");
        out.push_str(v);
    }
    Some(out)
}

/// The CORS check of the Fetch Standard. `origin` is the serialized request origin (`null` when tainted). The error is the text after
/// "blocked by CORS policy: " in Chrome.
fn cors_check(headers: &[(String, String)], origin: &str, include: bool) -> Result<(), String> {
    let Some(allow) = combined(headers, "access-control-allow-origin") else {
        return Err("No 'Access-Control-Allow-Origin' header is present on the requested resource.".into());
    };
    if allow.contains(',') {
        return Err(format!("The 'Access-Control-Allow-Origin' header contains multiple values '{allow}', but only one is allowed."));
    }
    if allow == "*" {
        if !include {
            return Ok(());
        }
        return Err("The value of the 'Access-Control-Allow-Origin' header in the response must not be the wildcard '*' when the request's credentials mode is 'include'.".into());
    }
    if allow != origin {
        return Err(format!("The 'Access-Control-Allow-Origin' header has a value '{allow}' that is not equal to the supplied origin."));
    }
    if include {
        let v = combined(headers, "access-control-allow-credentials").unwrap_or_default();
        if v != "true" {
            return Err(format!("The value of the 'Access-Control-Allow-Credentials' header in the response is '{v}' which must be 'true' when the request's credentials mode is 'include'."));
        }
    }
    Ok(())
}

/// The headers a script may read from a CORS reply: the safelisted ones and `Access-Control-Expose-Headers` (`*` only without credentials).
fn expose(mut headers: Vec<(String, String)>, include: bool) -> Vec<(String, String)> {
    const SAFE: [&str; 7] = ["cache-control", "content-language", "content-length", "content-type", "expires", "last-modified", "pragma"];
    let list = combined(&headers, "access-control-expose-headers").unwrap_or_default().to_ascii_lowercase();
    let names: Vec<&str> = list.split(',').map(str::trim).filter(|n| !n.is_empty()).collect();
    let all = !include && names.contains(&"*");
    headers.retain(|(n, _)| all || SAFE.contains(&n.as_str()) || names.contains(&n.as_str()));
    headers
}

/// A request header that does not need a preflight: the Fetch Standard "CORS-safelisted request-header".
fn safelisted_request_header(n: &str, v: &str) -> bool {
    matches!(n, "accept" | "accept-language" | "content-language" | "content-type")
        && v.len() <= 128
        && !v.bytes().any(|b| (b < b' ' && b != b'\t') || b == 0x7f || b"\"():<>?@[\\]{}".contains(&b))
        && (n != "content-type" || matches!(v.split(';').next().unwrap_or("").trim().to_ascii_lowercase().as_str(), "application/x-www-form-urlencoded" | "multipart/form-data" | "text/plain"))
}

fn needs_preflight(method: Method, headers: &[(String, String)]) -> bool {
    !matches!(method, Method::Get | Method::Head | Method::Post) || headers.iter().any(|(n, v)| !safelisted_request_header(n, v))
}

/// `Access-Control-Max-Age` in seconds: 5 when it is absent or not a number, at most 600 (the cap of Chrome).
pub(crate) fn preflight_max_age(v: Option<&str>) -> u64 {
    match v.map(str::trim) {
        Some(v) if !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()) => v.parse::<u64>().unwrap_or(u64::MAX).min(600),
        _ => 5,
    }
}

/// Preflights that passed: the key (origin, credentials, URL, method, header names) and when the entry ends. One list for the process,
/// as the cache of a browser profile is for all its pages.
static PREFLIGHTS: std::sync::Mutex<Vec<(String, std::time::Instant)>> = std::sync::Mutex::new(Vec::new());
const PREFLIGHT_ENTRIES: usize = 256;

/// The preflight of a request to another origin: an `OPTIONS` with `Access-Control-Request-Method` and `-Headers`, no body and no
/// credentials. `Err` holds the outcome of the whole request.
#[allow(clippy::too_many_arguments)]
fn preflight(client: &mut Client, lim: &FetchLimits, url: &Url, method: Method, headers: &[(String, String)], origin_s: &str, cors: &Cors, doc: &Url) -> Result<(), Outcome> {
    let mut names: Vec<&str> = headers.iter().filter(|(n, v)| !safelisted_request_header(n, v)).map(|(n, _)| n.as_str()).collect();
    names.sort_unstable();
    names.dedup();
    let names = names.join(",");
    let key = format!("{origin_s}\n{}\n{url}\n{}\n{names}", u8::from(cors.include), method.as_str());
    let now = std::time::Instant::now();
    {
        let mut c = PREFLIGHTS.lock().unwrap_or_else(|e| e.into_inner());
        c.retain(|(_, t)| *t > now);
        if c.iter().any(|(k, _)| *k == key) {
            return Ok(());
        }
    }
    let mut h: Vec<(&str, &str)> = vec![("origin", origin_s), ("access-control-request-method", method.as_str()), ("accept", "*/*")];
    if !names.is_empty() {
        h.push(("access-control-request-headers", &names));
    }
    let req = NetRequest { cookies: Default::default(), method: Method::Options, url, headers: &h, body: &[] };
    let resp = match client.send_once(&req, lim.max_response_body) {
        Ok(r) => unwrap_arc(r),
        Err(e) => return Err(Outcome::Net(bare_net::cause::describe(&*e))),
    };
    let fail = |reason: &str| Err(Outcome::Blocked(blocked_text(cors, url, doc, reason)));
    const PRE: &str = "Response to preflight request doesn't pass access control check: ";
    if !(200..300).contains(&resp.status) {
        return fail(&format!("{PRE}It does not have HTTP ok status."));
    }
    if let Err(r) = cors_check(&resp.headers, origin_s, cors.include) {
        return fail(&format!("{PRE}{r}"));
    }
    let list = |name: &str| combined(&resp.headers, name).unwrap_or_default();
    let (methods, allowed) = (list("access-control-allow-methods"), list("access-control-allow-headers").to_ascii_lowercase());
    let methods: Vec<&str> = methods.split(',').map(str::trim).collect();
    let allowed: Vec<&str> = allowed.split(',').map(str::trim).collect();
    let star = !cors.include;
    if !(methods.contains(&method.as_str()) || matches!(method, Method::Get | Method::Head | Method::Post) || (star && methods.contains(&"*"))) {
        return fail(&format!("Method {} is not allowed by Access-Control-Allow-Methods in preflight response.", method.as_str()));
    }
    for n in names.split(',').filter(|n| !n.is_empty()) {
        if !(allowed.contains(&n) || (star && allowed.contains(&"*") && n != "authorization")) {
            return fail(&format!("Request header field {n} is not allowed by Access-Control-Allow-Headers in preflight response."));
        }
    }
    let secs = preflight_max_age(combined(&resp.headers, "access-control-max-age").as_deref());
    if secs > 0 {
        let mut c = PREFLIGHTS.lock().unwrap_or_else(|e| e.into_inner());
        if c.len() >= PREFLIGHT_ENTRIES {
            c.remove(0);
        }
        c.push((key, now + std::time::Duration::from_secs(secs)));
    }
    Ok(())
}

fn unwrap_arc(a: std::sync::Arc<bare_net::http::Response>) -> bare_net::http::Response {
    std::sync::Arc::try_unwrap(a).unwrap_or_else(|a| (*a).clone())
}

fn finish(d: &Dom, it: &mut Interp<'_>, promise: Value, outcome: Outcome) -> R<()> {
    let r = match outcome {
        Outcome::Failed(_) | Outcome::Net(_) | Outcome::Blocked(_) => {
            let e = it.type_error("Failed to fetch");
            Err(e)
        }
        Outcome::TooBig => return Err(d.lim("fetch-response-body")),
        Outcome::Done { status, url, redirected, headers, body, taint } => build_response(d, it, status, url, redirected, headers, body, taint),
    };
    settle(it, promise, r).map(|_| ())
}

/// The text of a `fetch` input that is a string or a `URL` object, when it is a `data:` or a `blob:` URL.
fn data_input(d: &Dom, it: &mut Interp<'_>, v: Value) -> R<Option<String>> {
    let s = match v {
        // A string that does not start with `data:` costs no copy.
        Value::String(id) => {
            let Ok(u) = it.heap.str_units(id) else { return Ok(None) };
            let u = &u[u.iter().take_while(|&&c| c <= 32).count()..];
            if !u.get(..5).is_some_and(|p| [b"data:", b"blob:"].iter().any(|t| p.iter().zip(*t).all(|(&c, &b)| c < 128 && (c as u8).eq_ignore_ascii_case(&b)))) {
                return Ok(None);
            }
            d.string_arg(it, v)?
        }
        Value::Object(o) if d.nav.s.borrow().urls.contains_key(&o) => d.string_arg(it, v)?,
        _ => return Ok(None),
    };
    let t = s.trim_matches(|c: char| c <= ' ');
    Ok(t.get(..5).is_some_and(|p| p.eq_ignore_ascii_case("data:") || p.eq_ignore_ascii_case("blob:")).then(|| t.to_string()))
}

/// The response of `fetch('data:...')`: status 200, the media type of the URL as `content-type`, the decoded body. No network, no CORS.
/// A URL with no comma or bad base64 is a network error (`TypeError`). The fragment is not part of the body.
fn data_response(d: &Dom, it: &mut Interp<'_>, url: &str) -> R<Value> {
    let bare = url.split_once('#').map_or(url, |u| u.0);
    let Some(body) = crate::images::data_url(bare) else { return Err(it.type_error("Failed to fetch")) };
    let head = bare[5..].split_once(',').map_or("", |h| h.0).trim();
    let head = match head.len().checked_sub(7).filter(|&i| head.is_char_boundary(i) && head[i..].eq_ignore_ascii_case(";base64")) {
        Some(i) => &head[..i],
        None => head,
    };
    let mime = if head.is_empty() { "text/plain;charset=US-ASCII".to_string() } else if head.starts_with(';') { format!("text/plain{head}") } else { head.to_string() };
    plain_response(d, it, url, Some(mime), body)
}

/// The response of `fetch('blob:...')`: status 200, the type of the Blob as `content-type` (none when it is empty), its bytes. No network,
/// no CORS. A URL that is not in the store (never made, revoked) is a network error (`TypeError`).
#[cold]
#[inline(never)]
fn blob_response(d: &Dom, it: &mut Interp<'_>, url: &str) -> R<Value> {
    let Some((body, ty)) = crate::browser_web::blob_url_read(d, url) else { return Err(it.type_error("Failed to fetch")) };
    it.charge((body.len() / 64) as u64)?;
    plain_response(d, it, url, (!ty.is_empty()).then_some(ty), body)
}

fn plain_response(d: &Dom, it: &mut Interp<'_>, url: &str, mime: Option<String>, body: Vec<u8>) -> R<Value> {
    let lim = d.fl();
    let mut list = HeaderList::new(Guard::Response);
    if let Some(mime) = mime {
        list.append(&lim, "content-type".into(), mime).map_err(|e| d.herr(it, e))?;
    }
    list.guard = Guard::Immutable;
    let body = Some(Rc::new(body));
    d.reserve(it, 2, body.as_ref().map_or(0, |b| b.len()))?;
    let h = d.headers_obj(it, list)?;
    let r = RespData { c: Common { headers: h, body: Body::new(body) }, status: 200, status_text: String::new(), url: url.to_string(), redirected: false, ty: "basic", stream: None };
    Ok(Value::Object(d.add_obj(it, Pr::Response, F::Response(r))?))
}

fn build_response(d: &Dom, it: &mut Interp<'_>, status: u16, url: Url, redirected: bool, headers: Vec<(String, String)>, body: Vec<u8>, taint: Taint) -> R<Value> {
    let lim = d.fl();
    let mut list = HeaderList::new(Guard::Response);
    for (n, v) in headers {
        let v = v.trim_matches(['\t', ' ']).to_string();
        list.append(&lim, n.to_ascii_lowercase(), v).map_err(|e| d.herr(it, e))?;
    }
    list.guard = Guard::Immutable;
    let body = (!status_null_body(status) && taint != Taint::Opaque).then(|| Rc::new(body));
    d.reserve(it, 2, body.as_ref().map_or(0, |b| b.len()))?;
    let h = d.headers_obj(it, list)?;
    let r = RespData { c: Common { headers: h, body: Body::new(body) }, status, status_text: String::new(), url: if taint == Taint::Opaque { String::new() } else { url.to_string() }, redirected, ty: ["basic", "cors", "opaque"][taint as usize], stream: None };
    Ok(Value::Object(d.add_obj(it, Pr::Response, F::Response(r))?))
}

impl Browser {
    /// Set the document origin and the HTTP client. Call it before scripts run. Without it, `fetch` is `Unsupported`.
    pub fn configure_fetch(&mut self, cfg: FetchConfig) -> Result<(), String> {
        let base = cfg.base_url.as_deref().map(Url::parse).transpose()?;
        let f = &self.dom.fetch;
        f.lim.set(cfg.limits);
        let mut client = cfg.client;
        client.set_site(base.as_ref().map(bare_net::cookie::site_of));
        // `document.cookie` uses the jar of the client that `fetch` uses (`run_page` makes all clients of a page share one).
        self.dom.nav.s.borrow_mut().store.use_jar(client.jar());
        *f.client.borrow_mut() = client;
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

    /// Can the first queued request settle now without a wait? `None`: none is queued. `Some(false)`: its reply is not in yet.
    /// A request that has not started counts as ready: its turn starts it.
    pub(crate) fn fetch_ready(&self) -> Option<bool> {
        let st = self.dom.fetch.st.borrow();
        st.queue.front().map(|p| p.started.as_ref().is_none_or(|t| t.done()))
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
    /// as before: only the waiting is shared. Up to `AHEAD` are started, and `MAX_WORKERS` of them run at once: a slow reply holds one
    /// worker, and the others go on with the requests after it. A job does not send its request while the bodies that wait for their
    /// turn hold `max_retained` bytes; the page thread sends it at its turn. So the result does not change, only the time.
    /// A request that is not a GET or a HEAD may change the server. It runs when all before it are done, on the thread that settles
    /// it (a worker gains nothing: nothing after it starts before it is done), so the server sees these requests in the order of the script.
    /// Live: every queued request starts at once, also one alone; a request that is not a GET or a HEAD starts when it is first.
    fn start_queued(&self) {
        let live = self.dom.fetch.live.get();
        if !live && self.dom.fetch.st.borrow().queue.len() < 2 {
            // One request alone gains nothing from a worker thread: this thread makes it.
            return;
        }
        let (lim, client) = (self.dom.fl(), self.dom.fetch.client.borrow());
        if !live && !client.parallel_pays() {
            return;
        }
        let mut st = self.dom.fetch.st.borrow_mut();
        let st = &mut *st;
        for (i, p) in st.queue.iter_mut().take(AHEAD).enumerate() {
            if !p.method.safe() && !(live && i == 0) {
                break;
            }
            if p.started.is_none() {
                let (wire, done) = (p.wire(), st.done.clone());
                p.started = Some(client.spawn(move |client| {
                    if done.load(Ordering::Relaxed) >= lim.max_retained {
                        return None;
                    }
                    if let Some(t) = wire.queued {
                        bare_net::trace::wanted(t);
                    }
                    let mut diag = Vec::new();
                    let out = run_one(client, &lim, wire.parts(), &mut diag);
                    if let Outcome::Done { body, .. } = &out {
                        // Saturating: `usize::MAX` marks a page that is gone (`Drop for Fetch`).
                        let _ = done.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |d| Some(d.saturating_add(body.len())));
                    }
                    Some((out, diag))
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
            if dom.fetch.live.get() && self.fetch_ready() == Some(false) {
                break;
            }
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
            let (outcome, diag) = bare_net::trace::blocked(|| match task.started.take().map(|t| t.wait()) {
                Some(Some(Some(out))) => {
                    if let Outcome::Done { body, .. } = &out.0 {
                        dom.fetch.st.borrow().done.fetch_sub(body.len(), Ordering::Relaxed);
                    }
                    out
                }
                // A lost worker is a network error.
                Some(None) => (Outcome::Net("worker lost: the thread of the request ended".into()), Vec::new()),
                Some(Some(None)) | None => {
                    let mut diag = Vec::new();
                    if let Some(t) = task.queued {
                        bare_net::trace::wanted(t);
                    }
                    (run_one(&mut dom.fetch.client.borrow_mut(), &lim, task.parts(), &mut diag), diag)
                }
            });
            {
                let mut st = dom.fetch.st.borrow_mut();
                match &outcome {
                    Outcome::Failed(label) => st.diag(&lim, format!("failed: {label} ({})", origin(&task.url))),
                    Outcome::Net(cause) => st.diag(&lim, format!("failed: network error ({}): {cause}", origin(&task.url))),
                    Outcome::Blocked(_) => st.diag(&lim, format!("failed: blocked by CORS or the request policy ({})", origin(&task.url))),
                    _ => {}
                }
                for t in diag {
                    st.diag(&lim, t);
                }
            }
            if let Outcome::Blocked(text) = &outcome {
                crate::browser_console::push_text(&dom, &mut self.it, crate::browser_console::ConsoleLevel::Error, text);
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

    /// Notes about requests that were blocked or failed (origin only, never a path, query, header or body; a network error adds its cause: kind and error text). Clears them.
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
        let len = if matches!(name, "Headers" | "Response" | "ReadableStream") { 0 } else { 1 };
        // These three read `new.target`, so a class may extend them.
        let f = if matches!(p, Headers | Request | Response) { d.add_ctor_nt(it, h, name, len)? } else { d.add_fn(it, h, name, len, ctor)? };
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
