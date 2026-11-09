//! Tests of the HTTP cookie jar through `fetch`, XHR, `document.cookie` and the document session. Every server is a loopback server of this
//! process; `127.0.0.1` and `localhost` are two origins and two sites. No public network.

use crate::browser::*;
use crate::browser_fetch_tests::{at, big, go, js, ok, reply, serve, Srv};
use crate::browser_loader::*;
use crate::browser_session::*;
use bare_net::cookie::{At, Via};
use std::time::Duration;

fn cookie(s: &Srv, path: &str) -> Option<String> {
    s.recs().iter().rev().find(|r| r.path == path).and_then(|r| r.header("cookie").into_iter().next())
}

/// The document `http://127.0.0.1:PORT/app/login` of `fetch` tests, with a document URL for `document.cookie`.
fn doc(port: u16) -> Browser {
    let mut b = at(port);
    b.set_document_url(&format!("http://127.0.0.1:{port}/app/login")).unwrap();
    b
}

fn seed(b: &Browser, host: &str, text: &str) {
    let jar = b.storage().jar();
    let mut j = jar.lock().unwrap();
    let now = j.now();
    j.set(&At { secure: true, host, path: "/" }, text, Via::Http { third: false }, now);
}

/// The server of another origin: it allows `origin` with credentials, sets a cookie on `/set` and reports on `/echo`.
fn other(origin: String) -> Srv {
    serve(move |r| {
        let h = [("Content-Type", "text/plain"), ("Access-Control-Allow-Origin", origin.as_str()), ("Access-Control-Allow-Credentials", "true")];
        match r.path.as_str() {
            "/set" => reply(200, &[h[0], h[1], h[2], ("Set-Cookie", "third=1; Path=/")], b"set"),
            _ => reply(200, &h, b"echo"),
        }
    })
}

fn login_server() -> Srv {
    serve(|r| match r.path.as_str() {
        "/app/login" => reply(200, &[("Content-Type", "application/json"), ("Set-Cookie", "sid=abc; Path=/; HttpOnly"), ("Set-Cookie", "vis=1; Path=/")], b"{}"),
        _ => ok("{}"),
    })
}

#[test]
fn fetch_stores_a_cookie_and_sends_it_and_the_script_never_sees_http_only() {
    big(|| {
        let srv = login_server();
        let mut b = doc(srv.port);
        go(&mut b, "var out = []; fetch('/app/login').then(function () { return fetch('/api/next'); });");
        assert_eq!(cookie(&srv, "/app/login"), None);
        assert_eq!(cookie(&srv, "/api/next").as_deref(), Some("sid=abc; vis=1"));
        assert_eq!(js(&mut b, "document.cookie"), "vis=1");
        // A cookie that the script makes goes out with the next request.
        js(&mut b, "document.cookie = 'mine=2; Path=/'");
        go(&mut b, "var out = []; fetch('/api/again');");
        assert_eq!(cookie(&srv, "/api/again").as_deref(), Some("sid=abc; vis=1; mine=2"));
        // The script cannot make an HttpOnly cookie or replace one.
        js(&mut b, "document.cookie = 'sid=evil'; document.cookie = 'h=1; HttpOnly'");
        go(&mut b, "var out = []; fetch('/api/last');");
        assert_eq!(cookie(&srv, "/api/last").as_deref(), Some("sid=abc; vis=1; mine=2"));
        assert_eq!(js(&mut b, "navigator.cookieEnabled"), "true");
    });
}

#[test]
fn credentials_omit_sends_and_stores_nothing_and_same_origin_is_the_default() {
    big(|| {
        let srv = login_server();
        let mut b = doc(srv.port);
        go(&mut b, "var out = []; fetch('/app/login', {credentials: 'omit'}).then(function () { return fetch('/api/n1'); });");
        assert_eq!(cookie(&srv, "/api/n1"), None, "a reply to `omit` sets no cookie");
        go(&mut b, "var out = []; fetch('/app/login').then(function () { return fetch('/api/n2', {credentials: 'omit'}); }).then(function () { return fetch('/api/n3', {credentials: 'same-origin'}); });");
        assert_eq!(cookie(&srv, "/api/n2"), None);
        assert_eq!(cookie(&srv, "/api/n3").as_deref(), Some("sid=abc; vis=1"));
    });
}

