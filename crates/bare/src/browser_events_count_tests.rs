//! Event objects have no count cap. A page may hold any number of them, as in Chrome. The heap limits bound their memory.

use crate::browser::*;
use crate::eng_new::Peek;
use crate::js_limits::EvalLimits;
use crate::js_limits::HeapLimits;
use bare_html::tree;

#[test]
fn a_page_holds_many_live_events() {
    std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(|| {
            let mut b = Browser::new(tree::parse("<body></body>"), BrowserLimits::default(), EvalLimits { max_steps: 50_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
            // One task makes and keeps 40,000 events, more than the old cap of 16,384.
            let v = b.run_script("var evs = []; for (var i = 0; i < 40000; i++) evs.push(new Event('x' + (i % 7))); evs.length + ':' + evs[39999].type").expect("script");
            assert_eq!(b.it.display(v), "40000:x1");
            b.collect(&[]).unwrap();
            assert!(b.stats().events >= 40000, "the live events stay");
            let v = b.run_script("evs = null; 0").unwrap();
            assert_eq!(b.it.display(v), "0");
            b.collect(&[]).unwrap();
            assert!(b.stats().events < 100, "a collect frees the dead events: {}", b.stats().events);
        })
        .unwrap()
        .join()
        .unwrap();
}
