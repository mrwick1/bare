//! `XMLHttpRequest`, `XMLHttpRequestUpload`, `XMLHttpRequestEventTarget` and `ProgressEvent` (XHR Standard), on the request path of
//! `fetch` (`browser_fetch.rs`). There is no second network stack: `send()` puts a `Pending` in the queue of `fetch`
//! (`queue_xhr`: the same origin policy, queue and byte caps, the same extraction of a body), and `run_network_tasks` runs it with the
//! same client and the same redirect rules. The driver then calls `Browser::xhr_deliver`, which fires the events.
//!
//! Events (the order was recorded in headless Chrome: `tools/ui/chrome_xhr.mjs`, `tools/fixtures/ui/xhr-cases.txt`). `send()` fires
//! `loadstart` and, for a body with an upload listener, the upload `loadstart`, and returns. The reply arrives whole (except a GET
//! that asks for `text/event-stream`: see `Browser::xhr_stream_deliver`), so the driver runs three steps, each in its own scope with a microtask checkpoint after each listener (as after a task):
//! 1. the upload `progress`, `load`, `loadend` (if the request had a body and an upload listener), then `readystatechange` at state 2;
//! 2. for a body that is not empty: state 3, `readystatechange`, `progress` (loaded, total from `Content-Length`);
//! 3. state 4, `readystatechange`, `load`, `loadend`.
//!
//! Before each step the request is checked again: a listener that called `abort()` or `open()` ends the delivery. An error, an abort or
//! a timeout run the "request error steps": state 4, `readystatechange`, the upload event and `loadend` if the upload was not done,
//! then the event and `loadend` at the object. `abort()` then sets the state back to 0.
//!
//! Gates (`Error::Unsupported`, before anything changes): a synchronous request (`async = false`: the pump is the only place where
//! the host blocks), `user` and `password` arguments, `responseType = "document"`, a `Document` body, and the methods `fetch` gates (a token method and a lower-case `patch`: `bare_net::http::Method` has seven). A request
//! to another origin follows CORS (see `browser_fetch.rs`): `Origin`, a preflight for a non-simple request, the check of the reply, and only the safelisted and the exposed response headers. A refused request fires `error` (status 0). Cookies follow the jar of the client (see `browser_fetch.rs`): `withCredentials` sends them to another origin, and without it only the document origin gets them. A request that has an upload listener gets no preflight for that reason (the standard asks for one).
//! A response text in a charset other than UTF-8 is `Unsupported` unless every byte is ASCII.
//!
//! Deviations: `timeout` is checked when the reply arrives (the blocking I/O cannot be stopped): a reply later than the timeout is a
//! `timeout`, with no `load`. `statusText` is the standard phrase of the code, not the phrase of the server (the client drops it).
//! The `json` response is parsed at each read (Chrome does the same); `arraybuffer` and `blob` are made once. DOM methods are not
//! enumerable, as everywhere in this host. Subclassing is not supported.
//!
//! GC: the globals are accessors until the first read (like `AbortSignal`). A queued request roots its object. An `XMLHttpRequest`
//! keeps its upload object and its cached response alive. Side tables are pruned at `collect`.

use crate::browser::*;
use crate::browser_fetch::{Outcome, StreamMsg, dequeue_xhr, forbidden_request, queue_xhr, queue_xhr_blob, queue_xhr_stream};
use crate::browser_handlers::{handler_get, handler_set};
use bare_js::eval::{Error, Interp};
use bare_js::heap::PropertyDescriptor;
use bare_js::value::{ObjId, PropertyKey, Value};
use bare_url::Url;
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;

type V = R<Value>;

fn arg(a: &[Value], i: usize) -> Value {
    a.get(i).copied().unwrap_or(Value::Undefined)
}

const UNSENT: u8 = 0;
const OPENED: u8 = 1;
const HEADERS: u8 = 2;
const LOADING: u8 = 3;
const DONE: u8 = 4;

/// Event types of `on*` handlers. 0 is on `XMLHttpRequest`, the others on `XMLHttpRequestEventTarget`.
const TYPES: [&str; 8] = ["readystatechange", "loadstart", "progress", "abort", "error", "load", "timeout", "loadend"];
const NAMES: [&str; 4] = ["XMLHttpRequest", "XMLHttpRequestEventTarget", "XMLHttpRequestUpload", "ProgressEvent"];
const MAX_OBJECTS: usize = 1 << 14;

#[derive(Clone, Copy, PartialEq)]
enum Rt {
    Empty,
    Text,
    Buffer,
    Blob,
    Json,
}

impl Rt {
    fn name(self) -> &'static str {
        ["", "text", "arraybuffer", "blob", "json"][self as usize]
    }
}

struct Resp {
    status: u16,
    url: String,
    /// Lowercase names, trimmed values, in arrival order, without `set-cookie`.
    headers: Vec<(String, String)>,
    body: Rc<Vec<u8>>,
}

struct Xhr {
    state: u8,
    send: bool,
    upload_done: bool,
    up_listener: bool,
    method: String,
    url: Option<Url>,
    /// The `blob:` URL that `open` got. Then `url` is a stand-in (the document URL) and `send` reads the Blob URL store.
    blob: Option<String>,
    /// Lowercase names, values combined with ", ".
    headers: Vec<(String, String)>,
    timeout: u32,
    creds: bool,
    rt: Rt,
    mime: Option<String>,
    /// Changes at each `open`, `abort` and error, so a late delivery finds that its request is gone.
    seq: u64,
    started: f64,
    body_len: usize,
    resp: Option<Resp>,
    cache: Option<Value>,
    upload: ObjId,
}

