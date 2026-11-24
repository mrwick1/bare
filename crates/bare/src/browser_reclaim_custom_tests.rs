//! Tests of the reclaim of dead trees on a page that defined custom elements (`browser_reclaim.rs`, `browser_custom.rs`).

use crate::browser::*;
use crate::eng_new::Peek;
use crate::js_limits::{EvalLimits, HeapLimits};
use bare_html::tree;

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn browser() -> Browser {
    let mut b = Browser::new(tree::parse("<!doctype html><body><x-a id=a>hi</x-a></body>"), BrowserLimits::default(), EvalLimits { max_steps: 50_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
    b.use_virtual_clock_at(1000.0).unwrap();
    b
}

fn js(b: &mut Browser, src: &str) -> String {
    let r = b.run_script(src).expect(src);
    b.it.display(r)
}

const DEFINE: &str = "var log = []; customElements.define('x-a', class extends HTMLElement { connectedCallback() { log.push('c' + this.id); } disconnectedCallback() { log.push('d' + this.id); } });";

/// A page that rewrites the text of a style element: each write leaves the old text node in a dead tree. The trees go back to the arena also
/// after a `customElements.define`.
#[test]
fn dead_trees_go_back_to_the_arena_on_a_page_with_custom_elements() {
    big(|| {
        let mut b = browser();
        js(&mut b, DEFINE);
        js(&mut b, "var st = document.head.appendChild(document.createElement('style')); var css = ''; function step(i) { css += '.c' + i + ' { height: ' + i + 'px; }\\n' + 'x'.repeat(2000); st.textContent = css; } for (var i = 0; i < 400; i++) setTimeout(step.bind(null, i), i);");
        for _ in 0..20 {
            b.advance_time(25.0, 1000).expect("advance");
            b.collect(&[]).expect("collect");
        }
        let (runs, trees) = b.reclaim_stats();
        assert!(runs >= 1 && trees >= 100, "runs {runs} trees {trees}");
        let live = b.document().borrow().live_text_bytes();
        assert!(live < 8 << 20, "{live} bytes of text in use: about 400 * 800 KB were written, less than 1 MB is alive");
        assert_eq!(js(&mut b, "st.textContent.length == css.length && log.join()"), "ca");
    });
}

#[test]
fn custom_elements_and_their_state_survive_a_reclaim_with_poison() {
    big(|| {
        let mut b = browser();
        b.set_reclaim_stress(true);
        js(&mut b, DEFINE);
        js(&mut b, "customElements.define('x-b', class extends HTMLButtonElement {}, { extends: 'button' }); var held = []; function work(i) { var e = document.createElement('x-a'); e.id = 'n' + i; document.body.appendChild(e); e.remove(); var f = document.createElement('button', { is: 'x-b' }); if (i % 5 == 0) held.push(f); var d = document.createElement('div'); d.innerHTML = '<x-a id=m' + i + '></x-a>'; if (i % 7 == 0) document.body.appendChild(d); } for (var i = 0; i < 40; i++) setTimeout(work.bind(null, i), i);");
        for _ in 0..60 {
            b.advance_time(1.0, 50).expect("advance");
            b.collect(&[]).expect("collect");
        }
        assert!(b.reclaim_stats().0 >= 1, "a reclaim ran");
        assert_eq!(js(&mut b, "held.length + ',' + held.every(f => f instanceof HTMLButtonElement && f.constructor !== HTMLButtonElement) + ',' + document.querySelectorAll('body > div > x-a').length + ',' + log.filter(s => s[0] == 'c').length"), "8,true,6,47");
    });
}
