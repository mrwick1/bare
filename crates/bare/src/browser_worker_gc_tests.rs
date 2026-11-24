//! Tests of the collector and the messages of a dedicated worker (`browser_child.rs`, `browser.rs` trace of `Owner::Worker`).
//! A message that waits in the queue of a `Worker` object keeps its data through a collection.

use crate::browser_loader::*;
use crate::browser_session::*;
use crate::resources::test_server::{Reply, Server, serve};
use crate::js_limits::EvalLimits;
use std::time::Duration;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn server(routes: Vec<(&'static str, &'static str)>) -> Server {
    serve(move |path| match routes.iter().find(|r| r.0 == path) {
        Some((p, body)) if p.ends_with(".js") => Reply::new(200, Some("text/javascript"), body.to_string()),
        Some((_, body)) => Reply::new(200, Some("text/html; charset=utf-8"), body.to_string()),
        None => Reply::new(404, Some("text/plain"), "no"),
    })
}

/// The worker posts 20 messages at once. The first `message` handler makes much garbage, so a collection runs while 19 messages wait.
#[test]
fn queued_worker_messages_keep_their_data_through_a_collection() {
    big(|| {
        let page = "<script>var got = []; var w = new Worker('/w.js'); w.onmessage = function(e){
            if (got.length === 0) { var a = []; for (var i = 0; i < 400000; i++) { a.push({i: i, s: 'x' + i}); if (a.length > 1000) a = []; } }
            got.push(e.data.s + e.data.n.length);
            if (got.length === 20) localStorage.setItem('log', got.join(','));
        };</script>";
        let srv = server(vec![
            ("/", page),
            ("/w.js", "for (var i = 0; i < 20; i++) postMessage({s: 'm' + i, n: new Array(i + 1).fill(i)});"),
        ]);
        let mut c = SessionConfig::new(&srv.url("/"));
        c.virtual_clock = true;
        c.eval = EvalLimits { max_steps: 50_000_000, ..Default::default() };
        c.page.quiet = Duration::from_millis(100);
        let mut s = Session::new(c);
        let r = s.run();
        let want: Vec<String> = (0..20).map(|i| format!("m{i}{}", i + 1)).collect();
        assert_eq!(s.storage().get_item(false, &srv.url(""), "log").as_deref(), Some(want.join(",").as_str()), "{r:?}");
        assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
    });
}
