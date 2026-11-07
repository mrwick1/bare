//! Tests of `document.elementFromPoint` and `elementsFromPoint` (`browser_point.rs`). The 40 cases of `tools/fixtures/ui/point-cases.txt` must
//! answer what headless Chrome 155 answered (`chrome-point.txt`, made by `BARE_BARS=1 node tools/ui/chrome_topic.mjs point`).

use crate::browser_topic_tests::topic_test;

topic_test!(chrome_cases, "point", 39);
