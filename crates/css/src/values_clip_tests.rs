//! `clip`: `auto`, or a `rect()`. A rect with no area reads as `none` (nothing paints); any other rect reads as `auto`.
use crate::values::{parse_declaration, Keyword, Property, Side, Value};

fn clip(value: &str) -> Option<Keyword> {
    let mut out = Vec::new();
    if !parse_declaration("clip", value, false, &mut Side::default(), &mut out) {
        return None;
    }
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].property, Property::Clip);
    let Value::Keyword(k) = out[0].value else { panic!("not a keyword") };
    Some(k)
}

#[test]
fn a_rect_with_no_area_is_none() {
    for v in ["rect(0, 0, 0, 0)", "rect(0px, 0px, 0px, 0px)", "rect(0 0 0 0)", "RECT(1px,1px,1px,1px)", "rect(0, 5px, 0, 0)", "rect(0, 0, 5px, 0)"] {
        assert_eq!(clip(v), Some(Keyword::None), "{v}");
    }
}

#[test]
fn a_rect_with_an_area_and_auto_clip_nothing() {
    for v in ["rect(0, 5px, 5px, 0)", "rect(auto, auto, auto, auto)", "auto"] {
        assert_eq!(clip(v), Some(Keyword::Auto), "{v}");
    }
}

#[test]
fn a_bad_rect_is_not_good() {
    assert_eq!(clip("rect(0, 0, 0)"), None);
    assert_eq!(clip("rect(0, 0, 0, 0"), None);
    assert_eq!(clip("5px"), None);
}
