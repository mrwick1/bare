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

fn render(html: &str) -> Canvas<'static> {
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
    assert_eq!(c.hash(), 0x91f6_b033_d5cf_7d87);
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
    assert_eq!(c.pixel(61, 203), blue);
    let button = ink(&c, 60, 202, 132, 232, blue).expect("the label");
    assert!(button.0 > 61 && button.2 < 131, "the label sits inside the button: {button:?}");
    // The fields have a text line, and the value is drawn. Their border is the one of the page, 1 px.
    assert_eq!(c.pixel(60, 100), rgb(0x9c, 0xa3, 0xaf));
    assert!(ink(&c, 63, 104, 150, 116, WHITE).is_some(), "the value of the email field");
    // The error message is red: its ink has the color of the text where the coverage is full.
    assert!(c.pixels.contains(&rgb(0xb9, 0x1c, 0x1c)));
    assert_eq!(c.hash(), 0xcf33_7a28_1189_34f9);
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

/// A page with no margin, so that a box is at (0, 0).
fn bare(body: &str) -> Canvas<'static> {
    render(&format!("<!doctype html><style>body{{margin:0}}</style>{body}"))
}

const GRAY: u32 = 0xff76_7676;
const BLUE: u32 = 0xff00_75ff;

#[test]
fn an_input_has_a_text_line_a_value_and_chromes_border() {
    // Chrome: 211 by 24, a 1 px gray border, no pixel in the four corners, white inside.
    let c = bare("<input value='hello'>");
    assert_eq!(c.pixel(0, 0), WHITE, "the corner pixel is left out");
    assert_eq!(c.pixel(1, 0), GRAY);
    assert_eq!(c.pixel(0, 1), GRAY);
    assert_eq!(c.pixel(210, 12), GRAY);
    assert_eq!(c.pixel(100, 23), GRAY);
    assert_eq!(c.pixel(100, 12), WHITE);
    assert_eq!(c.pixel(211, 12), WHITE, "the box is 211 px wide");
    // The value is in the content box (x 4 to 207, y 3 to 21), 5 letters of 8 px.
    let v = ink(&c, 1, 1, 210, 23, WHITE).expect("the value");
    assert!(v.0 >= 4 && v.2 <= 4 + 5 * 8 + 1 && v.1 >= 3 && v.3 <= 21, "{v:?}");
    // The placeholder is gray, and only when the value is empty. A password shows dots, as wide as the letters.
    let p = bare("<input placeholder='Name'>");
    assert!(p.pixels.contains(&rgb(0x75, 0x75, 0x75)));
    let dots = bare("<input type=password value='abcdef'>");
    let (x0, _, x1, _) = ink(&dots, 1, 1, 210, 23, WHITE).expect("the dots");
    assert!(x1 - x0 <= 6 * 8 && x1 - x0 >= 5 * 8, "6 dots: {x0}..{x1}");
    // A value that is too long is cut at the content box.
    let long = bare(&format!("<input value='{}'>", "m".repeat(60)));
    assert_eq!(long.pixel(208, 12), WHITE);
    assert!(ink(&long, 4, 3, 208, 21, WHITE).is_some());
}

#[test]
fn a_page_that_styles_the_control_gets_a_css_box() {
    // `border` and `background` of the page turn the theme off: a round blue box, and the text on it.
    let c = bare("<input value='x' style='border:2px solid #f00;border-radius:8px;background:#0f0;width:60px'>");
    assert_eq!(c.pixel(0, 0), WHITE, "round corner");
    assert_eq!(c.pixel(30, 0), rgb(255, 0, 0));
    assert_eq!(c.pixel(30, 1), rgb(255, 0, 0));
    assert_eq!(c.pixel(30, 5), rgb(0, 255, 0));
    let none = bare("<input value='x' style='appearance:none;width:60px'>");
    assert_eq!(none.pixel(30, 0), rgb(0x76, 0x76, 0x76), "appearance:none keeps the 2 px inset border as a solid one");
}

