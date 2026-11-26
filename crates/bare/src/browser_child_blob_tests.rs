//! Tests of iframes whose document is a `blob:` URL of the parent (`browser_child.rs`). The pages load over loopback HTTP from a
//! throwaway server of this process. The blob document has the URL of the parent (a document URL is http(s) only), so it writes to
//! the same `localStorage`.

use crate::browser_loader::*;
use crate::browser_session::*;
use crate::resources::test_server::{Reply, Server, serve};
use crate::js_limits::EvalLimits;
use std::time::Duration;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn run(page: &'static str) -> (Session, SessionResult, Server) {
    let srv = serve(move |path| match path {
        "/" => Reply::new(200, Some("text/html; charset=utf-8"), page.to_string()),
        _ => Reply::new(404, Some("text/plain"), "no"),
    });
    let mut c = SessionConfig::new(&srv.url("/"));
    c.virtual_clock = true;
    c.eval = EvalLimits { max_steps: 5_000_000, ..Default::default() };
    c.page.quiet = Duration::from_millis(100);
    let mut s = Session::new(c);
    let r = s.run();
    (s, r, srv)
}

fn get(s: &Session, srv: &Server, key: &str) -> Option<String> {
    s.storage().get_item(false, &srv.url(""), key)
}

const BLOB: &str = "function doc(k){return URL.createObjectURL(new Blob(['<p>hi</p><script>localStorage.setItem(\"'+k+'\", location.pathname + document.querySelector(\"p\").textContent)<\\/script>'], {type: 'text/html'}))}";

#[test]
fn an_iframe_with_a_blob_src_loads_the_blob_as_its_document() {
    big(|| {
        let (s, r, srv) = run(Box::leak(format!("<script>{BLOB}; var f = document.createElement('iframe'); f.src = doc('plain'); document.body.appendChild(f); var g = document.createElement('iframe'); g.setAttribute('sandbox', ''); g.src = doc('sandboxed'); document.body.appendChild(g)</script>").into_boxed_str()));
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
        assert_eq!(get(&s, &srv, "plain").as_deref(), Some("/hi"));
        assert_eq!(get(&s, &srv, "sandboxed").as_deref(), Some("/hi"));
    });
}

#[test]
fn the_location_setter_of_a_frame_window_loads_a_blob_url() {
    big(|| {
        let (s, r, srv) = run(Box::leak(format!("<script>{BLOB}; var f = document.createElement('iframe'); f.src = doc('first'); document.body.appendChild(f); setTimeout(function(){{ f.contentWindow.location = doc('nav') }}, 30)</script>").into_boxed_str()));
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
        assert_eq!((get(&s, &srv, "first").as_deref(), get(&s, &srv, "nav").as_deref()), (Some("/hi"), Some("/hi")));
    });
}

#[test]
fn a_revoked_blob_src_leaves_the_frame_empty() {
    big(|| {
        let (s, r, srv) = run(Box::leak(format!("<script>{BLOB}; var u = doc('gone'); URL.revokeObjectURL(u); var f = document.createElement('iframe'); f.src = u; document.body.appendChild(f)</script>").into_boxed_str()));
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
        assert_eq!(get(&s, &srv, "gone"), None);
    });
}
