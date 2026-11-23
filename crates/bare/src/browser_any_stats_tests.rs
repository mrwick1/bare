//! `AnyBrowser::stats` on js2. The session_load example reads it after a run with `BARE_JS_EXEC=new`.

use crate::browser::*;
use crate::browser_any::AnyBrowser;
use crate::eng_new::New;
use bare_html::tree;

#[test]
fn stats_work_on_a_js2_browser() {
    std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(|| {
            let b = Browser::<New>::make(tree::parse("<p>hi</p>"), BrowserLimits::default(), Default::default(), Default::default()).expect("browser");
            let b = AnyBrowser::New(b);
            assert!(b.stats().nodes > 0);
        })
        .unwrap()
        .join()
        .unwrap();
}