#[test]
fn checkboxes_radios_buttons_and_selects() {
    let c = bare("<input type=checkbox checked style='position:absolute;left:0;top:0;margin:0'><input type=radio checked style='position:absolute;left:20px;top:0;margin:0'><input type=checkbox style='position:absolute;left:40px;top:0;margin:0'>");
    assert_eq!(c.pixel(6, 1), BLUE, "checked: blue box");
    assert_eq!(c.pixel(0, 0), 0xff9e_cbff, "the corner of the blue box is a light blue (Chrome: a9d0ff)");
    assert_eq!(c.pixel(26, 6), BLUE, "radio dot");
    assert_eq!(c.pixel(20, 6), BLUE, "radio ring");
    assert_eq!(c.pixel(40, 6), GRAY, "an empty box has a gray border");
    assert_eq!(c.pixel(46, 6), WHITE);
    let b = bare("<button style='position:absolute;left:0;top:0;margin:0'>Go</button><input type=submit style='position:absolute;top:40px;left:0;margin:0'>");
    assert_eq!(b.pixel(5, 5), rgb(0xef, 0xef, 0xef), "the face of a button");
    assert_eq!(b.pixel(5, 0), GRAY);
    // The label is in the middle: the ink has the same room left and right (the box is 32 px wide).
    let (x0, _, x1, _) = ink(&b, 3, 3, 29, 21, rgb(0xef, 0xef, 0xef)).expect("the label");
    assert!((x0 as i32 - (32 - x1 as i32)).abs() <= 2, "{x0} {x1}");
    let s = bare("<select style='position:absolute;left:0;top:0'><option>One</option><option selected>Second</option></select>");
    assert_eq!(s.pixel(1, 0), GRAY);
    assert!(ink(&s, 4, 2, 60, 20, WHITE).is_some(), "the chosen option");
    // The arrow is at the right.
    assert!(ink(&s, 60, 2, 76, 20, WHITE).is_some(), "the arrow");
}

#[test]
fn rounded_boxes_borders_and_circles() {
    let c = bare("<div style='width:40px;height:40px;background:#f00;border-radius:10px'></div>");
    assert_eq!(c.pixel(0, 0), WHITE);
    assert_eq!(c.pixel(39, 39), WHITE);
    assert_eq!(c.pixel(20, 0), rgb(255, 0, 0));
    assert_eq!(c.pixel(0, 20), rgb(255, 0, 0));
    assert_eq!(c.pixel(1, 1), WHITE, "the corner is cut: (1,1) is 12 px from the center of the arc, which has a radius of 10");
    assert_eq!(c.pixel(5, 5), rgb(255, 0, 0));
    let edge = c.pixel(3, 2);
    assert!(edge != WHITE && edge != rgb(255, 0, 0), "the edge is anti-aliased: {edge:08x}");
    // 50% is a circle on a square.
    let o = bare("<div style='width:40px;height:40px;background:#00f;border-radius:50%'></div>");
    assert_eq!(o.pixel(20, 20), rgb(0, 0, 255));
    assert_eq!(o.pixel(2, 2), WHITE);
    assert_eq!(o.pixel(20, 0) >> 24, 0xff);
    // The border follows the radius: a ring, with the inside curved too.
    let r = bare("<div style='width:30px;height:30px;border:5px solid #000;border-radius:20px;background:#ff0'></div>");
    assert_eq!(r.pixel(20, 2), rgb(0, 0, 0));
    assert_eq!(r.pixel(2, 20), rgb(0, 0, 0));
    assert_eq!(r.pixel(20, 20), rgb(255, 255, 0));
    assert_eq!(r.pixel(0, 0), WHITE);
    // A radius that is too big is cut to fit (CSS Backgrounds 3, 5.5): 100px on a 40 by 20 box is a pill.
    let pill = bare("<div style='width:40px;height:20px;background:#000;border-radius:100px'></div>");
    assert_eq!(pill.pixel(20, 0), rgb(0, 0, 0));
    assert_eq!(pill.pixel(0, 0), WHITE);
    assert_eq!(pill.pixel(10, 10), rgb(0, 0, 0));
}

#[test]
fn dashed_and_dotted_borders_have_chromes_pattern() {
    // Chrome, 3 px dashed, 26 px side: dashes of 6 px with gaps of 4 px (the gap grows to fit three dashes).
    let c = bare("<div style='width:60px;height:20px;border:3px dashed #000'></div>");
    let col = |y| c.pixel(1, y);
    let black = rgb(0, 0, 0);
    assert_eq!((col(0), col(5), col(6), col(9), col(10), col(15), col(16), col(19), col(20), col(25)), (black, black, WHITE, WHITE, black, black, WHITE, WHITE, black, black));
    assert_eq!(c.pixel(30, 10), WHITE, "the inside");
    // 2 px dotted: squares, 2 on and about 2 off.
    let d = bare("<div style='width:60px;height:20px;border:2px dotted #000'></div>");
    let row: Vec<bool> = (0..12).map(|x| d.pixel(x, 0) == black).collect();
    assert_eq!(row[..3], [true, true, false]);
    // 6 px dotted: round dots (the corner of the dot's box is white).
    let r = bare("<div style='width:60px;height:20px;border:6px dotted #000'></div>");
    assert_eq!(r.pixel(3, 3), black);
    assert_eq!(r.pixel(0, 0), WHITE);
}

