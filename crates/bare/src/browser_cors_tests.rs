//! Tests of CORS for `fetch` and `XMLHttpRequest` (`browser_fetch.rs`). Two loopback servers on two ports are two origins: the
//! page is `http://127.0.0.1:<page>`, the other origin is `http://127.0.0.1:<other>`. The network is never the public one.
//! Expected values follow the Fetch Standard and what Chrome sends and prints.

use crate::browser::*;
use crate::browser_fetch_tests::*;

fn console(b: &mut Browser) -> Vec<String> {
    let recs = b.take_console_records();
    recs.iter().map(|r| b.format_console_record(r).unwrap()).collect()
}

fn origin_of(r: &Rec) -> String {
    r.header("origin").first().cloned().unwrap_or_default()
}

/// The page server (it answers `{}`), the other origin with `h`, and a browser on the page.
fn two(h: impl Fn(&Rec) -> Reply + Send + Sync + 'static) -> (Srv, Srv, Browser) {
    let page = serve(|_| ok("{}"));
    let other = serve(h);
    let b = at(page.port);
    (page, other, b)
}

fn u(s: &Srv, p: &str) -> String {
    format!("http://127.0.0.1:{}{p}", s.port)
}

/// Answer with `Access-Control-Allow-Origin` set to the origin of the request, plus `extra`.
fn allow(r: &Rec, extra: &[(&str, &str)]) -> Reply {
    let o = origin_of(r);
    let mut h = vec![("Access-Control-Allow-Origin", o.as_str()), ("Content-Type", "text/plain")];
    h.extend_from_slice(extra);
    reply(200, &h, b"data")
}

#[test]
fn a_simple_cross_origin_get_sends_origin_and_reads_when_it_is_allowed() {
    big(|| {
        let (page, other, mut b) = two(|r| allow(r, &[]));
        let src = format!("var out = []; fetch('{}').then(function (r) {{ out.push(r.type, r.status, r.ok); return r.text(); }}).then(function (t) {{ out.push(t); }});", u(&other, "/a"));
        assert_eq!(go(&mut b, &src), "cors|200|true|data");
        let recs = other.recs();
        assert_eq!(recs.len(), 1, "a simple request has no preflight");
        assert_eq!(origin_of(&recs[0]), format!("http://127.0.0.1:{}", page.port));
        assert!(recs[0].header("cookie").is_empty() && recs[0].header("referer").is_empty());
        assert!(console(&mut b).is_empty());
        // `*` is enough without credentials.
        let (_, other, mut b) = two(|_| reply(200, &[("Access-Control-Allow-Origin", "*")], b"star"));
        assert_eq!(go(&mut b, &format!("var out = []; fetch('{}').then(function (r) {{ return r.text(); }}).then(function (t) {{ out.push(t); }});", u(&other, "/a"))), "star");
    });
}

#[test]
fn a_same_origin_request_has_no_origin_no_preflight_and_a_basic_response() {
    big(|| {
        let (page, other, mut b) = two(|_| ok("{}"));
        let src = "var out = []; fetch('/a', {method:'PUT', headers:{'X-Foo':'1', 'Content-Type':'application/json'}, body:'{}'}).then(function (r) { out.push(r.type, r.status); });";
        assert_eq!(go(&mut b, src), "basic|200");
        let recs = page.recs();
        assert_eq!(recs.len(), 1);
        assert!(recs[0].header("origin").is_empty());
        assert_eq!(other.recs().len(), 0);
    });
}