#[derive(Clone, Copy)]
struct Made {
    /// `XMLHttpRequest`, `XMLHttpRequestEventTarget`, `XMLHttpRequestUpload`, `ProgressEvent` prototypes (the order of `NAMES`).
    protos: [ObjId; 4],
    /// The constructors in the order of `NAMES`.
    ctors: [ObjId; 4],
}

#[derive(Default)]
struct Tables {
    made: Option<Made>,
    objs: IdMap<ObjId, Xhr>,
    ups: HashSet<ObjId>,
    /// `(loaded, total, lengthComputable)` of a `ProgressEvent`.
    prog: IdMap<ObjId, (f64, f64, bool)>,
}

#[derive(Default)]
pub(crate) struct XhrHost {
    s: RefCell<Tables>,
    /// An object was made and not pruned: `Dom::trace` skips `trace` below while this is false.
    pub(crate) active: Cell<bool>,
}

// ------------------------------------------------------------------------------------ GC hooks

pub(crate) fn roots(d: &Dom, out: &mut Vec<Value>) {
    if let Some(m) = d.xhr.s.borrow().made {
        out.extend(m.protos.into_iter().chain(m.ctors).map(Value::Object));
    }
}

pub(crate) fn trace(d: &Dom, owner: ObjId, out: &mut Vec<Value>) {
    if let Some(x) = d.xhr.s.borrow().objs.get(&owner) {
        out.push(Value::Object(x.upload));
        out.extend(x.cache);
    }
}

pub(crate) fn prune(d: &Dom, live: &dyn Fn(ObjId) -> bool) {
    let mut t = d.xhr.s.borrow_mut();
    t.objs.retain(|&o, _| live(o));
    t.ups.retain(|&o| live(o));
    t.prog.retain(|&o, _| live(o));
    d.xhr.active.set(!t.objs.is_empty() || !t.prog.is_empty());
}

// ------------------------------------------------------------------------------------ helpers

fn token(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        204 => "No Content",
        206 => "Partial Content",
        301 => "Moved Permanently",
        302 => "Found",
        303 => "See Other",
        304 => "Not Modified",
        307 => "Temporary Redirect",
        308 => "Permanent Redirect",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        410 => "Gone",
        413 => "Content Too Large",
        415 => "Unsupported Media Type",
        422 => "Unprocessable Content",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "",
    }
}

impl Dom {
    fn xo(&self, it: &mut Interp<'_>, this: Value) -> R<ObjId> {
        match this {
            Value::Object(o) if self.xhr.s.borrow().objs.contains_key(&o) => Ok(o),
            _ => Err(it.type_error("Illegal invocation")),
        }
    }

    /// Read or change the state of `o`. `o` must be an `XMLHttpRequest`.
    fn xs<T>(&self, o: ObjId, f: impl FnOnce(&mut Xhr) -> T) -> T {
        f(self.xhr.s.borrow_mut().objs.get_mut(&o).expect("an XMLHttpRequest"))
    }

    fn xerr(&self, it: &mut Interp<'_>, name: &str, what: &str, msg: &str) -> Error {
        self.dom_err(it, name, &format!("Failed to execute '{what}' on 'XMLHttpRequest': {msg}"))
    }

    fn event_object(&self, it: &mut Interp<'_>, proto: ObjId) -> R<ObjId> {
        if self.st.borrow().by_obj.len() >= self.lim.max_wrappers || self.xhr.s.borrow().objs.len() >= MAX_OBJECTS {
            return Err(self.lim("dom-wrappers"));
        }
        let o = self.obj(it, proto)?;
        self.st.borrow_mut().by_obj.insert(o, Target::Obj(o));
        Ok(o)
    }

    /// Fire `ty` at `t` (an `XMLHttpRequest` or its upload). With `Some((loaded, total))` it is a `ProgressEvent`. No object is made
    /// when nobody listens. The event is trusted.
    fn xfire(&self, it: &mut Interp<'_>, t: ObjId, ty: &str, p: Option<(f64, f64)>) -> R<()> {
        if !self.st.borrow().listeners.get(&Target::Obj(t)).is_some_and(|v| v.iter().any(|l| &*l.ty == ty)) {
            return Ok(());
        }
        let proto = if p.is_some() { self.xhr.s.borrow().made.expect("made").protos[3] } else { self.p().event };
        let ev = self.new_event_proto(it, ty, false, false, false, proto)?;
        if let Some((loaded, total)) = p {
            self.xhr.s.borrow_mut().prog.insert(ev, (loaded, total, total != 0.0));
            self.xhr.active.set(true);
        }
        self.st.borrow_mut().events.get_mut(&ev).expect("just made").trusted = true;
        self.dispatch(it, Target::Obj(t), ev).map(|_| ())
    }

    fn upload_of(&self, x: ObjId) -> ObjId {
        self.xs(x, |x| x.upload)
    }

    /// Is the request `(x, seq)` still the one in flight?
    fn xcurrent(&self, x: ObjId, seq: u64) -> bool {
        self.xhr.s.borrow().objs.get(&x).is_some_and(|x| x.seq == seq && x.send)
    }

