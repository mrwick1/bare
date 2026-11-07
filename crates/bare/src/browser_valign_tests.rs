//! Tests of `vertical-align` and of the baselines of atomic inlines in a line (CSS 2.1 section 10.8, read through `getBoundingClientRect`).
//! The Chrome cases (`tools/fixtures/ui/valign-cases.txt`): top and height of each box against headless Chrome 155.

use crate::browser_topic_tests::topic_test;

topic_test!(chrome_cases, "valign", 57);
