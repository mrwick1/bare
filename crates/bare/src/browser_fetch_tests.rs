//! Tests of `fetch`, `Headers`, `Request`, `Response` and the byte reader. Real scripts run in a real realm; the network is
//! a loopback HTTP server made here (throwaway synthetic data only). Expectations are written by hand from the Fetch
//! Standard; the pure parts also have a Node oracle (`tools/js/fetch_oracle.mjs`). Each test runs on a big stack.

use crate::browser::*;
use crate::browser_fetch::*;
use bare_html::tree;
use bare_js::eval::{Error, EvalLimits, LimitKind};
use bare_js::heap::HeapLimits;
use bare_js::value::Value;
use bare_net::http::Client;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Mutex};

pub(crate) fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

/// One request the server got.
#[derive(Clone, Debug)]
pub struct Rec {
    pub method: String,
    pub path: String,
    /// The head, lowercase, header lines only.
    pub head: String,
    pub body: Vec<u8>,
}

impl Rec {
    pub fn header(&self, name: &str) -> Vec<String> {
        let p = format!("{name}:");
        self.head.lines().filter_map(|l| l.strip_prefix(&p)).map(|v| v.trim().to_string()).collect()
    }
}

pub struct Reply {
    pub raw: Vec<u8>,
    /// Close the connection after the reply.
    pub close: bool,
}

pub fn ok(body: &str) -> Reply {
    reply(200, &[("Content-Type", "application/json")], body.as_bytes())
}

pub fn reply(status: u16, headers: &[(&str, &str)], body: &[u8]) -> Reply {
    let mut raw = format!("HTTP/1.1 {status} X\r\n");
    for (n, v) in headers {
        raw += &format!("{n}: {v}\r\n");
    }
    raw += &format!("Content-Length: {}\r\n\r\n", body.len());
    let mut raw = raw.into_bytes();
    raw.extend_from_slice(body);
    Reply { raw, close: false }
}

pub struct Srv {
    pub port: u16,
    pub recs: Arc<Mutex<Vec<Rec>>>,
    pub conns: Arc<AtomicUsize>,
    /// Connections that the server closed on purpose, counted after the close is sent.
    pub closed: Arc<AtomicUsize>,
}

impl Srv {
    pub fn recs(&self) -> Vec<Rec> {
        self.recs.lock().unwrap().clone()
    }
}

pub fn serve(handler: impl Fn(&Rec) -> Reply + Send + Sync + 'static) -> Srv {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (recs, conns, closed) = (Arc::new(Mutex::new(Vec::new())), Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let handler = Arc::new(handler);
    let (r2, c2, d2) = (recs.clone(), conns.clone(), closed.clone());
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            c2.fetch_add(1, SeqCst);
            let (h, r, d) = (handler.clone(), r2.clone(), d2.clone());
            std::thread::spawn(move || conn(stream, h, r, d));
        }
    });
    Srv { port, recs, conns, closed }
}

fn conn(mut s: TcpStream, h: Arc<dyn Fn(&Rec) -> Reply + Send + Sync>, recs: Arc<Mutex<Vec<Rec>>>, closed: Arc<AtomicUsize>) {
    let mut seen = Vec::new();
    let mut piece = [0u8; 8192];
    loop {
        let end = loop {
            if let Some(at) = seen.windows(4).position(|w| w == b"\r\n\r\n") {
                break at + 4;
            }
            match s.read(&mut piece) {
                Ok(0) | Err(_) => return,
                Ok(n) => seen.extend_from_slice(&piece[..n]),
            }
        };
        let head = String::from_utf8_lossy(&seen[..end]).to_string();
        seen.drain(..end);
        let lower = head.to_ascii_lowercase();
        let len = lower.lines().find_map(|l| l.strip_prefix("content-length:")).and_then(|v| v.trim().parse::<usize>().ok()).unwrap_or(0);
        while seen.len() < len {
            match s.read(&mut piece) {
                Ok(0) | Err(_) => return,
                Ok(n) => seen.extend_from_slice(&piece[..n]),
            }
        }
        let body: Vec<u8> = seen.drain(..len).collect();
        let mut first = head.lines().next().unwrap_or("").split_whitespace();
        let (method, path) = (first.next().unwrap_or("").to_string(), first.next().unwrap_or("").to_string());
        // Names are lowercase; values keep their case.
        let headers: String = head.lines().skip(1).map(|l| match l.split_once(':') { Some((n, v)) => format!("{}:{v}", n.to_ascii_lowercase()), None => l.to_string() }).collect::<Vec<_>>().join("\n");
        let rec = Rec { method, path, head: headers, body };
        recs.lock().unwrap().push(rec.clone());
        let r = h(&rec);
        if s.write_all(&r.raw).is_err() {
            return;
        }
        if r.close {
            // Send the FIN now, then say so: a test that waits for `closed` knows the client can see the close.
            let _ = s.shutdown(std::net::Shutdown::Both);
            closed.fetch_add(1, SeqCst);
            return;
        }
    }
}