    /// The "request error steps": `ev` is `error`, `abort` or `timeout`.
    fn request_error(&self, it: &mut Interp<'_>, x: ObjId, ev: &str) -> R<()> {
        let up = self.upload_of(x);
        self.xs(x, |x| {
            (x.state, x.send, x.resp, x.cache) = (DONE, false, None, None);
            x.seq += 1;
        });
        self.xfire(it, x, "readystatechange", None)?;
        let (pending, listening) = self.xs(x, |x| (!x.upload_done, x.up_listener));
        if pending {
            self.xs(x, |x| x.upload_done = true);
            if listening {
                self.xfire(it, up, ev, Some((0.0, 0.0)))?;
                self.xfire(it, up, "loadend", Some((0.0, 0.0)))?;
            }
        }
        self.xfire(it, x, ev, Some((0.0, 0.0)))?;
        self.xfire(it, x, "loadend", Some((0.0, 0.0)))
    }

    /// `abort()`. Also the abort of a request that was queued when the page left (`Browser::abort_fetches`).
    fn xabort(&self, it: &mut Interp<'_>, x: ObjId) -> R<()> {
        let (state, send) = self.xs(x, |x| (x.state, x.send));
        if (state == OPENED && send) || state == HEADERS || state == LOADING {
            dequeue_xhr(self, x);
            self.request_error(it, x, "abort")?;
        }
        self.xs(x, |x| {
            if x.state == DONE {
                (x.state, x.resp, x.cache) = (UNSENT, None, None);
            }
        });
        Ok(())
    }

    pub(crate) fn xhr_abort_queued(&self, it: &mut Interp<'_>, x: ObjId, seq: u64) -> R<()> {
        if self.xcurrent(x, seq) { self.xabort(it, x) } else { Ok(()) }
    }

    fn xhr_target(&self, it: &mut Interp<'_>, this: Value) -> R<Target> {
        match this {
            Value::Object(o) if { let t = self.xhr.s.borrow(); t.objs.contains_key(&o) || t.ups.contains(&o) } => Ok(Target::Obj(o)),
            _ => Err(it.type_error("Illegal invocation")),
        }
    }
}

// ------------------------------------------------------------------------------------ constructors

fn illegal(_: &Dom, it: &mut Interp<'_>, _: Value, _: &[Value]) -> V {
    Err(it.type_error("Illegal constructor"))
}

fn xhr_ctor(d: &Dom, it: &mut Interp<'_>, _: Value, _: &[Value]) -> V {
    let m = d.xhr.s.borrow().made.expect("made");
    let o = d.event_object(it, m.protos[0])?;
    let upload = d.event_object(it, m.protos[2])?;
    let x = Xhr {
        state: UNSENT,
        send: false,
        upload_done: false,
        up_listener: false,
        method: String::new(),
        url: None,
        blob: None,
        headers: Vec::new(),
        timeout: 0,
        creds: false,
        rt: Rt::Empty,
        mime: None,
        seq: 0,
        started: 0.0,
        body_len: 0,
        resp: None,
        cache: None,
        upload,
    };
    let mut t = d.xhr.s.borrow_mut();
    t.objs.insert(o, x);
    t.ups.insert(upload);
    d.xhr.active.set(true);
    Ok(Value::Object(o))
}

