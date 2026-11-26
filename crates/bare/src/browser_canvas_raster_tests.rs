//! Tests of the bitmap of a `<canvas>`: `fillRect`, `strokeRect` and `clearRect` draw pixels, and paint shows them. The expected pixels
//! are written by hand. Edges are anti-aliased, as in Chrome: a pixel that a rectangle covers in part is `?`.

use crate::browser::*;
use crate::js_limits::{EvalLimits, HeapLimits};
use bare_html::tree;

/// The first `<canvas>` of the page.
fn canvas(doc: &bare_html::dom::Document) -> bare_html::dom::NodeId {
    let mut stack = vec![doc.root()];
    while let Some(n) = stack.pop() {
        if doc.is_html(n, "canvas") {
            return n;
        }
        stack.extend(doc.children(n));
    }
    panic!("no canvas")
}

fn browser(page: &str) -> Browser {
    Browser::new(tree::parse(page), BrowserLimits::default(), EvalLimits { max_steps: 50_000_000, ..Default::default() }, HeapLimits::default()).expect("browser")
}

/// The bitmap of the canvas `#c`, one row per string: `#` for `dark`, `.` for clear, `?` for another color.
fn rows(b: &Browser, dark: u32) -> Vec<String> {
    let doc = b.dom.doc.borrow();
    let n = canvas(&doc);
    let (w, h, px) = doc.image(n).expect("a bitmap");
    let px = px.shared();
    (0..*h).map(|y| (0..*w).map(|x| match px[(y * w + x) as usize] { 0 => '.', p if p == dark => '#', _ => '?' }).collect()).collect()
}

#[test]
fn fill_stroke_and_clear_draw_pixels() {
    let mut b = browser("<canvas id=c width=6 height=5></canvas>");
    b.run_script("var x = document.getElementById('c').getContext('2d'); x.fillStyle = '#122e31'; x.fillRect(1, 1, 2, 2); x.fillRect(4.6, 0, -1, 1.4)").unwrap();
    // The rectangle 3.6..4.6 by 0..1.4 covers four pixels in part.
    assert_eq!(rows(&b, 0xFF12_2E31), ["...??.", ".##??.", ".##...", "......", "......"]);
    // A 1px line on a half pixel covers one pixel: the box 0..5 by 0..4. `clearRect` makes pixels transparent again.
    b.run_script("x.strokeStyle = 'rgb(18, 46, 49)'; x.strokeRect(0.5, 0.5, 4, 3); x.clearRect(1, 1, 2, 1)").unwrap();
    // (3, 1) keeps the part coverage of the first fill.
    assert_eq!(rows(&b, 0xFF12_2E31), ["#####.", "#..?#.", "###.#.", "#####.", "......"]);
}

#[test]
fn a_color_with_alpha_blends_and_a_bad_style_draws_nothing() {
    let mut b = browser("<canvas id=c width=2 height=1></canvas>");
    b.run_script("var x = document.getElementById('c').getContext('2d'); x.fillStyle = 'white'; x.fillRect(0, 0, 2, 1);
        x.fillStyle = 'rgba(0, 0, 0, 0.5)'; x.fillRect(0, 0, 1, 1); x.fillStyle = 'nonsense'; x.fillRect(1, 0, 1, 1); x.fillRect(0, 0, NaN, 1)").unwrap();
    let doc = b.dom.doc.borrow();
    let px = doc.image(canvas(&doc)).unwrap().2.shared();
    assert_eq!(&px[..], [0xFF7F_7F7F, 0xFFFF_FFFF]);
}

#[test]
fn a_new_size_clears_the_bitmap() {
    let mut b = browser("<canvas id=c width=2 height=1></canvas>");
    b.run_script("var c = document.getElementById('c'), x = c.getContext('2d'); x.fillRect(0, 0, 2, 1); c.width = 3; x.fillRect(2, 0, 1, 1)").unwrap();
    assert_eq!(rows(&b, 0xFF00_0000), ["..#"]);
}

#[test]
fn paint_shows_the_bitmap() {
    let mut b = browser("<body style='margin:0'><canvas id=c width=4 height=4 style='display:block;width:8px'></canvas>");
    b.run_script("var x = document.getElementById('c').getContext('2d'); x.fillStyle = '#f00'; x.fillRect(0, 0, 2, 2)").unwrap();
    let mut doc = b.dom.doc.borrow_mut();
    let viewport = (20.0, 20.0);
    let mut rules = bare_css::cascade::RuleSet::new(viewport);
    rules.add(&mut doc, bare_css::cascade::UA_CSS, bare_css::cascade::Origin::UserAgent);
    let styles = rules.compute(&doc);
    let layout = bare_layout::layout(&doc, &styles, viewport);
    let c = bare_paint::capture(&doc, &styles, &layout, 20, 20);
    // The bitmap is scaled to the 8 by 8 box: red at the top left quarter, the page white elsewhere.
    assert_eq!((c.pixels[1 * 20 + 1], c.pixels[3 * 20 + 3], c.pixels[6 * 20 + 6]), (0xFFFF_0000, 0xFFFF_0000, 0xFFFF_FFFF));
}
