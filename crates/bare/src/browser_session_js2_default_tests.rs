//! The engine that `BARE_JS_EXEC` picks: js2 unless the value names the old engine.

use crate::browser_session::js2_from;

#[test]
fn js2_is_the_default() {
    assert!(js2_from(""));
    assert!(js2_from("new"));
    assert!(js2_from("unknown"));
}

#[test]
fn the_old_engine_names_pick_the_old_engine() {
    for v in ["tree", "auto", "byte"] {
        assert!(!js2_from(v), "{v}");
    }
}