#[test]
fn a_failed_check_rejects_logs_one_line_and_the_simple_request_was_sent() {
    big(|| {
        let (page, other, mut b) = two(|r| match r.path.as_str() {
            "/none" => reply(200, &[], b"x"),
            "/wrong" => reply(200, &[("Access-Control-Allow-Origin", "http://example.test")], b"x"),
            "/two" => reply(200, &[("Access-Control-Allow-Origin", "http://a.test, http://b.test")], b"x"),
            _ => reply(200, &[("Access-Control-Allow-Origin", "*")], b"x"),
        });
        let po = format!("http://127.0.0.1:{}", page.port);
        let at_ = |p: &str| format!("fetch('{}').then(function () {{ out.push('ok'); }}, function (e) {{ out.push(e.name + ':' + e.message); }});", u(&other, p));
        let src = format!("var out = []; {}", at_("/none"));
        assert_eq!(go(&mut b, &src), "TypeError:Failed to fetch");
        assert_eq!(other.recs().len(), 1, "the browser sent the request; it only hid the reply");
        let c = console(&mut b);
        assert_eq!(c.len(), 1, "{c:?}");
        let want = format!("Access to fetch at 'http://127.0.0.1:{}' from origin '{po}' has been blocked by CORS policy: No 'Access-Control-Allow-Origin' header is present on the requested resource.", other.port);
        assert_eq!(c[0], want);
        assert_eq!(go(&mut b, &format!("out = []; {}", at_("/wrong"))), "TypeError:Failed to fetch");
        let c = console(&mut b);
        assert!(c[0].ends_with("blocked by CORS policy: The 'Access-Control-Allow-Origin' header has a value 'http://example.test' that is not equal to the supplied origin."), "{c:?}");
        assert_eq!(go(&mut b, &format!("out = []; {}", at_("/two"))), "TypeError:Failed to fetch");
        let c = console(&mut b);
        assert!(c[0].contains("contains multiple values 'http://a.test, http://b.test', but only one is allowed."), "{c:?}");
        // The page never sees the path or query in the log.
        assert_eq!(go(&mut b, &format!("out = []; {}", at_("/ok?secret=1"))), "ok");
        assert!(console(&mut b).is_empty());
    });
}

#[test]
fn credentials_include_needs_the_exact_origin_and_allow_credentials() {
    big(|| {
        let (_, other, mut b) = two(|r| {
            let o = origin_of(r);
            match r.path.as_str() {
                "/star" => reply(200, &[("Access-Control-Allow-Origin", "*"), ("Access-Control-Allow-Credentials", "true")], b"x"),
                "/nocreds" => reply(200, &[("Access-Control-Allow-Origin", &o)], b"x"),
                "/falsy" => reply(200, &[("Access-Control-Allow-Origin", &o), ("Access-Control-Allow-Credentials", "True")], b"x"),
                _ => reply(200, &[("Access-Control-Allow-Origin", &o), ("Access-Control-Allow-Credentials", "true")], b"x"),
            }
        });
        let run = |b: &mut Browser, p: &str, creds: &str| go(b, &format!("var out = []; fetch('{}', {{credentials:'{creds}'}}).then(function (r) {{ out.push(r.status); }}, function (e) {{ out.push(e.message); }});", u(&other, p)));
        assert_eq!(run(&mut b, "/good", "include"), "200");
        assert_eq!(run(&mut b, "/star", "include"), "Failed to fetch");
        assert!(console(&mut b)[0].contains("must not be the wildcard '*' when the request's credentials mode is 'include'."));
        assert_eq!(run(&mut b, "/nocreds", "include"), "Failed to fetch");
        assert!(console(&mut b)[0].contains("The value of the 'Access-Control-Allow-Credentials' header in the response is '' which must be 'true' when the request's credentials mode is 'include'."));
        assert_eq!(run(&mut b, "/falsy", "include"), "Failed to fetch");
        assert!(console(&mut b)[0].contains("is 'True' which must be 'true'"));
        // Without `include`, the credentials header does not matter and `*` is fine.
        assert_eq!(run(&mut b, "/star", "same-origin"), "200");
        assert_eq!(run(&mut b, "/nocreds", "omit"), "200");
        // No cookie is ever sent: the engine has no HTTP cookie jar.
        assert!(other.recs().iter().all(|r| r.header("cookie").is_empty()));
    });
}

