//! `querySelectorAll` gives a list with `item(index)` (Fluent and Griffel call `item` on the style elements of the head in Outlook web).

use crate::eng_new::Peek;
use crate::browser::*;
use crate::eng_new::NewFault as Error;
use crate::js_limits::EvalLimits;
use crate::js_limits::HeapLimits;
use bare_html::tree;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn js(b: &mut Browser, src: &str) -> String {
    match b.run_script(src) {
        Ok(v) => b.it.display(v),
        Err(Error::Throw(v)) if crate::eng_new::is_object(v) => format!("throw:{}:{}", b.it.get_str(v, "name"), b.it.get_str(v, "message")),
        Err(e) => format!("other:{e:?}"),
    }
}

#[test]
fn query_selector_all_has_item() {
    big(|| {
        let html = "<body><p id=a class=c>x</p><span id=b class=c></span></body>";
        let mut b = Browser::new(tree::parse(html), BrowserLimits::default(), EvalLimits { max_steps: 5_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
        assert_eq!(js(&mut b, "var l = document.body.querySelectorAll('.c'); [l.length, l.item(0).id, l.item(1).id, l.item(2), l.item(-1)].join()"), "2,a,b,,");
        assert_eq!(js(&mut b, "document.querySelectorAll('.none').item(0) + ',' + Array.isArray(l) + ',' + l.map(function (e) { return e.id }).join('')"), "null,true,ab");
    });
}
