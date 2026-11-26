//! Tests of the clipboard keys (`browser_ui/clipboard.rs`, `page.rs`). A test sends the keys through `Page::handle` and plays the window:
//! it takes what a copy put on the clipboard (`take_clipboard`) and answers a paste (`take_paste_request`, then `Event::Clipboard`).

use crate::browser::*;
use crate::eng_new::Peek;
use crate::js_limits::{EvalLimits, HeapLimits};
use crate::page::{Page, Source};
use bare_html::tree;
use bare_window::{Event, Mods};

const HTML: &str = "<body><input id=f value='hello world'><textarea id=t></textarea><div id=h contenteditable>ab</div><p id=p>page text</p><input id=pw type=password value=secret></body>";

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

struct Win {
    page: Page,
}

const CTRL: Mods = Mods { shift: false, ctrl: true, alt: false, meta: false };
const NONE: Mods = Mods { shift: false, ctrl: false, alt: false, meta: false };
const SHIFT: Mods = Mods { shift: true, ctrl: false, alt: false, meta: false };

fn text(s: &str) -> Vec<(String, Vec<u8>)> {
    vec![("text/plain;charset=utf-8".to_string(), s.as_bytes().to_vec())]
}

impl Win {
    fn new(script: &str) -> Win {
        let mut b = Browser::new(tree::parse(HTML), BrowserLimits::default(), EvalLimits { max_steps: 50_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
        b.set_viewport(400.0, 400.0).expect("viewport");
        b.set_document_url("http://localhost/c.html").expect("url");
        b.run_script(&format!("window.log = []; {script}")).expect("script");
        Win { page: Page::new(Source::Browser(Box::new(b.into())), (400, 400)) }
    }

    fn js(&mut self, src: &str) -> String {
        let b = self.page.browser().expect("browser");
        b.run_scheduled_tasks(100).expect("tasks");
        let v = b.run_script(src).expect("script");
        b.it.display(v)
    }

    fn key(&mut self, key: &str, code: &'static str, m: Mods) {
        for pressed in [true, false] {
            self.page.handle(Event::Key { key: key.into(), code, pressed, repeat: false, text: None, mods: m });
        }
    }

    /// Ctrl+`c`, then the window's part: for a paste, answer with `items`.
    fn chord(&mut self, c: &str, items: Vec<(String, Vec<u8>)>) {
        self.key(c, Box::leak(format!("Key{}", c.to_uppercase()).into_boxed_str()), CTRL);
        if c == "v" {
            self.answer(items);
        }
    }

    fn answer(&mut self, items: Vec<(String, Vec<u8>)>) {
        assert!(self.page.take_paste_request(), "a paste key asks for the clipboard");
        self.page.handle(Event::Clipboard { items });
    }

    fn log(&mut self) -> String {
        self.js("log.join('\\n')")
    }
}

const LOG: &str = "['copy', 'cut', 'paste', 'beforeinput', 'input'].forEach(function (t) { document.addEventListener(t, function (e) { \
    log.push(t + ' ' + e.target.id + (e.isTrusted ? ' trusted' : '') + (e.inputType ? ' ' + e.inputType : '') + (e.data != null ? ' data=' + e.data : '') + \
    ((e.dataTransfer || e.clipboardData) ? ' dt=' + (e.dataTransfer || e.clipboardData).getData('text/plain') : '')); }); });";

#[test]
fn copy_takes_the_selected_text_of_a_field() {
    big(|| {
        let mut w = Win::new(&format!("{LOG} var f = document.getElementById('f'); f.focus(); f.setSelectionRange(0, 5);"));
        w.chord("c", Vec::new());
        assert_eq!(w.page.take_clipboard(), Some(vec![("text/plain".to_string(), b"hello".to_vec())]));
        assert_eq!(w.log(), "copy f trusted dt=");
        assert_eq!(w.js("document.getElementById('f').value"), "hello world");
        // Taking it empties it.
        assert_eq!(w.page.take_clipboard(), None);
    });
}

#[test]
fn copy_takes_the_page_selection_when_no_field_has_focus() {
    big(|| {
        let mut w = Win::new("var p = document.getElementById('p'); getSelection().selectAllChildren(p);");
        w.chord("c", Vec::new());
        assert_eq!(w.page.take_clipboard(), Some(vec![("text/plain".to_string(), b"page text".to_vec())]));
    });
}

#[test]
fn a_password_field_is_not_copied() {
    big(|| {
        let mut w = Win::new("var f = document.getElementById('pw'); f.focus(); f.select();");
        w.chord("c", Vec::new());
        assert_eq!(w.page.take_clipboard(), None);
    });
}

#[test]
fn a_canceled_copy_uses_what_the_page_set() {
    big(|| {
        let mut w = Win::new("var f = document.getElementById('f'); f.focus(); f.select(); document.addEventListener('copy', function (e) { e.preventDefault(); e.clipboardData.setData('text/plain', 'custom'); });");
        w.chord("c", Vec::new());
        assert_eq!(w.page.take_clipboard(), Some(vec![("text/plain".to_string(), b"custom".to_vec())]));
    });
}

#[test]
fn a_canceled_keydown_makes_no_copy_event() {
    big(|| {
        let mut w = Win::new(&format!("{LOG} var f = document.getElementById('f'); f.focus(); f.select(); document.addEventListener('keydown', function (e) {{ e.preventDefault(); }});"));
        w.chord("c", Vec::new());
        assert_eq!(w.page.take_clipboard(), None);
        assert_eq!(w.log(), "");
    });
}

#[test]
fn cut_copies_then_deletes_the_selection_of_a_field() {
    big(|| {
        let mut w = Win::new(&format!("{LOG} var f = document.getElementById('f'); f.focus(); f.setSelectionRange(0, 6);"));
        w.chord("x", Vec::new());
        assert_eq!(w.page.take_clipboard(), Some(vec![("text/plain".to_string(), b"hello ".to_vec())]));
        assert_eq!(w.js("document.getElementById('f').value"), "world");
        assert_eq!(w.log(), "cut f trusted dt=\nbeforeinput f trusted deleteByCut\ninput f trusted deleteByCut");
    });
}

#[test]
fn a_canceled_cut_keeps_the_text() {
    big(|| {
        let mut w = Win::new("var f = document.getElementById('f'); f.focus(); f.select(); document.addEventListener('cut', function (e) { e.preventDefault(); e.clipboardData.setData('text/plain', 'mine'); });");
        w.chord("x", Vec::new());
        assert_eq!(w.page.take_clipboard(), Some(vec![("text/plain".to_string(), b"mine".to_vec())]));
        assert_eq!(w.js("document.getElementById('f').value"), "hello world");
    });
}

#[test]
fn cut_in_an_editing_host_deletes_the_selection() {
    big(|| {
        let mut w = Win::new(&format!("{LOG} var h = document.getElementById('h'); h.focus(); getSelection().setBaseAndExtent(h.firstChild, 0, h.firstChild, 1);"));
        w.chord("x", Vec::new());
        assert_eq!(w.page.take_clipboard(), Some(vec![("text/plain".to_string(), b"a".to_vec())]));
        assert_eq!(w.js("document.getElementById('h').textContent"), "b");
        assert_eq!(w.log(), "cut h trusted dt=\nbeforeinput h trusted deleteByCut\ninput h trusted deleteByCut");
    });
}

#[test]
fn paste_inserts_text_into_a_textarea() {
    big(|| {
        let mut w = Win::new(&format!("{LOG} document.getElementById('t').focus();"));
        w.chord("v", text("line1\r\nline2"));
        assert_eq!(w.js("document.getElementById('t').value"), "line1\nline2");
        assert_eq!(w.log(), "paste t trusted dt=line1\nline2\nbeforeinput t trusted insertFromPaste data=line1\nline2\ninput t trusted insertFromPaste data=line1\nline2");
    });
}

#[test]
fn paste_into_a_single_line_field_turns_line_breaks_into_spaces() {
    big(|| {
        let mut w = Win::new("var f = document.getElementById('f'); f.focus(); f.select();");
        w.chord("v", text("a\nb"));
        assert_eq!(w.js("document.getElementById('f').value"), "a b");
    });
}

#[test]
fn shift_insert_pastes_too() {
    big(|| {
        let mut w = Win::new("document.getElementById('t').focus();");
        w.key("Insert", "Insert", SHIFT);
        w.answer(text("zz"));
        assert_eq!(w.js("document.getElementById('t').value"), "zz");
    });
}

#[test]
fn paste_inserts_text_into_an_editing_host_with_a_data_transfer() {
    big(|| {
        let mut w = Win::new(&format!("{LOG} var h = document.getElementById('h'); h.focus(); getSelection().collapse(h.firstChild, 1);"));
        w.chord("v", text("XY"));
        assert_eq!(w.js("document.getElementById('h').textContent"), "aXYb");
        assert_eq!(w.log(), "paste h trusted dt=XY\nbeforeinput h trusted insertFromPaste dt=XY\ninput h trusted insertFromPaste dt=XY");
    });
}

#[test]
fn a_canceled_paste_inserts_nothing() {
    big(|| {
        let mut w = Win::new("var t = document.getElementById('t'); t.focus(); document.addEventListener('paste', function (e) { e.preventDefault(); });");
        w.chord("v", text("nope"));
        assert_eq!(w.js("document.getElementById('t').value"), "");
    });
}

#[test]
fn a_canceled_beforeinput_inserts_nothing() {
    big(|| {
        let mut w = Win::new("var t = document.getElementById('t'); t.focus(); t.addEventListener('beforeinput', function (e) { e.preventDefault(); });");
        w.chord("v", text("nope"));
        assert_eq!(w.js("document.getElementById('t').value"), "");
    });
}

#[test]
fn an_image_paste_gives_a_png_file_and_inserts_no_text() {
    big(|| {
        let mut w = Win::new(
            "var t = document.getElementById('t'); t.focus(); var seen = []; \
             document.addEventListener('paste', function (e) { var d = e.clipboardData, f = d.files[0]; \
               seen.push(d.files.length, f instanceof File, f.name, f.type, f.size, d.items.length, d.items[0].kind, d.items[0].type, d.types.join(','), d.getData('text/plain') === ''); });",
        );
        w.chord("v", vec![("image/png".to_string(), vec![0x89, b'P', b'N', b'G', 0, 255])]);
        assert_eq!(w.js("seen.join('|')"), "1|true|image.png|image/png|6|1|file|image/png|Files|true");
        assert_eq!(w.js("document.getElementById('t').value"), "");
    });
}

#[test]
fn text_and_image_together_make_two_items() {
    big(|| {
        let mut w = Win::new(
            "document.getElementById('t').focus(); var seen = []; \
             document.addEventListener('paste', function (e) { var d = e.clipboardData; seen.push(d.items.length, d.types.join(','), d.getData('text/plain'), d.files[0].type); });",
        );
        let mut items = text("cap");
        items.push(("image/jpeg".to_string(), vec![1, 2, 3]));
        w.chord("v", items);
        assert_eq!(w.js("seen.join('|')"), "2|text/plain,Files|cap|image/jpeg");
        assert_eq!(w.js("document.getElementById('t').value"), "cap");
    });
}

#[test]
fn an_empty_clipboard_still_fires_paste_and_inserts_nothing() {
    big(|| {
        let mut w = Win::new(&format!("{LOG} document.getElementById('t').focus();"));
        w.chord("v", Vec::new());
        assert_eq!(w.log(), "paste t trusted dt=");
        assert_eq!(w.js("document.getElementById('t').value"), "");
    });
}

#[test]
fn paste_data_transfer_is_read_only() {
    big(|| {
        let mut w = Win::new(
            "document.getElementById('t').focus(); var seen = []; document.addEventListener('paste', function (e) { var d = e.clipboardData; d.setData('text/plain', 'evil'); d.items.add('x', 'text/html'); seen.push(d.getData('text/plain'), d.items.length); });",
        );
        w.chord("v", text("real"));
        assert_eq!(w.js("seen.join('|')"), "real|1");
    });
}
