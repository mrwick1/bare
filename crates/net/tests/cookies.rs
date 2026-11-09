//! The cookie jar in `Client`: `Set-Cookie` in, `Cookie` out, on loopback with several host names (`localhost`, `a.localhost`,
//! `b.localhost`, `127.0.0.1` all reach this machine, and they are different sites or hosts). No public network.

use bare_net::cookie::SameSite;
use bare_net::http::{Client, Cookies, Ctx, Method, Request};
use bare_url::Url;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

/// A server that answers each request with `reply(path)` (the whole HTTP message) and records the `Cookie` header of each request.
struct Server {
    port: u16,
    seen: Arc<Mutex<Vec<(String, Option<String>)>>>,
}

fn serve(reply: impl Fn(&str) -> String + Send + 'static) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen: Arc<Mutex<Vec<(String, Option<String>)>>> = Arc::default();
    let log = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { return };
            let mut buf = [0u8; 8192];
            let mut head = String::new();
            while !head.contains("\r\n\r\n") {
                match s.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => head.push_str(&String::from_utf8_lossy(&buf[..n])),
                }
            }
            let path = head.split(' ').nth(1).unwrap_or("").to_string();
            let cookie = head.lines().find_map(|l| l.strip_prefix("Cookie: ").map(str::to_string));
            log.lock().unwrap().push((path.clone(), cookie));
            let _ = s.write_all(reply(&path).as_bytes());
        }
    });
    Server { port, seen }
}

fn ok(extra: &str) -> String {
    format!("HTTP/1.1 200 OK\r\n{extra}Content-Length: 2\r\nConnection: close\r\n\r\nok")
}

fn redirect(to: &str, extra: &str) -> String {
    format!("HTTP/1.1 302 Found\r\nLocation: {to}\r\n{extra}Content-Length: 0\r\nConnection: close\r\n\r\n")
}

fn url(host: &str, port: u16, path: &str) -> Url {
    Url::parse(&format!("http://{host}:{port}{path}")).unwrap()
}

impl Server {
    /// The `Cookie` header of the last request.
    fn last(&self) -> Option<String> {
        self.seen.lock().unwrap().last().and_then(|(_, c)| c.clone())
    }
    fn cookie_of(&self, path: &str) -> Option<String> {
        self.seen.lock().unwrap().iter().rev().find(|(p, _)| p == path).and_then(|(_, c)| c.clone())
    }
}

/// The reply of a login site: `/login` sets cookies, anything else is plain.
fn site(path: &str) -> String {
    match path {
        "/login" => ok("Set-Cookie: sid=abc; Path=/; HttpOnly\r\nSet-Cookie: theme=dark; Path=/\r\n"),
        "/to-login" => redirect("/login", ""),
        "/set-and-go" => redirect("/after", "Set-Cookie: hop=1\r\n"),
        "/clear" => ok("Set-Cookie: sid=; Max-Age=0; Path=/\r\n"),
        _ => ok(""),
    }
}

#[test]
fn a_cookie_from_a_reply_comes_back_on_the_next_request() {
    let srv = serve(site);
    let mut c = Client::new();
    c.get(&url("a.localhost", srv.port, "/login")).unwrap();
    c.get(&url("a.localhost", srv.port, "/page")).unwrap();
    assert_eq!(srv.cookie_of("/page").as_deref(), Some("sid=abc; theme=dark"));
    // The first request had none.
    assert_eq!(srv.cookie_of("/login"), None);
    // A host-only cookie does not go to another host name.
    c.get(&url("b.localhost", srv.port, "/page")).unwrap();
    assert_eq!(srv.last(), None);
    c.get(&url("127.0.0.1", srv.port, "/page")).unwrap();
    assert_eq!(srv.last(), None);
}

#[test]
fn a_cookie_is_set_on_a_redirect_hop_and_sent_on_the_next() {
    let srv = serve(site);
    let mut c = Client::new();
    c.get(&url("a.localhost", srv.port, "/set-and-go")).unwrap();
    assert_eq!(srv.cookie_of("/after").as_deref(), Some("hop=1"));
}

#[test]
fn a_cookie_set_before_the_redirect_in_the_same_chain_is_sent_at_the_next_hop() {
    let srv = serve(site);
    let mut c = Client::new();
    c.get(&url("a.localhost", srv.port, "/to-login")).unwrap();
    c.get(&url("a.localhost", srv.port, "/x")).unwrap();
    assert_eq!(srv.cookie_of("/x").as_deref(), Some("sid=abc; theme=dark"));
}

