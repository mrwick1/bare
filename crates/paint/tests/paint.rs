//! Paint tests: small pages from `tests/fixtures`, drawn at 400 by 300, with exact pixels and a hash of the whole buffer.
//!
//! A hash changes when any pixel changes. If a change is on purpose, look at the picture (`bare --paint=out.png file.html`)
//! and then copy the new hash from the failure message. The pixel checks tell you what moved.
//!
//! The fixtures were also drawn in Chrome (`docs/resources.md`, "Paint and pixel capture"). `backgrounds` is the same as
//! Chrome in every pixel. In the other fixtures the boxes and the borders are the same and the text is not.

use bare_css::cascade::{Origin, RuleSet, Source, UA_CSS};
use bare_html::tree;
use bare_layout::fragment::Layout;
use bare_paint::canvas::Canvas;
use bare_paint::display::Item;
use bare_paint::{Painter, capture};

const W: u32 = 400;
const H: u32 = 300;

const WHITE: u32 = 0xffff_ffff;

struct Page {
    doc: bare_html::dom::Document,
    styles: bare_css::cascade::Styles,
    layout: Layout,
}

/// Parse, cascade (the browser sheet and the `<style>` elements) and lay out.
fn page(html: &str) -> Page {
    let mut doc = tree::parse(html);
    let viewport = (W as f32, H as f32);
    let mut rules = RuleSet::new(viewport);
    rules.add(&mut doc, UA_CSS, Origin::UserAgent);
    for source in bare_css::cascade::sources(&doc, viewport) {
        if let Source::Inline(text) = source {
            rules.add(&mut doc, &text, Origin::Author);
        }
    }
    let styles = rules.compute(&doc);
    let layout = bare_layout::layout(&doc, &styles, viewport);
    Page { doc, styles, layout }
}

fn render(html: &str) -> Canvas {
    let p = page(html);
    capture(&p.doc, &p.styles, &p.layout, W, H)
}

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!("fixtures/", $name, ".html"))
    };
}

fn rgb(r: u32, g: u32, b: u32) -> u32 {
    0xff00_0000 | r << 16 | g << 8 | b
}

/// The bounding box of the pixels in a rectangle that are not `background`: `(x0, y0, x1, y1)` with x1 and y1 past the edge.
fn ink(c: &Canvas, x0: u32, y0: u32, x1: u32, y1: u32, background: u32) -> Option<(u32, u32, u32, u32)> {
    let mut b: Option<(u32, u32, u32, u32)> = None;
    for y in y0..y1 {
        for x in x0..x1 {
            if c.pixel(x, y) != background {
                b = Some(b.map_or((x, y, x + 1, y + 1), |(a, b2, c2, d)| (a.min(x), b2.min(y), c2.max(x + 1), d.max(y + 1))));
            }
        }
    }
    b
}

#[test]
fn backgrounds() {
    let c = render(fixture!("backgrounds"));
    let grey = rgb(0xee, 0xee, 0xee);
    // The `body` background is the canvas: all around the boxes.
    assert_eq!(c.pixel(0, 0), grey);
    assert_eq!(c.pixel(399, 299), grey);
    // `.a`: x 20 to 380, y 10 to 50, red.
    assert_eq!(c.pixel(20, 10), rgb(255, 0, 0));
    assert_eq!(c.pixel(379, 49), rgb(255, 0, 0));
    assert_eq!(c.pixel(19, 10), grey);
    assert_eq!(c.pixel(380, 10), grey);
    assert_eq!(c.pixel(20, 9), grey);
    assert_eq!(c.pixel(20, 50), grey, "the margin between `.a` and `.b`");
    // `.b` (y 60 to 140) is blue with 10 px of padding. `.c` inside (y 70 to 100) is green. `.d` (y 110 to 130) is half
    // black over the blue.
    assert_eq!(c.pixel(20, 60), rgb(0, 0, 255));
    assert_eq!(c.pixel(25, 65), rgb(0, 0, 255));
    assert_eq!(c.pixel(30, 70), rgb(0, 255, 0));
    assert_eq!(c.pixel(369, 99), rgb(0, 255, 0));
    assert_eq!(c.pixel(30, 100), rgb(0, 0, 255));
    assert_eq!(c.pixel(30, 110), rgb(0, 0, 127));
    assert_eq!(c.pixel(30, 135), rgb(0, 0, 255));
    // `.e` (y 140 to 160) is transparent: the canvas shows. `.f` is y 165.5 to 175.9, and snaps to 166 and 176.
    assert_eq!(c.pixel(30, 150), grey);
    assert_eq!(c.pixel(30, 165), grey);
    assert_eq!(c.pixel(30, 166), rgb(255, 255, 0));
    assert_eq!(c.pixel(30, 175), rgb(255, 255, 0));
    assert_eq!(c.pixel(30, 176), grey);
    assert_eq!(c.hash(), 0x6571_9e01_1dd7_e2fd);
}