pub(crate) fn mk(base: Option<String>, limits: FetchLimits) -> Browser {
    let mut b = Browser::new(tree::parse("<body></body>"), BrowserLimits::default(), EvalLimits { max_steps: 20_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
    b.configure_fetch(FetchConfig { base_url: base, client: Client::new(), limits }).unwrap();
    b
}

pub(crate) fn at(port: u16) -> Browser {
    mk(Some(format!("http://127.0.0.1:{port}/app/login")), FetchLimits::default())
}

fn show(b: &mut Browser, r: Result<Value, Error>) -> String {
    match r {
        Ok(v) => b.it.display(v),
        Err(Error::Throw(v)) if matches!(v, Value::Object(_)) => format!("throw:{}:{}", b.it.get_str(v, "name"), b.it.get_str(v, "message")),
        Err(Error::Throw(v)) => format!("throw-value:{}", b.it.display(v)),
        Err(Error::Limit(l)) => format!("limit:{l:?}"),
        Err(Error::Unsupported(u)) => format!("unsupported:{}", u.what),
        Err(e) => format!("other:{e:?}"),
    }
}

pub(crate) fn js(b: &mut Browser, src: &str) -> String {
    let r = b.run_script(src);
    show(b, r)
}

/// Run a script, run the microtasks, then all network tasks. Gives the value of `out.join('|')`.
pub(crate) fn go(b: &mut Browser, src: &str) -> String {
    let r = js(b, src);
    assert!(!r.starts_with("throw") && !r.starts_with("limit") && !r.starts_with("unsupported"), "{r}");
    b.drain().unwrap();
    while b.pending_fetches() > 0 {
        b.run_network_tasks(100).unwrap();
    }
    js(b, "out.join('|')")
}

fn lim(name: &'static str) -> String {
    format!("limit:{:?}", LimitKind::Host(name))
}

#[test]
fn an_unconfigured_browser_refuses_the_network() {
    big(|| {
        let srv = serve(|_| ok("{}"));
        let mut b = Browser::new(tree::parse("<body></body>"), BrowserLimits::default(), EvalLimits::default(), HeapLimits::default()).unwrap();
        let r = js(&mut b, &format!("fetch('http://127.0.0.1:{}/x')", srv.port));
        assert_eq!(r, "unsupported:fetch without a configured document origin (Browser::configure_fetch)");
        assert_eq!(js(&mut b, "typeof Headers + typeof Request + typeof Response + typeof fetch"), "functionfunctionfunctionfunction");
        // A configured browser with no base URL is the same.
        let mut b = mk(None, FetchLimits::default());
        assert!(js(&mut b, &format!("fetch('http://127.0.0.1:{}/x')", srv.port)).starts_with("unsupported:fetch without"));
        assert_eq!(b.pending_fetches(), 0);
        assert!(b.take_fetch_diagnostics().iter().any(|d| d.contains("no document origin")));
        assert_eq!(srv.recs().len(), 0);
        assert_eq!(srv.conns.load(SeqCst), 0);
    });
}

#[test]
fn fetch_only_queues_and_the_network_starts_in_the_driver() {
    big(|| {
        let srv = serve(|_| ok(r#"{"a":[1,2,{"b":"c"}]}"#));
        let mut b = at(srv.port);
        js(&mut b, "var out = []; fetch('/api/data').then(function (r) { out.push(r.status, r.ok); return r.json(); }).then(function (j) { out.push(j.a[2].b, j.a.length); });");
        b.drain().unwrap();
        assert_eq!((b.pending_fetches(), srv.conns.load(SeqCst), srv.recs().len()), (1, 0, 0), "no network before run_network_tasks");
        assert_eq!(js(&mut b, "out.join('|')"), "");
        assert_eq!(b.run_network_tasks(10).unwrap(), 1);
        assert_eq!(js(&mut b, "out.join('|')"), "200|true|c|3");
        let recs = srv.recs();
        assert_eq!(recs.len(), 1);
        assert_eq!((recs[0].method.as_str(), recs[0].path.as_str()), ("GET", "/api/data"));
        assert_eq!(recs[0].header("host"), vec![format!("127.0.0.1:{}", srv.port)]);
        assert_eq!(recs[0].header("accept"), vec!["*/*".to_string()]);
        assert!(recs[0].header("cookie").is_empty() && recs[0].header("origin").is_empty() && recs[0].header("referer").is_empty());
    });
}

#[test]
fn post_json_sends_exact_bytes_and_engine_made_framing() {
    big(|| {
        let srv = serve(|r| ok(&format!("{{\"len\":{}}}", r.body.len())));
        let mut b = at(srv.port);
        let body = r#"{"email":"a@example.test","password":"synthetic-é"}"#;
        let src =
            "var out = []; var body = JSON.stringify({email:'a@example.test', password:'synthetic-\\u00e9'});
             fetch('/api/auth/login', {method:'POST', headers:{'Content-Type':'application/json', 'Authorization':'Bearer tok', 'X-Req':'1'}, body: body})
               .then(function (r) { return r.json(); }).then(function (j) { out.push(j.len, body.length); });";
        assert_eq!(go(&mut b, src), format!("{}|{}", body.len(), body.chars().count()));
        let r = &srv.recs()[0];
        assert_eq!((r.method.as_str(), r.path.as_str()), ("POST", "/api/auth/login"));
        assert_eq!(r.body, body.as_bytes());
        assert_eq!(r.header("content-length"), vec![body.len().to_string()]);
        assert_eq!(r.header("content-type"), vec!["application/json".to_string()]);
        assert_eq!(r.header("authorization"), vec!["Bearer tok".to_string()]);
        assert_eq!(r.header("host").len(), 1);
    });
}

#[test]
fn bytes_and_string_bodies_get_the_right_content_type() {
    big(|| {
        let srv = serve(|_| ok("{}"));
        let mut b = at(srv.port);
        go(&mut b, "var out = []; fetch('/a', {method:'POST', body:'plain'}); fetch('/b', {method:'post', body:new Uint8Array([0, 255, 10])}); fetch('/c', {method:'POST', body:new Uint8Array([1,2,3,4]).subarray(1, 3).buffer});");
        let recs = srv.recs();
        let by = |p: &str| recs.iter().find(|r| r.path == p).unwrap().clone();
        assert_eq!((by("/a").body, by("/a").header("content-type")), (b"plain".to_vec(), vec!["text/plain;charset=UTF-8".to_string()]));
        assert_eq!((by("/b").body, by("/b").method, by("/b").header("content-type")), (vec![0, 255, 10], "POST".to_string(), vec![]));
        assert_eq!(by("/c").body.len(), 4, "an ArrayBuffer is the whole buffer");
    });
}

#[test]
fn managed_and_sensitive_headers_cannot_be_set_by_script() {
    big(|| {
        let srv = serve(|_| ok("{}"));
        let mut b = at(srv.port);
        let src = "var out = []; var h = {'Host':'evil.test', 'Content-Length':'1', 'Cookie':'sid=1', 'Cookie2':'x', 'Origin':'https://evil.test', 'Referer':'https://evil.test/', 'Connection':'close', 'Transfer-Encoding':'chunked', 'Accept-Encoding':'gzip', 'Upgrade':'x', 'TE':'trailers', 'Sec-Fetch-Mode':'x', 'sec-foo':'1', 'Proxy-Authorization':'p', 'Proxy-X':'1', 'Via':'1', 'Date':'d', 'Set-Cookie':'s=1', 'X-Ok':'yes', 'Authorization':'Bearer t'};
                   var req = new Request('/p', {method:'POST', body:'abc', headers:h});
                   var kept = []; for (var k in h) if (req.headers.has(k)) kept.push(k); out.push(kept.join());
                   fetch('/p', {method:'POST', body:'abc', headers:h});";
        assert_eq!(go(&mut b, src), "X-Ok,Authorization");
        let r = &srv.recs()[0];
        for forbidden in ["cookie", "cookie2", "origin", "referer", "connection", "transfer-encoding", "accept-encoding", "upgrade", "te", "sec-fetch-mode", "sec-foo", "proxy-authorization", "proxy-x", "via", "date", "set-cookie"] {
            assert!(r.header(forbidden).is_empty(), "{forbidden} reached the server: {}", r.head);
        }
        assert_eq!(r.header("host"), vec![format!("127.0.0.1:{}", srv.port)]);
        assert_eq!(r.header("content-length"), vec!["3".to_string()]);
        assert_eq!((r.header("x-ok"), r.header("authorization")), (vec!["yes".to_string()], vec!["Bearer t".to_string()]));
        assert!(!r.head.contains("evil.test") && !r.head.contains("sid=1"), "{}", r.head);
    });
}

#[test]
fn headers_follow_the_standard() {
    big(|| {
        let mut b = at(1);
        let r = js(&mut b, "var h = new Headers({'B-Name':' v1 ', 'a-name':'x'}); h.append('b-name','v2'); h.append('A-NAME','y'); h.set('C','1'); h.set('c','2'); h.set('d','3'); h.delete('D'); var seen = []; h.forEach(function (v, k, o) { seen.push(k + '=' + v + (o === h)); });
                           [h.get('b-NAME'), h.get('a-name'), h.get('c'), String(h.get('zz')), h.has('A-Name'), h.has('d'), seen.join(';')].join('|')");
        assert_eq!(r, "v1, v2|x, y|2|null|true|false|a-name=x, ytrue;b-name=v1, v2true;c=2true");
        assert_eq!(js(&mut b, "new Headers([['x','1'],['X','2']]).get('x')"), "1, 2");
        assert_eq!(js(&mut b, "new Headers(new Headers({a:'1'})).get('a')"), "1");
        assert_eq!(js(&mut b, "new Headers().append('bad name', 'x')"), "throw:TypeError:Invalid header name");
        assert_eq!(js(&mut b, "new Headers().append('x', 'a\\nb')"), "throw:TypeError:Invalid header value");
        assert_eq!(js(&mut b, "new Headers().append('x', '\\u0100')"), "throw:TypeError:Invalid header value");
        assert!(js(&mut b, "new Headers().append('x', '\\u00e9')").starts_with("unsupported:non-ASCII"));
        assert_eq!(js(&mut b, "new Headers([['a']])"), "throw:TypeError:Invalid header pair: a header is [name, value]");
        assert_eq!(js(&mut b, "new Headers('abc')"), "throw:TypeError:Headers init is not an object or a sequence");
        assert_eq!(js(&mut b, "new Headers(null)"), "throw:TypeError:Failed to construct 'Headers': The provided value is not of type 'HeadersInit'");
        assert_eq!(js(&mut b, "Headers()"), "throw:TypeError:Failed to construct: please use the 'new' operator");
        assert_eq!(js(&mut b, "new Headers().getSetCookie().length"), "0");
        assert_eq!(js(&mut b, "JSON.stringify([...new Headers([['B', '2'], ['a', '1'], ['A', '3']])])"), r#"[["a","1, 3"],["b","2"]]"#);
        assert_eq!(js(&mut b, "Headers.prototype.get.call({}, 'a')"), "throw:TypeError:Illegal invocation");
        assert_eq!(js(&mut b, "var h = new Headers({a:'1'}); h.get('a') === h.get('a') && h instanceof Headers && Object.prototype.toString.call(h)"), "[object Object]");
    });
}

#[test]
fn response_metadata_redirects_and_cookies() {
    big(|| {
        let srv = serve(|r| match r.path.as_str() {
            "/api/login" => reply(302, &[("Location", "/api/done?x=1#frag")], b""),
            "/api/done?x=1" => reply(200, &[("Content-Type", "text/plain; charset=utf-8"), ("Set-Cookie", "sid=secret; HttpOnly"), ("Set-Cookie2", "x"), ("X-Multi", "a"), ("X-Multi", "b")], "done é".as_bytes()),
            "/gone" => reply(404, &[], b"nope"),
            "/empty" => reply(204, &[], b""),
            _ => reply(500, &[], b""),
        });
        let mut b = at(srv.port);
        let src = "var out = []; fetch('/api/login', {method:'POST', body:'{}'}).then(function (r) {
            out.push(r.status, r.ok, r.redirected, r.url, r.type, JSON.stringify(r.statusText), r.headers.get('content-type'), r.headers.get('x-multi'), String(r.headers.get('set-cookie')), r.headers.has('set-cookie2'), r.bodyUsed);
            try { r.headers.set('a', 'b'); } catch (e) { out.push(e.name); }
            try { r.headers.delete('x-multi'); } catch (e) { out.push(e.name); }
            return r.text(); }).then(function (t) { out.push(t); });
          fetch('/gone').then(function (r) { out.push(r.status, r.ok, r.redirected, r.url.slice(r.url.indexOf('/gone'))); return r.text(); }).then(function (t) { out.push(t); });
          fetch('/empty').then(function (r) { out.push(r.status, String(r.body), r.bodyUsed); return r.text(); }).then(function (t) { out.push('[' + t + ']'); });";
        let r = go(&mut b, src);
        let url = format!("http://127.0.0.1:{}/api/done?x=1", srv.port);
        assert_eq!(r, format!("200|true|true|{url}|basic|\"\"|text/plain; charset=utf-8|a, b|null|false|false|TypeError|TypeError|done é|404|false|false|/gone|nope|204|null|false|[]"));
        let recs = srv.recs();
        // The POST turned into a GET without the body when it followed the 302.
        let second = recs.iter().find(|r| r.path == "/api/done?x=1").unwrap();
        assert_eq!((second.method.as_str(), second.body.len()), ("GET", 0));
        assert!(second.header("content-type").is_empty() && second.header("content-length").is_empty());
    });
}

#[test]
fn a_307_keeps_post_and_body_on_the_same_origin() {
    big(|| {
        let srv = serve(|r| if r.path == "/old" { reply(307, &[("Location", "/new")], b"") } else { ok("{}") });
        let mut b = at(srv.port);
        go(&mut b, "var out = []; fetch('/old', {method:'POST', body:'keep', headers:{'Content-Type':'text/x'}});");
        let recs = srv.recs();
        assert_eq!(recs.len(), 2);
        assert_eq!((recs[1].method.as_str(), recs[1].path.as_str(), recs[1].body.as_slice()), ("POST", "/new", b"keep".as_slice()));
    });
}

#[test]
fn body_is_consumed_once_and_clones_share_safely() {
    big(|| {
        let srv = serve(|_| reply(200, &[("Content-Type", "application/json")], br#"{"k":"v"}"#));
        let mut b = at(srv.port);
        let src = "var out = []; fetch('/x').then(function (r) {
            var c = r.clone(); out.push(r.bodyUsed, c.bodyUsed);
            return r.json().then(function (j) {
              out.push(j.k, r.bodyUsed, c.bodyUsed);
              try { r.clone(); } catch (e) { out.push('clone:' + e.name); }
              return r.text().then(function () { out.push('no'); }, function (e) { out.push('again:' + e.name + ':' + e.message);
                return c.arrayBuffer(); }); })
              .then(function (ab) { out.push(ab.byteLength, new Uint8Array(ab)[0], c.bodyUsed); return c.text(); })
              .then(function () { out.push('no'); }, function (e) { out.push('c-again:' + e.name); }); });";
        assert_eq!(go(&mut b, src), "false|false|v|true|false|clone:TypeError|again:TypeError:Body has already been consumed.|9|123|true|c-again:TypeError");
        assert_eq!(js(&mut b, "var rr = new Response('abc'); var x = rr.clone(); rr.text(); out.length = 0; rr.bodyUsed + ',' + x.bodyUsed"), "true,false");
    });
}

#[test]
fn response_constructor_request_and_clone() {
    big(|| {
        let mut b = at(1);
        assert_eq!(js(&mut b, "var r = new Response('hi', {status: 201, statusText: 'Made', headers: {'X-A': 'b', 'Set-Cookie': 'no'}}); [r.status, r.ok, r.statusText, r.type, r.url === '', r.redirected, r.headers.get('x-a'), r.headers.get('content-type'), r.headers.has('set-cookie')].join()"), "201,true,Made,default,true,false,b,text/plain;charset=UTF-8,false");
        assert_eq!(js(&mut b, "new Response(null, {status: 204}).body"), "null");
        assert_eq!(js(&mut b, "new Response('x', {status: 204})"), "throw:TypeError:Response with null body status cannot have body");
        assert_eq!(js(&mut b, "new Response('x', {status: 99})"), "throw:RangeError:init[\"status\"] must be in the range of 200 to 599, inclusive.");
        let r = js(&mut b, "var q = new Request('/api/x?y=1#f', {method:'post', body:'b', headers:{'A':'1'}}); var q2 = q.clone(); [q.method, q.url.slice(q.url.indexOf('/api')), q.headers.get('a'), q.credentials, q.mode, q.redirect, q.bodyUsed, q2.method, q2.headers.get('content-type')].join()");
        assert_eq!(r, "POST,/api/x?y=1,1,same-origin,cors,follow,false,POST,text/plain;charset=UTF-8");
        assert_eq!(js(&mut b, "new Request('/x', {method:'GET', body:'b'})"), "throw:TypeError:Request with GET/HEAD method cannot have body.");
        assert_eq!(js(&mut b, "new Request('/x', {method:'TRACE'})"), "throw:TypeError:'TRACE' HTTP method is unsupported");
        assert_eq!(js(&mut b, "new Request()"), "throw:TypeError:Failed to construct 'Request': 1 argument required, but only 0 present.");
        assert_eq!(js(&mut b, "new Request('http://')"), "throw:TypeError:Failed to parse URL");
        // Request body moves when a new request is made from it.
        assert_eq!(js(&mut b, "var a = new Request('/x', {method:'POST', body:'z'}); var c = new Request(a); [a.bodyUsed, c.bodyUsed].join()"), "true,false");
        assert_eq!(js(&mut b, "new Request(a)"), "throw:TypeError:Cannot construct a Request with a Request object that has already been used");
        // Gates.
        for (src, what) in [
            ("new Request('/x', {method:'FOO'}); fetch(new Request('/x', {method:'FOO'}))", "fetch method other than GET, POST, PUT, PATCH, DELETE, HEAD and OPTIONS"),
            ("fetch('/x', {method:'patch'})", "fetch method other than GET, POST, PUT, PATCH, DELETE, HEAD and OPTIONS"),
            ("new Request('/x', {redirect:'manual'})", "fetch redirect: manual (opaque redirect responses)"),
            ("new Request('/x', {integrity:'sha256-x'})", "fetch option integrity"),
            ("new Request('/x').body", "Request.body (a request body stream)"),
            ("new Response('x').body.tee()", "ReadableStream tee and async iteration"),
            ("new Response('x').body.getReader().closed", "ReadableStreamDefaultReader.closed"),
            ("new Response('x').body.getReader({mode: 'byob'})", "ReadableStream BYOB reader"),
        ] {
            // Stream gates are fatal for that layer. Check each independent gate in its own realm.
            let mut gate_browser = at(1);
            let r = js(&mut gate_browser, src);
            assert_eq!(r, format!("unsupported:{what}"), "{src}");
        }
        assert_eq!(js(&mut b, "new Request('/x', {signal: {}})"), "throw:TypeError:Failed to construct 'Request': Failed to read the 'signal' property from 'RequestInit': Failed to convert value to 'AbortSignal'.");
        assert_eq!(js(&mut b, "new Request('/x', {body: {getReader: function () {}}, method: 'POST'})"), "throw:TypeError:Failed to construct 'Request': a ReadableStream body is not supported. bare sends a request body in one piece: pass a string, Blob, ArrayBuffer or FormData.");
    });
}

#[test]
fn the_reader_hands_out_real_bytes_in_chunks() {
    big(|| {
        let data: Vec<u8> = (0..100_000u32).map(|i| (i.wrapping_mul(31) >> 2) as u8).collect();
        let d2 = data.clone();
        let srv = serve(move |_| reply(200, &[], &d2));
        let mut b = mk(Some(format!("http://127.0.0.1:{}/", srv.port)), FetchLimits { chunk_bytes: 30_000, ..Default::default() });
        let sum: u64 = data.iter().map(|&x| x as u64).sum();
        let src = "var out = []; var total = 0, sum = 0, chunks = 0; fetch('/big').then(function (r) {
            var rd = r.body.getReader(); out.push(r.body === r.body, r.body.locked, rd.constructor === ReadableStreamDefaultReader);
            out.push(r.body.locked, r.bodyUsed);
            function pump() { return rd.read().then(function (x) {
              if (x.done) { out.push('done', x.value === undefined, total, sum, chunks, r.bodyUsed); return rd.read().then(function (y) { out.push(y.done); }); }
              chunks++; total += x.value.length; for (var i = 0; i < x.value.length; i++) sum += x.value[i]; out.push(x.value instanceof Uint8Array);
              return pump(); }); }
            return pump().then(function () { return r.text().then(function () { out.push('no'); }, function (e) { out.push(e.name); }); }); });";
        let got = go(&mut b, src);
        let parts: Vec<&str> = got.split('|').collect();
        assert_eq!(&parts[..5], ["true", "true", "true", "true", "false"], "{got}");
        assert!(got.ends_with(&format!("done|true|100000|{sum}|4|true|true|TypeError")), "{got}");
        assert_eq!(b.fetch_stats().retained_bytes, 0, "the body is freed after the last chunk");
    });
}

#[test]
fn reader_cancel_release_and_lock_rules() {
    big(|| {
        let srv = serve(|_| reply(200, &[], b"0123456789"));
        let mut b = at(srv.port);
        let src = "var out = []; fetch('/a').then(function (r) {
            var s = r.body, rd = s.getReader();
            try { s.getReader(); } catch (e) { out.push('lock:' + e.name); }
            return s.cancel().then(function () { out.push('no'); }, function (e) { out.push('cancel-locked:' + e.name); })
              .then(function () { return rd.read(); }).then(function (x) { out.push(x.done, x.value.length); rd.releaseLock(); out.push(s.locked);
                return rd.read().then(function () { out.push('no'); }, function (e) { out.push('released:' + e.name); }); })
              .then(function () { var rd2 = s.getReader(); return rd2.cancel().then(function () { return rd2.read(); }); })
              .then(function (y) { out.push(y.done, r.bodyUsed); return r.text().then(function () { out.push('no'); }, function (e) { out.push('used:' + e.name); }); }); });
          fetch('/b').then(function (r) { return r.body.cancel().then(function () { out.push('c', r.bodyUsed); return r.json(); }).then(function () { out.push('no'); }, function (e) { out.push('after-cancel:' + e.name); }); });";
        assert_eq!(go(&mut b, src), "lock:TypeError|cancel-locked:TypeError|false|10|false|released:TypeError|true|true|used:TypeError|c|true|after-cancel:TypeError");
        assert_eq!(b.fetch_stats().retained_bytes, 0);
    });
}

