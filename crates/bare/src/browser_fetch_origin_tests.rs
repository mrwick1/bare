//! The `Origin` header of a same-origin request (`browser_fetch.rs`). The Fetch Standard ("append a request `Origin` header") sends it
//! on every request whose method is not GET or HEAD, also to the same origin. Chrome does the same. Teams sets its image cookie with
//! a same-origin `POST`, and the server answers 401 when the header is missing.

use crate::browser_fetch_tests::*;

#[test]
fn a_same_origin_post_from_fetch_and_xhr_sends_the_origin_of_the_page() {
    big(|| {
        let page = serve(|_| ok("{}"));
        let mut b = at(page.port);
        let src = "var out = []; fetch('/f', {method:'POST'}).then(function (r) { out.push(r.status); var x = new XMLHttpRequest(); x.open('POST', '/x'); x.onload = function () { out.push(x.status); }; x.send(); });";
        assert_eq!(go(&mut b, src), "200|200");
        let recs = page.recs();
        let want = format!("http://127.0.0.1:{}", page.port);
        let posts: Vec<_> = recs.iter().filter(|r| r.path == "/f" || r.path == "/x").collect();
        assert_eq!(posts.len(), 2);
        for r in posts {
            assert_eq!(r.header("origin"), vec![want.clone()], "{}", r.path);
        }
    });
}

#[test]
fn a_same_origin_get_sends_no_origin() {
    big(|| {
        let page = serve(|_| ok("{}"));
        let mut b = at(page.port);
        assert_eq!(go(&mut b, "var out = []; fetch('/g').then(function (r) { out.push(r.status); });"), "200");
        let recs = page.recs();
        let get = recs.iter().find(|r| r.path == "/g").expect("the GET");
        assert!(get.header("origin").is_empty());
    });
}