#[test]
fn borders() {
    let c = render(fixture!("borders"));
    let black = rgb(0, 0, 0);
    // `.all`: x 10 to 138 (120 + 2 * 4), y 10 to 48, a 4 px black border and a white inside.
    assert_eq!(c.pixel(10, 10), black);
    assert_eq!(c.pixel(13, 20), black);
    assert_eq!(c.pixel(14, 20), WHITE);
    assert_eq!(c.pixel(137, 20), black);
    assert_eq!(c.pixel(20, 13), black);
    assert_eq!(c.pixel(20, 14), WHITE);
    assert_eq!(c.pixel(20, 47), black);
    assert_eq!(c.pixel(20, 48), WHITE);
    // `.sides`: top 1 red, right 2 green, bottom 3 blue, left 4 orange. y starts at 58.
    assert_eq!(c.pixel(60, 58), rgb(255, 0, 0));
    assert_eq!(c.pixel(60, 59), WHITE);
    assert_eq!(c.pixel(60, 88), WHITE);
    assert_eq!(c.pixel(60, 89), rgb(0, 0, 255));
    assert_eq!(c.pixel(60, 91), rgb(0, 0, 255));
    assert_eq!(c.pixel(60, 92), WHITE);
    assert_eq!(c.pixel(10, 70), rgb(255, 170, 0));
    assert_eq!(c.pixel(13, 70), rgb(255, 170, 0));
    assert_eq!(c.pixel(14, 70), WHITE);
    assert_eq!(c.pixel(133, 70), WHITE);
    assert_eq!(c.pixel(134, 70), rgb(0, 170, 0));
    assert_eq!(c.pixel(135, 70), rgb(0, 170, 0));
    assert_eq!(c.pixel(136, 70), WHITE);
    // `.one`: only a left border, 8 px, purple. Nothing on the other sides.
    assert_eq!(c.pixel(10, 102), rgb(0x99, 0, 0x99));
    assert_eq!(c.pixel(17, 120), rgb(0x99, 0, 0x99));
    assert_eq!(c.pixel(18, 120), WHITE);
    assert_eq!(c.pixel(60, 102), WHITE);
    assert_eq!(c.hash(), 0xe1dc_e9eb_bebc_2847);
}

#[test]
fn text_uses_the_layout_metrics() {
    // 50 bars at 16 px: each is 9.6 px wide in layout, so the run is 480 px: it wraps. Use a wide window of 100 letters.
    let p = {
        let mut doc = tree::parse(&format!("<!doctype html><body style='margin:0'><div style='width:2000px'>{}</div>", "|".repeat(50)));
        let viewport = (2000.0, 40.0);
        let mut rules = RuleSet::new(viewport);
        rules.add(&mut doc, UA_CSS, Origin::UserAgent);
        let styles = rules.compute(&doc);
        let layout = bare_layout::layout(&doc, &styles, viewport);
        Page { doc, styles, layout }
    };
    let c = capture(&p.doc, &p.styles, &p.layout, 2000, 40);
    let text = p.layout.fragments.iter().find(|f| matches!(f.kind, bare_layout::fragment::Kind::Text { .. })).unwrap();
    assert_eq!(text.rect.w, 480.0, "50 letters of 9.6 px");
    // The ink of each bar: find the columns that have ink, and group them.
    let mut groups: Vec<(u32, u32)> = Vec::new();
    for x in 0..2000 {
        let has = (0..40).any(|y| c.pixel(x, y) != WHITE);
        match groups.last_mut() {
            Some(g) if has && g.1 == x => g.1 = x + 1,
            _ if has => groups.push((x, x + 1)),
            _ => {}
        }
    }
    assert_eq!(groups.len(), 50, "one group of columns for each letter");
    // The pitch from the first letter to the last is 49 steps of 9.6 px. The centers of the groups measure it to
    // better than a quarter of a pixel, even though each letter sits on a whole pixel or a quarter.
    let center = |g: (u32, u32)| (g.0 + g.1) as f32 / 2.0;
    let pitch = (center(groups[49]) - center(groups[0])) / 49.0;
    assert!((pitch - 9.6).abs() < 0.02, "pitch {pitch}");
    // The run stays inside the rectangle that layout made for it.
    assert!(groups[0].0 as f32 >= text.rect.x - 1.0);
    assert!(groups[49].1 as f32 <= text.rect.x + text.rect.w + 1.0);
}