#[test]
fn array_buffer_text_json_decoding_and_errors() {
    big(|| {
        let srv = serve(|r| match r.path.as_str() {
            "/bom" => reply(200, &[], b"\xEF\xBB\xBFhi"),
            "/bad" => reply(200, &[], b"{\"a\":"),
            "/utf" => reply(200, &[], b"a\xFFb"),
            _ => reply(200, &[], b"\x00\x01\x02"),
        });
        let mut b = at(srv.port);
        let src = "var out = []; fetch('/bom').then(function (r) { return r.text(); }).then(function (t) { out.push(t.length, t); });
          fetch('/bad').then(function (r) { return r.json(); }).then(function () { out.push('no'); }, function (e) { out.push(e.name); });
          fetch('/utf').then(function (r) { return r.text(); }).then(function (t) { out.push(t.length, t.charCodeAt(1)); });
          fetch('/bin').then(function (r) { return r.arrayBuffer(); }).then(function (ab) { out.push(ab instanceof ArrayBuffer, ab.byteLength, new Uint8Array(ab)[2]); });";
        assert_eq!(go(&mut b, src), "2|hi|SyntaxError|3|65533|true|3|2");
    });
}

#[test]
fn json_uses_the_intrinsic_parse() {
    big(|| {
        let srv = serve(|_| ok("[1]"));
        let mut b = at(srv.port);
        assert_eq!(go(&mut b, "var out = []; var real = JSON.parse; JSON.parse = function () { return 'hijack'; }; fetch('/j').then(function (r) { return r.json(); }).then(function (j) { JSON.parse = real; out.push(Array.isArray(j), j[0]); });"), "true|1");
    });
}

