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
use std::time::Duration;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
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
    fn header(&self, name: &str) -> Vec<String> {
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
}

impl Srv {
    pub fn recs(&self) -> Vec<Rec> {
        self.recs.lock().unwrap().clone()
    }
}

pub fn serve(handler: impl Fn(&Rec) -> Reply + Send + Sync + 'static) -> Srv {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (recs, conns) = (Arc::new(Mutex::new(Vec::new())), Arc::new(AtomicUsize::new(0)));
    let handler = Arc::new(handler);
    let (r2, c2) = (recs.clone(), conns.clone());
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            c2.fetch_add(1, SeqCst);
            let (h, r) = (handler.clone(), r2.clone());
            std::thread::spawn(move || conn(stream, h, r));
        }
    });
    Srv { port, recs, conns }
}

fn conn(mut s: TcpStream, h: Arc<dyn Fn(&Rec) -> Reply + Send + Sync>, recs: Arc<Mutex<Vec<Rec>>>) {
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
        if s.write_all(&r.raw).is_err() || r.close {
            return;
        }
    }
}

fn mk(base: Option<String>, limits: FetchLimits) -> Browser {
    let mut b = Browser::new(tree::parse("<body></body>"), BrowserLimits::default(), EvalLimits { max_steps: 20_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
    b.configure_fetch(FetchConfig { base_url: base, client: Client::new(), limits }).unwrap();
    b
}

fn at(port: u16) -> Browser {
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

fn js(b: &mut Browser, src: &str) -> String {
    let r = b.run_script(src);
    show(b, r)
}

/// Run a script, run the microtasks, then all network tasks. Gives the value of `out.join('|')`.
fn go(b: &mut Browser, src: &str) -> String {
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
        std::thread::sleep(Duration::from_millis(50));
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
        std::thread::sleep(Duration::from_millis(50));
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
        assert!(js(&mut b, "new Headers().entries()").starts_with("unsupported:Headers iteration"));
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
            ("new Request('/x', {method:'PUT'}); fetch(new Request('/x', {method:'PUT'}))", "fetch method other than GET and POST"),
            ("new Request('/x', {signal: {}})", "fetch option signal (AbortController)"),
            ("new Request('/x', {mode:'no-cors'})", "fetch mode: no-cors (opaque responses)"),
            ("new Request('/x', {redirect:'manual'})", "fetch redirect: manual (opaque redirect responses)"),
            ("new Request('/x', {integrity:'sha256-x'})", "fetch option integrity"),
            ("new Request('/x').body", "Request.body (a request body stream)"),
            ("new Response('x').blob()", "Body.blob and Body.formData (no Blob or FormData)"),
            ("new Response('x').formData()", "Body.blob and Body.formData (no Blob or FormData)"),
            ("new ReadableStream({start(c) {}})", "new ReadableStream (a custom underlying source)"),
            ("new Response('x').body.tee()", "ReadableStream tee, pipeTo, pipeThrough and async iteration"),
            ("new Response('x').body.getReader().closed", "ReadableStreamDefaultReader.closed"),
            ("new Response('x').body.getReader({mode: 'byob'})", "ReadableStream BYOB reader"),
        ] {
            let r = js(&mut b, src);
            assert_eq!(r, format!("unsupported:{what}"), "{src}");
        }
        assert_eq!(js(&mut b, "new Request('/x', {body: {getReader: function () {}}, method: 'POST'})"), "unsupported:a ReadableStream as a body");
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
fn failures_are_type_errors_with_fixed_text_and_clean_diagnostics() {
    big(|| {
        let srv = serve(|r| match r.path.as_str() {
            p if p.starts_with("/truncated") => Reply { raw: b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\nshort".to_vec(), close: true },
            p if p.starts_with("/garbage") => Reply { raw: b"not http\r\n\r\n".to_vec(), close: true },
            p if p.starts_with("/gzip") => reply(200, &[("Content-Encoding", "gzip")], b"\x1f\x8b"),
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
fn cross_origin_requests_are_a_gate_before_any_socket() {
    big(|| {
        let a = serve(|_| ok("{}"));
        let other = serve(|_| ok("{}"));
        let mut b = at(a.port);
        let r = js(&mut b, &format!("fetch('http://127.0.0.1:{}/x?secret=1', {{method:'POST', body:'pw=hunter2', headers:{{Authorization:'Bearer t'}}}})", other.port));
        assert_eq!(r, "unsupported:fetch to another origin (CORS is not implemented)");
        for u in ["https://127.0.0.1/x", "http://localhost:1/x", "//127.0.0.1:1/x"] {
            assert_eq!(js(&mut b, &format!("fetch('{u}')")), "unsupported:fetch to another origin (CORS is not implemented)", "{u}");
        }
        assert_eq!(js(&mut b, "fetch('ftp://x/y').then(null, function (e) { out2 = e.name; }); typeof out2"), "undefined");
        b.drain().unwrap();
        assert_eq!(js(&mut b, "out2"), "TypeError");
        assert_eq!(b.pending_fetches(), 0);
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!((a.conns.load(SeqCst), other.conns.load(SeqCst)), (0, 0));
        let d = b.take_fetch_diagnostics().join("\n");
        assert!(d.contains("cross-origin") && !d.contains("secret") && !d.contains("hunter2") && !d.contains("/x"), "{d}");
    });
}

#[test]
fn a_cross_origin_307_never_forwards_the_body_or_credentials() {
    big(|| {
        let target = TcpListener::bind("127.0.0.1:0").unwrap();
        target.set_nonblocking(true).unwrap();
        let tp = target.local_addr().unwrap().port();
        for code in [307, 308, 302, 303] {
            let srv = serve(move |_| reply(code, &[("Location", &format!("http://127.0.0.1:{tp}/steal"))], b""));
            let mut b = at(srv.port);
            let got = go(&mut b, "var out = []; fetch('/login', {method:'POST', body:'pw=hunter2', headers:{Authorization:'Bearer tok', Cookie:'x'}}).then(function () { out.push('ok'); }, function (e) { out.push(e.name + ':' + e.message); });");
            assert_eq!(got, "TypeError:Failed to fetch", "{code}");
            assert_eq!(srv.recs().len(), 1);
            let d = b.take_fetch_diagnostics().join("\n");
            assert!(d.contains("cross-origin redirect") && !d.contains("steal") && !d.contains("hunter2"), "{d}");
        }
        std::thread::sleep(Duration::from_millis(50));
        assert!(target.accept().is_err(), "a socket was opened to the other origin");
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
        go(&mut b, "var out = []; fetch('/close-after').then(function () { return new Promise(function (r) { r(); }); }).then(function () { return fetch('/after', {method:'POST', body:'once'}); }).then(function (r) { out.push(r.status); });");
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
        assert_eq!(js(&mut b, &format!("fetch('http://127.0.0.1:{}/old')", a.port)), "unsupported:fetch to another origin (CORS is not implemented)");
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
        std::thread::sleep(Duration::from_millis(50));
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
