//! The `Referer` header of a subresource request (`resources::referrer`). The rule is Chrome's default referrer policy,
//! `strict-origin-when-cross-origin`: the full document URL to the same origin, only the origin to another origin, and nothing from
//! https to http. Teams answers 401 to its avatar pictures (`profilepicturev2`) when the header is missing.

use crate::browser_fetch_tests::*;
use crate::resources::{self, referrer, Expect, Limits, Target};
use bare_url::Url;
use std::sync::atomic::AtomicUsize;
use std::time::{Duration, Instant};

fn u(s: &str) -> Url {
    Url::parse(s).unwrap()
}

#[test]
fn the_default_policy_gives_the_url_the_origin_or_nothing() {
    let doc = u("https://a.example/app/page?q=1");
    assert_eq!(referrer(&doc, &u("https://a.example/img.png")).as_deref(), Some("https://a.example/app/page?q=1"));
    assert_eq!(referrer(&doc, &u("https://b.example/x")).as_deref(), Some("https://a.example/"));
    assert_eq!(referrer(&u("https://a.example:8443/p"), &u("https://b.example/x")).as_deref(), Some("https://a.example:8443/"));
    assert_eq!(referrer(&doc, &u("http://a.example/x")), None, "https to http");
    assert_eq!(referrer(&u("http://a.example/p"), &u("http://b.example/x")).as_deref(), Some("http://a.example/"));
    let long = u(&format!("https://a.example/{}", "a".repeat(4100)));
    assert_eq!(referrer(&long, &u("https://a.example/x")).as_deref(), Some("https://a.example/"), "a URL over 4096 bytes is cut to its origin");
}

#[test]
fn fetch_and_xhr_send_the_page_url_to_the_same_origin_and_the_origin_to_another() {
    big(|| {
        let other = serve(|_| reply(200, &[("Access-Control-Allow-Origin", "*")], b"{}"));
        let page = serve(|_| ok("{}"));
        let mut b = at(page.port);
        let src = format!("var out = []; fetch('/f').then(function (r) {{ out.push(r.status); var x = new XMLHttpRequest(); x.open('GET', '/x'); x.onload = function () {{ out.push(x.status); fetch('http://localhost:{}/c').then(function (r) {{ out.push(r.status); }}); }}; x.send(); }});", other.port);
        assert_eq!(go(&mut b, &src), "200|200|200");
        let full = format!("http://127.0.0.1:{}/app/login", page.port);
        for path in ["/f", "/x"] {
            let r = page.recs().into_iter().find(|r| r.path == path).expect(path);
            assert_eq!(r.header("referer"), vec![full.clone()], "{path}");
        }
        let c = other.recs().into_iter().find(|r| r.path == "/c").expect("/c");
        assert_eq!(c.header("referer"), vec![format!("http://127.0.0.1:{}/", page.port)]);
    });
}

#[test]
fn a_script_and_a_picture_send_the_url_of_the_document() {
    let srv = serve(|r| if r.path == "/s.js" { reply(200, &[("Content-Type", "text/javascript")], b"1") } else { reply(200, &[("Content-Type", "image/png")], b"x") });
    let doc = u(&format!("http://127.0.0.1:{}/page", srv.port));
    let target = Target { expect: Expect::Script, url: u(&format!("http://127.0.0.1:{}/s.js", srv.port)) };
    resources::fetch(&mut bare_net::http::Client::new(), &target, &Limits::default(), Instant::now() + Duration::from_secs(5), 0, Some(&doc)).unwrap();
    let _ = crate::images::load(&mut bare_net::http::Client::new(), &format!("http://127.0.0.1:{}/i.png", srv.port), &AtomicUsize::new(0), Some(&doc));
    for path in ["/s.js", "/i.png"] {
        let r = srv.recs().into_iter().find(|r| r.path == path).expect(path);
        assert_eq!(r.header("referer"), vec![doc.to_string()], "{path}");
    }
}
