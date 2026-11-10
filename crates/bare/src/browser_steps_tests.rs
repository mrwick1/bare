//! Indexed style computations charge the work they perform.
use crate::browser::{Browser, BrowserLimits, Survive};
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

#[test]
fn steps_abort_names_the_innermost_function() {
    std::thread::Builder::new().stack_size(64 << 20).spawn(|| {
        let mut b = Browser::new(bare_html::tree::parse("<body></body>"), BrowserLimits::default(), EvalLimits { max_steps: 100_000, ..Default::default() }, HeapLimits::default()).unwrap();
        let e = b.run_script("function spin() { while (true) {} }\nspin();").unwrap_err();
        assert!(matches!(b.survive(e, "inline script"), Survive::Aborted));
        let records = b.take_console_records();
        let text = b.format_console_record(&records[0]).unwrap();
        assert!(text.contains("Steps budget ended the inline script at spin (script#"), "{text}");
        assert!(text.contains(":1:1)"), "{text}");
        let v = b.run_script("6 * 7").unwrap();
        assert_eq!(b.it.display(v), "42");
    }).unwrap().join().unwrap();
}
