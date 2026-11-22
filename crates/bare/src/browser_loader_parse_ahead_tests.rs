use super::*;
use crate::browser_any::AnyBrowser;
use crate::browser_session::{Session, SessionConfig};
use crate::page::{Page, Source};
use crate::resources::test_server::{Reply, serve};

#[test]
fn live_sessions_queue_classic_parses_only_for_old() {
    std::thread::Builder::new().stack_size(64 << 20).spawn(|| {
        for js2 in [false, true] {
            let body = format!("/* {} */\nvar speculativeParse = {js2};", "x".repeat(bare_js::parse_ahead::MIN_AHEAD));
            let server = serve(move |path| match path {
                "/" => Reply::new(200, Some("text/html"), "<link rel=preload as=script href=/classic.js><p>Page</p>"),
                "/classic.js" => Reply::js(body.clone()),
                _ => Reply::new(404, None, ""),
            });
            let mut cfg = SessionConfig::new(&server.url("/"));
            cfg.js2 = js2;
            cfg.live = true;
            let mut session = Session::new(cfg);
            session.run();
            let mut page = Page::new(Source::Session(Box::new(session)), (320, 240));
            page.wait_load(Duration::from_secs(5)).unwrap();
            let mut net = match page.browser().unwrap() {
                AnyBrowser::Old(b) => b.dom.scripts.net.borrow_mut(),
                AnyBrowser::New(b) => b.dom.scripts.net.borrow_mut(),
            };
            let net = net.as_deref_mut().unwrap();
            let url = Url::parse(&server.url("/classic.js")).unwrap();
            let expect = Expect::Script;
            assert!(net.started.contains_key(&Net::key(expect, &url)));
            // Wait for the scanned download to submit its parse. No script consumes or cancels the slot.
            let script = net.fetch(&resources::Target { expect, url }, &Limits::default(), deadline_after(Duration::from_secs(5)), 0).unwrap();
            assert!(script.text.contains("var speculativeParse"));
            let ahead = net.ahead.as_ref();
            assert_eq!(ahead.is_some_and(|a| a.running() || a.began() > 0), !js2, "js2={js2}");
        }
    }).unwrap().join().unwrap();
}
