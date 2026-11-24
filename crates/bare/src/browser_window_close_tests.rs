//! `window.close()` (HTML Standard, "Browsing the web": the window object). A page that a script did not open cannot close itself:
//! Chrome 155 ignores the call. Microsoft Teams binds `window.close` while it builds its window wrapper; a missing `close` stops its app.

use crate::browser::*;
use crate::eng::Ops;
use crate::eng_new::New;
use bare_html::tree;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

const PAGE: &str = "<!doctype html><html><head></head><body><p id=p>hi</p></body></html>";
const CHECK: &str = "[typeof close, typeof window.close.bind(window), close(), closed, document.getElementById('p').textContent].map(String).join()";
const WANT: &str = "function,function,undefined,false,hi";

#[test]
fn close_is_a_method_that_leaves_the_page_open_on_vex() {
    big(|| {
        let mut b = Browser::<New>::make(tree::parse(PAGE), BrowserLimits::default(), Default::default(), Default::default()).expect("browser");
        let v = b.run_script(CHECK).map(|v| b.it.display_o(v));
        assert_eq!(v.as_deref().ok(), Some(WANT), "{v:?}");
    });
}

#[test]
fn close_is_a_method_that_leaves_the_page_open_on_the_old_engine() {
    big(|| {
        let mut b = Browser::new(tree::parse(PAGE), BrowserLimits::default(), Default::default(), Default::default()).expect("browser");
        let v = b.run_script(CHECK).map(|v| b.it.display_o(v));
        assert_eq!(v.as_deref().ok(), Some(WANT), "{v:?}");
    });
}
