//! Many listeners on one target. A browser has no cap for each target. The Teams data worker puts more than 1000 listeners on its
//! global (510 `message`, 509 `beforeinstallprompt`, 2026-11-25). Only the total cap (`max_listeners`) stays.

use crate::browser::*;
use crate::eng_new::NewFault as Error;
use crate::eng_new::Peek;
use crate::js_limits::{EvalLimits, HeapLimits};
use bare_html::tree;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

#[test]
fn two_thousand_listeners_on_one_target_are_added_and_all_run() {
    big(|| {
        let mut b = Browser::new(tree::parse("<!doctype html><body>"), BrowserLimits::default(), EvalLimits::default(), HeapLimits::default()).expect("browser");
        let r = b.run_script("var n = 0; for (var i = 0; i < 2000; i++) { addEventListener('x', function () { n++ }); } dispatchEvent(new Event('x')); n");
        match r {
            Ok(v) => assert_eq!(b.it.display(v), "2000"),
            Err(Error::Limit(l)) => panic!("limit: {l:?}"),
            Err(e) => panic!("{e:?}"),
        }
    });
}
