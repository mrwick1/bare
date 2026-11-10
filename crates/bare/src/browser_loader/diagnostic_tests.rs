//! Heap stop labels omit payloads and remain sticky across loader runs.
use super::*;
use bare_js::heap::{HeapError, HeapLimits};
use bare_js::eval::EvalLimits;
use std::mem::size_of;

#[test]
fn heap_variant_labels_and_public_stop_text() {
    use HeapError::*;
    let owner = bare_js::heap::Heap::default().new_object(None).unwrap();
    let cases = [
        (Stale, "Stale"), (Limit(bare_js::heap::Limit::Objects), "Limit"),
        (InvalidKey, "InvalidKey"), (InvalidDescriptor, "InvalidDescriptor"),
        (InvalidArrayLength, "InvalidArrayLength"), (SymbolConversion, "SymbolConversion"),
        (SymbolToNumber, "SymbolToNumber"), (BigIntToNumber, "BigIntToNumber"),
        (NeedsExecution("private source"), "NeedsExecution"), (NeedsBigInt, "NeedsBigInt"),
        (BigUnsigned(u64::MAX), "BigUnsigned"), (BigElement, "BigElement"),
        (NeedsHost { owner, index: 987654 }, "NeedsHost"), (HostUnavailable, "HostUnavailable"),
        (NeedsNamed, "NeedsNamed"), (Proxy(owner), "Proxy"),
    ];
    for (error, name) in cases {
        let Stop::Fatal(fatal) = fatal_of(Error::Heap(error)) else { panic!("expected fatal") };
        assert_eq!(format!("page stopped: {fatal:?}"), format!("page stopped: Heap({name:?})"));
    }
}

#[test]
fn constructed_heap_stop_starts_no_more_js_or_io() {
    std::thread::Builder::new().stack_size(64 << 20).spawn(|| {
        use crate::resources::test_server::{serve, Reply};
        let srv = serve(|_| Reply::new(200, Some("application/javascript"), "ran = true"));
        let mut b = Browser::new(
            bare_html::tree::parse("<script>var ran = true; fetch('/io')</script><script src='/later.js'></script>"),
            Default::default(), EvalLimits::default(), HeapLimits::default(),
        ).unwrap();
        let Stop::Fatal(fatal) = fatal_of(Error::Heap(HeapError::NeedsHost {
            owner: bare_js::heap::Heap::default().new_object(None).unwrap(), index: 987654,
        })) else { panic!("expected fatal") };
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
    eprintln!("Fatal={} Stop={} Scripts={} PageResult={}", size_of::<Fatal>(), size_of::<Stop>(), size_of::<Scripts>(), size_of::<PageResult>());
}

#[test]
fn public_page_exposes_heap_stop_label() {
    std::thread::Builder::new().stack_size(64 << 20).spawn(|| {
        use crate::browser_session::{Session, SessionConfig};
        use crate::page::{Page, Source};
        use crate::resources::test_server::{serve, Reply};
        let srv = serve(|_| Reply::new(200, Some("text/html"), "<p>ok</p>"));
        let mut session = Session::new(SessionConfig::new(&srv.url("/")));
        session.run();
        let Stop::Fatal(fatal) = fatal_of(Error::Heap(HeapError::Stale)) else { panic!("expected fatal") };
        *session.browser().unwrap().dom.scripts.fatal.borrow_mut() = Some(fatal);
        session.resume(&[]);
        let page = Page::new(Source::Session(Box::new(session)), (800, 600));
        assert_eq!(page.stopped().as_deref(), Some("Heap(\"Stale\")"));
    }).unwrap().join().unwrap();
}