#[test]
fn border_width_snaps_like_chrome() {
    // 0.4 px is 1 px, 2.7 px is 2 px.
    let c = bare("<div style='width:10px;height:10px;border:0.4px solid #000'></div><div style='width:10px;height:10px;border:2.7px solid #000;margin-top:5px'></div>");
    let black = rgb(0, 0, 0);
    assert_eq!((c.pixel(0, 0), c.pixel(1, 1)), (black, WHITE));
    // The first box is 12 px high, then 5 px of margin: the second starts at y 17.
    assert_eq!((c.pixel(0, 17), c.pixel(1, 18), c.pixel(2, 19)), (black, black, WHITE));
}

#[test]
fn opacity_mixes_the_whole_box() {
    let c = bare("<div style='width:20px;height:20px;background:#000;opacity:.5'><div style='width:10px;height:10px;background:#fff'></div></div>");
    // 50% of black over white is 127 or 128 gray, and the white child shows no more than the white page: it is in the layer.
    assert_eq!(c.pixel(15, 15), rgb(127, 127, 127));
    assert_eq!(c.pixel(5, 5), WHITE);
    // The children do not show through each other: two overlapping children of a layer mix once.
    let g = bare("<div style='position:relative;width:30px;height:30px;opacity:.5'><div style='position:absolute;width:20px;height:20px;background:#000'></div><div style='position:absolute;left:10px;top:10px;width:20px;height:20px;background:#000'></div></div>");
    assert_eq!(g.pixel(15, 15), rgb(127, 127, 127), "overlap: the same gray, not darker");
    assert_eq!(g.pixel(5, 5), rgb(127, 127, 127));
    // 0 paints nothing, 1 is plain.
    assert_eq!(bare("<div style='width:20px;height:20px;background:#000;opacity:0'></div>").pixel(5, 5), WHITE);
    assert_eq!(bare("<div style='width:20px;height:20px;background:#000;opacity:1'></div>").pixel(5, 5), rgb(0, 0, 0));
}

#[test]
fn z_index_orders_stacking_contexts() {
    let box_ = |extra: &str, color: &str| format!("<div style='position:absolute;left:0;top:0;width:30px;height:30px;background:{color};{extra}'></div>");
    // The later box is on top, unless the earlier has a higher z-index.
    assert_eq!(bare(&format!("{}{}", box_("", "#f00"), box_("", "#00f"))).pixel(5, 5), rgb(0, 0, 255));
    assert_eq!(bare(&format!("{}{}", box_("z-index:1", "#f00"), box_("", "#00f"))).pixel(5, 5), rgb(255, 0, 0));
    assert_eq!(bare(&format!("{}{}", box_("z-index:2", "#f00"), box_("z-index:3", "#00f"))).pixel(5, 5), rgb(0, 0, 255));
    // A negative one goes behind the blocks of the context.
    let behind = bare(&format!("<div style='height:30px;background:#0f0'>{}</div>", box_("z-index:-1", "#f00")));
    assert_eq!(behind.pixel(5, 5), rgb(0, 255, 0), "behind the block background");
    // A positioned box that is not a context (no z-index) paints over its negative child, which goes up to the root...
    let wrap = |z: &str| bare(&format!("<div style='position:relative;{z}width:50px;height:50px;background:#00f'>{}</div>", box_("z-index:-1", "#f00")));
    assert_eq!(wrap("").pixel(5, 5), rgb(0, 0, 255));
    // ...and a context keeps it inside, over its own background.
    assert_eq!(wrap("z-index:0;").pixel(5, 5), rgb(255, 0, 0));
    // A flex item with a z-index is a context, as a positioned box is.
    let f = bare("<div style='display:flex'><i style='width:30px;height:30px;margin-right:-20px;background:#f00;z-index:1'></i><i style='width:30px;height:30px;background:#00f'></i></div>");
    assert_eq!(f.pixel(25, 5), rgb(255, 0, 0));
    // Without the z-index, the later item is on top.
    let g = bare("<div style='display:flex'><i style='width:30px;height:30px;margin-right:-20px;background:#f00'></i><i style='width:30px;height:30px;background:#00f'></i></div>");
    assert_eq!(g.pixel(25, 5), rgb(0, 0, 255));
}

