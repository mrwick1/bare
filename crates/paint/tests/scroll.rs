//! Scrolling in paint: the display list of a whole page is drawn with an offset, scroll containers move their content,
//! fixed boxes stay, and the scrollbars look like Chrome's (the numbers are pixels of headless Chrome 15x scroll positions).

use bare_css::cascade::{Origin, RuleSet, Source, UA_CSS};
use bare_html::dom::Document;
use bare_html::tree;
use bare_layout::fragment::Layout;
use bare_layout::scroll::{Areas, areas};
use bare_paint::canvas::Canvas;
use bare_paint::{Painter, Scroll, View};

const W: u32 = 400;
const H: u32 = 300;

struct Page {
    doc: Document,
    styles: bare_css::cascade::Styles,
    layout: Layout,
    areas: Areas,
}

fn page(html: &str, vp: (f32, f32)) -> Page {
    let mut doc = tree::parse(html);
    let mut rules = RuleSet::new(vp);
    rules.add(&mut doc, UA_CSS, Origin::UserAgent);
    for source in bare_css::cascade::sources(&doc, vp) {
        if let Source::Inline(text) = source {
            rules.add(&mut doc, &text, Origin::Author);
        }
    }
    let styles = rules.compute(&doc);
    let layout = bare_layout::layout(&doc, &styles, vp);
    let areas = areas(&doc, &styles, &layout);
    Page { doc, styles, layout, areas }
}

fn draw(p: &Page, scroll: &Scroll, bars: (bool, bool)) -> Canvas<'static> {
    let mut painter = Painter::new();
    painter.build(&p.doc, &p.styles, &p.layout, &p.areas, (W, H));
    let mut canvas = Canvas::new(W, H);
    painter.draw_view(&mut canvas, Some(&View { scroll, areas: &p.areas, bars }));
    canvas
}

fn node(p: &Page, id: &str) -> bare_html::dom::NodeId {
    let mut stack = vec![p.doc.root()];
    while let Some(n) = stack.pop() {
        if p.doc.attr(n, bare_html::dom::Atom::ID) == Some(id) {
            return n;
        }
        stack.extend(p.doc.children(n));
    }
    panic!("no #{id}")
}

fn px(c: &Canvas, x: u32, y: u32) -> u32 {
    c.pixel(x, y) & 0x00ff_ffff
}

/// Stripes of 50 px: the stripe at a pixel row tells what page row it shows.
const STRIPES: &str = "<style>body{margin:0} div{height:50px} #a{background:#ff0000} #b{background:#00ff00} #c{background:#0000ff} #d{background:#ffff00} #e{background:#ff00ff} #f{background:#00ffff}</style><div id=a></div><div id=b></div><div id=c></div><div id=d></div><div id=e></div><div id=f></div>";

#[test]
fn the_page_scrolls_by_an_offset_of_the_same_list() {
    let p = page(STRIPES, (W as f32, H as f32));
    let top = draw(&p, &Scroll::default(), (false, false));
    assert_eq!(px(&top, 10, 10), 0xff0000);
    assert_eq!(px(&top, 10, 60), 0x00ff00);
    let s = Scroll { y: 120.0, ..Scroll::default() };
    let c = draw(&p, &s, (false, false));
    assert_eq!(px(&c, 10, 10), 0x0000ff, "row 130 of the page is in the third stripe (100..150)");
    assert_eq!(px(&c, 10, 40), 0xffff00, "row 160 is in the fourth");
    assert_eq!(px(&c, 10, 179), 0x00ffff, "row 299 is in the last stripe (250..300)");
}

#[test]
fn a_scroll_container_moves_its_content_and_clips_it() {
    let p = page("<style>body{margin:0} #s{overflow:hidden;width:100px;height:60px;margin:20px} #in{height:200px;background:linear} i{display:block;height:30px} #i1{background:#f00} #i2{background:#0f0} #i3{background:#00f}</style><div id=s><i id=i1></i><i id=i2></i><i id=i3></i></div>", (W as f32, H as f32));
    let id = node(&p, "s");
    let mut s = Scroll::default();
    assert_eq!(px(&draw(&p, &s, (false, false)), 30, 30), 0xff0000);
    s.nodes.insert(id, (0.0, 30.0));
    let c = draw(&p, &s, (false, false));
    assert_eq!(px(&c, 30, 30), 0x00ff00, "scrolled by one item: the second item is at the top of the box");
    assert_eq!(px(&c, 30, 79), 0x0000ff);
    assert_eq!(px(&c, 30, 81), 0xffffff, "below the box (20 + 60): clipped, the page behind it");
    assert_eq!(px(&c, 10, 30), 0xffffff, "left of the box");
}

