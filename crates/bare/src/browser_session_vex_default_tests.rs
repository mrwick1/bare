//! The engine that `BARE_JS_EXEC` picks: Vex unless the value names the old engine.

use crate::browser_session::vex_from;

#[test]
fn vex_is_the_default() {
    assert!(vex_from(""));
    assert!(vex_from("vex"));
    assert!(vex_from("new"));
    assert!(vex_from("unknown"));
}

#[test]
fn the_old_engine_names_pick_the_old_engine() {
    for v in ["tree", "auto", "byte"] {
        assert!(!vex_from(v), "{v}");
    }
}