#[test]
fn a_hoisted_box_keeps_the_clip_of_its_parent() {
    // The red box with z-index goes up to the root context, and `overflow:hidden` of its parent still cuts it.
    let c = bare("<div style='overflow:hidden;width:20px;height:20px'><div style='position:absolute;z-index:3;left:0;top:0;width:50px;height:50px;background:#f00'></div></div>");
    assert_eq!(c.pixel(5, 5), rgb(255, 0, 0));
    // (A positioned child of a box that is not its containing block escapes the clip in CSS, so the clip belongs to the
    // box whose subtree has the child. The check here is for the plain case only.)
    assert_eq!(bare("<div style='position:relative;overflow:hidden;width:20px;height:20px'><div style='position:absolute;z-index:3;left:0;top:0;width:50px;height:50px;background:#f00'></div></div>").pixel(30, 5), WHITE);
}

#[test]
fn symbols_outside_the_latin_subset_are_drawn() {
    let ink_of = |s: &str| ink(&bare(&format!("<p style='margin:0;font-size:32px'>{s}</p>")), 0, 0, 100, 60, WHITE);
    for s in ["\u{20b9}", "\u{2192}", "\u{2190}", "\u{21d2}", "\u{2026}", "\u{2260}", "\u{2264}", "\u{2039}", "\u{2713}", "\u{2715}", "\u{2605}", "\u{2630}", "\u{25be}"] {
        assert!(ink_of(s).is_some(), "{s} has a glyph");
    }
    assert!(ink_of("\u{4e2d}").is_none(), "a Chinese letter is not in any face");
    // Layers and clips stay balanced when the page has everything.
    let p = page(&format!("{}{}", fixture!("stacking"), fixture!("controls")));
    let mut painter = Painter::new();
    let mut canvas = Canvas::new(W, H);
    painter.paint(&p.doc, &p.styles, &p.layout, &mut canvas);
    let (mut clips, mut layers) = (0i32, 0i32);
    for i in &painter.list.items {
        match i {
            Item::PushClip(_) => clips += 1,
            Item::PopClip => clips -= 1,
            Item::PushLayer { .. } => layers += 1,
            Item::PopLayer => layers -= 1,
            _ => {}
        }
        assert!(clips >= 0 && layers >= 0);
    }
    assert_eq!((clips, layers), (0, 0));
}

/// Pixels from headless Chrome 155 for `fixtures/svg-pixels.html`: ten 20 by 20 pictures side by side.
#[test]
fn svg_shapes_have_chromes_pixels() {
    let c = render(fixture!("svg-pixels"));
    let (red, blue, black) = (rgb(0xcc, 0, 0), rgb(0, 0, 0xcc), rgb(0, 0, 0));
    // A rect fills its box, to the pixel.
    assert_eq!([c.pixel(3, 3), c.pixel(4, 4), c.pixel(15, 15), c.pixel(16, 16)], [WHITE, red, red, WHITE]);
    // A rect at x 4.5 covers half of the first and of the last pixel.
    assert_eq!([c.pixel(24, 10), c.pixel(25, 10), c.pixel(35, 10), c.pixel(36, 10)], [rgb(0x7f, 0x7f, 0xe5), blue, rgb(0x7f, 0x7f, 0xe5), WHITE]);
    // A butt line of width 2 from 2 to 18 at y 10: rows 9 and 10, pixels 2 to 17. A square cap adds one pixel at each end.
    assert_eq!([c.pixel(41, 10), c.pixel(42, 9), c.pixel(42, 10), c.pixel(57, 10), c.pixel(58, 10), c.pixel(50, 8), c.pixel(50, 11)], [WHITE, blue, blue, blue, WHITE, WHITE, WHITE]);
    assert_eq!([c.pixel(59, 10), c.pixel(61, 9), c.pixel(62, 9), c.pixel(77, 10)], [WHITE, blue, blue, blue]);
    // A viewBox of 20 by 10 in 40 by 20 doubles the shapes.
    assert_eq!([c.pixel(118, 19), c.pixel(119, 19), c.pixel(120, 5)], [red, red, WHITE]);
    // A rule of the style sheet beats the attribute `fill`, and a `g` gives its `fill` to the shapes in it.
    assert_eq!([c.pixel(143, 3), c.pixel(155, 12)], [rgb(0, 0xaa, 0), blue]);
    assert_eq!([c.pixel(170, 10), c.pixel(160, 0)], [red, WHITE]);
    // `stroke="currentColor"`: the color of the text, black here. A `fill="none"` shape has no inside.
    assert_eq!([c.pixel(210, 1), c.pixel(210, 2), c.pixel(210, 3), c.pixel(210, 10)], [black, black, WHITE, WHITE]);
    // The picture ends at the box of the `<svg>`: nothing below its 20 rows.
    assert_eq!(c.pixel(10, 20), WHITE);
}

