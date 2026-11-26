//! Tests of `Element.prototype.getAnimations` and `Document.prototype.getAnimations` (`shims/animation.js`).

use crate::eng_new::Peek;
use crate::browser::*;
use bare_html::tree;
use crate::js_limits::EvalLimits;
use crate::js_limits::HeapLimits;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

#[test]
fn get_animations_gives_an_empty_list() {
    big(|| {
        let mut b = Browser::new(tree::parse("<body><p id=p>x</p></body>"), BrowserLimits::default(), EvalLimits { max_steps: 5_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
        // T3 Code reads it on a plain element before anything reads `animate` or `Animation`.
        let v = b.run_script("var p = document.getElementById('p'); var a = p.getAnimations(), d = document.getAnimations(); [Array.isArray(a), a.length, Array.isArray(d), d.length, a !== p.getAnimations()].join()").expect("run");
        assert_eq!(b.it.display(v), "true,0,true,0,true");
    });
}
