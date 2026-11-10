//! Indexed style computations charge the work they perform.
use crate::browser::{Browser, BrowserLimits};
use bare_js::{eval::EvalLimits, heap::HeapLimits};

#[test]
fn indexed_style_reads_charge_the_candidates_they_test() {
    std::thread::Builder::new().stack_size(64 << 20).spawn(|| {
        let mut html = String::from("<style>");
        for i in 0..1000 { html.push_str(&format!(".unused{i} {{ color: red }}")); }
        html.push_str("</style><body>");
        for _ in 0..200 { html.push_str("<p></p>"); }
        let mut b = Browser::new(bare_html::tree::parse(&html), BrowserLimits::default(), EvalLimits { max_steps: 200_000, ..Default::default() }, HeapLimits::default()).unwrap();
        let v = b.run_script("var nodes = document.querySelectorAll('p'); for (var i=0; i<10; i++) { document.head.appendChild(document.createElement('style')).textContent = '.more' + i + '{color:blue}'; getComputedStyle(nodes[i]).color; } 'done'").expect("indexed rules fit the task budget");
        assert_eq!(b.it.display(v), "done");
    }).unwrap().join().unwrap();
}
