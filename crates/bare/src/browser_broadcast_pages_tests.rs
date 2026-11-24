//! Tests of `BroadcastChannel` across pages (`browser_broadcast.rs`): a frame, its parent and a worker of the same origin share the
//! channels of one name. Pages load over loopback HTTP; two servers on two ports are two origins. Time is the virtual clock.

use crate::browser_loader::*;
use crate::browser_session::*;
use crate::js_limits::EvalLimits;
use crate::resources::test_server::{Reply, Server, serve};
use std::time::Duration;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn server(routes: impl Fn() -> Vec<(&'static str, &'static str, String)> + Send + Sync + 'static) -> Server {
    serve(move |path| match routes().into_iter().find(|r| r.0 == path) {
        Some((_, ty, body)) => Reply::new(200, Some(ty), body),
        None => Reply::new(404, Some("text/plain"), "no"),
    })
}

const HTML: &str = "text/html; charset=utf-8";

/// The console lines of the top page and its children, in run order, with the URL prefix of a child cut.
fn lines(url: String) -> Vec<String> {
    let mut c = SessionConfig::new(&url);
    c.virtual_clock = true;
    c.eval = EvalLimits { max_steps: 5_000_000, ..Default::default() };
    c.page.quiet = Duration::from_millis(100);
    let mut s = Session::new(c);
    let r = s.run();
    assert_eq!(r.stop, SessionStop::Page(Stop::Settled), "{r:?}");
    let b = s.browser().unwrap();
    let recs = b.take_console_records();
    recs.iter().map(|r| b.format_console_record(r).unwrap()).map(|l| l.rsplit_once(": ").map_or(l.clone(), |p| p.1.to_string())).collect()
}

const LISTEN: &str = "var bc = new BroadcastChannel('auth'); bc.onmessage = function(e){ console.log('top got ' + e.data.v + ' ' + (e.origin === location.origin) + ' ' + e.source) };";

#[test]
fn a_same_origin_frame_posts_and_closes_and_the_parent_gets_the_message() {
    big(|| {
        let srv = server(|| {
            vec![
                ("/", HTML, format!("<script>{LISTEN}</script><iframe src='/c'></iframe>")),
                ("/c", HTML, "<script>var c = new BroadcastChannel('auth'); c.postMessage({v: 1}); c.close()</script>".into()),
            ]
        });
        assert_eq!(lines(srv.url("/")), ["top got 1 true null"]);
    });
}

#[test]
fn the_parent_and_a_frame_talk_both_ways_in_order() {
    big(|| {
        let srv = server(|| {
            vec![
                ("/", HTML, "<script>var bc = new BroadcastChannel('x'); var n = 0; bc.onmessage = function(e){ console.log('p' + e.data); if (++n < 3) bc.postMessage(n) }</script><iframe src='/c'></iframe>".into()),
                ("/c", HTML, "<script>var c = new BroadcastChannel('x'); c.onmessage = function(e){ console.log('c' + e.data); c.postMessage('r' + e.data) }; c.postMessage('r0')</script>".into()),
            ]
        });
        assert_eq!(lines(srv.url("/")), ["pr0", "c1", "pr1", "c2", "pr2"]);
    });
}

#[test]
fn a_frame_of_another_origin_or_another_name_does_not_get_the_message() {
    big(|| {
        let other = server(|| vec![("/c", HTML, "<script>var c = new BroadcastChannel('auth'); c.postMessage({v: 2}); console.log('sent')</script>".into())]);
        let o = other.url("/c");
        let srv = server(move || {
            vec![
                ("/", HTML, format!("<script>{LISTEN}</script><iframe src='{o}'></iframe><iframe src='/n'></iframe>")),
                ("/n", HTML, "<script>new BroadcastChannel('other').postMessage({v: 3}); console.log('sent n')</script>".into()),
            ]
        });
        assert_eq!(lines(srv.url("/")), ["sent", "sent n"]);
    });
}

#[test]
fn a_worker_of_the_same_origin_gets_and_sends_messages() {
    big(|| {
        let srv = server(|| {
            vec![
                ("/", HTML, "<script>var bc = new BroadcastChannel('w'); bc.onmessage = function(e){ console.log('top got ' + e.data) }; new Worker('/w.js')</script>".into()),
                ("/w.js", "text/javascript", "var c = new BroadcastChannel('w'); c.postMessage('hi from worker')".into()),
            ]
        });
        assert_eq!(lines(srv.url("/")), ["top got hi from worker"]);
    });
}
