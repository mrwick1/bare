//! Tests of the text list without a cap: dead text is rebuilt between two tasks once the list is past `TEXT_COMPACT` (`Dom::room`,
//! `browser_reclaim.rs`), and live text past the old 64 MiB cap is allowed.

use crate::browser::*;
use crate::browser_reclaim::TEXT_COMPACT;
use crate::eng_new::Peek;
use bare_html::tree;

const PAGE: &str = "<!doctype html><body><p id=a class=x>hi</p></body>";

fn browser() -> Browser {
    let mut b = Browser::new(tree::parse(PAGE), BrowserLimits::default(), Default::default(), Default::default()).expect("browser");
    b.use_virtual_clock_at(1000.0).unwrap();
    b
}

fn js(b: &mut Browser, src: &str) -> String {
    let r = b.run_script(src).expect(src);
    b.it.display(r)
}

/// Each task replaces 2 MB of text: 100 MB in all. The old text is dead: the list is rebuilt between two tasks once it is past `TEXT_COMPACT`.
#[test]
fn rewrites_in_many_tasks_are_rebuilt_past_the_mark() {
    let mut b = browser();
    js(&mut b, "var a = document.getElementById('a'), n = 0; function step(i) { a.textContent = 'x'.repeat(1_000_000) + i; a.className = 'y'.repeat(1_000_000) + i; n++; } for (var i = 0; i < 50; i++) setTimeout(step.bind(null, i), i);");
    b.advance_time(100.0, 1000).expect("no limit");
    assert_eq!(js(&mut b, "n + ',' + a.textContent.length"), "50,1000002");
    let bytes = b.stats().text_bytes;
    assert!(bytes <= TEXT_COMPACT + TEXT_COMPACT / 4 + (4 << 20), "{bytes} bytes");
}

/// Live text past the old 64 MiB cap, and one string past the old 16 MiB string cap, in one task.
#[test]
fn live_text_past_the_old_caps_is_allowed() {
    let mut b = browser();
    let r = js(&mut b, "var a = document.getElementById('a'); for (var i = 0; i < 5; i++) a.appendChild(document.createTextNode('x'.repeat(14 << 20))); a.textContent.length");
    assert_eq!(r, (2 + 5 * (14 << 20)).to_string());
}