#[test]
fn a_script_sees_only_the_safelisted_and_the_exposed_response_headers() {
    big(|| {
        let (_, other, mut b) = two(|r| match r.path.as_str() {
            "/expose" => allow(r, &[("X-Secret", "s"), ("X-Shown", "v"), ("Access-Control-Expose-Headers", "X-Shown, content-length"), ("Cache-Control", "no-cache"), ("Last-Modified", "m"), ("ETag", "e")]),
            "/star" => allow(r, &[("X-One", "1"), ("X-Two", "2"), ("Access-Control-Expose-Headers", "*")]),
            _ => allow(r, &[("X-Secret", "s")]),
        });
        let list = "var l = []; r.headers.forEach(function (v, k) { l.push(k); }); out.push(l.sort().join());";
        let run = |b: &mut Browser, p: &str, extra: &str| go(b, &format!("var out = []; fetch('{}'{extra}).then(function (r) {{ {list} }});", u(&other, p)));
        assert_eq!(run(&mut b, "/plain", ""), "content-length,content-type");
        assert_eq!(run(&mut b, "/expose", ""), "cache-control,content-length,content-type,last-modified,x-shown");
        assert_eq!(run(&mut b, "/star", ""), "access-control-allow-origin,access-control-expose-headers,content-length,content-type,x-one,x-two");
        // With credentials, `*` names nothing.
        let (_, other, mut b) = two(|r| allow(r, &[("X-One", "1"), ("Access-Control-Expose-Headers", "*"), ("Access-Control-Allow-Credentials", "true")]));
        let src = format!("var out = []; fetch('{}', {{credentials:'include'}}).then(function (r) {{ {list} }});", u(&other, "/a"));
        assert_eq!(go(&mut b, &src), "content-length,content-type");
    });
}

/// The preflight answer of the tests: it allows what the query lists.
fn preflight_ok(r: &Rec, methods: &str, headers: &str, extra: &[(&str, &str)]) -> Reply {
    let o = origin_of(r);
    let mut h = vec![("Access-Control-Allow-Origin", o.as_str()), ("Access-Control-Allow-Methods", methods), ("Access-Control-Allow-Headers", headers)];
    h.extend_from_slice(extra);
    reply(204, &h, b"")
}

#[test]
fn a_non_simple_request_sends_a_preflight_first() {
    big(|| {
        let (page, other, mut b) = two(|r| if r.method == "OPTIONS" { preflight_ok(r, "PUT, DELETE", "x-foo, Content-Type", &[]) } else { allow(r, &[]) });
        let src = format!("var out = []; fetch('{}', {{method:'PUT', headers:{{'X-Foo':'1', 'Content-Type':'application/json', 'Accept-Language':'en'}}, body:'{{}}'}}).then(function (r) {{ out.push(r.status); return r.text(); }}).then(function (t) {{ out.push(t); }});", u(&other, "/p?q=1"));
        assert_eq!(go(&mut b, &src), "200|data");
        let recs = other.recs();
        assert_eq!(recs.iter().map(|r| r.method.as_str()).collect::<Vec<_>>(), ["OPTIONS", "PUT"]);
        let po = format!("http://127.0.0.1:{}", page.port);
        let (pre, act) = (&recs[0], &recs[1]);
        assert_eq!(pre.path, "/p?q=1");
        assert_eq!(origin_of(pre), po);
        assert_eq!(pre.header("access-control-request-method"), ["PUT"]);
        assert_eq!(pre.header("access-control-request-headers"), ["content-type,x-foo"], "sorted, lower case, without the safelisted accept-language");
        assert!(pre.header("authorization").is_empty() && pre.header("content-type").is_empty() && pre.body.is_empty() && pre.header("cookie").is_empty());
        assert_eq!(origin_of(act), po);
        assert_eq!(act.header("x-foo"), ["1"]);
        assert_eq!(act.body, b"{}");
        assert!(act.header("access-control-request-method").is_empty());
        assert!(console(&mut b).is_empty());
    });
}

