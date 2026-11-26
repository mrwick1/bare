//! `prefers-color-scheme` follows the scheme that the window sets with `set_prefers_dark`. Its own process: the setting is global.
use bare_css::at_rules::{media_matches, set_prefers_dark};

#[test]
fn the_scheme_follows_the_setting() {
    let vp = (1280.0, 800.0);
    assert!(media_matches("(prefers-color-scheme: light)", vp));
    assert!(!media_matches("(prefers-color-scheme: dark)", vp));
    set_prefers_dark(true);
    assert!(media_matches("(prefers-color-scheme: dark)", vp));
    assert!(!media_matches("(prefers-color-scheme: light)", vp));
    assert!(media_matches("screen and (prefers-color-scheme: dark)", vp));
}
