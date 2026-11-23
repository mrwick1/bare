//! `backdrop-filter: blur()`: the pixels under the box are blurred before the box paints its background. Outside the box they stay sharp.

use bare_css::cascade::{Origin, RuleSet, Source, UA_CSS};
use bare_html::tree;
use bare_paint::capture;

const W: u32 = 200;
const H: u32 = 100;

fn render(html: &str) -> bare_paint::canvas::Canvas<'static> {
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
    capture(&doc, &styles, &layout, W, H)
}

/// Black and white stripes, 2 px each, under a box at the left half. `filter` is the style of that box.
fn stripes(filter: &str) -> bare_paint::canvas::Canvas<'static> {
    let bars: String = (0..50).map(|_| "<i></i>").collect();
    render(&format!(
        "<style>body {{ margin: 0 }} #s {{ display: flex; height: 100px; background: #000 }} i {{ width: 2px; margin-right: 2px; background: #fff }} \
         #b {{ position: absolute; left: 0; top: 0; width: 100px; height: 100px; {filter} }}</style><div id=s>{bars}</div><div id=b></div>"
    ))
}

fn gray(p: u32) -> bool {
    let g = p & 0xff;
    (0x30..=0xd0).contains(&g)
}

#[test]
fn a_backdrop_blur_mixes_the_pixels_under_the_box_only() {
    let sharp = stripes("");
    assert!(!(0..100).any(|x| gray(sharp.pixel(x, 50))), "no blur: only black and white");
    let blurred = stripes("backdrop-filter: blur(4px); background: rgba(255, 255, 255, 0.1)");
    assert!((10..90).all(|x| gray(blurred.pixel(x, 50))), "under the box the stripes mix to gray");
    assert!(!(110..190).any(|x| gray(blurred.pixel(x, 50))), "outside the box they stay sharp");
}