#[test]
fn these_requests_are_simple_and_those_are_not() {
    big(|| {
        let (_, other, mut b) = two(|r| if r.method == "OPTIONS" { preflight_ok(r, "*", "authorization, x-req, accept, content-type", &[]) } else { allow(r, &[]) });
        let run = |b: &mut Browser, init: &str| {
            let before = other.recs().len();
            go(b, &format!("var out = []; fetch('{}', {init});", u(&other, "/s")));
            other.recs()[before..].iter().map(|r| r.method.clone()).collect::<Vec<_>>().join(",")
        };
        assert_eq!(run(&mut b, "{method:'POST', body:'a=b', headers:{'Content-Type':'application/x-www-form-urlencoded'}}"), "POST");
        assert_eq!(run(&mut b, "{method:'POST', body:'a', headers:{'Content-Type':'text/plain; charset=utf-8'}}"), "POST");
        assert_eq!(run(&mut b, "{method:'POST', body:new FormData()}"), "POST");
        assert_eq!(run(&mut b, "{method:'HEAD', headers:{'Accept':'text/html', 'Accept-Language':'de', 'Content-Language':'de'}}"), "HEAD");
        assert_eq!(run(&mut b, "{method:'POST', body:'{}', headers:{'Content-Type':'application/json'}}"), "OPTIONS,POST");
        assert_eq!(run(&mut b, "{headers:{'Authorization':'Bearer t'}}"), "OPTIONS,GET");
        assert_eq!(run(&mut b, "{method:'DELETE'}"), "OPTIONS,DELETE");
        assert_eq!(run(&mut b, "{method:'PATCH', body:'x'}"), "OPTIONS,PATCH");
        assert_eq!(run(&mut b, &format!("{{headers:{{'Accept':'{}'}}}}", "a".repeat(129))), "OPTIONS,GET", "a safelisted value over 128 bytes is unsafe");
        assert_eq!(run(&mut b, "{headers:{'X-Req':'1'}}"), "OPTIONS,GET");
    });
}

#[test]
fn a_failed_preflight_sends_no_request_and_names_the_reason() {
    big(|| {
        let (_, other, mut b) = two(|r| {
            if r.method != "OPTIONS" {
                return allow(r, &[]);
            }
            match r.path.as_str() {
                "/status" => reply(403, &[("Access-Control-Allow-Origin", "*"), ("Access-Control-Allow-Methods", "PUT")], b""),
                "/method" => preflight_ok(r, "GET, POST", "x-foo", &[]),
                "/header" => preflight_ok(r, "PUT", "x-bar", &[]),
                "/origin" => reply(204, &[("Access-Control-Allow-Methods", "PUT"), ("Access-Control-Allow-Headers", "x-foo")], b""),
                "/redirect" => reply(302, &[("Location", "/method"), ("Access-Control-Allow-Origin", "*")], b""),
                "/auth" => preflight_ok(r, "PUT", "*", &[]),
                _ => preflight_ok(r, "PUT", "x-foo", &[]),
            }
        });
        let run = |b: &mut Browser, p: &str, h: &str| go(b, &format!("var out = []; fetch('{}', {{method:'PUT', headers:{{{h}}}}}).then(function () {{ out.push('ok'); }}, function (e) {{ out.push(e.message); }});", u(&other, p)));
        for (p, want) in [
            ("/status", "Response to preflight request doesn't pass access control check: It does not have HTTP ok status."),
            ("/method", "Method PUT is not allowed by Access-Control-Allow-Methods in preflight response."),
            ("/header", "Request header field x-foo is not allowed by Access-Control-Allow-Headers in preflight response."),
            ("/origin", "Response to preflight request doesn't pass access control check: No 'Access-Control-Allow-Origin' header is present on the requested resource."),
            ("/redirect", "Response to preflight request doesn't pass access control check: It does not have HTTP ok status."),
        ] {
            let before = other.recs().len();
            assert_eq!(run(&mut b, p, "'X-Foo':'1'"), "Failed to fetch", "{p}");
            let recs = other.recs();
            assert_eq!(recs.len() - before, 1, "{p}: the request after a failed preflight was sent");
            let c = console(&mut b);
            assert_eq!(c.len(), 1, "{p}: {c:?}");
            assert!(c[0].starts_with("Access to fetch at ") && c[0].ends_with(&format!("blocked by CORS policy: {want}")), "{p}: {c:?}");
        }
        // `*` in the allowed headers does not cover `Authorization`.
        assert_eq!(run(&mut b, "/auth", "'Authorization':'t'"), "Failed to fetch");
        assert!(console(&mut b)[0].contains("Request header field authorization is not allowed by Access-Control-Allow-Headers"));
        assert_eq!(run(&mut b, "/auth", "'X-Any':'t'"), "ok");
    });
}

