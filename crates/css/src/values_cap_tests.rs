//! The `cap` unit: the cap height of the font. T3 Code sizes its logo with `height: 1cap`.
use crate::values::{length, Length};

#[test]
fn cap_is_the_cap_height_of_noto_sans() {
    // `OS/2` `sCapHeight` 714 of 1000 in the bundled Noto Sans and Noto Sans Mono.
    assert_eq!(length(1.0, "cap"), Some(Length::Em(0.714)));
    assert_eq!(length(2.0, "CAP"), Some(Length::Em(1.428)));
}
