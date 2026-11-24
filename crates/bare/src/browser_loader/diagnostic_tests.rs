//! Heap stop labels omit payloads and remain sticky across loader runs.
use crate::eng_new::Peek;
use super::*;
use crate::eng::Fault;
use crate::eng_new::New;
use crate::js_limits::HeapLimits;
use crate::js_limits::EvalLimits;
use std::mem::size_of;

#[test]
fn constructed_heap_stop_starts_no_more_js_or_io() {
    std::thread::Builder::new().stack_size(64 << 20).spawn(|| {
        use crate::resources::test_server::{serve, Reply};
        let srv = serve(|_| Reply::new(200, Some("application/javascript"), "ran = true"));
        let mut b = Browser::new(
            bare_html::tree::parse("<script>var ran = true; fetch('/io')</script><script src='/later.js'></script>"),
            Default::default(), EvalLimits::default(), HeapLimits::default(),
        ).unwrap();
        let Stop::Fatal(fatal) = fatal_of::<New>(Fault::Heap("NeedsHost")) else { panic!("expected fatal") };
        *b.dom.scripts.fatal.borrow_mut() = Some(fatal.clone());
        for _ in 0..3 {
            let r = b.run_page(PageConfig {
                url: srv.url("/page"), client: Client::new(), limits: Default::default(), fetch: None,
            });
            assert_eq!(r.stop, Stop::Fatal(fatal.clone()));
            assert_eq!((r.scripts_run, r.tasks, r.fetches), (0, 0, 0));
            assert_eq!(srv.total(), 0);
            let v = b.run_script("typeof ran").unwrap();
            assert_eq!(b.it.display(v), "undefined");
        }
        assert_eq!(format!("{fatal:?}"), "Heap(\"NeedsHost\")");
    }).unwrap().join().unwrap();
}

#[test]
fn diagnostic_type_sizes() {
    eprintln!("Fatal={} Stop={} Scripts={} PageResult={}", size_of::<Fatal>(), size_of::<Stop>(), size_of::<Scripts<New>>(), size_of::<PageResult>());
}

#[test]
fn public_page_exposes_heap_stop_label() {
    std::thread::Builder::new().stack_size(64 << 20).spawn(|| {
        use crate::browser_session::{Session, SessionConfig};
        use crate::page::{Page, Source};
        use crate::resources::test_server::{serve, Reply};
        let srv = serve(|_| Reply::new(200, Some("text/html"), "<p>ok</p>"));
        let cfg = SessionConfig::new(&srv.url("/"));
        let mut session = Session::new(cfg);
        session.run();
        let Stop::Fatal(fatal) = fatal_of::<New>(Fault::Heap("Stale")) else { panic!("expected fatal") };
        *session.browser().unwrap().dom.scripts.fatal.borrow_mut() = Some(fatal);
        session.resume(&[]);
        let page = Page::new(Source::Session(Box::new(session)), (800, 600));
        assert_eq!(page.stopped().as_deref(), Some("Heap(\"Stale\")"));
    }).unwrap().join().unwrap();
}