#[test]
fn a_preflight_is_cached_for_max_age_and_keyed_by_url_method_headers() {
    big(|| {
        let (page, other, mut b) = two(|r| if r.method == "OPTIONS" { preflight_ok(r, "PUT, DELETE", "x-foo, x-bar", &[("Access-Control-Max-Age", if r.path.starts_with("/one") { "1" } else { "100" })]) } else { allow(r, &[]) });
        let run = |b: &mut Browser, p: &str, m: &str, h: &str| {
            let before = other.recs().len();
            go(b, &format!("var out = []; fetch('{}', {{method:'{m}', headers:{{{h}}}}});", u(&other, p)));
            other.recs()[before..].iter().map(|r| r.method.clone()).collect::<Vec<_>>().join(",")
        };
        assert_eq!(run(&mut b, "/long", "PUT", "'X-Foo':'1'"), "OPTIONS,PUT");
        assert_eq!(run(&mut b, "/long", "PUT", "'X-Foo':'2'"), "PUT", "the same URL, method and header names");
        assert_eq!(run(&mut b, "/long", "DELETE", "'X-Foo':'1'"), "OPTIONS,DELETE", "another method");
        assert_eq!(run(&mut b, "/long", "PUT", "'X-Bar':'1'"), "OPTIONS,PUT", "other header names");
        assert_eq!(run(&mut b, "/long2", "PUT", "'X-Foo':'1'"), "OPTIONS,PUT", "another URL");
        // A new browser (a new page) on the same origin reuses the entry, as a browser cache does for a profile.
        let mut b2 = at(page.port);
        assert_eq!(run(&mut b2, "/long", "PUT", "'X-Foo':'1'"), "PUT");
        // A max age of 1 s ends.
        assert_eq!(run(&mut b, "/one", "PUT", "'X-Foo':'1'"), "OPTIONS,PUT");
        assert_eq!(run(&mut b, "/one", "PUT", "'X-Foo':'1'"), "PUT");
        std::thread::sleep(std::time::Duration::from_millis(1100));
        assert_eq!(run(&mut b, "/one", "PUT", "'X-Foo':'1'"), "OPTIONS,PUT", "the entry ended");
    });
}

#[test]
fn max_age_defaults_to_5_seconds_and_is_capped_at_600() {
    use crate::browser_fetch::preflight_max_age as m;
    assert_eq!((m(None), m(Some("")), m(Some("abc")), m(Some("-1")), m(Some("0")), m(Some("7")), m(Some(" 30 ")), m(Some("600")), m(Some("601")), m(Some("99999999999999999999"))), (5, 5, 5, 5, 0, 7, 30, 600, 600, 600));
}