#[test]
fn text_fixture() {
    let p = page(fixture!("text"));
    let c = capture(&p.doc, &p.styles, &p.layout, W, H);
    // The big text is red: some pixel in its line has a red ink color (full coverage).
    let red = rgb(0xcc, 0, 0);
    assert!(c.pixels.contains(&red), "red text");
    // The yellow background under `.mark` is drawn under the text: yellow and black both appear in its box.
    let yellow = rgb(255, 255, 0);
    assert!(c.pixels.contains(&yellow));
    // Underline: a run of black pixels, at least 9 letters wide, in one row.
    let mut best = 0;
    for y in 0..H {
        let mut run = 0;
        for x in 0..W {
            run = if c.pixel(x, y) == rgb(0, 0, 0) { run + 1 } else { 0 };
            best = best.max(run);
        }
    }
    assert!(best >= 9 * 9, "an underline and a strike-through are rows of ink: {best}");
    assert_eq!(c.hash(), 0x9eb7_1cfb_b8b9_148d);
}

#[test]
fn login_fixture() {
    let c = render(fixture!("login"));
    let page = rgb(0xf3, 0xf4, 0xf6);
    assert_eq!(c.pixel(5, 5), page, "the body background is the canvas");
    // The card: x 60 to 340 content + 16 padding + 1 border on each side: 43 to 357, from y 20.
    assert_eq!(c.pixel(43, 20), rgb(0xd1, 0xd5, 0xdb), "card border");
    assert_eq!(c.pixel(44, 21), WHITE, "card background");
    assert_eq!(c.pixel(42, 30), page);
    // The button is blue, and its label is white on it.
    let blue = rgb(0x25, 0x63, 0xeb);
    assert_eq!(c.pixel(61, 168), blue);
    let button = ink(&c, 60, 166, 134, 197, blue).expect("the label");
    assert!(button.0 > 61 && button.2 < 133, "the label sits inside the button: {button:?}");
    // The error message is red: its ink has the color of the text where the coverage is full.
    assert!(c.pixels.contains(&rgb(0xb9, 0x1c, 0x1c)));
    assert_eq!(c.hash(), 0x41d1_9e99_2524_de24);
}

#[test]
fn painting_order() {
    let c = render(fixture!("order"));
    // The clip: `.in` is orange and starts 40 px inside a box of 100 px, 300 px wide: it stops at the padding box.
    let orange = rgb(0xff, 0x88, 0x00);
    assert_eq!(c.pixel(60, 15), orange, "inside the clip");
    assert_eq!(c.pixel(111, 15), orange, "the last pixel of the padding box (x 12 to 112)");
    assert_eq!(c.pixel(112, 15), rgb(0, 0, 0), "the border, not orange");
    assert_eq!(c.pixel(113, 15), rgb(0, 0, 0), "the border is 2 px wide");
    assert_eq!(c.pixel(114, 15), WHITE, "outside the box: the clip held");
    // The float is under the text of the block beside it, and over nothing: it is a block of its own.
    assert_eq!(c.pixel(10, 80), rgb(0, 0xcc, 0xff));
    // The relative box is over the lines of text, which come before it in the tree.
    let magenta = rgb(0xff, 0, 0xff);
    assert_eq!(c.pixel(250, 90), magenta);
    // The absolute box is black with white text.
    assert_eq!(c.pixel(110, 170), rgb(0, 0, 0));
    assert!(ink(&c, 20, 150, 120, 180, rgb(0, 0, 0)).is_some(), "white text on the black box");
    assert_eq!(c.hash(), 0x4c8d_d952_005f_7797);
}