#[test]
fn a_fixed_box_stays_and_the_clip_of_a_container_does_not_cut_it() {
    let p = page("<style>body{margin:0} #s{overflow:hidden;width:100px;height:60px;margin:20px 20px 400px} #t{height:300px;background:#ccc} #f{position:fixed;left:200px;top:100px;width:50px;height:50px;background:#f00}</style><div id=s><div id=t></div><div id=f></div></div>", (W as f32, H as f32));
    let s = Scroll { y: 200.0, ..Scroll::default() };
    let c = draw(&p, &s, (false, false));
    assert_eq!(px(&c, 220, 120), 0xff0000, "the fixed box is where it was, outside the clip of its parent");
    let none = draw(&p, &Scroll::default(), (false, false));
    assert_eq!(px(&none, 220, 120), 0xff0000);
}

#[test]
fn a_box_with_z_index_in_a_scroll_container_scrolls_with_it() {
    let p = page("<style>body{margin:0} #s{overflow:hidden;width:100px;height:60px} #z{position:relative;z-index:2;height:30px;margin-top:40px;background:#f00}</style><div id=s><div id=z></div></div>", (W as f32, H as f32));
    let mut s = Scroll::default();
    assert_eq!(px(&draw(&p, &s, (false, false)), 10, 50), 0xff0000);
    s.nodes.insert(node(&p, "s"), (0.0, 40.0));
    let c = draw(&p, &s, (false, false));
    assert_eq!(px(&c, 10, 10), 0xff0000, "the z-index box moved up with the container");
    assert_eq!(px(&c, 10, 50), 0xffffff);
}

/// The column of the page scrollbar, as the runs `(y, red)` of Chrome 15x show them (`docs/resources.md`, "Scrolling").
fn column(c: &Canvas, x: u32) -> Vec<(u32, u32)> {
    let mut runs: Vec<(u32, u32)> = Vec::new();
    for y in 0..c.height {
        let v = (c.pixel(x, y) >> 16) & 255;
        if runs.last().is_none_or(|r| r.1 != v) {
            runs.push((y, v));
        }
    }
    runs
}

#[test]
fn the_page_scrollbar_has_the_pixels_of_chrome() {
    // A page of 1000 px in a window of 400 x 300 with a bar: layout is 385 wide.
    let p = page("<style>body{margin:0}#a{height:1000px;background:#eef}</style><div id=a></div>", ((W - 15) as f32, H as f32));
    assert_eq!(p.areas.page.1, 1000.0);
    for (offset, thumb) in [(0.0, 18..97), (100.0, 44..123), (350.0, 110..189), (700.0, 203..282)] {
        let s = Scroll { y: offset, ..Scroll::default() };
        let c = draw(&p, &s, (true, false));
        let col = column(&c, 392);
        // Chrome: the track `fc`, the up arrow rows 6..11 and the down arrow rows 288..293 are `8b`; the thumb is `8b` in the
        // middle of its width. The end rows of the thumb are round (the first and the last row are not full).
        assert_eq!(col[0], (0, 0xfc));
        assert_eq!(col[1], (6, 0x8b), "{offset}: up arrow");
        assert_eq!(col[2], (11, 0xfc));
        let (a, b) = (thumb.start, thumb.end);
        let t: Vec<_> = col.iter().filter(|r| r.0 >= 12 && r.0 < 285).collect();
        assert!(t.iter().any(|r| (r.0 == a || r.0 == a + 1) && (r.1 == 0x8b || r.1 == 0x8e)), "{offset}: the thumb starts at {a}: {col:?}");
        assert!(t.iter().any(|r| r.0 == b && r.1 == 0xfc), "{offset}: and ends at {b}: {col:?}");
        assert!(col.contains(&(288, 0x8b)) && col.contains(&(293, 0xfc)), "{offset}: down arrow");
    }
    // The content next to the bar is the page (`eef`), and the bar is 15 px wide.
    let c = draw(&p, &Scroll::default(), (true, false));
    assert_eq!(px(&c, 384, 100), 0xeeeeff);
    assert_eq!(px(&c, 385, 100), 0xfcfcfc);
    assert_eq!(px(&c, 399, 100), 0xfcfcfc);
}

#[test]
fn a_container_with_a_bar_draws_it_inside_its_padding_box() {
    let p = page("<style>body{margin:0} #s{overflow:auto;width:100px;height:100px;margin:10px} #c{height:300px;background:#cde}</style><div id=s><div id=c></div></div>", (W as f32, H as f32));
    let c = draw(&p, &Scroll::default(), (false, false));
    // The box is at 10..110. Its bar is 95..110 wide.
    assert_eq!(px(&c, 94, 50), 0xccddee);
    assert_eq!(px(&c, 96, 50), 0xfcfcfc);
    assert_eq!(px(&c, 109, 50), 0xfcfcfc);
    assert_eq!(px(&c, 110, 50), 0xffffff);
}
