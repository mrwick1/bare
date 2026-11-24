//! The data of a queued message survives a collection while it waits in the port queue (`browser_scheduler.rs`
//! `sched_roots`, `sched_trace_port`). The data is a rope (a concatenation that is not flattened), so this covers
//! rope roots in the queue: a same-page `postMessage` keeps the sender's own string, unlike a cross-page message
//! which the receiver flattens.

use crate::browser_session::*;
use crate::js_limits::EvalLimits;
use crate::resources::test_server::{Reply, Server, serve};
use std::time::Duration;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

const HTML: &str = "text/html; charset=utf-8";

fn server(routes: impl Fn() -> Vec<(&'static str, &'static str, String)> + Send + Sync + 'static) -> Server {
    serve(move |path| match routes().into_iter().find(|r| r.0 == path) {
        Some((_, ty, body)) => Reply::new(200, Some(ty), body),
        None => Reply::new(404, Some("text/plain"), "no"),
    })
}

fn run(top: &'static str) -> (Session, SessionResult, String) {
    let srv = server(move || vec![("/", HTML, top.into())]);
    let base = srv.url("");
    let mut cfg = SessionConfig::new(&srv.url("/"));
    cfg.virtual_clock = true;
    cfg.eval = EvalLimits { max_steps: 50_000_000, ..Default::default() };
    cfg.page.quiet = Duration::from_millis(100);
    let mut s = Session::new(cfg);
    let r = s.run();
    let log = s.storage().get_item(false, &base, "log").unwrap_or_default();
    (s, r, log)
}

const CHURN: &str = "if (got.length === 0) { var a = []; for (var i = 0; i < 400000; i++) { a.push({i: i, s: 'x' + i}); if (a.length > 1000) a = []; } }";

#[test]
fn queued_window_messages_keep_their_rope_data_through_a_collection() {
    big(|| {
        let top: &'static str = Box::leak(format!(
            "<script>var got = []; window.addEventListener('message', function(e){{ {CHURN}
                got.push(e.data);
                if (got.length === 20) localStorage.setItem('log', got.join(',')); }});
             for (var i = 0; i < 20; i++) window.postMessage('m' + i + '-' + 'tail', '*');</script>"
        ).into_boxed_str());
        let (_s, r, log) = run(top);
        let want: Vec<String> = (0..20).map(|i| format!("m{i}-tail")).collect();
        assert_eq!(log, want.join(","), "{r:?}");
    });
}

#[test]
fn queued_same_page_broadcast_messages_keep_their_rope_data_through_a_collection() {
    big(|| {
        let top: &'static str = Box::leak(format!(
            "<script>var got = []; var a = new BroadcastChannel('x'); var b = new BroadcastChannel('x');
             a.onmessage = function(e){{ {CHURN}
                got.push(e.data);
                if (got.length === 20) localStorage.setItem('log', got.join(',')); }};
             for (var i = 0; i < 20; i++) b.postMessage('m' + i + '-' + 'tail');</script>"
        ).into_boxed_str());
        let (_s, r, log) = run(top);
        let want: Vec<String> = (0..20).map(|i| format!("m{i}-tail")).collect();
        assert_eq!(log, want.join(","), "{r:?}");
    });
}