/// `letter-spacing` and `word-spacing` move the letters as in Chrome 155 (`fixtures/spacing.html` differs from Chrome only in glyph shading).
#[test]
fn spacing_moves_the_letters() {
    // "II" at 16px: the second letter is 9.6 px after the first. With 10px of spacing it is 19.6 px after it.
    let right = |css: &str, text: &str| ink(&bare(&format!("<p style='margin:0;{css}'>{text}</p>")), 0, 0, 300, 22, WHITE).expect("ink").2 as i32;
    let plain = right("", "II");
    assert_eq!(right("letter-spacing:10px", "II") - plain, 10);
    assert_eq!(right("letter-spacing:-3px", "II") - plain, -3);
    // The spacing after the last letter is room, not ink.
    assert_eq!(right("letter-spacing:10px", "I") - right("", "I"), 0);
    assert_eq!(right("word-spacing:20px", "I I") - right("", "I I"), 20);
    assert_eq!(right("letter-spacing:5px;word-spacing:20px", "I I") - right("", "I I"), 5 + 5 + 20 + 0);
}

#[test]
fn an_svg_draws_only_inside_its_box_and_hidden_ones_draw_nothing() {
    let c = bare("<svg width=10 height=10><rect x=-5 y=-5 width=30 height=30 fill=red /></svg><svg width=10 height=10 style='visibility:hidden'><rect width=10 height=10 fill=red /></svg><svg width=10 height=10 style='display:none'><rect width=10 height=10 fill=red /></svg>");
    // One red square of 10 by 10 at x 0: the rect that sticks out is cut, the hidden svg draws nothing and the `none` one has no box.
    let b = ink(&c, 0, 0, 100, 100, WHITE).expect("the red square");
    assert_eq!((b.0, b.2, b.3 - b.1), (0, 10, 10));
    assert_eq!(ink(&c, 0, b.1, 10, b.3, rgb(255, 0, 0)), None, "all of it is red");
}

#[test]
fn the_svg_fixture() {
    let c = render(fixture!("svg"));
    // The first icon: a chevron of 2 px strokes in the color of the text (#123), with round ends, in a box of 24 px at (8, 8).
    assert!(ink(&c, 8, 8, 32, 32, WHITE).is_some());
    // Against Chrome 155 at the same font: 1,010 of 320,000 pixels differ, 2 by more than 64 levels (the edge of a curve).
    assert_eq!(c.hash(), 0x733e_1aff_a0f8_b851);
}

#[test]
fn a_rounded_dashed_or_dotted_border_follows_the_curve_in_dashes() {
    // The dashes of a round border, counted along the middle line of the border. Chrome 155 draws 13 dashes on a circle of 80 px with a
    // 6 px border, 14 round dots on a 8 px one, and the same on the corners of a rounded rectangle.
    let runs = |style: &str, border: u32| {
        let c = bare(&format!("<div style='width:80px;height:80px;box-sizing:border-box;border:{border}px {style} #000;border-radius:50%'></div>"));
        let r = 40.0 - border as f32 / 2.0;
        let on: Vec<bool> = (0..720).map(|k| {
            let a = k as f32 * std::f32::consts::PI / 360.0;
            c.pixel((40.0 + r * a.cos()) as u32, (40.0 + r * a.sin()) as u32) != WHITE
        }).collect();
        (on.iter().filter(|&&p| p).count(), (0..720).filter(|&k| on[k] && !on[(k + 719) % 720]).count())
    };
    let (ink, dashes) = runs("dashed", 6);
    assert_eq!(dashes, 13);
    assert!(ink > 720 / 2 && ink < 720 * 3 / 4, "two thirds of the line has ink: {ink}");
    assert_eq!(runs("dotted", 8).1, 14);
    assert_eq!(runs("solid", 6), (720, 0), "a solid border has no gap");
    // A rounded rectangle: the straight sides have dashes too, and a corner is not a straight cut.
    let c = bare("<div style='width:120px;height:80px;box-sizing:border-box;border:4px dashed #000;border-radius:30px'></div>");
    let top: Vec<bool> = (30..90).map(|x| c.pixel(x, 2) != WHITE).collect();
    assert!(top.contains(&true) && top.contains(&false), "dashes along the top");
}

#[test]
fn the_round_borders_fixture() {
    let c = render(fixture!("roundborders"));
    // Against Chrome 155 (a picture of 400 by 800): 4,694 of 320,000 pixels differ, 1,239 by more than 64 levels (the ends of the dashes). Before the
    // dashes followed the curve: 8,042 and 5,752.
    assert_eq!(c.hash(), 0x84b5_d476_67ed_0167);
}
