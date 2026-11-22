use super::{LOCALES, icu_locale};

#[test]
fn precomputed_icu_locales_match_generated_tags() {
    for (i, tag) in LOCALES.iter().enumerate() {
        assert_eq!(icu_locale(i as u8), &tag.parse::<icu_locale_core::Locale>().unwrap());
    }
}