fn progress_ctor(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> V {
    if a.is_empty() {
        return Err(it.type_error("Failed to construct 'ProgressEvent': 1 argument required, but only 0 present."));
    }
    let ty = d.string_arg(it, a[0])?;
    let init = arg(a, 1);
    let (mut flags, mut nums) = ([false; 4], [0.0f64; 2]);
    match init {
        Value::Undefined | Value::Null => {}
        Value::Object(_) => {
            for (i, n) in ["bubbles", "cancelable", "composed", "lengthComputable"].into_iter().enumerate() {
                flags[i] = crate::browser_events::init_flag(d, it, init, n)?;
            }
            for (i, n) in ["loaded", "total"].into_iter().enumerate() {
                let k = d.key(it, n)?;
                let v = it.get_value(init, k)?;
                let x = it.to_js_number(v)?;
                nums[i] = if x.is_finite() && x > 0.0 { x.trunc() } else { 0.0 };
            }
        }
        _ => return Err(it.type_error("The provided value is not of type 'ProgressEventInit'.")),
    }
    let proto = d.xhr.s.borrow().made.expect("made").protos[3];
    let ev = d.new_event_proto(it, &ty, flags[0], flags[1], flags[2], proto)?;
    d.xhr.s.borrow_mut().prog.insert(ev, (nums[0], nums[1], flags[3]));
    d.xhr.active.set(true);
    Ok(Value::Object(ev))
}

fn prog_field(d: &Dom, it: &mut Interp<'_>, this: Value, f: fn((f64, f64, bool)) -> Value) -> V {
    match this {
        Value::Object(o) => match d.xhr.s.borrow().prog.get(&o) {
            Some(&p) => Ok(f(p)),
            None => Err(it.type_error("Illegal invocation")),
        },
        _ => Err(it.type_error("Illegal invocation")),
    }
}

fn prog_loaded(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    prog_field(d, it, this, |p| Value::Number(p.0))
}

fn prog_total(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    prog_field(d, it, this, |p| Value::Number(p.1))
}

fn prog_computable(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    prog_field(d, it, this, |p| Value::Bool(p.2))
}

// ------------------------------------------------------------------------------------ methods

fn open(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let x = d.xo(it, this)?;
    if a.len() < 2 {
        return Err(it.type_error(&format!("Failed to execute 'open' on 'XMLHttpRequest': 2 arguments required, but only {} present.", a.len())));
    }
    let method = d.string_arg(it, a[0])?;
    let url = d.string_arg(it, a[1])?;
    let asynchronous = match arg(a, 2) {
        Value::Undefined => true,
        v => it.to_js_bool(v)?,
    };
    for &v in a.iter().skip(3) {
        if !matches!(v, Value::Undefined | Value::Null) {
            return Err(Error::Unsupported(unsupported("XMLHttpRequest.open with a user name or a password")));
        }
    }
    if !token(&method) {
        return Err(d.xerr(it, "SyntaxError", "open", &format!("'{method}' is not a valid HTTP method.")));
    }
    if ["CONNECT", "TRACE", "TRACK"].iter().any(|m| m.eq_ignore_ascii_case(&method)) {
        return Err(d.xerr(it, "SecurityError", "open", &format!("'{method}' HTTP method is unsupported.")));
    }
    let method = if ["DELETE", "GET", "HEAD", "OPTIONS", "POST", "PUT"].iter().any(|m| m.eq_ignore_ascii_case(&method)) { method.to_ascii_uppercase() } else { method };
    if url.len() > d.fl().max_url_bytes {
        return Err(d.lim("fetch-url"));
    }
    let s = url.trim_matches(|c: char| c <= ' ');
    let blob = crate::browser_web::is_blob_url(s).then(|| s.to_string());
    let parsed = match d.fetch_base() {
        Some(b) if blob.is_some() => Ok(b),
        Some(b) => b.join(s),
        None if blob.is_some() => Url::parse("http://blob.invalid/"),
        None => Url::parse(s),
    };
    let Ok(parsed) = parsed else { return Err(d.xerr(it, "SyntaxError", "open", &format!("Invalid URL '{url}'."))) };
    if !asynchronous {
        return Err(Error::Unsupported(unsupported("synchronous XMLHttpRequest (async = false)")));
    }
    dequeue_xhr(d, x);
    let was = d.xs(x, |x| {
        let was = x.state;
        (x.method, x.url, x.blob, x.send, x.resp, x.cache, x.state) = (method, Some(parsed), blob, false, None, None, OPENED);
        x.headers.clear();
        x.seq += 1;
        was
    });
    if was != OPENED {
        d.xfire(it, x, "readystatechange", None)?;
    }
    Ok(Value::Undefined)
}

fn set_request_header(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let x = d.xo(it, this)?;
    if a.len() < 2 {
        return Err(it.type_error(&format!("Failed to execute 'setRequestHeader' on 'XMLHttpRequest': 2 arguments required, but only {} present.", a.len())));
    }
    let name = d.string_arg(it, a[0])?;
    let value = d.string_arg(it, a[1])?;
    if d.xs(x, |x| x.state != OPENED || x.send) {
        return Err(d.xerr(it, "InvalidStateError", "setRequestHeader", "The object's state must be OPENED."));
    }
    let value = value.trim_matches(['\t', '\n', '\r', ' ']);
    if !token(&name) {
        return Err(d.xerr(it, "SyntaxError", "setRequestHeader", &format!("'{name}' is not a valid HTTP header field name.")));
    }
    if value.contains(['\0', '\r', '\n']) {
        return Err(d.xerr(it, "SyntaxError", "setRequestHeader", &format!("'{value}' is not a valid HTTP header field value.")));
    }
    if !value.is_ascii() {
        return Err(Error::Unsupported(unsupported("a non-ASCII XMLHttpRequest header value")));
    }
    let name = name.to_ascii_lowercase();
    if forbidden_request(&name) {
        return Ok(Value::Undefined);
    }
    let lim = d.fl();
    d.xs(x, |x| {
        if x.headers.len() >= lim.max_headers && !x.headers.iter().any(|(n, _)| *n == name) {
            return Err(d.lim("fetch-headers"));
        }
        match x.headers.iter_mut().find(|(n, _)| *n == name) {
            Some((_, v)) => {
                v.push_str(", ");
                v.push_str(value);
            }
            None => x.headers.push((name, value.to_string())),
        }
        Ok(())
    })?;
    Ok(Value::Undefined)
}

fn send(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let x = d.xo(it, this)?;
    if d.xs(x, |x| x.state != OPENED || x.send) {
        return Err(d.xerr(it, "InvalidStateError", "send", "The object's state must be OPENED."));
    }
    let (method, url, mut headers, seq, creds) = d.xs(x, |x| (x.method.clone(), x.url.as_ref().map(|u| Url::parse(&u.to_string()).expect("a parsed URL")), x.headers.clone(), x.seq, x.creds));
    let url = url.expect("opened");
    if let Some(blob) = d.xs(x, |x| x.blob.clone()) {
        // A Blob URL is read from the store, with no body and no network. Only GET reads it.
        let found = (method == "GET").then(|| crate::browser_web::blob_url_read(d, &blob)).flatten();
        queue_xhr_blob(d, (x, seq), url, found)?;
        return send_started(d, it, x, seq, 0);
    }
    // A GET that asks for `text/event-stream` (as `EventSource` does) is read as it comes: see `queue_xhr_stream`.
    if method == "GET" && headers.iter().any(|(n, v)| n == "accept" && v.trim().eq_ignore_ascii_case("text/event-stream")) {
        queue_xhr_stream(d, (x, seq), url, headers, creds)?;
        return send_started(d, it, x, seq, 0);
    }
    let mut body = None;
    if method != "GET" && method != "HEAD" {
        let v = arg(a, 0);
        if let Value::Object(o) = v
            && matches!(d.st.borrow().by_obj.get(&o), Some(Target::Node(_)))
        {
            return Err(Error::Unsupported(unsupported("a Document as an XMLHttpRequest body")));
        }
        if let Some((bytes, ty)) = d.extract_body(it, v, "execute 'send' on 'XMLHttpRequest'")? {
            if let Some(ty) = ty
                && !headers.iter().any(|(n, _)| n == "content-type")
            {
                headers.push(("content-type".into(), ty.to_string()));
            }
            body = Some(bytes);
        }
    }
    let body_len = body.as_ref().map_or(0, |b| b.len());
    queue_xhr(d, (x, seq), &method, url, headers, body, creds)?;
    send_started(d, it, x, seq, body_len)
}

/// After `send` queued the request: mark it sent and fire `loadstart`.
fn send_started(d: &Dom, it: &mut Interp<'_>, x: ObjId, seq: u64, body_len: usize) -> V {
    let up = d.upload_of(x);
    let listening = d.st.borrow().listeners.get(&Target::Obj(up)).is_some_and(|v| !v.is_empty());
    let now = d.now_ms();
    d.xs(x, |x| {
        (x.send, x.upload_done, x.up_listener, x.body_len, x.started) = (true, body_len == 0, listening, body_len, now);
    });
    d.xfire(it, x, "loadstart", Some((0.0, 0.0)))?;
    if body_len != 0 && listening && d.xcurrent(x, seq) {
        d.xfire(it, up, "loadstart", Some((0.0, body_len as f64)))?;
    }
    Ok(Value::Undefined)
}

fn abort(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let x = d.xo(it, this)?;
    d.xabort(it, x)?;
    Ok(Value::Undefined)
}

fn ready_state(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let x = d.xo(it, this)?;
    Ok(Value::Number(f64::from(d.xs(x, |x| x.state))))
}

fn upload(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let x = d.xo(it, this)?;
    Ok(Value::Object(d.upload_of(x)))
}

fn status(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let x = d.xo(it, this)?;
    Ok(Value::Number(d.xs(x, |x| x.resp.as_ref().map_or(0, |r| r.status)).into()))
}

fn status_text(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let x = d.xo(it, this)?;
    let s = d.xs(x, |x| x.resp.as_ref().map_or("", |r| reason(r.status)));
    d.js_str(it, s)
}

fn response_url(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let x = d.xo(it, this)?;
    let s = d.xs(x, |x| x.resp.as_ref().map_or(String::new(), |r| r.url.split('#').next().unwrap_or("").to_string()));
    d.js_str(it, &s)
}

fn get_response_header(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let x = d.xo(it, this)?;
    if a.is_empty() {
        return Err(it.type_error("Failed to execute 'getResponseHeader' on 'XMLHttpRequest': 1 argument required, but only 0 present."));
    }
    let name = d.string_arg(it, a[0])?.to_ascii_lowercase();
    let v = d.xs(x, |x| {
        let r = x.resp.as_ref()?;
        let all: Vec<&str> = r.headers.iter().filter(|(n, _)| *n == name).map(|(_, v)| v.as_str()).collect();
        (!all.is_empty()).then(|| all.join(", "))
    });
    match v {
        Some(s) => d.js_str(it, &s),
        None => Ok(Value::Null),
    }
}

fn get_all_response_headers(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let x = d.xo(it, this)?;
    let s = d.xs(x, |x| {
        let mut out = String::new();
        let Some(r) = x.resp.as_ref() else { return out };
        let mut names: Vec<&str> = r.headers.iter().map(|(n, _)| n.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        for n in names {
            let all: Vec<&str> = r.headers.iter().filter(|(k, _)| k == n).map(|(_, v)| v.as_str()).collect();
            out.push_str(&format!("{n}: {}\r\n", all.join(", ")));
        }
        out
    });
    d.js_str(it, &s)
}

fn response_type(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let x = d.xo(it, this)?;
    let n = d.xs(x, |x| x.rt.name());
    d.js_str(it, n)
}

fn set_response_type(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let x = d.xo(it, this)?;
    let s = d.string_arg(it, arg(a, 0))?;
    if d.xs(x, |x| x.state == LOADING || x.state == DONE) {
        return Err(d.dom_err(it, "InvalidStateError", "Failed to set the 'responseType' property on 'XMLHttpRequest': The response type cannot be set if the object's state is LOADING or DONE."));
    }
    let rt = match s.as_str() {
        "" => Rt::Empty,
        "text" => Rt::Text,
        "arraybuffer" => Rt::Buffer,
        "blob" => Rt::Blob,
        "json" => Rt::Json,
        "document" => return Err(Error::Unsupported(unsupported("XMLHttpRequest responseType document"))),
        _ => return Ok(Value::Undefined),
    };
    d.xs(x, |x| x.rt = rt);
    Ok(Value::Undefined)
}

fn timeout(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let x = d.xo(it, this)?;
    Ok(Value::Number(f64::from(d.xs(x, |x| x.timeout))))
}

fn set_timeout(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let x = d.xo(it, this)?;
    let n = it.to_js_number(arg(a, 0))?;
    let n = if n.is_finite() { n.trunc().rem_euclid(4294967296.0) as u32 } else { 0 };
    d.xs(x, |x| x.timeout = n);
    Ok(Value::Undefined)
}

fn with_credentials(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let x = d.xo(it, this)?;
    Ok(Value::Bool(d.xs(x, |x| x.creds)))
}

fn set_with_credentials(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let x = d.xo(it, this)?;
    let b = it.to_js_bool(arg(a, 0))?;
    if d.xs(x, |x| (x.state != UNSENT && x.state != OPENED) || x.send) {
        return Err(d.dom_err(it, "InvalidStateError", "Failed to set the 'withCredentials' property on 'XMLHttpRequest': The value may only be set if the object's state is UNSENT or OPENED."));
    }
    d.xs(x, |x| x.creds = b);
    Ok(Value::Undefined)
}

fn override_mime_type(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let x = d.xo(it, this)?;
    if a.is_empty() {
        return Err(it.type_error("Failed to execute 'overrideMimeType' on 'XMLHttpRequest': 1 argument required, but only 0 present."));
    }
    let s = d.string_arg(it, a[0])?;
    if d.xs(x, |x| x.state == LOADING || x.state == DONE) {
        return Err(d.xerr(it, "InvalidStateError", "overrideMimeType", "MimeType cannot be overridden when the state is LOADING or DONE."));
    }
    d.xs(x, |x| x.mime = Some(s));
    Ok(Value::Undefined)
}

// ------------------------------------------------------------------------------------ response

/// The essence (`type/subtype`, lower case) of a content type, or `""`.
fn essence(ct: &str) -> String {
    let e = ct.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    if e.contains('/') { e } else { String::new() }
}

fn charset(ct: &str) -> Option<String> {
    ct.split(';').skip(1).find_map(|p| p.trim().strip_prefix("charset=").map(|c| c.trim_matches('"').to_ascii_lowercase()))
}

impl Dom {
    /// The final MIME type text of the response: the override, else the `Content-Type` header.
    fn final_mime(&self, x: ObjId) -> String {
        self.xs(x, |x| x.mime.clone().or_else(|| x.resp.as_ref().and_then(|r| r.headers.iter().find(|(n, _)| n == "content-type").map(|(_, v)| v.clone()))).unwrap_or_default())
    }

    fn response_text_of(&self, it: &mut Interp<'_>, x: ObjId) -> V {
        let Some(body) = self.xs(x, |x| if x.state == LOADING || x.state == DONE { x.resp.as_ref().map(|r| r.body.clone()) } else { None }) else { return self.js_str(it, "") };
        if let Some(cs) = charset(&self.final_mime(x))
            && !matches!(cs.as_str(), "utf-8" | "utf8" | "unicode-1-1-utf-8")
            && (cs.starts_with("utf-16") || !body.is_ascii())
        {
            return Err(Error::Unsupported(unsupported("an XMLHttpRequest response text in a charset other than UTF-8")));
        }
        it.charge((body.len() / 16) as u64)?;
        let text = String::from_utf8_lossy(body.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&body)).into_owned();
        self.js_str(it, &text)
    }
}

fn response_text(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let x = d.xo(it, this)?;
    let rt = d.xs(x, |x| x.rt);
    if !matches!(rt, Rt::Empty | Rt::Text) {
        return Err(d.dom_err(it, "InvalidStateError", &format!("Failed to read the 'responseText' property from 'XMLHttpRequest': The value is only accessible if the object's 'responseType' is '' or 'text' (was '{}').", rt.name())));
    }
    d.response_text_of(it, x)
}

fn response(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let x = d.xo(it, this)?;
    let (rt, state, cache) = d.xs(x, |x| (x.rt, x.state, x.cache));
    match rt {
        Rt::Empty | Rt::Text => d.response_text_of(it, x),
        _ if state != DONE => Ok(Value::Null),
        _ if cache.is_some() => Ok(cache.expect("some")),
        _ => {
            let Some(body) = d.xs(x, |x| x.resp.as_ref().map(|r| r.body.clone())) else { return Ok(Value::Null) };
            it.charge((body.len() / 64) as u64)?;
            let v = match rt {
                Rt::Buffer => it.new_array_buffer(body.to_vec())?,
                Rt::Blob => crate::browser_web::blob_from_bytes(d, it, body.to_vec(), essence(&d.final_mime(x)))?,
                _ => {
                    let text = d.response_text_of(it, x)?;
                    let Some(parse) = d.fetch.json_parse.get() else { return Err(Error::Unsupported(unsupported("XMLHttpRequest json without JSON.parse"))) };
                    it.charge((body.len() / 16) as u64)?;
                    return match it.call(parse, Value::Undefined, &[text]) {
                        Err(Error::Throw(_)) => Ok(Value::Null),
                        r => r,
                    };
                }
            };
            d.xs(x, |x| x.cache = Some(v));
            Ok(v)
        }
    }
}

// ------------------------------------------------------------------------------------ delivery

impl Browser {
    /// A reply (or a failure) of the request `(x, seq)` came back from the pump of `fetch`. Fires the events, as the header says.
    pub(crate) fn xhr_deliver(&mut self, x: ObjId, seq: u64, outcome: Outcome) -> R<()> {
        let dom = self.dom.clone();
        if !dom.xcurrent(x, seq) {
            return Ok(());
        }
        // A microtask checkpoint follows each listener, as after a task.
        dom.st.borrow_mut().driver = true;
        let r = self.xhr_steps(&dom, x, seq, outcome);
        dom.st.borrow_mut().driver = false;
        r
    }

    fn step(&mut self, f: impl FnOnce(&mut Interp<'_>) -> R<()>) -> R<()> {
        self.it.host_scope(|it| {
            f(it)?;
            it.host_checkpoint().map(|_| ())
        })
    }

    fn xhr_steps(&mut self, dom: &Rc<Dom>, x: ObjId, seq: u64, outcome: Outcome) -> R<()> {
        let now = dom.now_ms();
        let late = dom.xs(x, |x| x.timeout != 0 && now - x.started >= f64::from(x.timeout));
        let (status, url, headers, body) = match outcome {
            Outcome::TooBig => return Err(dom.lim("fetch-response-body")),
            _ if late => return self.step(|it| dom.request_error(it, x, "timeout")),
            Outcome::Failed(_) | Outcome::Net(_) | Outcome::Blocked(_) => return self.step(|it| dom.request_error(it, x, "error")),
            Outcome::Done { status, url, headers, body, .. } => (status, url, headers, body),
        };
        {
            let t = dom.xhr.s.borrow();
            let held: usize = t.objs.values().filter_map(|x| x.resp.as_ref()).map(|r| r.body.len()).sum();
            if held.saturating_add(body.len()) > dom.fl().max_retained {
                return Err(dom.lim("xhr-retained"));
            }
        }
        let (resp, total) = resp_of(status, &url, headers, body);
        let loaded = resp.body.len() as f64;
        self.xhr_headers(dom, x, seq, resp)?;
        // Step 2: the body.
        if loaded != 0.0 {
            self.step(|it| {
                if dom.xcurrent(x, seq) && dom.xs(x, |x| x.state) == HEADERS {
                    dom.xs(x, |x| x.state = LOADING);
                    dom.xfire(it, x, "readystatechange", None)?;
                    if dom.xcurrent(x, seq) && dom.xs(x, |x| x.state) == LOADING {
                        dom.xfire(it, x, "progress", Some((loaded, total)))?;
                    }
                }
                Ok(())
            })?;
        }
        // Step 3: the end.
        self.step(|it| {
            if dom.xcurrent(x, seq) {
                dom.xs(x, |x| (x.state, x.send) = (DONE, false));
                dom.xfire(it, x, "readystatechange", None)?;
                dom.xfire(it, x, "load", Some((loaded, total)))?;
                dom.xfire(it, x, "loadend", Some((loaded, total)))?;
            }
            Ok(())
        })
    }

    /// Step 1 of a delivery: the end of the upload, then the headers.
    fn xhr_headers(&mut self, dom: &Rc<Dom>, x: ObjId, seq: u64, resp: Resp) -> R<()> {
        self.step(|it| {
            let (pending, listening, len, up) = dom.xs(x, |x| (!x.upload_done, x.up_listener, x.body_len as f64, x.upload));
            if pending {
                dom.xs(x, |x| x.upload_done = true);
                if listening {
                    for ty in ["progress", "load", "loadend"] {
                        dom.xfire(it, up, ty, Some((len, len)))?;
                    }
                }
            }
            if !dom.xcurrent(x, seq) {
                return Ok(());
            }
            dom.xs(x, |x| (x.resp, x.state) = (Some(resp), HEADERS));
            dom.xfire(it, x, "readystatechange", None)
        })
    }

    /// A message of an event stream (`queue_xhr_stream`): the headers, a chunk of the body (state 3, `readystatechange` and
    /// `progress` with the bytes so far, as in Chrome), the end (state 4, `load`, `loadend`) or a network error.
    pub(crate) fn xhr_stream_deliver(&mut self, x: ObjId, seq: u64, msg: StreamMsg) -> R<()> {
        let dom = self.dom.clone();
        if !dom.xcurrent(x, seq) {
            return Ok(());
        }
        dom.st.borrow_mut().driver = true;
        let r = match msg {
            StreamMsg::Head(Outcome::Done { status, url, headers, body, .. }) => self.xhr_headers(&dom, x, seq, resp_of(status, &url, headers, body).0),
            StreamMsg::Head(_) | StreamMsg::End(Some(_)) => self.step(|it| dom.request_error(it, x, "error")),
            StreamMsg::Chunk(bytes) => self.step(|it| {
                let Some(loaded) = dom.xs(x, |x| {
                    let r = x.resp.as_mut()?;
                    Rc::make_mut(&mut r.body).extend_from_slice(&bytes);
                    x.state = LOADING;
                    Some(r.body.len() as f64)
                }) else {
                    return Ok(());
                };
                dom.xfire(it, x, "readystatechange", None)?;
                if dom.xcurrent(x, seq) && dom.xs(x, |x| x.state) == LOADING {
                    dom.xfire(it, x, "progress", Some((loaded, 0.0)))?;
                }
                Ok(())
            }),
            StreamMsg::End(None) => self.step(|it| {
                if dom.xcurrent(x, seq) {
                    let loaded = dom.xs(x, |x| {
                        (x.state, x.send) = (DONE, false);
                        x.resp.as_ref().map_or(0.0, |r| r.body.len() as f64)
                    });
                    dom.xfire(it, x, "readystatechange", None)?;
                    dom.xfire(it, x, "load", Some((loaded, 0.0)))?;
                    dom.xfire(it, x, "loadend", Some((loaded, 0.0)))?;
                }
                Ok(())
            }),
        };
        dom.st.borrow_mut().driver = false;
        r
    }
}

/// The response of a reply, as the page sees it, and its `Content-Length` (0 when absent).
fn resp_of(status: u16, url: &Url, headers: Vec<(String, String)>, body: Vec<u8>) -> (Resp, f64) {
    let total = headers.iter().find(|(n, _)| n == "content-length").and_then(|(_, v)| v.trim().parse::<u64>().ok()).unwrap_or(0) as f64;
    let headers: Vec<(String, String)> = headers
        .into_iter()
        .filter(|(n, _)| n != "set-cookie" && n != "set-cookie2")
        .map(|(n, v)| (n.to_ascii_lowercase(), v.trim_matches(['\t', ' ']).to_string()))
        .collect();
    (Resp { status, url: url.to_string(), headers, body: Rc::new(body) }, total)
}

// ------------------------------------------------------------------------------------ install

fn xh_get<const N: usize>(d: &Dom, it: &mut Interp<'_>, this: Value, _: &[Value]) -> V {
    let t = d.xhr_target(it, this)?;
    Ok(handler_get(d, t, TYPES[N]))
}

fn xh_set<const N: usize>(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let t = d.xhr_target(it, this)?;
    handler_set(d, t, TYPES[N], a.first().copied())?;
    Ok(Value::Undefined)
}

/// The globals are accessors until one is read. A call with an argument is the setter.
fn lazy<const I: usize>(d: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> V {
    let g = *d.global.get().expect("installed");
    let k = d.key(it, NAMES[I])?;
    if let Some(&v) = a.first() {
        it.define_data(g, k, v, true, false, true)?;
        return Ok(Value::Undefined);
    }
    let m = made(d, it)?;
    it.define_data(g, k, Value::Object(m.ctors[I]), true, false, true)?;
    Ok(Value::Object(m.ctors[I]))
}

fn made(d: &Dom, it: &mut Interp<'_>) -> R<Made> {
    let m = d.xhr.s.borrow().made;
    match m {
        Some(m) => Ok(m),
        None => make(d, it),
    }
}

#[cold]
#[inline(never)]
fn make(d: &Dom, it: &mut Interp<'_>) -> R<Made> {
    let g = *d.global.get().expect("installed");
    let (et, ev) = (d.p().et, d.p().event);
    let xet = d.obj(it, et)?;
    // `XMLHttpRequest` and `XMLHttpRequestUpload` inherit from `XMLHttpRequestEventTarget`.
    let (xp, up, pp) = (d.obj(it, xet)?, d.obj(it, xet)?, d.obj(it, ev)?);
    let (kp, kc) = (d.key(it, "prototype")?, d.key(it, "constructor")?);
    let handlers: [H; 4] = [xhr_ctor, illegal, illegal, progress_ctor];
    let protos = [xp, xet, up, pp];
    let mut ctors = [xet; 4];
    for (i, name) in NAMES.iter().enumerate() {
        let p = protos[i];
        let f = d.add_fn(it, handlers[i], name, u32::from(i == 3), true)?;
        it.define_data(f, kp, Value::Object(p), false, false, false)?;
        it.define_data(p, kc, Value::Object(f), true, false, true)?;
        let (ks, kt) = (d.key(it, "Symbol")?, d.key(it, "toStringTag")?);
        let sy = it.get_value(Value::Object(g), ks)?;
        if let Value::Symbol(s) = it.get_value(sy, kt)? {
            let t = d.js_str(it, name)?;
            it.define_data(p, PropertyKey::Symbol(s), t, false, false, true)?;
        }
        ctors[i] = f;
    }
    for (n, v) in ["UNSENT", "OPENED", "HEADERS_RECEIVED", "LOADING", "DONE"].into_iter().enumerate() {
        let k = d.key(it, v)?;
        for o in [ctors[0], xp] {
            it.define_data(o, k, Value::Number(n as f64), false, true, false)?;
        }
    }
    let gets: [H; 8] = [xh_get::<0>, xh_get::<1>, xh_get::<2>, xh_get::<3>, xh_get::<4>, xh_get::<5>, xh_get::<6>, xh_get::<7>];
    let sets: [H; 8] = [xh_set::<0>, xh_set::<1>, xh_set::<2>, xh_set::<3>, xh_set::<4>, xh_set::<5>, xh_set::<6>, xh_set::<7>];
    for n in 0..8 {
        d.def_get(it, if n == 0 { xp } else { xet }, &format!("on{}", TYPES[n]), gets[n], Some(sets[n]))?;
    }
    for (name, len, h) in [
        ("open", 2, open as H),
        ("setRequestHeader", 2, set_request_header),
        ("send", 0, send),
        ("abort", 0, abort),
        ("getResponseHeader", 1, get_response_header),
        ("getAllResponseHeaders", 0, get_all_response_headers),
        ("overrideMimeType", 1, override_mime_type),
    ] {
        d.def_method(it, xp, name, len, h)?;
    }
    for (name, get, set) in [
        ("readyState", ready_state as H, None),
        ("status", status, None),
        ("statusText", status_text, None),
        ("responseURL", response_url, None),
        ("responseText", response_text, None),
        ("response", response, None),
        ("upload", upload, None),
        ("responseType", response_type, Some(set_response_type as H)),
        ("timeout", timeout, Some(set_timeout as H)),
        ("withCredentials", with_credentials, Some(set_with_credentials as H)),
    ] {
        d.def_get(it, xp, name, get, set)?;
    }
    for (name, get) in [("loaded", prog_loaded as H), ("total", prog_total), ("lengthComputable", prog_computable)] {
        d.def_get(it, pp, name, get, None)?;
    }
    let m = Made { protos, ctors };
    d.xhr.s.borrow_mut().made = Some(m);
    Ok(m)
}

pub(crate) fn install(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    let g = *d.global.get().expect("installed");
    let hs: [H; 4] = [lazy::<0>, lazy::<1>, lazy::<2>, lazy::<3>];
    for (name, h) in NAMES.into_iter().zip(hs) {
        let key = d.key(it, name)?;
        let f = d.add_fn(it, h, name, 0, false)?;
        let desc = PropertyDescriptor { get: Some(Some(f)), set: Some(Some(f)), enumerable: Some(false), configurable: Some(true), ..Default::default() };
        if let Err(e) = it.heap.define_own_property(g, key, &desc) {
            return Err(it.heap_error(e));
        }
    }
    Ok(())
}