#[test]
fn a_fork_and_a_new_page_share_the_jar_but_a_new_client_does_not() {
    let srv = serve(site);
    let mut c = Client::new();
    c.get(&url("a.localhost", srv.port, "/login")).unwrap();
    let mut f = c.fork();
    f.get(&url("a.localhost", srv.port, "/forked")).unwrap();
    assert_eq!(srv.cookie_of("/forked").as_deref(), Some("sid=abc; theme=dark"));
    let mut fresh = Client::new();
    fresh.get(&url("a.localhost", srv.port, "/fresh")).unwrap();
    assert_eq!(srv.cookie_of("/fresh"), None);
}

#[test]
fn max_age_zero_deletes_and_the_clock_can_be_moved() {
    let srv = serve(|p| if p == "/timed" { ok("Set-Cookie: t=1; Max-Age=60\r\n") } else { site(p) });
    let mut c = Client::new();
    c.get(&url("a.localhost", srv.port, "/login")).unwrap();
    c.get(&url("a.localhost", srv.port, "/clear")).unwrap();
    c.get(&url("a.localhost", srv.port, "/p1")).unwrap();
    assert_eq!(srv.cookie_of("/p1").as_deref(), Some("theme=dark"));
    c.get(&url("a.localhost", srv.port, "/timed")).unwrap();
    c.get(&url("a.localhost", srv.port, "/p2")).unwrap();
    assert_eq!(srv.cookie_of("/p2").as_deref(), Some("theme=dark; t=1"));
    c.jar().lock().unwrap().skew_ms = 61_000.0;
    c.get(&url("a.localhost", srv.port, "/p3")).unwrap();
    assert_eq!(srv.cookie_of("/p3").as_deref(), Some("theme=dark"), "`t` ended, the session cookie stays");
}

#[test]
fn secure_is_sent_to_a_loopback_host_over_http() {
    let srv = serve(|p| if p == "/sec" { ok("Set-Cookie: s=1; Secure\r\n") } else { ok("") });
    let mut c = Client::new();
    c.get(&url("a.localhost", srv.port, "/sec")).unwrap();
    c.get(&url("a.localhost", srv.port, "/p")).unwrap();
    assert_eq!(srv.cookie_of("/p").as_deref(), Some("s=1"));
}

#[test]
fn credentials_off_sends_and_stores_nothing() {
    let srv = serve(site);
    let mut c = Client::new();
    let u = url("a.localhost", srv.port, "/login");
    let off = Cookies { on: false, ctx: Ctx::Embedded };
    c.send_once(&Request { method: Method::Get, url: &u, headers: &[], body: &[], cookies: off }, 1 << 20).unwrap();
    c.get(&url("a.localhost", srv.port, "/p")).unwrap();
    assert_eq!(srv.cookie_of("/p"), None, "a reply to a request with credentials off sets no cookie");
    c.get(&u).unwrap();
    let u2 = url("a.localhost", srv.port, "/q");
    c.send_once(&Request { method: Method::Get, url: &u2, headers: &[], body: &[], cookies: off }, 1 << 20).unwrap();
    assert_eq!(srv.cookie_of("/q"), None);
}

#[test]
fn same_site_decides_for_a_subresource() {
    let srv = serve(|p| match p {
        "/set" => ok("Set-Cookie: strict=1; SameSite=Strict\r\nSet-Cookie: lax=1; SameSite=Lax\r\nSet-Cookie: none=1; SameSite=None; Secure\r\nSet-Cookie: plain=1\r\n"),
        _ => ok(""),
    });
    let mut c = Client::new();
    // `localhost` and `a.localhost` are different sites.
    c.get(&url("localhost", srv.port, "/set")).unwrap();
    let mut doc = c.fork();
    doc.set_site(Some("http://localhost".into()));
    doc.get(&url("localhost", srv.port, "/same")).unwrap();
    assert_eq!(srv.cookie_of("/same").as_deref(), Some("strict=1; lax=1; none=1; plain=1"));
    let mut other = c.fork();
    other.set_site(Some("http://b.localhost".into()));
    other.get(&url("localhost", srv.port, "/cross")).unwrap();
    assert_eq!(srv.cookie_of("/cross").as_deref(), Some("none=1"));
}