#[test]
fn the_origin_is_null_once_a_redirect_leaves_a_cross_origin_hop() {
    big(|| {
        let page = serve(|r| match r.path.as_str() {
            "/back" => reply(200, &[("Access-Control-Allow-Origin", "null")], b"back"),
            "/backstar" => reply(200, &[("Access-Control-Allow-Origin", "*")], b"back"),
            _ => ok("{}"),
        });
        let third = serve(|r| allow(r, &[]));
        let (pp, tp) = (page.port, third.port);
        let other = serve(move |r| match r.path.as_str() {
            // The same cross origin: not tainted.
            "/same" => reply(302, &[("Location", "/final"), ("Access-Control-Allow-Origin", "*")], b""),
            "/final" => allow(r, &[]),
            // Back to the page, and on to a third origin: tainted.
            "/back" => reply(302, &[("Location", &format!("http://127.0.0.1:{pp}/back")), ("Access-Control-Allow-Origin", "*")], b""),
            "/third" => reply(307, &[("Location", &format!("http://127.0.0.1:{tp}/t")), ("Access-Control-Allow-Origin", "*")], b""),
            "/norule" => reply(302, &[("Location", "/final")], b""),
            _ => allow(r, &[]),
        });
        let mut b = at(page.port);
        let po = format!("http://127.0.0.1:{}", page.port);
        let run = |b: &mut Browser, p: &str| go(b, &format!("var out = []; fetch('{}').then(function (r) {{ return r.text(); }}).then(function (t) {{ out.push(t); }}, function (e) {{ out.push(e.message); }});", u(&other, p)));
        assert_eq!(run(&mut b, "/same"), "data");
        let recs = other.recs();
        assert_eq!((origin_of(&recs[0]), origin_of(&recs[1])), (po.clone(), po.clone()));
        // Each hop is checked: a redirect reply without the header fails.
        assert_eq!(run(&mut b, "/norule"), "Failed to fetch");
        assert!(console(&mut b).iter().any(|l| l.contains("No 'Access-Control-Allow-Origin' header")));
        assert_eq!(other.recs().iter().filter(|r| r.path == "/final").count(), 1, "no hop after a failed hop");
        // Cross-origin, then back to the page origin: the origin is `null`, and the page must allow `null` or `*`.
        assert_eq!(run(&mut b, "/back"), "back");
        assert_eq!(origin_of(page.recs().last().unwrap()), "null");
        // Through a third origin the origin is null there as well.
        assert_eq!(run(&mut b, "/third"), "data");
        assert_eq!(origin_of(third.recs().last().unwrap()), "null");
    });
}

#[test]
fn a_same_origin_request_may_redirect_to_another_origin_with_the_check() {
    big(|| {
        let other = serve(|r| allow(r, &[]));
        let op = other.port;
        let page = serve(move |r| match r.path.as_str() {
            "/go" => reply(302, &[("Location", &format!("http://127.0.0.1:{op}/x"))], b""),
            "/post" => reply(307, &[("Location", &format!("http://127.0.0.1:{op}/x"))], b""),
            _ => ok("{}"),
        });
        let mut b = at(page.port);
        assert_eq!(go(&mut b, "var out = []; fetch('/go').then(function (r) { out.push(r.type, r.redirected); return r.text(); }).then(function (t) { out.push(t); });"), "cors|true|data");
        assert_eq!(origin_of(&other.recs()[0]), format!("http://127.0.0.1:{}", page.port));
        // A 307 keeps the method and the body, as in Chrome (the request is simple), but `Authorization` never leaves the origin.
        // The reply has no allow header, so the page gets an error.
        let silent = serve(|_| reply(200, &[], b"no"));
        let sp = silent.port;
        let page = serve(move |_| reply(307, &[("Location", &format!("http://127.0.0.1:{sp}/target"))], b""));
        let mut b = at(page.port);
        let got = go(&mut b, "var out = []; fetch('/login', {method:'POST', body:'pw=1', headers:{Authorization:'Bearer tok', Cookie:'x'}}).then(function () { out.push('ok'); }, function (e) { out.push(e.message); });");
        assert_eq!(got, "Failed to fetch");
        let recs = silent.recs();
        assert_eq!(recs.len(), 1, "{recs:?}");
        assert!(recs[0].header("authorization").is_empty() && recs[0].header("cookie").is_empty(), "{recs:?}");
        let log = format!("{:?}{:?}", b.take_fetch_diagnostics(), console(&mut b));
        assert!(!log.contains("pw=1") && !log.contains("target") && !log.contains("tok"), "{log}");
    });
}

