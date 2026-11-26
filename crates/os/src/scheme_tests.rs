//! Tests of the colour scheme of the desktop (`scheme.rs`).
use super::{dark_in_ini, scheme_from_env};

#[test]
fn the_environment_wins() {
    assert_eq!(scheme_from_env(Some("dark")), Some(true));
    assert_eq!(scheme_from_env(Some("LIGHT")), Some(false));
    assert_eq!(scheme_from_env(Some("blue")), None);
    assert_eq!(scheme_from_env(None), None);
}

#[test]
fn gtk_settings_give_the_dark_preference() {
    assert!(dark_in_ini("[Settings]\ngtk-theme-name=Adwaita\ngtk-application-prefer-dark-theme=1\n"));
    assert!(dark_in_ini("[Settings]\ngtk-application-prefer-dark-theme = true\n"));
    assert!(!dark_in_ini("[Settings]\ngtk-application-prefer-dark-theme=0\n"));
    assert!(!dark_in_ini("[Settings]\n# gtk-application-prefer-dark-theme=1\n"));
    assert!(!dark_in_ini(""));
}
