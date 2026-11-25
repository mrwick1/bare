//! `clip: rect(0 0 0 0)` on an absolute box: nothing of the box paints. Teams hides its screen reader text so (Griffel
//! `screenReaderContainer`). The box stays in layout with its size; only the paint is cut.

use bare_css::cascade::{Origin, RuleSet, Source, UA_CSS};
use bare_html::tree;
use bare_paint::capture;

fn render(style: &str) -> bare_paint::canvas::Canvas<'static> {
    let html = format!("<style>body {{ margin: 0 }} #b {{ position: absolute; left: 10px; top: 10px; width: 50px; height: 50px; background: #f00; {style} }}</style><div id=b></div>");
    let mut doc = tree::parse(&html);
    let viewport = (100.0, 100.0);
    let mut rules = RuleSet::new(viewport);
    rules.add(&mut doc, UA_CSS, Origin::UserAgent);
    for source in bare_css::cascade::sources(&doc, viewport) {
        if let Source::Inline(text) = source {
            rules.add(&mut doc, &text, Origin::Author);
        }
    }
    let styles = rules.compute(&doc);
    let layout = bare_layout::layout(&doc, &styles, viewport);
    capture(&doc, &styles, &layout, 100, 100)
}

#[test]
fn an_absolute_box_paints_with_no_clip() {
    assert_eq!(render("").pixel(30, 30) & 0xffffff, 0xff0000);
    assert_eq!(render("clip: auto").pixel(30, 30) & 0xffffff, 0xff0000);
}

#[test]
fn an_empty_clip_rect_hides_the_whole_box() {
    for clip in ["clip: rect(0px, 0px, 0px, 0px)", "clip: rect(0 0 0 0)", "clip: rect(1px, 1px, 1px, 1px)"] {
        assert_eq!(render(clip).pixel(30, 30) & 0xffffff, 0xffffff, "{clip}");
    }
}

#[test]
fn a_clip_on_a_static_box_does_nothing() {
    // `clip` applies to absolute and fixed boxes only (CSS Masking 1, 7.1).
    let html = "<style>body { margin: 0 } #b { height: 50px; background: #f00; clip: rect(0, 0, 0, 0) }</style><div id=b></div>";
    let mut doc = tree::parse(html);
    let viewport = (100.0, 100.0);
    let mut rules = RuleSet::new(viewport);
    rules.add(&mut doc, UA_CSS, Origin::UserAgent);
    for source in bare_css::cascade::sources(&doc, viewport) {
        if let Source::Inline(text) = source {
            rules.add(&mut doc, &text, Origin::Author);
        }
    }
    let styles = rules.compute(&doc);
    let layout = bare_layout::layout(&doc, &styles, viewport);
    assert_eq!(capture(&doc, &styles, &layout, 100, 100).pixel(30, 30) & 0xffffff, 0xff0000);
}