#[test]
fn a_mixed_content_request_is_blocked_before_any_dns() {
    big(|| {
        let mut b = mk_https_page();
        assert_eq!(go(&mut b, "var out = []; fetch('http://other.test/x').catch(function (e) { out.push(e.name + ':' + e.message); });"), "TypeError:Failed to fetch");
        assert!(console(&mut b).iter().any(|l| l.starts_with("Mixed Content:")));
    });
}

fn mk_https_page() -> Browser {
    mk(Some("https://page.test/app".into()), crate::browser_fetch::FetchLimits::default())
}

#[test]
fn no_cors_gives_an_opaque_response_whatever_the_server_says() {
    big(|| {
        let (page, other, mut b) = two(|_| reply(404, &[("Content-Type", "text/plain"), ("X-A", "1")], b"hidden"));
        let src = format!("var out = []; fetch('{}', {{mode:'no-cors'}}).then(function (r) {{ var n = 0; r.headers.forEach(function () {{ n++; }}); out.push(r.type, r.status, r.ok, r.url === '', n, r.body); return r.text(); }}).then(function (t) {{ out.push('[' + t + ']'); }});", u(&other, "/o"));
        assert_eq!(go(&mut b, &src), "opaque|0|false|true|0||[]");
        // A GET carries no Origin; a POST does.
        let src = format!("var out = []; fetch('{}', {{mode:'no-cors', method:'POST', body:'a=1', headers:{{'Content-Type':'application/x-www-form-urlencoded', 'X-Custom':'dropped'}}}}).then(function (r) {{ out.push(r.type, r.status); }});", u(&other, "/p"));
        assert_eq!(go(&mut b, &src), "opaque|0");
        let recs = other.recs();
        assert!(recs[0].header("origin").is_empty());
        assert_eq!(origin_of(&recs[1]), format!("http://127.0.0.1:{}", page.port));
        assert!(recs[1].header("x-custom").is_empty(), "a no-cors request drops the headers that are not safelisted");
        assert_eq!(recs.len(), 2, "no preflight in no-cors");
        // Only GET, HEAD and POST.
        assert_eq!(js(&mut b, &format!("new Request('{}', {{mode:'no-cors', method:'PUT'}})", u(&other, "/p"))), "throw:TypeError:'PUT' is unsupported in no-cors mode.");
        assert_eq!(js(&mut b, "new Request('/p', {mode:'no-cors'}).mode"), "no-cors");
        // A same-origin request in no-cors mode is a basic response.
        assert_eq!(go(&mut b, "var out = []; fetch('/q', {mode:'no-cors'}).then(function (r) { out.push(r.type, r.status); });"), "basic|200");
    });
}

#[test]
fn same_origin_mode_rejects_a_cross_origin_url_before_any_socket() {
    big(|| {
        let (_, other, mut b) = two(|r| allow(r, &[]));
        let src = format!("var out = []; fetch('{}', {{mode:'same-origin'}}).then(function () {{ out.push('ok'); }}, function (e) {{ out.push(e.name + ':' + e.message); }});", u(&other, "/x?secret=1"));
        assert_eq!(go(&mut b, &src), "TypeError:Failed to fetch");
        assert_eq!(other.recs().len(), 0);
        assert_eq!(other.conns.load(std::sync::atomic::Ordering::SeqCst), 0);
        let c = console(&mut b);
        assert_eq!(c.len(), 1, "{c:?}");
        assert!(c[0].contains("Request mode is \"same-origin\" but the URL's origin is not the same as the request origin") && !c[0].contains("secret"), "{c:?}");
        // And a redirect out of the origin fails in this mode.
        let op = other.port;
        let page = serve(move |_| reply(302, &[("Location", &format!("http://127.0.0.1:{op}/x"))], b""));
        let mut b = at(page.port);
        assert_eq!(go(&mut b, "var out = []; fetch('/r', {mode:'same-origin'}).then(function () { out.push('ok'); }, function (e) { out.push(e.message); });"), "Failed to fetch");
        assert_eq!(other.recs().len(), 0);
    });
}

