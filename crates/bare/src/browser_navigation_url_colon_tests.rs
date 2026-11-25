//! A relative reference with a colon in its first path segment (`browser_navigation_url.rs`, `relative`). The part before the colon
//! is not a scheme when it does not start with a letter (WHATWG URL, "scheme start state"), so the whole input is a path. Teams
//! builds such URLs from user ids (`8:orgid:<id>`). The expected values are those of Node's `URL`.

use crate::browser_navigation_url::{parse, parse_any};

const BASE: &str = "https://teams.cloud.microsoft/v2/worker/x.js";

fn href(input: &str) -> String {
    let base = parse(BASE, None).unwrap();
    parse_any(input, Some(&base)).map(|r| r.href()).unwrap_or_else(|e| format!("{e:?}"))
}

#[test]
fn a_colon_after_a_segment_that_is_not_a_scheme_stays_in_the_path() {
    assert_eq!(href("8:orgid:abc"), "https://teams.cloud.microsoft/v2/worker/8:orgid:abc");
    assert_eq!(href("8:orgid:abc?x=1:2"), "https://teams.cloud.microsoft/v2/worker/8:orgid:abc?x=1:2");
    assert_eq!(href("a@b:c/d"), "https://teams.cloud.microsoft/v2/worker/a@b:c/d");
    assert_eq!(href("1a:b/../c"), "https://teams.cloud.microsoft/v2/worker/c");
}
