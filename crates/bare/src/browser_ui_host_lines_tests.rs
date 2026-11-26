//! Tests of Up, Down, Home and End in an editing host (`contenteditable`) of two paragraphs (`browser_ui.rs`, `browser_range.rs`):
//! the caret moves by line and keeps its column, Shift extends the selection, and typing goes in at the moved caret.

use crate::browser::*;
use crate::eng_new::Peek;
use crate::js_limits::{EvalLimits, HeapLimits};
use crate::page::{Page, Source};
use bare_html::tree;
use bare_window::{Event, Mods};

const NO: Mods = Mods { shift: false, ctrl: false, alt: false, meta: false };
const SHIFT: Mods = Mods { shift: true, ..NO };

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

/// A host with the paragraphs "abcd", "ef" and "ghijk", and the caret at offset 2 of the first.
fn page() -> Page {
    let html = "<!doctype html><div id=h contenteditable style='position:absolute;left:10px;top:10px;width:200px;height:100px'><p>abcd</p><p>ef</p><p>ghijk</p></div>";
    let mut b = Browser::new(tree::parse(html), BrowserLimits::default(), EvalLimits { max_steps: 50_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
    b.set_viewport(400.0, 300.0).unwrap();
    b.set_document_url("http://localhost/p").unwrap();
    let mut p = Page::new(Source::Browser(Box::new(b.into())), (400, 300));
    for pressed in [true, false] {
        p.handle(Event::PointerButton { x: 150.0, y: 20.0, button: 0, buttons: pressed as u16, pressed, mods: NO });
    }
    eval(&mut p, "getSelection().collapse(h.firstChild.firstChild, 2); 0");
    p
}

fn eval(p: &mut Page, src: &str) -> String {
    let b = p.browser().unwrap();
    let v = b.run_script(src).unwrap_or_else(|e| panic!("{src}: {e:?}"));
    b.it.display(v)
}

fn press(p: &mut Page, key: &str, text: Option<&str>, mods: Mods) {
    for pressed in [true, false] {
        p.handle(Event::Key { key: key.into(), code: "", pressed, repeat: false, text: text.filter(|_| pressed).map(str::to_string), mods });
    }
}

/// The paragraph text with `|` at the caret, or `[` and `]` around a selection.
fn show(p: &mut Page) -> String {
    eval(p, "var s = getSelection(), r = s.getRangeAt(0), t = h.textContent, all = []; for (const q of h.children) all.push(q.textContent); \
        var pos = (n, o) => { var u = 0; for (const q of h.children) { if (q.contains(n)) return u + o; u += q.textContent.length + 1 } return -1 }; \
        var a = pos(r.startContainer, r.startOffset), z = pos(r.endContainer, r.endOffset), j = all.join('/'); \
        a == z ? j.slice(0, a) + '|' + j.slice(a) : j.slice(0, a) + '[' + j.slice(a, z) + ']' + j.slice(z)")
}

#[test]
fn up_and_down_move_by_paragraph_and_keep_the_column() {
    big(|| {
        let mut p = page();
        assert_eq!(show(&mut p), "ab|cd/ef/ghijk");
        press(&mut p, "ArrowDown", None, NO);
        assert_eq!(show(&mut p), "abcd/ef|/ghijk", "a shorter line: the end of it");
        press(&mut p, "ArrowDown", None, NO);
        assert_eq!(show(&mut p), "abcd/ef/gh|ijk");
        press(&mut p, "ArrowDown", None, NO);
        assert_eq!(show(&mut p), "abcd/ef/ghijk|", "down on the last line goes to its end");
        press(&mut p, "ArrowUp", None, NO);
        assert_eq!(show(&mut p), "abcd/ef|/ghijk", "up from the end goes to the line above, at the same column");
        press(&mut p, "ArrowUp", None, NO);
        press(&mut p, "ArrowUp", None, NO);
        assert_eq!(show(&mut p), "|abcd/ef/ghijk", "up on the first line goes to its start");
    });
}

#[test]
fn shift_extends_and_typing_goes_in_at_the_moved_caret() {
    big(|| {
        let mut p = page();
        press(&mut p, "ArrowDown", None, SHIFT);
        assert_eq!(show(&mut p), "ab[cd/ef]/ghijk");
        press(&mut p, "ArrowUp", None, SHIFT);
        assert_eq!(show(&mut p), "ab|cd/ef/ghijk");
        press(&mut p, "ArrowDown", None, NO);
        press(&mut p, "X", Some("X"), NO);
        assert_eq!(eval(&mut p, "h.textContent"), "abcdefXghijk");
    });
}

#[test]
fn home_and_end_stop_at_the_ends_of_the_paragraph_of_the_caret() {
    big(|| {
        let mut p = page();
        eval(&mut p, "getSelection().collapse(h.children[2].firstChild, 2); 0");
        press(&mut p, "Home", None, NO);
        assert_eq!(show(&mut p), "abcd/ef/|ghijk");
        press(&mut p, "End", None, NO);
        assert_eq!(show(&mut p), "abcd/ef/ghijk|");
        eval(&mut p, "getSelection().collapse(h.children[1].firstChild, 2); 0");
        press(&mut p, "Home", None, SHIFT);
        assert_eq!(show(&mut p), "abcd/[ef]/ghijk");
    });
}
