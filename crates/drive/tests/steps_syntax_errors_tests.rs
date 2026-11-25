//! A step line with a wrong shape names what is wrong, besides the form of the step, so the caller can correct it.

use bare_drive::steps::parse;
use std::collections::HashMap;

fn err(src: &str) -> String {
    parse(src, &HashMap::new()).unwrap_err()
}

#[test]
fn fill_and_type_name_a_missing_or_unquoted_text() {
    assert_eq!(err("fill label=\"Email\""), "line 1: fill LOC \"text\" [keys=last]: the text is missing");
    assert_eq!(err("click label=\"Email\"\nfill label=\"Email\" someone@example.test"), "line 2: fill LOC \"text\" [keys=last]: put the text in double quotes");
    assert_eq!(err("fill label=\"Email\" ${EMAIL}"), "line 1: fill LOC \"text\" [keys=last]: put the text in double quotes");
    assert_eq!(err("type label=\"Email\""), "line 1: type LOC \"text\": the text is missing");
    assert_eq!(err("type label=\"Email\" hello"), "line 1: type LOC \"text\": put the text in double quotes");
    // A quoted text with a wrong option keeps the plain form.
    assert_eq!(err("fill css=\"#q\" \"ab\" keys=all"), "line 1: fill LOC \"text\" [keys=last]");
}

#[test]
fn expect_names_a_missing_state() {
    assert_eq!(
        err("expect text=\"Sign in\""),
        "line 1: expect LOC visible|hidden|enabled|disabled|checked|unchecked|focused|count N|text=\"x\"|value=\"x\": the state is missing"
    );
    assert_eq!(
        err("wait css=\"#q\""),
        "line 1: expect LOC visible|hidden|enabled|disabled|checked|unchecked|focused|count N|text=\"x\"|value=\"x\": the state is missing"
    );
}
