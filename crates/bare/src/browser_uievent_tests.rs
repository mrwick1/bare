//! Tests of the `MouseEvent`, `PointerEvent`, `KeyboardEvent` and `InputEvent` constructors, their members and pointer capture
//! (`browser_uievent.rs`). The cases of `tools/fixtures/ui/uievent-cases.txt` match the answers of headless Chrome 155
//! (`tools/fixtures/ui/chrome-uievent.txt`, made by `tools/ui/chrome_topic.mjs uievent`).

use crate::browser_topic_tests::topic_test;

topic_test!(chrome_cases, "uievent", 47);