#[test]
fn a_repeat_paint_is_the_same_and_allocates_nothing_new() {
    let p = page(fixture!("login"));
    let mut painter = Painter::new();
    let mut canvas = Canvas::new(W, H);
    painter.paint(&p.doc, &p.styles, &p.layout, &mut canvas);
    let first = canvas.hash();
    let (items, text, glyphs) = (painter.list.items.capacity(), painter.list.text.capacity(), painter.glyphs.stats());
    let at = canvas.pixels.as_ptr();
    painter.paint(&p.doc, &p.styles, &p.layout, &mut canvas);
    assert_eq!(canvas.hash(), first);
    assert_eq!(canvas.pixels.as_ptr(), at);
    assert_eq!((painter.list.items.capacity(), painter.list.text.capacity(), painter.glyphs.stats()), (items, text, glyphs));
    // A new painter gives the same picture as an old one.
    assert_eq!(capture(&p.doc, &p.styles, &p.layout, W, H).hash(), first);
}

#[test]
fn what_is_outside_the_canvas_makes_no_item() {
    let rows = "<div style='height:20px;background:#f00'>row</div>".repeat(1000);
    let p = page(&format!("<!doctype html><body style='margin:0'>{rows}"));
    let mut painter = Painter::new();
    let mut canvas = Canvas::new(W, H);
    painter.paint(&p.doc, &p.styles, &p.layout, &mut canvas);
    // 15 rows are on the canvas (300 / 20), each with a rectangle and a text.
    let rects = painter.list.items.iter().filter(|i| matches!(i, Item::Rect { .. })).count();
    let texts = painter.list.items.iter().filter(|i| matches!(i, Item::Text { .. })).count();
    assert_eq!((rects, texts), (15, 15));
    assert_eq!(painter.list.text, "row".repeat(15));
}

#[test]
fn clips_are_balanced() {
    let p = page(fixture!("order"));
    let mut painter = Painter::new();
    let mut canvas = Canvas::new(W, H);
    painter.paint(&p.doc, &p.styles, &p.layout, &mut canvas);
    let mut depth = 0i32;
    for i in &painter.list.items {
        match i {
            Item::PushClip(_) => depth += 1,
            Item::PopClip => depth -= 1,
            Item::Later(_) => panic!("`Later` is only in the scratch lists"),
            _ => {}
        }
        assert!(depth >= 0);
    }
    assert_eq!(depth, 0);
}

#[test]
fn png_of_a_fixture() {
    let c = render(fixture!("backgrounds"));
    let mut bytes = Vec::new();
    bare_paint::png::encode(&c, &mut bytes).unwrap();
    assert_eq!(&bytes[..8], &[0x89, b'P', b'N', b'G', 13, 10, 26, 10]);
    // A page of flat colors compresses well.
    assert!(bytes.len() < 8000, "{} bytes", bytes.len());
    let mut again = Vec::new();
    bare_paint::png::encode(&c, &mut again).unwrap();
    assert_eq!(bytes, again, "the same canvas gives the same file");
}

#[test]
fn positioned_boxes_and_floats_paint_over_block_backgrounds() {
    let body = "<!doctype html><body style='margin:0'>";
    // A relative box comes first in the tree, and a later block overlaps it with a negative margin.
    // Without `position` the later block would be on top. With it, the relative box is.
    let c = render(&format!("{body}<div style='position:relative;width:50px;height:50px;background:#f00'></div><div style='margin-top:-30px;height:50px;width:50px;background:#00f'></div>"));
    assert_eq!(c.pixel(10, 10), rgb(255, 0, 0));
    assert_eq!(c.pixel(10, 30), rgb(255, 0, 0), "the positioned box is over the block that comes after it");
    assert_eq!(c.pixel(10, 60), rgb(0, 0, 255));
    let c = render(&format!("{body}<div style='width:50px;height:50px;background:#f00'></div><div style='margin-top:-30px;height:50px;width:50px;background:#00f'></div>"));
    assert_eq!(c.pixel(10, 30), rgb(0, 0, 255), "in the flow, the later block is on top");
    // A float is over the background of the block beside it, which comes later in the tree.
    let c = render(&format!("{body}<div style='float:left;width:50px;height:50px;background:#f00'></div><div style='height:50px;background:#00f'></div>"));
    assert_eq!(c.pixel(10, 10), rgb(255, 0, 0));
    assert_eq!(c.pixel(60, 10), rgb(0, 0, 255));
}
