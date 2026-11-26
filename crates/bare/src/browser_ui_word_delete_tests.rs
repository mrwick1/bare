//! Tests of Control+Backspace and Control+Delete (`browser_ui.rs`): they delete a word in a field, a textarea and an editing host, by the
//! same word boundaries as Control+Left and Control+Right. `beforeinput` has `deleteWordBackward` or `deleteWordForward`.

use crate::browser::*;
use crate::eng_new::Peek;
use crate::js_limits::{EvalLimits, HeapLimits};
use crate::page::{Page, Source};
use bare_html::tree;
use bare_window::{Event, Mods};

const NO: Mods = Mods { shift: false, ctrl: false, alt: false, meta: false };
const CTRL: Mods = Mods { ctrl: true, ..NO };
const BOX: &str = "position:absolute;left:10px;width:200px;height:40px;font:16px monospace";

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

fn page() -> Page {
    let html = format!("<!doctype html><input id=i style='{BOX};top:0' value='foo bar baz'><textarea id=t style='{BOX};top:50px'>ab cd\nef</textarea><div id=h contenteditable style='{BOX};top:110px'>foo bar</div>");
    let mut b = Browser::new(tree::parse(&html), BrowserLimits::default(), EvalLimits { max_steps: 50_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
    b.set_viewport(400.0, 300.0).unwrap();
    b.set_document_url("http://localhost/p").unwrap();
    b.run_script("window.log = []; for (const id of ['i', 't', 'h']) document.getElementById(id).addEventListener('beforeinput', e => log.push(e.inputType))").unwrap();
    Page::new(Source::Browser(Box::new(b.into())), (400, 300))
}

fn eval(p: &mut Page, src: &str) -> String {
    let b = p.browser().unwrap();
    let v = b.run_script(src).unwrap_or_else(|e| panic!("{src}: {e:?}"));
    b.it.display(v)
}

fn click(p: &mut Page, x: f64, y: f64) {
    p.handle(Event::PointerMove { x, y, buttons: 0, mods: NO });
    for pressed in [true, false] {
        p.handle(Event::PointerButton { x, y, button: 0, buttons: pressed as u16, pressed, mods: NO });
    }
}

fn press(p: &mut Page, key: &str, mods: Mods) {
    for pressed in [true, false] {
        p.handle(Event::Key { key: key.into(), code: "", pressed, repeat: false, text: None, mods });
    }
}

#[test]
fn control_backspace_and_delete_remove_a_word_in_a_field() {
    big(|| {
        let mut p = page();
        click(&mut p, 150.0, 10.0);
        eval(&mut p, "i.setSelectionRange(11, 11); 0");
        press(&mut p, "Backspace", CTRL);
        assert_eq!(eval(&mut p, "i.value + '|' + i.selectionStart"), "foo bar |8");
        press(&mut p, "Backspace", CTRL);
        assert_eq!(eval(&mut p, "i.value"), "foo ");
        eval(&mut p, "i.setSelectionRange(0, 0); 0");
        press(&mut p, "Delete", CTRL);
        assert_eq!(eval(&mut p, "i.value + '|' + i.selectionStart"), " |0");
        assert_eq!(eval(&mut p, "log.join()"), "deleteWordBackward,deleteWordBackward,deleteWordForward");
        // A selection is deleted as it is.
        eval(&mut p, "i.value = 'foo bar'; i.setSelectionRange(1, 6); 0");
        press(&mut p, "Backspace", CTRL);
        assert_eq!(eval(&mut p, "i.value"), "fr");
    });
}

#[test]
fn control_backspace_crosses_a_line_feed_in_a_textarea() {
    big(|| {
        let mut p = page();
        click(&mut p, 150.0, 60.0);
        eval(&mut p, "t.setSelectionRange(8, 8); 0");
        press(&mut p, "Backspace", CTRL);
        assert_eq!(eval(&mut p, "JSON.stringify(t.value)"), "\"ab cd\\n\"");
        press(&mut p, "Backspace", CTRL);
        assert_eq!(eval(&mut p, "JSON.stringify(t.value)"), "\"ab \"");
    });
}

#[test]
fn control_backspace_and_delete_remove_a_word_in_an_editing_host() {
    big(|| {
        let mut p = page();
        click(&mut p, 150.0, 120.0);
        press(&mut p, "Backspace", CTRL);
        assert_eq!(eval(&mut p, "h.textContent"), "foo ");
        press(&mut p, "ArrowLeft", CTRL);
        press(&mut p, "Delete", CTRL);
        assert_eq!(eval(&mut p, "h.textContent"), " ");
        assert!(eval(&mut p, "log.join()").ends_with("deleteWordBackward,deleteWordForward"), "{}", eval(&mut p, "log.join()"));
    });
}
