//! Tests of the wrap of a `<textarea>` in the window (`page.rs`): a long line breaks at the box width, and a press maps through the
//! visual lines. The font is 20px monospace: a letter is 12px, so a box of 72px holds 6 letters.

use crate::browser::*;
use crate::eng_new::Peek;
use crate::js_limits::{EvalLimits, HeapLimits};
use crate::page::{Page, Source};
use bare_html::tree;
use bare_layout::font::{Face, Metrics};
use bare_window::{Event, Mods};

const NO: Mods = Mods { shift: false, ctrl: false, alt: false, meta: false };
const STYLE: &str = "position:absolute;left:10px;top:10px;width:72px;height:100px;padding:0;border:0;overflow:hidden;font:20px monospace";

fn big<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap()
}

pub(crate) fn page(value: &str, style: &str) -> Page {
    let html = format!("<!doctype html><textarea id=t style='{style}'>{value}</textarea>");
    let mut b = Browser::new(tree::parse(&html), BrowserLimits::default(), EvalLimits { max_steps: 50_000_000, ..Default::default() }, HeapLimits::default()).expect("browser");
    b.set_viewport(400.0, 300.0).unwrap();
    b.set_document_url("http://localhost/p").unwrap();
    Page::new(Source::Browser(Box::new(b.into())), (400, 300))
}

pub(crate) fn click(p: &mut Page, x: f64, y: f64) {
    p.handle(Event::PointerMove { x, y, buttons: 0, mods: NO });
    for pressed in [true, false] {
        p.handle(Event::PointerButton { x, y, button: 0, buttons: pressed as u16, pressed, mods: NO });
    }
}

pub(crate) fn eval(p: &mut Page, src: &str) -> String {
    let b = p.browser().unwrap();
    let v = b.run_script(src).unwrap_or_else(|e| panic!("{src}: {e:?}"));
    b.it.display(v)
}

/// The window y of the middle of visual line `i` (from 0) of a textarea at `top` with no scroll.
pub(crate) fn mid(i: usize) -> f64 {
    10.0 + (i as f64 + 0.5) * Metrics::new(20.0, Face::Mono).normal_line_height() as f64
}

#[test]
fn a_long_line_wraps_at_the_box_width_and_a_press_maps_through_the_visual_lines() {
    big(|| {
        // Three visual lines: "aaaa ", "bbbb ", "cccc". The space stays at the end of the line that it wraps.
        let mut p = page("aaaa bbbb cccc", STYLE);
        click(&mut p, 34.0, mid(1));
        assert_eq!(eval(&mut p, "t.selectionStart"), "7");
        click(&mut p, 34.0, mid(2));
        assert_eq!(eval(&mut p, "t.selectionStart"), "12");
        // Right of the text of a wrapped line: before the space that wraps, not on the next line.
        click(&mut p, 80.0, mid(0));
        assert_eq!(eval(&mut p, "t.selectionStart"), "4");
        // Below the text: the last line.
        click(&mut p, 34.0, 100.0);
        assert_eq!(eval(&mut p, "t.selectionStart"), "12");
    });
}

#[test]
fn a_word_longer_than_the_box_breaks_at_a_letter_and_a_line_feed_still_breaks() {
    big(|| {
        let mut p = page("abcdefghijkl\nxy", STYLE);
        click(&mut p, 34.0, mid(1));
        assert_eq!(eval(&mut p, "t.selectionStart"), "8");
        click(&mut p, 34.0, mid(2));
        assert_eq!(eval(&mut p, "t.selectionStart"), "15");
    });
}
