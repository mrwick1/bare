//! Tests of `browser_controls.rs` and `browser_textarea.rs`: constraint validation, `labels`, `form.reset()` and `HTMLTextAreaElement`.
//! The Chrome cases are in `tools/fixtures/ui/{validity,textarea,labels,reset}-cases.txt`.

use crate::browser_topic_tests::topic_test;

topic_test!(chrome_validity, "validity", 83);
topic_test!(chrome_textarea, "textarea", 37);
topic_test!(chrome_labels, "labels", 33);
topic_test!(chrome_reset, "reset", 29);