#[test]
fn a_cors_error_is_not_a_fatal_stop_and_the_page_goes_on() {
    big(|| {
        let (_, other, mut b) = two(|_| reply(200, &[], b"x"));
        let src = format!("var out = []; fetch('{}').catch(function () {{ out.push('caught'); }}); setTimeout(function () {{ out.push('timer'); }}, 0);", u(&other, "/x"));
        assert_eq!(go(&mut b, &src), "caught");
        b.run_scheduled_tasks(10).unwrap();
        assert_eq!(js(&mut b, "out.join('|')"), "caught|timer");
    });
}

// ------------------------------------------------------------------------------------------------------ XMLHttpRequest

fn xhr(b: &mut Browser, other: &Srv, method: &str, p: &str, setup: &str) -> String {
    let src = format!(
        "var out = []; var x = new XMLHttpRequest(); x.open('{}', '{}'); {setup} x.onload = function () {{ out.push('load', x.status, x.responseText); }}; x.onerror = function () {{ out.push('error', x.status, x.readyState, x.responseText === ''); }}; x.send();",
        method,
        u(other, p)
    );
    go(b, &src)
}

#[test]
fn xhr_follows_the_same_rules_and_a_failure_is_an_error_event_with_status_0() {
    big(|| {
        let (page, other, mut b) = two(|r| match r.path.as_str() {
            "/none" => reply(200, &[], b"x"),
            "/put" => if r.method == "OPTIONS" { preflight_ok(r, "PUT", "x-foo", &[]) } else { allow(r, &[]) },
            "/creds" => reply(200, &[("Access-Control-Allow-Origin", &origin_of(r)), ("Access-Control-Allow-Credentials", "true"), ("X-Hidden", "h"), ("X-Shown", "s"), ("Access-Control-Expose-Headers", "X-Shown")], b"c"),
            _ => allow(r, &[("X-Hidden", "h")]),
        });
        assert_eq!(xhr(&mut b, &other, "GET", "/ok", ""), "load|200|data");
        assert_eq!(origin_of(&other.recs()[0]), format!("http://127.0.0.1:{}", page.port));
        assert_eq!(xhr(&mut b, &other, "GET", "/none", ""), "error|0|4|true");
        let c = console(&mut b);
        assert_eq!(c.len(), 1, "{c:?}");
        assert!(c[0].starts_with("Access to XMLHttpRequest at 'http://127.0.0.1:") && c[0].contains("No 'Access-Control-Allow-Origin' header is present"), "{c:?}");
        // Credentials: `withCredentials` needs the exact origin and the header.
        assert_eq!(xhr(&mut b, &other, "GET", "/ok", "x.withCredentials = true;"), "error|0|4|true");
        assert!(console(&mut b)[0].contains("The value of the 'Access-Control-Allow-Credentials' header in the response is ''"));
        assert_eq!(xhr(&mut b, &other, "GET", "/creds", "x.withCredentials = true;"), "load|200|c");
        // Exposed headers only.
        let src = format!("var out = []; var x = new XMLHttpRequest(); x.open('GET', '{}'); x.onload = function () {{ out.push(x.getResponseHeader('x-hidden'), x.getResponseHeader('x-shown'), x.getResponseHeader('content-type'), x.getAllResponseHeaders().split('\\r\\n').filter(Boolean).length); }}; x.send();", u(&other, "/creds"));
        assert_eq!(go(&mut b, &src), "|s||2");
        // A non-simple method sends a preflight first.
        let before = other.recs().len();
        assert_eq!(xhr(&mut b, &other, "PUT", "/put", "x.setRequestHeader('X-Foo', '1');"), "load|200|data");
        let recs = other.recs();
        assert_eq!(recs[before..].iter().map(|r| r.method.as_str()).collect::<Vec<_>>(), ["OPTIONS", "PUT"]);
        assert_eq!(recs[before].header("access-control-request-headers"), ["x-foo"]);
    });
}