#[test]
fn a_cross_origin_request_sends_cookies_only_with_include_and_its_reply_sets_them_only_then() {
    big(|| {
        let page = serve(|_| ok("{}"));
        let o = other(format!("http://127.0.0.1:{}", page.port));
        let mut b = doc(page.port);
        let u = |p: &str| format!("http://localhost:{}{p}", o.port);
        seed(&b, "localhost", "seeded=1; Path=/; SameSite=None; Secure");
        // The default (`same-origin`): no cookie goes to another origin, and its reply sets none.
        go(&mut b, &format!("var out = []; fetch('{}').then(function () {{ return fetch('{}'); }});", u("/set"), u("/plain")));
        assert_eq!(cookie(&o, "/set"), None);
        assert_eq!(cookie(&o, "/plain"), None, "the reply to a request with no credentials set no cookie");
        // `include`: the cookie goes, and the reply sets one.
        go(&mut b, &format!("var out = []; fetch('{}', {{credentials: 'include'}}).then(function () {{ return fetch('{}', {{credentials: 'include'}}); }});", u("/set"), u("/echo")));
        assert_eq!(cookie(&o, "/set").as_deref(), Some("seeded=1"));
        // `third=1` has no `SameSite`: it is Lax by default, so a cross-site response may not set it (only `SameSite=None; Secure` may).
        assert_eq!(cookie(&o, "/echo").as_deref(), Some("seeded=1"));
        // XHR with `withCredentials`.
        go(&mut b, &format!("var out = []; var x = new XMLHttpRequest(); x.open('GET', '{}'); x.withCredentials = true; x.onload = function () {{ out.push(x.status); }}; x.send();", u("/xhr")));
        assert_eq!(cookie(&o, "/xhr").as_deref(), Some("seeded=1"));
        go(&mut b, &format!("var out = []; var x = new XMLHttpRequest(); x.open('GET', '{}'); x.onload = function () {{ out.push(x.status); }}; x.send();", u("/xhr2")));
        assert_eq!(cookie(&o, "/xhr2"), None);
    });
}

#[test]
fn a_cookie_set_by_a_navigation_reply_lasts_for_the_next_document_and_the_script_reads_the_visible_ones() {
    big(|| {
        let srv = serve(|r| match r.path.as_str() {
            "/" => reply(200, &[("Content-Type", "text/html"), ("Set-Cookie", "sid=abc; HttpOnly"), ("Set-Cookie", "vis=2")], b"<script>location.assign('/second')</script>"),
            "/second" => reply(200, &[("Content-Type", "text/html")], b"<script>localStorage.setItem('seen', document.cookie)</script>"),
            _ => reply(404, &[], b""),
        });
        let mut c = SessionConfig::new(&format!("http://127.0.0.1:{}/", srv.port));
        c.virtual_clock = true;
        c.page.quiet = Duration::from_millis(100);
        let mut s = Session::new(c);
        let r = s.run();
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
        assert_eq!(cookie(&srv, "/second").as_deref(), Some("sid=abc; vis=2"));
        assert_eq!(s.storage().get_item(false, &format!("http://127.0.0.1:{}", srv.port), "seen").as_deref(), Some("vis=2"));
        // A new session of the same storage keeps the cookies; a session with a new storage does not.
        let mut c = SessionConfig::new(&format!("http://127.0.0.1:{}/second", srv.port));
        c.storage = Some(s.storage());
        c.virtual_clock = true;
        Session::new(c).run();
        assert_eq!(cookie(&srv, "/second").as_deref(), Some("sid=abc; vis=2"));
        let mut c = SessionConfig::new(&format!("http://127.0.0.1:{}/second", srv.port));
        c.virtual_clock = true;
        Session::new(c).run();
        assert_eq!(cookie(&srv, "/second"), None);
    });
}

#[test]
fn a_cookie_is_not_put_in_a_diagnostic_or_a_console_message() {
    big(|| {
        let srv = login_server();
        let mut b = doc(srv.port);
        go(&mut b, "var out = []; fetch('/app/login').then(function () { return fetch('/api/next'); });");
        let text = format!("{:?}{:?}", b.take_fetch_diagnostics(), b.take_console_records());
        assert!(!text.contains("abc") && !text.contains("sid="), "{text}");
    });
}