#[test]
fn a_gzip_body_is_decoded_and_its_headers_stay() {
    big(|| {
        let srv = serve(|r| match r.path.as_str() {
            "/gz" => reply(200, &[("Content-Type", "application/json"), ("Content-Encoding", "gzip")], b"\x1f\x8b\x08\x00\x00\x00\x00\x00\x02\xff\xab\x56\x4a\xaf\x52\xb2\x52\x4a\x49\x4d\xce\x4f\x49\x4d\x51\xaa\x05\x00\xc6\x8d\x73\x29\x10\x00\x00\x00"),
            _ => reply(200, &[("Content-Encoding", "gzip")], b"not gzip"),
        });
        let mut b = at(srv.port);
        let src = "var out = []; fetch('/gz').then(function (r) { out.push(r.headers.get('content-encoding'), r.headers.get('content-length')); return r.json(); }).then(function (j) { out.push(j.gz); }); fetch('/bad').then(function (r) { out.push('ok'); }, function (e) { out.push(e.name); });";
        assert_eq!(go(&mut b, src), "gzip|36|decoded|TypeError");
    });
}

#[test]
fn failures_are_type_errors_with_fixed_text_and_clean_diagnostics() {
    big(|| {
        let srv = serve(|r| match r.path.as_str() {
            p if p.starts_with("/truncated") => Reply { raw: b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\nshort".to_vec(), close: true },
            p if p.starts_with("/garbage") => Reply { raw: b"not http\r\n\r\n".to_vec(), close: true },
            p if p.starts_with("/gzip") => reply(200, &[("Content-Encoding", "zstd")], b"\x1f\x8b"),
            p if p.starts_with("/loop") => reply(302, &[("Location", "/loop?token=SECRET-QUERY")], b""),
            p if p.starts_with("/chunk") => Reply { raw: b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n".to_vec(), close: true },
            p if p.starts_with("/err") => reply(302, &[("Location", "/ok")], b""),
            _ => ok("{}"),
        });
        let mut b = at(srv.port);
        let one = |p: &str, extra: &str| format!("fetch('{p}?token=SECRET-QUERY', {{headers:{{Authorization:'Bearer SECRET-HEADER'}}{extra}}}).then(function (r) {{ out.push('ok' + r.status); }}, function (e) {{ out.push(e.name + ':' + e.message); }});");
        let src = format!("var out = []; {} {} {} {} {} {}", one("/truncated", ""), one("/garbage", ""), one("/gzip", ""), one("/loop", ""), one("/chunk", ""), one("/err", ", redirect:'error'"));
        let got = go(&mut b, &src);
        assert_eq!(got, ["TypeError:Failed to fetch"; 6].join("|"));
        assert!(!got.contains("SECRET"));
        let diag = b.take_fetch_diagnostics().join("\n");
        assert!(!diag.contains("SECRET") && !diag.contains("token") && !diag.contains("/loop"), "{diag}");
        assert!(diag.contains("redirect refused (redirect: error)") && diag.contains("too many redirects") && diag.contains("compressed response") && diag.contains("network error"), "{diag}");
        // The loop followed 20 redirects and no more.
        assert_eq!(srv.recs().iter().filter(|r| r.path.starts_with("/loop")).count(), 21);
    });
}

#[test]
fn a_refused_cross_origin_request_leaks_no_secret_into_the_diagnostics_or_the_console() {
    big(|| {
        let a = serve(|_| ok("{}"));
        let other = serve(|_| ok("{}"));
        let mut b = at(a.port);
        let r = js(&mut b, &format!("fetch('http://127.0.0.1:{}/x?secret=1', {{method:'POST', body:'pw=hunter2', headers:{{Authorization:'Bearer t'}}}})", other.port));
        assert_eq!(r, "object");
        b.drain().unwrap();
        while b.pending_fetches() > 0 {
            b.run_network_tasks(10).unwrap();
        }
        // The `Authorization` header needs a preflight, and the preflight has no allow header: the request itself never leaves.
        assert_eq!(other.recs().iter().map(|r| r.method.as_str()).collect::<Vec<_>>(), ["OPTIONS"]);
        assert!(other.recs()[0].body.is_empty() && other.recs()[0].header("authorization").is_empty());
        let recs = b.take_console_records();
        let mut log = b.take_fetch_diagnostics().join("\n");
        for r in &recs {
            log += &b.format_console_record(r).unwrap();
        }
        assert!(log.contains("CORS") && !log.contains("secret") && !log.contains("hunter2") && !log.contains("/x"), "{log}");
        assert_eq!(js(&mut b, "fetch('ftp://x/y').then(null, function (e) { out2 = e.name; }); typeof out2"), "undefined");
        b.drain().unwrap();
        assert_eq!(js(&mut b, "out2"), "TypeError");
        assert_eq!(b.pending_fetches(), 0);
        assert_eq!(a.conns.load(SeqCst), 0);
    });
}

#[test]
fn reused_connections_and_posts_are_never_resent() {
    big(|| {
        let srv = serve(|r| if r.path == "/close-after" { Reply { raw: reply(200, &[], b"x").raw, close: true } } else { ok("{}") });
        let mut b = at(srv.port);
        go(&mut b, "var out = []; fetch('/one').then(function () { return fetch('/two'); }).then(function () { return fetch('/three', {method:'POST', body:'p'}); });");
        assert_eq!(srv.conns.load(SeqCst), 1, "keep-alive reuses the connection");
        assert_eq!(srv.recs().len(), 3);
        // The server closes an idle connection (without a Connection header). A POST must open a new one and arrive once.
        let mut b = at(srv.port);
        let before = srv.recs().len();
        go(&mut b, "var out = []; fetch('/close-after');");
        // Wait for the close itself, not for a time: the client must see the FIN before it picks the connection.
        while srv.closed.load(SeqCst) == 0 {
            std::thread::yield_now();
        }
        go(&mut b, "fetch('/after', {method:'POST', body:'once'}).then(function (r) { out.push(r.status); });");
        let posts = srv.recs()[before..].iter().filter(|r| r.method == "POST").count();
        assert_eq!(posts, 1, "{:?}", srv.recs());
        assert_eq!(js(&mut b, "out.join()"), "200");
    });
}

#[test]
fn update_fetch_base_url_moves_the_origin_for_new_requests_only() {
    big(|| {
        let (a, c) = (serve(|_| ok("\"a\"")), serve(|_| ok("\"c\"")));
        let mut b = at(a.port);
        js(&mut b, "var out = []; fetch('/q').then(function (r) { return r.json(); }).then(function (j) { out.push(j); });");
        b.update_fetch_base_url(&format!("http://127.0.0.1:{}/other/page", c.port)).unwrap();
        // The queued request keeps the origin it was made for.
        js(&mut b, "fetch('/q', {}).then(function (r) { return r.json(); }).then(function (j) { out.push(j); });");
        b.drain().unwrap();
        b.run_network_tasks(10).unwrap();
        assert_eq!(js(&mut b, "out.join()"), "a,c");
        assert_eq!((a.recs().len(), c.recs().len()), (1, 1));
        assert!(b.update_fetch_base_url("not a url").is_err());
    });
}

#[test]
fn aborting_queued_requests_rejects_them_and_sends_nothing() {
    big(|| {
        let srv = serve(|_| ok("{}"));
        let mut b = at(srv.port);
        js(&mut b, "var out = []; for (var i = 0; i < 3; i++) fetch('/x' + i, {method:'POST', body:'abc'}).then(function () { out.push('ok'); }, function (e) { out.push(e.name); });");
        b.drain().unwrap();
        assert_eq!(b.abort_fetches().unwrap(), 3);
        assert_eq!(js(&mut b, "out.join()"), "AbortError,AbortError,AbortError");
        assert_eq!((b.pending_fetches(), b.fetch_stats().queued_bytes, b.run_network_tasks(5).unwrap()), (0, 0, 0));
        assert_eq!(srv.conns.load(SeqCst), 0);
    });
}

#[test]
fn resource_caps_are_limits_before_growth() {
    big(|| {
        let srv = serve(|r| if r.path == "/big" { reply(200, &[], &vec![b'a'; 5000]) } else { ok("{}") });
        let base = Some(format!("http://127.0.0.1:{}/", srv.port));
        let l = FetchLimits { max_pending: 2, max_request_body: 100, max_response_body: 4000, max_retained: 300, max_url_bytes: 64, max_started: 6, max_headers: 3, max_objects: 40, ..Default::default() };
        let mut b = mk(base.clone(), l);
        assert_eq!(js(&mut b, "fetch('/a'); fetch('/b'); 1"), "1");
        assert_eq!(js(&mut b, "fetch('/c')"), lim("fetch-queue"));
        b.run_network_tasks(10).unwrap();
        assert_eq!(js(&mut b, &format!("fetch('/{}')", "x".repeat(100))), lim("fetch-url"));
        assert_eq!(js(&mut b, &format!("fetch('/p', {{method:'POST', body:'{}'}})", "x".repeat(101))), lim("fetch-request-body"));
        // Aggregate: 3 queued bodies of 100 bytes are above 300 with the held ones.
        let mut b2 = mk(base.clone(), FetchLimits { max_pending: 8, ..l });
        assert_eq!(js(&mut b2, &format!("var body = '{}'; fetch('/p', {{method:'POST', body}}); fetch('/p', {{method:'POST', body}}); fetch('/p', {{method:'POST', body}}); 1", "y".repeat(100))), "1");
        assert_eq!(js(&mut b2, "fetch('/p', {method:'POST', body})"), lim("fetch-retained"));
        assert_eq!(b2.fetch_stats().queued_bytes, 300);
        // Headers.
        assert_eq!(js(&mut b, "var h = new Headers(); h.append('a','1'); h.append('b','2'); h.append('c','3'); h.append('d','4')"), lim("fetch-headers"));
        let mut bh = mk(None, FetchLimits { max_header_bytes: 40, ..Default::default() });
        assert_eq!(js(&mut bh, &format!("new Headers({{a:'{}'}})", "v".repeat(41))), lim("fetch-header-bytes"));
        // Response bytes: the cap fails before the body is read into the heap, and no JS runs afterwards.
        let mut b3 = mk(base.clone(), l);
        js(&mut b3, "var out = []; fetch('/big').then(function () { out.push('then'); }, function () { out.push('catch'); });");
        assert_eq!(show_err(b3.run_network_tasks(1)), lim("fetch-response-body"));
        assert_eq!(js(&mut b3, "out.length"), "0");
        // Total requests.
        let mut b4 = mk(base, l);
        for _ in 0..3 {
            js(&mut b4, "fetch('/a'); fetch('/a')");
            b4.run_network_tasks(10).unwrap();
        }
        assert_eq!(js(&mut b4, "fetch('/a')"), lim("fetch-requests"));
        // The queue is bounded by the text of its URLs and headers once it holds 64 requests (gitlab queues 64 at once).
        let mut b5 = mk(Some(format!("http://127.0.0.1:{}/", srv.port)), FetchLimits { max_pending_bytes: 2000, ..Default::default() });
        assert_eq!(js(&mut b5, "for (var i = 0; i < 64; i++) fetch('/a'); 1"), "1");
        assert_eq!(js(&mut b5, &format!("fetch('/{}')", "x".repeat(3000))), lim("fetch-queue"));
    });
}

fn show_err<T: std::fmt::Debug>(r: Result<T, Error>) -> String {
    match r {
        Err(Error::Limit(l)) => format!("limit:{l:?}"),
        other => format!("not-a-limit:{other:?}"),
    }
}

#[test]
fn retained_and_object_caps_hold_for_responses() {
    big(|| {
        let srv = serve(|_| reply(200, &[], &[b'z'; 200]));
        let l = FetchLimits { max_retained: 500, max_objects: 20, max_readers: 2, ..Default::default() };
        let mut b = mk(Some(format!("http://127.0.0.1:{}/", srv.port)), l);
        // Three 200 byte responses held by JS exceed 500 retained bytes: the third settle is a Limit.
        js(&mut b, "var held = []; for (var i = 0; i < 3; i++) fetch('/r').then(function (r) { held.push(r); });");
        let r = b.run_network_tasks(10);
        assert_eq!(show_err(r), lim("fetch-retained"));
        assert_eq!(b.fetch_stats().retained_bytes, 400);
        // Objects: Response + Headers per response.
        let mut b = mk(Some(format!("http://127.0.0.1:{}/", srv.port)), l);
        assert_eq!(js(&mut b, "var x = []; for (var i = 0; i < 9; i++) x.push(new Headers()); 1"), "1");
        assert_eq!(js(&mut b, "for (var i = 0; i < 20; i++) x.push(new Headers()); 1"), lim("fetch-objects"));
        // Readers: the cap counts live reader objects.
        let mut b = mk(Some(format!("http://127.0.0.1:{}/", srv.port)), l);
        assert_eq!(js(&mut b, "var r = new Response('x'); var s = r.body; s.getReader().releaseLock(); s.getReader().releaseLock(); s.getReader()"), lim("fetch-readers"));
    });
}

#[test]
fn collection_while_pending_and_after_consumption_and_generation_reuse() {
    big(|| {
        let srv = serve(|_| ok("{\"v\":7}"));
        let mut b = at(srv.port);
        js(&mut b, "var out = []; (function () { for (var i = 0; i < 20; i++) fetch('/p' + i, {method:'POST', body:'abcd'}).then(function (r) { return r.json(); }).then(function (j) { out.push(j.v); }); })();");
        b.drain().unwrap();
        // The promises are held only by the queue. A collection must keep them and their reactions.
        let before = b.collect(&[]).unwrap();
        let _ = before;
        assert_eq!(b.fetch_stats().pending, 20);
        b.run_network_tasks(100).unwrap();
        assert_eq!(js(&mut b, "var allv = true; for (var i = 0; i < out.length; i++) if (out[i] !== 7) allv = false; out.length + ':' + allv"), "20:true");
        // After consumption nothing holds the objects: a collection prunes every entry.
        b.collect(&[]).unwrap();
        let s = b.fetch_stats();
        assert_eq!((s.objects, s.retained_bytes, s.readers, s.queued_bytes), (0, 0, 0, 0), "{s:?}");
        // Make many objects in rounds: slots are reused (generations differ) and the totals return to zero.
        for round in 0..30 {
            js(&mut b, "(function () { for (var i = 0; i < 100; i++) { var r = new Response('abc'); var rd = r.body.getReader(); r.headers.get('x'); new Request('/x').clone(); } })()");
            let live = b.fetch_stats().objects;
            assert!(live >= 100, "round {round}: {live}");
            b.collect(&[]).unwrap();
            assert_eq!(b.fetch_stats().objects, 0, "round {round}");
        }
        // A response and its stream and reader stay alive together while JS holds only the reader.
        js(&mut b, "var keep = new Response('abcdef').body.getReader();");
        b.collect(&[]).unwrap();
        assert_eq!(b.fetch_stats().objects, 4, "response, headers, stream, reader");
        assert_eq!(js(&mut b, "var rr; keep.read().then(function (x) { rr = x.value.length; }); 1"), "1");
        b.drain().unwrap();
        assert_eq!(js(&mut b, "rr"), "6");
    });
}

#[test]
fn a_poisoned_object_does_not_run_js_while_the_host_holds_a_borrow() {
    big(|| {
        // Getters and toString run during argument conversion. They may call back into fetch classes freely.
        let srv = serve(|_| ok("{}"));
        let mut b = at(srv.port);
        let src = "var out = []; var h = new Headers(); var evil = {toString: function () { h.append('z', '1'); return 'k'; }};
          h.append(evil, {toString: function () { h.set('y', '2'); return 'v'; }});
          var init = {}; Object.defineProperty(init, 'headers', {enumerable: true, get: function () { out.push('getter'); fetch('/inner'); return {'A': '1'}; }});
          var q = new Request('/outer', init);
          h.forEach(function (v, k) { h.append('w' + k, v); out.push(k); });
          out.push(q.headers.get('a'));";
        assert_eq!(go(&mut b, src), "getter|k|y|z|1");
        assert_eq!(srv.recs().len(), 1);
    });
}

#[test]
fn run_network_tasks_runs_at_most_max_and_leaves_the_rest_queued() {
    big(|| {
        let srv = serve(|_| ok("{}"));
        let mut b = at(srv.port);
        js(&mut b, "var out = []; fetch('/a').then(function () { out.push('a'); }); fetch('/b').then(function () { out.push('b'); });");
        let n = b.run_network_tasks(1).unwrap();
        assert_eq!(n, 1);
        assert_eq!(js(&mut b, "out.join()"), "a");
        assert_eq!(b.pending_fetches(), 1);
    });
}

#[test]
fn numeric_header_names_are_not_discarded() {
    big(|| {
        let mut b = mk(None, FetchLimits::default());
        assert_eq!(js(&mut b, "new Headers({'123':'value'}).get('123')"), "value");
    });
}

#[test]
fn network_pump_checks_failed_jobs_before_io() {
    big(|| {
        let srv = serve(|_| ok("{}"));
        let mut b = Browser::new(tree::parse("<body></body>"), BrowserLimits::default(), EvalLimits { max_steps: 200, ..Default::default() }, HeapLimits::default()).unwrap();
        b.configure_fetch(FetchConfig { base_url: Some(format!("http://127.0.0.1:{}/", srv.port)), client: Client::new(), limits: FetchLimits::default() }).unwrap();
        js(&mut b, "fetch('/x'); queueMicrotask(function(){while(true){}});");
        assert!(matches!(b.drain(), Err(Error::Limit(LimitKind::Steps))));
        assert!(matches!(b.run_network_tasks(1), Err(Error::Limit(LimitKind::Steps))));
        assert_eq!(srv.conns.load(SeqCst), 0);
        assert_eq!(b.pending_fetches(), 1);
    });
}

#[test]
fn zero_fetch_diagnostic_capacity_retains_nothing() {
    big(|| {
        let mut b = mk(None, FetchLimits { max_diag: 0, ..Default::default() });
        js(&mut b, "fetch('http://example.invalid/')");
        assert!(b.take_fetch_diagnostics().is_empty());
        assert_eq!(b.fetch_stats().dropped_diagnostics, 1);
    });
}

fn plain_view(b: &mut Browser, len: usize) {
    let buf = b.it.heap.new_buffer(None, len).unwrap();
    let op = b.it.object_prototype();
    let v = b.it.heap.new_view(Some(op), buf, 0, len).unwrap();
    let k = b.it.heap.key_from_str("pv").unwrap();
    let g = b.it.global_object();
    b.it.define_data(g, k, Value::Object(v), true, true, true).unwrap();
}

#[test]
fn headers_iterate_through_the_shared_engine() {
    big(|| {
        let mut b = at(1);
        // the realm's own symbol, the same function as `entries`, an iterator that is itself iterable
        assert_eq!(js(&mut b, "var S = Symbol.iterator, P = Headers.prototype, d = Object.getOwnPropertyDescriptor(P, S); [d.value === P.entries, d.writable, d.enumerable, d.configurable, P.entries.name, P.keys.length].join()"), "true,true,false,true,entries,0");
        assert_eq!(js(&mut b, "Symbol = undefined; var h = new Headers([['b', '2'], ['A', '1'], ['a', '3']]); [JSON.stringify([...h]), JSON.stringify([...new Headers(new Map([['x', 'y']]))]), h[S] === P.entries].join(' ')"), r#"[["a","1, 3"],["b","2"]] [["x","y"]] true"#);
        b.collect(&[]).unwrap();
        assert_eq!(js(&mut b, "var it = h.keys(); [it[S]() === it, Object.getPrototypeOf(Object.getPrototypeOf(it)) === Object.getPrototypeOf(Object.getPrototypeOf([][S]())), [...it].join(), JSON.stringify(h.values().next())].join()"), r#"true,true,a,b,{"value":"1, 3","done":false}"#);
        // live: the index goes on in the list sorted again at each step
        assert_eq!(js(&mut b, "var out = []; for (var [k] of h) { out.push(k); if (k === 'a') { h.append('aa', '0'); h.delete('b'); } } out.join()"), "a,aa");
        // forbidden and immutable guards are not bypassed by an iterable
        assert_eq!(js(&mut b, "JSON.stringify([...new Request('/x', {headers: new Map([['Cookie', 'a'], ['Host', 'h'], ['X-A', 'b'], ['sec-fetch-mode', 'x']])}).headers])"), r#"[["x-a","b"]]"#);
        assert_eq!(js(&mut b, "var rh = new Response('x', {headers: new Map([['Set-Cookie', 'a'], ['B', '1']])}).headers; JSON.stringify([...rh])"), r#"[["b","1"],["content-type","text/plain;charset=UTF-8"]]"#);
        // pair rules
        assert_eq!(js(&mut b, "new Headers([['a', 'b', 'c']])"), "throw:TypeError:Invalid header pair: a header is [name, value]");
        assert_eq!(js(&mut b, "new Headers([{0: 'a', 1: 'b', length: 2}])"), "throw:TypeError:Invalid header pair: a header is [name, value]");
        assert_eq!(js(&mut b, "new Headers(['ab'])"), "throw:TypeError:Invalid header pair");
        assert_eq!(js(&mut b, "new Headers({[S]: 1})"), "throw:TypeError:Symbol.iterator is not a function");
        assert_eq!(js(&mut b, "var o = {k: 'v'}; Object.defineProperty(o, S, {value: undefined}); JSON.stringify([...new Headers(o)])"), r#"[["k","v"]]"#, "a hidden symbol key is skipped");
        assert_eq!(js(&mut b, "var o2 = {k: 'v'}; o2[S] = undefined; new Headers(o2)"), "throw:TypeError:Cannot convert a Symbol value to a string");
        assert_eq!(js(&mut b, "new Headers(new Map([['bad name', 'x']]))"), "throw:TypeError:Invalid header name");
        assert!(js(&mut b, "new Headers(new Map([['x', '\\u00e9']]))").starts_with("unsupported:non-ASCII"));
        // WebIDL sequence conversion does not close iterators on errors.
        assert_eq!(js(&mut b, "var closed = 0, inner = 0; var pair = {[S]() { var n = 0; return {next() { return {done: false, value: 'v' + n++}; }, return() { inner++; return {}; }}; }}; try { new Headers({[S]() { return {next() { return {done: false, value: pair}; }, return() { closed++; return {}; }}; }}) } catch (e) { e.name + closed + inner }"), "TypeError00");
        assert_eq!(js(&mut b, "closed = 0; try { new Headers({[S]() { return {next() { return {done: false, value: 'x'}; }, return() { closed++; return {}; }}; }}) } catch (e) { e.name + closed }"), "TypeError0");
        // brand checks
        assert_eq!(js(&mut b, "Headers.prototype.entries.call({})"), "throw:TypeError:Illegal invocation");
        assert_eq!(js(&mut b, "Object.getPrototypeOf(new Headers().keys()).next.call({})"), "throw:TypeError:Illegal invocation");
        assert_eq!(js(&mut b, "Object.getPrototypeOf(new Headers().keys()).next.call(new Headers())"), "throw:TypeError:Illegal invocation");
    });
}

#[test]
fn headers_init_caps_are_limits_before_growth_and_stop_all_js() {
    big(|| {
        let mut b = mk(None, FetchLimits { max_headers: 3, max_header_bytes: 60, ..Default::default() });
        assert_eq!(js(&mut b, "var closed = 0, n = 0; new Headers({[Symbol.iterator]() { return {next() { n++; return {done: false, value: ['a' + n, 'b']}; }, return() { closed++; return {}; }}; }})"), lim("fetch-headers"));
        assert_eq!(js(&mut b, "closed + ',' + n"), "0,4", "the cap is checked before the fourth pair is read");
        assert_eq!(js(&mut b, "new Headers(new Map([['a', 'x'.repeat ? 'x'.repeat(70) : new Array(71).join('x')]]))"), lim("fetch-header-bytes"));
        assert_eq!(js(&mut b, "new Headers([['a', '1'], ['b', '2'], ['c', '3']]).get('c')"), "3");
        assert_eq!(js(&mut b, "new Headers([['a', '1'], ['b', '2'], ['c', '3'], ['d', '4']])"), lim("fetch-headers"));
        // a byte view lists one key for each byte: refused before the list is made
        assert_eq!(js(&mut b, "var u = new Uint8Array(1 << 26); u[Symbol.iterator] = undefined; new Headers(u)"), lim("fetch-headers"));
        plain_view(&mut b, 1 << 26);
        assert_eq!(js(&mut b, "new Headers(pv)"), lim("fetch-headers"));
        // an endless iterable ends in a limit, not in a hang
        let mut s = Browser::new(tree::parse("<body></body>"), BrowserLimits::default(), EvalLimits { max_steps: 300, ..Default::default() }, HeapLimits::default()).expect("browser");
        s.configure_fetch(FetchConfig { base_url: None, client: Client::new(), limits: FetchLimits { max_headers: 1 << 30, ..Default::default() } }).unwrap();
        let r = js(&mut s, "var closed = 0; new Headers({[Symbol.iterator]() { return {next() { return {done: false, value: ['a', 'b']}; }, return() { closed++; return {}; }}; }})");
        assert!(r == lim("fetch-header-bytes") || r == "limit:Steps", "{r}");
        assert_eq!(js(&mut s, "closed"), "0");
    });
}

#[test]
fn headers_iterators_survive_collection_and_cycles_are_freed() {
    big(|| {
        let mut b = at(1);
        // the iterator alone keeps its Headers alive
        js(&mut b, "var it = new Headers({a: '1', b: '2'}).entries(); it.next();");
        b.collect(&[]).unwrap();
        assert_eq!(b.fetch_stats().objects, 2, "headers and iterator");
        assert_eq!(js(&mut b, "JSON.stringify([it.next(), it.next()])"), r#"[{"value":["b","2"],"done":false},{"done":true}]"#);
        js(&mut b, "it = null;");
        b.collect(&[]).unwrap();
        assert_eq!(b.fetch_stats().objects, 0);
        // a request's headers iterator keeps the request's headers (and so the request) alive only as far as headers go
        for round in 0..20 {
            js(&mut b, "(function () { for (var i = 0; i < 100; i++) { var h = new Headers([['k' + i, 'v']]); var j = h[Symbol.iterator](); j.next(); var r = new Response('x'); r.headers.keys().next(); } })()");
            b.collect(&[]).unwrap();
            assert_eq!(b.fetch_stats().objects, 0, "round {round}");
        }
        // a self cycle: an iterable that holds the headers its own iterator reads
        js(&mut b, "var o = {}; o.me = o; o.h = new Headers({a: '1'}); o[Symbol.iterator] = function () { return o.h.entries(); }; var made = JSON.stringify([...new Headers(o)]);");
        assert_eq!(js(&mut b, "made"), r#"[["a","1"]]"#);
        js(&mut b, "o = null; made = null;");
        b.collect(&[]).unwrap();
        assert_eq!(b.fetch_stats().objects, 0);
        // the object cap counts iterators
        let mut c = mk(None, FetchLimits { max_objects: 5, ..Default::default() });
        assert_eq!(js(&mut c, "var h = new Headers(), its = []; for (var i = 0; i < 10; i++) its.push(h.keys()); 1"), lim("fetch-objects"));
        assert_eq!(c.fetch_stats().objects, 5);
    });
}

#[test]
fn callbacks_from_other_scripts_with_the_same_ids_do_not_mix_in_headers() {
    big(|| {
        let mut b = at(1);
        js(&mut b, "var A = {[Symbol.iterator]() { var i = 0; return {next() { return i++ < 2 ? {done: false, value: ['a' + i, '1']} : {done: true}; }}; }};");
        js(&mut b, "var B = {[Symbol.iterator]() { var i = 0; return {next() { return i++ < 2 ? {done: false, value: ['b' + i, '2']} : {done: true}; }}; }};");
        b.collect(&[]).unwrap();
        js(&mut b, "var C = {[Symbol.iterator]() { var i = 0; return {next() { return i++ < 1 ? {done: false, value: ['c', '3']} : {done: true}; }}; }};");
        assert_eq!(js(&mut b, "[B, A, C, B].map(function (x) { return JSON.stringify([...new Headers(x)]); }).join('|')"), r#"[["b1","2"],["b2","2"]]|[["a1","1"],["a2","1"]]|[["c","3"]]|[["b1","2"],["b2","2"]]"#);
    });
}

#[test]
fn headers_init_reads_the_iterator_on_existing_headers() {
    big(|| {
        let mut b = mk(None, FetchLimits::default());
        assert_eq!(js(&mut b, "var calls=0; var h=new Headers([['old','v']]); h[Symbol.iterator]=function(){ calls++; return [['new','v']][Symbol.iterator](); }; JSON.stringify([...new Headers(h)]) + ':' + calls"), "[[\"new\",\"v\"]]:1");
    });
}

#[test]
fn headers_init_converts_values_before_header_validation() {
    big(|| {
        let mut b = mk(None, FetchLimits::default());
        assert_eq!(js(&mut b, "var seen=[]; var name={toString(){seen.push('name');return 'bad name'}}, value={toString(){seen.push('value');return 'v'}}; try{new Headers([[name,value],['later',{toString(){seen.push('later');return 'v'}}]])}catch(e){seen.push(e.name)} seen.join()"), "name,value,later,TypeError");
    });
}

#[test]
fn headers_sequence_conversion_does_not_close_on_failure() {
    big(|| {
        let mut b = mk(None, FetchLimits::default());
        assert_eq!(js(&mut b, "var closed=0;try{new Headers({[Symbol.iterator](){return{next(){return{done:false,value:'x'}},return(){closed++;return{}}}}})}catch(e){e.name+':'+closed}"), "TypeError:0");
    });
}

#[test]
fn headers_set_rejects_bytes_before_changing_the_list() {
    big(|| {
        let mut b = mk(None, FetchLimits { max_header_bytes: 8, ..Default::default() });
        assert_eq!(js(&mut b, "var h = new Headers({x:'v'}); h.set('x','0123456789');"), lim("fetch-header-bytes"));
        assert_eq!(js(&mut b, "h.get('x')"), "v");
    });
}

#[test]
fn an_async_headers_walk_survives_collection_without_the_source_global() {
    big(|| {
        let mut b = mk(None, FetchLimits::default());
        assert_eq!(js(&mut b, "var h = new Headers({a:'1',b:'2',c:'3'}), seen=''; async function walk(){for(var [k,v] of h){await 0; seen+=k+v;}} walk(); h=undefined;"), "undefined");
        b.collect(&[]).unwrap();
        b.drain().unwrap();
        assert_eq!(js(&mut b, "seen"), "a1b2c3");
    });
}

const METHOD_CASES: &str = include_str!("../../../tools/fixtures/ui/methods-cases.txt");
const METHOD_EXPECTED: &str = include_str!("../../../tools/fixtures/ui/methods-expected.txt");

/// A server for the method cases. `/rNNN` answers NNN with `Location: /t`, `/empty` answers 204, and HEAD gets the length of "ok" and no body.
fn method_server() -> Srv {
    serve(|r| {
        let code = r.path.strip_prefix("/r").and_then(|c| c.parse::<u16>().ok());
        match (code, r.path.as_str()) {
            (Some(c), _) => reply(c, &[("Location", "/t")], b""),
            (_, "/empty") => reply(204, &[], b""),
            _ if r.method == "HEAD" => Reply { raw: b"HTTP/1.1 200 X\r\nContent-Type: text/plain\r\nContent-Length: 2\r\n\r\n".to_vec(), close: false },
            _ => reply(200, &[("Content-Type", "text/plain")], b"ok"),
        }
    })
}

#[test]
fn the_methods_send_what_chrome_sends() {
    big(|| {
        let srv = method_server();
        // A case that needs a token method or a lower-case `patch` on the wire: `Unsupported` here, so no comparison.
        let skip = ["patch-lower"];
        let expected: std::collections::HashMap<&str, &str> = METHOD_EXPECTED.lines().filter_map(|l| l.split_once('\t')).collect();
        let mut bad = Vec::new();
        for (name, src) in METHOD_CASES.lines().filter(|l| !l.is_empty() && !l.starts_with('#')).filter_map(|l| l.split_once('\t')) {
            if skip.contains(&name) {
                continue;
            }
            let before = srv.recs().len();
            let mut b = at(srv.port);
            let script = format!("var out = []; (function () {{ try {{ return ({src}); }} catch (e) {{ return Promise.reject(e); }} }})().then(function (v) {{ out.push('ok:' + v); }}, function (e) {{ out.push(e.name); }});");
            let r = go(&mut b, &script);
            let wire: Vec<String> = srv.recs()[before..]
                .iter()
                .map(|q| format!("{};{};{};{}", format!("{} {}", q.method, q.path).trim_end(), q.header("content-length").join(""), q.header("content-type").join(""), String::from_utf8_lossy(&q.body)))
                .collect();
            let got = format!("{r}\t{}", wire.join(" || "));
            let want = expected.get(name).expect(name);
            if got != *want {
                bad.push(format!("{name}: got {got:?}, Chrome {want:?}"));
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    });
}

#[test]
fn a_head_reply_has_no_body_and_the_next_request_on_the_connection_reads_right() {
    big(|| {
        let srv = method_server();
        let mut b = at(srv.port);
        let r = go(&mut b, "var out = []; fetch('/a', {method:'HEAD'}).then(function (r) { out.push(r.status, r.headers.get('content-length')); return r.text(); }).then(function (t) { out.push('[' + t + ']'); return fetch('/b'); }).then(function (r) { return r.text(); }).then(function (t) { out.push(t); });");
        assert_eq!(r, "200|2|[]|ok");
        assert_eq!(srv.conns.load(SeqCst), 1, "one connection for both");
    });
}

#[test]
fn a_forbidden_method_is_a_type_error_and_nothing_is_sent() {
    big(|| {
        let srv = method_server();
        let mut b = at(srv.port);
        for m in ["CONNECT", "connect", "TRACE", "Track"] {
            assert_eq!(js(&mut b, &format!("new Request('/a', {{method:'{m}'}})")), format!("throw:TypeError:'{m}' HTTP method is unsupported"));
        }
        assert_eq!(js(&mut b, "new Request('/a', {method:'GET', body:'x'})"), "throw:TypeError:Request with GET/HEAD method cannot have body.");
        assert_eq!(js(&mut b, "new Request('/a', {method:'head', body:'x'})"), "throw:TypeError:Request with GET/HEAD method cannot have body.");
        assert_eq!(b.pending_fetches(), 0);
        assert!(srv.recs().is_empty());
    });
}

// ------------------------------------------------------------------------------------------ Blob, FormData and other bodies

/// What Chrome 155 sent for each body (`tools/ui/chrome_multipart.mjs`). `{B}` stands for the boundary.
const CHROME_BODIES: &str = include_str!("../../../tools/fixtures/net/chrome-bodies.txt");
const BOUNDARY: &str = "----WebKitFormBoundary0123456789abcdef";

fn unescape(s: &str) -> String {
    let mut out = String::new();
    let mut it = s.chars();
    while let Some(c) = it.next() {
        out.push(if c != '\\' { c } else { match it.next() { Some('r') => '\r', Some('n') => '\n', Some('t') => '\t', Some(x) => x, None => '\\' } });
    }
    out
}

#[test]
fn bodies_are_the_bytes_that_chrome_sends() {
    big(|| {
        let srv = serve(|_| ok("{}"));
        let mut b = at(srv.port);
        *b.dom.web.boundary.borrow_mut() = Some(BOUNDARY.into());
        let cases: Vec<Vec<&str>> = CHROME_BODIES.lines().map(|l| l.split('\t').collect()).collect();
        assert_eq!(cases.len(), 20);
        for c in &cases {
            let (name, expr, headers, ct, body) = (c[0], c[1], c[2], c[3], c[4]);
            let r = js(&mut b, &format!("fetch('/{name}', {{method: 'POST', body: {expr}, headers: {headers}}})"));
            assert!(!r.starts_with("throw") && !r.starts_with("unsupported"), "{name}: {r}");
            b.drain().unwrap();
            while b.pending_fetches() > 0 {
                b.run_network_tasks(100).unwrap();
            }
            let recs = srv.recs();
            let rec = recs.iter().find(|r| r.path == format!("/{name}")).unwrap_or_else(|| panic!("{name}: not sent"));
            let want = unescape(body).replace("{B}", BOUNDARY);
            assert_eq!(String::from_utf8_lossy(&rec.body), want, "{name}: body");
            let want_ct: Vec<String> = if ct.is_empty() { vec![] } else { vec![ct.replace("{B}", BOUNDARY)] };
            assert_eq!(rec.header("content-type"), want_ct, "{name}: content-type");
        }
    });
}

#[test]
fn each_request_has_a_new_random_boundary() {
    big(|| {
        let srv = serve(|_| ok("{}"));
        let mut b = at(srv.port);
        js(&mut b, "var f = new FormData(); f.append('a', '1'); fetch('/1', {method: 'POST', body: f}); fetch('/2', {method: 'POST', body: f});");
        b.drain().unwrap();
        while b.pending_fetches() > 0 {
            b.run_network_tasks(100).unwrap();
        }
        let cts: Vec<String> = srv.recs().iter().map(|r| r.header("content-type")[0].clone()).collect();
        assert_ne!(cts[0], cts[1]);
        for (ct, rec) in cts.iter().zip(srv.recs()) {
            let bound = ct.strip_prefix("multipart/form-data; boundary=").unwrap();
            let rand = bound.strip_prefix("----WebKitFormBoundary").unwrap();
            assert!(rand.len() == 16 && rand.bytes().all(|c| c.is_ascii_alphanumeric()), "{bound}");
            assert!(rec.body.starts_with(format!("--{bound}\r\n").as_bytes()) && rec.body.ends_with(format!("--{bound}--\r\n").as_bytes()));
        }
    });
}

#[test]
fn request_and_response_take_blob_form_and_params_bodies() {
    big(|| {
        let mut b = at(1);
        js(&mut b, "var out = [], f = new FormData(); f.append('a', 'x\\ny'); f.append('up', new File(['hello'], 'h.txt', {type: 'text/plain'}));");
        for (src, want) in [
            // FormData: the type, the text, the size, and the round trip through formData().
            ("out.push(/^multipart\\/form-data; boundary=----WebKitFormBoundary\\w{16}$/.test(new Response(f).headers.get('content-type'))); new Response(f).text().then(function (t) { out.push(t.includes('name=\"up\"; filename=\"h.txt\"\\r\\nContent-Type: text/plain\\r\\n\\r\\nhello\\r\\n')); });", "true|true"),
            ("var r = new Response(f); r.formData().then(function (g) { out.push(g.get('a'), g.get('up').name, g.get('up').type, g.get('up').size, [...g.keys()].join('+'), r.bodyUsed); return g.get('up').text(); }).then(function (t) { out.push(t); });", "x\r\ny|h.txt|text/plain|5|a+up|true|hello"),
            ("new Response(f).arrayBuffer().then(function (x) { out.push(x.byteLength > 200); });", "true"),
            ("out.push(new Request('/u', {method: 'POST', body: f}).headers.get('content-type').slice(0, 30), new Request('/u', {method: 'POST', body: f, headers: {'content-type': 'x/y'}}).headers.get('content-type'));", "multipart/form-data; boundary=|x/y"),
            // Blob: its bytes, its type (lower case), no type means no header.
            ("new Response(new Blob(['abc'], {type: 'Text/X'})).blob().then(function (x) { out.push(x.type, x.size); }); out.push(new Response(new Blob(['abc'])).headers.has('content-type'));", "false|text/x|3"),
            ("new Request('/u', {method: 'POST', body: new Blob(['zz'], {type: 'a/b'})}).blob().then(function (x) { out.push(x.type); return x.text(); }).then(function (t) { out.push(t); });", "a/b|zz"),
            // URLSearchParams, and formData() of a urlencoded body.
            ("var q = new Request('/u', {method: 'POST', body: new URLSearchParams({a: '1 2'})}); out.push(q.headers.get('content-type')); q.text().then(function (t) { out.push(t); });", "application/x-www-form-urlencoded;charset=UTF-8|a=1+2"),
            ("new Response('a=1&b=%C3%BC+x&a=2', {headers: {'content-type': 'application/x-www-form-urlencoded; charset=utf-8'}}).formData().then(function (g) { out.push(g.getAll('a').join(), g.get('b')); });", "1,2|ü x"),
            ("new Response('x').formData().then(function () { out.push('bad'); }, function (e) { out.push(e.name); });", "TypeError"),
            // Typed arrays keep no type; a string is text.
            ("out.push(new Response(new Uint8Array([1, 2])).headers.has('content-type'), new Response('s').headers.get('content-type'));", "false|text/plain;charset=UTF-8"),
        ] {
            js(&mut b, "out.length = 0");
            assert_eq!(go(&mut b, src), want, "{src}");
        }
    });
}

#[test]
fn a_form_body_over_the_limit_is_refused_before_the_copy() {
    big(|| {
        let mut b = mk(Some("http://127.0.0.1:1/".into()), FetchLimits { max_request_body: 1000, ..Default::default() });
        assert_eq!(js(&mut b, "var f = new FormData(); f.append('f', new Blob([new Uint8Array(2000)])); fetch('/x', {method: 'POST', body: f})"), lim("fetch-request-body"));
        assert_eq!(js(&mut b, "fetch('/x', {method: 'POST', body: new Blob([new Uint8Array(2000)])})"), lim("fetch-request-body"));
    });
}

#[test]
fn multipart_parsing_handles_odd_parts() {
    big(|| {
        let mut b = at(1);
        let src = "var out = [], body = ['junk', '--q', 'Content-Disposition: form-data; name=\"a%22b\"', '', 'v', '--q', 'Content-Disposition: form-data; name=\"e\"; filename=\"n%0A.bin\"', '', '', '--q', 'Content-Disposition: form-data; name=\"c\"', '', '', '--q--', 'tail'].join('\\r\\n');
          new Response(body, {headers: {'content-type': 'multipart/form-data; boundary=\"q\"'}}).formData().then(function (g) { out.push([...g].map(function (e) { return typeof e[1] === 'string' ? e[0] + '=' + e[1] : e[0] + ':' + e[1].name + ':' + e[1].type + ':' + e[1].size; }).join(';')); });
          new Response('--q\\r\\nbroken', {headers: {'content-type': 'multipart/form-data; boundary=q'}}).formData().then(function () { out.push('bad'); }, function (e) { out.push(e.name); });
          new Response('--q--', {headers: {'content-type': 'multipart/form-data'}}).formData().then(function () { out.push('bad'); }, function (e) { out.push(e.name); });";
        assert_eq!(go(&mut b, src), "a\"b=v;e:n\n.bin:text/plain:0;c=|TypeError|TypeError");
    });
}

#[test]
fn stream_body_rejects_one_fetch_without_ending_the_script() {
    big(|| {
        let mut b = at(1);
        assert_eq!(js(&mut b, "var caught = ''; fetch('/x', {method: 'POST', body: new ReadableStream()}).catch(function(e) { caught = e.name }); 'continued'"), "continued");
        b.drain().unwrap();
        assert_eq!(js(&mut b, "caught"), "TypeError");
        assert_eq!(js(&mut b, "try { new Request('/x', {method: 'POST', body: new ReadableStream()}) } catch(e) { e.name }"), "TypeError");
        assert_eq!(js(&mut b, "new Response(new Blob(['abc'])).text().then(function(x) { caught = x }); 'continued'"), "continued");
        b.drain().unwrap();
        assert_eq!(js(&mut b, "caught"), "abc");
    });
}

#[test]
fn credential_fetch_rejects_without_sending_a_request() {
    big(|| {
        let mut b = at(1);
        assert_eq!(js(&mut b, "var error = ''; fetch('http://user:pass@127.0.0.1:1/x').catch(function(e) { error = e.name }); 'continued'"), "continued");
        b.drain().unwrap();
        assert_eq!(js(&mut b, "error"), "TypeError");
        assert_eq!(b.dom.fetch_queued(), 0);
        assert_eq!(js(&mut b, "try { new Request('http://user:pass@127.0.0.1:1/x') } catch(e) { e.name }"), "TypeError");
    });
}

/// gitlab: a script that calls `fetch` 64 times in one task, before any request can finish, stopped the page with `fetch-queue`. The queue
/// holds 1024 requests now (Chrome queues too), so a page with 200 `fetch` and 200 `XMLHttpRequest` calls in one task loads and gets every answer.
#[test]
fn many_requests_in_one_task_are_queued() {
    big(|| {
        let srv = serve(|_| ok("{}"));
        let mut b = mk(Some(format!("http://127.0.0.1:{}/", srv.port)), FetchLimits::default());
        js(&mut b, "var done = 0; for (var i = 0; i < 200; i++) { fetch('/f' + i).then(function () { done++; }); var x = new XMLHttpRequest(); x.open('GET', '/x' + i); x.onload = function () { done++; }; x.send(); }");
        assert_eq!(b.pending_fetches(), 400);
        b.run_network_tasks(1000).unwrap();
        assert_eq!(js(&mut b, "done"), "400");
        assert_eq!(b.pending_fetches(), 0);
    });
}
