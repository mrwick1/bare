//! `infinity` in `calc()` (CSS Values 4, 10.7.1). It is the largest finite number, so `calc(infinity * 1px)` (Tailwind 4
//! `rounded-full`) is a huge length and is not dropped.
use crate::calc::{Calc, Linear, parse};
use crate::tokenizer::tokenize;
use crate::values::Length;

fn calc(text: &str) -> Option<Calc> {
    let tokens: Vec<_> = tokenize(text).collect();
    parse(&tokens, |v, u| (u == "px").then_some(Length::Px(v)))
}

fn px(v: f32) -> Option<Calc> {
    Some(Calc::Length(Linear([v, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0])))
}

#[test]
fn infinity_is_the_largest_finite_number() {
    assert_eq!(calc("infinity * 1px"), px(f32::MAX));
    assert_eq!(calc("1px * INFINITY"), px(f32::MAX));
    assert_eq!(calc("-infinity * 1px"), px(-f32::MAX));
    assert_eq!(calc("infinity"), Some(Calc::Number(f32::MAX)));
    assert_eq!(calc("infinityx * 1px"), None);
}