#[test]
fn a_navigation_from_another_site_sends_lax_and_not_strict() {
    let srv = serve(|p| match p {
        "/set" => ok("Set-Cookie: strict=1; SameSite=Strict\r\nSet-Cookie: lax=1; SameSite=Lax\r\nSet-Cookie: plain=1\r\n"),
        "/bounce" => redirect(&format!("/landing"), ""),
        _ => ok(""),
    });
    let mut c = Client::new();
    c.get(&url("localhost", srv.port, "/set")).unwrap();
    fn nav(from: Option<&str>) -> Cookies<'_> {
        Cookies { on: true, ctx: Ctx::Navigation(from) }
    }
    let u = url("localhost", srv.port, "/typed");
    c.send_once(&Request { method: Method::Get, url: &u, headers: &[], body: &[], cookies: nav(None) }, 1 << 20).unwrap();
    assert_eq!(srv.cookie_of("/typed").as_deref(), Some("strict=1; lax=1; plain=1"), "typed: every cookie");
    let u = url("localhost", srv.port, "/link");
    c.send_once(&Request { method: Method::Get, url: &u, headers: &[], body: &[], cookies: nav(Some("http://b.localhost")) }, 1 << 20).unwrap();
    assert_eq!(srv.cookie_of("/link").as_deref(), Some("lax=1; plain=1"), "a link from another site");
    let u = url("localhost", srv.port, "/form");
    c.send_once(&Request { method: Method::Post, url: &u, headers: &[], body: b"x=1", cookies: nav(Some("http://b.localhost")) }, 1 << 20).unwrap();
    assert_eq!(srv.cookie_of("/form"), None, "a cross-site POST: no Lax cookie");
    let u = url("localhost", srv.port, "/link2");
    c.send_once(&Request { method: Method::Get, url: &u, headers: &[], body: &[], cookies: nav(Some("http://localhost")) }, 1 << 20).unwrap();
    assert_eq!(srv.cookie_of("/link2").as_deref(), Some("strict=1; lax=1; plain=1"), "from the same site");
    // `Client::get` follows the redirect itself and uses the same rule at each hop.
    let u = url("localhost", srv.port, "/bounce");
    c.send(&Request { method: Method::Get, url: &u, headers: &[], body: &[], cookies: nav(Some("http://b.localhost")) }).unwrap();
    assert_eq!(srv.cookie_of("/landing").as_deref(), Some("lax=1; plain=1"));
}

#[test]
fn a_manual_cookie_header_is_kept_and_the_jar_adds_none() {
    let srv = serve(site);
    let mut c = Client::new();
    c.get(&url("a.localhost", srv.port, "/login")).unwrap();
    let u = url("a.localhost", srv.port, "/p");
    c.send(&Request::get(&u)).unwrap();
    assert_eq!(srv.cookie_of("/p").as_deref(), Some("sid=abc; theme=dark"));
    let h = [("Cookie", "mine=1")];
    c.send(&Request { method: Method::Get, url: &u, headers: &h, body: &[], cookies: Cookies::default() }).unwrap();
    assert_eq!(srv.cookie_of("/p").as_deref(), Some("mine=1"));
}

#[test]
fn the_jar_lists_flags_and_a_script_cannot_read_http_only() {
    let srv = serve(site);
    let mut c = Client::new();
    c.get(&url("a.localhost", srv.port, "/login")).unwrap();
    let jar = c.jar();
    let mut j = jar.lock().unwrap();
    let l = j.list();
    let sid = l.iter().find(|c| c.name == "sid").unwrap();
    assert!(sid.http_only && sid.host_only && sid.path == "/" && sid.same_site == SameSite::Unset);
    let now = j.now();
    let at = bare_net::cookie::At { secure: true, host: "a.localhost", path: "/" };
    assert_eq!(j.script_get(&at, now), "theme=dark");
}

#[test]
fn a_worker_job_uses_the_jar_and_the_site_of_the_client_that_spawned_it() {
    let srv = serve(site);
    let a = Client::new();
    // A fork that shares the workers of `a` and has a jar of its own (a new profile that shares the connections).
    let mut b = a.fork();
    b.use_jar(Default::default());
    let (pa, pb) = (srv.port, srv.port);
    // The worker thread is made for `a`, and then it serves `b`.
    a.spawn(move |c| c.get(&url("a.localhost", pa, "/login")).map(|_| ()).map_err(|e| e.to_string())).wait().unwrap().unwrap();
    b.spawn(move |c| c.get(&url("a.localhost", pb, "/from-b")).map(|_| ()).map_err(|e| e.to_string())).wait().unwrap().unwrap();
    assert_eq!(srv.cookie_of("/from-b"), None, "the job of `b` must not see the cookies of `a`");
    a.spawn(move |c| c.get(&url("a.localhost", pa, "/from-a")).map(|_| ()).map_err(|e| e.to_string())).wait().unwrap().unwrap();
    assert_eq!(srv.cookie_of("/from-a").as_deref(), Some("sid=abc; theme=dark"));
    // A job also stores in the jar of its spawner.
    b.spawn(move |c| c.get(&url("a.localhost", pb, "/login")).map(|_| ()).map_err(|e| e.to_string())).wait().unwrap().unwrap();
    assert_eq!(b.jar().lock().unwrap().len(), 2);
    assert_eq!(a.jar().lock().unwrap().len(), 2);
}
