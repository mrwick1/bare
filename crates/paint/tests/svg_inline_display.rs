//! An `svg` with `display: inline` (Fluent icons in the Teams app bar, class `f1w7gpdv`) is a replaced element: its `width` and
//! `height` count, so it paints. Before, it was a plain inline with no size, and the icon did not show.

use bare_css::cascade::{Origin, RuleSet, Source, UA_CSS};
use bare_html::tree;
use bare_paint::capture;

fn render(svg: &str) -> bare_paint::canvas::Canvas<'static> {
    let html = format!("<style>body {{ margin: 0 }} :root {{ --c: #f00 }}</style>{svg}");
    let mut doc = tree::parse(&html);
    let viewport = (60.0, 60.0);
    let mut rules = RuleSet::new(viewport);
    rules.add(&mut doc, UA_CSS, Origin::UserAgent);
    for source in bare_css::cascade::sources(&doc, viewport) {
        if let Source::Inline(text) = source {
            rules.add(&mut doc, &text, Origin::Author);
        }
    }
    let styles = rules.compute(&doc);
    let layout = bare_layout::layout(&doc, &styles, viewport);
    capture(&doc, &styles, &layout, 60, 60)
}

#[test]
fn an_inline_svg_has_the_size_of_its_attributes() {
    let c = render("<style>svg { display: inline }</style><div><svg width=24 height=24 viewBox='0 0 24 24' fill='#f00'><path d='M0 0h24v24H0z'/></svg></div>");
    assert_eq!(c.pixel(12, 12) & 0xffffff, 0xff0000);
    assert_eq!(c.pixel(30, 12) & 0xffffff, 0xffffff, "right of the 24px icon");
}

#[test]
fn an_inline_icon_in_a_flex_button_paints() {
    // The Teams app bar: button > span (inline-flex, 20px) > div (flex, centered) > div > svg with `display: inline`.
    let c = render("<style>.i { display: inline } .l { line-height: 0 } .b { display: inline-flex; align-items: center; justify-content: center; width: 20px; height: 20px }
        .f { display: flex; align-items: center; justify-content: center }</style>
        <button style='padding:0;border:0;background:none'><span class=b><div class=f><div><svg class='i l' width=24 height=24 viewBox='0 0 24 24' fill='var(--c)'><path d='M0 0h24v24H0z' fill='var(--c)'/></svg></div></div></span></button>");
    assert!((0..60).any(|y| (0..60).any(|x| c.pixel(x, y) & 0xffffff == 0xff0000)), "the icon paints some red");
}
